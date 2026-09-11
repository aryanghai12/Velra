//! Continuation delivery state machine (§15.3–§15.5).
//!
//! PENDING → ATTACHED → CONFIRMED, terminal SUPERSEDED / EXPIRED. A partial
//! unique index guarantees at most one live (PENDING/ATTACHED) continuation
//! per session; conditional updates guarantee exactly one emitter.

use crate::checkpoint::summary_from_json;
use crate::db::{DbError, Result};
use crate::model::{hook_event as he, Channel, ContinuationState};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

/// Re-emission cap for aborted turns (T4).
pub const MAX_ATTACH: i64 = 3;
/// PENDING continuations older than this expire (T8).
pub const PENDING_TTL_MS: i64 = 7 * 24 * 3600 * 1000;

/// A capsule to emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub checkpoint_id: String,
    pub capsule: String,
    pub summary: String,
    pub tokens: u32,
    pub channel: Channel,
    /// True when re-emitting for an already-recorded delivery key (§15.5).
    pub replay: bool,
}

/// `blake3(hook_event|session_id|discriminator)[0..32]` where the
/// discriminator is the prompt id, tool use id, or `source`+`ts_ms`.
pub fn delivery_key(hook_event: &str, session_id: &str, discriminator: &str) -> String {
    crate::hash::hex_prefix(
        format!("{hook_event}|{session_id}|{discriminator}").as_bytes(),
        32,
    )
}

/// The live continuation of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub checkpoint_id: String,
    pub state: ContinuationState,
    pub attach_count: i64,
    pub attached_ms: Option<i64>,
    pub attached_channel: Option<String>,
    pub created_ms: i64,
}

/// Cheap read used by hooks to skip the write path entirely.
pub fn live(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<Live>> {
    conn.prepare_cached(
        "SELECT c.checkpoint_id, c.state, c.attach_count, c.attached_ms, c.attached_channel, k.created_ms \
         FROM continuations c JOIN checkpoints k ON k.checkpoint_id = c.checkpoint_id \
         WHERE c.session_id = ?1 AND c.state IN ('PENDING', 'ATTACHED')",
    )?
    .query_row([session_id], |r| {
        let state: String = r.get(1)?;
        Ok(Live {
            checkpoint_id: r.get(0)?,
            state: ContinuationState::parse(&state).unwrap_or(ContinuationState::Pending),
            attach_count: r.get(2)?,
            attached_ms: r.get(3)?,
            attached_channel: r.get(4)?,
            created_ms: r.get(5)?,
        })
    })
    .optional()
}

/// First confirmation evidence (T3) after `after_ms`: a PostToolUse,
/// PostToolUseFailure or Stop event of the session.
pub fn evidence_after(
    conn: &Connection,
    session_id: &str,
    after_ms: i64,
) -> rusqlite::Result<Option<(i64, i64)>> {
    conn.prepare_cached(
        "SELECT id, ts_ms FROM events WHERE session_id = ?1 AND ts_ms > ?2 \
         AND hook_event IN ('PostToolUse', 'PostToolUseFailure', 'Stop') ORDER BY ts_ms, id LIMIT 1",
    )?
    .query_row(params![session_id, after_ms], |r| Ok((r.get(0)?, r.get(1)?)))
    .optional()
}

fn confirm(
    conn: &Connection,
    checkpoint_id: &str,
    event_id: i64,
    ts_ms: i64,
) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "UPDATE continuations SET state = 'CONFIRMED', confirmed_ms = ?3, confirm_event_id = ?2, updated_ms = ?3 \
         WHERE checkpoint_id = ?1 AND state = 'ATTACHED'",
    )?
    .execute(params![checkpoint_id, event_id, ts_ms])
}

/// Reconcile (T3, T7, T8). Reads first and only takes the write lock when a
/// transition is due. Returns the state after reconciliation.
pub fn reconcile(
    conn: &mut Connection,
    session_id: &str,
    now_ms: i64,
) -> Result<Option<ContinuationState>> {
    let Some(l) = live(conn, session_id)? else {
        return Ok(None);
    };
    let due = match l.state {
        ContinuationState::Pending => now_ms - l.created_ms > PENDING_TTL_MS,
        ContinuationState::Attached => {
            evidence_after(conn, session_id, l.attached_ms.unwrap_or(i64::MAX))?.is_some()
        }
        _ => false,
    };
    if !due {
        return Ok(Some(l.state));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state = match l.state {
        ContinuationState::Pending => {
            tx.execute(
                "UPDATE continuations SET state = 'EXPIRED', updated_ms = ?2 WHERE checkpoint_id = ?1 AND state = 'PENDING'",
                params![l.checkpoint_id, now_ms],
            )?;
            ContinuationState::Expired
        }
        _ => match evidence_after(&tx, session_id, l.attached_ms.unwrap_or(i64::MAX))? {
            Some((eid, ts)) => {
                confirm(&tx, &l.checkpoint_id, eid, ts)?;
                ContinuationState::Confirmed
            }
            None => l.state,
        },
    };
    tx.commit()?;
    Ok(Some(state))
}

/// T6: expire the live continuation (`/clear`, logout).
pub fn expire_live(conn: &Connection, session_id: &str, now_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE continuations SET state = 'EXPIRED', updated_ms = ?2 WHERE session_id = ?1 AND state IN ('PENDING', 'ATTACHED')",
        params![session_id, now_ms],
    )?)
}

/// A delivery opportunity.
#[derive(Debug, Clone)]
pub struct DeliveryRequest<'a> {
    pub session_id: &'a str,
    pub channel: Channel,
    pub delivery_key: &'a str,
    pub ts_ms: i64,
}

