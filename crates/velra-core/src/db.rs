//! SQLite storage: connection roles, schema, forward-only migrations and
//! corruption recovery (§10).

use rusqlite::config::DbConfig;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Schema version stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 3;

/// Schema v1 (§10.4) plus secondary indexes used by the reducer and renderer.
const SCHEMA_V1: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;

CREATE TABLE events (
  id            INTEGER PRIMARY KEY,
  dedupe_key    TEXT NOT NULL UNIQUE,
  session_id    TEXT NOT NULL,
  project_id    TEXT NOT NULL,
  agent_id      TEXT,
  hook_event    TEXT NOT NULL,
  tool_name     TEXT,
  tool_use_id   TEXT,
  ts_ms         INTEGER NOT NULL,
  payload       TEXT NOT NULL
) STRICT;
CREATE INDEX events_session_ts ON events(session_id, ts_ms);

CREATE TABLE projects (project_id TEXT PRIMARY KEY, root_path TEXT NOT NULL, is_git INTEGER NOT NULL, created_ms INTEGER NOT NULL) STRICT;

CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, started_ms INTEGER NOT NULL,
  last_event_ms INTEGER NOT NULL, transcript_path TEXT, epoch INTEGER NOT NULL DEFAULT 1,
  ended_ms INTEGER, end_reason TEXT
) STRICT;

CREATE TABLE reducer_cursor (id INTEGER PRIMARY KEY CHECK (id = 1), last_event_id INTEGER NOT NULL) STRICT;

CREATE TABLE intents (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  level TEXT NOT NULL CHECK (level IN ('ROOT','SUBTASK','LATEST')),
  text TEXT NOT NULL, source_event_id INTEGER NOT NULL, created_ms INTEGER NOT NULL,
  superseded_ms INTEGER
) STRICT;
CREATE INDEX intents_live ON intents(session_id, epoch, level) WHERE superseded_ms IS NULL;

CREATE TABLE file_versions (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, path TEXT NOT NULL,
  content_hash TEXT NOT NULL, size INTEGER NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('pre_edit','post_edit','original','git_pre','git_post','turn_scan')),
  event_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX file_versions_path ON file_versions(session_id, path, id);

CREATE TABLE edits (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  event_id INTEGER NOT NULL UNIQUE, agent_id TEXT, path TEXT NOT NULL, tool_name TEXT NOT NULL,
  pre_hash TEXT, post_hash TEXT NOT NULL, lines_added INTEGER, lines_removed INTEGER,
  excerpt TEXT, ts_ms INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('ACTIVE','REVERTED','DISCARDED','COMMITTED','REAPPLIED')),
  resolved_event_id INTEGER, mechanism TEXT CHECK (mechanism IN ('git_command','inverse_edit','rewrite','external'))
) STRICT;

CREATE TABLE dead_ends (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL, path TEXT NOT NULL,
  edit_ids TEXT NOT NULL,
  mechanism TEXT NOT NULL, command_text TEXT, resolved_ms INTEGER NOT NULL,
  observed_after_command_id INTEGER,
  reapplied INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE commands (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL, event_id INTEGER NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('test','build','lint','git','other')),
  signature TEXT NOT NULL, command_text TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('PASS','FAIL','INTERRUPTED','UNKNOWN')),
  exit_code INTEGER, excerpt TEXT, mentioned_paths TEXT, ts_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE file_stats (
  session_id TEXT NOT NULL, epoch INTEGER NOT NULL, path TEXT NOT NULL,
  reads INTEGER NOT NULL DEFAULT 0, edits INTEGER NOT NULL DEFAULT 0,
  in_failure INTEGER NOT NULL DEFAULT 0, last_touch_ms INTEGER NOT NULL,
  PRIMARY KEY (session_id, epoch, path)
) STRICT;

