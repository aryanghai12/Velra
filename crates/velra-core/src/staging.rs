//! The staged capsule: state a user explicitly chose to carry out of one
//! Claude Code session and into the next one.
//!
//! # Where this sits in the model
//!
//! ```text
//! WORKSPACE                      <- durable ownership boundary
//! ├── SESSION A -> checkpoints   <- source session, a first-class identity
//! ├── SESSION B -> checkpoints
//! └── staged capsule             <- the newest record, chosen explicitly by the user
//! ```
//!
//! Automatic delivery (`continuation.rs`) never crosses a session boundary and
//! that does not change. This module is the *only* path by which one session's
//! state can reach another, and it opens solely because a person selected a
//! source session by hand.
//!
//! # Location
//!
//! `$VELRA_HOME/staged/<workspace_id>/`, one file per staged record.
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
//! # Records and claims
//!
//! ```text
//! capsule.<gen>.json      a staged record; written once, never rewritten
//! capsule.<gen>.claimed   its claim; created once, never taken over
//! ```
//!
//! `<gen>` orders records ([`Gen`]): a stage never replaces a file, it adds a
//! record newer than every one present and then removes the older ones by
//! their exact names. Only the newest record is ever delivered.
//!
//! The invariants, and what each rests on:
//!
//! 1. **Cleanup touches only what it owns.** Every removal names one record
//!    or one claim exactly, and a name is never reused, so a claimant's
//!    cleanup cannot remove a record staged after it began. (The layout before
//!    this one kept a single `staged_capsule` that a restage renamed over; a
//!    claimant that finished after a restage removed the new capsule.)
//! 2. **A record is claimed at most once.** The claim is the exclusive
//!    creation of `capsule.<gen>.claimed` — the one filesystem operation both
//!    platforms agree is atomic — and nothing ever breaks or takes over a
//!    claim. There is no lease to expire, so there is no moment at which two
//!    processes both hold one. (The previous claim marker could be broken once
//!    it was a minute old; a slow holder and the breaker then both delivered,
//!    as did two breakers.)
//! 3. **A claim is removed only after its record, or by a holder that did not
//!    deliver.** A claimant checks that its record still exists *after*
//!    creating the claim, so a claim name freed by any other path — the
//!    record's own delivery, a restage, `sweep` — leads nowhere.
//! 4. **Nothing unverifiable is delivered.** A record is delivered only if it
//!    parses at [`STAGED_VERSION`], its hash matches, it names the claiming
//!    workspace, it accepts the `SessionStart` source and it is inside
//!    [`STAGED_TTL_MS`]. A newest record that fails any of these is not
//!    replaced by an older one: that would be delivering state the user
//!    already superseded.
//!
//! # Delivery is at most once
//!
//! `emit` runs while the claim is held and before the record is removed. A
//! process that dies between the two — the watchdog firing after the capsule
//! reached stdout — leaves the record and its claim behind. Nothing can tell
//! from the filesystem whether that capsule reached Claude Code, so it is
//! **not** delivered again: the claim stays, and [`claim_with`] reports
//! [`ClaimError::Interrupted`] once it is older than [`CLAIM_LEASE_MS`].
//! `velra status` says so and `velra restore` stages it again. A second copy
//! of a capsule injected into a session is the failure this module exists to
//! prevent; a restore that has to be repeated by hand is not silent and loses
//! nothing the ledger does not still hold.
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

/// A temp file left by a stage that died before publishing is swept after
/// this long. Generous, because the alternative to waiting is deleting a file
/// out from under a process that is merely slow.
pub const CLAIM_SWEEP_MS: i64 = 24 * 3600 * 1000;

/// A claim older than this belongs to a process that did not finish.
///
/// It is not a lease: nothing takes an old claim over (module docs,
/// invariant 2). It only decides how a held claim is reported —
/// [`ClaimError::Busy`] while another session start may still be working,
/// [`ClaimError::Interrupted`] once no hook could still be (the hook's
/// watchdog ends it within a second).
pub const CLAIM_LEASE_MS: i64 = 60_000;

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

/// `$VELRA_HOME/staged/<workspace_id>`: where a workspace's records live.
pub fn staged_dir(velra_home: &Path, workspace_id: &str) -> PathBuf {
    staged_root(velra_home).join(workspace_id)
}

const RECORD_PREFIX: &str = "capsule.";
const RECORD_SUFFIX: &str = ".json";
const CLAIM_SUFFIX: &str = ".claimed";
const TMP_PREFIX: &str = ".capsule.velra-tmp-";

