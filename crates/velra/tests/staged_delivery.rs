//! Phase 2: delivering a staged capsule at `SessionStart`.
//!
//! The rule under test, stated once:
//!
//! > A capsule staged by `velra restore` is for a **brand-new session**. It is
//! > delivered on `SessionStart(startup)` and on nothing else. The sources
//! > that continue an existing conversation — `clear`, `resume`, `compact`,
//! > `fork` — must leave it untouched and still waiting.
//!
//! Everything here drives the real `velra hook session-start` binary, because
//! the things most likely to break this are not in the staging logic at all:
//! the workspace id the hook computes, the hook's rule that it emits at most
//! one JSON object, and whether anything else can reach stdout.

mod common;

use common::{Env, Log};
use serde_json::{json, Value};
use velra_core::staging::source::{CLEAR, COMPACT, FORK, RESUME, STARTUP};
use velra_core::staging::{self, StagedCapsule};

/// Every `SessionStart` source that must *not* consume a startup capsule.
const REFUSING: &[&str] = &[CLEAR, RESUME, COMPACT, FORK];

// ------------------------------------------------------------------ harness

/// A workspace with one session worth restoring, staged exactly as
/// `velra restore` stages it.
fn staged_workspace() -> (Env, StagedCapsule) {
    staged_workspace_with(|c| c)
}

/// `staged_workspace`, with the capsule adjusted before it is written.
fn staged_workspace_with(
    edit: impl FnOnce(StagedCapsule) -> StagedCapsule,
) -> (Env, StagedCapsule) {
    let mut log = Log::new();
    log.switch_session("source-session");
    log.env.write_file("src/invoice.rs", "round_half_up\n");
    log.prompt("fix the invoice rounding bug in the ledger totals");
    log.edit("src/invoice.rs", "round_half_even\n");
    log.command_fail(
        "cargo test invoice",
        1,
        "FAILED tests/invoice.rs::rounds_half_up\n1 failed",
    );
    log.reduce();

    let workspace_id = log.env.project_id();
    let staged = velra_core::restore::build(
        &log.db.conn,
        &velra_core::restore::RestoreRequest {
            workspace_id: &workspace_id,
            workspace_root: "/workspace",
            source_session_id: "source-session",
            // The hook runs on the real clock and a staged capsule has a
            // seven-day TTL. The harness clock starts at `BASE_MS`, already
            // outside that window, so staging at the log's time would make
            // every capsule here legitimately stale — and these tests are
            // about which *source* may consume one. `stale_capsule_rejected`
            // covers the TTL on purpose.
            now_ms: velra_core::time::now_ms(),
        },
        &velra_core::render::RenderConfig::default(),
    )
    .expect("build");

    let staged = edit(staged);
    let env = log.into_env();
    staging::stage(&staging::staged_path(&env.home, &workspace_id), &staged).expect("stage");
    (env, staged)
}

/// Runs `velra hook session-start` for a brand-new destination session.
fn session_start(env: &Env, source: &str) -> common::HookOutput {
    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(source);
    // A brand-new session: never the id the state came from.
    payload["session_id"] = json!("destination-session");
    env.hook("session-start", &payload)
}

fn staged_file(env: &Env) -> std::path::PathBuf {
    staging::staged_path(&env.home, &env.project_id())
}

/// Asserts stdout is exactly one JSON object and nothing else, and returns it.
///
/// This is the assertion the whole output contract rests on: a single stray
/// byte — a log line, a progress message, a panic — makes the response
/// unparseable, so "is only JSON" is checked before "parses as JSON".
fn assert_only_json(out: &common::HookOutput) -> Value {
    assert_eq!(out.code, 0, "exit code");
    assert!(
        out.stderr.is_empty(),
        "stderr must be empty: {}",
        out.stderr
    );
    assert!(
        out.stdout.ends_with('\n'),
        "stdout must end with exactly one newline: {:?}",
        out.stdout
    );
    let body = out.stdout.strip_suffix('\n').expect("trailing newline");
    assert!(
        !body.contains('\n'),
        "stdout must be a single line, got: {body:?}"
    );
    assert_eq!(body.trim(), body, "stdout must have no surrounding slop");
    let value: Value =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {body:?}"));
    assert!(value.is_object(), "stdout must be one JSON object: {value}");
    value
}

