//! User-facing commands (§7). The hook path never reaches this module.

use crate::compat::{self, Features};
use crate::home::{self, Config, State};
use crate::inspect::{self, Section};
use crate::restore as restore_ui;
use crate::settings;
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;
use velra_core::db::{Db, Role};
use velra_core::reducer;

pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("VELRA_GIT_SHA"),
    ", ",
    env!("VELRA_TARGET"),
    ")"
);

#[derive(Parser)]
#[command(
    name = "velra",
    version = VERSION,
    about = "Local-first session continuity for Claude Code: clear the context, keep the state.",
    max_term_width = 100
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register Velra's hooks in your Claude Code settings.
    Enable {
        /// Print the change as a diff without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove Velra's hooks from your Claude Code settings.
    Disable {
        /// Also delete ~/.velra (backups are kept).
        #[arg(long)]
        purge: bool,
        /// Skip the confirmation prompt for --purge.
        #[arg(long)]
        yes: bool,
        /// Print the change as a diff without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show whether Velra is enabled and what it is tracking.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Show what would survive a /compact right now.
    Inspect {
        /// Session to inspect (defaults to this project's most recent).
        #[arg(long)]
        session: Option<String>,
        /// Use the most recently active session on this machine.
        #[arg(long)]
        last: bool,
        /// Print a frozen capsule instead of a live preview.
        #[arg(long)]
        checkpoint: Option<String>,
        /// Full detail for one section: dead-ends, failure, files, attempts.
        #[arg(long)]
        section: Option<String>,
        /// Trace a string (a test name, symbol, path) through every layer from
        /// the recorded events to the capsule, and report where it was lost.
        /// Repeatable.
        #[arg(long, value_name = "MARKER")]
        trace: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Diagnose the installation.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Carry a previous session's task state into your next one.
    ///
    /// Pick a session this workspace has seen before; Velra stages its
    /// operational state so a brand-new Claude Code session can pick up where
    /// it left off. Nothing is read from the old conversation.
    Restore {
        /// Restore this session id instead of showing the picker.
        #[arg(long)]
        session: Option<String>,
        /// List the sessions this workspace can restore from, and stop.
        #[arg(long)]
        list: bool,
        /// Print the capsule that would be staged without staging it.
        #[arg(long)]
        dry_run: bool,
        /// Discard whatever is currently staged for this workspace.
        #[arg(long)]
        clear: bool,
        #[arg(long)]
        json: bool,
    },
}

// ------------------------------------------------------------------- output

fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(text: &str, code: &str) -> String {
    if color_enabled() {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

fn ok_mark() -> String {
    paint("\u{2713}", "32")
}
fn warn_mark() -> String {
    paint("!", "33")
}
fn fail_mark() -> String {
    paint("\u{2717}", "31")
}

enum Check {
    Ok(String),
    Warn(String),
    Fail(String),
}

impl Check {
    fn print(&self) {
        match self {
            Check::Ok(m) => println!("{} {m}", ok_mark()),
            Check::Warn(m) => println!("{} {m}", warn_mark()),
            Check::Fail(m) => println!("{} {m}", fail_mark()),
        }
    }

    fn level(&self) -> &'static str {
        match self {
            Check::Ok(_) => "ok",
            Check::Warn(_) => "warn",
            Check::Fail(_) => "fail",
        }
    }

    fn message(&self) -> &str {
        match self {
            Check::Ok(m) | Check::Warn(m) | Check::Fail(m) => m,
        }
    }
}

// ------------------------------------------------------------- binary path

/// Stable path to register in settings (§5.3): prefer a PATH entry that
/// resolves to this executable without following symlinks, when the canonical
/// path lives inside a versioned package-manager directory.
pub fn stable_bin_path() -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("velra"));
    let canonical = velra_core::paths::canonical(&exe).unwrap_or_else(|| exe.clone());
    let canonical_str = canonical.to_string_lossy().replace('\\', "/");
    let versioned = [
        "/cellar/",
        "/nix/store/",
        "/registry/src/",
        "/.cargo/registry/",
        "/versions/",
        "/pkgs/",
    ]
    .iter()
    .any(|marker| canonical_str.to_lowercase().contains(marker));
    if versioned {
        if let Some(path_entry) = path_entry_resolving_to(&canonical) {
            return path_entry.to_string_lossy().into_owned();
        }
    }
    canonical.to_string_lossy().into_owned()
}

fn path_entry_resolving_to(canonical: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "velra.exe" } else { "velra" };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if !candidate.exists() {
            continue;
        }
        if velra_core::paths::canonical(&candidate).as_deref() == Some(canonical) {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------- commands

pub fn run() -> i32 {
    match Cli::parse().command {
        Command::Enable { dry_run } => cmd_enable(dry_run),
        Command::Disable {
            purge,
            yes,
            dry_run,
        } => cmd_disable(purge, yes, dry_run),
        Command::Status { json } => cmd_status(json),
        Command::Inspect {
            session,
            last,
            checkpoint,
            section,
            trace,
            json,
        } => cmd_inspect(session, last, checkpoint, section, trace, json),
        Command::Doctor { json } => cmd_doctor(json),
        Command::Restore {
            session,
            list,
            dry_run,
            clear,
            json,
        } => cmd_restore(session, list, dry_run, clear, json),
    }
}

fn require_home() -> Option<PathBuf> {
    match home::velra_home() {
        Some(h) => Some(h),
        None => {
            println!(
                "{} Could not determine your home directory. Set VELRA_HOME and retry.",
                fail_mark()
            );
            None
        }
    }
}

fn cmd_enable(dry_run: bool) -> i32 {
    let Some(home) = require_home() else { return 1 };
    let Some(settings_path) = settings::settings_path() else {
        println!(
            "{} Could not determine the Claude Code settings path.",
            fail_mark()
        );
        return 1;
    };
    if let Err(e) = home::ensure_home(&home) {
        println!("{} Could not create {}: {e}", fail_mark(), home.display());
        return 1;
    }
    // §6.2 step 1: create the settings directory if Claude Code is absent.
    let mut claude_missing = false;
    if let Some(dir) = settings_path.parent() {
        if !dir.exists() {
            claude_missing = true;
            if !dry_run {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    println!("{} Could not create {}: {e}", fail_mark(), dir.display());
                    return 1;
                }
            }
        }
    }
    let detection = compat::detect(Duration::from_secs(3));
    let features = Features::for_version(detection.version());
    let bin = stable_bin_path();
    match settings::enable(&settings_path, &home, &bin, &features, dry_run) {
        Ok(outcome) => {
            if let Some(diff) = outcome.diff {
                if diff.trim().is_empty() {
                    println!("{} Velra already enabled; no changes.", ok_mark());
                } else {
                    print!("{diff}");
                    println!("\n(dry run: nothing was written)");
                }
                return 0;
            }
            if claude_missing {
                println!("{} Claude Code not detected; hooks registered and will activate when it is installed.", warn_mark());
            }
            // The state file is refreshed even when the settings file needed
            // no change, so that a moved binary is repaired by `velra enable`.
            let mut state = State::load(&home);
            if state.hooks_existed_before.is_none() {
                state.hooks_existed_before = Some(outcome.hooks_existed_before);
            }
            state.bin_path = Some(bin.clone());
            state.settings_path = Some(outcome.target.display().to_string());
            state.claude_version = detection.version().map(|v| v.to_string());
            state.velra_version = Some(env!("CARGO_PKG_VERSION").to_string());
            if outcome.changed {
                state.enabled_ms = Some(velra_core::time::now_ms());
                state.last_backup = outcome.backup.as_ref().map(|p| p.display().to_string());
            }
            let _ = state.save(&home);

            if !outcome.changed {
                println!("{} Velra already enabled.", ok_mark());
                return 0;
            }

            println!("{} Velra enabled for Claude Code.", ok_mark());
            if outcome.created_file {
                println!("  Created {}", outcome.target.display());
            }
            match &outcome.backup {
                Some(b) => println!(
                    "  Settings: {}  (backup: {})",
                    outcome.target.display(),
                    b.display()
                ),
                None => println!("  Settings: {}", outcome.target.display()),
            }
            if !outcome.changes.skipped.is_empty() {
                println!(
                    "  Skipped (needs a newer Claude Code, {}): {}",
                    detection.describe(),
                    outcome.changes.skipped.join(", ")
                );
            }
            if outcome.disable_all_hooks {
                println!(
                    "{} \"disableAllHooks\": true is set, so no hooks run until you remove it.",
                    warn_mark()
                );
            }
            println!();
            println!("Nothing else required. Keep coding normally.");
            println!("Tip: run `velra inspect` any time to see what would survive a /compact.");
            0
        }
        Err(e) => {
            println!("{} {e}", fail_mark());
            1
        }
    }
}

fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn cmd_disable(purge: bool, yes: bool, dry_run: bool) -> i32 {
    let Some(home) = require_home() else { return 1 };
    let Some(settings_path) = settings::settings_path() else {
        println!(
            "{} Could not determine the Claude Code settings path.",
            fail_mark()
        );
        return 1;
    };
    let state = State::load(&home);
    // Remove the `hooks` key only if it did not exist before `enable` (§6.4).
    let remove_hooks_key = state.hooks_existed_before == Some(false);
    match settings::disable(&settings_path, &home, remove_hooks_key, dry_run) {
        Ok(outcome) => {
            if let Some(diff) = outcome.diff {
                if diff.trim().is_empty() {
                    println!("{} Velra is not enabled; no changes.", ok_mark());
                } else {
                    print!("{diff}");
                    println!("\n(dry run: nothing was written)");
                }
                return 0;
            }
            if outcome.changed {
                println!(
                    "{} Velra disabled. Claude Code is otherwise untouched.",
                    ok_mark()
                );
                if let Some(b) = &outcome.backup {
                    println!(
                        "  Settings: {}  (backup: {})",
                        outcome.target.display(),
                        b.display()
                    );
                }
            } else {
                println!("{} Velra was not enabled; nothing to remove.", ok_mark());
            }
            if purge {
                if !yes && !confirm(&format!("Delete {} (backups are kept)?", home.display())) {
                    println!("  Kept {}. Re-run with --yes to delete it.", home.display());
                    return 0;
                }
                match purge_home(&home) {
                    Ok(()) => println!("{} Removed {} (backups kept).", ok_mark(), home.display()),
                    Err(e) => {
                        println!(
                            "{} Could not fully remove {}: {e}",
                            warn_mark(),
                            home.display()
                        );
                        return 1;
                    }
                }
            }
            0
        }
        Err(e) => {
            println!("{} {e}", fail_mark());
            1
        }
    }
}

/// Deletes `$VELRA_HOME` except `backups/`.
fn purge_home(home: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(home)? {
        let entry = entry?;
        if entry.file_name() == "backups" {
            continue;
        }
        let path = entry.path();
        let result = if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        result?;
    }
    Ok(())
}

struct Installed {
    handlers: Vec<settings::InstalledHandler>,
    settings_text: Option<String>,
    settings_path: Option<PathBuf>,
}

fn read_installed() -> Installed {
    let path = settings::settings_path().map(|p| settings::resolve_target(&p));
    let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    let handlers = text
        .as_deref()
        .map(settings::installed_handlers)
        .unwrap_or_default();
    Installed {
        handlers,
        settings_text: text,
        settings_path: path,
    }
}

fn open_db_ro(home: &Path) -> Option<Db> {
    let path = home::db_path(home);
    path.exists()
        .then(|| Db::open(&path, Role::Cli).ok())
        .flatten()
}

fn cmd_status(json: bool) -> i32 {
    let Some(home) = require_home() else { return 1 };
    let installed = read_installed();
    let detection = compat::detect(Duration::from_secs(3));
    let features = Features::for_version(detection.version());
    let expected = settings::REGISTRATIONS
        .iter()
        .filter(|r| (r.supported)(&features))
        .count();
    let bin = State::load(&home).bin_path;
    let bin_ok = bin
        .as_deref()
        .map(|b| Path::new(b).exists())
        .unwrap_or(false);
    let enabled = !installed.handlers.is_empty();

    // A database the hooks cannot use -- a newer schema, a migration that
    // cannot complete -- means every hook is a no-op that spools; status is
    // not healthy then, whatever the settings say (DECISIONS D118).
    let (db, database_error) = if home::db_path(&home).exists() {
        match Db::open(&home::db_path(&home), Role::Cli) {
            Ok(db) => (Some(db), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (None, None)
    };
    let mut sessions = 0i64;
    let mut events = 0i64;
    let mut last_event_ms = 0i64;
    let mut live: Vec<(String, String)> = Vec::new();
    let mut latest: Option<velra_core::continuation::Latest> = None;
    if let Some(db) = &db {
        sessions = db
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);
        events = db
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap_or(0);
        last_event_ms = db
            .conn
            .query_row("SELECT COALESCE(MAX(ts_ms), 0) FROM events", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        if let Ok(mut stmt) = db.conn.prepare(
            "SELECT session_id, state FROM continuations WHERE state IN ('PENDING', 'ATTACHED') ORDER BY updated_ms DESC",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                live = rows.flatten().collect();
            }
        }
        latest = velra_core::continuation::latest(&db.conn).ok().flatten();
    }
    let db_size = std::fs::metadata(home::db_path(&home))
        .map(|m| m.len())
        .unwrap_or(0);
    let healthy = enabled
        && bin_ok
        && installed.handlers.len() >= expected.min(1)
        && database_error.is_none();

    if json {
        let value = serde_json::json!({
            "enabled": enabled,
            "handlers": installed.handlers.len(),
            "expected_handlers": expected,
            "binary": bin,
            "binary_exists": bin_ok,
            "claude_code": detection.describe(),
            "db_path": home::db_path(&home).display().to_string(),
            "db_bytes": db_size,
            "sessions": sessions,
            "events": events,
            "last_event_age": (last_event_ms > 0).then(|| velra_core::time::human_age(velra_core::time::now_ms() - last_event_ms)),
            "live_continuations": live.iter().map(|(s, st)| serde_json::json!({"session": s, "state": st})).collect::<Vec<_>>(),
            "latest_continuation": latest.as_ref().map(|l| serde_json::json!({
                "session": l.session_id,
                "checkpoint": l.checkpoint_id,
                "state": l.state,
                "channel": l.attached_channel,
                "attach_count": l.attach_count,
                "meaning": velra_core::continuation::meaning(&l.state),
            })),
            "database_error": database_error,
            "healthy": healthy,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return i32::from(!healthy);
    }

    if enabled {
        println!(
            "{} Enabled ({} hook handlers registered)",
            ok_mark(),
            installed.handlers.len()
        );
    } else {
        println!("{} Not enabled. Run `velra enable`.", fail_mark());
    }
    println!("  Claude Code: {}", detection.describe());
    match (&bin, bin_ok) {
        (Some(b), true) => println!("  Binary:      {b}"),
        (Some(b), false) => println!(
            "{} Binary:      {b} (missing — run `velra enable` to repair)",
            warn_mark()
        ),
        (None, _) => println!("  Binary:      (not recorded; run `velra enable`)"),
    }
    match &database_error {
        None => println!(
            "  Database:    {} ({} KiB)",
            home::db_path(&home).display(),
            db_size / 1024
        ),
        Some(e) => println!(
            "{} Database:    {} will not open: {e}. Hooks record nothing until it does; see `velra doctor`.",
            fail_mark(),
            home::db_path(&home).display()
        ),
    }
    println!("  Tracking:    {sessions} session(s), {events} event(s)");
    if last_event_ms > 0 {
        println!(
            "  Last event:  {} ago",
            velra_core::time::human_age(velra_core::time::now_ms() - last_event_ms)
        );
    } else {
        println!("  Last event:  none yet");
    }
    if live.is_empty() {
        println!("  Continuations: none live");
    } else {
        for (session, state) in &live {
            println!("  Continuation: {session} {state}");
        }
    }
    // The last one in any state: whether the previous `/compact` was written
    // out, and what that does and does not establish (DECISIONS D113).
    if let Some(l) = &latest {
        println!(
            "  Last continuation: {} {}{} \u{2014} {}",
            l.session_id.chars().take(8).collect::<String>(),
            l.state,
            l.attached_channel
                .as_deref()
                .map(|c| format!(" on {c}"))
                .unwrap_or_default(),
            velra_core::continuation::meaning(&l.state)
        );
    }
    // A staged capsule is invisible everywhere else — it is one file outside
    // the repository — so `status` is where a user finds out that their next
    // session in this workspace is going to start with restored state.
    let (workspace_id, workspace_root) = workspace_for_cwd(db.as_ref().map(|d| &d.conn));
    let staged_dir = velra_core::staging::staged_dir(&home, &workspace_id);
    let summary = |c: &velra_core::staging::StagedCapsule| {
        format!(
            "{} ({} tokens) from session {}",
            if c.summary.is_empty() {
                "task state"
            } else {
                &c.summary
            },
            c.tokens,
            c.source_session_id.chars().take(8).collect::<String>()
        )
    };
    match velra_core::staging::slot(&staged_dir, velra_core::time::now_ms()) {
        velra_core::staging::Slot::Empty => {}
        velra_core::staging::Slot::Staged(c) => {
            println!("  Staged:      {}", summary(&c));
            println!(
                "               delivered on SessionStart({}) \u{2014} start a new session in {workspace_root} to pick it up",
                c.deliver_on.join("|")
            );
        }
        velra_core::staging::Slot::Claimed {
            capsule,
            interrupted: true,
        } => {
            println!(
                "{} Staged:      {} was claimed by a session start that did not finish.",
                warn_mark(),
                capsule.as_ref().map_or("a capsule".to_string(), summary)
            );
            println!(
                "               It may already have been delivered, so it will not be delivered again; run `velra restore` to stage it again."
            );
        }
        velra_core::staging::Slot::Claimed { .. } => {
            println!("  Staged:      being delivered to a session that is starting now");
        }
        velra_core::staging::Slot::Unreadable => println!(
            "{} Staged:      unreadable; it will not be delivered (`velra restore --clear` removes it)",
            warn_mark()
        ),
    }
    if velra_core::staging::has_legacy(&staged_dir) {
        println!(
            "{} Staged:      a capsule in an earlier development format is ignored; run `velra restore` again.",
            warn_mark()
        );
    }
    i32::from(!healthy)
}

fn cmd_inspect(
    session: Option<String>,
    last: bool,
    checkpoint: Option<String>,
    section: Option<String>,
    trace: Vec<String>,
    json: bool,
) -> i32 {
    let Some(home) = require_home() else { return 1 };
    let section = match section.as_deref().map(Section::parse) {
        Some(None) => {
            println!(
                "{} Unknown section. Use dead-ends, failure, files or attempts.",
                fail_mark()
            );
            return 1;
        }
        other => other.flatten(),
    };
    let Some(mut db) = open_db_ro(&home) else {
        println!(
            "{} No Velra database yet at {}.",
            warn_mark(),
            home::db_path(&home).display()
        );
        println!("  Start Claude Code with Velra enabled and try again.");
        return 1;
    };
    let config = Config::load(&home);
    let tz = velra_core::time::local_offset_secs(velra_core::time::now_ms());

    if let Some(id) = checkpoint {
        let Ok(Some(stored)) = velra_core::checkpoint::load(&db.conn, &id) else {
            println!("{} No checkpoint {id}.", fail_mark());
            return 1;
        };
        if let Some(section) = section {
            match inspect::section_detail(
                &db.conn,
                &stored.session_id,
                stored.epoch,
                stored.created_ms,
                section,
                tz,
            ) {
                Ok(text) => println!("{text}"),
                Err(e) => {
                    println!("{} {e}", fail_mark());
                    return 1;
                }
            }
            return 0;
        }
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&inspect::checkpoint_json(&stored))
                    .unwrap_or_default()
            );
        } else {
            println!("{}", stored.capsule);
        }
        return 0;
    }

    // Live preview: run a reducer pass first, but never create a checkpoint.
    if let Err(e) = reducer::reduce_all(&mut db.conn, Some(&home::spool_dir(&home))) {
        println!("{} Could not catch up the reducer: {e}", warn_mark());
    }
    let session_id = match session {
        Some(s) => Some(s),
        None if last => inspect::latest_session(&db.conn).ok().flatten(),
        None => {
            let (pid, _) = workspace_for_cwd(Some(&db.conn));
            let by_project = inspect::latest_session_for_project(&db.conn, &pid)
                .ok()
                .flatten();
            match by_project {
                Some(s) => Some(s),
                None => inspect::latest_session(&db.conn).ok().flatten(),
            }
        }
    };
    let Some(session_id) = session_id else {
        println!("{} No sessions recorded yet.", warn_mark());
        return 1;
    };
    let now = velra_core::time::now_ms();
    if !trace.is_empty() {
        return match inspect::trace(&db.conn, &home, &session_id, now, &config.render(), &trace) {
            Ok(traces) => {
                if json {
                    let all: Vec<serde_json::Value> = traces.iter().map(|t| t.to_json()).collect();
                    println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
                } else {
                    for t in &traces {
                        println!("{}", t.report());
                    }
                }
                0
            }
            Err(e) => {
                println!("{} {e}", fail_mark());
                1
            }
        };
    }
    let snapshot = match inspect::preview(&db.conn, &session_id, now) {
        Ok(s) => s,
        Err(e) => {
            println!("{} {e}", fail_mark());
            return 1;
        }
    };
    if let Some(section) = section {
        match inspect::section_detail(&db.conn, &session_id, snapshot.epoch, now, section, tz) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                println!("{} {e}", fail_mark());
                return 1;
            }
        }
        return 0;
    }
    let rendered = velra_core::render::render(&snapshot, &config.render());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&inspect::snapshot_json(
                &snapshot,
                &rendered.text,
                rendered.tokens
            ))
            .unwrap_or_default()
        );
    } else {
        println!("{}", rendered.text);
    }
    0
}

