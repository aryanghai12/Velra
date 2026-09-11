//! No-loss spool fallback (§10.3): one file per event, created with
//! create-new semantics and a single write; ingested by the reducer in
//! filename order, then deleted.

use crate::db::{DbError, Result};
use crate::event::NewEvent;
use crate::eventlog::insert_event;
use rusqlite::{Connection, TransactionBehavior};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Files that fail to parse are only quarantined once older than this, so a
/// file still being written is never discarded.
const PARSE_GRACE_MS: u128 = 5_000;

fn rand4() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    format!("{:04x}", h.finish() & 0xffff)
}

fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn create_new_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Writes `ev` to `dir/{ts_ms}-{pid}-{rand4}.jsonl`.
pub fn write(dir: &Path, ev: &NewEvent) -> std::io::Result<PathBuf> {
    if !dir.is_dir() {
        create_dir_private(dir)?;
    }
    let mut line = serde_json::to_string(ev).map_err(std::io::Error::other)?;
    line.push('\n');
    for _ in 0..8 {
        let path = dir.join(format!(
            "{}-{}-{}.jsonl",
            ev.ts_ms,
            std::process::id(),
            rand4()
        ));
        match create_new_private(&path) {
            Ok(mut f) => {
                f.write_all(line.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "spool name collision",
    ))
}

/// Spool files in ingestion (filename) order.
pub fn pending(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    files
}

/// Number of spool files waiting.
pub fn backlog(dir: &Path) -> usize {
    pending(dir).len()
}

fn age_ms(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Ingests up to `max_files` spool files in one transaction, then deletes
/// them. Returns the number of files consumed.
pub fn ingest(conn: &mut Connection, dir: &Path, max_files: usize) -> Result<usize> {
    let files = pending(dir);
    if files.is_empty() {
        return Ok(0);
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DbError::from)?;
    let mut consumed = Vec::new();
    let mut bad = Vec::new();
    for path in files.into_iter().take(max_files) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<NewEvent>(text.trim()) {
            Ok(ev) => {
                insert_event(&tx, &ev).map_err(DbError::from)?;
                consumed.push(path);
            }
            Err(_) if age_ms(&path) > PARSE_GRACE_MS => bad.push(path),
            Err(_) => {}
        }
    }
    tx.commit().map_err(DbError::from)?;
    for p in &consumed {
        let _ = std::fs::remove_file(p);
    }
    if !bad.is_empty() {
        let quarantine = dir.join("bad");
        let _ = create_dir_private(&quarantine);
        for p in bad {
            if let Some(name) = p.file_name() {
                let _ = std::fs::rename(&p, quarantine.join(name));
            }
        }
    }
    Ok(consumed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, Role};

    #[test]
    fn spool_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        let mut ev = NewEvent {
            dedupe_key: "k".into(),
            session_id: "s".into(),
            project_id: "p".into(),
            agent_id: None,
            hook_event: "Stop".into(),
            tool_name: None,
            tool_use_id: None,
            ts_ms: 42,
            payload: "{}".into(),
            project: None,
        };
        write(&spool, &ev).unwrap();
        ev.dedupe_key = "k2".into();
        ev.ts_ms = 41;
        write(&spool, &ev).unwrap();
        std::fs::write(spool.join("0-0-zzzz.jsonl"), "{partial").unwrap();
        let names: Vec<String> = pending(&spool)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names[0].starts_with("0-")
                && names[1].starts_with("41-")
                && names[2].starts_with("42-")
        );

        let mut db = Db::open(&dir.path().join("v.db"), Role::Reduce).unwrap();
        assert_eq!(ingest(&mut db.conn, &spool, 100).unwrap(), 2);
        // The fresh unparseable file stays until its grace period passes.
        assert_eq!(backlog(&spool), 1);
        let ts: Vec<i64> = db
            .conn
            .prepare("SELECT ts_ms FROM events ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ts, vec![41, 42]);
    }
}
