//! Incremental, cursor-based reducer (§11): turns raw `events` (and spool
//! files) into the materialized tables. Each batch is one `BEGIN IMMEDIATE`
//! transaction that reads the cursor, applies events, advances the cursor and
//! commits — so concurrent reducers are safe by construction and a crash
//! mid-batch rolls back cleanly.
//!
//! # Order
//!
//! A session's projection is the fold of its events in **logical** order
//! (`crate::order`), not in the order rows were inserted. The cursor walks
//! rows in id order, and for almost every event the two agree: a directly
//! appended event is always logically last among what has been ingested. A
//! spooled event can belong earlier. When one does, the session is rebuilt:
//! its projection rows are cleared and every ingested event of the session is
//! applied again in logical order ([`rebuild_session`]). The projection is
//! derived state, so a rebuild changes nothing but its order; row ids of the
//! rebuilt rows then follow logical order too, which is what the ordered
//! queries downstream read.
//!
//! Applying an event reads nothing but the ledger. Two observations used to
//! be made here, against the disk as it was whenever the reducer happened to
//! run, and filed under the event's time: the turn-end file scan, and which
//! paths a failing command's output names that exist. Both are now made by
//! the hook when the event happens and carried in it (`Payload::turn_scan`,
//! `Payload::mentioned`). A rebuild keeps the mentions of each command's
//! first reduction ([`Replay::mentions`]), which for rows reduced by an
//! earlier build are the only record of them.