fn additional_context(value: &Value) -> &str {
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "hookEventName must be exactly SessionStart: {value}"
    );
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("additionalContext must exist and be a string: {value}"))
}

// ------------------------------------------------------- the delivering case

#[test]
fn startup_delivers_startup_capsule() {
    let (env, staged) = staged_workspace();
    let value = assert_only_json(&session_start(&env, STARTUP));
    assert_eq!(
        additional_context(&value),
        staged.capsule,
        "the staged bytes, unchanged"
    );
    assert!(additional_context(&value).contains("invoice"));
    assert!(!staged_file(&env).exists(), "delivery consumes the capsule");
}

#[test]
fn session_start_without_capsule() {
    let (env, _) = staged_workspace();
    std::fs::remove_file(staged_file(&env)).expect("unstage");
    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        }
        out.push(p);
    }
    out
}

/// A workspace that has never had anything staged: no directory, no files.
/// The fast no-op path must not create any of them.
#[test]
fn empty_workspace_no_op() {
    let env = Env::new();
    let before: Vec<_> = walk(&env.home);

    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);

    assert!(
        !staging::staged_root(&env.home).exists(),
        "the staged tree must not be created by a lookup"
    );
    // The hook still records its own event, so the ledger legitimately grows;
    // what must not appear is anything under `staged/`.
    let after: Vec<_> = walk(&env.home);
    let new_staged: Vec<_> = after
        .iter()
        .filter(|p| !before.contains(p))
        .filter(|p| p.to_string_lossy().contains("staged"))
        .collect();
    assert!(new_staged.is_empty(), "{new_staged:?}");
}

// -------------------------------------------------------- the refusal matrix

/// One row of the delivery-intent matrix: this source emits nothing and
/// leaves the capsule exactly as it found it.
fn assert_refuses(source: &str) {
    let (env, staged) = staged_workspace();
    let out = session_start(&env, source);
    assert_eq!(out.code, 0, "{source}");
    assert!(out.stdout.is_empty(), "{source} emitted: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "{source} stderr: {:?}", out.stderr);
    assert!(staged_file(&env).is_file(), "{source} consumed the capsule");
    // Byte-identical, not merely present: a refusal must not rewrite it.
    assert_eq!(
        staging::peek(&staged_file(&env)).expect("still readable"),
        staged,
        "{source} altered the capsule"
    );
}

#[test]
fn clear_rejects_startup_only_capsule() {
    assert_refuses(CLEAR);
}

#[test]
fn resume_rejects_startup_only_capsule() {
    assert_refuses(RESUME);
}

#[test]
fn compact_rejects_startup_only_capsule() {
    assert_refuses(COMPACT);
}

#[test]
fn fork_rejects_startup_only_capsule() {
    assert_refuses(FORK);
}

/// Having been refused by every non-startup source in turn, the capsule is
/// still intact and still delivers to the session it was staged for.
#[test]
fn a_capsule_survives_every_refusing_source_and_then_delivers() {
    let (env, staged) = staged_workspace();
    for source in REFUSING.iter().chain(REFUSING.iter()) {
        let out = session_start(&env, source);
        assert!(out.stdout.is_empty(), "{source} emitted");
        assert!(staged_file(&env).is_file(), "{source} ate it");
    }
    let value = assert_only_json(&session_start(&env, STARTUP));
    assert_eq!(additional_context(&value), staged.capsule);
    assert!(!staged_file(&env).exists());
}

/// A capsule whose metadata names sources this build does not ship is refused
/// by all of them, rather than delivered by whichever happens to run.
#[test]
fn wrong_source_metadata_rejected() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.intent = "some_future_workflow".to_string();
        c.deliver_on = vec!["a-source-that-does-not-exist".to_string()];
        c
    });
    for source in std::iter::once(&STARTUP).chain(REFUSING.iter()) {
        let out = session_start(&env, source);
        assert_eq!(out.code, 0, "{source}");
        assert!(out.stdout.is_empty(), "{source} emitted: {:?}", out.stdout);
        assert!(staged_file(&env).is_file(), "{source} consumed it");
    }
}

