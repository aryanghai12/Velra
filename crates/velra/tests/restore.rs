//! Explicit cross-session restore (Phase 1).
//!
//! The model these tests pin:
//!
//! ```text
//! WORKSPACE                     <- durable ownership boundary
//! ├── SESSION A -> its own state
//! ├── SESSION B -> its own state
//! └── staged capsule            <- written only when a user names a session
//! ```
//!
//! Two things have to be true at once, and most of what follows is one or the
//! other. Sessions must stay separated — B must never see A's work because
//! they share a workspace. And a person must be able to move A's state into a
//! new session deliberately, which is the whole point of the command.

mod common;

use common::Log;
use velra_core::restore::{self, RestoreError, RestoreRequest};
use velra_core::staging::source::STARTUP;
use velra_core::staging::{self, ClaimError, StagedCapsule};

const BUDGET: velra_core::render::RenderConfig = velra_core::render::RenderConfig {
    budget_tokens: velra_core::render::DEFAULT_BUDGET_TOKENS,
};

/// A workspace with two sessions, each with its own distinct task state.
///
/// The two sessions are deliberately about different things, so "A's state"
/// and "B's state" are distinguishable by content rather than by trusting the
/// query that produced them.
fn workspace_with_two_sessions() -> Log {
    let mut log = Log::new();

    log.switch_session("session-a");
    log.env.write_file("src/invoice.rs", "round_half_up\n");
    log.prompt("fix the invoice rounding bug in the ledger totals");
    log.edit("src/invoice.rs", "round_half_even\n");
    log.command_fail(
        "cargo test invoice",
        1,
        "FAILED tests/invoice.rs::rounds_half_up\n1 failed",
    );

    log.switch_session("session-b");
    log.env.write_file("src/payment.rs", "retry_once\n");
    log.prompt("debug the payment retry loop that spins on a 429");
    log.edit("src/payment.rs", "retry_backoff\n");
    log.command_fail(
        "cargo test payment",
        1,
        "FAILED tests/payment.rs::backs_off\n1 failed",
    );

    log.reduce();
    log
}

fn request<'a>(log: &'a Log, workspace: &'a str, session: &'a str) -> RestoreRequest<'a> {
    RestoreRequest {
        workspace_id: workspace,
        workspace_root: "/workspace",
        source_session_id: session,
        now_ms: log.ts + 10_000,
    }
}

fn build(log: &Log, session: &str) -> Result<StagedCapsule, RestoreError> {
    let ws = log.env.project_id();
    restore::build(&log.db.conn, &request(log, &ws, session), &BUDGET)
}

// ------------------------------------------------- resolving a source session

#[test]
fn restore_unknown_session() {
    let log = workspace_with_two_sessions();
    let err = build(&log, "no-such-session").expect_err("must not resolve");
    assert_eq!(
        err,
        RestoreError::UnknownSession {
            session_id: "no-such-session".into()
        }
    );
    // And it fails *safely*: nothing partial was produced.
    assert!(err.to_string().contains("no-such-session"));
}

/// A session id that exists, but belongs to a different workspace, must not
/// resolve. This is the check that keeps the workspace an ownership boundary
/// rather than a label: without the `project_id` predicate, any id typed by
/// hand would read whatever the ledger happened to hold.
#[test]
fn wrong_workspace() {
    let log = workspace_with_two_sessions();
    let other_workspace = "ffffffffffffffff";
    let err = restore::build(
        &log.db.conn,
        &request(&log, other_workspace, "session-a"),
        &BUDGET,
    )
    .expect_err("another workspace must not reach this session");
    assert!(
        matches!(err, RestoreError::UnknownSession { .. }),
        "{err:?}"
    );
}

#[test]
fn wrong_source_session() {
    let log = workspace_with_two_sessions();
    // Right workspace, session id that was never recorded in it.
    assert!(matches!(
        build(&log, "session-c"),
        Err(RestoreError::UnknownSession { .. })
    ));
}

/// A session Velra saw but recorded nothing useful for must say so rather
/// than stage an empty capsule.
#[test]
fn a_session_without_state_is_refused() {
    let mut log = Log::new();
    log.switch_session("session-empty");
    // A prompt short enough not to become a ROOT intent, and nothing else.
    log.prompt("hi");
    log.reduce();
    assert!(matches!(
        build(&log, "session-empty"),
        Err(RestoreError::NoState { .. })
    ));
}

