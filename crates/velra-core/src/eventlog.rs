//! Appending normalized events (§10.2: tiny `BEGIN IMMEDIATE` transactions).

use crate::db::{DbError, Result};
use crate::event::NewEvent;
use rusqlite::{params, Connection, TransactionBehavior};

/// Inserts an event (and its project row) without managing a transaction.
/// Returns the new event id, or `None` for a duplicate `dedupe_key`.
pub fn insert_event(conn: &Connection, ev: &NewEvent) -> rusqlite::Result<Option<i64>> {
    let inserted = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO events \
             (dedupe_key, session_id, project_id, agent_id, hook_event, tool_name, tool_use_id, ts_ms, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?
        .execute(params![
            ev.dedupe_key,
            ev.session_id,
            ev.project_id,
            ev.agent_id,
            ev.hook_event,
            ev.tool_name,
            ev.tool_use_id,
            ev.ts_ms,
            ev.payload
        ])?;
    let id = (inserted == 1).then(|| conn.last_insert_rowid());
    if let Some(p) = &ev.project {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO projects (project_id, root_path, is_git, created_ms) VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![p.project_id, p.root_path, i64::from(p.is_git), ev.ts_ms])?;
    }
    Ok(id)
}

/// Appends one event in its own `BEGIN IMMEDIATE` transaction (≤ 2
/// statements). `DbError::Busy` tells the caller to spool instead.
pub fn append(conn: &mut Connection, ev: &NewEvent) -> Result<Option<i64>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DbError::from)?;
    let id = insert_event(&tx, ev).map_err(DbError::from)?;
    tx.commit().map_err(DbError::from)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, Role};
    use crate::event::ProjectInfo;

    fn ev(key: &str) -> NewEvent {
        NewEvent {
            dedupe_key: key.into(),
            session_id: "s1".into(),
            project_id: "p1".into(),
            agent_id: None,
            hook_event: "PostToolUse".into(),
            tool_name: Some("Read".into()),
            tool_use_id: Some("t1".into()),
            ts_ms: 1,
            payload: "{}".into(),
            project: Some(ProjectInfo {
                project_id: "p1".into(),
                root_path: "/r".into(),
                is_git: true,
            }),
        }
    }

    #[test]
    fn append_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("v.db"), Role::HookAppend).unwrap();
        assert_eq!(append(&mut db.conn, &ev("k1")).unwrap(), Some(1));
        assert_eq!(append(&mut db.conn, &ev("k1")).unwrap(), None);
        assert_eq!(append(&mut db.conn, &ev("k2")).unwrap(), Some(2));
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