CREATE TABLE checkpoints (
  checkpoint_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL, project_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  created_ms INTEGER NOT NULL, "trigger" TEXT NOT NULL CHECK ("trigger" IN ('manual','auto','cli')),
  head_commit TEXT, branch TEXT, event_watermark INTEGER NOT NULL, partial INTEGER NOT NULL,
  render_version INTEGER NOT NULL, capsule TEXT NOT NULL, capsule_tokens_est INTEGER NOT NULL,
  summary_json TEXT NOT NULL
) STRICT;
CREATE TRIGGER checkpoints_immutable BEFORE UPDATE ON checkpoints
BEGIN SELECT RAISE(ABORT, 'checkpoints are immutable'); END;

CREATE TABLE continuations (
  checkpoint_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('PENDING','ATTACHED','CONFIRMED','SUPERSEDED','EXPIRED')),
  attach_count INTEGER NOT NULL DEFAULT 0, attached_ms INTEGER, attached_channel TEXT,
  confirmed_ms INTEGER, confirm_event_id INTEGER, updated_ms INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX one_live_continuation ON continuations(session_id)
  WHERE state IN ('PENDING','ATTACHED');

CREATE TABLE injections (
  injection_id TEXT PRIMARY KEY,
  checkpoint_id TEXT NOT NULL, delivery_key TEXT NOT NULL UNIQUE,
  channel TEXT NOT NULL CHECK (channel IN ('session_start','post_tool','user_prompt')),
  ts_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE compactions (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, checkpoint_id TEXT,
  "trigger" TEXT, pre_ms INTEGER, post_ms INTEGER, native_summary TEXT
) STRICT;

-- Secondary indexes (not part of the normative schema; see DECISIONS.md).
CREATE INDEX ix_edits_session ON edits(session_id, epoch, status);
CREATE INDEX ix_edits_path ON edits(session_id, path, id);
CREATE INDEX ix_commands_session ON commands(session_id, epoch, ts_ms);
CREATE INDEX ix_dead_ends_session ON dead_ends(session_id, path);
CREATE INDEX ix_sessions_project ON sessions(project_id, last_event_ms);
CREATE INDEX ix_checkpoints_session ON checkpoints(session_id, created_ms);
CREATE INDEX ix_injections_checkpoint ON injections(checkpoint_id);
CREATE INDEX ix_compactions_session ON compactions(session_id, id);

INSERT INTO reducer_cursor (id, last_event_id) VALUES (1, 0);
"#;

/// Schema v2: when a file was *first* touched in the epoch.
///
/// `last_touch_ms` alone decides ties in the working-file ranking, and it
/// always favours whatever was touched most recently — so a breadth-first read
/// sweep across a codebase evicts the handful of files the task is actually
/// about, every one of which scores the same single read. First touch is the
/// stable half of the same signal: among files with equally weak evidence, the
/// ones the session opened with are the ones that framed it (D59).
const SCHEMA_V2: &str = r#"
ALTER TABLE file_stats ADD COLUMN first_touch_ms INTEGER NOT NULL DEFAULT 0;
UPDATE file_stats SET first_touch_ms = last_touch_ms;
"#;

/// Schema v3: constraints stated in a user prompt.
///
/// Until this table existed, a constraint survived compaction only if it
/// happened to sit inside the first 160-240 characters of the session's very
/// first prompt, because that is all `[FIRST_MESSAGE]` carries once the
/// truncation ladder has run. A rule stated in the second sentence of turn 0,
/// or in any later turn, was observed, stored in `events`, and then dropped on
/// the floor by every projection downstream of it.
///
/// A row here is a sentence the user wrote, quoted verbatim, together with the
/// cue that selected it (`crates/velra-core/src/constraint.rs`) and the event
/// it came from. It is never a paraphrase and never an inference.
const SCHEMA_V3: &str = r#"
CREATE TABLE constraints (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  text TEXT NOT NULL, cue TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('labelled','prohibition','requirement')),
  source_event_id INTEGER NOT NULL, prompt_ordinal INTEGER NOT NULL,
  created_ms INTEGER NOT NULL, superseded_ms INTEGER
) STRICT;
CREATE UNIQUE INDEX constraints_unique ON constraints(session_id, epoch, text);
CREATE INDEX constraints_live ON constraints(session_id, epoch, id)
  WHERE superseded_ms IS NULL;