/// Best-effort network-filesystem detection (WAL is unreliable there).
fn on_network_fs(path: &Path) -> bool {
    let display = path.to_string_lossy();
    if display.starts_with("\\\\") || display.starts_with("//") {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
            return false;
        };
        let mut best: Option<(usize, String)> = None;
        for line in mounts.lines() {
            let mut parts = line.split_whitespace();
            let (_dev, mount, fstype) = (parts.next(), parts.next(), parts.next());
            let (Some(mount), Some(fstype)) = (mount, fstype) else {
                continue;
            };
            if display.starts_with(mount) && best.as_ref().is_none_or(|(len, _)| mount.len() > *len)
            {
                best = Some((mount.len(), fstype.to_string()));
            }
        }
        best.is_some_and(|(_, fs)| {
            matches!(
                fs.as_str(),
                "nfs" | "nfs4" | "cifs" | "smbfs" | "afpfs" | "fuse.sshfs" | "9p"
            )
        })
    }
    #[cfg(not(target_os = "linux"))]
    false
}

// ---------------------------------------------------------------- restore

/// The workspace the current directory belongs to: `(project_id, root)`.
///
/// The hook's mapping (`velra_core::workspace::resolve`), except that inside a
/// repository the nearest directory the ledger recorded a session in wins
/// (`resolve_recorded`): Claude Code started in a subdirectory records it
/// as the workspace, and a terminal there has no `CLAUDE_PROJECT_DIR` to say
/// so. Without a ledger it is the plain mapping.
fn workspace_for_cwd(conn: Option<&rusqlite::Connection>) -> (String, String) {
    velra_core::workspace::resolve_recorded(None, |id| {
        conn.is_some_and(|c| velra_core::restore::is_recorded_workspace(c, id))
    })
}