#[test]
fn workspace_contains_multiple_sessions() {
    let log = workspace_with_two_sessions();
    let sessions = restore::sessions_for_workspace(&log.db.conn, &log.env.project_id())
        .expect("sessions for workspace");
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    assert!(ids.contains(&"session-a"), "{ids:?}");
    assert!(ids.contains(&"session-b"), "{ids:?}");
    assert_eq!(ids.len(), 2, "one row per session, not one merged stream");

    // Another workspace's listing is empty, not a superset.
    assert!(
        restore::sessions_for_workspace(&log.db.conn, "ffffffffffffffff")
            .expect("query")
            .is_empty()
    );
}

// ---------------------------------------------------------- what is restored

#[test]
fn restore_session_a() {
    let log = workspace_with_two_sessions();
    let a = build(&log, "session-a").expect("session a restores");
    assert_eq!(a.source_session_id, "session-a");
    assert!(a.capsule.contains("invoice"), "{}", a.capsule);
}

#[test]
fn restore_session_b() {
    let log = workspace_with_two_sessions();
    let b = build(&log, "session-b").expect("session b restores");
    assert_eq!(b.source_session_id, "session-b");
    assert!(b.capsule.contains("payment"), "{}", b.capsule);
}

/// The core separation claim: restoring one session returns that session's
/// state and nothing of the other's, even though both live in one workspace
/// and one database.
#[test]
fn restore_a_does_not_leak_into_b() {
    let log = workspace_with_two_sessions();
    let a = build(&log, "session-a").expect("a");
    let b = build(&log, "session-b").expect("b");

    assert!(!a.capsule.contains("payment"), "A leaked B: {}", a.capsule);
    assert!(!a.capsule.contains("retry"), "A leaked B: {}", a.capsule);
    assert!(!b.capsule.contains("invoice"), "B leaked A: {}", b.capsule);
    assert!(!b.capsule.contains("rounding"), "B leaked A: {}", b.capsule);
    assert_ne!(a.content_hash, b.content_hash);
}

/// Restore names the session the state came *from*. It says nothing about
/// where the state is going, because the destination session does not exist
/// yet and must not inherit the source's identity.
#[test]
fn destination_session_does_not_inherit_source_session_identity() {
    let log = workspace_with_two_sessions();
    let staged = build(&log, "session-a").expect("a");

    let json = staged.to_json();
    assert!(json.contains("source_session_id"));
    assert!(
        !json.contains("destination_session"),
        "the artifact must not name a destination"
    );
    // The workspace is the durable owner; the source session is recorded as
    // provenance alongside it, not in place of it.
    assert_eq!(staged.workspace_id, log.env.project_id());
    assert_eq!(staged.source_session_id, "session-a");

    // Nothing about staging attaches A's state to any other session: the
    // ledger's continuation rows are untouched by a restore.
    let live_for_b = velra_core::continuation::live(&log.db.conn, "session-b").expect("live");
    assert!(
        live_for_b.is_none(),
        "restore must not create continuations"
    );
}

/// Two sessions of one workspace each stage into the same slot, one at a
/// time, and neither corrupts the other's ledger state. The staged slot is
/// last-write-wins by design — it holds the one session the user chose — but
/// *choosing* B must leave A restorable, not consume it.
#[test]
fn multiple_sessions_in_one_workspace_cannot_overwrite_each_other() {
    let log = workspace_with_two_sessions();
    let home = log.env.home.clone();
    let ws = log.env.project_id();
    let path = staging::staged_dir(&home, &ws);

    let a = build(&log, "session-a").expect("a");
    staging::stage(&path, &a).expect("stage a");
    let b = build(&log, "session-b").expect("b");
    staging::stage(&path, &b).expect("stage b");

    assert_eq!(
        staging::peek(&path).expect("staged").source_session_id,
        "session-b"
    );
    // A is still fully restorable: staging B consumed nothing of A's.
    let a_again = build(&log, "session-a").expect("a is still there");
    assert_eq!(a_again.capsule, a.capsule);
}

// -------------------------------------------------------------- the artifact