"#;

/// Forward-only migrations; index `i` upgrades from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[SCHEMA_V1, SCHEMA_V2, SCHEMA_V3];

/// Connection role, which determines lock waiting (§10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    HookAppend,
    HookDelivery,
    PreCompact,
    Reduce,
    Cli,
}

impl Role {
    pub fn busy_timeout(self) -> Duration {
        Duration::from_millis(match self {
            Role::HookAppend | Role::HookDelivery => 100,
            Role::PreCompact => 50,
            Role::Reduce => 1_000,
            Role::Cli => 5_000,
        })
    }

    fn is_hook(self) -> bool {
        !matches!(self, Role::Cli)
    }
}

/// Storage errors, classified for fail-open handling.
#[derive(Debug)]
pub enum DbError {
    /// Locked beyond the role's budget.
    Busy,
    /// The file was written by a newer Velra.
    NewerSchema(i64),
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Busy => write!(f, "database is busy"),
            DbError::NewerSchema(v) => write!(
                f,
                "database schema version {v} is newer than this binary ({SCHEMA_VERSION})"
            ),
            DbError::Sqlite(e) => write!(f, "sqlite: {e}"),
            DbError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for DbError {}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        if is_busy(&e) {
            DbError::Busy
        } else {
            DbError::Sqlite(e)
        }
    }
}

impl From<std::io::Error> for DbError {
    fn from(e: std::io::Error) -> Self {
        DbError::Io(e)
    }
}

pub type Result<T, E = DbError> = std::result::Result<T, E>;

pub fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

pub fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

/// An open database.
pub struct Db {
    pub conn: Connection,
    pub path: PathBuf,
    /// Set when a corrupt file was rotated aside during open.
    pub rotated_corrupt: Option<PathBuf>,
}

/// Drops the group and world bits from a file that already exists.
///
/// SQLite creates a database with 0666 masked by the umask — 0644 on a stock
/// POSIX box — which is wider than prompts, paths and failure output deserve
/// even inside a 0700 home. Only ever clears bits, and never fails an open over
/// one: a database we can read but not chmod is still usable.
#[cfg(unix)]
fn make_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o700));
    }
}

#[cfg(not(unix))]
fn make_private(_path: &Path) {}

impl Db {
    /// Opens (creating and migrating if needed) with the role's settings.
    /// A corrupt file is renamed to `velra.db.corrupt-{ts}` and recreated.
    ///
    /// Many hooks can meet the same corrupt file at once. Rotation is done
    /// by one of them at a time, under [`rotation_lock`], and only when the
    /// file is still corrupt once the lock is held: a process that saw the
    /// old file used to rename whatever was at the path by the time it got
    /// there -- the fresh database another had just created and written to
    /// (reproduced on Linux, DECISIONS D137).
    pub fn open(path: &Path, role: Role) -> Result<Db> {
        match Self::open_once(path, role) {
            Err(DbError::Sqlite(e)) if is_corrupt(&e) => {
                let _lock = rotation_lock(path, role.busy_timeout())?;
                match Self::open_once(path, role) {
                    Err(DbError::Sqlite(e)) if is_corrupt(&e) => {}
                    // Rotated and recreated by another process meanwhile.
                    other => return other,
                }
                let rotated = rotate_corrupt(path)?;
                // Still under the lock, so a process that saw the old file
                // finds this one when it looks again.
                let mut db = Self::open_once(path, role)?;
                db.rotated_corrupt = Some(rotated);
                Ok(db)
            }
            other => other,
        }
    }

    /// [`open_once_at`], again when the file at `path` was replaced while it
    /// was being opened.
    fn open_once(path: &Path, role: Role) -> Result<Db> {
        for _ in 0..3 {
            if let Some(db) = Self::open_once_at(path, role)? {
                return Ok(db);
            }
        }
        Err(DbError::Busy)
    }