/// A capsule with no `deliver_on` at all is inert, not universal. A missing
/// gate must never read as an open one.
#[test]
fn a_capsule_with_no_delivery_intent_is_never_delivered() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.deliver_on = Vec::new();
        c
    });
    for source in std::iter::once(&STARTUP).chain(REFUSING.iter()) {
        let out = session_start(&env, source);
        assert!(out.stdout.is_empty(), "{source} emitted: {:?}", out.stdout);
    }
}

/// A source Velra has never heard of is refused, not delivered by default.
#[test]
fn an_unknown_session_start_source_is_refused() {
    let (env, _) = staged_workspace();
    for source in ["", "Startup", "STARTUP", "startup ", "some-future-source"] {
        let out = session_start(&env, source);
        assert_eq!(out.code, 0, "{source:?}");
        assert!(
            out.stdout.is_empty(),
            "{source:?} delivered: {}",
            out.stdout
        );
        assert!(staged_file(&env).is_file(), "{source:?} consumed it");
    }
}

// ----------------------------------------------------- invalid staged state

#[test]
fn wrong_workspace_rejected() {
    let (env, staged) = staged_workspace();
    std::fs::remove_file(staged_file(&env)).expect("clear ours");

    // The same capsule, filed under a different workspace key.
    let elsewhere = "ffffffffffffffff";
    staging::stage(&staging::staged_path(&env.home, elsewhere), &staged).expect("stage");

    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(
        staging::staged_path(&env.home, elsewhere).is_file(),
        "the other workspace's capsule is untouched"
    );
}

/// A record whose `workspace_id` disagrees with the directory holding it is
/// refused rather than injected into the wrong repository.
#[test]
fn a_capsule_whose_recorded_workspace_disagrees_is_rejected() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.workspace_id = "ffffffffffffffff".to_string();
        c
    });
    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
}

#[test]
fn stale_capsule_rejected() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.created_ms -= staging::STAGED_TTL_MS + 60_000;
        c
    });
    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(out.stderr.is_empty());
    assert!(!staged_file(&env).exists(), "and it is cleared away");
}

#[test]
fn malformed_capsule_rejected() {
    let (env, _) = staged_workspace();
    for garbage in [
        "",
        "{ not json",
        "null",
        "[]",
        r#"{"version":99}"#,
        r#"{"version":2}"#,
        "\u{0}\u{1}\u{2}",
        "{\"version\":2,\"workspace_id\":\"truncated",
    ] {
        std::fs::write(staged_file(&env), garbage).expect("write garbage");
        let out = session_start(&env, STARTUP);
        assert_eq!(out.code, 0, "{garbage:?}");
        assert!(out.stdout.is_empty(), "{garbage:?} -> {:?}", out.stdout);
        assert!(out.stderr.is_empty(), "{garbage:?} -> {:?}", out.stderr);
    }
}

/// A capsule whose text no longer matches its hash has been tampered with or
/// mangled; it must not be injected.
#[test]
fn a_capsule_failing_its_content_hash_is_rejected() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.capsule.push_str("\nINJECTED INSTRUCTION");
        c
    });
    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
}

/// A capsule larger than Claude Code's hook-output margin would be truncated,
/// and half a capsule is worse than none. It is refused and left in place:
/// `velra restore` cannot produce one, so this means tampering or a foreign
/// build, and destroying the evidence would help nobody.
#[test]
fn oversized_capsule_rejected() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.capsule = "x".repeat(12_000);
        c.content_hash = StagedCapsule::compute_hash(&c.capsule);
        c
    });
    let out = session_start(&env, STARTUP);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "oversized capsule was emitted");
    assert!(out.stderr.is_empty());
    assert!(
        staged_file(&env).is_file(),
        "left recoverable, not consumed"
    );
}

