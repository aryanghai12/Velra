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
/// Stdin a `UserPromptSubmit` is parsed from. Larger input is not parsed:
/// the event records only that a prompt of that size arrived
/// ([`user_prompt_oversize`]).
///
/// Set from measurement on the release build (DECISIONS D81): parsing a
/// prompt's JSON costs up to ~4 ms per MB when the text is escape-heavy
/// (`\n` everywhere), so a prompt near the 64 MiB cap spent the whole 250 ms
/// watchdog in reading and parsing, before any record of it could be armed,
/// and was lost outright. At this bound the worst shape measured, read,
/// parsed, examined and written, finishes in about two thirds of the
/// deadline. A prompt of 16 MiB is some four million tokens: no model's
/// context holds it, so no turn anyone can take is cut off.
const PROMPT_MAX_STDIN: usize = 16 * 1024 * 1024;
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

/// The event currently being persisted. The watchdog spools it on the way
/// out, so a deadline that fires mid-write still loses nothing (§10.3).
static PENDING: Mutex<Option<(PathBuf, NewEvent)>> = Mutex::new(None);

fn arm_pending(dir: PathBuf, ev: &NewEvent) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((dir, ev.clone()));
}

fn disarm_pending() {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Writes any armed event to the spool. Duplicates are harmless: ingestion
/// deduplicates on `dedupe_key`.
fn flush_pending() {
    let taken = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some((dir, ev)) = taken {
        let _ = spool::write(&dir, &ev);
    }
}

fn start_watchdog(ms: u64) {
    let _ = std::thread::Builder::new()
        .name("velra-watchdog".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            // Wait for any in-flight write, then leave without further work.
            let _guard = EMITTED.lock().unwrap_or_else(|e| e.into_inner());
            flush_pending();
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

/// Stdin up to `cap` bytes, and how many bytes arrived in all.
fn read_stdin_capped(cap: usize) -> (Vec<u8>, usize) {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 64 * 1024];
    let mut stdin = std::io::stdin().lock();
    let mut total = 0usize;
    loop {
        match stdin.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if buf.len() < cap {
                    let take = n.min(cap - buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                }
                // Beyond the cap we keep draining so the writer never blocks.
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (buf, total)
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

/// The watchdog deadline, overridable only in fault-injection builds so that
/// storage tests can saturate the machine without the deadline firing.
fn watchdog_ms(default: u64) -> u64 {
    #[cfg(feature = "fault-injection")]
    if let Ok(ms) = std::env::var("VELRA_TEST_WATCHDOG_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return ms;
        }
    }
    default
}

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
    start_watchdog(watchdog_ms(if command == "reduce" {
        WATCHDOG_REDUCE_MS
    } else {
        WATCHDOG_SYNC_MS
    }));
    let started = Instant::now();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        dispatch(&sub, ts_ms, home.as_deref(), &started)
    }));
    // A panic between arming and persisting still leaves the event spooled.
    flush_pending();
    if let Ok(Err(e)) = outcome {
        log::error(home.as_deref(), &label, None, e);
    }
    log::debug(
        home.as_deref(),
        &label,
        None,
        format!("{} us{}", started.elapsed().as_micros(), phases()),
    );
}

