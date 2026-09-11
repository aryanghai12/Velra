//! Hook runtime (§8): always exit 0, stdout is empty or exactly one JSON
//! object, stderr is always empty, panics are contained, and an internal
//! watchdog abandons work rather than delaying Claude Code (§4).

use crate::home::{self, Config};
use crate::log;
use crate::normalize::{self, FileResponse, HookInput, ShellResponse, ToolInput};
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use velra_core::checkpoint::{self, CheckpointRequest};
use velra_core::continuation::{self, DeliveryRequest};
use velra_core::db::{self, Db, DbError, Role};
use velra_core::event::{limits, FileObservation, GitObservation, NewEvent, Payload, ProjectInfo};
use velra_core::model::{hook_event as he, tools, Channel, ContinuationState, Trigger};
use velra_core::reducer::{self, ReduceOptions};
use velra_core::{eventlog, git, hash, paths, shell, spool, text};

/// Watchdog for synchronous hooks (§4).
const WATCHDOG_SYNC_MS: u64 = 250;
/// Watchdog for the async reducer (§4).
const WATCHDOG_REDUCE_MS: u64 = 1_000;
/// Reducer deadline, inside the async watchdog.
const REDUCE_DEADLINE_MS: u64 = 900;
/// Bounded reducer pass at PreCompact (§15.2).
const PRECOMPACT_REDUCE_MS: u64 = 6;
/// Hard cap on stdin (§8.1 rule 4).
const MAX_STDIN: usize = 64 * 1024 * 1024;
/// Files rehashed around a git restore-family command (§8.3).
const GIT_HASH_MAX_FILES: usize = 64;

/// Guards stdout so the watchdog can never truncate a partial write.
static EMITTED: Mutex<bool> = Mutex::new(false);

fn emit(json: &str) -> bool {
    let mut guard = EMITTED.lock().unwrap_or_else(|e| e.into_inner());
    if *guard {
        return false;
    }
    let mut out = std::io::stdout().lock();
    if out.write_all(json.as_bytes()).is_err() || out.write_all(b"\n").is_err() {
        return false;
    }
    let _ = out.flush();
    *guard = true;
    true
}

fn start_watchdog(ms: u64) {
    let _ = std::thread::Builder::new()
        .name("velra-watchdog".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            // Wait for any in-flight write, then leave without further work.
            let _guard = EMITTED.lock().unwrap_or_else(|e| e.into_inner());
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        });
}

/// Replaces the default panic hook so nothing ever reaches stderr (§8.1).
fn silence_panics(home: Option<PathBuf>, label: String) {
    std::panic::set_hook(Box::new(move |info| {
        log::error(home.as_deref(), &label, None, format!("panic: {info}"));
    }));
}