/// The largest capsule still inside the margin must deliver intact — the
/// ceiling is a ceiling, not an off-by-one that drops valid output.
#[test]
fn a_maximum_size_capsule_still_delivers() {
    const SIZE: usize = 9_400;
    let (env, staged) = staged_workspace_with(|mut c| {
        c.capsule = "y".repeat(SIZE);
        c.content_hash = StagedCapsule::compute_hash(&c.capsule);
        c
    });
    let value = assert_only_json(&session_start(&env, STARTUP));
    assert_eq!(additional_context(&value), staged.capsule);
    assert_eq!(additional_context(&value).chars().count(), SIZE);
    assert!(!staged_file(&env).exists());
}

// ------------------------------------------------------------ exactly once

#[test]
fn duplicate_session_start_delivers_once() {
    let (env, _) = staged_workspace();
    assert_only_json(&session_start(&env, STARTUP));

    let second = session_start(&env, STARTUP);
    assert_eq!(second.code, 0);
    assert!(
        second.stdout.is_empty(),
        "delivered twice: {}",
        second.stdout
    );
}

#[test]
fn consumed_capsule_is_not_redelivered() {
    let (env, _) = staged_workspace();
    assert_only_json(&session_start(&env, STARTUP));
    assert!(!staged_file(&env).exists());

    for source in std::iter::once(&STARTUP).chain(REFUSING.iter()) {
        let out = session_start(&env, source);
        assert_eq!(out.code, 0, "{source}");
        assert!(
            out.stdout.is_empty(),
            "{source} redelivered: {}",
            out.stdout
        );
    }
}

/// Several sessions starting at once: exactly one receives the capsule, and
/// the losers neither delete it, corrupt it, replace it nor inject it.
#[test]
fn concurrent_session_start_single_winner() {
    let (env, staged) = staged_workspace();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let mut handles = Vec::new();
    for i in 0..6 {
        let (home, config, project, barrier) = (
            env.home.clone(),
            env.config.clone(),
            env.project.clone(),
            barrier.clone(),
        );
        handles.push(std::thread::spawn(move || {
            let payload = json!({
                "session_id": format!("destination-{i}"),
                "hook_event_name": "SessionStart",
                "source": STARTUP,
                "cwd": project.to_string_lossy(),
            })
            .to_string();
            barrier.wait();
            let out = assert_cmd::Command::cargo_bin("velra")
                .expect("binary")
                .env("VELRA_HOME", &home)
                .env("CLAUDE_CONFIG_DIR", &config)
                .env("CLAUDE_PROJECT_DIR", &project)
                .env("TZ", "UTC")
                .env("VELRA_TEST_WATCHDOG_MS", common::TEST_WATCHDOG_MS)
                .args(["hook", "session-start"])
                .write_stdin(payload)
                .output()
                .expect("run hook");
            assert_eq!(out.status.code(), Some(0), "every hook exits 0");
            assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
            String::from_utf8_lossy(&out.stdout).into_owned()
        }));
    }
    let outputs: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();

    let winners: Vec<&String> = outputs.iter().filter(|s| !s.is_empty()).collect();
    assert_eq!(winners.len(), 1, "exactly one winner, got {winners:?}");

    let value: Value = serde_json::from_str(winners[0].trim_end()).expect("winner JSON");
    assert_eq!(additional_context(&value), staged.capsule);
    assert!(!staged_file(&env).exists());
}

/// A claimant that dies holding the claim must not wedge the slot forever:
/// the lease expires and the next session start recovers the capsule.
#[test]
fn interrupted_claim_recovers() {
    let (env, staged) = staged_workspace();
    let marker = staging::staged_dir(&env.home, &env.project_id()).join("staged_capsule.claim");

    // A claim left behind by a process that never came back.
    std::fs::write(&marker, "pid=999999 at=0").expect("marker");
    let blocked = session_start(&env, STARTUP);
    assert_eq!(blocked.code, 0);
    assert!(blocked.stdout.is_empty(), "a held claim must block");
    assert!(staged_file(&env).is_file(), "and must not consume");

    // Age the marker past its lease, as a dead holder's would be.
    let stale = std::time::SystemTime::now()
        - std::time::Duration::from_millis((staging::CLAIM_LEASE_MS + 120_000) as u64);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&marker)
        .expect("open marker");
    f.set_modified(stale).expect("age the marker");
    drop(f);

    let value = assert_only_json(&session_start(&env, STARTUP));
    assert_eq!(additional_context(&value), staged.capsule);
    assert!(!marker.exists(), "the broken claim is cleaned up");
    assert!(!staged_file(&env).exists());
}

