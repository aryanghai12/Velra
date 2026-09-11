//! Builds a render [`Snapshot`] from the materialized tables, applying the
//! deterministic content-selection rules of §16.2.

use crate::git;
use crate::model::{CommandKind, IntentLevel, Mechanism, Outcome, Trigger};
use crate::paths;
use crate::reducer::{project_root, session_epoch};
use crate::render::{
    AttemptView, CommandRef, DeadEndView, FailureView, IntentView, NextTarget, Snapshot,
    WorkingFileView,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

/// Identity and framing for a snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub checkpoint_id: String,
    pub created_ms: i64,
    pub trigger: Trigger,
    pub partial: bool,
    pub preview: bool,
    pub tz_offset_secs: i32,
}

const DEAD_ENDS_MAX: usize = 4;
const ATTEMPTS_MAX: usize = 4;
const WORKING_MAX: usize = 8;

fn session_project(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    let from_sessions = conn
        .prepare_cached("SELECT project_id FROM sessions WHERE session_id = ?1")?
        .query_row([session_id], |r| r.get::<_, String>(0))
        .optional()?;
    if from_sessions.is_some() {
        return Ok(from_sessions);
    }
    conn.prepare_cached(
        "SELECT project_id FROM events WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
    )?
    .query_row([session_id], |r| r.get(0))
    .optional()
}

fn command_ref(conn: &Connection, id: i64) -> rusqlite::Result<Option<CommandRef>> {
    conn.prepare_cached("SELECT command_text, outcome FROM commands WHERE id = ?1")?
        .query_row([id], |r| {
            let outcome: String = r.get(1)?;
            Ok(CommandRef {
                id,
                command: r.get(0)?,
                outcome: Outcome::parse(&outcome).unwrap_or(Outcome::Unknown),
            })
        })
        .optional()
}

/// Splits a stored `- old\n+ new` excerpt into its two lines.
pub fn split_excerpt(excerpt: &str) -> (Option<String>, Option<String>) {
    let mut minus = None;
    let mut plus = None;
    for line in excerpt.lines() {
        if let Some(m) = line.strip_prefix("- ") {
            minus.get_or_insert_with(|| m.to_string());
        } else if let Some(p) = line.strip_prefix("+ ") {
            plus.get_or_insert_with(|| p.to_string());
        }
    }
    (minus, plus)
}