fn read_stdin_capped() -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 64 * 1024];
    let mut stdin = std::io::stdin().lock();
    loop {
        match stdin.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < MAX_STDIN {
                    let take = n.min(MAX_STDIN - buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                }
                // Beyond the cap we keep draining so the writer never blocks.
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    buf
}

#[cfg(feature = "fault-injection")]
fn fault_injection(sub: &str) {
    if let Ok(target) = std::env::var("VELRA_TEST_PANIC") {
        if target == "all" || target == sub {
            panic!("injected panic in {sub}");
        }
    }
    if let Ok(ms) = std::env::var("VELRA_TEST_STALL_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

#[cfg(not(feature = "fault-injection"))]
fn fault_injection(_sub: &str) {}

/// Entry point for `velra hook <event>` and `velra reduce`.
pub fn run(command: &str, subcommand: Option<String>, ts_ms: i64) {
    let home = home::velra_home();
    if home::is_disabled(home.as_deref()) {
        return;
    }
    let sub = if command == "reduce" {
        "reduce".to_string()
    } else {
        subcommand.unwrap_or_default()
    };
    let label = if command == "reduce" {
        "reduce".to_string()
    } else {
        format!("hook {sub}")
    };
    silence_panics(home.clone(), label.clone());
    start_watchdog(if command == "reduce" {
        WATCHDOG_REDUCE_MS
    } else {
        WATCHDOG_SYNC_MS
    });
    let started = Instant::now();
    let outcome =
        std::panic::catch_unwind(AssertUnwindSafe(|| dispatch(&sub, ts_ms, home.as_deref())));
    if let Ok(Err(e)) = outcome {
        log::error(home.as_deref(), &label, None, e);
    }
    log::debug(
        home.as_deref(),
        &label,
        None,
        format!("{} us", started.elapsed().as_micros()),
    );
}

fn dispatch(sub: &str, ts_ms: i64, home: Option<&Path>) -> Result<(), String> {
    fault_injection(sub);
    let Some(home) = home else { return Ok(()) };
    let raw = read_stdin_capped();
    if sub == "reduce" {
        return run_reduce(home, ts_ms);
    }
    let parsed = normalize::parse(&raw);
    let malformed = parsed.is_none();
    let input = parsed.unwrap_or_default();
    let session_id = match input
        .session_id
        .clone()
        .or_else(|| normalize::salvage_session_id(&raw))
    {
        Some(s) if !s.is_empty() => s,
        // §8.2: no session id → no-op.
        _ => return Ok(()),
    };
    if home::ensure_home(home).is_err() {
        return Ok(()); // read-only home: fail open
    }
    let ctx = Ctx::new(home, sub, ts_ms, input, session_id, malformed);
    if malformed {
        let payload = Payload::default();
        let mut ev = ctx.new_event(payload);
        ev.hook_event = he::MALFORMED.to_string();
        ctx.store(ev, Role::HookAppend);
        return Ok(());
    }
    match sub {
        "session-start" => session_start(&ctx),
        "user-prompt-submit" => user_prompt_submit(&ctx),
        "pre-tool-use" => pre_tool_use(&ctx),
        "post-tool-use" => post_tool_use(&ctx, false),
        "post-tool-use-failure" => post_tool_use(&ctx, true),
        "stop" => stop(&ctx),
        "pre-compact" => pre_compact(&ctx),
        "post-compact" => post_compact(&ctx),
        "session-end" => session_end(&ctx),
        // Unknown event: record a minimal event (§18).
        _ => {
            ctx.store(ctx.new_event(Payload::default()), Role::HookAppend);
            Ok(())
        }
    }
}

struct Ctx<'a> {
    home: &'a Path,
    sub: &'a str,
    label: String,
    ts_ms: i64,
    input: HookInput<'a>,
    session_id: String,
    agent_id: Option<String>,
    root: PathBuf,
    root_str: String,
    project_id: String,
    is_git: bool,
    config: Config,
}

impl<'a> Ctx<'a> {
    fn new(
        home: &'a Path,
        sub: &'a str,
        ts_ms: i64,
        input: HookInput<'a>,
        session_id: String,
        _malformed: bool,
    ) -> Ctx<'a> {
        let cwd = input.cwd.clone();
        let root = project_root(cwd.as_deref());
        let canonical = paths::canonical(&root).unwrap_or_else(|| root.clone());
        let root_str = paths::normalize_abs(&canonical.to_string_lossy());
        let project_id = hash::hex_prefix(paths::identity(&root_str).as_bytes(), 16);
        let is_git = git::git_dir(&canonical).is_some();
        Ctx {
            home,
            sub,
            label: format!("hook {sub}"),
            ts_ms,
            agent_id: input.agent_id.clone(),
            input,
            session_id,
            root: canonical,
            root_str,
            project_id,
            is_git,
            config: Config::load(home),
        }
    }

    fn hook_event(&self) -> &'static str {
        match self.sub {
            "session-start" => he::SESSION_START,
            "user-prompt-submit" => he::USER_PROMPT_SUBMIT,
            "pre-tool-use" => he::PRE_TOOL_USE,
            "post-tool-use" => he::POST_TOOL_USE,
            "post-tool-use-failure" => he::POST_TOOL_USE_FAILURE,
            "stop" => he::STOP,
            "pre-compact" => he::PRE_COMPACT,
            "post-compact" => he::POST_COMPACT,
            "session-end" => he::SESSION_END,
            _ => "Unknown",
        }
    }

    fn tool_name(&self) -> &str {
        self.input.tool_name.as_deref().unwrap_or("")
    }

    /// Absolute path for a tool-supplied path (already absolute in practice).
    fn resolve(&self, p: &str) -> PathBuf {
        if paths::is_absolute_str(p) {
            PathBuf::from(p)
        } else {
            match self.input.cwd.as_deref() {
                Some(cwd) => Path::new(cwd).join(p),
                None => self.root.join(p),
            }
        }
    }

    fn display_path(&self, abs: &Path) -> (String, bool) {
        normalize::display_path(&abs.to_string_lossy(), &self.root_str)
    }

    fn new_event(&self, payload: Payload) -> NewEvent {
        let hook_event = self.hook_event();
        NewEvent {
            dedupe_key: velra_core::event::dedupe_key(
                hook_event,
                &self.session_id,
                self.input.tool_use_id.as_deref(),
                self.input.prompt_id.as_deref(),
                self.ts_ms,
                self.agent_id.as_deref(),
            ),
            session_id: self.session_id.clone(),
            project_id: self.project_id.clone(),
            agent_id: self.agent_id.clone(),
            hook_event: hook_event.to_string(),
            tool_name: self.input.tool_name.clone(),
            tool_use_id: self.input.tool_use_id.clone(),
            ts_ms: self.ts_ms,
            payload: payload.to_json(),
            project: Some(ProjectInfo {
                project_id: self.project_id.clone(),
                root_path: self.root_str.clone(),
                is_git: self.is_git,
            }),
        }
    }

    fn spool(&self, ev: &NewEvent) {
        if let Err(e) = spool::write(&home::spool_dir(self.home), ev) {
            log::error(
                Some(self.home),
                &self.label,
                Some(&self.session_id),
                format!("spool failed: {e}"),
            );
        }
    }

    /// Opens the database, or `None` when the caller should spool instead.
    fn open_db(&self, role: Role) -> Option<Db> {
        match Db::open(&home::db_path(self.home), role) {
            Ok(db) => {
                if let Some(rotated) = &db.rotated_corrupt {
                    log::warn(
                        Some(self.home),
                        &self.label,
                        Some(&self.session_id),
                        format!("corrupt database rotated to {}", rotated.display()),
                    );
                }
                Some(db)
            }
            Err(DbError::NewerSchema(v)) => {
                self.log_newer_schema(v);
                None
            }
            Err(e) => {
                log::debug(
                    Some(self.home),
                    &self.label,
                    Some(&self.session_id),
                    format!("db unavailable: {e}"),
                );
                None
            }
        }
    }

    /// Logs the "newer schema" no-op at most once per hour (§10.5).
    fn log_newer_schema(&self, version: i64) {
        let marker = home::logs_dir(self.home).join(".newer-schema");
        let recent = std::fs::metadata(&marker)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d < Duration::from_secs(3600));
        if recent {
            return;
        }
        let _ = home::ensure_dir(&home::logs_dir(self.home));
        let _ = std::fs::write(&marker, b"");
        log::warn(
            Some(self.home),
            &self.label,
            Some(&self.session_id),
            format!("database schema {version} is newer than this binary; hooks are no-ops until upgrade"),
        );
    }

    /// Appends via an open database, spooling on failure. Returns the event id.
    fn append_with(&self, db: &mut Db, ev: &NewEvent) -> Option<i64> {
        match eventlog::append(&mut db.conn, ev) {
            Ok(id) => id,
            Err(e) => {
                log::debug(
                    Some(self.home),
                    &self.label,
                    Some(&self.session_id),
                    format!("append failed: {e}"),
                );
                self.spool(ev);
                None
            }
        }
    }

    /// Opens, appends and returns the database for follow-up work.
    fn store(&self, ev: NewEvent, role: Role) -> Option<Db> {
        match self.open_db(role) {
            Some(mut db) => {
                self.append_with(&mut db, &ev);
                Some(db)
            }
            None => {
                self.spool(&ev);
                None
            }
        }
    }

    fn discriminator(&self) -> String {
        self.input
            .prompt_id
            .clone()
            .or_else(|| self.input.tool_use_id.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}+{}",
                    self.input.source.clone().unwrap_or_default(),
                    self.ts_ms
                )
            })
    }

    /// Attempts delivery on `channel`, emitting at most one JSON object.
    fn deliver(&self, db: &mut Db, channel: Channel) {
        let key =
            continuation::delivery_key(self.hook_event(), &self.session_id, &self.discriminator());
        let req = DeliveryRequest {
            session_id: &self.session_id,
            channel,
            delivery_key: &key,
            ts_ms: self.ts_ms,
        };
        let emit_capsule = |d: &velra_core::continuation::Delivery| {
            // The renderer caps the capsule far below this, but a stored
            // capsule from another build must never silently spill to a file.
            let chars = d.capsule.chars().count();
            if chars > crate::compat::CAPSULE_MAX_CHARS {
                log::warn(
                    Some(self.home),
                    &self.label,
                    Some(&self.session_id),
                    format!(
                        "capsule is {chars} chars, above the {} char margin under Claude Code's {} char hook output limit",
                        crate::compat::CAPSULE_MAX_CHARS,
                        crate::compat::HOOK_OUTPUT_MAX_CHARS
                    ),
                );
            }
            emit(&continuation::delivery_json(d))
        };
        match continuation::deliver(&mut db.conn, &req, emit_capsule) {
            Ok(_) => {}
            // Busy: leave the continuation deliverable and try again later.
            Err(DbError::Busy) => {}
            Err(e) => log::error(
                Some(self.home),
                &self.label,
                Some(&self.session_id),
                format!("delivery failed: {e}"),
            ),
        }
    }

    fn reconcile(&self, db: &mut Db) {
        if let Err(e) = continuation::reconcile(&mut db.conn, &self.session_id, self.ts_ms) {
            log::debug(
                Some(self.home),
                &self.label,
                Some(&self.session_id),
                format!("reconcile: {e}"),
            );
        }
    }
}

