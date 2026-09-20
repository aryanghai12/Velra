//! SQLite storage: connection roles, schema, forward-only migrations and
//! corruption recovery (§10).

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    pub fn open(path: &Path, role: Role) -> Result<Db> {
        match Self::open_once(path, role) {
            Err(DbError::Sqlite(e)) if is_corrupt(&e) => {
                let rotated = rotate_corrupt(path)?;
                let mut db = Self::open_once(path, role)?;
                db.rotated_corrupt = Some(rotated);
                Ok(db)
            }
            other => other,
        }
    }

    fn open_once(path: &Path, role: Role) -> Result<Db> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // Before `migrate` turns on WAL: SQLite derives the -wal and -shm modes
        // from the database file, so tightening it here makes them private too.
        make_private(path);
        conn.busy_timeout(role.busy_timeout())?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = OFF; \
             PRAGMA journal_size_limit = 67108864;",
        )?;
        let version = user_version(&conn)?;
        if version > SCHEMA_VERSION {
            return Err(DbError::NewerSchema(version));
        }
        if version < SCHEMA_VERSION {
            migrate(&conn, version, role)?;
        }
        Ok(Db {
            conn,
            path: path.to_path_buf(),
            rotated_corrupt: None,
        })
    }

    /// Read-only open for diagnostics; never creates or migrates.
    pub fn open_readonly(path: &Path) -> Result<Db> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Role::Cli.busy_timeout())?;
        Ok(Db {
            conn,
            path: path.to_path_buf(),
            rotated_corrupt: None,
        })
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
        conn.busy_timeout(Duration::from_millis(200))?;
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
    conn.busy_timeout(role.busy_timeout())?;
    Ok(())
}

/// Renames a corrupt database (and its WAL/SHM files) aside.
pub fn rotate_corrupt(path: &Path) -> Result<PathBuf> {
    let ts = crate::time::now_ms();
    let mut target = path.as_os_str().to_owned();
    target.push(format!(".corrupt-{ts}"));
    let target = PathBuf::from(target);
    std::fs::rename(path, &target)?;
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