/// Names of the single-file layout that preceded records (never released):
/// ignored by every reader, removed by [`sweep`] and [`clear`].
const LEGACY_NAMES: &[&str] = &["staged_capsule", "staged_capsule.claim"];
const LEGACY_PREFIXES: &[&str] = &[".staged_capsule.velra-tmp-", "staged_capsule.claimed-"];

/// A record's generation: `<sequence>-<nonce>`, ordered by sequence, then by
/// nonce.
///
/// The sequence is the stage's wall clock in nanoseconds, or one more than
/// the newest record present when the clock reads lower — so a record staged
/// after another is newer even across a clock step backwards. The nonce is
/// random, so two stages that pick the same sequence still get distinct names
/// and one consistent order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Gen {
    seq: u128,
    nonce: String,
}

impl Gen {
    fn parse(s: &str) -> Option<Gen> {
        let (seq, nonce) = s.split_once('-')?;
        let well_formed = !seq.is_empty()
            && seq.bytes().all(|b| b.is_ascii_digit())
            && !nonce.is_empty()
            && nonce.bytes().all(|b| b.is_ascii_hexdigit());
        if !well_formed {
            return None;
        }
        Some(Gen {
            seq: seq.parse().ok()?,
            nonce: nonce.to_string(),
        })
    }

    fn render(&self) -> String {
        format!("{:020}-{}", self.seq, self.nonce)
    }
}

impl std::fmt::Display for Gen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// One staged record on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub gen: Gen,
    /// `capsule.<gen>.json`.
    pub path: PathBuf,
}

impl Record {
    /// `capsule.<gen>.claimed`: this record's claim.
    pub fn claim_path(&self) -> PathBuf {
        self.path
            .with_file_name(format!("{RECORD_PREFIX}{}{CLAIM_SUFFIX}", self.gen))
    }
}

fn parse_record_name(name: &str) -> Option<Gen> {
    Gen::parse(
        name.strip_prefix(RECORD_PREFIX)?
            .strip_suffix(RECORD_SUFFIX)?,
    )
}

fn parse_claim_name(name: &str) -> Option<Gen> {
    Gen::parse(
        name.strip_prefix(RECORD_PREFIX)?
            .strip_suffix(CLAIM_SUFFIX)?,
    )
}

/// The records in `dir`, oldest first. A name that does not parse is not a
/// record.
pub fn records(dir: &Path) -> Vec<Record> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Record> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let gen = parse_record_name(name.to_str()?)?;
            Some(Record {
                gen,
                path: e.path(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.gen.cmp(&b.gen));
    out
}

/// The newest record in `dir`: the only one that is ever delivered.
pub fn newest(dir: &Path) -> Option<Record> {
    records(dir).pop()
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

fn nonce() -> String {
    format!("{:020x}", ulid::Ulid::generate().random())
}

fn wall_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Stages `capsule` in `dir` as a new record and returns its path.
///
/// The record is written to a temp file, fsynced, and renamed to a name no
/// file has had before, so a reader sees a whole record or none, and the
/// rename never replaces anything. Once it is published, every older record
/// is removed by its exact name, record before claim (invariant 3); a record
/// staged concurrently with a newer generation is not touched, and it
/// supersedes this one in turn.
pub fn stage(dir: &Path, capsule: &StagedCapsule) -> std::io::Result<PathBuf> {
    create_dir_private(dir)?;
    let floor = records(dir).last().map(|r| r.gen.seq + 1).unwrap_or(0);
    let gen = Gen {
        seq: wall_ns().max(floor),
        nonce: nonce(),
    };
    let tmp = dir.join(format!("{TMP_PREFIX}{gen}"));
    let path = dir.join(format!("{RECORD_PREFIX}{gen}{RECORD_SUFFIX}"));
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
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    after_publish_hook();
    for older in records(dir).into_iter().filter(|r| r.gen < gen) {
        remove_record(&older);
    }
    Ok(path)
}

/// Removes a record and then its claim, each by its exact name.
fn remove_record(r: &Record) {
    let _ = std::fs::remove_file(&r.path);
    let _ = std::fs::remove_file(r.claim_path());
}

/// Removes a newest record nobody can use, and every record older than it.
/// The older ones are superseded -- present only because a stage died before
/// its cleanup -- and removing the newest alone would make one of them the
/// newest, delivered in place of what the user staged after it.
fn remove_through(dir: &Path, newest: &Record) {
    for r in records(dir).iter().filter(|r| r.gen < newest.gen) {
        remove_record(r);
    }
    remove_record(newest);
}

/// Parses one record file. `None` for anything unreadable, unparseable or of
/// an unknown version.
pub fn read_record(path: &Path) -> Option<StagedCapsule> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: StagedCapsule = serde_json::from_str(&text).ok()?;
    (parsed.version == STAGED_VERSION).then_some(parsed)
}

/// The newest staged capsule in `dir`, without consuming it. `None` when
/// nothing is staged or the newest record does not parse — never an older
/// record in its place.
pub fn peek(dir: &Path) -> Option<StagedCapsule> {
    read_record(&newest(dir)?.path)
}

/// What a workspace's staging directory holds, for `velra status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Empty,
    /// Waiting for a session start it accepts.
    Staged(StagedCapsule),
    /// Claimed by a session start that has not finished; `interrupted` once
    /// the claim is older than [`CLAIM_LEASE_MS`] (module docs).
    Claimed {
        capsule: Option<StagedCapsule>,
        interrupted: bool,
    },
    /// The newest record does not parse at this version.
    Unreadable,
}

