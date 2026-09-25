//! Exact continuation state across restore (v0.1.2 retention fix, D64).
//!
//! The v0.1.2 Token-Burn qualification restored four sessions and every one of
//! them lost an identifier the next session needed, while the ledger still
//! held it. Tracing each loss to its first layer found three mechanisms:
//!
//! 1. **snapshot, superseded intent** -- the only message naming the next
//!    function was superseded by a sign-off, and the snapshot carries one
//!    `LATEST` message.
//! 2. **snapshot, command-level failures** -- a test that failed and was then
//!    fixed disappeared once a newer run of the same command failed on
//!    something else, because failures were modelled per command, not per test.
//! 3. **renderer, budget** -- the failing test's name lived only in failure
//!    excerpt lines, which the ladder cut from eight to zero while it kept
//!    lower-density lines.
//!
//! Each is reproduced below in a session about something else entirely, so the
//! tests pin the generic behaviour rather than the benchmark's fixture. The last
//! group checks the other side of the trade: that genuine noise still goes.

mod common;

use common::Log;
use velra_core::model::Trigger;
use velra_core::provenance::{self, Presence, TraceInputs};
use velra_core::render::{RenderConfig, DEFAULT_BUDGET_TOKENS};
use velra_core::restore::{self, RestoreRequest};
use velra_core::snapshot::SnapshotMeta;
use velra_core::text::estimate_tokens;

const DEFAULT: RenderConfig = RenderConfig {
    budget_tokens: DEFAULT_BUDGET_TOKENS,
};

/// A session under real budget pressure: a long objective, three stated
/// constraints, reverted attempts on two files, edits, a wide read sweep and a
/// noisy failing suite. Every capsule below renders well past the target at
/// full detail, so the ladder has to choose.
fn busy_session(log: &mut Log) {
    for (f, v) in [
        ("src/http/codec.rs", "fn parse_header() {}\n"),
        ("src/http/limits.rs", "const MAX: usize = 8192;\n"),
        ("src/http/server.rs", "fn serve() {}\n"),
        ("tests/test_codec.py", "def test_x(): ...\n"),
    ] {
        log.env.write_file(f, v);
    }
    log.prompt(
        "The HTTP codec rejects valid chunked bodies under load. Find out why the \
         chunk-size parser disagrees with the spec and fix it without touching the \
         public API of the server module.",
    );
    log.prompt(
        "Two rules. Never raise MAX globally, other services depend on it. \
         Constraint: the parser must stay allocation-free on the hot path. \
         Do not add a dependency for this.",
    );
    log.read("src/http/codec.rs");
    log.read("src/http/server.rs");
    log.command_fail(
        "python -m pytest -q",
        1,
        "E       AssertionError: chunk size 0x1f parsed as 1\n\
         E        +  where 1 = parse_chunk_size(b'1f\\r\\n')\n\
         tests/test_codec.py:41: AssertionError\n\
         E       assert 413 == 200\n\
         tests/test_limits.py:12: AssertionError\n\
         FAILED tests/test_codec.py::test_chunk_size_is_hex - AssertionError: chunk size 0x1f parsed as 1\n\
         FAILED tests/test_limits.py::test_large_header_accepted - assert 413 == 200\n\
         2 failed, 30 passed",
    );
    // A rejected approach on each of two files.
    log.edit("src/http/limits.rs", "const MAX: usize = 65536;\n");
    log.git_restore(
        "git restore src/http/limits.rs",
        &[("src/http/limits.rs", "const MAX: usize = 8192;\n")],
    );
    log.edit("src/http/server.rs", "fn serve() { buffer_all(); }\n");
    log.edit("src/http/server.rs", "fn serve() {}\n");
    // A wide sweep of unrelated reads.
    for i in 0..30 {
        let path = format!("src/misc/module_{i:02}.rs");
        log.env.write_file(&path, "x\n");
        log.read(&path);
    }
}

fn capsule(log: &mut Log) -> String {
    let c = log.capsule();
    assert!(
        estimate_tokens(&c) <= DEFAULT_BUDGET_TOKENS,
        "the budget still holds: {} tokens\n{c}",
        estimate_tokens(&c)
    );
    c
}

// ------------------------------------------------------------ the categories

/// ACTIVE_FAILURE, and root cause 3: the exact failing test survives the ladder.
#[test]
fn exact_failing_test_identifier_is_retained_under_budget_pressure() {
    let mut log = Log::new();
    busy_session(&mut log);
    let c = capsule(&mut log);
    assert!(
        c.contains("FAIL tests/test_codec.py::test_chunk_size_is_hex"),
        "the failing test by its exact id:\n{c}"
    );
}

