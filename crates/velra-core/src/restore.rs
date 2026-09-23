//! Explicit cross-session restore: the one path by which state captured in
//! one Claude Code session may be carried into another.
//!
//! # The invariant this module is built around
//!
//! Velra's automatic delivery is, and remains, strictly session-scoped:
//! `continuation::deliver` will not hand session A's checkpoint to session B,
//! and `continuations_never_cross_sessions_without_an_explicit_restore` pins
//! both halves of that. Restore does not weaken
//! it. It adds a second, narrower path with a much stronger precondition —
//! *a person named the source session* — and that path stops at staging. What
//! gets staged is only ever the state of the one session that was named.
//!
//! So the durable ownership boundary is the workspace, the source session
//! stays a first-class identity that is recorded in the artifact, and the
//! destination session inherits nothing but the text of the capsule.
//!
//! # What is restored
//!
//! The operational state the ledger already holds, rendered by the existing
//! renderer under the existing token budget: the objective, stated
//! constraints, the open failure, reverted dead ends, recent edits, the
//! working set, the inferred next target. Not the conversation. No model is
//! consulted at any point — this is a query over `events` and its projections.

use crate::db::Result;
use crate::model::Trigger;
use crate::render::{self, RenderConfig};
use crate::snapshot::{self, SnapshotMeta};
use crate::staging::{self, StagedCapsule, STAGED_VERSION};
use rusqlite::{Connection, OptionalExtension};

/// A session Velra holds ledger state for, within one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerSession {
    pub session_id: String,
    pub project_id: String,
    pub started_ms: i64,
    pub last_event_ms: i64,
    pub transcript_path: Option<String>,
    /// Frozen checkpoints recorded for this session.
    pub checkpoints: i64,
}

/// Every session the ledger has recorded for one workspace, newest first.
///
/// Scoped by `project_id` — the workspace — and never by anything else, which
/// is what makes the workspace the ownership boundary. Sessions stay separate
/// rows: this is a list of sessions, not a merged state stream.
pub fn sessions_for_workspace(
    conn: &Connection,
    project_id: &str,
) -> rusqlite::Result<Vec<LedgerSession>> {
    conn.prepare(
        "SELECT s.session_id, s.project_id, s.started_ms, s.last_event_ms, s.transcript_path, \
         (SELECT COUNT(*) FROM checkpoints k WHERE k.session_id = s.session_id) \
         FROM sessions s WHERE s.project_id = ?1 \
         ORDER BY s.last_event_ms DESC, s.session_id",
    )?
    .query_map([project_id], |r| {
        Ok(LedgerSession {
            session_id: r.get(0)?,
            project_id: r.get(1)?,
            started_ms: r.get(2)?,
            last_event_ms: r.get(3)?,
            transcript_path: r.get(4)?,
            checkpoints: r.get(5)?,
        })
    })?
    .collect()
}

/// One session of a workspace, or `None` when that session is not part of
/// this workspace.
///
/// The `project_id` predicate is not decoration. Without it, a session id
/// typed by hand — or carried over from another checkout — would resolve
/// against whatever the ledger happens to hold, and restore would read one
/// workspace's state into another.
pub fn session_in_workspace(
    conn: &Connection,
    project_id: &str,
    session_id: &str,
) -> rusqlite::Result<Option<LedgerSession>> {
    conn.prepare(
        "SELECT s.session_id, s.project_id, s.started_ms, s.last_event_ms, s.transcript_path, \
         (SELECT COUNT(*) FROM checkpoints k WHERE k.session_id = s.session_id) \
         FROM sessions s WHERE s.project_id = ?1 AND s.session_id = ?2",
    )?
    .query_row(rusqlite::params![project_id, session_id], |r| {
        Ok(LedgerSession {
            session_id: r.get(0)?,
            project_id: r.get(1)?,
            started_ms: r.get(2)?,
            last_event_ms: r.get(3)?,
            transcript_path: r.get(4)?,
            checkpoints: r.get(5)?,
        })
    })
    .optional()
}

/// Why a restore could not produce a capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreError {
    /// No such session in this workspace — unknown, or owned by another one.
    UnknownSession {
        session_id: String,
    },
    /// The session exists but the ledger holds nothing worth carrying over.
    NoState {
        session_id: String,
    },
    Db(String),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::UnknownSession { session_id } => {
                write!(f, "no session {session_id} recorded for this workspace")
            }
            RestoreError::NoState { session_id } => {
                write!(f, "session {session_id} has no task state to restore")
            }
            RestoreError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RestoreError {}

/// What to build, and for whom.
#[derive(Debug, Clone)]
pub struct RestoreRequest<'a> {
    /// Workspace doing the restoring. Both the ownership check and the
    /// staging key.
    pub workspace_id: &'a str,
    pub workspace_root: &'a str,
    /// The session the user chose.
    pub source_session_id: &'a str,
    pub now_ms: i64,
}