/// §8.2: `CLAUDE_PROJECT_DIR`, else the first ancestor with `.git`, else cwd.
fn project_root(cwd: Option<&str>) -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_PROJECT_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let start = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    git::find_repo_root(&start).unwrap_or(start)
}

/// Hashes the files edited in this session's current epoch (§8.3), including
/// edits that the reducer has not processed yet.
fn hash_session_files(
    conn: &rusqlite::Connection,
    session_id: &str,
    root: &Path,
) -> Vec<FileObservation> {
    const SQL: &str = "SELECT path FROM ( \
         SELECT path AS path, MAX(id) AS ord FROM edits \
           WHERE session_id = ?1 AND epoch = COALESCE((SELECT epoch FROM sessions WHERE session_id = ?1), 1) GROUP BY path \
         UNION ALL \
         SELECT json_extract(payload, '$.path') AS path, MAX(id) AS ord FROM events \
           WHERE session_id = ?1 AND hook_event = 'PostToolUse' \
             AND tool_name IN ('Write', 'Edit', 'MultiEdit', 'NotebookEdit') \
             AND id > COALESCE((SELECT last_event_id FROM reducer_cursor WHERE id = 1), 0) GROUP BY path) \
         WHERE path IS NOT NULL GROUP BY path ORDER BY MAX(ord) DESC LIMIT ?2";
    let Ok(mut stmt) = conn.prepare_cached(SQL) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(
        rusqlite::params![session_id, GIT_HASH_MAX_FILES as i64],
        |r| r.get::<_, String>(0),
    ) else {
        return Vec::new();
    };
    rows.flatten()
        .map(|path| {
            let (hash, size) = hash::hash_file(&paths::resolve(&path, root));
            FileObservation { path, hash, size }
        })
        .collect()
}