/// NEXT_ACTION, and root cause 1: the directive survives a sign-off.
#[test]
fn exact_next_action_identifier_survives_a_later_sign_off() {
    let mut log = Log::new();
    busy_session(&mut log);
    log.prompt("Next, look at parse_chunk_size in src/http/codec.rs. Don't change it yet.");
    log.prompt("I have to go now. Leave everything as it is and stop here.");
    let c = capsule(&mut log);
    assert!(c.contains("[EARLIER_MESSAGE]"), "{c}");
    assert!(c.contains("parse_chunk_size"), "the next function:\n{c}");
}

/// CURRENT_TASK, and root cause 2: a test the session fixed keeps its identity
/// and its truthful status after a newer run of the same command fails on
/// something else.
#[test]
fn a_fixed_target_test_keeps_its_exact_identity_and_status() {
    let mut log = Log::new();
    log.env.write_file("src/http/codec.rs", "int(s)\n");
    log.prompt("fix the chunked-body parsing bug in the HTTP codec, nothing else");
    log.command_fail(
        "python -m pytest tests/ -q",
        1,
        "FAILED tests/test_codec.py::test_chunk_size_is_hex - AssertionError\n\
         FAILED tests/test_limits.py::test_large_header_accepted - assert 413 == 200\n\
         2 failed, 30 passed",
    );
    log.edit("src/http/codec.rs", "int(s, 16)\n");
    log.command_ok(
        "python -m pytest tests/test_codec.py -q",
        "5 passed in 0.02s",
    );
    // The same command as the first run fails again -- on the other test only.
    log.command_fail(
        "python -m pytest tests/ -q",
        1,
        "FAILED tests/test_limits.py::test_large_header_accepted - assert 413 == 200\n\
         1 failed, 31 passed",
    );
    let snap = log.snapshot();
    let fixed = snap
        .tests
        .iter()
        .find(|t| t.id == "tests/test_codec.py::test_chunk_size_is_hex")
        .expect("the fixed test is still tracked");
    assert!(!fixed.failing, "{fixed:?}");
    let c = log.capsule();
    assert!(
        c.contains("PASS tests/test_codec.py::test_chunk_size_is_hex | failed"),
        "{c}"
    );
    assert!(
        c.contains("FAIL tests/test_limits.py::test_large_header_accepted"),
        "{c}"
    );
    // Focus: the test the session worked on ranks first.
    assert_eq!(
        snap.tests[0].id,
        "tests/test_codec.py::test_chunk_size_is_hex"
    );
}

/// RELEVANT_FILES: the file the task is about is named, whatever else goes.
#[test]
fn exact_relevant_file_is_retained() {
    let mut log = Log::new();
    busy_session(&mut log);
    let c = capsule(&mut log);
    assert!(
        c.contains("src/http/codec.rs") || c.contains("tests/test_codec.py"),
        "{c}"
    );
    assert!(c.contains("src/http/limits.rs"), "the reverted file:\n{c}");
}

/// CURRENT_TASK: the objective is carried verbatim at the head.
#[test]
fn exact_current_task_is_retained() {
    let mut log = Log::new();
    busy_session(&mut log);
    let c = capsule(&mut log);
    assert!(c.contains("[FIRST_MESSAGE]"), "{c}");
    assert!(
        c.contains("The HTTP codec rejects valid chunked bodies"),
        "{c}"
    );
}

/// STATED_CONSTRAINTS: the user's rules survive the ladder.
#[test]
fn exact_constraints_are_retained() {
    let mut log = Log::new();
    busy_session(&mut log);
    let c = capsule(&mut log);
    assert!(c.contains("Never raise MAX globally"), "{c}");
}

/// REJECTED_APPROACHES: the reverted attempts are listed by file.
#[test]
fn dead_ends_are_retained() {
    let mut log = Log::new();
    busy_session(&mut log);
    let c = capsule(&mut log);
    assert!(c.contains("[REVERTED_EDITS]"), "{c}");
    assert!(
        c.contains(
            "src/http/limits.rs | 1 edit(s) | reverted via `git restore src/http/limits.rs`"
        ),
        "{c}"
    );
}

