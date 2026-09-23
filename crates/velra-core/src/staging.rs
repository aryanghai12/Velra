//! The staged capsule: state a user explicitly chose to carry out of one
//! Claude Code session and into the next one.
//!
//! # Where this sits in the model
//!
//! ```text
//! WORKSPACE                      <- durable ownership boundary
//! ├── SESSION A -> checkpoints   <- source session, a first-class identity
//! ├── SESSION B -> checkpoints
//! └── staged capsule             <- exactly one, chosen explicitly by the user
//! ```
//!
//! Automatic delivery (`continuation.rs`) never crosses a session boundary and
//! that does not change. This module is the *only* path by which one session's
//! state can reach another, and it opens solely because a person selected a
//! source session by hand.
//!
//! # Location
//!
//! `$VELRA_HOME/staged/<workspace_id>/staged_capsule`.
//!
//! The obvious alternative is `<workspace>/.velra/staged_capsule`, inside the
//! repository. It is rejected because the requirements on this artifact are
//! "never committed, never in a fixture, never part of repository state", and
//! a path inside the working tree can only approximate those through a
//! `.gitignore` that the artifact does not control. Under `$VELRA_HOME` they
//! hold by construction: the directory is created 0700, it is outside every
//! repository, and no `git add -A` can reach it. The workspace is still the
//! ownership boundary — it is the directory key.
//!
//! # Consumption
//!
//! [`claim`] is one-shot and safe against duplicate consumers, because the
//! claim is the exclusive *creation* of a marker file — the one filesystem
//! operation both platforms agree is atomic. A plain read-then-delete would
//! hand the capsule to every concurrent consumer; a rename-aside, which is the
//! other obvious answer, is wrong on Windows for the reason documented on
//! [`acquire`].
//!
//! `velra restore` stages; the `SessionStart` hook (`hook.rs`,
//! `deliver_staged`) claims through [`claim_with`] on every session start and
//! emits the capsule only when the source is in the record's `deliver_on`.

use crate::hash;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Format version of the staged record, so a future reader can refuse a shape
/// it does not understand instead of misreading it.
///
/// v2 added `intent` and `deliver_on`. A v1 record carried neither, so there
/// is no honest way to guess which `SessionStart` sources it was meant for;
/// it is refused as [`ClaimError::Malformed`] and discarded, and the user
/// re-runs `velra restore`. v1 was never released.
pub const STAGED_VERSION: u32 = 2;

/// How long a staged capsule stays claimable.
///
/// A capsule describes a workspace at a moment. Two weeks later the branch has
/// moved, the failing test has been fixed or forgotten, and injecting it would
/// be worse than injecting nothing. Seven days matches
/// `continuation::PENDING_TTL_MS`, which is the same judgement about the same
/// kind of staleness.
pub const STAGED_TTL_MS: i64 = 7 * 24 * 3600 * 1000;

/// A claim left behind by a consumer that died mid-consume is swept after
/// this long. Generous, because the alternative to waiting is deleting state
/// out from under a process that is merely slow.
pub const CLAIM_SWEEP_MS: i64 = 24 * 3600 * 1000;

/// The staged artifact.
///
/// Field order is the serialized order, and every field is scalar, so two
/// stages of the same ledger at the same clock produce byte-identical files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedCapsule {
    pub version: u32,
    /// Workspace that owns this capsule.
    pub workspace_id: String,
    /// Workspace root as recorded, for diagnostics and for the mismatch check.
    pub workspace_root: String,
    /// Why this was staged, e.g. `new_session`. Provenance only — the field
    /// below is what actually gates delivery.
    pub intent: String,
    /// `SessionStart` sources this capsule may be consumed on.
    ///
    /// Carried as plain strings rather than an enum on purpose: a Velra that
    /// has never heard of a future source name still parses the record and
    /// simply never matches it, which is the safe direction to fail in.
    pub deliver_on: Vec<String>,
    /// The session the state came from. Never the session it will go to.
    pub source_session_id: String,
    /// Frozen checkpoint the capsule was rendered from, when there was one.
    pub source_checkpoint_id: Option<String>,
    pub created_ms: i64,
    pub render_version: i64,
    pub tokens: u32,
    /// `blake3(capsule)[0..32]`, so a reader can detect a mangled file.
    pub content_hash: String,
    /// One-line summary, for the message a consumer prints.
    pub summary: String,
    /// The bounded, redacted capsule text.
    pub capsule: String,
}