fn session_start(ctx: &Ctx<'_>) -> Result<(), String> {
    let source = ctx.input.source.clone().unwrap_or_default();
    let payload = Payload {
        source: Some(source.clone()),
        model: ctx.input.model.clone(),
        transcript_path: ctx.input.transcript_path.clone(),
        ..Default::default()
    };
    let ev = ctx.new_event(payload);
    let Some(mut db) = ctx.store(ev, Role::HookDelivery) else {
        return Ok(());
    };
    match source.as_str() {
        // T6
        "clear" => {
            let _ = continuation::expire_live(&db.conn, &ctx.session_id, ctx.ts_ms);
        }
        // Channel 1
        "compact" | "resume" => {
            ctx.reconcile(&mut db);
            ctx.deliver(&mut db, Channel::SessionStart);
        }
        _ => {}
    }
    Ok(())
}

fn user_prompt_submit(ctx: &Ctx<'_>) -> Result<(), String> {
    let payload = Payload {
        prompt: ctx
            .input
            .prompt
            .as_deref()
            .map(|p| normalize::redact_capped(p, limits::PROMPT)),
        prompt_id: ctx.input.prompt_id.clone(),
        ..Default::default()
    };
    let ev = ctx.new_event(payload);
    let Some(mut db) = ctx.store(ev, Role::HookDelivery) else {
        return Ok(());
    };
    ctx.reconcile(&mut db);
    ctx.deliver(&mut db, Channel::UserPrompt);
    Ok(())
}