/// Builds the snapshot for `session_id`'s current epoch.
// Row tuples are wide by nature here; naming each one would not make the
// selection rules clearer.
#[allow(clippy::type_complexity)]
pub fn build(
    conn: &Connection,
    session_id: &str,
    meta: &SnapshotMeta,
) -> rusqlite::Result<Snapshot> {
    let epoch = session_epoch(conn, session_id)?;
    let project_id = session_project(conn, session_id)?.unwrap_or_default();
    let root_path: Option<PathBuf> = project_root(conn, &project_id)?;
    let sid = session_id;

    // Intents.
    let mut root = None;
    let mut subtask = None;
    let mut latest = None;
    {
        let mut stmt = conn.prepare_cached(
            "SELECT id, level, text, created_ms FROM intents \
             WHERE session_id = ?1 AND epoch = ?2 AND superseded_ms IS NULL ORDER BY id",
        )?;
        let rows = stmt.query_map(params![sid, epoch], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, level, text, ts_ms) = row?;
            let view = IntentView { id, text, ts_ms };
            match IntentLevel::parse(&level) {
                Some(IntentLevel::Root) => {
                    root.get_or_insert(view);
                }
                Some(IntentLevel::Subtask) => subtask = Some(view),
                Some(IntentLevel::Latest) => latest = Some(view),
                None => {}
            }
        }
    }

    let git = root_path.as_deref().and_then(git::head_info);
    let edit_count: i64 = conn
        .prepare_cached("SELECT COUNT(*) FROM edits WHERE session_id = ?1 AND epoch = ?2")?
        .query_row(params![sid, epoch], |r| r.get(0))?;
    let last_test = conn
        .prepare_cached(
            "SELECT id FROM commands WHERE session_id = ?1 AND epoch = ?2 AND kind = 'test' \
             ORDER BY ts_ms DESC, id DESC LIMIT 1",
        )?
        .query_row(params![sid, epoch], |r| r.get::<_, i64>(0))
        .optional()?;
    let last_test = match last_test {
        Some(id) => command_ref(conn, id)?,
        None => None,
    };

    // Active failure: most recent FAIL not followed by a PASS of the same signature.
    const OPEN_FAILURE: &str = "FROM commands c WHERE c.session_id = ?1 AND c.epoch = ?2 \
         AND c.kind IN ('test', 'build', 'lint') AND c.outcome = 'FAIL' \
         AND NOT EXISTS (SELECT 1 FROM commands d WHERE d.session_id = c.session_id AND d.epoch = c.epoch \
           AND d.signature = c.signature AND d.outcome = 'PASS' \
           AND (d.ts_ms > c.ts_ms OR (d.ts_ms = c.ts_ms AND d.id > c.id)))";
    let failure_row = conn
        .prepare_cached(&format!(
            "SELECT c.id, c.kind, c.command_text, c.exit_code, c.excerpt, c.ts_ms, c.mentioned_paths {OPEN_FAILURE} \
             ORDER BY c.ts_ms DESC, c.id DESC LIMIT 1"
        ))?
        .query_row(params![sid, epoch], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .optional()?;
    let failing_count: i64 = conn
        .prepare_cached(&format!(
            "SELECT COUNT(DISTINCT c.signature) {OPEN_FAILURE}"
        ))?
        .query_row(params![sid, epoch], |r| r.get(0))?;
    let mut failure_mentions: Vec<serde_json::Value> = Vec::new();
    let failure = failure_row.map(|(id, kind, command, exit_code, excerpt, ts_ms, mentions)| {
        failure_mentions = mentions
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default();
        FailureView {
            id,
            kind: CommandKind::parse(&kind).unwrap_or(CommandKind::Test),
            command,
            exit_code,
            excerpt: excerpt
                .map(|e| e.lines().map(str::to_string).collect())
                .unwrap_or_default(),
            ts_ms,
        }
    });

    // Dead ends.
    let dead_rows: Vec<(i64, String, String, String, Option<String>, i64, Option<i64>)> = conn
        .prepare_cached(
            "SELECT id, path, edit_ids, mechanism, command_text, resolved_ms, observed_after_command_id \
             FROM dead_ends WHERE session_id = ?1 AND epoch = ?2 AND reapplied = 0 \
             AND resolved_ms <= ?3 ORDER BY resolved_ms DESC, id DESC",
        )?
        .query_map(params![sid, epoch, meta.created_ms], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let dead_end_total = dead_rows.len() as u32;
    let mut dead_ends = Vec::new();
    for (id, path, edit_ids, mechanism, command, resolved_ms, observed) in
        dead_rows.into_iter().take(DEAD_ENDS_MAX)
    {
        let edit_ids: Vec<i64> = serde_json::from_str(&edit_ids).unwrap_or_default();
        let mut subagent = false;
        let mut first_excerpt: Option<String> = None;
        let mut stmt = conn.prepare_cached("SELECT agent_id, excerpt FROM edits WHERE id = ?1")?;
        for (i, eid) in edit_ids.iter().enumerate() {
            if let Some((agent, excerpt)) = stmt
                .query_row([eid], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                })
                .optional()?
            {
                subagent |= agent.is_some();
                if i == 0 {
                    first_excerpt = excerpt;
                }
            }
        }
        let (minus, plus) = if paths::is_sensitive(&path) {
            (None, None)
        } else {
            first_excerpt
                .as_deref()
                .map(split_excerpt)
                .unwrap_or((None, None))
        };
        let observed_after = match observed {
            Some(cid) => command_ref(conn, cid)?,
            None => None,
        };
        dead_ends.push(DeadEndView {
            id,
            path,
            subagent,
            edit_ids,
            mechanism: Mechanism::parse(&mechanism).unwrap_or(Mechanism::External),
            command,
            resolved_ms,
            minus,
            plus,
            observed_after,
        });
    }

    // Recent attempts: latest ACTIVE edit per path, excluding dead-end paths.
    let attempt_rows: Vec<(i64, String, Option<u32>, Option<u32>, i64, Option<String>)> = conn
        .prepare_cached(
            "SELECT e.id, e.path, e.lines_added, e.lines_removed, e.ts_ms, e.agent_id FROM edits e \
             WHERE e.session_id = ?1 AND e.epoch = ?2 AND e.status = 'ACTIVE' AND e.id = \
               (SELECT MAX(x.id) FROM edits x WHERE x.session_id = e.session_id AND x.epoch = e.epoch \
                AND x.path = e.path AND x.status = 'ACTIVE') \
             ORDER BY e.ts_ms DESC, e.id DESC",
        )?
        .query_map(params![sid, epoch], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut attempts = Vec::new();
    for (edit_id, path, added, removed, ts_ms, agent) in attempt_rows {
        if attempts.len() >= ATTEMPTS_MAX {
            break;
        }
        if dead_ends.iter().any(|d| d.path == path) {
            continue;
        }
        let after_id = conn
            .prepare_cached(
                "SELECT id FROM commands WHERE session_id = ?1 AND epoch = ?2 AND kind IN ('test', 'build') \
                 AND ts_ms > ?3 ORDER BY ts_ms, id LIMIT 1",
            )?
            .query_row(params![sid, epoch, ts_ms], |r| r.get::<_, i64>(0))
            .optional()?;
        let afterward = match after_id {
            Some(id) => command_ref(conn, id)?,
            None => None,
        };
        attempts.push(AttemptView {
            edit_id,
            path,
            subagent: agent.is_some(),
            added,
            removed,
            ts_ms,
            afterward,
        });
    }

    // Working files: score = 3·edits + min(reads, 5) + 4·in_failure.
    let mut stats: Vec<(String, u32, u32, bool, i64)> = conn
        .prepare_cached(
            "SELECT path, edits, reads, in_failure, last_touch_ms FROM file_stats WHERE session_id = ?1 AND epoch = ?2",
        )?
        .query_map(params![sid, epoch], |r| {
            Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32, r.get::<_, i64>(3)? != 0, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if let Some(root) = &root_path {
        stats.retain(|(p, ..)| paths::resolve(p, root).exists());
    }
    stats.sort_by(|a, b| {
        let score =
            |x: &(String, u32, u32, bool, i64)| 3 * x.1 + x.2.min(5) + if x.3 { 4 } else { 0 };
        score(b)
            .cmp(&score(a))
            .then(b.4.cmp(&a.4))
            .then(a.0.cmp(&b.0))
    });
    let working_files: Vec<WorkingFileView> = stats
        .into_iter()
        .take(WORKING_MAX)
        .map(|(path, edits, reads, in_failure, _)| WorkingFileView {
            path,
            edits,
            reads,
            in_failure,
        })
        .collect();

    // Next known target.
    let mut next_target = None;
    if let Some(f) = &failure {
        for m in &failure_mentions {
            let (Some(path), Some(line)) = (m["path"].as_str(), m["line"].as_u64()) else {
                continue;
            };
            let raw = m["raw"].as_str().unwrap_or(path);
            if f.excerpt.iter().any(|l| l.contains(raw)) {
                next_target = Some(NextTarget {
                    rule: "failure-location",
                    target: format!("{path}:{line}"),
                    source: format!("commands:{}", f.id),
                });
                break;
            }
        }
    }
    if next_target.is_none() {
        next_target = conn
            .prepare_cached(
                "SELECT id, path FROM edits WHERE session_id = ?1 AND epoch = ?2 AND status = 'ACTIVE' \
                 ORDER BY ts_ms DESC, id DESC LIMIT 1",
            )?
            .query_row(params![sid, epoch], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .optional()?
            .map(|(id, path)| NextTarget { rule: "last-active-edit", target: path, source: format!("edits:{id}") });
    }

    Ok(Snapshot {
        checkpoint_id: meta.checkpoint_id.clone(),
        created_ms: meta.created_ms,
        trigger: meta.trigger,
        partial: meta.partial,
        preview: meta.preview,
        session_id: sid.to_string(),
        project_id,
        epoch,
        root,
        subtask,
        latest,
        git,
        edit_count: edit_count as u32,
        last_test,
        failure,
        failing_count: failing_count as u32,
        dead_ends,
        dead_end_total,
        attempts,
        working_files,
        next_target,
        tz_offset_secs: meta.tz_offset_secs,
    })
}

/// Whether the session's current epoch has anything worth checkpointing
/// (§15.2 step 4): a ROOT, a command, or an edit.
pub fn has_state(conn: &Connection, session_id: &str) -> rusqlite::Result<bool> {
    let epoch = session_epoch(conn, session_id)?;
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM intents WHERE session_id = ?1 AND epoch = ?2 AND level = 'ROOT' AND superseded_ms IS NULL) \
             OR EXISTS (SELECT 1 FROM commands WHERE session_id = ?1 AND epoch = ?2) \
             OR EXISTS (SELECT 1 FROM edits WHERE session_id = ?1 AND epoch = ?2)",
    )?
    .query_row(params![session_id, epoch], |r| r.get(0))
}
