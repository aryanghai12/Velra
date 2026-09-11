//! Immutable checkpoints (§15.2 step 5).

use crate::db::Result;
use crate::model::Trigger;
use crate::render::{self, RenderConfig, RENDER_VERSION};
use crate::snapshot::{self, SnapshotMeta};
use rusqlite::{params, Connection};

/// Parameters for creating a checkpoint.
#[derive(Debug, Clone)]
pub struct CheckpointRequest<'a> {
    pub session_id: &'a str,
    pub trigger: Trigger,
    pub created_ms: i64,
    pub partial: bool,
    /// Max reduced event id at capture time.
    pub watermark: i64,
}

/// What was frozen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInfo {
    pub checkpoint_id: String,
    pub summary: String,
    pub tokens: u32,
}

/// New checkpoint id: `ckpt_` + ULID.
pub fn new_id() -> String {
    format!(
        "ckpt_{}",
        ulid::Ulid::from_datetime(std::time::SystemTime::now())
    )
}

/// Inside an open write transaction: returns `None` when the epoch has
/// nothing worth saving; otherwise supersedes the live continuation, inserts
/// the checkpoint, a PENDING continuation and a `compactions` row.
pub fn create_in_tx(
    tx: &Connection,
    req: &CheckpointRequest<'_>,
    cfg: &RenderConfig,
) -> Result<Option<CheckpointInfo>> {
    if !snapshot::has_state(tx, req.session_id)? {
        return Ok(None);
    }
    let checkpoint_id = new_id();
    let meta = SnapshotMeta {
        checkpoint_id: checkpoint_id.clone(),
        created_ms: req.created_ms,
        trigger: req.trigger,
        partial: req.partial,
        preview: false,
        tz_offset_secs: crate::time::local_offset_secs(req.created_ms),
    };
    let snap = snapshot::build(tx, req.session_id, &meta)?;
    let rendered = render::render(&snap, cfg);
    let summary = render::summary(&snap);
    tx.prepare_cached(
        "UPDATE continuations SET state = 'SUPERSEDED', updated_ms = ?2 \
         WHERE session_id = ?1 AND state IN ('PENDING', 'ATTACHED')",
    )?
    .execute(params![req.session_id, req.created_ms])?;
    tx.prepare_cached(
        "INSERT INTO checkpoints (checkpoint_id, session_id, project_id, epoch, created_ms, \"trigger\", head_commit, \
         branch, event_watermark, partial, render_version, capsule, capsule_tokens_est, summary_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?
    .execute(params![
        checkpoint_id,
        req.session_id,
        snap.project_id,
        snap.epoch,
        req.created_ms,
        req.trigger.as_str(),
        snap.git.as_ref().and_then(|g| g.head.clone()),
        snap.git.as_ref().and_then(|g| g.branch.clone()),
        req.watermark,
        i64::from(req.partial),
        RENDER_VERSION,
        rendered.text,
        i64::from(rendered.tokens),
        render::summary_json(&snap, rendered.tokens)
    ])?;
    tx.prepare_cached(
        "INSERT INTO continuations (checkpoint_id, session_id, state, attach_count, updated_ms) VALUES (?1, ?2, 'PENDING', 0, ?3)",
    )?
    .execute(params![checkpoint_id, req.session_id, req.created_ms])?;
    tx.prepare_cached("INSERT INTO compactions (session_id, checkpoint_id, \"trigger\", pre_ms) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![req.session_id, checkpoint_id, req.trigger.as_str(), req.created_ms])?;
    Ok(Some(CheckpointInfo {
        checkpoint_id,
        summary,
        tokens: rendered.tokens,
    }))
}

/// Records the native compaction summary on the latest compaction row of the
/// session that has none yet (PostCompact).
pub fn record_native_summary(
    conn: &Connection,
    session_id: &str,
    ts_ms: i64,
    summary: &str,
) -> Result<bool> {
    let n = conn
        .prepare_cached(
            "UPDATE compactions SET post_ms = ?2, native_summary = ?3 WHERE id = \
             (SELECT id FROM compactions WHERE session_id = ?1 AND post_ms IS NULL ORDER BY id DESC LIMIT 1)",
        )?
        .execute(params![session_id, ts_ms, summary])?;
    Ok(n == 1)
}

/// A stored checkpoint.
#[derive(Debug, Clone)]
pub struct StoredCheckpoint {
    pub checkpoint_id: String,
    pub session_id: String,
    pub epoch: i64,
    pub created_ms: i64,
    pub trigger: String,
    pub partial: bool,
    pub capsule: String,
    pub tokens: u32,
    pub summary: String,
}

pub fn load(conn: &Connection, checkpoint_id: &str) -> rusqlite::Result<Option<StoredCheckpoint>> {
    use rusqlite::OptionalExtension;
    conn.prepare_cached(
        "SELECT checkpoint_id, session_id, epoch, created_ms, \"trigger\", partial, capsule, capsule_tokens_est, summary_json \
         FROM checkpoints WHERE checkpoint_id = ?1",
    )?
    .query_row([checkpoint_id], |r| {
        let summary_json: String = r.get(8)?;
        Ok(StoredCheckpoint {
            checkpoint_id: r.get(0)?,
            session_id: r.get(1)?,
            epoch: r.get(2)?,
            created_ms: r.get(3)?,
            trigger: r.get(4)?,
            partial: r.get::<_, i64>(5)? != 0,
            capsule: r.get(6)?,
            tokens: r.get::<_, i64>(7)? as u32,
            summary: summary_from_json(&summary_json),
        })
    })
    .optional()
}

pub fn summary_from_json(s: &str) -> String {
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .and_then(|v| v["summary"].as_str().map(str::to_string))
        .unwrap_or_default()
}