impl StagedCapsule {
    /// Recomputes the content hash from the capsule text.
    pub fn compute_hash(capsule: &str) -> String {
        hash::hex_prefix(capsule.as_bytes(), 32)
    }

    /// Whether `content_hash` still matches `capsule`.
    pub fn hash_matches(&self) -> bool {
        self.content_hash == Self::compute_hash(&self.capsule)
    }

    /// Whether this capsule may be delivered on a `SessionStart` of `source`.
    ///
    /// Set membership against the record's own `deliver_on`. No source name
    /// appears in this function, which is what lets a new workflow ship
    /// without touching the staging model.
    pub fn accepts(&self, source: &str) -> bool {
        self.deliver_on.iter().any(|s| s == source)
    }

    /// Deterministic serialized form.
    pub fn to_json(&self) -> String {
        // Pretty-printed because a human debugging a restore will read this
        // file, and a struct serializes its fields in declaration order, so
        // the output is stable without needing a map ordering guarantee.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// `$VELRA_HOME/staged`.
pub fn staged_root(velra_home: &Path) -> PathBuf {
    velra_home.join("staged")
}

/// `$VELRA_HOME/staged/<workspace_id>`.
pub fn staged_dir(velra_home: &Path, workspace_id: &str) -> PathBuf {
    staged_root(velra_home).join(workspace_id)
}

/// The staged capsule of a workspace.
pub fn staged_path(velra_home: &Path, workspace_id: &str) -> PathBuf {
    staged_dir(velra_home, workspace_id).join("staged_capsule")
}

fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
        {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn unique_suffix(now_ms: i64) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now_ms);
    h.write_u32(std::process::id());
    format!(
        "{now_ms}-{}-{:04x}",
        std::process::id(),
        h.finish() & 0xffff
    )
}

/// Writes `capsule` to `path` atomically: a temp file in the same directory,
/// fsynced, then renamed over the target.
///
/// The rename is what makes an interrupted stage harmless. A reader either
/// sees the previous capsule or the new one, never a half-written file, and a
/// process killed before the rename leaves only a temp file behind.
pub fn stage(path: &Path, capsule: &StagedCapsule) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("staged capsule path has no parent"))?;
    create_dir_private(dir)?;
    let tmp = dir.join(format!(
        ".staged_capsule.velra-tmp-{}",
        unique_suffix(capsule.created_ms)
    ));
    let body = capsule.to_json();
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Reads a staged capsule without consuming it. `None` for anything
/// unreadable, unparseable or of an unknown version.
pub fn peek(path: &Path) -> Option<StagedCapsule> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: StagedCapsule = serde_json::from_str(&text).ok()?;
    (parsed.version == STAGED_VERSION).then_some(parsed)
}

/// Why a claim did not produce a capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// Nothing staged.
    Empty,
    /// Another claimant holds the slot right now.
    Busy,
    /// Unreadable or not a staged capsule of a version we know.
    Malformed,
    /// `content_hash` does not match the capsule text.
    Corrupt,
    /// Older than [`STAGED_TTL_MS`].
    Stale { age_ms: i64 },
    /// Staged for a different workspace than the one claiming it.
    WrongWorkspace { staged_for: String },
    /// Staged for a different `SessionStart` source. Never consumes: the
    /// capsule stays where it is, waiting for a source it accepts.
    NotForThisSource {
        source: String,
        deliver_on: Vec<String>,
    },
    /// The consumer declined to emit, so nothing was delivered and the
    /// capsule stays staged.
    NotEmitted,
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Empty => write!(f, "nothing staged"),
            ClaimError::Busy => write!(f, "another process is claiming the staged capsule"),
            ClaimError::Malformed => write!(f, "staged capsule is unreadable"),
            ClaimError::Corrupt => write!(f, "staged capsule failed its content hash"),
            ClaimError::Stale { age_ms } => {
                write!(
                    f,
                    "staged capsule is {} days old",
                    age_ms / (24 * 3600 * 1000)
                )
            }
            ClaimError::WrongWorkspace { staged_for } => {
                write!(f, "staged capsule belongs to workspace {staged_for}")
            }
            ClaimError::NotForThisSource { source, deliver_on } => write!(
                f,
                "staged capsule is for SessionStart({}), not ({source})",
                deliver_on.join("|")
            ),
            ClaimError::NotEmitted => write!(f, "staged capsule was not emitted"),
        }
    }
}