fn cmd_restore(session: Option<String>, list: bool, dry_run: bool, clear: bool, json: bool) -> i32 {
    let Some(home) = require_home() else { return 1 };
    let now = velra_core::time::now_ms();

    if clear {
        let (workspace_id, workspace_root) =
            workspace_for_cwd(open_db_ro(&home).as_ref().map(|d| &d.conn));
        let removed = velra_core::staging::clear(&home, &workspace_id);
        println!(
            "{} {}",
            ok_mark(),
            if removed > 0 {
                format!("Discarded the staged capsule for {workspace_root}.")
            } else {
                format!("Nothing was staged for {workspace_root}.")
            }
        );
        return 0;
    }

    let Some(mut db) = open_db_ro(&home) else {
        println!(
            "{} No Velra database yet at {}.",
            warn_mark(),
            home::db_path(&home).display()
        );
        println!("  Start Claude Code with Velra enabled and try again.");
        return 1;
    };
    // Restore reads projections, so the log has to be caught up first, exactly
    // as `inspect` does. A spooled event that has not been reduced is state
    // the user would otherwise silently lose.
    if let Err(e) = reducer::reduce_all(&mut db.conn, Some(&home::spool_dir(&home))) {
        println!("{} Could not catch up the reducer: {e}", warn_mark());
    }
    let (workspace_id, workspace_root) = workspace_for_cwd(Some(&db.conn));
    // Clear out anything abandoned by an earlier run before writing.
    velra_core::staging::sweep(&home, &workspace_id, now);

    let candidates = restore_ui::discover(
        &db.conn,
        &workspace_id,
        &workspace_root,
        restore_ui::user_home().as_deref(),
    );
    let tz = velra_core::time::local_offset_secs(now);

    if list || (session.is_none() && json) {
        if json {
            let rows: Vec<serde_json::Value> = candidates
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "session_id": c.session.session_id,
                        "title": c.session.title,
                        "last_activity_ms": c.session.last_activity_ms,
                        "has_state": c.session.has_state,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "workspace_id": workspace_id,
                    "workspace_root": workspace_root,
                    "sessions": rows,
                }))
                .unwrap_or_default()
            );
        } else if candidates.is_empty() {
            println!(
                "{} No previous sessions found for this workspace ({workspace_root}).",
                warn_mark()
            );
        } else {
            for (i, c) in candidates.iter().enumerate() {
                println!("{}\n", restore_ui::render_entry(i + 1, c, tz));
            }
        }
        return 0;
    }

    // Resolve the source session: named explicitly, or chosen in the picker.
    let chosen = match session {
        Some(id) => id,
        None => {
            if candidates.is_empty() {
                println!(
                    "{} No previous sessions found for this workspace ({workspace_root}).",
                    warn_mark()
                );
                println!(
                    "  Velra records a session once Claude Code runs here with its hooks enabled."
                );
                println!("  Run `velra restore` from the directory Claude Code was started in.");
                return 1;
            }
            let restorable = candidates.iter().filter(|c| c.restorable()).count();
            if restorable == 0 {
                println!(
                    "{} Velra has no task state for any of this workspace's {} session(s) yet.",
                    warn_mark(),
                    candidates.len()
                );
                return 1;
            }
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut output = std::io::stdout();
            let interactive = std::io::stdin().is_terminal();
            match restore_ui::prompt_choice(&candidates, tz, &mut input, &mut output, interactive) {
                Ok(Some(i)) => candidates[i].session.session_id.clone(),
                Ok(None) => {
                    println!("\n{} Cancelled. Nothing was staged.", warn_mark());
                    return 1;
                }
                Err(e) => {
                    println!("{} Could not read a selection: {e}", fail_mark());
                    return 1;
                }
            }
        }
    };

    let config = Config::load(&home);
    let req = velra_core::restore::RestoreRequest {
        workspace_id: &workspace_id,
        workspace_root: &workspace_root,
        source_session_id: &chosen,
        now_ms: now,
    };
    let staged = match velra_core::restore::build(&db.conn, &req, &config.render()) {
        Ok(s) => s,
        Err(e) => {
            println!("{} {e}", fail_mark());
            if matches!(e, velra_core::restore::RestoreError::UnknownSession { .. }) {
                println!("  Run `velra restore --list` to see this workspace's sessions.");
            }
            return 1;
        }
    };

    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "workspace_id": staged.workspace_id,
                    "source_session_id": staged.source_session_id,
                    "source_checkpoint_id": staged.source_checkpoint_id,
                    "tokens": staged.tokens,
                    "content_hash": staged.content_hash,
                    "summary": staged.summary,
                    "staged": false,
                }))
                .unwrap_or_default()
            );
        } else {
            println!("{}", staged.capsule);
        }
        return 0;
    }

    if let Err(e) = home::ensure_home(&home) {
        println!("{} Could not create {}: {e}", fail_mark(), home.display());
        return 1;
    }
    let path = match velra_core::staging::stage(
        &velra_core::staging::staged_dir(&home, &workspace_id),
        &staged,
    ) {
        Ok(p) => p,
        Err(e) => {
            println!("{} Could not stage the capsule: {e}", fail_mark());
            return 1;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "workspace_id": staged.workspace_id,
                "source_session_id": staged.source_session_id,
                "source_checkpoint_id": staged.source_checkpoint_id,
                "tokens": staged.tokens,
                "content_hash": staged.content_hash,
                "summary": staged.summary,
                "staged": true,
                "staged_path": path.to_string_lossy(),
                "workspace_root": staged.workspace_root,
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    println!(
        "{} Staged {} from session {}.",
        ok_mark(),
        if staged.summary.is_empty() {
            "task state".to_string()
        } else {
            staged.summary.clone()
        },
        staged.source_session_id
    );
    println!("  {} estimated tokens · {}", staged.tokens, path.display());
    println!();
    println!(
        "  Start a new Claude Code session in {} to pick it up.",
        staged.workspace_root
    );
    0
}

fn cmd_doctor(json: bool) -> i32 {
    let Some(home) = require_home() else { return 1 };
    let mut checks: Vec<Check> = Vec::new();
    let installed = read_installed();
    let detection = compat::detect(Duration::from_secs(3));
    let features = Features::for_version(detection.version());
    let state = State::load(&home);

    match &installed.settings_path {
        Some(p) if p.exists() => match &installed.settings_text {
            Some(text) => {
                match jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default()) {
                    Ok(_) => checks.push(Check::Ok(format!("settings parse: {}", p.display()))),
                    Err(e) => checks.push(Check::Fail(format!(
                        "settings parse failed at line {}, column {}: {}",
                        e.line_display(),
                        e.column_display(),
                        e.kind()
                    ))),
                }
            }
            None => checks.push(Check::Fail(format!("settings unreadable: {}", p.display()))),
        },
        Some(p) => checks.push(Check::Warn(format!(
            "no settings file yet at {}",
            p.display()
        ))),
        None => checks.push(Check::Fail("could not determine the settings path".into())),
    }

    match state.bin_path.as_deref() {
        Some(bin) if Path::new(bin).exists() => checks.push(Check::Ok(format!("binary: {bin}"))),
        Some(bin) => checks.push(Check::Fail(format!(
            "binary missing: {bin} — run `velra enable` to repair"
        ))),
        None => checks.push(Check::Warn(
            "binary path not recorded; run `velra enable`".into(),
        )),
    }

    let expected: Vec<&settings::Registration> = settings::REGISTRATIONS
        .iter()
        .filter(|r| (r.supported)(&features))
        .collect();
    let missing: Vec<String> = expected
        .iter()
        .filter(|r| {
            !installed.handlers.iter().any(|(event, args, _)| {
                event == r.event && args.iter().map(String::as_str).eq(r.args.iter().copied())
            })
        })
        .map(|r| format!("{} ({})", r.event, r.args.join(" ")))
        .collect();
    if installed.handlers.is_empty() {
        checks.push(Check::Fail(
            "no Velra hooks registered — run `velra enable`".into(),
        ));
    } else if missing.is_empty() {
        // One event can carry several handlers (`Stop` runs both the hook and
        // the async reducer), so counting registrations and calling them events
        // overstates the coverage.
        let mut events: Vec<&str> = expected.iter().map(|r| r.event).collect();
        events.sort_unstable();
        events.dedup();
        checks.push(Check::Ok(format!(
            "{} hook handlers registered across {} events",
            expected.len(),
            events.len()
        )));
    } else {
        checks.push(Check::Warn(format!(
            "missing hook registrations: {}",
            missing.join(", ")
        )));
    }

    if installed
        .settings_text
        .as_deref()
        .is_some_and(settings::disable_all_hooks)
    {
        checks.push(Check::Warn(
            "\"disableAllHooks\": true is set; no hooks run".into(),
        ));
    }

    if !features.prompt_id {
        checks.push(Check::Warn(format!(
            "Claude Code {} does not send prompt_id; delivery keys fall back to timestamps",
            detection.describe()
        )));
    }

    let db_path = home::db_path(&home);
    if db_path.exists() {
        match Db::open(&db_path, Role::Cli) {
            Ok(db) => {
                let mode = velra_core::db::journal_mode(&db.conn).unwrap_or_default();
                let version = velra_core::db::user_version(&db.conn).unwrap_or(-1);
                if mode.eq_ignore_ascii_case("wal") {
                    checks.push(Check::Ok(format!("database: WAL, schema v{version}")));
                } else {
                    checks.push(Check::Warn(format!(
                        "database journal mode is {mode}, expected WAL"
                    )));
                }
                let unreduced: i64 = db
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM events WHERE id > COALESCE((SELECT last_event_id FROM reducer_cursor WHERE id = 1), 0)",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if unreduced > 5_000 {
                    checks.push(Check::Warn(format!("{unreduced} events not reduced yet")));
                }
                let last: i64 = db
                    .conn
                    .query_row("SELECT COALESCE(MAX(ts_ms), 0) FROM events", [], |r| {
                        r.get(0)
                    })
                    .unwrap_or(0);
                if last == 0 {
                    checks.push(Check::Warn(
                        "no hook events recorded yet — hooks may be disabled by `disableAllHooks` or managed policy".into(),
                    ));
                } else {
                    checks.push(Check::Ok(format!(
                        "last hook event {} ago",
                        velra_core::time::human_age(velra_core::time::now_ms() - last)
                    )));
                }
            }
            Err(e) => checks.push(Check::Fail(format!("database will not open: {e}"))),
        }
    } else {
        checks.push(Check::Warn(format!(
            "no database yet at {}",
            db_path.display()
        )));
    }

    let backlog = velra_core::spool::backlog(&home::spool_dir(&home));
    if backlog > 1_000 {
        checks.push(Check::Warn(format!(
            "{backlog} spool files waiting to be ingested"
        )));
    } else if backlog > 0 {
        checks.push(Check::Ok(format!("spool backlog: {backlog}")));
    }

    let corrupt: Vec<String> = std::fs::read_dir(&home)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".corrupt-"))
                .collect()
        })
        .unwrap_or_default();
    if !corrupt.is_empty() {
        checks.push(Check::Warn(format!(
            "rotated corrupt databases present: {}",
            corrupt.join(", ")
        )));
    }

    if on_network_fs(&home) {
        checks.push(Check::Warn(format!(
            "{} looks like a network filesystem; SQLite WAL is unreliable there",
            home.display()
        )));
    }

    let errors = crate::log::tail_errors(&home, 3);
    if errors.is_empty() {
        checks.push(Check::Ok("no recent errors".into()));
    } else {
        for e in &errors {
            checks.push(Check::Warn(format!("recent error: {e}")));
        }
    }

    let failed = checks.iter().any(|c| matches!(c, Check::Fail(_)));
    if json {
        let value = serde_json::json!({
            "checks": checks.iter().map(|c| serde_json::json!({"level": c.level(), "message": c.message()})).collect::<Vec<_>>(),
            "healthy": !failed,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        for c in &checks {
            c.print();
        }
    }
    i32::from(failed)
}