/// The state of a workspace's newest record.
pub fn slot(dir: &Path, now_ms: i64) -> Slot {
    let Some(r) = newest(dir) else {
        return Slot::Empty;
    };
    let capsule = read_record(&r.path);
    match claim_age(&r, now_ms) {
        Some(age) => Slot::Claimed {
            capsule,
            interrupted: age > CLAIM_LEASE_MS,
        },
        None => capsule.map_or(Slot::Unreadable, Slot::Staged),
    }
}

/// Whether a file of the single-file layout that preceded records is present.
/// Nothing reads it; `velra status` mentions it.
pub fn has_legacy(dir: &Path) -> bool {
    dir.join(LEGACY_NAMES[0]).is_file()
}

/// Why a claim did not produce a capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// Nothing staged.
    Empty,
    /// Another session start holds the newest record's claim, and may still be
    /// delivering it.
    Busy,
    /// The newest record was claimed by a session start that did not finish:
    /// it may have been delivered, so it is not delivered again. The record
    /// and its claim stay until a restage or `sweep` removes them.
    Interrupted { age_ms: i64 },
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
            ClaimError::Busy => write!(f, "another session start is claiming the staged capsule"),
            ClaimError::Interrupted { age_ms } => write!(
                f,
                "the staged capsule was claimed {}s ago by a session start that did not finish; \
                 it may already have been delivered, so it is not delivered again \
                 (run `velra restore` to stage it again)",
                age_ms / 1000
            ),
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

/// Creates `r`'s claim. `create_new` is the whole mechanism: the filesystem
/// decides, atomically, which single caller creates it, and everyone else
/// sees `AlreadyExists`. The time inside is what [`claim_age`] reads.
///
/// # Why this is not a rename
///
/// The obvious implementation is to rename the record aside and treat a
/// successful rename as the claim. On POSIX that works. On Windows it does
/// not, and the failure is silent: `MoveFileEx` resolves the source to a
/// *handle* and then renames whatever that handle points at, so a second
/// claimant that opened the source before the first one moved it will happily
/// rename the already-moved file to its own target and report success. Both
/// callers then hold a valid capsule and both inject it.
fn create_claim(r: &Record, now_ms: i64) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(r.claim_path())?;
    // The claim is the file's existence; the contents say when, for
    // `claim_age`, and who, for a human reading the directory.
    let _ = writeln!(f, "at={now_ms} pid={}", std::process::id());
    let _ = f.sync_all();
    Ok(())
}

/// How long ago `r` was claimed, or `None` when it is not. A claim whose
/// time cannot be read counts as just made: it is reported as busy, never
/// taken over.
fn claim_age(r: &Record, now_ms: i64) -> Option<i64> {
    let text = match std::fs::read_to_string(r.claim_path()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(0),
    };
    let at = text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("at="))
        .and_then(|v| v.parse::<i64>().ok());
    Some(at.map_or(0, |at| now_ms.saturating_sub(at).max(0)))
}