// --------------------------------------------------------------- fail open

/// A directory where the database belongs: `Db::open` fails with CANTOPEN,
/// which is neither "busy" nor "corrupt", so nothing rotates it aside.
fn break_the_database(env: &Env) {
    let db_path = env.db_path();
    let _ = std::fs::remove_file(&db_path);
    for side in ["-wal", "-shm"] {
        let mut p = db_path.clone().into_os_string();
        p.push(side);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
    std::fs::create_dir(&db_path).expect("db path as a directory");
    std::fs::write(db_path.join("keep"), b"not a database").expect("occupy it");
}

/// Staged delivery reads one file and touches no table, so a ledger that
/// cannot be opened must not take the restore down with it.
#[test]
fn fail_open_on_database_failure() {
    let (env, staged) = staged_workspace();
    break_the_database(&env);

    let value = assert_only_json(&session_start(&env, STARTUP));
    assert_eq!(additional_context(&value), staged.capsule);
    assert!(!staged_file(&env).exists(), "consumed exactly once");
}

/// The mirror: with the ledger broken, a refusing source still refuses.
#[test]
fn fail_open_on_database_failure_still_refuses_other_sources() {
    let (env, _) = staged_workspace();
    break_the_database(&env);
    for source in REFUSING {
        let out = session_start(&env, source);
        assert_eq!(out.code, 0, "{source}");
        assert!(out.stdout.is_empty(), "{source}: {}", out.stdout);
        assert!(staged_file(&env).is_file(), "{source} consumed it");
    }
}

/// Staging itself being unusable — the capsule path replaced by a directory —
/// must fail open rather than break the session start.
#[test]
fn fail_open_on_staging_failure() {
    let (env, _) = staged_workspace();
    let path = staged_file(&env);
    std::fs::remove_file(&path).expect("unstage");
    std::fs::create_dir(&path).expect("capsule path as a directory");
    std::fs::write(path.join("inner"), b"x").expect("occupy");

    for source in std::iter::once(&STARTUP).chain(REFUSING.iter()) {
        let out = session_start(&env, source);
        assert_eq!(out.code, 0, "{source}");
        assert!(out.stdout.is_empty(), "{source}: {:?}", out.stdout);
        assert!(out.stderr.is_empty(), "{source}: {:?}", out.stderr);
    }
}

#[test]
fn malformed_hook_input_is_fail_open() {
    let (env, _) = staged_workspace();
    for raw in [
        &b""[..],
        &b"not json at all"[..],
        &b"{"[..],
        &b"[]"[..],
        &b"null"[..],
        &b"{\"hook_event_name\":\"SessionStart\""[..],
        &[0x00, 0x01, 0xff, 0xfe][..],
    ] {
        let out = env.hook_raw("session-start", raw);
        assert_eq!(out.code, 0, "{raw:?}");
        assert!(out.stdout.is_empty(), "{raw:?} -> {:?}", out.stdout);
        assert!(out.stderr.is_empty(), "{raw:?} -> {:?}", out.stderr);
        assert!(staged_file(&env).is_file(), "{raw:?} consumed the capsule");
    }
}

/// The installed runtime omits optional fields: Velra's own event log for
/// Claude Code 2.1.272 holds `SessionStart` payloads carrying nothing but
/// `{"source":"startup"}`. Delivery must not require what is not sent.
#[test]
fn missing_optional_sessionstart_fields_is_safe() {
    let (env, staged) = staged_workspace();
    let minimal = json!({
        "session_id": "destination-session",
        "hook_event_name": "SessionStart",
        "source": STARTUP,
    });
    let value = assert_only_json(&env.hook("session-start", &minimal));
    assert_eq!(additional_context(&value), staged.capsule);
    assert!(!staged_file(&env).exists());
}

/// Even the session id is optional as far as delivery is concerned — the
/// workspace, not the session, is what locates a staged capsule.
#[test]
fn a_session_start_without_a_session_id_still_resolves_the_workspace() {
    let (env, _) = staged_workspace();
    let out = env.hook(
        "session-start",
        &json!({"hook_event_name": "SessionStart", "source": STARTUP}),
    );
    assert_eq!(out.code, 0);
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    // Whether a session-less input is delivered to or skipped, it must never
    // crash and never half-consume.
    if out.stdout.is_empty() {
        assert!(staged_file(&env).is_file());
    } else {
        assert_only_json(&out);
        assert!(!staged_file(&env).exists());
    }
}

#[test]
fn a_disabled_velra_delivers_nothing() {
    let (env, _) = staged_workspace();
    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(STARTUP);
    let out = env.hook_with_env("session-start", &payload, &[("VELRA_DISABLE", "1")]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(staged_file(&env).is_file(), "nothing was consumed");
}

// ------------------------------------------------------ the output contract

#[test]
fn structured_json_output_is_valid() {
    let (env, staged) = staged_workspace();
    let value = assert_only_json(&session_start(&env, STARTUP));

    let hso = &value["hookSpecificOutput"];
    assert!(hso.is_object(), "hookSpecificOutput must be an object");
    assert_eq!(hso["hookEventName"], "SessionStart");
    assert!(hso["additionalContext"].is_string());
    assert_eq!(hso["additionalContext"].as_str().unwrap(), staged.capsule);
    assert!(value["systemMessage"].is_string(), "{value}");
    let keys: Vec<&String> = value.as_object().expect("object").keys().collect();
    assert_eq!(keys.len(), 2, "unexpected top-level keys: {keys:?}");
}

/// The escaping test. A capsule holding every character that breaks
/// hand-rolled JSON must survive the round trip byte for byte.
#[test]
fn capsule_content_is_byte_exact_after_json_decode() {
    let nasty = concat!(
        "line one with \"double quotes\" and 'single'\n",
        "a backslash \\ and a double \\\\ and a path C:\\Users\\dev\\app\n",
        "JSON-like text: {\"hookSpecificOutput\": {\"additionalContext\": \"nested\"}}\n",
        "a closing brace } and a bracket ] on their own\n",
        "unicode: \u{26a1} \u{2014} \u{00e9}\u{00e8} \u{4e2d}\u{6587} \u{1f600}\n",
        "control-ish: tab\there, carriage\rreturn\n",
        "trailing backslash at end of line \\\n",
        "</VELRA_WORKSPACE_STATE>",
    );
    let (env, staged) = staged_workspace_with(|mut c| {
        c.capsule = nasty.to_string();
        c.content_hash = StagedCapsule::compute_hash(&c.capsule);
        c
    });

    let out = session_start(&env, STARTUP);
    let value = assert_only_json(&out);
    let decoded = additional_context(&value);

    assert_eq!(
        decoded, nasty,
        "decoded capsule differs from what was staged"
    );
    assert_eq!(decoded.as_bytes(), staged.capsule.as_bytes(), "byte-exact");
    // The raw line really did carry escapes rather than literal characters.
    assert!(out.stdout.contains("\\n"), "newlines must be escaped");
    assert!(out.stdout.contains("\\\""), "quotes must be escaped");
    assert!(out.stdout.contains("\\\\"), "backslashes must be escaped");
    assert!(
        out.stdout.contains("\\r"),
        "carriage returns must be escaped"
    );
    assert!(!staged_file(&env).exists(), "consumed exactly once");
}

#[test]
fn stdout_contains_only_structured_json() {
    let (env, _) = staged_workspace();
    let out = session_start(&env, STARTUP);

    assert_eq!(out.stdout.matches('\n').count(), 1, "{:?}", out.stdout);
    assert!(out.stdout.starts_with('{'), "{:?}", out.stdout);
    assert!(out.stdout.ends_with("}\n"), "{:?}", out.stdout);
    assert_eq!(
        out.stdout.matches("\"hookSpecificOutput\"").count(),
        1,
        "exactly one delivery object"
    );
    // Not the raw capsule leaking alongside the JSON.
    assert!(
        !out.stdout.contains("\n<VELRA_WORKSPACE_STATE"),
        "raw capsule text on stdout: {:?}",
        out.stdout
    );
    assert_only_json(&out);
}

/// Velra's diagnostics go to files under `$VELRA_HOME/logs`, never to stdout
/// and never to stderr. This drives a delivery with debug logging on and
/// proves the output contract is unaffected.
#[test]
fn logger_output_never_reaches_stdout() {
    let (env, staged) = staged_workspace();
    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(STARTUP);
    payload["session_id"] = json!("destination-session");

    let out = env.hook_with_env("session-start", &payload, &[("VELRA_LOG", "debug")]);
    let value = assert_only_json(&out);
    assert_eq!(additional_context(&value), staged.capsule);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);

    // Debug logging really did happen — to a file.
    let logs = env.home.join("logs");
    let mut lines = 0usize;
    for entry in std::fs::read_dir(&logs).expect("logs dir").flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            lines += 1;
            assert!(
                !out.stdout.contains(line),
                "a log line reached stdout: {line}"
            );
        }
    }
    assert!(lines > 0, "debug logging produced nothing to check");
}