fn pre_tool_use(ctx: &Ctx<'_>) -> Result<(), String> {
    let tool = ctx.tool_name().to_string();
    let ti = ToolInput::parse(ctx.input.tool_input);
    if tools::is_edit(&tool) {
        let Some(target) = ti.target_path() else {
            return Ok(());
        };
        let abs = ctx.resolve(target);
        let (hash, size) = hash::hash_file(&abs);
        let (path, _sensitive) = ctx.display_path(&abs);
        let payload = Payload {
            path: Some(path),
            pre_hash: Some(hash),
            size: Some(size),
            ..Default::default()
        };
        ctx.store(ctx.new_event(payload), Role::HookAppend);
        return Ok(());
    }
    if !tools::is_shell(&tool) {
        return Ok(());
    }
    let Some(command) = ti.command.as_deref() else {
        return Ok(());
    };
    let effects = shell::git_effects(command, &|p| ctx.resolve(p).is_file());
    if !effects.any() {
        // Not a restore-family or commit command: nothing to observe.
        return Ok(());
    }
    let mut payload = Payload {
        command: Some(normalize::redact_capped(command, limits::COMMAND)),
        cwd: ctx.input.cwd.clone(),
        ..Default::default()
    };
    match ctx.open_db(Role::HookAppend) {
        Some(mut db) => {
            let files = hash_session_files(&db.conn, &ctx.session_id, &ctx.root);
            payload.git = Some(GitObservation {
                restore: effects.restore,
                commit: effects.commit,
                files,
            });
            normalize::enforce_budget(&mut payload, limits::PAYLOAD);
            let ev = ctx.new_event(payload);
            ctx.append_with(&mut db, &ev);
        }
        None => ctx.spool(&ctx.new_event(payload)),
    }
    Ok(())
}