#[cfg(any(test, feature = "fault-injection"))]
thread_local! {
    /// Runs between a claimant's validation and its claim: the window in which
    /// a concurrent stage or claimant can act, opened deterministically by the
    /// tests instead of by timing.
    pub static BEFORE_CLAIM: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
    /// Runs between a stage publishing its record and removing the older
    /// ones: the window in which a concurrent stage can publish a newer one.
    pub static AFTER_PUBLISH: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

fn before_claim_hook() {
    #[cfg(any(test, feature = "fault-injection"))]
    BEFORE_CLAIM.with(|h| {
        let f = h.borrow_mut().take();
        if let Some(mut f) = f {
            f()
        }
    });
}

fn after_publish_hook() {
    #[cfg(any(test, feature = "fault-injection"))]
    AFTER_PUBLISH.with(|h| {
        // Taken before it runs, so a stage made inside it does not run it again.
        let f = h.borrow_mut().take();
        if let Some(mut f) = f {
            f()
        }
    });
}

/// Claims the newest staged capsule of a workspace, at most once, for one
/// `SessionStart` source.
///
/// `emit` runs while this process holds the record's claim and **before** the
/// record is removed; it returns whether the capsule actually reached the
/// consumer. A `false` return releases the claim and leaves the capsule
/// staged, exactly as `continuation::deliver` leaves a continuation
/// deliverable when its write fails — the hook's `emit` refuses a second JSON
/// object per process, so this is a real case, not a hypothetical one.
///
/// Outcomes, and what each does to the newest record:
///
/// | outcome | record |
/// |---|---|
/// | delivered | removed, then its claim |
/// | [`ClaimError::NotForThisSource`] | **kept** — it is waiting for its source |
/// | [`ClaimError::NotEmitted`] | **kept**, claim released — delivery did not happen |
/// | [`ClaimError::Busy`], [`ClaimError::Interrupted`] | kept with its claim — not delivered again |
/// | [`ClaimError::Stale`] | removed — nothing will ever accept it |
/// | [`ClaimError::Malformed`] | removed — it can never become readable |
/// | [`ClaimError::Corrupt`], [`ClaimError::WrongWorkspace`] | kept as evidence of a bug |
///
/// An older record is never delivered in the newest one's place.
pub fn claim_with(
    velra_home: &Path,
    workspace_id: &str,
    source: &str,
    now_ms: i64,
    emit: impl FnOnce(&StagedCapsule) -> bool,
) -> Result<StagedCapsule, ClaimError> {
    let dir = staged_dir(velra_home, workspace_id);
    let mut emit = Some(emit);
    // A record removed under us -- delivered by another session start, or
    // superseded by a restage -- sends the claimant back to the newest one.
    // Each retry follows a removal by someone else, so this is bounded by
    // concurrent activity, and the bound below by a constant.
    for _ in 0..4 {
        let Some(r) = newest(&dir) else {
            return Err(ClaimError::Empty);
        };
        let text = match std::fs::read_to_string(&r.path) {
            Ok(t) => t,
            // Removed between the listing and the read (on Windows a file
            // pending deletion also fails to open): look again.
            Err(_) if !r.path.exists() => continue,
            Err(_) => return Err(ClaimError::Malformed),
        };
        let capsule = match serde_json::from_str::<StagedCapsule>(&text) {
            Ok(c) if c.version == STAGED_VERSION => c,
            _ => {
                remove_through(&dir, &r);
                return Err(ClaimError::Malformed);
            }
        };
        // Source first: every `/clear`, compaction and resume reaches this
        // line, and none of them should create a claim they cannot use, or
        // report one they could never have made.
        if !capsule.accepts(source) {
            return Err(ClaimError::NotForThisSource {
                source: source.to_string(),
                deliver_on: capsule.deliver_on,
            });
        }
        if let Some(age_ms) = claim_age(&r, now_ms) {
            return Err(if age_ms > CLAIM_LEASE_MS {
                ClaimError::Interrupted { age_ms }
            } else {
                ClaimError::Busy
            });
        }
        if !capsule.hash_matches() {
            return Err(ClaimError::Corrupt);
        }
        if capsule.workspace_id != workspace_id {
            return Err(ClaimError::WrongWorkspace {
                staged_for: capsule.workspace_id,
            });
        }
        let age_ms = now_ms.saturating_sub(capsule.created_ms);
        if age_ms > STAGED_TTL_MS {
            remove_through(&dir, &r);
            return Err(ClaimError::Stale { age_ms });
        }

        before_claim_hook();
        match create_claim(&r, now_ms) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ClaimError::Busy)
            }
            // The directory went away, or cannot be written: nothing is
            // claimed and nothing is delivered.
            Err(_) => return Err(ClaimError::Busy),
        }
        // Invariant 3: the claim is only worth anything if the record it
        // names is still there. A record removed between the read and the
        // claim was delivered or superseded; its claim name leads nowhere.
        if !r.path.is_file() {
            let _ = std::fs::remove_file(r.claim_path());
            continue;
        }
        let emit = emit.take().expect("emit runs at most once");
        if !emit(&capsule) {
            let _ = std::fs::remove_file(r.claim_path());
            return Err(ClaimError::NotEmitted);
        }
        remove_record(&r);
        return Ok(capsule);
    }
    Err(ClaimError::Busy)
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