/// Longest a claim marker is honoured before another claimant may take it.
///
/// A marker only exists for the few microseconds between acquiring it and
/// finishing with the capsule, so any marker older than this belongs to a
/// process that died holding it. Long enough that a merely slow claimant is
/// never robbed, short enough that a crash does not wedge the slot until the
/// daily sweep.
pub const CLAIM_LEASE_MS: i64 = 60_000;

/// Name of the exclusive claim marker.
const CLAIM_MARKER: &str = "staged_capsule.claim";

/// Acquires the exclusive right to consume this workspace's staged capsule.
///
/// `create_new` is the whole mechanism: the filesystem decides, atomically,
/// which single caller creates the marker, and everyone else sees
/// `AlreadyExists`. It is the same primitive `spool::write` relies on.
///
/// # Why this is not a rename
///
/// The obvious implementation is to rename the capsule aside and treat a
/// successful rename as the claim. On POSIX that works. On Windows it does
/// not, and the failure is silent: `MoveFileEx` resolves the source to a
/// *handle* and then renames whatever that handle points at, so a second
/// claimant that opened the source before the first one moved it will happily
/// rename the already-moved file to its own target and report success. Both
/// callers then hold a valid capsule and both inject it.
///
/// That is not a theoretical concern. `two_parallel_claims_produce_exactly_one_winner`
/// caught it on the first full run of this suite, with four winners out of
/// eight threads.
fn acquire(marker: &Path, now_ms: i64) -> Result<(), ClaimError> {
    fn create_exclusive(marker: &Path, now_ms: i64) -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(marker)?;
        // Contents are for a human reading a wedged directory; the claim is
        // the existence of the file, not anything inside it.
        let _ = writeln!(f, "pid={} at={now_ms}", std::process::id());
        f.sync_all()
    }

    match create_exclusive(marker, now_ms) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(ClaimError::Empty),
        Err(_) => {}
    }

    // Someone holds it. If their lease has expired they died holding it, so
    // break the lease and retry exactly once — `create_new` still decides
    // which of several breakers actually gets it.
    let expired = std::fs::metadata(marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX) > CLAIM_LEASE_MS)
        .unwrap_or(false);
    if !expired {
        return Err(ClaimError::Busy);
    }
    let _ = std::fs::remove_file(marker);
    create_exclusive(marker, now_ms).map_err(|_| ClaimError::Busy)
}

