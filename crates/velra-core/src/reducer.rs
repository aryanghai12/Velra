//! Incremental, cursor-based reducer (§11): turns raw `events` (and spool
//! files) into the materialized tables. Each batch is one `BEGIN IMMEDIATE`
//! transaction that reads the cursor, applies events in id order, advances
//! the cursor and commits — so concurrent reducers are safe by construction
//! and a crash mid-batch rolls back cleanly.

use crate::commands::{self, OutcomeInput};
use crate::db::{self, DbError, Result};
use crate::event::{EventRow, FileObservation, Payload};
use crate::hash;
use crate::intent::{self, IntentAction};
use crate::model::{
    hook_event as he, tools, CommandKind, EditStatus, IntentLevel, Mechanism, Outcome,
    VersionSource,
};
use crate::paths;
use crate::revert::{self, ActiveEdit, OpenDeadEnd, Version};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Events per batch (§11).
pub const BATCH: usize = 200;
/// Files rehashed by a turn-end scan.
pub const TURN_SCAN_MAX_FILES: usize = 64;
/// Version history considered for revert detection.
const HISTORY_LIMIT: i64 = 256;
/// Mentioned paths stored per failing command.
const MENTION_LIMIT: usize = 16;

#[derive(Debug, Clone)]
pub struct ReduceOptions {
    /// Stop starting new work after this instant (§4 watchdogs, §15.2).
    pub deadline: Option<Instant>,
    pub batch: usize,
    /// Spool directory to ingest first.
    pub spool_dir: Option<PathBuf>,
    pub render: crate::render::RenderConfig,
}

