//! `velra inspect`: capsule preview and full section detail (§7).

use rusqlite::{params, Connection, OptionalExtension};
use velra_core::checkpoint::StoredCheckpoint;
use velra_core::model::Trigger;
use velra_core::render::Snapshot;
use velra_core::snapshot::{self, SnapshotMeta};
use velra_core::time::{hh_mm, local_offset_secs};

/// Sections that `--section` can expand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    DeadEnds,
    Failure,
    Files,
    Attempts,
}

impl Section {
    pub fn parse(s: &str) -> Option<Section> {
        match s {
            "dead-ends" | "dead_ends" => Some(Section::DeadEnds),
            "failure" => Some(Section::Failure),
            "files" => Some(Section::Files),
            "attempts" => Some(Section::Attempts),
            _ => None,
        }
    }
}

/// The most recently active session of a project.
pub fn latest_session_for_project(
    conn: &Connection,
    project_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.prepare(
        "SELECT session_id FROM sessions WHERE project_id = ?1 ORDER BY last_event_ms DESC, rowid DESC LIMIT 1",
    )?
    .query_row([project_id], |r| r.get(0))
    .optional()
}

/// The most recently active session overall (fallback when the current
/// directory has no recorded project).
pub fn latest_session(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.prepare("SELECT session_id FROM sessions ORDER BY last_event_ms DESC, rowid DESC LIMIT 1")?
        .query_row([], |r| r.get(0))
        .optional()
}

/// Builds a preview snapshot (no checkpoint is created).
pub fn preview(conn: &Connection, session_id: &str, now_ms: i64) -> rusqlite::Result<Snapshot> {
    let meta = SnapshotMeta {
        checkpoint_id: "preview".to_string(),
        created_ms: now_ms,
        trigger: Trigger::Cli,
        partial: false,
        preview: true,
        tz_offset_secs: local_offset_secs(now_ms),
    };
    snapshot::build(conn, session_id, &meta)
}

fn header(title: &str) -> String {
    format!("{title}\n{}", "-".repeat(title.len()))
}

/// Full, untruncated detail for one section, as of `checkpoint` when given.
pub fn section_detail(
    conn: &Connection,
    session_id: &str,
    epoch: i64,
    as_of_ms: i64,
    section: Section,
    tz: i32,
) -> rusqlite::Result<String> {
    let mut out = String::new();
    match section {
        Section::DeadEnds => {
            out.push_str(&header("DEAD ENDS"));
            let mut stmt = conn.prepare(
                "SELECT id, path, edit_ids, mechanism, command_text, resolved_ms, observed_after_command_id, reapplied \
                 FROM dead_ends WHERE session_id = ?1 AND epoch = ?2 AND resolved_ms <= ?3 ORDER BY resolved_ms DESC, id DESC",
            )?;
            let rows = stmt.query_map(params![session_id, epoch, as_of_ms], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (id, path, edit_ids, mechanism, command, resolved_ms, observed, reapplied) =
                    row?;
                out.push_str(&format!(
                    "\n[{id}] {path} | {mechanism}{} at {}\n",
                    if reapplied != 0 { " (reapplied)" } else { "" },
                    hh_mm(resolved_ms, tz)
                ));
                if let Some(c) = command {
                    out.push_str(&format!("  command: {c}\n"));
                }
                for eid in serde_json::from_str::<Vec<i64>>(&edit_ids).unwrap_or_default() {
                    let edit = conn
                        .prepare_cached(
                            "SELECT path, tool_name, lines_added, lines_removed, excerpt, ts_ms, status FROM edits WHERE id = ?1",
                        )?
                        .query_row([eid], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Option<i64>>(2)?,
                                r.get::<_, Option<i64>>(3)?,
                                r.get::<_, Option<String>>(4)?,
                                r.get::<_, i64>(5)?,
                                r.get::<_, String>(6)?,
                            ))
                        })
                        .optional()?;
                    if let Some((p, tool, added, removed, excerpt, ts, status)) = edit {
                        out.push_str(&format!(
                            "  edit {eid}: {tool} {p} +{}/-{} {status} at {}\n",
                            added.map_or("?".into(), |n| n.to_string()),
                            removed.map_or("?".into(), |n| n.to_string()),
                            hh_mm(ts, tz)
                        ));
                        for line in excerpt.unwrap_or_default().lines() {
                            out.push_str(&format!("      {line}\n"));
                        }
                    }
                }
                if let Some(cid) = observed {
                    let cmd = conn
                        .prepare_cached("SELECT command_text, outcome FROM commands WHERE id = ?1")?
                        .query_row([cid], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        })
                        .optional()?;
                    if let Some((text, outcome)) = cmd {
                        out.push_str(&format!(
                            "  observed afterward: `{text}` {outcome}. Causal link: UNCONFIRMED.\n"
                        ));
                    }
                }
            }
        }
        Section::Failure => {
            out.push_str(&header("FAILING COMMANDS"));
            let mut stmt = conn.prepare(
                "SELECT id, kind, command_text, outcome, exit_code, excerpt, mentioned_paths, ts_ms FROM commands \
                 WHERE session_id = ?1 AND epoch = ?2 AND ts_ms <= ?3 AND outcome = 'FAIL' ORDER BY ts_ms DESC, id DESC",
            )?;
            let rows = stmt.query_map(params![session_id, epoch, as_of_ms], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (id, kind, command, outcome, exit, excerpt, mentioned, ts) = row?;
                out.push_str(&format!("\n[{id}] {kind} {outcome}"));
                if let Some(code) = exit {
                    out.push_str(&format!(" (exit {code})"));
                }
                out.push_str(&format!(" at {}\n  {command}\n", hh_mm(ts, tz)));
                for line in excerpt.unwrap_or_default().lines() {
                    out.push_str(&format!("    {line}\n"));
                }
                if let Some(m) = mentioned {
                    if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&m) {
                        for entry in list {
                            out.push_str(&format!(
                                "  mentions: {}{}\n",
                                entry["path"].as_str().unwrap_or(""),
                                entry["line"]
                                    .as_u64()
                                    .map(|l| format!(":{l}"))
                                    .unwrap_or_default()
                            ));
                        }
                    }
                }
            }
        }
        Section::Files => {
            out.push_str(&header("WORKING FILES"));
            // Same ranking the capsule uses (`snapshot::working_score`), minus
            // the bonus for files another section already names — this view
            // lists every file, so nothing is competing for a slot. Ordering
            // still matches, or "full detail for this section" would hand the
            // reader a differently sorted list than the one they came from.
            let mut stmt = conn.prepare(
                "SELECT path, edits, reads, in_failure, last_touch_ms FROM file_stats \
                 WHERE session_id = ?1 AND epoch = ?2 AND last_touch_ms <= ?3 \
                 ORDER BY (4 * in_failure + 3 * MIN(edits, 3) + 2 * (reads >= 2) + MIN(reads, 3)) DESC, \
                   edits DESC, reads DESC, first_touch_ms ASC, path ASC",
            )?;
            let rows = stmt.query_map(params![session_id, epoch, as_of_ms], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            for row in rows {
                let (path, edits, reads, in_failure, ts) = row?;
                out.push_str(&format!(
                    "\n{path} | edited {edits}x, read {reads}x{} | last touch {}",
                    if in_failure != 0 {
                        ", in failure output"
                    } else {
                        ""
                    },
                    hh_mm(ts, tz)
                ));
            }
            out.push('\n');
        }
        Section::Attempts => {
            out.push_str(&header("EDIT ATTEMPTS"));
            let mut stmt = conn.prepare(
                "SELECT id, path, tool_name, lines_added, lines_removed, excerpt, ts_ms, status, mechanism FROM edits \
                 WHERE session_id = ?1 AND epoch = ?2 AND ts_ms <= ?3 ORDER BY ts_ms DESC, id DESC",
            )?;
            let rows = stmt.query_map(params![session_id, epoch, as_of_ms], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            })?;
            for row in rows {
                let (id, path, tool, added, removed, excerpt, ts, status, mechanism) = row?;
                out.push_str(&format!(
                    "\n[{id}] {tool} {path} +{}/-{} {status}{} at {}\n",
                    added.map_or("?".into(), |n| n.to_string()),
                    removed.map_or("?".into(), |n| n.to_string()),
                    mechanism.map(|m| format!(" ({m})")).unwrap_or_default(),
                    hh_mm(ts, tz)
                ));
                for line in excerpt.unwrap_or_default().lines() {
                    out.push_str(&format!("    {line}\n"));
                }
            }
        }
    }
    Ok(out)
}