/// Claims the staged capsule of a workspace exactly once, for one
/// `SessionStart` source.
///
/// `emit` runs while this process holds the claim and **before** the capsule
/// is deleted; it returns whether the capsule actually reached the consumer.
/// A `false` return leaves the capsule staged, exactly as
/// `continuation::deliver` leaves a continuation deliverable when its write
/// fails. Without that ordering a capsule would be consumed by a delivery that
/// never happened — the hook's `emit` refuses a second JSON object per
/// process, so this is a real case, not a hypothetical one.
///
/// Outcomes, and what each does to the file:
///
/// | outcome | capsule |
/// |---|---|
/// | delivered | deleted — it has been used |
/// | [`ClaimError::NotForThisSource`] | **kept** — it is waiting for its source |
/// | [`ClaimError::NotEmitted`] | **kept** — delivery did not happen |
/// | [`ClaimError::Stale`] | deleted — nothing will ever accept it |
/// | [`ClaimError::Malformed`] | deleted — it can never become readable |
/// | [`ClaimError::Corrupt`], [`ClaimError::WrongWorkspace`] | kept as evidence of a bug |
pub fn claim_with(
    velra_home: &Path,
    workspace_id: &str,
    source: &str,
    now_ms: i64,
    emit: impl FnOnce(&StagedCapsule) -> bool,
) -> Result<StagedCapsule, ClaimError> {
    let dir = staged_dir(velra_home, workspace_id);
    let path = staged_path(velra_home, workspace_id);
    if !path.is_file() {
        return Err(ClaimError::Empty);
    }

    // Source check before the marker, not after. Every `/clear`, every
    // compaction and every resume of every session reaches this line, and none
    // of them should contend for a claim they are not eligible to win.
    if let Some(peeked) = peek(&path) {
        if !peeked.accepts(source) {
            return Err(ClaimError::NotForThisSource {
                source: source.to_string(),
                deliver_on: peeked.deliver_on,
            });
        }
    }

    let marker = dir.join(CLAIM_MARKER);
    acquire(&marker, now_ms)?;

    let outcome = (|| {
        // Re-read under the claim: everything above was advisory, because
        // another process could have restaged in between.
        if !path.is_file() {
            return Err(ClaimError::Empty);
        }
        let Some(capsule) = peek(&path) else {
            return Err(ClaimError::Malformed);
        };
        if !capsule.hash_matches() {
            return Err(ClaimError::Corrupt);
        }
        if capsule.workspace_id != workspace_id {
            return Err(ClaimError::WrongWorkspace {
                staged_for: capsule.workspace_id,
            });
        }
        if !capsule.accepts(source) {
            return Err(ClaimError::NotForThisSource {
                source: source.to_string(),
                deliver_on: capsule.deliver_on,
            });
        }
        let age_ms = now_ms.saturating_sub(capsule.created_ms);
        if age_ms > STAGED_TTL_MS {
            return Err(ClaimError::Stale { age_ms });
        }
        if !emit(&capsule) {
            return Err(ClaimError::NotEmitted);
        }
        Ok(capsule)
    })();

    match &outcome {
        // Used, or unusable by anyone ever: the slot is freed.
        Ok(_) | Err(ClaimError::Stale { .. }) | Err(ClaimError::Malformed) => {
            let _ = std::fs::remove_file(&path);
        }
        // Kept: still waiting for its source, not delivered, or evidence.
        _ => {}
    }
    let _ = std::fs::remove_file(&marker);
    outcome
}

/// [`claim_with`] for a caller that has nothing to emit — it takes the
/// capsule and is itself responsible for what happens next.
pub fn claim(
    velra_home: &Path,
    workspace_id: &str,
    source: &str,
    now_ms: i64,
) -> Result<StagedCapsule, ClaimError> {
    claim_with(velra_home, workspace_id, source, now_ms, |_| true)
}

/// Removes abandoned temp files and long-dead claims from a workspace's
/// staging directory, and discards a staged capsule that has gone stale.
///
/// Returns how many files were removed. Best effort throughout: a file that
/// cannot be removed is left alone rather than turned into an error.
pub fn sweep(velra_home: &Path, workspace_id: &str, now_ms: i64) -> usize {
    let dir = staged_dir(velra_home, workspace_id);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let age = |default_stale: bool| {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                .map(|ms| ms > CLAIM_SWEEP_MS)
                .unwrap_or(default_stale)
        };
        let doomed = if name.starts_with(".staged_capsule.velra-tmp-")
            || name.starts_with("staged_capsule.claimed-")
            || name == CLAIM_MARKER
        {
            age(false)
        } else if name == "staged_capsule" {
            peek(&path)
                .map(|c| now_ms.saturating_sub(c.created_ms) > STAGED_TTL_MS)
                // A file under this name that does not parse is not a capsule
                // anyone can use; removing it is how the next stage succeeds.
                .unwrap_or(true)
        } else {
            false
        };
        if doomed && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Why a capsule was staged, and — as a consequence — the `SessionStart`
/// sources it may be delivered on.
///
/// # This is data, not a match arm
///
/// The allowed sources travel *inside* the staged record as
/// [`StagedCapsule::deliver_on`], and [`claim_with`] decides by set membership
/// against that field. Nothing in this module names a particular source.
///
/// That is the whole point. A later workflow — handing state across an
/// explicit `/clear`, or across a `resume` — becomes a new constant here and
/// a different value in one field. The staging model, the claim protocol, the
/// atomicity rules and the consumer all stay exactly as they are. Adding
/// `CLEAR_HANDOFF` below would not change a single line of [`claim_with`].
///
/// `deliver_on_is_data_not_code` is the test that holds this open: it stages a
/// capsule with a source no shipped intent uses and watches it deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingIntent {
    /// Recorded verbatim in the artifact, for diagnostics and provenance.
    pub name: &'static str,
    /// `SessionStart` sources this capsule may be consumed on.
    pub deliver_on: &'static [&'static str],
}