impl Default for ReduceOptions {
    fn default() -> Self {
        ReduceOptions {
            deadline: None,
            batch: BATCH,
            spool_dir: None,
            render: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReduceStats {
    pub processed: usize,
    pub spool_ingested: usize,
    /// True when no unreduced events remain.
    pub caught_up: bool,
}

fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

/// Runs reducer batches until caught up or the deadline passes.
pub fn reduce(conn: &mut Connection, opts: &ReduceOptions) -> Result<ReduceStats> {
    let mut stats = ReduceStats::default();
    if let Some(dir) = &opts.spool_dir {
        loop {
            let n = crate::spool::ingest(conn, dir, 500)?;
            stats.spool_ingested += n;
            if n < 500 || expired(opts.deadline) {
                break;
            }
        }
    }
    let batch = opts.batch.max(1);
    loop {
        if expired(opts.deadline) {
            return Ok(stats);
        }
        let cur = db::cursor(conn)?;
        let head: Vec<(i64, String, String)> = conn
            .prepare_cached(
                "SELECT id, session_id, hook_event FROM events WHERE id > ?1 ORDER BY id LIMIT ?2",
            )?
            .query_map(params![cur, batch as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if head.is_empty() {
            stats.caught_up = true;
            return Ok(stats);
        }
        // Turn-end scans hash files; do that outside the write transaction.
        let (take, scan) = match head.iter().position(|(_, _, ev)| ev == he::STOP) {
            Some(0) => (1, Some(turn_scan(conn, &head[0].1)?)),
            Some(p) => (p, None),
            None => (head.len(), None),
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if db::cursor(&tx)? != cur {
            continue; // another reducer advanced; tx rolls back on drop
        }
        let events = load_events(&tx, cur, take)?;
        let mut last = cur;
        for ev in &events {
            apply(&tx, ev, scan.as_deref(), opts)?;
            last = ev.id;
            stats.processed += 1;
            if expired(opts.deadline) {
                break;
            }
        }
        tx.execute(
            "UPDATE reducer_cursor SET last_event_id = ?1 WHERE id = 1",
            [last],
        )?;
        tx.commit()?;
    }
}

fn load_events(conn: &Connection, after: i64, limit: usize) -> rusqlite::Result<Vec<EventRow>> {
    conn.prepare_cached(
        "SELECT id, session_id, project_id, agent_id, hook_event, tool_name, tool_use_id, ts_ms, payload \
         FROM events WHERE id > ?1 ORDER BY id LIMIT ?2",
    )?
    .query_map(params![after, limit as i64], |r| {
        let payload: String = r.get(8)?;
        Ok(EventRow {
            id: r.get(0)?,
            session_id: r.get(1)?,
            project_id: r.get(2)?,
            agent_id: r.get(3)?,
            hook_event: r.get(4)?,
            tool_name: r.get(5)?,
            tool_use_id: r.get(6)?,
            ts_ms: r.get(7)?,
            payload: Payload::from_json(&payload),
        })
    })?
    .collect()
}

/// Project root for a project id.
pub fn project_root(conn: &Connection, project_id: &str) -> rusqlite::Result<Option<PathBuf>> {
    conn.prepare_cached("SELECT root_path FROM projects WHERE project_id = ?1")?
        .query_row([project_id], |r| r.get::<_, String>(0))
        .optional()
        .map(|o| o.map(PathBuf::from))
}

/// Current epoch of a session (1 if unknown).
pub fn session_epoch(conn: &Connection, session_id: &str) -> rusqlite::Result<i64> {
    conn.prepare_cached("SELECT epoch FROM sessions WHERE session_id = ?1")?
        .query_row([session_id], |r| r.get(0))
        .optional()
        .map(|o| o.unwrap_or(1))
}

/// Paths with ACTIVE edits in the session's current epoch, most recent first.
pub fn active_edit_paths(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    let epoch = session_epoch(conn, session_id)?;
    conn.prepare_cached(
        "SELECT path FROM edits WHERE session_id = ?1 AND epoch = ?2 AND status = 'ACTIVE' \
         GROUP BY path ORDER BY MAX(id) DESC LIMIT ?3",
    )?
    .query_map(params![session_id, epoch, limit as i64], |r| r.get(0))?
    .collect()
}

fn turn_scan(conn: &Connection, session_id: &str) -> Result<Vec<FileObservation>> {
    let Some(project_id) = conn
        .prepare_cached("SELECT project_id FROM sessions WHERE session_id = ?1")?
        .query_row([session_id], |r| r.get::<_, String>(0))
        .optional()?
    else {
        return Ok(Vec::new());
    };
    let Some(root) = project_root(conn, &project_id)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for path in active_edit_paths(conn, session_id, TURN_SCAN_MAX_FILES)? {
        let (hash, size) = hash::hash_file(&paths::resolve(&path, &root));
        out.push(FileObservation { path, hash, size });
    }
    Ok(out)
}

struct Ctx<'a> {
    tx: &'a Connection,
    ev: &'a EventRow,
    epoch: i64,
}

fn apply(
    tx: &Connection,
    ev: &EventRow,
    scan: Option<&[FileObservation]>,
    opts: &ReduceOptions,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO sessions (session_id, project_id, started_ms, last_event_ms, transcript_path, epoch) \
         VALUES (?1, ?2, ?3, ?3, ?4, 1) \
         ON CONFLICT(session_id) DO UPDATE SET \
           last_event_ms = MAX(last_event_ms, excluded.last_event_ms), \
           started_ms = MIN(started_ms, excluded.started_ms), \
           transcript_path = COALESCE(excluded.transcript_path, transcript_path)",
    )?
    .execute(params![ev.session_id, ev.project_id, ev.ts_ms, ev.payload.transcript_path])?;
    let mut ctx = Ctx {
        tx,
        ev,
        epoch: session_epoch(tx, &ev.session_id)?,
    };
    let p = &ev.payload;
    let tool = ev.tool_name.as_deref().unwrap_or("");
    match ev.hook_event.as_str() {
        he::SESSION_START => {
            // A new epoch starts here; nothing later in this event reads it.
            if p.source.as_deref() == Some("clear") {
                bump_epoch(&ctx)?;
            }
        }
        he::SESSION_END => {
            tx.prepare_cached(
                "UPDATE sessions SET ended_ms = ?2, end_reason = ?3 WHERE session_id = ?1",
            )?
            .execute(params![ev.session_id, ev.ts_ms, p.reason])?;
        }
        he::USER_PROMPT_SUBMIT => apply_prompt(&mut ctx)?,
        he::PRE_TOOL_USE => {
            if tools::is_edit(tool) {
                if let (Some(path), Some(hash)) = (&p.path, &p.pre_hash) {
                    record_version(
                        &ctx,
                        path,
                        hash,
                        p.size.unwrap_or(0),
                        VersionSource::PreEdit,
                    )?;
                }
            } else if tools::is_shell(tool) {
                if let Some(git) = &p.git {
                    for f in &git.files {
                        record_version(&ctx, &f.path, &f.hash, f.size, VersionSource::GitPre)?;
                    }
                }
            }
        }
        he::POST_TOOL_USE => {
            if tools::is_edit(tool) {
                apply_edit(&ctx)?;
            } else if tool == "Read" {
                if let Some(path) = &p.path {
                    bump_stats(&ctx, path, 1, 0, false)?;
                }
            } else if tools::is_shell(tool) {
                apply_shell(&ctx, false)?;
            }
        }
        he::POST_TOOL_USE_FAILURE => {
            if tools::is_shell(tool) {
                apply_shell(&ctx, true)?;
            }
        }
        he::STOP => {
            for f in scan.unwrap_or(&[]) {
                if last_version(&ctx, &f.path)?.map(|v| v.hash) != Some(f.hash.clone()) {
                    record_version(&ctx, &f.path, &f.hash, f.size, VersionSource::TurnScan)?;
                }
            }
        }
        he::CHECKPOINT_REQUEST => {
            let trigger = p
                .trigger
                .as_deref()
                .and_then(crate::model::Trigger::parse)
                .unwrap_or(crate::model::Trigger::Auto);
            crate::checkpoint::create_in_tx(
                tx,
                &crate::checkpoint::CheckpointRequest {
                    session_id: &ev.session_id,
                    trigger,
                    created_ms: ev.ts_ms,
                    partial: true,
                    watermark: ev.id,
                },
                &opts.render,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn bump_epoch(ctx: &Ctx<'_>) -> Result<i64> {
    ctx.tx
        .prepare_cached("UPDATE sessions SET epoch = epoch + 1 WHERE session_id = ?1")?
        .execute([&ctx.ev.session_id])?;
    Ok(session_epoch(ctx.tx, &ctx.ev.session_id)?)
}

fn live_intent_id(ctx: &Ctx<'_>, level: IntentLevel) -> Result<Option<i64>> {
    Ok(ctx
        .tx
        .prepare_cached(
            "SELECT id FROM intents WHERE session_id = ?1 AND epoch = ?2 AND level = ?3 AND superseded_ms IS NULL \
             ORDER BY id DESC LIMIT 1",
        )?
        .query_row(params![ctx.ev.session_id, ctx.epoch, level.as_str()], |r| r.get(0))
        .optional()?)
}

fn set_intent(ctx: &Ctx<'_>, level: IntentLevel, text: &str) -> Result<()> {
    if level != IntentLevel::Root {
        ctx.tx
            .prepare_cached(
                "UPDATE intents SET superseded_ms = ?4 \
                 WHERE session_id = ?1 AND epoch = ?2 AND level = ?3 AND superseded_ms IS NULL",
            )?
            .execute(params![
                ctx.ev.session_id,
                ctx.epoch,
                level.as_str(),
                ctx.ev.ts_ms
            ])?;
    }
    ctx.tx
        .prepare_cached(
            "INSERT INTO intents (session_id, epoch, level, text, source_event_id, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            ctx.ev.session_id,
            ctx.epoch,
            level.as_str(),
            text,
            ctx.ev.id,
            ctx.ev.ts_ms
        ])?;
    Ok(())
}

fn apply_prompt(ctx: &mut Ctx<'_>) -> Result<()> {
    let norm = intent::normalize_prompt(ctx.ev.payload.prompt.as_deref().unwrap_or(""));
    let has_root = live_intent_id(ctx, IntentLevel::Root)?.is_some();
    match intent::classify_prompt(&norm, has_root) {
        IntentAction::None => {}
        IntentAction::NewEpoch { root } => {
            ctx.epoch = bump_epoch(ctx)?;
            if let Some(r) = root {
                set_intent(ctx, IntentLevel::Root, &r)?;
            }
        }
        IntentAction::Subtask(t) => set_intent(ctx, IntentLevel::Subtask, &t)?,
        IntentAction::Root(t) => set_intent(ctx, IntentLevel::Root, &t)?,
        IntentAction::Latest(t) => set_intent(ctx, IntentLevel::Latest, &t)?,
    }
    Ok(())
}

fn bump_stats(ctx: &Ctx<'_>, path: &str, reads: i64, edits: i64, in_failure: bool) -> Result<()> {
    ctx.tx
        .prepare_cached(
            "INSERT INTO file_stats (session_id, epoch, path, reads, edits, in_failure, last_touch_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(session_id, epoch, path) DO UPDATE SET \
               reads = reads + excluded.reads, edits = edits + excluded.edits, \
               in_failure = MAX(in_failure, excluded.in_failure), \
               last_touch_ms = MAX(last_touch_ms, excluded.last_touch_ms)",
        )?
        .execute(params![ctx.ev.session_id, ctx.epoch, path, reads, edits, i64::from(in_failure), ctx.ev.ts_ms])?;
    Ok(())
}

fn versions(ctx: &Ctx<'_>, path: &str) -> Result<Vec<Version>> {
    let mut rows: Vec<Version> = ctx
        .tx
        .prepare_cached(
            "SELECT id, content_hash, source, event_id FROM file_versions \
             WHERE session_id = ?1 AND path = ?2 ORDER BY id DESC LIMIT ?3",
        )?
        .query_map(params![ctx.ev.session_id, path, HISTORY_LIMIT], |r| {
            let source: String = r.get(2)?;
            Ok(Version {
                id: r.get(0)?,
                hash: r.get(1)?,
                source: VersionSource::parse(&source).unwrap_or(VersionSource::TurnScan),
                event_id: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.reverse();
    Ok(rows)
}

fn last_version(ctx: &Ctx<'_>, path: &str) -> Result<Option<Version>> {
    Ok(versions(ctx, path)?.pop())
}

fn active_edits(ctx: &Ctx<'_>, path: &str) -> Result<Vec<ActiveEdit>> {
    Ok(ctx
        .tx
        .prepare_cached(
            "SELECT id, event_id, post_hash FROM edits WHERE session_id = ?1 AND path = ?2 AND status = 'ACTIVE' ORDER BY id",
        )?
        .query_map(params![ctx.ev.session_id, path], |r| {
            Ok(ActiveEdit { id: r.get(0)?, event_id: r.get(1)?, post_hash: r.get(2)? })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn open_dead_ends(ctx: &Ctx<'_>, path: &str) -> Result<Vec<OpenDeadEnd>> {
    let rows: Vec<(i64, String)> = ctx
        .tx
        .prepare_cached("SELECT id, edit_ids FROM dead_ends WHERE session_id = ?1 AND path = ?2 AND reapplied = 0")?
        .query_map(params![ctx.ev.session_id, path], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, edit_ids) in rows {
        let ids: Vec<i64> = serde_json::from_str(&edit_ids).unwrap_or_default();
        let mut post_hashes = Vec::with_capacity(ids.len());
        for eid in ids {
            if let Some(h) = ctx
                .tx
                .prepare_cached("SELECT post_hash FROM edits WHERE id = ?1")?
                .query_row([eid], |r| r.get::<_, String>(0))
                .optional()?
            {
                post_hashes.push(h);
            }
        }
        out.push(OpenDeadEnd { id, post_hashes });
    }
    Ok(out)
}

fn resolve_edits(
    ctx: &Ctx<'_>,
    ids: &[i64],
    status: EditStatus,
    mechanism: Option<Mechanism>,
) -> Result<()> {
    let mut stmt = ctx.tx.prepare_cached(
        "UPDATE edits SET status = ?2, mechanism = ?3, resolved_event_id = ?4 WHERE id = ?1 AND status = 'ACTIVE'",
    )?;
    for id in ids {
        stmt.execute(params![
            id,
            status.as_str(),
            mechanism.map(Mechanism::as_str),
            ctx.ev.id
        ])?;
    }
    Ok(())
}

fn insert_dead_end(
    ctx: &Ctx<'_>,
    path: &str,
    edit_ids: &[i64],
    mechanism: Mechanism,
    command: Option<&str>,
) -> Result<()> {
    let first_ts: Option<i64> = {
        let mut min: Option<i64> = None;
        let mut stmt = ctx
            .tx
            .prepare_cached("SELECT ts_ms FROM edits WHERE id = ?1")?;
        for id in edit_ids {
            if let Some(ts) = stmt.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
                min = Some(min.map_or(ts, |m: i64| m.min(ts)));
            }
        }
        min
    };
    let observed: Option<i64> = match first_ts {
        Some(first) => ctx
            .tx
            .prepare_cached(
                "SELECT id FROM commands WHERE session_id = ?1 AND kind IN ('test', 'build') \
                 AND ts_ms >= ?2 AND ts_ms <= ?3 ORDER BY ts_ms, id LIMIT 1",
            )?
            .query_row(params![ctx.ev.session_id, first, ctx.ev.ts_ms], |r| {
                r.get(0)
            })
            .optional()?,
        None => None,
    };
    let ids_json = serde_json::to_string(edit_ids).unwrap_or_else(|_| "[]".into());
    ctx.tx
        .prepare_cached(
            "INSERT INTO dead_ends (session_id, epoch, path, edit_ids, mechanism, command_text, resolved_ms, observed_after_command_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![ctx.ev.session_id, ctx.epoch, path, ids_json, mechanism.as_str(), command, ctx.ev.ts_ms, observed])?;
    Ok(())
}

/// Records a version and runs reapplication, then hash-return revert
/// detection (§13.2, §13.4).
fn record_version(
    ctx: &Ctx<'_>,
    path: &str,
    hash: &str,
    size: u64,
    source: VersionSource,
) -> Result<()> {
    let history = versions(ctx, path)?;
    ctx.tx
        .prepare_cached(
            "INSERT INTO file_versions (session_id, path, content_hash, size, source, event_id, ts_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            ctx.ev.session_id,
            path,
            hash,
            i64::try_from(size).unwrap_or(i64::MAX),
            source.as_str(),
            ctx.ev.id,
            ctx.ev.ts_ms
        ])?;
    if history.last().is_some_and(|v| v.hash == hash) {
        return Ok(());
    }
    let reapplied = revert::reapplied(hash, &open_dead_ends(ctx, path)?);
    if !reapplied.is_empty() {
        let mut upd = ctx
            .tx
            .prepare_cached("UPDATE dead_ends SET reapplied = 1 WHERE id = ?1")?;
        let mut get = ctx
            .tx
            .prepare_cached("SELECT edit_ids FROM dead_ends WHERE id = ?1")?;
        let mut edit = ctx
            .tx
            .prepare_cached("UPDATE edits SET status = 'REAPPLIED' WHERE id = ?1")?;
        for id in reapplied {
            upd.execute([id])?;
            let ids: String = get.query_row([id], |r| r.get(0))?;
            for eid in serde_json::from_str::<Vec<i64>>(&ids).unwrap_or_default() {
                edit.execute([eid])?;
            }
        }
        return Ok(());
    }
    let active = active_edits(ctx, path)?;
    let reverted = revert::detect_revert(&history, hash, &active);
    if !reverted.is_empty() {
        let mechanism = revert::mechanism_for(source, ctx.ev.tool_name.as_deref());
        resolve_edits(ctx, &reverted, EditStatus::Reverted, Some(mechanism))?;
        let command = if mechanism == Mechanism::GitCommand {
            ctx.ev.payload.command.as_deref()
        } else {
            None
        };
        insert_dead_end(ctx, path, &reverted, mechanism, command)?;
    }
    Ok(())
}

fn apply_edit(ctx: &Ctx<'_>) -> Result<()> {
    let p = &ctx.ev.payload;
    let (Some(path), Some(post)) = (&p.path, &p.post_hash) else {
        return Ok(());
    };
    let mut last = last_version(ctx, path)?.map(|v| v.hash);
    if let Some(orig) = &p.original_hash {
        if last.as_deref() != Some(orig.as_str()) {
            record_version(ctx, path, orig, 0, VersionSource::Original)?;
            last = Some(orig.clone());
        }
    }
    ctx.tx
        .prepare_cached(
            "INSERT OR IGNORE INTO edits (session_id, epoch, event_id, agent_id, path, tool_name, pre_hash, post_hash, \
             lines_added, lines_removed, excerpt, ts_ms, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'ACTIVE')",
        )?
        .execute(params![
            ctx.ev.session_id,
            ctx.epoch,
            ctx.ev.id,
            ctx.ev.agent_id,
            path,
            ctx.ev.tool_name.as_deref().unwrap_or("Edit"),
            last,
            post,
            p.lines_added,
            p.lines_removed,
            p.excerpt,
            ctx.ev.ts_ms
        ])?;
    bump_stats(ctx, path, 0, 1, false)?;
    record_version(
        ctx,
        path,
        post,
        p.size.unwrap_or(0),
        VersionSource::PostEdit,
    )
}

fn resolve_mentions(ctx: &Ctx<'_>, output: &str) -> Result<Vec<serde_json::Value>> {
    let Some(root) = project_root(ctx.tx, &ctx.ev.project_id)? else {
        return Ok(Vec::new());
    };
    let root_str = root.to_string_lossy().into_owned();
    let cwd = ctx.ev.payload.cwd.as_deref().map(Path::new);
    let mut out: Vec<serde_json::Value> = Vec::new();
    for tok in commands::path_tokens(output, 40) {
        if out.len() >= MENTION_LIMIT {
            break;
        }
        let candidates: Vec<PathBuf> = if paths::is_absolute_str(&tok.path) {
            vec![PathBuf::from(&tok.path)]
        } else {
            cwd.map(|c| c.join(&tok.path))
                .into_iter()
                .chain(std::iter::once(root.join(&tok.path)))
                .collect()
        };
        let Some(found) = candidates.into_iter().find(|c| c.is_file()) else {
            continue;
        };
        let rel = paths::relative_to_root(&found.to_string_lossy(), &root_str);
        if paths::is_absolute_str(&rel) {
            continue; // outside the project root
        }
        let rel = rel.replace("/./", "/");
        if out
            .iter()
            .any(|m| m["path"] == rel && m["line"] == serde_json::json!(tok.line))
        {
            continue;
        }
        out.push(serde_json::json!({ "path": rel, "line": tok.line, "raw": tok.raw }));
    }
    Ok(out)
}

fn apply_shell(ctx: &Ctx<'_>, failure: bool) -> Result<()> {
    let p = &ctx.ev.payload;
    let Some(command) = p.command.as_deref().filter(|c| !c.trim().is_empty()) else {
        return Ok(());
    };
    let classified = commands::classify(command);
    let (exit_code, output) = if failure {
        let err = p.error.as_deref().unwrap_or("");
        let (code, rest) = commands::parse_exit_code_prefix(err);
        (p.exit_code.or(code), rest.to_string())
    } else {
        let mut out = p.stdout_tail.clone().unwrap_or_default();
        if let Some(e) = p.stderr_tail.as_deref().filter(|e| !e.is_empty()) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(e);
        }
        (p.exit_code, out)
    };
    let interrupted = p.interrupted == Some(true) || p.is_interrupt == Some(true);
    let outcome = commands::outcome(OutcomeInput {
        kind: classified.kind,
        failure_event: failure,
        success_event: !failure,
        exit_code,
        interrupted,
        output: &output,
    });
    let runner = matches!(
        classified.kind,
        CommandKind::Test | CommandKind::Build | CommandKind::Lint
    );
    let (excerpt, mentions) = if outcome == Outcome::Fail {
        let ex = commands::failure_excerpt(&output).join("\n");
        let m = if runner {
            resolve_mentions(ctx, &output)?
        } else {
            Vec::new()
        };
        (Some(ex), m)
    } else {
        (None, Vec::new())
    };
    let mentions_json =
        (!mentions.is_empty()).then(|| serde_json::Value::Array(mentions.clone()).to_string());
    ctx.tx
        .prepare_cached(
            "INSERT OR IGNORE INTO commands (session_id, epoch, event_id, kind, signature, command_text, outcome, \
             exit_code, excerpt, mentioned_paths, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?
        .execute(params![
            ctx.ev.session_id,
            ctx.epoch,
            ctx.ev.id,
            classified.kind.as_str(),
            commands::signature(classified.kind, &classified.subcommand),
            command,
            outcome.as_str(),
            exit_code,
            excerpt,
            mentions_json,
            ctx.ev.ts_ms
        ])?;
    for m in &mentions {
        if let Some(path) = m["path"].as_str() {
            bump_stats(ctx, path, 0, 0, true)?;
        }
    }
    if failure {
        return Ok(());
    }
    let Some(git) = &p.git else { return Ok(()) };
    for f in &git.files {
        if let Some(restore) = &git.restore {
            let history = versions(ctx, &f.path)?;
            let git_pre = history
                .last()
                .filter(|v| v.source == VersionSource::GitPre)
                .map(|v| v.hash.as_str());
            let last_post = history
                .iter()
                .rev()
                .find(|v| v.source == VersionSource::PostEdit)
                .map(|v| v.hash.as_str());
            let since_commit: Vec<i64> = ctx
                .tx
                .prepare_cached(
                    "SELECT id FROM edits WHERE session_id = ?1 AND path = ?2 AND status = 'ACTIVE' AND id > \
                     COALESCE((SELECT MAX(id) FROM edits WHERE session_id = ?1 AND path = ?2 AND status = 'COMMITTED'), 0) \
                     ORDER BY id",
                )?
                .query_map(params![ctx.ev.session_id, f.path], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if revert::is_discarded(last_post, git_pre, &f.hash, !since_commit.is_empty()) {
                resolve_edits(
                    ctx,
                    &since_commit,
                    EditStatus::Discarded,
                    Some(Mechanism::GitCommand),
                )?;
                insert_dead_end(
                    ctx,
                    &f.path,
                    &since_commit,
                    Mechanism::GitCommand,
                    Some(restore),
                )?;
            }
        }
        if git.commit {
            let active = active_edits(ctx, &f.path)?;
            if let Some(pos) = active.iter().rposition(|e| e.post_hash == f.hash) {
                let ids: Vec<i64> = active[..=pos].iter().map(|e| e.id).collect();
                resolve_edits(ctx, &ids, EditStatus::Committed, None)?;
            }
        }
        record_version(ctx, &f.path, &f.hash, f.size, VersionSource::GitPost)?;
    }
    Ok(())
}

/// Convenience for tests and the CLI: reduce everything now.
pub fn reduce_all(conn: &mut Connection, spool_dir: Option<&Path>) -> Result<ReduceStats> {
    reduce(
        conn,
        &ReduceOptions {
            spool_dir: spool_dir.map(Path::to_path_buf),
            ..Default::default()
        },
    )
}

/// Maps busy errors for callers that treat them as "try later".
pub fn is_retryable(e: &DbError) -> bool {
    matches!(e, DbError::Busy)
}