/// Builds the staged capsule for an explicitly chosen source session.
///
/// Pure with respect to the filesystem: it reads the ledger and returns a
/// record. Writing that record is [`crate::staging::stage`]'s job, which keeps
/// the decision of *what* to restore separate from the concurrency rules about
/// *where* it lands.
///
/// Determinism: for a fixed ledger and a fixed `now_ms`, the returned capsule
/// text — and therefore its content hash — is byte-identical, because
/// `snapshot::build` and `render::render` are both pure functions of the rows
/// and the config.
pub fn build(
    conn: &Connection,
    req: &RestoreRequest<'_>,
    cfg: &RenderConfig,
) -> std::result::Result<StagedCapsule, RestoreError> {
    let session = session_in_workspace(conn, req.workspace_id, req.source_session_id)
        .map_err(|e| RestoreError::Db(e.to_string()))?
        .ok_or_else(|| RestoreError::UnknownSession {
            session_id: req.source_session_id.to_string(),
        })?;

    let has_state = snapshot::has_state(conn, &session.session_id)
        .map_err(|e| RestoreError::Db(e.to_string()))?;
    if !has_state {
        return Err(RestoreError::NoState {
            session_id: session.session_id.clone(),
        });
    }

    let source_checkpoint_id = source_checkpoint(conn, &session.session_id)
        .map_err(|e| RestoreError::Db(e.to_string()))?;
    let meta = snapshot_meta(
        source_checkpoint_id.as_deref(),
        req.now_ms,
        crate::time::local_offset_secs(req.now_ms),
    );
    let snap = snapshot::build(conn, &session.session_id, &meta)
        .map_err(|e| RestoreError::Db(e.to_string()))?;
    let rendered = render::render(&snap, cfg);
    let summary = render::summary(&snap);

    // Defence in depth. Everything in the capsule came through the hook path,
    // which redacts at ingest, so this pass is expected to be a no-op — but
    // the staged file outlives the session that produced it and is the one
    // artifact a future consumer injects verbatim, so it is not the place to
    // rely on an upstream guarantee alone.
    let capsule = crate::redact::redact(&rendered.text).into_owned();
    // Re-estimating after redaction: a replacement token is never longer than
    // what it replaces, so this can only go down, but the recorded figure
    // should describe the bytes actually staged.
    let tokens = crate::text::estimate_tokens(&capsule).min(rendered.tokens);

    Ok(StagedCapsule {
        version: STAGED_VERSION,
        // `velra restore` stages for a session that does not exist yet, so the
        // capsule is eligible on `startup` and on nothing else. See
        // `staging::NEW_SESSION` for why the other three sources are excluded.
        intent: staging::NEW_SESSION.name.to_string(),
        deliver_on: staging::NEW_SESSION.deliver_on(),
        workspace_id: req.workspace_id.to_string(),
        workspace_root: req.workspace_root.to_string(),
        source_session_id: session.session_id,
        source_checkpoint_id,
        created_ms: req.now_ms,
        render_version: render::RENDER_VERSION,
        tokens,
        content_hash: StagedCapsule::compute_hash(&capsule),
        summary,
        capsule,
    })
}

/// The most recent frozen checkpoint of a session, recorded in a restore for
/// provenance. The capsule itself is rendered fresh from the ledger so that
/// work done after the last compaction is not silently dropped.
pub fn source_checkpoint(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare(
        "SELECT checkpoint_id FROM checkpoints WHERE session_id = ?1 \
             ORDER BY created_ms DESC, rowid DESC LIMIT 1",
    )?
    .query_row([session_id], |r| r.get::<_, String>(0))
    .optional()
}

/// The snapshot framing `velra restore` renders with.
///
/// Public because it is not incidental: the checkpoint id is printed in the
/// capsule's opening tag, so a restore and a preview of the same ledger spend
/// different budgets on it and can keep different lines. Anything that claims
/// to show "what restore stages" -- `velra inspect --trace` -- must render with
/// exactly this.
pub fn snapshot_meta(
    source_checkpoint_id: Option<&str>,
    now_ms: i64,
    tz_offset_secs: i32,
) -> SnapshotMeta {
    SnapshotMeta {
        checkpoint_id: source_checkpoint_id.unwrap_or("restore").to_string(),
        created_ms: now_ms,
        trigger: Trigger::Cli,
        partial: false,
        preview: false,
        tz_offset_secs,
    }
}

/// Whether the ledger holds anything restorable for a session.
pub fn has_restorable_state(conn: &Connection, session_id: &str) -> Result<bool> {
    Ok(snapshot::has_state(conn, session_id)?)
}