impl StagingIntent {
    pub fn deliver_on(&self) -> Vec<String> {
        self.deliver_on.iter().map(|s| (*s).to_string()).collect()
    }
}

/// `velra restore`: state carried into a **brand-new** session.
///
/// `startup` only, and deliberately nothing else. The other three sources are
/// all continuations of a conversation that is still going:
///
/// * `compact` and `resume` already belong to the in-session continuation path
///   (`continuation.rs`), which has its own exactly-once accounting; delivering
///   a staged capsule there would inject a second, older record alongside it.
/// * `clear` is the user emptying the context of the session they are *in*.
///   Consuming a restore capsule there would spend state the user staged for
///   their next session on the one they are still sitting in, which is the
///   opposite of what they asked for.
///
/// So a restore capsule waits. It is not consumed, not discarded and not
/// counted against its retry budget by any of those three — it simply stays
/// staged until a new session starts.
pub const NEW_SESSION: StagingIntent = StagingIntent {
    name: "new_session",
    deliver_on: &[source::STARTUP],
};

/// `SessionStart` source names, as Claude Code reports them.
///
/// Verified against Claude Code 2.1.272 on the development machine: the
/// matcher-less `SessionStart` registration Velra has always used has
/// observed `startup`, `resume`, `compact` and `clear` in the event log.
/// `fork` is documented for the same version and is listed here so that it is
/// a *named* refusal rather than an unrecognised string that happens not to
/// match.
///
/// Note that nothing in this module consults these constants when deciding
/// whether to deliver — [`StagedCapsule::accepts`] compares against the
/// capsule's own `deliver_on`. They exist so callers and tests can name
/// sources without spelling them, and so an unknown future source is refused
/// by default rather than by omission.
pub mod source {
    pub const STARTUP: &str = "startup";
    pub const RESUME: &str = "resume";
    pub const COMPACT: &str = "compact";
    pub const CLEAR: &str = "clear";
    pub const FORK: &str = "fork";

    /// Every source this build knows about.
    pub const ALL: &[&str] = &[STARTUP, RESUME, COMPACT, CLEAR, FORK];
}

#[cfg(test)]
mod tests {
    use super::source::STARTUP;
    use super::*;

    const NOW: i64 = 1_789_207_445_000;
    const WS: &str = "0123456789abcdef";

    fn sample(workspace_id: &str, created_ms: i64) -> StagedCapsule {
        let capsule =
            "<VELRA_WORKSPACE_STATE v=\"1\">\n[FIRST_MESSAGE] fix it\n</VELRA_WORKSPACE_STATE>";
        StagedCapsule {
            version: STAGED_VERSION,
            intent: NEW_SESSION.name.to_string(),
            deliver_on: NEW_SESSION.deliver_on(),
            workspace_id: workspace_id.to_string(),
            workspace_root: "/home/u/proj".to_string(),
            source_session_id: "session-a".to_string(),
            source_checkpoint_id: None,
            created_ms,
            render_version: 1,
            tokens: 42,
            content_hash: StagedCapsule::compute_hash(capsule),
            summary: "fix it".to_string(),
            capsule: capsule.to_string(),
        }
    }

    #[test]
    fn stages_and_claims_once() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let capsule = sample(WS, NOW);
        stage(&staged_path(home, WS), &capsule).unwrap();