fn post_tool_use(ctx: &Ctx<'_>, failure: bool) -> Result<(), String> {
    let tool = ctx.tool_name().to_string();
    let ti = ToolInput::parse(ctx.input.tool_input);
    let response = ctx.input.tool_response.or(ctx.input.tool_output);
    let mut payload = Payload::default();
    if failure {
        payload.tool_name = Some(tool.clone());
        payload.error = ctx
            .input
            .error_text()
            .map(|e| normalize::redact_capped(e, limits::ERROR));
        payload.error_type = ctx.input.error_type.clone();
        payload.is_interrupt = ctx.input.is_interrupt;
    }
    let mut git_effects = None;
    if tools::is_edit(&tool) {
        if let Some(target) = ti.target_path() {
            let abs = ctx.resolve(target);
            let (path, sensitive) = ctx.display_path(&abs);
            payload.path = Some(path);
            if !failure {
                let (hash, size) = hash::hash_file(&abs);
                payload.post_hash = Some(hash);
                payload.size = Some(size);
                let fr = FileResponse::parse(response);
                if let Some(original) = fr.original_file.as_deref() {
                    payload.original_hash = Some(hash::content_hash(original.as_bytes()));
                }
                let (added, removed) = ti.line_counts(&tool);
                payload.lines_added = added;
                payload.lines_removed = removed;
                if !sensitive {
                    payload.excerpt = ti.excerpt(&tool).map(|e| normalize::redact_capped(&e, 512));
                }
            }
        }
    } else if tool == "Read" {
        if let Some(target) = ti.target_path() {
            let abs = ctx.resolve(target);
            payload.path = Some(ctx.display_path(&abs).0);
            payload.offset = ti.offset;
            payload.limit = ti.limit;
        }
    } else if tool == "Grep" || tool == "Glob" {
        payload.pattern = ti.pattern.as_deref().map(|p| {
            normalize::redact_capped(&text::truncate_chars(p, limits::PATTERN_CHARS), 1024)
        });
        payload.path = ti.path.clone().or_else(|| ti.file_path.clone());
    } else if tools::is_shell(&tool) {
        let command = ti.command.clone().unwrap_or_default();
        payload.command = Some(normalize::redact_capped(&command, limits::COMMAND));
        payload.cwd = ctx.input.cwd.clone();
        if !failure {
            let sr = ShellResponse::parse(response);
            payload.exit_code = sr.exit_code;
            payload.interrupted = sr.interrupted;
            payload.stdout_tail = sr
                .stdout
                .as_deref()
                .map(|s| normalize::redact_tail(s, limits::OUTPUT_TAIL));
            payload.stderr_tail = sr
                .stderr
                .as_deref()
                .map(|s| normalize::redact_tail(s, limits::OUTPUT_TAIL));
            let effects = shell::git_effects(&command, &|p| ctx.resolve(p).is_file());
            if effects.any() {
                git_effects = Some(effects);
            }
        }
    }
    normalize::enforce_budget(&mut payload, limits::PAYLOAD);
    let role = if failure {
        Role::HookAppend
    } else {
        Role::HookDelivery
    };
    let Some(mut db) = ctx.open_db(role) else {
        ctx.spool(&ctx.new_event(payload));
        return Ok(());
    };
    if let Some(effects) = git_effects {
        let files = hash_session_files(&db.conn, &ctx.session_id, &ctx.root);
        payload.git = Some(GitObservation {
            restore: effects.restore,
            commit: effects.commit,
            files,
        });
        normalize::enforce_budget(&mut payload, limits::PAYLOAD);
    }
    let ev = ctx.new_event(payload);
    ctx.append_with(&mut db, &ev);
    // Delivery channel 2 / confirmation evidence (T3).
    match continuation::live(&db.conn, &ctx.session_id) {
        Ok(Some(live)) => {
            if live.state == ContinuationState::Pending && !failure {
                ctx.deliver(&mut db, Channel::PostTool);
            } else {
                ctx.reconcile(&mut db);
            }
        }
        Ok(None) => {}
        Err(e) => log::debug(
            Some(ctx.home),
            &ctx.label,
            Some(&ctx.session_id),
            format!("live lookup: {e}"),
        ),
    }
    Ok(())
}

fn stop(ctx: &Ctx<'_>) -> Result<(), String> {
    let payload = Payload {
        stop_hook_active: ctx.input.stop_hook_active,
        ..Default::default()
    };
    let Some(mut db) = ctx.store(ctx.new_event(payload), Role::HookAppend) else {
        return Ok(());
    };
    ctx.reconcile(&mut db);
    Ok(())
}

fn session_end(ctx: &Ctx<'_>) -> Result<(), String> {
    let reason = ctx.input.reason.clone().unwrap_or_default();
    let payload = Payload {
        reason: Some(reason.clone()),
        ..Default::default()
    };
    let Some(db) = ctx.store(ctx.new_event(payload), Role::HookAppend) else {
        return Ok(());
    };
    if matches!(reason.as_str(), "clear" | "logout") {
        let _ = continuation::expire_live(&db.conn, &ctx.session_id, ctx.ts_ms);
    }
    Ok(())
}

fn post_compact(ctx: &Ctx<'_>) -> Result<(), String> {
    let summary = ctx
        .input
        .summary_text()
        .map(|s| normalize::redact_capped(s, limits::SUMMARY));
    let mut payload = Payload {
        trigger: ctx.input.trigger.clone(),
        summary: summary.clone(),
        ..Default::default()
    };
    normalize::enforce_budget(&mut payload, limits::POST_COMPACT_PAYLOAD);
    let Some(db) = ctx.store(ctx.new_event(payload), Role::HookAppend) else {
        return Ok(());
    };
    if let Some(summary) = summary {
        if let Err(e) =
            checkpoint::record_native_summary(&db.conn, &ctx.session_id, ctx.ts_ms, &summary)
        {
            log::debug(
                Some(ctx.home),
                &ctx.label,
                Some(&ctx.session_id),
                format!("native summary: {e}"),
            );
        }
    }
    Ok(())
}