fn load_capsule(conn: &Connection, checkpoint_id: &str) -> rusqlite::Result<(String, String, u32)> {
    conn.prepare_cached("SELECT capsule, summary_json, capsule_tokens_est FROM checkpoints WHERE checkpoint_id = ?1")?
        .query_row([checkpoint_id], |r| {
            let summary: String = r.get(1)?;
            Ok((r.get(0)?, summary_from_json(&summary), r.get::<_, i64>(2)? as u32))
        })
}

fn insert_injection(
    conn: &Connection,
    checkpoint_id: &str,
    n: i64,
    req: &DeliveryRequest<'_>,
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO injections (injection_id, checkpoint_id, delivery_key, channel, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![format!("{checkpoint_id}:inj:{n}"), checkpoint_id, req.delivery_key, req.channel.as_str(), req.ts_ms])?;
    Ok(())
}

/// Attempts delivery (T2, T4, T5, §15.5). `emit` runs inside the write
/// transaction *before* commit and returns whether the output was written;
/// if it returns false the transaction rolls back and the continuation stays
/// deliverable. Returns what was emitted.
pub fn deliver(
    conn: &mut Connection,
    req: &DeliveryRequest<'_>,
    emit: impl FnOnce(&Delivery) -> bool,
) -> Result<Option<Delivery>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DbError::from)?;

    // §15.5: a known delivery key re-emits the same capsule, no new injection.
    let replay: Option<(String, String)> = tx
        .prepare_cached(
            "SELECT i.checkpoint_id, i.channel FROM injections i WHERE i.delivery_key = ?1",
        )?
        .query_row([req.delivery_key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    if let Some((checkpoint_id, channel)) = replay {
        let (capsule, summary, tokens) = load_capsule(&tx, &checkpoint_id)?;
        let d = Delivery {
            checkpoint_id,
            capsule,
            summary,
            tokens,
            channel: Channel::parse(&channel).unwrap_or(req.channel),
            replay: true,
        };
        let emitted = emit(&d);
        tx.commit()?;
        return Ok(emitted.then_some(d));
    }

    let Some(l) = live(&tx, req.session_id)? else {
        return Ok(None);
    };
    let n = match l.state {
        ContinuationState::Pending => {
            if req.channel == Channel::PostTool && req.ts_ms <= l.created_ms {
                return Ok(None);
            }
            let changed = tx
                .prepare_cached(
                    "UPDATE continuations SET state = 'ATTACHED', attach_count = attach_count + 1, attached_ms = ?2, \
                     attached_channel = ?3, updated_ms = ?2 WHERE checkpoint_id = ?1 AND state = 'PENDING'",
                )?
                .execute(params![l.checkpoint_id, req.ts_ms, req.channel.as_str()])?;
            if changed != 1 {
                return Ok(None);
            }
            l.attach_count + 1
        }
        ContinuationState::Attached => {
            // T5: context injected at session start / after a tool call stays in
            // the conversation; only prompt-channel deliveries are re-emitted.
            if req.channel != Channel::UserPrompt
                || l.attached_channel.as_deref() != Some(Channel::UserPrompt.as_str())
            {
                return Ok(None);
            }
            let attached_ms = l.attached_ms.unwrap_or(i64::MAX);
            if let Some((eid, ts)) = evidence_after(&tx, req.session_id, attached_ms)? {
                confirm(&tx, &l.checkpoint_id, eid, ts)?; // T7
                tx.commit()?;
                return Ok(None);
            }
            let next = l.attach_count + 1;
            if next > MAX_ATTACH {
                tx.execute(
                    "UPDATE continuations SET state = 'EXPIRED', updated_ms = ?2 WHERE checkpoint_id = ?1 AND state = 'ATTACHED'",
                    params![l.checkpoint_id, req.ts_ms],
                )?;
                tx.commit()?;
                return Ok(None);
            }
            tx.prepare_cached(
                "UPDATE continuations SET attach_count = ?2, attached_ms = ?3, updated_ms = ?3 \
                 WHERE checkpoint_id = ?1 AND state = 'ATTACHED'",
            )?
            .execute(params![l.checkpoint_id, next, req.ts_ms])?;
            next
        }
        _ => return Ok(None),
    };
    insert_injection(&tx, &l.checkpoint_id, n, req)?;
    let (capsule, summary, tokens) = load_capsule(&tx, &l.checkpoint_id)?;
    let d = Delivery {
        checkpoint_id: l.checkpoint_id,
        capsule,
        summary,
        tokens,
        channel: req.channel,
        replay: false,
    };
    if !emit(&d) {
        return Ok(None); // rollback: stays deliverable
    }
    tx.commit()?;
    Ok(Some(d))
}

/// Hook event name used in `hookSpecificOutput.hookEventName` per channel.
pub fn hook_event_name(channel: Channel) -> &'static str {
    match channel {
        Channel::SessionStart => he::SESSION_START,
        Channel::PostTool => he::POST_TOOL_USE,
        Channel::UserPrompt => he::USER_PROMPT_SUBMIT,
    }
}

/// `⚡ Velra restored: {summary} ({n} tokens)`.
pub fn restored_message(d: &Delivery) -> String {
    if d.summary.is_empty() {
        format!("\u{26a1} Velra restored: task state ({} tokens)", d.tokens)
    } else {
        format!(
            "\u{26a1} Velra restored: {} ({} tokens)",
            d.summary, d.tokens
        )
    }
}

/// Exact delivery JSON (§8.4), one line, no trailing newline.
pub fn delivery_json(d: &Delivery) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": hook_event_name(d.channel),
            "additionalContext": d.capsule,
        },
        "systemMessage": restored_message(d),
    })
    .to_string()
}