        assert_eq!(claim(home, WS, STARTUP, NOW + 1_000).unwrap(), capsule);
        // The second consumer gets nothing, not a duplicate.
        assert_eq!(
            claim(home, WS, STARTUP, NOW + 2_000),
            Err(ClaimError::Empty)
        );
        assert!(!staged_path(home, WS).exists());
    }

    /// Duplicate consumers are the case a read-then-delete gets wrong: both
    /// read the file, both act on it, and the state is injected twice.
    #[test]
    fn two_parallel_claims_produce_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        stage(&staged_path(&home, WS), &sample(WS, NOW)).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..8 {
                let (home, barrier, winners) = (home.clone(), barrier.clone(), winners.clone());
                s.spawn(move || {
                    barrier.wait();
                    if claim(&home, WS, STARTUP, NOW + 1_000).is_ok() {
                        winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A live claim marker holds the slot. The capsule is not handed out and,
    /// crucially, it is not consumed either — the blocked caller can come back.
    #[test]
    fn a_held_claim_blocks_without_consuming() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let capsule = sample(WS, NOW);
        stage(&staged_path(home, WS), &capsule).unwrap();

        let marker = staged_dir(home, WS).join("staged_capsule.claim");
        std::fs::write(&marker, "pid=1 at=0").unwrap();

        assert_eq!(claim(home, WS, STARTUP, NOW + 1), Err(ClaimError::Busy));
        assert!(staged_path(home, WS).is_file(), "nothing was consumed");

        // Once the holder is gone, the capsule is claimable again.
        std::fs::remove_file(&marker).unwrap();
        assert_eq!(claim(home, WS, STARTUP, NOW + 2).unwrap(), capsule);
    }

    /// A process that dies holding the marker must not wedge the slot: a
    /// marker older than the lease is broken by the next claimant.
    #[test]
    fn an_expired_lease_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let capsule = sample(WS, NOW);
        stage(&staged_path(home, WS), &capsule).unwrap();

        let marker = staged_dir(home, WS).join("staged_capsule.claim");
        std::fs::write(&marker, "pid=999999 at=0").unwrap();
        // The lease is measured against the marker's mtime, so age it.
        let stale = std::time::SystemTime::now()
            - std::time::Duration::from_millis((CLAIM_LEASE_MS + 60_000) as u64);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap();
        f.set_modified(stale).unwrap();
        drop(f);

        assert_eq!(claim(home, WS, STARTUP, NOW + 1).unwrap(), capsule);
        assert!(!marker.exists(), "the broken marker is cleaned up");
    }

    /// Whatever the outcome, a claim never leaves its marker behind — that is
    /// what makes the next claim possible.
    #[test]
    fn every_claim_outcome_releases_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let marker = staged_dir(home, WS).join("staged_capsule.claim");

        for setup in [0u8, 1, 2] {
            let path = staged_path(home, WS);
            match setup {
                0 => stage(&path, &sample(WS, NOW)).unwrap(),
                1 => stage(&path, &sample("ffffffffffffffff", NOW)).unwrap(),
                _ => {
                    create_dir_private(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, "{ not json").unwrap();
                }
            }
            let _ = claim(home, WS, STARTUP, NOW + 1);
            assert!(!marker.exists(), "marker leaked for setup {setup}");
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn claiming_an_empty_slot_is_not_an_error_condition() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(claim(dir.path(), WS, STARTUP, NOW), Err(ClaimError::Empty));
        assert_eq!(
            claim(dir.path(), "never-seen", STARTUP, NOW),
            Err(ClaimError::Empty)
        );
    }

    #[test]
    fn a_stale_capsule_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        stage(&staged_path(dir.path(), WS), &sample(WS, NOW)).unwrap();
        let later = NOW + STAGED_TTL_MS + 1;
        assert!(matches!(
            claim(dir.path(), WS, STARTUP, later),
            Err(ClaimError::Stale { .. })
        ));
        // Refused, and not silently handed over on a retry either.
        assert_eq!(
            claim(dir.path(), WS, STARTUP, later),
            Err(ClaimError::Empty)
        );
    }

    #[test]
    fn a_capsule_staged_for_another_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        // Written into this workspace's directory but carrying another
        // workspace's id: the directory key and the record must agree.
        let foreign = sample("ffffffffffffffff", NOW);
        stage(&staged_path(dir.path(), WS), &foreign).unwrap();
        assert_eq!(
            claim(dir.path(), WS, STARTUP, NOW + 1),
            Err(ClaimError::WrongWorkspace {
                staged_for: "ffffffffffffffff".to_string()
            })
        );
    }

    #[test]
    fn a_tampered_capsule_fails_its_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = sample(WS, NOW);
        c.capsule.push_str("\nINJECTED");
        stage(&staged_path(dir.path(), WS), &c).unwrap();
        assert_eq!(
            claim(dir.path(), WS, STARTUP, NOW + 1),
            Err(ClaimError::Corrupt)
        );
    }

    #[test]
    fn a_malformed_file_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().unwrap();
        let path = staged_path(dir.path(), WS);
        create_dir_private(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            claim(dir.path(), WS, STARTUP, NOW),
            Err(ClaimError::Malformed)
        );

        // A record from a future format version is refused too.
        std::fs::write(&path, r#"{"version":999}"#).unwrap();
        assert_eq!(
            claim(dir.path(), WS, STARTUP, NOW),
            Err(ClaimError::Malformed)
        );
    }

    /// A stage killed before its rename leaves a temp file, and the previous
    /// capsule must still be the one a consumer sees.
    #[test]
    fn an_interrupted_stage_never_replaces_the_previous_capsule() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let good = sample(WS, NOW);
        stage(&staged_path(home, WS), &good).unwrap();

        let partial = staged_dir(home, WS).join(".staged_capsule.velra-tmp-interrupted");
        std::fs::write(&partial, "{\"version\":1,\"workspace").unwrap();

        assert_eq!(peek(&staged_path(home, WS)).unwrap(), good);
        assert_eq!(claim(home, WS, STARTUP, NOW + 1).unwrap(), good);
    }

    #[test]
    fn restaging_replaces_in_place_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        stage(&staged_path(home, WS), &sample(WS, NOW)).unwrap();
        let mut second = sample(WS, NOW + 5_000);
        second.source_session_id = "session-b".into();
        stage(&staged_path(home, WS), &second).unwrap();

        assert_eq!(
            peek(&staged_path(home, WS)).unwrap().source_session_id,
            "session-b"
        );
        let leftovers: Vec<_> = std::fs::read_dir(staged_dir(home, WS))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("velra-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn sweep_discards_a_stale_capsule_but_keeps_a_live_one() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        stage(&staged_path(home, WS), &sample(WS, NOW)).unwrap();
        assert_eq!(sweep(home, WS, NOW + 1_000), 0, "a live capsule stays");
        assert_eq!(sweep(home, WS, NOW + STAGED_TTL_MS + 1), 1);
        assert!(!staged_path(home, WS).exists());
    }

    #[test]
    fn two_workspaces_stage_independently() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let (a, b) = ("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
        stage(&staged_path(home, a), &sample(a, NOW)).unwrap();
        stage(&staged_path(home, b), &sample(b, NOW)).unwrap();

        assert_eq!(claim(home, a, STARTUP, NOW + 1).unwrap().workspace_id, a);
        // Claiming one workspace's capsule leaves the other's untouched.
        assert_eq!(claim(home, b, STARTUP, NOW + 1).unwrap().workspace_id, b);
    }

    #[test]
    fn serialization_is_byte_stable() {
        let a = sample(WS, NOW).to_json();
        let b = sample(WS, NOW).to_json();
        assert_eq!(a, b);
        let parsed: StagedCapsule = serde_json::from_str(&a).unwrap();
        assert_eq!(parsed, sample(WS, NOW));
    }

    // ------------------------------------------- staged-delivery intent

    use super::source::{CLEAR, COMPACT, RESUME};

    /// A `velra restore` capsule is for a brand-new session and nothing else.
    /// The three sources that continue an existing conversation must leave it
    /// completely alone — not consumed, not discarded, not degraded.
    #[test]
    fn a_restore_capsule_is_refused_by_clear_resume_and_compact() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let capsule = sample(WS, NOW);
        stage(&staged_path(home, WS), &capsule).unwrap();

        for source in [CLEAR, RESUME, COMPACT] {
            match claim(home, WS, source, NOW + 1) {
                Err(ClaimError::NotForThisSource {
                    source: got,
                    deliver_on,
                }) => {
                    assert_eq!(got, source);
                    assert_eq!(deliver_on, vec![STARTUP.to_string()]);
                }
                other => panic!("{source} must not claim a restore capsule: {other:?}"),
            }
            assert!(
                staged_path(home, WS).is_file(),
                "{source} consumed the capsule"
            );
        }

        // After all three, it is still intact and still delivers on startup.
        assert_eq!(claim(home, WS, STARTUP, NOW + 2).unwrap(), capsule);
    }

    /// A refused source must not even contend for the claim marker, or every
    /// compaction in every session would take a lock it can never use.
    #[test]
    fn a_refused_source_never_takes_the_claim_marker() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        stage(&staged_path(home, WS), &sample(WS, NOW)).unwrap();
        let marker = staged_dir(home, WS).join("staged_capsule.claim");

        // Hold the marker: a startup claim would block, but `clear` should
        // not care, because it is refused before the marker is consulted.
        std::fs::write(&marker, "pid=1 at=0").unwrap();
        assert!(matches!(
            claim(home, WS, CLEAR, NOW + 1),
            Err(ClaimError::NotForThisSource { .. })
        ));
        assert_eq!(claim(home, WS, STARTUP, NOW + 1), Err(ClaimError::Busy));
    }

    /// The extensibility claim, stated as a test: a capsule whose `deliver_on`
    /// names a source no shipped intent uses is delivered on that source, with
    /// no change to this module. A future clear- or resume-handoff workflow is
    /// a new constant and a different field value, nothing more.
    #[test]
    fn deliver_on_is_data_not_code() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut future = sample(WS, NOW);
        future.intent = "clear_handoff".to_string();
        future.deliver_on = vec![CLEAR.to_string()];
        stage(&staged_path(home, WS), &future).unwrap();

        // The shipped source is now the refused one, and vice versa.
        assert!(matches!(
            claim(home, WS, STARTUP, NOW + 1),
            Err(ClaimError::NotForThisSource { .. })
        ));
        assert_eq!(claim(home, WS, CLEAR, NOW + 1).unwrap(), future);
    }

    /// Several sources at once, for a workflow that wants them.
    #[test]
    fn a_capsule_may_accept_more_than_one_source() {
        let mut multi = sample(WS, NOW);
        multi.deliver_on = vec![STARTUP.to_string(), RESUME.to_string()];
        assert!(multi.accepts(STARTUP));
        assert!(multi.accepts(RESUME));
        assert!(!multi.accepts(CLEAR));

        // And an empty list is inert rather than universal, which is the
        // right direction for a field that gates delivery.
        let mut none = sample(WS, NOW);
        none.deliver_on = Vec::new();
        for s in [STARTUP, RESUME, COMPACT, CLEAR] {
            assert!(!none.accepts(s), "{s}");
        }
    }

    /// A consumer that fails to emit must leave the capsule staged. The hook
    /// refuses a second JSON object per process, so this happens for real.
    #[test]
    fn a_capsule_is_not_consumed_when_the_emit_declines() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let capsule = sample(WS, NOW);
        stage(&staged_path(home, WS), &capsule).unwrap();

        let mut saw = None;
        let outcome = claim_with(home, WS, STARTUP, NOW + 1, |c| {
            saw = Some(c.content_hash.clone());
            false
        });
        assert_eq!(outcome, Err(ClaimError::NotEmitted));
        assert_eq!(saw.as_deref(), Some(capsule.content_hash.as_str()));
        assert!(staged_path(home, WS).is_file(), "it must still be staged");

        // A later, successful delivery gets it.
        assert_eq!(
            claim_with(home, WS, STARTUP, NOW + 2, |_| true).unwrap(),
            capsule
        );
        assert!(!staged_path(home, WS).exists());
    }

    /// `emit` runs while the capsule is still on disk, so a consumer that
    /// crashes mid-emit loses nothing.
    #[test]
    fn emit_runs_before_the_capsule_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = staged_path(home, WS);
        stage(&path, &sample(WS, NOW)).unwrap();

        let mut present_during_emit = false;
        let _ = claim_with(home, WS, STARTUP, NOW + 1, |_| {
            present_during_emit = path.is_file();
            true
        });
        assert!(present_during_emit);
        assert!(!path.exists(), "and gone afterwards");
    }

    /// A v1 record predates `deliver_on`. There is no honest default for it,
    /// so it is refused and cleared rather than guessed at.
    #[test]
    fn a_record_from_the_previous_format_is_discarded_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = staged_path(home, WS);
        create_dir_private(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":1,"workspace_id":"0123456789abcdef","workspace_root":"/p",
                "source_session_id":"a","source_checkpoint_id":null,"created_ms":1,
                "render_version":1,"tokens":1,"content_hash":"x","summary":"s","capsule":"c"}"#,
        )
        .unwrap();
        assert_eq!(
            claim(home, WS, STARTUP, NOW),
            Err(ClaimError::Malformed),
            "a v1 record must not be read as if it were v2"
        );
        assert!(!path.exists(), "and it is cleared so the next stage works");
    }
}