/// Diagnostics on a *rejected* capsule must also stay off both streams.
#[test]
fn a_rejected_capsule_logs_without_touching_stdout_or_stderr() {
    let (env, _) = staged_workspace_with(|mut c| {
        c.created_ms -= staging::STAGED_TTL_MS + 60_000;
        c
    });
    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(STARTUP);
    let out = env.hook_with_env("session-start", &payload, &[("VELRA_LOG", "debug")]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

// -------------------------------------------------- source-session isolation

/// The destination session is new and must stay new: delivery gives it the
/// capsule text and nothing of the source session's identity.
#[test]
fn the_destination_session_does_not_inherit_the_source_identity() {
    let (env, _) = staged_workspace();
    assert_only_json(&session_start(&env, STARTUP));
    env.drain();

    let db = env.open_db();
    let rows: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = 'destination-session'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert!(rows >= 1, "the destination logged its own events");

    assert!(
        velra_core::continuation::live(&db.conn, "destination-session")
            .expect("live")
            .is_none(),
        "a staged delivery must not create a continuation for the destination"
    );
    let source_rows: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = 'source-session'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert!(source_rows >= 1, "the source session's log is intact");
}

/// Without an explicit restore, one session's state never reaches another.
#[test]
fn an_unrelated_session_start_receives_nothing() {
    let (env, _) = staged_workspace();
    std::fs::remove_file(staged_file(&env)).expect("nothing staged");

    for source in std::iter::once(&STARTUP).chain(REFUSING.iter()) {
        let out = session_start(&env, source);
        assert!(
            out.stdout.is_empty(),
            "{source} injected state with nothing staged: {}",
            out.stdout
        );
    }
}

/// `clear` still expires the live continuation: Phase 1's behaviour is not
/// disturbed by staged delivery sharing the same function.
#[test]
fn clear_still_expires_the_continuation_without_touching_the_staged_capsule() {
    let (env, _) = staged_workspace();
    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(CLEAR);
    env.hook("session-start", &payload).assert_contract();
    env.drain();

    let db = env.open_db();
    assert!(
        velra_core::continuation::live(&db.conn, &env.session)
            .expect("live")
            .is_none(),
        "clear expires the live continuation"
    );
    assert!(staged_file(&env).is_file(), "and leaves staging alone");
}

// -------------------------------------------------------- workspace identity

/// The invariant the feature rests on: the id `velra restore` writes under is
/// the id `SessionStart` reads from.
#[test]
fn hook_and_cli_agree_on_workspace_identity() {
    let (env, _) = staged_workspace();
    let out = env
        .cmd()
        .args(["restore", "--list", "--json"])
        .output()
        .expect("restore --list");
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        value["workspace_id"].as_str().expect("workspace_id"),
        env.project_id()
    );
    // A successful delivery *is* the assertion that the hook agreed.
    assert_only_json(&session_start(&env, STARTUP));
}

/// The regression this refactor exists for. `hook.rs` always honoured
/// `CLAUDE_PROJECT_DIR`; `cli.rs` did not, so where that variable pointed
/// anywhere but the repository root the two computed different ids — restore
/// staged under one key, `SessionStart` looked under another, and because a
/// missing capsule is indistinguishable from nothing staged, the feature
/// would simply never work, silently.
#[test]
fn the_cli_and_the_hook_agree_even_from_a_subdirectory() {
    let (env, _) = staged_workspace();
    let deep = env.project.join("src/deep/nested");
    std::fs::create_dir_all(&deep).expect("nested dirs");

    let out = env
        .cmd()
        .current_dir(&deep)
        .args(["restore", "--list", "--json"])
        .output()
        .expect("restore --list");
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(
        value["workspace_id"].as_str().expect("workspace_id"),
        env.project_id(),
        "the CLI must resolve the workspace from a subdirectory"
    );

    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(STARTUP);
    payload["session_id"] = json!("destination-session");
    payload["cwd"] = json!(deep.to_string_lossy());
    assert_only_json(&env.hook("session-start", &payload));
}

/// A capsule staged by the Phase 1 CLI, found by the Phase 2 hook, with no
/// help from the test harness: `velra restore` writes it and `velra hook
/// session-start` reads it back.
#[test]
fn a_capsule_staged_by_the_cli_is_found_by_the_hook() {
    let (env, _) = staged_workspace();
    std::fs::remove_file(staged_file(&env)).expect("start clean");

    // Phase 1 writes it.
    let staged = env
        .cmd()
        .args(["restore", "--session", "source-session", "--json"])
        .output()
        .expect("velra restore");
    assert!(staged.status.success(), "{staged:?}");
    let meta: Value = serde_json::from_str(&String::from_utf8_lossy(&staged.stdout)).expect("json");
    assert_eq!(meta["staged"], true);

    // Phase 2 reads it, and gets exactly what Phase 1 recorded.
    let value = assert_only_json(&session_start(&env, STARTUP));
    assert_eq!(
        StagedCapsule::compute_hash(additional_context(&value)),
        meta["content_hash"].as_str().expect("content_hash"),
        "the hook delivered different bytes than the CLI staged"
    );
}

#[test]
fn workspace_identity_is_stable_across_path_spellings() {
    let (env, _) = staged_workspace();
    let respelled = env.project.join("src").join("..");

    let mut payload = env.base_payload("SessionStart");
    payload["source"] = json!(STARTUP);
    payload["session_id"] = json!("destination-session");
    payload["cwd"] = json!(respelled.to_string_lossy());
    let out = env
        .cmd()
        .env("CLAUDE_PROJECT_DIR", &respelled)
        .args(["hook", "session-start"])
        .write_stdin(payload.to_string())
        .output()
        .expect("run hook");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !out.stdout.is_empty(),
        "a respelled path must resolve to the same workspace"
    );
}