/// Removes what nobody will read from a workspace's staging directory:
/// records older than the newest, a newest record that is stale or does not
/// parse, claims whose record is gone, temp files of a stage that died more
/// than [`CLAIM_SWEEP_MS`] ago, and the files of the layout before records.
///
/// A claimed newest record that is still inside its TTL is kept: it is the
/// evidence `velra status` reports as an interrupted delivery, and a restage
/// replaces it. Every removal is by exact name, records before claims.
///
/// Returns how many files were removed. Best effort throughout: a file that
/// cannot be removed is left alone rather than turned into an error.
pub fn sweep(velra_home: &Path, workspace_id: &str, now_ms: i64) -> usize {
    let dir = staged_dir(velra_home, workspace_id);
    let mut removed = 0;
    let mut remove = |p: &Path| {
        if std::fs::remove_file(p).is_ok() {
            removed += 1;
        }
    };
    let mut recs = records(&dir);
    if let Some(newest) = recs.pop() {
        for older in &recs {
            remove(&older.path);
            remove(&older.claim_path());
        }
        let unusable = read_record(&newest.path)
            .map(|c| now_ms.saturating_sub(c.created_ms) > STAGED_TTL_MS)
            // A record that does not parse is not a capsule anyone can use.
            .unwrap_or(true);
        if unusable {
            remove(&newest.path);
            remove(&newest.claim_path());
        }
    }
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return removed;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let doomed = if let Some(gen) = parse_claim_name(&name) {
            // Invariant 3: a claim goes once its record has.
            !dir.join(format!("{RECORD_PREFIX}{gen}{RECORD_SUFFIX}"))
                .exists()
        } else if name.starts_with(TMP_PREFIX) {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX) > CLAIM_SWEEP_MS)
                .unwrap_or(false)
        } else {
            LEGACY_NAMES.contains(&name.as_str())
                || LEGACY_PREFIXES.iter().any(|p| name.starts_with(p))
        };
        if doomed {
            remove(&path);
        }
    }
    removed
}