/// JSON view of a snapshot for `--json`.
pub fn snapshot_json(s: &Snapshot, capsule: &str, tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "checkpoint_id": s.checkpoint_id,
        "session_id": s.session_id,
        "epoch": s.epoch,
        "created_ms": s.created_ms,
        "partial": s.partial,
        "objective": s.root.as_ref().map(|r| &r.text),
        "subtask": s.subtask.as_ref().map(|r| &r.text),
        "latest_request": s.latest.as_ref().map(|r| &r.text),
        "branch": s.git.as_ref().and_then(|g| g.branch.clone()),
        "head": s.git.as_ref().and_then(|g| g.head.clone()),
        "edits": s.edit_count,
        "failing": s.failing_count,
        "active_failure": s.failure.as_ref().map(|f| serde_json::json!({
            "kind": f.kind.as_str(),
            "command": f.command,
            "exit_code": f.exit_code,
            "excerpt": f.excerpt,
        })),
        "dead_ends": s.dead_ends.iter().map(|d| serde_json::json!({
            "path": d.path,
            "mechanism": d.mechanism.as_str(),
            "command": d.command,
            "edits": d.edit_ids.len(),
        })).collect::<Vec<_>>(),
        "dead_end_total": s.dead_end_total,
        "attempts": s.attempts.iter().map(|a| serde_json::json!({
            "path": a.path, "added": a.added, "removed": a.removed,
        })).collect::<Vec<_>>(),
        "working_files": s.working_files.iter().map(|w| serde_json::json!({
            "path": w.path, "edits": w.edits, "reads": w.reads, "in_failure": w.in_failure,
        })).collect::<Vec<_>>(),
        "next_target": s.next_target.as_ref().map(|t| serde_json::json!({ "rule": t.rule, "target": t.target })),
        "capsule": capsule,
        "capsule_tokens_est": tokens,
    })
}

/// JSON view of a stored checkpoint.
pub fn checkpoint_json(c: &StoredCheckpoint) -> serde_json::Value {
    serde_json::json!({
        "checkpoint_id": c.checkpoint_id,
        "session_id": c.session_id,
        "epoch": c.epoch,
        "created_ms": c.created_ms,
        "trigger": c.trigger,
        "partial": c.partial,
        "summary": c.summary,
        "capsule": c.capsule,
        "capsule_tokens_est": c.tokens,
    })
}