use crate::commands::{self, OutcomeInput};
use crate::constraint;
use crate::db::{self, DbError, Result};
use crate::event::{EventRow, Payload};
use crate::intent::{self, IntentAction};
use crate::model::{
    hook_event as he, tools, CommandKind, EditStatus, IntentLevel, Mechanism, Outcome,
    VersionSource,
};
use crate::revert::{self, ActiveEdit, OpenDeadEnd, Version};
use crate::shell;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Events per batch (§11).
pub const BATCH: usize = 200;
/// Version history considered for revert detection.
const HISTORY_LIMIT: i64 = 256;

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
    // One for the whole reduction: its turn counts describe event rows, which
    // do not change, so they hold across batches, rebuilds and retries.
    let incremental = Replay::default();
    run_pending_rebuilds(conn, opts)?;
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
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if db::cursor(&tx)? != cur {
            continue; // another reducer advanced; tx rolls back on drop
        }
        let events = load_events(&tx, cur, head.len())?;
        let mut last = cur;
        // Events a rebuild already applied, by id.
        let mut rebuilt_upto: std::collections::HashMap<String, i64> = Default::default();
        for (k, ev) in events.iter().enumerate() {
            let covered = rebuilt_upto
                .get(&ev.session_id)
                .is_some_and(|&upto| ev.id <= upto);
            if !covered {
                if is_logically_last(&tx, ev)? {
                    apply(&tx, ev, opts, &incremental)?;
                } else {
                    // One rebuild covers every event of the session in this
                    // batch: a drained spool arrives as a run of late rows.
                    let upto = events[k..]
                        .iter()
                        .filter(|e| e.session_id == ev.session_id)
                        .map(|e| e.id)
                        .max()
                        .unwrap_or(ev.id);
                    if rebuild_fits(&tx, &ev.session_id, upto, opts.deadline)? {
                        rebuild_session(&tx, &ev.session_id, upto, opts)?;
                        rebuilt_upto.insert(ev.session_id.clone(), upto);
                    } else {
                        // Not in the time this reduction has: fold it where it
                        // arrived for now, and leave the rebuild to one that
                        // has the time. A rebuild the watchdog cut short would
                        // roll back and be retried by every reduction after,
                        // and the cursor -- shared by every session -- would
                        // never move past it.
                        apply(&tx, ev, opts, &incremental)?;
                        mark_pending_rebuild(&tx, &ev.session_id)?;
                    }
                }
            }
            last = ev.id;
            stats.processed += 1;
            if expired(opts.deadline) && !rebuilt_upto.values().any(|&u| u > ev.id) {
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
    .query_map(params![after, limit as i64], row_to_event)?
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

/// The files a hook hashes to observe a session's state: those edited in the
/// current epoch, including edits the reducer has not processed yet, most
/// recently edited first, at most `limit`. Used by the Stop hook's turn-end
/// scan and around git restore-family commands.
pub fn scan_paths(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    const SQL: &str = "SELECT path FROM ( \
         SELECT path AS path, MAX(id) AS ord FROM edits \
           WHERE session_id = ?1 AND epoch = COALESCE((SELECT epoch FROM sessions WHERE session_id = ?1), 1) GROUP BY path \
         UNION ALL \
         SELECT json_extract(payload, '$.path') AS path, MAX(id) AS ord FROM events \
           WHERE session_id = ?1 AND hook_event = 'PostToolUse' \
             AND tool_name IN ('Write', 'Edit', 'MultiEdit', 'NotebookEdit') \
             AND id > COALESCE((SELECT last_event_id FROM reducer_cursor WHERE id = 1), 0) GROUP BY path) \
         WHERE path IS NOT NULL GROUP BY path ORDER BY MAX(ord) DESC LIMIT ?2";
    conn.prepare_cached(SQL)?
        .query_map(params![session_id, limit as i64], |r| r.get::<_, String>(0))?
        .collect()
}

/// How an event is being applied: incrementally (the default), or as part of
/// a session rebuild.
#[derive(Default)]
struct Replay {
    /// Applying events again in logical order ([`rebuild_session`]).
    rebuilding: bool,
    /// The event's position among the session's user turns, counted by the
    /// rebuild as it goes; `None` counts the rows already applied.
    ordinal: Option<i64>,
    /// `commands.mentioned_paths` from the first reduction, by event id. An
    /// earlier build checked them against the disk as it was when it reduced;
    /// a rebuild keeps that rather than dropping what it cannot re-derive.
    mentions: std::collections::HashMap<i64, Option<String>>,
    /// Incremental turn counts, by session: `(counted through this id, turns)`.
    /// Each prompt event is parsed once per reduction, not once per later
    /// prompt that states a rule -- which made reducing a long session of
    /// rule-bearing prompts quadratic (20,000 events took 62 s).
    turns: std::cell::RefCell<std::collections::HashMap<String, (i64, i64)>>,
}

/// The projection tables a session's events are folded into.
const SESSION_TABLES: &[&str] = &[
    "intents",
    "constraints",
    "file_versions",
    "edits",
    "dead_ends",
    "commands",
    "file_stats",
];

/// The logical-order keys of a session's events with ids up to `upto`.
fn session_keys(conn: &Connection, session_id: &str, upto: i64) -> Result<Vec<crate::order::Key>> {
    Ok(conn
        .prepare_cached(
            "SELECT id, ts_ms, COALESCE(json_extract(payload, '$.spooled'), 0) FROM events \
             WHERE session_id = ?1 AND id <= ?2 ORDER BY id",
        )?
        .query_map(params![session_id, upto], |r| {
            Ok(crate::order::Key {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                spooled: r.get::<_, i64>(2)? != 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether `ev`, about to be applied after every earlier row, is logically
/// last in its session. Always true for a directly appended event
/// (`crate::order`); a spooled one is checked.
fn is_logically_last(conn: &Connection, ev: &EventRow) -> Result<bool> {
    if ev.payload.spooled != Some(true) {
        return Ok(true);
    }
    let keys = session_keys(conn, &ev.session_id, ev.id)?;
    let order = crate::order::logical_order(&keys);
    Ok(order.last().map(|&i| keys[i].id) == Some(ev.id))
}

/// Estimated cost of re-folding one event, from measurement on the release
/// build (Phase 2): 17 ms for 1,000 events, 108 ms for 5,000, 544 ms for
/// 15,000 -- 18 to 36 microseconds each -- with margin.
const REBUILD_US_PER_EVENT: u128 = 60;

/// Whether rebuilding `session_id` through `upto` fits in what is left of
/// `deadline`. Without a deadline anything fits.
fn rebuild_fits(
    conn: &Connection,
    session_id: &str,
    upto: i64,
    deadline: Option<Instant>,
) -> Result<bool> {
    let Some(deadline) = deadline else {
        return Ok(true);
    };
    let count: i64 = conn
        .prepare_cached("SELECT COUNT(*) FROM events WHERE session_id = ?1 AND id <= ?2")?
        .query_row(params![session_id, upto], |r| r.get(0))?;
    let left = deadline
        .saturating_duration_since(Instant::now())
        .as_micros();
    Ok(left >= count.max(0) as u128 * REBUILD_US_PER_EVENT)
}

/// `meta` key recording that a session's projection holds an event folded
/// out of logical order and needs a rebuild.
fn pending_key(session_id: &str) -> String {
    format!("rebuild:{session_id}")
}

fn mark_pending_rebuild(tx: &Connection, session_id: &str) -> Result<()> {
    tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')")?
        .execute([pending_key(session_id)])?;
    Ok(())
}

/// Sessions whose projection is waiting for a rebuild.
pub fn pending_rebuilds(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached("SELECT substr(key, 9) FROM meta WHERE key LIKE 'rebuild:%' ORDER BY key")?
        .query_map([], |r| r.get(0))?
        .collect()
}

/// Rebuilds, through the cursor, every session a deadline once made wait,
/// as far as this reduction's own deadline allows.
fn run_pending_rebuilds(conn: &mut Connection, opts: &ReduceOptions) -> Result<()> {
    for session in pending_rebuilds(conn)? {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let upto = db::cursor(&tx)?;
        if !rebuild_fits(&tx, &session, upto, opts.deadline)? {
            continue;
        }
        rebuild_session(&tx, &session, upto, opts)?;
        tx.commit()?;
    }
    Ok(())
}

/// Folds a session's events with ids up to `upto` again, in logical order.
///
/// Its projection rows are deleted and its `sessions` row is reset to what
/// the fold recomputes; checkpoints, continuations and deliveries are not
/// projection and are left alone, and a `checkpoint_request` is not acted on
/// twice.
fn rebuild_session(
    tx: &Connection,
    session_id: &str,
    upto: i64,
    opts: &ReduceOptions,
) -> Result<()> {
    let mut replay = Replay {
        rebuilding: true,
        ..Default::default()
    };
    replay.mentions = tx
        .prepare_cached("SELECT event_id, mentioned_paths FROM commands WHERE session_id = ?1")?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for table in SESSION_TABLES {
        tx.execute(
            &format!("DELETE FROM {table} WHERE session_id = ?1"),
            [session_id],
        )?;
    }
    tx.prepare_cached(
        "UPDATE sessions SET epoch = 1, started_ms = ?2, last_event_ms = ?3, \
         ended_ms = NULL, end_reason = NULL WHERE session_id = ?1",
    )?
    .execute(params![session_id, i64::MAX, i64::MIN])?;
    let events: Vec<EventRow> = tx
        .prepare_cached(
            "SELECT id, session_id, project_id, agent_id, hook_event, tool_name, tool_use_id, ts_ms, payload \
             FROM events WHERE session_id = ?1 AND id <= ?2 ORDER BY id",
        )?
        .query_map(params![session_id, upto], row_to_event)?
        .collect::<rusqlite::Result<_>>()?;
    let keys: Vec<crate::order::Key> = events
        .iter()
        .map(|e| crate::order::Key {
            id: e.id,
            ts_ms: e.ts_ms,
            spooled: e.payload.spooled == Some(true),
        })
        .collect();
    // Every rebuild reaches at least every event already applied, so it
    // discharges a rebuild a deadline once deferred.
    tx.execute("DELETE FROM meta WHERE key = ?1", [pending_key(session_id)])?;
    let mut turns = 0i64;
    for i in crate::order::logical_order(&keys) {
        let ev = &events[i];
        let prompt = ev.hook_event == he::USER_PROMPT_SUBMIT;
        replay.ordinal = prompt.then_some(turns);
        apply(tx, ev, opts, &replay)?;
        if prompt && is_turn(&ev.payload) {
            turns += 1;
        }
    }
    Ok(())
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
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
}

/// Whether a `UserPromptSubmit` payload is one of the user's turns: it holds
/// text the user wrote, or it was recorded without being processed (its text
/// is unknown, but it was the user's turn far more likely than not).
fn is_turn(p: &Payload) -> bool {
    let unprocessed = p
        .prompt_facts
        .as_ref()
        .is_some_and(|f| f.usable() && f.unprocessed);
    unprocessed || !crate::prompt::authored(p.prompt.as_deref().unwrap_or("")).is_empty()
}

struct Ctx<'a> {
    tx: &'a Connection,
    ev: &'a EventRow,
    /// Logical time (see [`apply`]).
    ts: i64,
    epoch: i64,
    replay: &'a Replay,
}

fn apply(tx: &Connection, ev: &EventRow, opts: &ReduceOptions, replay: &Replay) -> Result<()> {
    // Logical time: the event's clock, never earlier than anything already
    // folded for the session. Events are folded in logical order, so this is
    // the running maximum along it, and every timestamp the projection stores
    // orders exactly as the fold did -- the snapshot's `ORDER BY ts_ms`
    // queries included -- even when the wall clock ran backwards. The raw
    // clock stays in `events`.
    let folded: Option<i64> = tx
        .prepare_cached("SELECT last_event_ms FROM sessions WHERE session_id = ?1")?
        .query_row([&ev.session_id], |r| r.get(0))
        .optional()?;
    let ts = folded.map_or(ev.ts_ms, |f| f.max(ev.ts_ms));
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
        ts,
        epoch: session_epoch(tx, &ev.session_id)?,
        replay,
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
            .execute(params![ev.session_id, ts, p.reason])?;
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
            // The files as the Stop hook found them when the turn ended.
            // Without that observation there is no turn-end state to record:
            // the disk as it is now is not what it was then.
            for f in p.turn_scan.as_deref().unwrap_or(&[]) {
                if last_version(&ctx, &f.path)?.map(|v| v.hash) != Some(f.hash.clone()) {
                    record_version(&ctx, &f.path, &f.hash, f.size, VersionSource::TurnScan)?;
                }
            }
        }
        // A rebuild re-folds projection; the checkpoint this request made the
        // first time is not projection, and is not made again.
        he::CHECKPOINT_REQUEST if replay.rebuilding => {}
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
                    created_ms: ts,
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
                ctx.ts
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
            ctx.ts
        ])?;
    Ok(())
}

fn apply_prompt(ctx: &mut Ctx<'_>) -> Result<()> {
    // Only what the user wrote is classified. Context a client injected around
    // it -- the VS Code extension's `<ide_opened_file>`, a background task's
    // `<task-notification>` -- is neither an objective nor a constraint, and a
    // prompt made of nothing else changes no intent (`crate::prompt`).
    let raw = crate::prompt::authored(ctx.ev.payload.prompt.as_deref().unwrap_or(""));
    let norm = intent::normalize_prompt(raw);
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
    // Constraints are recorded for every prompt, including ones that change no
    // intent: a rule can be stated in a follow-up as easily as in turn 0, and
    // the intent rules would classify that follow-up as nothing more than the
    // LATEST message and truncate it away.
    record_constraints(ctx, raw)?;
    Ok(())
}

/// The ordinal of this prompt among the user's turns in the session, 0-based.
///
/// Only prompts that carry text the user wrote are turns. A prompt made of
/// nothing but injected context -- a background task's `<task-notification>`,
/// slash-command bookkeeping -- is delivered through the same hook but is not
/// the user speaking; counting it made a rule stated in the user's third
/// message read `turn 42` after forty notifications.
///
/// A prompt recorded without being processed (`PromptFacts::unprocessed`:
/// the hook's deadline, or input too large to parse) is counted: its text is
/// unknown, but it was the user's turn far more likely than not, and leaving
/// it out would number every later rule one turn early.
///
/// Counted in **logical** order (`crate::order`), the one order the
/// projection is folded in: a spooled prompt ingested after later ones is
/// numbered where it happened, because its arrival rebuilds the session. It
/// is not an ingestion counter and not a row id.
fn prompt_ordinal(ctx: &Ctx<'_>) -> Result<i64> {
    if let Some(n) = ctx.replay.ordinal {
        return Ok(n);
    }
    // Applied incrementally, the event is logically last in its session, so
    // every earlier row of the session precedes it: the turns among them are
    // counted, continuing from what this reduction already counted.
    let mut cache = ctx.replay.turns.borrow_mut();
    let (mut through, mut n) = cache
        .get(&ctx.ev.session_id)
        .copied()
        .filter(|&(through, _)| through < ctx.ev.id)
        .unwrap_or((0, 0));
    let mut stmt = ctx.tx.prepare_cached(
        // `+session_id`: walk the id range, not the session's whole index
        // entry, so continuing a count costs the rows since the last one.
        "SELECT id, payload FROM events WHERE id > ?2 AND id < ?3 \
         AND +session_id = ?1 AND hook_event = 'UserPromptSubmit' ORDER BY id",
    )?;
    let mut rows = stmt.query(params![ctx.ev.session_id, through, ctx.ev.id])?;
    while let Some(row) = rows.next()? {
        through = row.get(0)?;
        if is_turn(&Payload::from_json(&row.get::<_, String>(1)?)) {
            n += 1;
        }
    }
    cache.insert(ctx.ev.session_id.clone(), (through, n));
    Ok(n)
}

/// Records a prompt's constraint sentences.
///
/// They come from `prompt_facts` when the hook recorded it: extracted from
/// the whole prompt before it was bounded for storage, so a rule stated past
/// the bound is still a rule (`crate::prompt::for_storage`). Events written
/// without it -- before v0.1.2's Phase 1B hardening, or appended by a tool
/// that does not bound the prompt -- are extracted from the stored text, as
/// they always were. A record whose kind this build does not know is skipped,
/// not guessed at.
fn record_constraints(ctx: &Ctx<'_>, prompt: &str) -> Result<()> {
    // A record from a build this one does not know is not read as one it does
    // (`PromptFacts::usable`): the stored prompt is extracted instead.
    let facts = ctx.ev.payload.prompt_facts.as_ref().filter(|f| f.usable());
    let found: Vec<(String, String, &'static str)> = match facts {
        Some(facts) => facts
            .constraints
            .iter()
            .filter_map(|c| {
                let kind = constraint::ConstraintKind::parse(&c.kind)?;
                Some((c.text.clone(), c.cue.clone(), kind.as_str()))
            })
            .collect(),
        None => constraint::extract(prompt)
            .into_iter()
            .map(|c| (c.text, c.cue.to_string(), c.kind.as_str()))
            .collect(),
    };
    if found.is_empty() {
        return Ok(());
    }
    let ordinal = prompt_ordinal(ctx)?;
    let mut stmt = ctx.tx.prepare_cached(
        "INSERT OR IGNORE INTO constraints          (session_id, epoch, text, cue, kind, source_event_id, prompt_ordinal, created_ms)          VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for (text, cue, kind) in found {
        stmt.execute(params![
            ctx.ev.session_id,
            ctx.epoch,
            text,
            cue,
            kind,
            ctx.ev.id,
            ordinal,
            ctx.ts
        ])?;
    }
    Ok(())
}

fn bump_stats(ctx: &Ctx<'_>, path: &str, reads: i64, edits: i64, in_failure: bool) -> Result<()> {
    ctx.tx
        .prepare_cached(
            "INSERT INTO file_stats (session_id, epoch, path, reads, edits, in_failure, last_touch_ms, first_touch_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) \
             ON CONFLICT(session_id, epoch, path) DO UPDATE SET \
               reads = reads + excluded.reads, edits = edits + excluded.edits, \
               in_failure = MAX(in_failure, excluded.in_failure), \
               last_touch_ms = MAX(last_touch_ms, excluded.last_touch_ms), \
               first_touch_ms = MIN(first_touch_ms, excluded.first_touch_ms)",
        )?
        .execute(params![ctx.ev.session_id, ctx.epoch, path, reads, edits, i64::from(in_failure), ctx.ts])?;
    Ok(())
}

/// Version history for `(session, path)`, oldest first, in **logical** order.
///
/// Ordering is by hook timestamp and only then by row id. A hook that cannot
/// reach the database spools its event and the reducer ingests it later, which
/// gives it a row id newer than events that really happened after it; reading
/// history in insertion order therefore makes an old snapshot look like the
/// newest state of the file. Revert and reapplication decisions are made from
/// this sequence, so it has to be the order things happened in.
fn versions(ctx: &Ctx<'_>, path: &str) -> Result<Vec<Version>> {
    let mut rows: Vec<Version> = ctx
        .tx
        .prepare_cached(
            "SELECT id, content_hash, source, event_id, ts_ms FROM file_versions \
             WHERE session_id = ?1 AND path = ?2 ORDER BY ts_ms DESC, id DESC LIMIT ?3",
        )?
        .query_map(params![ctx.ev.session_id, path, HISTORY_LIMIT], |r| {
            let source: String = r.get(2)?;
            Ok(Version {
                id: r.get(0)?,
                hash: r.get(1)?,
                source: VersionSource::parse(&source).unwrap_or(VersionSource::TurnScan),
                event_id: r.get(3)?,
                ts_ms: r.get(4)?,
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
    let rows: Vec<(i64, String, i64)> = ctx
        .tx
        .prepare_cached("SELECT id, edit_ids, resolved_ms FROM dead_ends WHERE session_id = ?1 AND path = ?2 AND reapplied = 0")?
        .query_map(params![ctx.ev.session_id, path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, edit_ids, resolved_ms) in rows {
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
        out.push(OpenDeadEnd {
            id,
            post_hashes,
            resolved_ms,
        });
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
            .query_row(params![ctx.ev.session_id, first, ctx.ts], |r| r.get(0))
            .optional()?,
        None => None,
    };
    let ids_json = serde_json::to_string(edit_ids).unwrap_or_else(|_| "[]".into());
    ctx.tx
        .prepare_cached(
            "INSERT INTO dead_ends (session_id, epoch, path, edit_ids, mechanism, command_text, resolved_ms, observed_after_command_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![ctx.ev.session_id, ctx.epoch, path, ids_json, mechanism.as_str(), command, ctx.ts, observed])?;
    Ok(())
}

/// Records a version and runs reapplication, then hash-return revert
/// detection (§13.2, §13.4).
///
/// An observation that is *older* than something already recorded for the file
/// is stored but draws no conclusions. Spooled events keep their original hook
/// timestamp and get a fresh row id when they are finally ingested, so they can
/// arrive after events that happened later; such a row describes a state the
/// file has already left, and letting it decide a revert or a reapplication
/// produces exactly the wrong answer (D56).
///
/// Since the fold follows logical order (`crate::order`), a marked spooled
/// event is placed where it happened and never reaches this point late. The
/// guard stays for what the order cannot place: an event spooled by a build
/// that did not mark it, and a direct event whose clock ran backwards. It
/// compares the event's raw clock with the logical times already recorded,
/// so either one is stored and draws no conclusion.
fn record_version(
    ctx: &Ctx<'_>,
    path: &str,
    hash: &str,
    size: u64,
    source: VersionSource,
) -> Result<()> {
    record_version_by(ctx, path, hash, size, source, None)
}

/// [`record_version`] for a `git_post` observation, with the restore-family
/// subcommand that could have changed the file, if one could
/// (`shell::reaches`). Only then is a revert it shows credited to git, and
/// quoted with that subcommand rather than the whole line.
fn record_version_by(
    ctx: &Ctx<'_>,
    path: &str,
    hash: &str,
    size: u64,
    source: VersionSource,
    git_reach: Option<&str>,
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
            ctx.ts
        ])?;
    if history.last().is_some_and(|v| v.hash == hash) {
        return Ok(());
    }
    if history.iter().any(|v| v.ts_ms > ctx.ev.ts_ms) {
        return Ok(());
    }
    let observation = revert::Observation {
        hash,
        source,
        ts_ms: ctx.ts,
    };
    let reapplied = revert::reapplied(&observation, &open_dead_ends(ctx, path)?);
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
        let mechanism =
            revert::mechanism_for(source, ctx.ev.tool_name.as_deref(), git_reach.is_some());
        resolve_edits(ctx, &reverted, EditStatus::Reverted, Some(mechanism))?;
        let command = if mechanism == Mechanism::GitCommand {
            git_reach
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
            ctx.ts
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

/// The paths a failing command's output names, as the hook found them when
/// the command returned (`Payload::mentioned`), at most
/// [`commands::MENTION_LIMIT`] and never in an installed or generated
/// directory.
///
/// An event without the record -- from a build before it existed -- names
/// none. Checking its paths against the disk here would file whatever exists
/// when the reducer happens to run as what existed when the command ran: a
/// file created afterwards as named by the failure, one deleted afterwards as
/// not.
fn recorded_mentions(ctx: &Ctx<'_>) -> Vec<serde_json::Value> {
    ctx.ev
        .payload
        .mentioned
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|m| !m.path.is_empty() && !commands::is_third_party(&m.path))
        .take(commands::MENTION_LIMIT)
        .map(|m| serde_json::json!({ "path": m.path, "line": m.line, "raw": m.raw }))
        .collect()
}

/// The working directory a shell event's command started in: the one its
/// `PreToolUse` reported, when that event is in the ledger, else its own.
/// A `cd` inside the command may have moved the directory the post event
/// reports; pathspecs are relative to where the command began.
fn command_cwd(ctx: &Ctx<'_>) -> Result<Option<String>> {
    if let Some(id) = ctx.ev.tool_use_id.as_deref() {
        let pre: Option<String> = ctx
            .tx
            .prepare_cached(
                "SELECT payload FROM events WHERE session_id = ?1 AND tool_use_id = ?2 \
                 AND hook_event = 'PreToolUse' ORDER BY id LIMIT 1",
            )?
            .query_row(params![ctx.ev.session_id, id], |r| r.get(0))
            .optional()?;
        if let Some(cwd) = pre.and_then(|p| Payload::from_json(&p).cwd) {
            return Ok(Some(cwd));
        }
    }
    Ok(ctx.ev.payload.cwd.clone())
}

fn apply_shell(ctx: &Ctx<'_>, failure: bool) -> Result<()> {
    let p = &ctx.ev.payload;
    let Some(command) = p.command.as_deref().filter(|c| !c.trim().is_empty()) else {
        return Ok(());
    };
    let classified = commands::classify(command);
    let (exit_code, output) = commands::stored_output(p, failure);
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
        let m = match ctx.replay.mentions.get(&ctx.ev.id) {
            // Rebuilding: what the first reduction found, not the disk now.
            Some(kept) => kept
                .as_deref()
                .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(j).ok())
                .unwrap_or_default(),
            None if runner => recorded_mentions(ctx),
            None => Vec::new(),
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
            ctx.ts
        ])?;
    for m in &mentions {
        if let Some(path) = m["path"].as_str() {
            bump_stats(ctx, path, 0, 0, true)?;
        }
    }
    // Git effects are read from failed calls too. A chained
    // `git restore x && pytest` is reported as one failed tool call whenever
    // the suite still fails — which is the normal shape of discarding an
    // attempt — and skipping the observation loses the revert's attribution
    // entirely, leaving it to the turn-end scan to report as "changed outside
    // the agent". Every rule below compares hashes that were measured after
    // the call returned, so they hold whether or not the call succeeded (D57).
    let Some(git) = &p.git else { return Ok(()) };
    // Which files the line's restore-family calls could have changed. The
    // hook observes every file the session edited around the command, so a
    // file that changed across it may have been changed by another
    // subcommand; a change is credited to git only where a restore's own
    // pathspec reaches the file. Where the line does not say (`-p`, a path
    // built by a substitution, a `cd` into a variable), nothing is credited.
    let targets = if git.restore.is_some() {
        shell::restore_targets(
            command,
            shell::Dialect::for_tool(ctx.ev.tool_name.as_deref().unwrap_or("")),
        )
    } else {
        Vec::new()
    };
    let root = if targets.is_empty() {
        None
    } else {
        project_root(ctx.tx, &ctx.ev.project_id)?.map(|r| r.to_string_lossy().into_owned())
    };
    let cwd = if targets.is_empty() {
        None
    } else {
        command_cwd(ctx)?
    };
    for f in &git.files {
        let reached_by: Option<&str> = root.as_deref().and_then(|root| {
            targets
                .iter()
                .find(|t| shell::reaches(t, &f.path, cwd.as_deref(), root) == Some(true))
                .map(|t| t.command.as_str())
        });
        if let Some(restore) = reached_by {
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
        // A commit, unlike a restore, is not self-evident from the file's
        // content: an edit that was never committed hashes the same as one
        // that was. Only a call that reported success is taken as proof.
        if git.commit && !failure {
            let active = active_edits(ctx, &f.path)?;
            if let Some(pos) = active.iter().rposition(|e| e.post_hash == f.hash) {
                let ids: Vec<i64> = active[..=pos].iter().map(|e| e.id).collect();
                resolve_edits(ctx, &ids, EditStatus::Committed, None)?;
            }
        }
        record_version_by(
            ctx,
            &f.path,
            &f.hash,
            f.size,
            VersionSource::GitPost,
            reached_by,
        )?;
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