fn spool_checkpoint_request(ctx: &Ctx<'_>, trigger: &str) {
    let mut ev = ctx.new_event(Payload {
        trigger: Some(trigger.to_string()),
        partial: Some(true),
        ..Default::default()
    });
    ev.hook_event = he::CHECKPOINT_REQUEST.to_string();
    ev.dedupe_key = velra_core::event::dedupe_key(
        he::CHECKPOINT_REQUEST,
        &ctx.session_id,
        None,
        None,
        ctx.ts_ms,
        ctx.agent_id.as_deref(),
    );
    ctx.spool(&ev);
}

fn pre_compact(ctx: &Ctx<'_>) -> Result<(), String> {
    let trigger_text = ctx
        .input
        .trigger
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    let trigger = Trigger::parse(&trigger_text).unwrap_or(Trigger::Auto);
    let payload = Payload {
        trigger: Some(trigger_text.clone()),
        custom_instructions: Some(ctx.input.has_custom_instructions()),
        ..Default::default()
    };
    let ev = ctx.new_event(payload);
    let Some(mut db) = ctx.open_db(Role::PreCompact) else {
        ctx.spool(&ev);
        spool_checkpoint_request(ctx, &trigger_text);
        return Ok(());
    };
    ctx.append_with(&mut db, &ev);

    // Bounded reducer pass; anything left unreduced makes the checkpoint partial.
    let deadline = Instant::now() + Duration::from_millis(PRECOMPACT_REDUCE_MS);
    let render = ctx.config.render();
    let stats = reducer::reduce(
        &mut db.conn,
        &ReduceOptions {
            deadline: Some(deadline),
            batch: 50,
            spool_dir: None,
            render,
        },
    )
    .unwrap_or_default();
    let watermark = db::cursor(&db.conn).unwrap_or(0);
    let request = CheckpointRequest {
        session_id: &ctx.session_id,
        trigger,
        created_ms: ctx.ts_ms,
        partial: !stats.caught_up,
        watermark,
    };
    let tx = match db
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
    {
        Ok(tx) => tx,
        Err(_) => {
            spool_checkpoint_request(ctx, &trigger_text);
            return Ok(());
        }
    };
    match checkpoint::create_in_tx(&tx, &request, &render) {
        Ok(Some(info)) => {
            if tx.commit().is_ok() {
                let msg = serde_json::json!({
                    "systemMessage": format!("\u{26a1} Velra checkpoint saved: {}", info.summary),
                });
                emit(&msg.to_string());
            } else {
                spool_checkpoint_request(ctx, &trigger_text);
            }
        }
        // Nothing worth saving (§15.2 step 4): no checkpoint, empty stdout.
        Ok(None) => {
            let _ = tx.commit();
        }
        Err(e) => {
            drop(tx);
            log::error(
                Some(ctx.home),
                &ctx.label,
                Some(&ctx.session_id),
                format!("checkpoint failed: {e}"),
            );
            spool_checkpoint_request(ctx, &trigger_text);
        }
    }
    Ok(())
}

fn run_reduce(home: &Path, _ts_ms: i64) -> Result<(), String> {
    if home::ensure_home(home).is_err() {
        return Ok(());
    }
    let config = Config::load(home);
    let Ok(mut db) = Db::open(&home::db_path(home), Role::Reduce) else {
        return Ok(());
    };
    let opts = ReduceOptions {
        deadline: Some(Instant::now() + Duration::from_millis(REDUCE_DEADLINE_MS)),
        batch: reducer::BATCH,
        spool_dir: Some(home::spool_dir(home)),
        render: config.render(),
    };
    match reducer::reduce(&mut db.conn, &opts) {
        Ok(stats) => {
            log::debug(
                Some(home),
                "reduce",
                None,
                format!(
                    "processed {} spool {}",
                    stats.processed, stats.spool_ingested
                ),
            );
            Ok(())
        }
        Err(DbError::Busy) => Ok(()),
        Err(e) => Err(format!("reduce failed: {e}")),
    }
}