/// Several reverts of one file share one header instead of repeating it.
#[test]
fn repeated_reverts_of_one_file_share_a_line() {
    let mut log = Log::new();
    log.env.write_file("src/window.py", "a <= x <= b\n");
    log.prompt("fix the window boundary so month-end rows are counted once");
    for attempt in ["a - 1 <= x <= b + 1\n", "a <= x < b\n", "a < x <= b\n"] {
        log.edit("src/window.py", attempt);
        log.edit("src/window.py", "a <= x <= b\n");
    }
    let c = log.capsule_at(900);
    let headers = c
        .lines()
        .skip_while(|l| !l.starts_with("[REVERTED_EDITS]"))
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .filter(|l| l.starts_with("- src/window.py |"))
        .count();
    assert_eq!(headers, 1, "one header for the file:\n{c}");
    assert!(c.contains("3 reverts, 3 edit(s)"), "{c}");
}

// ----------------------------------------------------------- full pipeline

/// Source events -> ledger -> snapshot -> restore -> staged capsule, offline,
/// with every category's exact identifier checked at the far end and the
/// diagnostic confirming no layer lost them.
#[test]
fn full_offline_pipeline_carries_every_exact_identifier() {
    let mut log = Log::new();
    busy_session(&mut log);
    log.prompt("Next, look at parse_chunk_size in src/http/codec.rs. Don't change it yet.");
    log.prompt("That's all for today, stop here.");
    log.reduce();

    let ws = log.env.project_id();
    let session = log.session();
    let staged = restore::build(
        &log.db.conn,
        &RestoreRequest {
            workspace_id: &ws,
            workspace_root: "/workspace",
            source_session_id: &session,
            now_ms: log.ts + 10_000,
        },
        &DEFAULT,
    )
    .expect("restore");
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("staged");
    velra_core::staging::stage(&path, &staged).expect("stage");
    let on_disk = velra_core::staging::peek(&path).expect("staged record");
    assert_eq!(on_disk.capsule, staged.capsule);

    let markers = [
        "The HTTP codec rejects valid chunked bodies", // CURRENT_TASK
        "tests/test_codec.py::test_chunk_size_is_hex", // ACTIVE_FAILURE
        "parse_chunk_size",                            // NEXT_ACTION
        "src/http/limits.rs",                          // DEAD_END / file
        "Never raise MAX globally",                    // CONSTRAINT
    ];
    for m in markers {
        assert!(
            on_disk.capsule.contains(m),
            "{m} missing:\n{}",
            on_disk.capsule
        );
    }
    assert!(on_disk.tokens <= DEFAULT_BUDGET_TOKENS);

    let meta = SnapshotMeta {
        checkpoint_id: "restore".into(),
        created_ms: log.ts + 10_000,
        trigger: Trigger::Cli,
        partial: false,
        preview: false,
        tz_offset_secs: 0,
    };
    let inputs = TraceInputs {
        meta: &meta,
        cfg: &DEFAULT,
        staged: Some(&on_disk.capsule),
    };
    for m in markers {
        let t = provenance::trace_marker(&log.db.conn, &session, &inputs, m).expect("trace");
        assert_eq!(t.first_loss, None, "{}", t.report());
        let capsule = t
            .layers
            .iter()
            .find(|l| l.layer == "capsule")
            .expect("layer");
        assert_eq!(capsule.presence, Presence::Present, "{}", t.report());
    }
}

// ------------------------------------------------------------- diagnostic

/// A marker only in a superseded message, when the latest message already
/// names code, is lost at the snapshot -- and the diagnostic says why.
#[test]
fn the_diagnostic_names_a_superseded_intent() {
    let mut log = Log::new();
    log.prompt("migrate the settings loader to the new config format");
    log.prompt("first check whether legacy_toml_shim is still imported anywhere");
    log.prompt("now update src/config/loader.rs to read the new keys");
    let session = log.session();
    log.reduce();
    let meta = SnapshotMeta {
        checkpoint_id: "t".into(),
        created_ms: log.ts + 1_000,
        trigger: Trigger::Cli,
        partial: false,
        preview: false,
        tz_offset_secs: 0,
    };
    let inputs = TraceInputs {
        meta: &meta,
        cfg: &DEFAULT,
        staged: None,
    };
    let t = provenance::trace_marker(&log.db.conn, &session, &inputs, "legacy_toml_shim")
        .expect("trace");
    assert_eq!(t.first_loss, Some("snapshot"), "{}", t.report());
    let reason = t.reason.clone().unwrap_or_default();
    assert!(reason.contains("superseded"), "{}", t.report());
    let capsule = t
        .layers
        .iter()
        .find(|l| l.layer == "capsule")
        .expect("layer");
    assert_eq!(capsule.presence, Presence::Unavailable);
}