    /// Opens `path`, or `None` when the file there changed between the
    /// open and the first read (POSIX only).
    ///
    /// SQLite pairs a database with its `-wal` by *name*. A hook that opened
    /// `velra.db` just before another rotated it aside kept a descriptor to
    /// the old file, found the new database's `-wal` beside the path, read a
    /// valid page 1 from it -- and on closing checkpointed that journal into
    /// the old file and reset it, leaving the new database empty; the next
    /// hook rotated that too (reproduced on Linux, D137). So the first read
    /// is taken with checkpoint-on-close off, the path is checked to still
    /// be the file that was opened, and nothing is written before it is.
    /// Windows refuses to rename an open database, so the case cannot arise
    /// there.
    fn open_once_at(path: &Path, role: Role) -> Result<Option<Db>> {
        let before = file_id(path);
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        // Before `migrate` turns on WAL: SQLite derives the -wal and -shm modes
        // from the database file, so tightening it here makes them private too.
        make_private(path);
        set_busy_budget(&conn, role.busy_timeout())?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = OFF; \
             PRAGMA journal_size_limit = 67108864;",
        )?;
        let version = user_version(&conn)?;
        // A path that was empty when looked at cannot have held the old file
        // since: a rotated file never returns to it.
        if before.is_some() && file_id(path) != before {
            return Ok(None);
        }
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false)?;
        if version > SCHEMA_VERSION {
            return Err(DbError::NewerSchema(version));
        }
        if version < SCHEMA_VERSION {
            migrate(&conn, version, role)?;
        }
        Ok(Some(Db {
            conn,
            path: path.to_path_buf(),
            rotated_corrupt: None,
        }))
    }

    /// Read-only open for diagnostics; never creates or migrates.
    pub fn open_readonly(path: &Path) -> Result<Db> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        set_busy_budget(&conn, Role::Cli.busy_timeout())?;
        Ok(Db {
            conn,
            path: path.to_path_buf(),
            rotated_corrupt: None,
        })
    }
}

/// Waits on a lock for at most `budget` of elapsed time.
///
/// `busy_timeout` installs SQLite's own handler, which sleeps a schedule of
/// short delays (1, 2, 5, 10 ms, …) and stops once the delays it *asked for*
/// add up to the timeout. Windows rounds every sleep up to its timer tick
/// (15.6 ms, measured: `measure_sqlite_sleep_against_its_busy_schedule`), so
/// a 100 ms hook budget cost 171 ms of waiting, 50 ms cost 109 ms. This
/// handler keeps the same schedule and counts the clock instead: it overruns
/// by at most the one sleep in progress at the deadline.
pub fn set_busy_budget(conn: &Connection, budget: Duration) -> rusqlite::Result<()> {
    // A handler is a plain `fn`, so each budget in use has its own.
    let handler: fn(i32) -> bool = match budget.as_millis() {
        50 => busy::<50>,
        100 => busy::<100>,
        200 => busy::<200>,
        1_000 => busy::<1_000>,
        5_000 => busy::<5_000>,
        _ => return conn.busy_timeout(budget),
    };
    conn.busy_handler(Some(handler))
}

fn busy<const MS: u64>(count: i32) -> bool {
    busy_wait(count, Duration::from_millis(MS))
}