// ------------------------------------------------------------- real fixtures

/// The recorded `SessionStart` fixtures for the installed Claude Code version,
/// replayed against a real staged capsule. This is the closest thing to the
/// production contract that can run without starting Claude Code.
#[test]
fn recorded_session_start_fixtures_drive_the_matrix() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude-code/2.1.272");

    for (file, delivers) in [
        ("session_start_startup.json", true),
        ("session_start_minimal.json", true),
        ("session_start_clear.json", false),
        ("session_start_resume.json", false),
        ("session_start_compact.json", false),
        ("session_start_fork.json", false),
    ] {
        let (env, staged) = staged_workspace();
        let raw = std::fs::read(dir.join(file)).expect("fixture");
        // The fixture's `cwd` is a recorded path from another machine; the
        // workspace comes from CLAUDE_PROJECT_DIR, as it does in production.
        let out = env.hook_raw("session-start", &raw);
        assert_eq!(out.code, 0, "{file}");
        assert!(out.stderr.is_empty(), "{file}: {:?}", out.stderr);

        if delivers {
            let value = assert_only_json(&out);
            assert_eq!(additional_context(&value), staged.capsule, "{file}");
            assert!(!staged_file(&env).exists(), "{file} did not consume");
        } else {
            assert!(out.stdout.is_empty(), "{file} delivered: {:?}", out.stdout);
            assert!(staged_file(&env).is_file(), "{file} consumed the capsule");
        }
    }
}