/// Per-phase timing for `VELRA_LOG=debug`. A mark costs one `Instant::now()`
/// and is only recorded, or formatted, when debug logging is on.
struct Phases {
    cursor: Instant,
    items: Vec<(&'static str, u128)>,
}

static PHASES: Mutex<Option<Phases>> = Mutex::new(None);

/// Records the time since the previous mark. `origin` seeds the cursor for
/// the first mark of the process.
fn mark(name: &'static str, origin: &Instant) {
    if !log::debug_enabled() {
        return;
    }
    let mut guard = PHASES.lock().unwrap_or_else(|e| e.into_inner());
    let phases = guard.get_or_insert_with(|| Phases {
        cursor: *origin,
        items: Vec::new(),
    });
    let now = Instant::now();
    let delta = now.saturating_duration_since(phases.cursor);
    phases.items.push((name, delta.as_micros()));
    phases.cursor = now;
}

fn phases() -> String {
    let taken = PHASES.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(phases) = taken else {
        return String::new();
    };
    let mut out = String::new();
    for (name, us) in phases.items {
        out.push_str(&format!(" {name}={us}us"));
    }
    out
}

fn dispatch(sub: &str, ts_ms: i64, home: Option<&Path>, started: &Instant) -> Result<(), String> {
    fault_injection(sub);
    let Some(home) = home else { return Ok(()) };
    let cap = if sub == "user-prompt-submit" {
        PROMPT_MAX_STDIN
    } else {
        MAX_STDIN
    };
    let (raw, total) = read_stdin_capped(cap);
    mark("stdin", started);
    if sub == "reduce" {
        return run_reduce(home, ts_ms);
    }
    if sub == "user-prompt-submit" && total > cap {
        return user_prompt_oversize(home, ts_ms, &raw, total);
    }
    let parsed = normalize::parse(&raw);
    mark("parse", started);
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
    mark("context", started);
    if malformed {
        let payload = Payload::default();
        let mut ev = ctx.new_event(payload);
        ev.hook_event = he::MALFORMED.to_string();
        ctx.store(ev, Role::HookAppend);
        return Ok(());
    }
    let result = match sub {
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
    };
    mark("handler", started);
    result
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
        let root_str = velra_core::workspace::root_string(&canonical);
        let project_id = velra_core::workspace::id(&root_str);
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
        disarm_pending();
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
        let at = Instant::now();
        let opened = Db::open(&home::db_path(self.home), role);
        mark("db-open", &at);
        match opened {
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
        arm_pending(home::spool_dir(self.home), ev);
        let at = Instant::now();
        let appended = eventlog::append(&mut db.conn, ev);
        mark("db-append", &at);
        match appended {
            Ok(id) => {
                disarm_pending();
                id
            }
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
        arm_pending(home::spool_dir(self.home), &ev);
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

    /// Attempts to deliver this workspace's staged capsule (`velra restore`).
    ///
    /// The workspace key is `self.project_id`, which comes from
    /// `velra_core::workspace` — the very same function `velra restore` used
    /// to decide where to write. The lookup is therefore a path join, not a
    /// search, and the two cannot drift apart.
    ///
    /// Eligibility is not decided here. `claim_with` compares `source` against
    /// the capsule's own `deliver_on`, so a record staged for a source this
    /// build has never heard of is left alone rather than mis-delivered.
    ///
    /// Fail-open throughout: nothing below can change the exit code, write to
    /// stderr, or put anything on stdout except the one delivery object.
    fn deliver_staged(&self, source: &str) {
        use velra_core::staging::ClaimError;
        let now = self.ts_ms;

        // Set by the emit closure so the outcome can tell "Claude Code would
        // have truncated this" apart from "something else already emitted".
        let oversized = std::cell::Cell::new(false);

        let emit_staged = |c: &velra_core::staging::StagedCapsule| {
            // A capsule this large would be cut off by Claude Code's hook
            // output limit, and half a capsule is worse than none. `velra
            // restore` cannot produce one — the renderer enforces the token
            // budget — so this means a tampered file or a foreign build.
            let chars = c.capsule.chars().count();
            if chars > crate::compat::CAPSULE_MAX_CHARS {
                oversized.set(true);
                log::warn(
                    Some(self.home),
                    &self.label,
                    Some(&self.session_id),
                    format!(
                        "staged capsule is {chars} chars, above the {} char margin under \
                         Claude Code's {} char hook output limit; not delivered",
                        crate::compat::CAPSULE_MAX_CHARS,
                        crate::compat::HOOK_OUTPUT_MAX_CHARS
                    ),
                );
                return false;
            }
            // The only write to stdout in this path, and the only one the
            // process permits at all: `emit` holds a once-guard.
            emit(&staged_delivery_json(c))
        };

        match velra_core::staging::claim_with(self.home, &self.project_id, source, now, emit_staged)
        {
            Ok(c) => log::debug(
                Some(self.home),
                &self.label,
                Some(&self.session_id),
                format!(
                    "restored staged capsule from session {} ({} tokens, {})",
                    c.source_session_id, c.tokens, c.content_hash
                ),
            ),
            // Routine, and silent by design. Nothing staged is the overwhelming
            // majority of session starts; a capsule waiting for another source
            // is every `/clear`, compaction and resume in between. Logging
            // either would turn the log into noise and hide the rest.
            Err(ClaimError::Empty) | Err(ClaimError::NotForThisSource { .. }) => {}
            // Someone else is mid-claim, or we declined to emit. Both leave the
            // capsule staged and recoverable on the next session start.
            Err(ClaimError::Busy) => {}
            Err(ClaimError::NotEmitted) if oversized.get() => {}
            Err(ClaimError::NotEmitted) => log::debug(
                Some(self.home),
                &self.label,
                Some(&self.session_id),
                "staged capsule not emitted; another object had already been written".to_string(),
            ),
            // Stale, malformed, corrupt or filed under another workspace. Worth
            // a line in the log, and nothing else: the session still starts.
            Err(e) => log::warn(
                Some(self.home),
                &self.label,
                Some(&self.session_id),
                format!("staged capsule not delivered: {e}"),
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

/// The delivery JSON for a staged capsule (§8.4), one line, no trailing
/// newline.
///
/// Identical in shape to `continuation::delivery_json` — Claude Code must not
/// be able to tell the two apart, because they are the same kind of thing
/// arriving by a different route. Only the `systemMessage` differs, and only
/// because the user deserves to be told *which* session this came out of:
/// they chose it by hand, possibly days ago.
fn staged_delivery_json(c: &velra_core::staging::StagedCapsule) -> String {
    let short: String = c.source_session_id.chars().take(8).collect();
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": he::SESSION_START,
            "additionalContext": c.capsule,
        },
        "systemMessage": if c.summary.is_empty() {
            format!(
                "\u{26a1} Velra restored task state from session {short} ({} tokens)",
                c.tokens
            )
        } else {
            format!(
                "\u{26a1} Velra restored: {} \u{2014} from session {short} ({} tokens)",
                c.summary, c.tokens
            )
        },
    })
    .to_string()
}

/// §8.2: `CLAUDE_PROJECT_DIR`, else the first ancestor with `.git`, else cwd.
///
/// Delegates to `velra_core::workspace` so the hook and `velra restore` cannot
/// drift apart about which project this is — see that module for why the
/// consequence of drift would be silent.
fn project_root(cwd: Option<&str>) -> PathBuf {
    velra_core::workspace::root(cwd)
}

/// Hashes the files edited in this session's current epoch (§8.3), including
/// edits that the reducer has not processed yet.
fn hash_session_files(
    conn: &rusqlite::Connection,
    session_id: &str,
    root: &Path,
) -> Vec<FileObservation> {
    hash_session_files_until(conn, session_id, root, None)
}

/// [`hash_session_files`], stopping at `deadline`: the files not reached are
/// not observed, and absent from the result rather than guessed at.
fn hash_session_files_until(
    conn: &rusqlite::Connection,
    session_id: &str,
    root: &Path,
    deadline: Option<Instant>,
) -> Vec<FileObservation> {
    let Ok(paths_) = velra_core::reducer::scan_paths(conn, session_id, GIT_HASH_MAX_FILES) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(paths_.len());
    for path in paths_ {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let (hash, size) = hash::hash_file(&paths::resolve(&path, root));
        out.push(FileObservation { path, hash, size });
    }
    out
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
    // Channel 1: the in-session continuation, which needs the ledger.
    if let Some(mut db) = ctx.store(ev, Role::HookDelivery) {
        match source.as_str() {
            // T6
            "clear" => {
                let _ = continuation::expire_live(&db.conn, &ctx.session_id, ctx.ts_ms);
            }
            "compact" | "resume" => {
                ctx.reconcile(&mut db);
                ctx.deliver(&mut db, Channel::SessionStart);
            }
            _ => {}
        }
    }
    // Channel 1b: a capsule the user staged with `velra restore`.
    //
    // Deliberately outside the block above. Staged delivery reads one file and
    // touches no table, so a database that is locked, missing or unopenable
    // must not take it down with it — the event itself has already gone to the
    // spool by this point, and the user's restore is the last thing that should
    // be lost to a lock they will never hear about.
    //
    // It runs *after* the continuation so that on `compact`/`resume`, where
    // both could in principle fire, the in-session path wins: its exactly-once
    // accounting lives in the database, whereas a staged capsule that loses the
    // race is simply left staged by the `emit` refusal.
    //
    // Which sources are eligible is not decided here. `claim_with` compares the
    // source against the capsule's own `deliver_on`, so the three arms above
    // leave a `velra restore` capsule — which names `startup` — untouched.
    ctx.deliver_staged(&source);
    Ok(())
}

fn user_prompt_submit(ctx: &Ctx<'_>) -> Result<(), String> {
    // Split, redact, extract, then bound: a cut or a redaction applied to the
    // whole prompt can remove an injected block's closing tag and turn the
    // block into the user's words, and state extracted from a cut copy loses
    // everything past the cut (`velra_core::prompt::for_storage`). The facts
    // read from the whole prompt are stored beside the bounded text.
    //
    // Processing is bounded (`prompt::SCAN_LIMIT`, `prompt::MAX_EDGE_BLOCKS`)
    // and measured well inside the watchdog, but the watchdog, not the
    // measurement, is the guarantee: until the real event is armed, an event
    // that records only that a prompt of this size arrived is. A deadline that
    // fires mid-processing then spools that record instead of nothing.
    if let Some(p) = ctx.input.prompt.as_deref().filter(|p| !p.is_empty()) {
        let placeholder = Payload {
            prompt_truncated: Some(true),
            prompt_facts: Some(velra_core::prompt::unprocessed_record(p.len())),
            prompt_id: ctx.input.prompt_id.clone(),
            ..Default::default()
        };
        arm_pending(home::spool_dir(ctx.home), &ctx.new_event(placeholder));
    }
    // A deadline that fires while the prompt is being processed, on demand.
    #[cfg(feature = "fault-injection")]
    if let Some(ms) = std::env::var("VELRA_TEST_STALL_PROMPT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(ms));
    }
    let stored = ctx
        .input
        .prompt
        .as_deref()
        .map(|p| velra_core::prompt::for_storage(p, limits::PROMPT));
    // `Stored::omitted` lists at most `prompt::OMITTED_LISTED` blocks; the
    // facts count them all, so a prompt of thousands of tiny blocks cannot
    // turn the record of what was left out into the bulk of the payload.
    let omitted = stored.as_ref().map(|s| {
        s.omitted
            .iter()
            .map(|(tag, bytes)| velra_core::event::OmittedBlock {
                tag: tag.clone(),
                bytes: *bytes as u64,
            })
            .collect::<Vec<_>>()
    });
    let payload = Payload {
        prompt_truncated: stored
            .as_ref()
            .and_then(|s| s.authored_truncated.then_some(true)),
        prompt_facts: stored.as_ref().map(velra_core::prompt::facts_record),
        prompt: stored.map(|s| s.text),
        prompt_omitted: omitted.filter(|o| !o.is_empty()),
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

/// A `UserPromptSubmit` whose input exceeds [`PROMPT_MAX_STDIN`]: recorded
/// without parsing, as a prompt of `total` bytes that was not processed.
///
/// Nothing of the prompt is stored -- none of it has been redacted or
/// classified -- and nothing is delivered. What the event keeps is that a
/// prompt arrived, how large it was, and whose session and workspace it
/// belongs to, read from the raw input (`normalize::salvage_json_string`).
/// Without a session id there is nothing to attach it to, as for any hook.
fn user_prompt_oversize(home: &Path, ts_ms: i64, raw: &[u8], total: usize) -> Result<(), String> {
    let Some(session_id) = normalize::salvage_session_id(raw).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if home::ensure_home(home).is_err() {
        return Ok(());
    }
    let input = HookInput {
        cwd: normalize::salvage_json_string(raw, "cwd"),
        prompt_id: normalize::salvage_json_string(raw, "prompt_id"),
        agent_id: normalize::salvage_json_string(raw, "agent_id"),
        ..Default::default()
    };
    let ctx = Ctx::new(home, "user-prompt-submit", ts_ms, input, session_id, false);
    let payload = Payload {
        prompt_truncated: Some(true),
        prompt_facts: Some(velra_core::prompt::unprocessed_record(total)),
        prompt_id: ctx.input.prompt_id.clone(),
        ..Default::default()
    };
    ctx.store(ctx.new_event(payload), Role::HookAppend);
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
        }
        // Observed for failed calls as well: `git restore x && pytest` fails
        // as a whole whenever the suite still fails, and that is the usual
        // shape of discarding an attempt. The file hashes below are taken
        // after the call returned either way (D57).
        let effects = shell::git_effects(&command, &|p| ctx.resolve(p).is_file());
        if effects.any() {
            git_effects = Some(effects);
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

/// Time the Stop hook spends hashing files for its turn-end scan.
const STOP_SCAN_BUDGET_MS: u64 = 80;

fn stop(ctx: &Ctx<'_>) -> Result<(), String> {
    let mut payload = Payload {
        stop_hook_active: ctx.input.stop_hook_active,
        ..Default::default()
    };
    // The turn-end scan is taken here, when the turn ends, and carried in the
    // event. The reducer used to hash the disk whenever it ran -- possibly
    // long after, and after the files had changed again -- and file what it
    // found under this event's time, so present content posed as the state
    // the turn ended in. Without a database there is no list of files to
    // scan, and the event says no observation was made (`turn_scan: None`).
    let Some(mut db) = ctx.open_db(Role::HookAppend) else {
        ctx.spool(&ctx.new_event(payload));
        return Ok(());
    };
    // Until the scan is done, a deadline spools the event without one.
    arm_pending(home::spool_dir(ctx.home), &ctx.new_event(payload.clone()));
    let deadline = Instant::now() + Duration::from_millis(STOP_SCAN_BUDGET_MS);
    payload.turn_scan = Some(hash_session_files_until(
        &db.conn,
        &ctx.session_id,
        &ctx.root,
        Some(deadline),
    ));
    ctx.append_with(&mut db, &ctx.new_event(payload));
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