#[test]
fn deterministic_capsule_generation() {
    let log = workspace_with_two_sessions();
    let first = build(&log, "session-a").expect("first");
    let second = build(&log, "session-a").expect("second");

    // Same ledger, same clock: byte-identical, hash included.
    assert_eq!(first.capsule, second.capsule);
    assert_eq!(first.content_hash, second.content_hash);
    assert_eq!(first.to_json(), second.to_json());
    assert_eq!(
        first.content_hash,
        StagedCapsule::compute_hash(&first.capsule)
    );
}

#[test]
fn capsule_budget_enforced() {
    let mut log = Log::new();
    log.switch_session("session-big");
    log.prompt("refactor the ledger aggregation across the whole importer tree");
    // Far more state than the budget can hold, so the ladder must bite.
    for i in 0..60 {
        let path = format!("src/module_{i:02}.rs");
        log.env.write_file(&path, "v0\n");
        log.edit(
            &path,
            &format!("v1 with a reasonably long line of content {i}\n"),
        );
        log.read(&path);
    }
    log.command_fail(
        "pytest -q",
        1,
        &format!(
            "FAILED {}\n1 failed",
            "tests/test_aggregation.py::test_totals"
        ),
    );
    log.reduce();

    let staged = build(&log, "session-big").expect("restores");
    let budget = velra_core::render::DEFAULT_BUDGET_TOKENS;
    assert!(
        staged.tokens <= budget,
        "staged {} tokens against a {budget} budget",
        staged.tokens
    );
    assert!(
        velra_core::text::estimate_tokens(&staged.capsule) <= budget,
        "the staged bytes themselves must be within budget"
    );
    // Restore does not get its own, larger budget.
    assert!(staged.capsule.contains("VELRA_WORKSPACE_STATE"));
}

/// The staged file is the one artifact a future consumer injects verbatim, so
/// a secret must not survive into it even if one reached the ledger.
#[test]
fn secret_redaction() {
    let mut log = Log::new();
    log.switch_session("session-secret");
    log.prompt("deploy the service and keep the staging credentials working");
    log.env.write_file("deploy.sh", "echo hi\n");
    log.edit(
        "deploy.sh",
        "export TOKEN=ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII\n",
    );
    log.command_fail(
        "./deploy.sh",
        1,
        "auth failed for sk-ant-api03-SECRETSECRETSECRETSECRETSECRET\nAKIAIOSFODNN7EXAMPLE rejected",
    );
    log.reduce();

    let staged = build(&log, "session-secret").expect("restores");
    for secret in [
        "ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII",
        "sk-ant-api03-SECRETSECRETSECRETSECRETSECRET",
        "AKIAIOSFODNN7EXAMPLE",
    ] {
        assert!(
            !staged.capsule.contains(secret),
            "secret reached the staged capsule: {secret}"
        );
        assert!(
            !staged.to_json().contains(secret),
            "secret reached the staged file: {secret}"
        );
    }
}

// ---------------------------------------------------------------- the staging