/// Removes every record and claim in a workspace's staging directory
/// (`velra restore --clear`). Returns how many records there were.
pub fn clear(velra_home: &Path, workspace_id: &str) -> usize {
    let dir = staged_dir(velra_home, workspace_id);
    let recs = records(&dir);
    for r in &recs {
        remove_record(r);
    }
    let legacy = has_legacy(&dir);
    sweep(velra_home, workspace_id, i64::MIN);
    recs.len() + usize::from(legacy)
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

    fn dir(home: &Path) -> PathBuf {
        staged_dir(home, WS)
    }

    /// Every file in the staging directory, by name.
    fn names(home: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir(home))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    fn write_newest(home: &Path, text: &str) -> PathBuf {
        let path = stage(&dir(home), &sample(WS, NOW)).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn stages_and_claims_once() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let capsule = sample(WS, NOW);
        stage(&dir(home), &capsule).unwrap();

        assert_eq!(claim(home, WS, STARTUP, NOW + 1_000).unwrap(), capsule);
        // The second consumer gets nothing, not a duplicate.
        assert_eq!(
            claim(home, WS, STARTUP, NOW + 2_000),
            Err(ClaimError::Empty)
        );
        assert!(names(home).is_empty(), "{:?}", names(home));
    }

    /// Duplicate consumers are the case a read-then-delete gets wrong: both
    /// read the file, both act on it, and the state is injected twice.
    #[test]
    fn two_parallel_claims_produce_exactly_one_winner() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        stage(&dir(&home), &sample(WS, NOW)).unwrap();

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

    /// A held claim is reported, and the record is neither delivered nor
    /// consumed.
    #[test]
    fn a_held_claim_blocks_without_consuming() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let path = stage(&dir(home), &sample(WS, NOW)).unwrap();
        let r = newest(&dir(home)).unwrap();
        std::fs::write(r.claim_path(), format!("at={NOW}")).unwrap();

        assert_eq!(claim(home, WS, STARTUP, NOW + 1), Err(ClaimError::Busy));
        assert!(path.is_file(), "nothing was consumed");
    }

    /// Nothing takes an old claim over: past the lease it is reported as an
    /// interrupted delivery, and still not delivered.
    #[test]
    fn an_old_claim_is_reported_never_taken_over() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let path = stage(&dir(home), &sample(WS, NOW)).unwrap();
        let r = newest(&dir(home)).unwrap();
        std::fs::write(r.claim_path(), format!("at={NOW} pid=999999")).unwrap();

        let later = NOW + CLAIM_LEASE_MS + 1;
        assert_eq!(
            claim(home, WS, STARTUP, later),
            Err(ClaimError::Interrupted {
                age_ms: CLAIM_LEASE_MS + 1
            })
        );
        assert!(path.is_file() && r.claim_path().is_file());
        assert!(matches!(
            slot(&dir(home), later),
            Slot::Claimed {
                interrupted: true,
                capsule: Some(_)
            }
        ));
        // An unreadable claim is a held claim, not a free one.
        std::fs::write(r.claim_path(), "garbage").unwrap();
        assert_eq!(claim(home, WS, STARTUP, later), Err(ClaimError::Busy));
    }

    /// Whatever the outcome, a claim that did not deliver leaves no claim of
    /// its own behind: the next session start can still claim.
    #[test]
    fn every_undelivered_outcome_releases_its_claim() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        for setup in 0u8..4 {
            let _ = clear(home, WS);
            match setup {
                0 => {
                    stage(&dir(home), &sample("ffffffffffffffff", NOW)).unwrap();
                }
                1 => {
                    write_newest(home, "{ not json");
                }
                2 => {
                    let mut c = sample(WS, NOW);
                    c.capsule.push('x');
                    stage(&dir(home), &c).unwrap();
                }
                _ => {
                    stage(&dir(home), &sample(WS, NOW)).unwrap();
                }
            }
            let _ = claim_with(home, WS, STARTUP, NOW + 1, |_| false);
            assert!(
                !names(home).iter().any(|n| n.ends_with(CLAIM_SUFFIX)),
                "claim leaked for setup {setup}: {:?}",
                names(home)
            );
        }
    }

    #[test]
    fn claiming_an_empty_slot_is_not_an_error_condition() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(claim(d.path(), WS, STARTUP, NOW), Err(ClaimError::Empty));
        assert_eq!(
            claim(d.path(), "never-seen", STARTUP, NOW),
            Err(ClaimError::Empty)
        );
    }

    #[test]
    fn a_stale_capsule_is_refused() {
        let d = tempfile::tempdir().unwrap();
        stage(&dir(d.path()), &sample(WS, NOW)).unwrap();
        let later = NOW + STAGED_TTL_MS + 1;
        assert!(matches!(
            claim(d.path(), WS, STARTUP, later),
            Err(ClaimError::Stale { .. })
        ));
        // Refused, and not silently handed over on a retry either.
        assert_eq!(claim(d.path(), WS, STARTUP, later), Err(ClaimError::Empty));
    }

    #[test]
    fn a_capsule_staged_for_another_workspace_is_refused() {
        let d = tempfile::tempdir().unwrap();
        // Written into this workspace's directory but carrying another
        // workspace's id: the directory key and the record must agree.
        let foreign = sample("ffffffffffffffff", NOW);
        let path = stage(&dir(d.path()), &foreign).unwrap();
        assert_eq!(
            claim(d.path(), WS, STARTUP, NOW + 1),
            Err(ClaimError::WrongWorkspace {
                staged_for: "ffffffffffffffff".to_string()
            })
        );
        assert!(path.is_file(), "kept as evidence");
    }

    #[test]
    fn a_tampered_capsule_fails_its_content_hash() {
        let d = tempfile::tempdir().unwrap();
        let mut c = sample(WS, NOW);
        c.capsule.push_str("\nINJECTED");
        stage(&dir(d.path()), &c).unwrap();
        assert_eq!(
            claim(d.path(), WS, STARTUP, NOW + 1),
            Err(ClaimError::Corrupt)
        );
    }

    #[test]
    fn a_malformed_file_is_refused_rather_than_misread() {
        let d = tempfile::tempdir().unwrap();
        let path = write_newest(d.path(), "{ not json");
        assert_eq!(
            claim(d.path(), WS, STARTUP, NOW),
            Err(ClaimError::Malformed)
        );
        assert!(!path.exists(), "removed: it can never become readable");

        // A record from a future format version is refused too.
        write_newest(d.path(), r#"{"version":999}"#);
        assert_eq!(
            claim(d.path(), WS, STARTUP, NOW),
            Err(ClaimError::Malformed)
        );
    }

    /// A newest record that cannot be delivered is not replaced by an older
    /// one: the older one is state the user already superseded.
    #[test]
    fn an_older_record_is_never_delivered_in_the_newest_ones_place() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let old = stage(&dir(home), &sample(WS, NOW)).unwrap();
        let old_bytes = std::fs::read(&old).unwrap();
        let mut bad = sample(WS, NOW + 1);
        bad.capsule.push_str("tampered");
        stage(&dir(home), &bad).unwrap();
        // As if the older record had survived the restage (a stage that died
        // before its cleanup).
        std::fs::write(&old, &old_bytes).unwrap();
        assert_eq!(records(&dir(home)).len(), 2);
        assert_eq!(claim(home, WS, STARTUP, NOW + 2), Err(ClaimError::Corrupt));
        assert_eq!(
            claim(home, WS, STARTUP, NOW + 2),
            Err(ClaimError::Corrupt),
            "still the newest, still refused"
        );
    }

    /// A stage killed before its rename leaves a temp file, and the previous
    /// capsule must still be the one a consumer sees.
    #[test]
    fn an_interrupted_stage_never_replaces_the_previous_capsule() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let good = sample(WS, NOW);
        stage(&dir(home), &good).unwrap();

        let partial = dir(home).join(format!("{TMP_PREFIX}interrupted"));
        std::fs::write(&partial, "{\"version\":1,\"workspace").unwrap();

        assert_eq!(peek(&dir(home)).unwrap(), good);
        assert_eq!(claim(home, WS, STARTUP, NOW + 1).unwrap(), good);
    }

    #[test]
    fn restaging_supersedes_and_leaves_no_temp_files() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        stage(&dir(home), &sample(WS, NOW)).unwrap();
        let mut second = sample(WS, NOW + 5_000);
        second.source_session_id = "session-b".into();
        stage(&dir(home), &second).unwrap();

        assert_eq!(peek(&dir(home)).unwrap().source_session_id, "session-b");
        assert_eq!(records(&dir(home)).len(), 1, "{:?}", names(home));
        assert!(
            !names(home).iter().any(|n| n.contains("velra-tmp")),
            "{:?}",
            names(home)
        );
    }

    /// A record staged after another is newer, whatever the clock says.
    #[test]
    fn generations_order_by_sequence_then_nonce() {
        let a = Gen::parse("00000000000000000005-ff").unwrap();
        let b = Gen::parse("00000000000000000006-00").unwrap();
        let c = Gen::parse("00000000000000000006-01").unwrap();
        assert!(a < b && b < c);
        assert_eq!(Gen::parse(&c.render()), Some(c));
        for bad in ["", "-", "12", "12-", "-ab", "x1-ab", "12-zz", "12-ab-cd"] {
            assert_eq!(Gen::parse(bad), None, "{bad}");
        }
        // A record whose sequence is ahead of the clock is still followed.
        let d = tempfile::tempdir().unwrap();
        create_dir_private(&dir(d.path())).unwrap();
        let ahead = dir(d.path()).join(format!(
            "{RECORD_PREFIX}{:020}-00{RECORD_SUFFIX}",
            u128::from(u64::MAX)
        ));
        std::fs::write(&ahead, sample(WS, NOW).to_json()).unwrap();
        let mut next = sample(WS, NOW);
        next.source_session_id = "next".into();
        stage(&dir(d.path()), &next).unwrap();
        assert_eq!(peek(&dir(d.path())).unwrap().source_session_id, "next");
        assert!(!ahead.exists());
    }

    /// A stage removes only records older than its own: one staged
    /// concurrently, after it published and before its cleanup, is newer and
    /// is kept.
    #[test]
    fn a_stage_never_removes_a_newer_record_staged_concurrently() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        let h = home.clone();
        AFTER_PUBLISH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut b = sample(WS, NOW + 1);
                b.source_session_id = "B".into();
                stage(&dir(&h), &b).unwrap();
            }))
        });
        let mut a = sample(WS, NOW);
        a.source_session_id = "A".into();
        stage(&dir(&home), &a).unwrap();
        assert_eq!(peek(&dir(&home)).unwrap().source_session_id, "B");
        assert_eq!(records(&dir(&home)).len(), 1, "{:?}", names(&home));
    }

    #[test]
    fn sweep_discards_a_stale_capsule_but_keeps_a_live_one() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        stage(&dir(home), &sample(WS, NOW)).unwrap();
        assert_eq!(sweep(home, WS, NOW + 1_000), 0, "a live capsule stays");
        assert_eq!(sweep(home, WS, NOW + STAGED_TTL_MS + 1), 1);
        assert!(names(home).is_empty());
    }

    /// An interrupted delivery is evidence, kept until a restage or its TTL;
    /// an orphaned claim and the old single-file layout are removed.
    #[test]
    fn sweep_keeps_an_interrupted_claim_and_removes_orphans() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        stage(&dir(home), &sample(WS, NOW)).unwrap();
        let r = newest(&dir(home)).unwrap();
        std::fs::write(r.claim_path(), format!("at={NOW}")).unwrap();
        let orphan = dir(home).join(format!(
            "{RECORD_PREFIX}00000000000000000001-ab{CLAIM_SUFFIX}"
        ));
        std::fs::write(&orphan, "at=0").unwrap();
        for legacy in ["staged_capsule", "staged_capsule.claim"] {
            std::fs::write(dir(home).join(legacy), "{}").unwrap();
        }
        assert!(has_legacy(&dir(home)));

        assert_eq!(sweep(home, WS, NOW + CLAIM_LEASE_MS * 10), 3);
        assert!(r.path.is_file() && r.claim_path().is_file());
        assert!(!orphan.exists() && !has_legacy(&dir(home)));
    }

    #[test]
    fn two_workspaces_stage_independently() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let (a, b) = ("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
        stage(&staged_dir(home, a), &sample(a, NOW)).unwrap();
        stage(&staged_dir(home, b), &sample(b, NOW)).unwrap();

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
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let capsule = sample(WS, NOW);
        let path = stage(&dir(home), &capsule).unwrap();

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
            assert!(path.is_file(), "{source} consumed the capsule");
        }

        // After all three, it is still intact and still delivers on startup.
        assert_eq!(claim(home, WS, STARTUP, NOW + 2).unwrap(), capsule);
    }

    /// A refused source never creates a claim, or every compaction in every
    /// session would leave one behind.
    #[test]
    fn a_refused_source_never_takes_a_claim() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        stage(&dir(home), &sample(WS, NOW)).unwrap();
        assert!(matches!(
            claim(home, WS, CLEAR, NOW + 1),
            Err(ClaimError::NotForThisSource { .. })
        ));
        assert!(!names(home).iter().any(|n| n.ends_with(CLAIM_SUFFIX)));

        // Nor does it report an interrupted delivery it could never have made:
        // that is for the source the capsule was staged for.
        let r = newest(&dir(home)).unwrap();
        std::fs::write(r.claim_path(), format!("at={NOW}")).unwrap();
        let later = NOW + CLAIM_LEASE_MS + 1;
        assert!(matches!(
            claim(home, WS, CLEAR, later),
            Err(ClaimError::NotForThisSource { .. })
        ));
        assert!(matches!(
            claim(home, WS, STARTUP, later),
            Err(ClaimError::Interrupted { .. })
        ));
    }

    /// The extensibility claim, stated as a test: a capsule whose `deliver_on`
    /// names a source no shipped intent uses is delivered on that source, with
    /// no change to this module. A future clear- or resume-handoff workflow is
    /// a new constant and a different field value, nothing more.
    #[test]
    fn deliver_on_is_data_not_code() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let mut future = sample(WS, NOW);
        future.intent = "clear_handoff".to_string();
        future.deliver_on = vec![CLEAR.to_string()];
        stage(&dir(home), &future).unwrap();

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
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let capsule = sample(WS, NOW);
        let path = stage(&dir(home), &capsule).unwrap();

        let mut saw = None;
        let outcome = claim_with(home, WS, STARTUP, NOW + 1, |c| {
            saw = Some(c.content_hash.clone());
            false
        });
        assert_eq!(outcome, Err(ClaimError::NotEmitted));
        assert_eq!(saw.as_deref(), Some(capsule.content_hash.as_str()));
        assert!(path.is_file(), "it must still be staged");

        // A later, successful delivery gets it.
        assert_eq!(
            claim_with(home, WS, STARTUP, NOW + 2, |_| true).unwrap(),
            capsule
        );
        assert!(!path.exists());
    }

    /// `emit` runs while the capsule is still on disk, so a consumer that
    /// crashes mid-emit leaves evidence behind rather than nothing.
    #[test]
    fn emit_runs_before_the_capsule_is_deleted() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let path = stage(&dir(home), &sample(WS, NOW)).unwrap();

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
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        let path = write_newest(
            home,
            r#"{"version":1,"workspace_id":"0123456789abcdef","workspace_root":"/p",
                "source_session_id":"a","source_checkpoint_id":null,"created_ms":1,
                "render_version":1,"tokens":1,"content_hash":"x","summary":"s","capsule":"c"}"#,
        );
        assert_eq!(
            claim(home, WS, STARTUP, NOW),
            Err(ClaimError::Malformed),
            "a v1 record must not be read as if it were v2"
        );
        assert!(!path.exists(), "and it is cleared so the next stage works");
    }

    /// The file the previous layout staged is not a record: nothing delivers
    /// it, and `status` can say it is there.
    #[test]
    fn the_single_file_layout_is_never_read() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        create_dir_private(&dir(home)).unwrap();
        std::fs::write(dir(home).join("staged_capsule"), sample(WS, NOW).to_json()).unwrap();
        assert_eq!(claim(home, WS, STARTUP, NOW + 1), Err(ClaimError::Empty));
        assert!(has_legacy(&dir(home)));
        assert_eq!(clear(home, WS), 1);
        assert!(names(home).is_empty());
    }
}