/// A marker the ladder removes is reported at the renderer with its rung.
#[test]
fn the_diagnostic_names_the_ladder_rung_that_removed_a_line() {
    let mut log = Log::new();
    busy_session(&mut log);
    let session = log.session();
    log.reduce();
    let meta = SnapshotMeta {
        checkpoint_id: "t".into(),
        created_ms: log.ts + 1_000,
        trigger: Trigger::Cli,
        partial: false,
        preview: false,
        tz_offset_secs: 0,
    };
    let tight = RenderConfig { budget_tokens: 300 };
    let inputs = TraceInputs {
        meta: &meta,
        cfg: &tight,
        staged: None,
    };
    let t = provenance::trace_marker(&log.db.conn, &session, &inputs, "Never raise MAX globally")
        .expect("trace");
    assert_eq!(t.first_loss, Some("renderer"), "{}", t.report());
    assert!(
        t.reason
            .as_deref()
            .unwrap_or("")
            .starts_with("budgeted out: removed by ladder rung `"),
        "{}",
        t.report()
    );
}

// ---------------------------------------------------- noise still discarded

/// The other side of the trade: prioritisation must still discard low-value
/// content. An unrelated read sweep, an edit outside the workspace, and old
/// messages that name nothing do not displace the task's state.
#[test]
fn genuinely_low_value_noise_is_still_discarded() {
    let mut log = Log::new();
    busy_session(&mut log);
    // A live workspace edit, older than the note written after it.
    log.edit("src/http/codec.rs", "fn parse_header() { strict(); }\n");
    let outside = log
        .env
        .project
        .parent()
        .expect("temp dir")
        .join("agent-notes")
        .join("MEMORY.md");
    let outside = outside.to_string_lossy().replace('\\', "/");
    log.edit(&outside, "- remember the codec rule\n");
    log.prompt("Next, look at parse_chunk_size in src/http/codec.rs.");
    let c = capsule(&mut log);
    assert!(
        !c.contains("src/misc/module_"),
        "the read sweep is not carried:\n{c}"
    );
    assert!(
        !c.contains("agent-notes"),
        "the out-of-workspace note is not carried:\n{c}"
    );
    // The latest message names code, so no earlier message is carried.
    assert!(!c.contains("[EARLIER_MESSAGE]"), "{c}");

    // And at full detail the note ranks below every workspace file.
    let snap = log.snapshot();
    let pos = |p: &str| snap.working_files.iter().position(|w| w.path.contains(p));
    if let Some(note) = pos("agent-notes") {
        for w in &snap.working_files[..note] {
            assert!(!velra_core::snapshot::is_outside_workspace(&w.path));
        }
    }
    assert_eq!(
        snap.attempts.first().map(|a| a.path.as_str()),
        Some("src/http/codec.rs"),
        "the workspace edit leads [RECENT_EDITS] although the note is newer: {:?}",
        snap.attempts
    );
}

/// A sign-off is not an identifier, and a message with no code in it is never
/// promoted over the latest one.
#[test]
fn messages_without_identifiers_are_not_carried_as_earlier_messages() {
    let mut log = Log::new();
    log.prompt("fix the flaky upload test in the storage client please");
    log.prompt("can you take another look, something still feels off");
    log.prompt("thanks, that is enough for today");
    let snap = log.snapshot();
    assert_eq!(snap.earlier, None, "{:?}", snap.earlier);
}

/// A whole-suite pass clears every tracked failure; a narrowed pass does not.
#[test]
fn only_a_run_that_covers_a_test_changes_its_status() {
    let mut log = Log::new();
    log.prompt("get the storage client tests green again");
    log.command_fail(
        "pytest -q",
        1,
        "FAILED tests/test_upload.py::test_retry - boom\n1 failed, 9 passed",
    );
    log.command_ok("pytest -q -k download", "3 passed, 7 deselected");
    assert!(log.snapshot().tests[0].failing, "a -k run that excludes it");
    log.command_ok("pytest tests/test_download.py", "3 passed");
    assert!(log.snapshot().tests[0].failing, "a run of another file");
    log.command_ok("pytest -q", "10 passed");
    assert!(!log.snapshot().tests[0].failing, "the whole suite passed");
}