#[test]
fn atomic_staging() {
    let log = workspace_with_two_sessions();
    let home = log.env.home.clone();
    let ws = log.env.project_id();
    let path = staging::staged_dir(&home, &ws);

    let staged = build(&log, "session-a").expect("a");
    staging::stage(&path, &staged).expect("stage");

    // What landed on disk is exactly what was built.
    let read_back = staging::peek(&path).expect("peek");
    assert_eq!(read_back, staged);
    assert!(read_back.hash_matches());

    // No temp file survived the write.
    let leftovers: Vec<String> = std::fs::read_dir(staging::staged_dir(&home, &ws))
        .expect("read staged dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("velra-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A stage killed between writing its temp file and renaming it must leave
/// the previously staged capsule intact — not a half-written replacement.
#[test]
fn interrupted_staging() {
    let log = workspace_with_two_sessions();
    let home = log.env.home.clone();
    let ws = log.env.project_id();
    let path = staging::staged_dir(&home, &ws);

    let good = build(&log, "session-a").expect("a");
    staging::stage(&path, &good).expect("stage");

    // Simulate the interruption: a temp file that never got renamed.
    let orphan = staging::staged_dir(&home, &ws).join(".capsule.velra-tmp-killed");
    std::fs::write(&orphan, "{\"version\":1,\"workspace_id\":\"trunc").expect("write orphan");

    assert_eq!(staging::peek(&path).expect("still there"), good);
    assert_eq!(
        staging::claim(&home, &ws, STARTUP, good.created_ms + 1).expect("claim"),
        good
    );
}

/// Two consumers racing the same staged capsule: exactly one gets it. This is
/// the case a read-then-delete gets wrong, which is why the claim is a rename.
#[test]
fn two_parallel_restore_operations() {
    let log = workspace_with_two_sessions();
    let home = log.env.home.clone();
    let ws = log.env.project_id();
    let staged = build(&log, "session-a").expect("a");
    staging::stage(&staging::staged_dir(&home, &ws), &staged).expect("stage");

    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    std::thread::scope(|s| {
        for _ in 0..6 {
            let (home, ws, winners, barrier) =
                (home.clone(), ws.clone(), winners.clone(), barrier.clone());
            s.spawn(move || {
                barrier.wait();
                if staging::claim(&home, &ws, STARTUP, staged_now()).is_ok() {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(
        winners.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a staged capsule must be consumable exactly once"
    );
}

fn staged_now() -> i64 {
    common::BASE_MS + 60_000
}

#[test]
fn stale_staged_capsule() {
    let log = workspace_with_two_sessions();
    let home = log.env.home.clone();
    let ws = log.env.project_id();
    let staged = build(&log, "session-a").expect("a");
    staging::stage(&staging::staged_dir(&home, &ws), &staged).expect("stage");

    let much_later = staged.created_ms + staging::STAGED_TTL_MS + 1;
    assert!(matches!(
        staging::claim(&home, &ws, STARTUP, much_later),
        Err(ClaimError::Stale { .. })
    ));
    // And a stale capsule is never handed over on a retry.
    assert_eq!(
        staging::claim(&home, &ws, STARTUP, much_later),
        Err(ClaimError::Empty)
    );
}

/// A capsule whose recorded workspace disagrees with the directory it sits in
/// is refused rather than injected into the wrong repository.
#[test]
fn a_capsule_claimed_from_the_wrong_workspace_is_refused() {
    let log = workspace_with_two_sessions();
    let home = log.env.home.clone();
    let ws = log.env.project_id();
    let mut staged = build(&log, "session-a").expect("a");
    staged.workspace_id = "ffffffffffffffff".into();
    staging::stage(&staging::staged_dir(&home, &ws), &staged).expect("stage");

    assert!(matches!(
        staging::claim(&home, &ws, STARTUP, staged.created_ms + 1),
        Err(ClaimError::WrongWorkspace { .. })
    ));
}

// --------------------------------------------------- the command, end to end

/// `velra restore --list` on a workspace with sessions: the sessions appear,
/// and the command stays inside the CLI contract.
#[test]
fn the_command_lists_this_workspaces_sessions() {
    let log = workspace_with_two_sessions();
    let env = log.into_env();
    let out = env
        .cmd()
        .args(["restore", "--list", "--json"])
        .output()
        .expect("run restore --list");
    assert!(out.status.success(), "{:?}", out);
    let text = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    let ids: Vec<&str> = value["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .filter_map(|s| s["session_id"].as_str())
        .collect();
    assert!(ids.contains(&"session-a"), "{ids:?}");
    assert!(ids.contains(&"session-b"), "{ids:?}");
}

#[test]
fn the_command_stages_the_named_session_and_clears_it_again() {
    let log = workspace_with_two_sessions();
    let ws = log.env.project_id();
    let env = log.into_env();
    let staged_path = staging::staged_dir(&env.home, &ws);

    let out = env
        .cmd()
        .args(["restore", "--session", "session-a", "--json"])
        .output()
        .expect("run restore");
    assert!(out.status.success(), "{:?}", out);
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(value["source_session_id"], "session-a");
    assert_eq!(value["staged"], true);
    assert_eq!(
        staging::records(&staged_path).len(),
        1,
        "{}",
        staged_path.display()
    );

    let on_disk = staging::peek(&staged_path).expect("staged");
    assert_eq!(on_disk.source_session_id, "session-a");
    assert!(on_disk.hash_matches());

    let out = env
        .cmd()
        .args(["restore", "--clear"])
        .output()
        .expect("run restore --clear");
    assert!(out.status.success());
    assert!(staging::records(&staged_path).is_empty());
}

/// An unknown session is a clean, non-zero exit with an explanation — never a
/// panic and never a silently empty capsule.
#[test]
fn the_command_fails_safely_on_an_unknown_session() {
    let log = workspace_with_two_sessions();
    let env = log.into_env();
    let out = env
        .cmd()
        .args(["restore", "--session", "does-not-exist"])
        .output()
        .expect("run restore");
    assert!(!out.status.success(), "must not report success");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("does-not-exist"), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "the CLI reports on stdout"
    );
}

#[test]
fn the_command_does_not_stage_on_a_dry_run() {
    let log = workspace_with_two_sessions();
    let ws = log.env.project_id();
    let env = log.into_env();
    let out = env
        .cmd()
        .args(["restore", "--session", "session-a", "--dry-run"])
        .output()
        .expect("run restore --dry-run");
    assert!(out.status.success(), "{:?}", out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("VELRA_WORKSPACE_STATE"));
    assert!(staging::records(&staging::staged_dir(&env.home, &ws)).is_empty());
}

/// A transcript that is corrupt, truncated or written in a shape this build
/// has never seen must not stop the command: the session stays selectable,
/// labelled by its id and its timestamp.
#[test]
fn restore_malformed_transcript() {
    let log = workspace_with_two_sessions();
    let env = log.into_env();

    // `CLAUDE_CONFIG_DIR` is already this env's, so the transcripts live at
    // <config>/projects/<encoded cwd>/<session>.jsonl.
    let root = velra_core::paths::canonical(&env.project).unwrap_or_else(|| env.project.clone());
    let root = velra_core::paths::normalize_abs(&root.to_string_lossy());
    let dir = env
        .config
        .join("projects")
        .join(velra_core::transcript::encode_project_dir(
            &root.replace('/', "\\"),
        ));
    std::fs::create_dir_all(&dir).expect("transcript dir");

    std::fs::write(
        dir.join("session-a.jsonl"),
        // Garbage, a record type from the future, a record with no fields we
        // read, and a final line cut mid-write.
        "\u{0}\u{1}not json\n{\"type\":\"who-knows\"}\n{}\n{\"type\":\"ai-tit",
    )
    .expect("write malformed transcript");
    std::fs::write(dir.join("session-b.jsonl"), "").expect("write empty transcript");

    let out = env
        .cmd()
        .args(["restore", "--list", "--json"])
        .output()
        .expect("run restore --list");
    assert!(out.status.success(), "{:?}", out);
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let sessions = value["sessions"].as_array().expect("sessions");
    let a = sessions
        .iter()
        .find(|s| s["session_id"] == "session-a")
        .expect("session-a is still listed");
    assert!(a["title"].is_null(), "no title could be read: {a}");
    assert_eq!(a["has_state"], true, "and it is still restorable");

    // Selectable, and it really does restore.
    let out = env
        .cmd()
        .args(["restore", "--session", "session-a", "--json"])
        .output()
        .expect("run restore");
    assert!(out.status.success(), "{:?}", out);
}

/// The same path, with a transcript that is well-formed but carries no title:
/// the entry falls back to the session id and the last-activity timestamp.
#[test]
fn restore_missing_title() {
    let log = workspace_with_two_sessions();
    let env = log.into_env();
    let root = velra_core::paths::canonical(&env.project).unwrap_or_else(|| env.project.clone());
    let root = velra_core::paths::normalize_abs(&root.to_string_lossy());
    let dir = env
        .config
        .join("projects")
        .join(velra_core::transcript::encode_project_dir(
            &root.replace('/', "\\"),
        ));
    std::fs::create_dir_all(&dir).expect("transcript dir");
    // Valid records throughout, but nothing that yields a label: a slash
    // command and a subagent turn are not session titles.
    std::fs::write(
        dir.join("session-a.jsonl"),
        "{\"type\":\"queue-operation\",\"operation\":\"enqueue\"}\n\
         {\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"/clear\"}]}}\n",
    )
    .expect("write transcript");

    let out = env
        .cmd()
        .args(["restore", "--list"])
        .output()
        .expect("run restore --list");
    assert!(out.status.success(), "{:?}", out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("session-a"), "{text}");
    assert!(text.contains("Last activity:"), "{text}");
    assert!(text.contains("state: yes"), "{text}");
}