thread_local! {
    /// When the wait in progress on this thread began. A busy handler runs
    /// inside the call that met the lock, so a thread has one at a time.
    static BUSY_SINCE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// SQLite's delay schedule (`sqliteDefaultBusyCallback`).
const BUSY_STEPS_MS: [u64; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];

/// `count` is how many times SQLite has already called the handler for this
/// lock; 0 starts a new wait.
fn busy_wait(count: i32, budget: Duration) -> bool {
    let now = Instant::now();
    let since = BUSY_SINCE.with(|c| {
        let since = match c.get() {
            Some(t) if count > 0 => t,
            _ => now,
        };
        c.set(Some(since));
        since
    });
    let sleep = busy_step(count, since, now, budget);
    #[cfg(test)]
    busy_trace::record(count, since, now, sleep);
    match sleep {
        Some(d) => {
            std::thread::sleep(d);
            true
        }
        None => false,
    }
}

/// What the handler decides on its `count`th call of a wait that began at
/// `since`, called at `now`: how long to sleep before SQLite retries, or
/// `None` to give up.
///
/// Every decision is taken against the clock, so this never asks for a sleep
/// that ends past the deadline, and gives up at the first call on or after
/// it. How late the OS then wakes the thread is not decided here: on a loaded
/// or virtualised runner a sleep can end tens of milliseconds after it was
/// asked to, and that lateness is the whole of any overrun (D146).
fn busy_step(count: i32, since: Instant, now: Instant, budget: Duration) -> Option<Duration> {
    let left = budget.saturating_sub(now.duration_since(since));
    if left.is_zero() {
        return None;
    }
    let step = BUSY_STEPS_MS[usize::try_from(count)
        .unwrap_or(0)
        .min(BUSY_STEPS_MS.len() - 1)];
    Some(left.min(Duration::from_millis(step)))
}

/// Every busy-handler call on this thread, as decided, for the tests that
/// hold the handler to its budget against the real clock.
#[cfg(test)]
mod busy_trace {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    #[derive(Debug, Clone, Copy)]
    pub struct Call {
        pub count: i32,
        pub since: Instant,
        pub at: Instant,
        pub sleep: Option<Duration>,
    }

    thread_local! {
        static CALLS: RefCell<Vec<Call>> = const { RefCell::new(Vec::new()) };
    }

    pub fn record(count: i32, since: Instant, at: Instant, sleep: Option<Duration>) {
        CALLS.with(|c| {
            c.borrow_mut().push(Call {
                count,
                since,
                at,
                sleep,
            })
        });
    }

    /// The calls recorded since the last `take`.
    pub fn take() -> Vec<Call> {
        CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
}

pub fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

pub fn journal_mode(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
}

fn migrate(conn: &Connection, from: i64, role: Role) -> Result<()> {
    // Hooks may migrate only if they get the lock within 200 ms (§10.5).
    if role.is_hook() {
        set_busy_budget(conn, Duration::from_millis(200))?;
    }
    if from == 0 {
        // Persistent; issued only when the database is created (§10.2).
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        let current = user_version(conn)?;
        for step in current..SCHEMA_VERSION {
            let sql = MIGRATIONS[usize::try_from(step).unwrap_or(0)];
            conn.execute_batch(sql)?;
        }
        if current < SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('created_by', ?1)",
                [concat!("velra ", env!("CARGO_PKG_VERSION"))],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    set_busy_budget(conn, role.busy_timeout())?;
    Ok(())
}

/// Which file `path` names now: device and inode on POSIX. `None` when there
/// is none -- and always on Windows, which does not let an open database be
/// renamed.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// The lock that serializes rotating a corrupt database: an OS lock on
/// `<db>.rotate-lock`, which the system releases when its holder exits, so
/// a hook killed mid-rotation leaves nothing to clean up. The file itself is
/// never deleted: unlinking a lock file others may be opening lets two of
/// them lock two different files.
///
/// Waits up to `budget`, the role's lock budget; `Busy` after that, which a
/// hook treats like any other busy database (it spools).
fn rotation_lock(path: &Path, budget: Duration) -> Result<std::fs::File> {
    let mut name = path.as_os_str().to_owned();
    name.push(".rotate-lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(name))?;
    let deadline = Instant::now() + budget;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Err(DbError::Busy),
            Err(std::fs::TryLockError::Error(e)) => return Err(DbError::Io(e)),
        }
    }
}

/// Renames a corrupt database (and its WAL/SHM files) aside.
///
/// The WAL and SHM go first. Once the main file is gone, the next open
/// creates a fresh database -- and its own `-wal` -- at the same path; moving
/// the side files after that took the new database's journal with the old
/// one, and left the live file malformed (reproduced on Linux, D137).
pub fn rotate_corrupt(path: &Path) -> Result<PathBuf> {
    let ts = crate::time::now_ms();
    let mut target = path.as_os_str().to_owned();
    target.push(format!(".corrupt-{ts}"));
    let target = PathBuf::from(target);
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        let side = PathBuf::from(side);
        if side.exists() {
            let mut t = target.as_os_str().to_owned();
            t.push(suffix);
            let _ = std::fs::rename(&side, PathBuf::from(t));
        }
    }
    std::fs::rename(path, &target)?;
    Ok(target)
}

/// Current reducer cursor.
pub fn cursor(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT last_event_id FROM reducer_cursor WHERE id = 1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map(|v| v.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the handler's rules for one wait on a simulated clock, where the
    /// `i`th sleep asked for `d` really lasts `wake(i, d)`. Returns how long
    /// the wait took and how late the sleep that crossed the deadline woke.
    fn simulate(
        budget: Duration,
        wake: impl Fn(usize, Duration) -> Duration,
    ) -> (Duration, Duration) {
        let since = Instant::now();
        let (mut now, mut count, mut last_late) = (since, 0i32, Duration::ZERO);
        while let Some(d) = busy_step(count, since, now, budget) {
            assert!(
                now + d <= since + budget,
                "call {count} asked to sleep past the deadline"
            );
            let slept = wake(count as usize, d);
            assert!(slept >= d, "a simulated sleep never ends early");
            last_late = slept - d;
            now += slept;
            count += 1;
            assert!(count < 10_000, "the wait ends");
        }
        (now - since, last_late)
    }

    /// The handler's rules, independent of any real clock: it gives up at
    /// the first call on or after the deadline, never before, and never asks
    /// to sleep past it -- so the only overrun is how late the OS woke the
    /// thread from its last sleep. Before D133 the budget was the sum of the
    /// sleeps *requested*, which a 15.625 ms timer tick turned into 171 ms.
    #[test]
    fn the_busy_budget_is_kept_against_the_clock_however_late_sleeps_wake() {
        let ms = Duration::from_millis;
        for budget in [ms(50), ms(100), ms(200), ms(1_000), ms(5_000)] {
            // Exact sleeps: SQLite's schedule, cut off at the deadline.
            let (took, _) = simulate(budget, |_, d| d);
            assert_eq!(took, budget);

            // Windows: every sleep rounded up to a 15.625 ms tick.
            let tick = Duration::from_micros(15_625);
            let (took, late) = simulate(budget, |_, d| {
                let ticks = d.as_nanos().div_ceil(tick.as_nanos()).max(1);
                tick * u32::try_from(ticks).unwrap()
            });
            assert!(took >= budget && took - budget <= late && late < tick);

            // A loaded or virtualised runner: any sleep can wake very late.
            for bad in 0..12 {
                for extra in [ms(3), ms(40), ms(250)] {
                    let (took, late) =
                        simulate(budget, |i, d| if i == bad { d + extra } else { d });
                    assert!(took >= budget, "{budget:?}: gave up early");
                    assert!(
                        took - budget <= late,
                        "{budget:?}, sleep {bad} +{extra:?}: overran by {:?}, woke {late:?} late",
                        took - budget
                    );
                }
            }
        }
        let schedule: Vec<u64> = {
            let since = Instant::now();
            let (mut now, mut out) = (since, Vec::new());
            while let Some(d) = busy_step(out.len() as i32, since, now, ms(100)) {
                out.push(d.as_millis() as u64);
                now += d;
            }
            out
        };
        assert_eq!(schedule, [1, 2, 5, 10, 15, 20, 25, 22], "SQLite's schedule");
    }

    /// A call with `count` 0 starts a new wait; any other continues the one
    /// in progress on this thread.
    #[test]
    fn each_statement_starts_its_own_wait() {
        busy_trace::take();
        let budget = Duration::from_secs(10);
        for count in [0, 1, 0, 1] {
            assert!(busy_wait(count, budget));
        }
        let calls = busy_trace::take();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].since, calls[0].at);
        assert_eq!(calls[1].since, calls[0].at, "count 1 continues the wait");
        assert_eq!(calls[2].since, calls[2].at, "count 0 starts a new one");
        assert!(calls[2].since > calls[0].since);
        assert_eq!(calls[3].since, calls[2].at);
    }

    /// A role's lock budget is elapsed time, held against the real clock and
    /// real SQLite. With SQLite's own handler it was the sum of the sleeps
    /// requested: on Windows, where each is rounded up to a 15.6 ms tick, the
    /// 50 ms pre-compact budget waited 109 ms and the 100 ms hook budget
    /// 171 ms (measured, D133).
    ///
    /// Every wait is checked against the handler's own record of it: one
    /// wait per statement, no call gives up before the deadline, none asks to
    /// sleep past it, and the first call on or after it gives up. What is left
    /// of the elapsed time once the budget and the OS's lateness in waking the
    /// thread from its last sleep are taken away is SQLite's own work around
    /// the wait, and that is bounded. The lateness itself is not: a macOS CI
    /// runner overran the old `budget + 25 ms` bound on the median of five
    /// waits (D146), and no code in this process decides it.
    #[test]
    fn a_locked_database_is_waited_on_for_the_role_budget_and_no_longer() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.db");
        drop(Db::open(&p, Role::Cli).unwrap());
        let holder = Db::open(&p, Role::Cli).unwrap();
        holder.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        for (role, budget_ms) in [(Role::PreCompact, 50u64), (Role::HookAppend, 100)] {
            let budget = Duration::from_millis(budget_ms);
            let db = Db::open(&p, role).unwrap();
            let mut own: Vec<Duration> = Vec::new();
            let mut report: Vec<String> = Vec::new();
            for _ in 0..5 {
                busy_trace::take();
                let started = Instant::now();
                let r = db.conn.execute_batch("BEGIN IMMEDIATE");
                let waited = started.elapsed();
                let calls = busy_trace::take();
                assert!(r.is_err(), "the lock is held");
                assert!(calls.len() >= 2, "{role:?}: the handler waited: {calls:?}");
                let (first, last) = (calls[0], calls[calls.len() - 1]);
                let deadline = first.since + budget;
                for (i, c) in calls.iter().enumerate() {
                    assert_eq!(c.count, i as i32, "{role:?}: one wait: {calls:?}");
                    assert_eq!(c.since, first.at, "{role:?}: one wait: {calls:?}");
                }
                assert!(
                    calls[..calls.len() - 1].iter().all(|c| c.sleep.is_some()),
                    "{role:?} gave up before its deadline: {calls:?}"
                );
                assert!(last.sleep.is_none(), "{role:?}: the last call gives up");
                assert!(last.at >= deadline, "{role:?} gave up early: {calls:?}");
                for c in &calls {
                    if let Some(d) = c.sleep {
                        assert!(
                            c.at + d <= deadline,
                            "{role:?} asked to sleep past its deadline: {calls:?}"
                        );
                    }
                }
                assert!(waited >= budget, "{role:?} returned early: {waited:?}");
                // Everything past the last wake-up the handler asked for.
                let prev = calls[calls.len() - 2];
                let asked = prev.at + prev.sleep.unwrap();
                let late = last.at.saturating_duration_since(asked);
                own.push(waited.saturating_sub(budget + late));
                report.push(format!(
                    "waited {:.1} ms, last wake-up {:.1} ms late, {} calls",
                    waited.as_secs_f64() * 1e3,
                    late.as_secs_f64() * 1e3,
                    calls.len()
                ));
            }
            own.sort_unstable();
            assert!(
                own[2] < Duration::from_millis(25),
                "{role:?} waited past its {budget_ms} ms budget by more than the OS \
                 accounts for: {report:#?}"
            );
        }
        holder.conn.execute_batch("ROLLBACK").unwrap();
    }

    /// Measurement, not a check: for `VELRA_MEASURE_WAITS` waits (default 40)
    /// on a held write lock under the 100 ms hook budget, how long each took,
    /// how late the OS woke the thread from the handler's last sleep, and what
    /// is left. Run it on a starved CPU to see which part grows.
    #[test]
    #[ignore]
    fn measure_busy_waits_against_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.db");
        drop(Db::open(&p, Role::Cli).unwrap());
        let holder = Db::open(&p, Role::Cli).unwrap();
        holder.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let db = Db::open(&p, Role::HookAppend).unwrap();
        let n: usize = std::env::var("VELRA_MEASURE_WAITS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let (mut waited, mut late, mut own) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..n {
            busy_trace::take();
            let started = Instant::now();
            assert!(db.conn.execute_batch("BEGIN IMMEDIATE").is_err());
            let w = started.elapsed();
            let calls = busy_trace::take();
            let prev = calls[calls.len() - 2];
            let l = calls[calls.len() - 1]
                .at
                .saturating_duration_since(prev.at + prev.sleep.unwrap());
            waited.push(ms(w));
            late.push(ms(l));
            own.push(ms(w.saturating_sub(Duration::from_millis(100) + l)));
        }
        holder.conn.execute_batch("ROLLBACK").unwrap();
        let summary = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            format!(
                "p50 {:.1} p90 {:.1} max {:.1}",
                v[v.len() / 2],
                v[v.len() * 9 / 10],
                v[v.len() - 1]
            )
        };
        let medians_over: usize = waited
            .chunks(5)
            .filter(|c| {
                let mut c = c.to_vec();
                c.sort_by(f64::total_cmp);
                c[c.len() / 2] >= 125.0
            })
            .count();
        println!(
            "{n} waits, budget 100 ms: waited {}; last wake-up late {}; own {}; \
             5-wait groups whose median is >= 125 ms (the old bound): {medians_over} of {}",
            summary(waited),
            summary(late),
            summary(own),
            n / 5
        );
    }

    /// A lock released during the wait is taken.
    #[test]
    fn a_lock_released_during_the_wait_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.db");
        drop(Db::open(&p, Role::Cli).unwrap());
        let holder = Db::open(&p, Role::Cli).unwrap();
        holder.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let path = p.clone();
        let waiter = std::thread::spawn(move || {
            let db = Db::open(&path, Role::Reduce).unwrap();
            db.conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        });
        std::thread::sleep(Duration::from_millis(100));
        holder.conn.execute_batch("ROLLBACK").unwrap();
        assert!(waiter.join().unwrap().is_ok());
    }

    #[test]
    fn creates_schema_in_wal_mode_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("velra.db");
        let db = Db::open(&p, Role::Cli).unwrap();
        assert_eq!(user_version(&db.conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(journal_mode(&db.conn).unwrap(), "wal");
        assert_eq!(cursor(&db.conn).unwrap(), 0);
        drop(db);
        let db = Db::open(&p, Role::HookAppend).unwrap();
        assert_eq!(user_version(&db.conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn checkpoints_are_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("v.db"), Role::Cli).unwrap();
        db.conn
            .execute(
                "INSERT INTO checkpoints VALUES ('ckpt_1','s','p',1,0,'manual',NULL,NULL,0,0,1,'c',1,'{}')",
                [],
            )
            .unwrap();
        assert!(db
            .conn
            .execute("UPDATE checkpoints SET capsule = 'x'", [])
            .is_err());
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.db");
        let db = Db::open(&p, Role::Cli).unwrap();
        db.conn.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(db);
        assert!(matches!(
            Db::open(&p, Role::HookAppend),
            Err(DbError::NewerSchema(99))
        ));
    }

    #[test]
    fn corrupt_file_is_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.db");
        std::fs::write(&p, vec![0x42u8; 8192]).unwrap();
        let db = Db::open(&p, Role::HookAppend).unwrap();
        assert!(db.rotated_corrupt.as_ref().is_some_and(|r| r.exists()));
        assert_eq!(user_version(&db.conn).unwrap(), SCHEMA_VERSION);
    }
}
