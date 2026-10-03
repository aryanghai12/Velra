//! Snapshot selection under pressure (Phase 4 hardening).
//!
//! Given correct reduced state, does `snapshot::build` pick the state a
//! continuation needs? Every fixture here is crowded on purpose -- several
//! dead ends, noisy files, competing failures, more rules than slots -- because
//! a selection rule that is only ever shown one candidate is not tested.
//!
//! These tests pin selected *state*. What the renderer prints from it, and
//! whether a model understands it, are other layers.

mod common;

use common::Log;
use velra_core::render::Snapshot;

fn texts(v: &[velra_core::render::ConstraintView]) -> Vec<&str> {
    v.iter().map(|c| c.text.as_str()).collect()
}

/// An absolute, `/`-separated path outside the project, as the hook stores
/// one (an agent's auto-memory note, a global config file).
fn outside(log: &Log, rel: &str) -> String {
    let p = log.env.dir.path().join("claude-home").join(rel);
    velra_core::paths::normalize_abs(&p.to_string_lossy())
}

fn working(s: &Snapshot) -> Vec<&str> {
    s.working_files.iter().map(|w| w.path.as_str()).collect()
}

// ------------------------------------------------------ 1. dead-end evidence

/// A dead end of two edits where the first is an intermediate step the second
/// replaced: the evidence shown is the change that was finally rejected.
#[test]
fn a_dead_ends_excerpt_is_the_change_that_was_rejected() {
    let mut log = Log::new();
    log.env
        .write_file("src/money.py", "rounding = ROUND_HALF_UP\n");
    log.prompt("the gap is one cent, so start with the rounding hypothesis");
    log.edit("src/money.py", "rounding = ROUND_HALF_UP  # ?\n");
    log.edit("src/money.py", "rounding = ROUND_HALF_EVEN\n");
    log.git_restore(
        "git restore src/money.py",
        &[("src/money.py", "rounding = ROUND_HALF_UP\n")],
    );
    let snap = log.snapshot();
    let d = &snap.dead_ends[0];
    assert_eq!(
        d.plus.as_deref(),
        Some("rounding = ROUND_HALF_EVEN"),
        "{d:?}"
    );
    assert_eq!(
        d.minus.as_deref(),
        Some("rounding = ROUND_HALF_UP"),
        "{d:?}"
    );
    assert_eq!(d.excerpt_edit, Some(d.edit_ids[1]), "{d:?}");
}

/// Two independent edits in one dead end -- the approach, then an unrelated
/// touch-up elsewhere in the file. Neither replaced the other; the first,
/// which carries the approach, is the evidence.
#[test]
fn an_independent_later_edit_does_not_displace_the_approach() {
    let mut log = Log::new();
    log.env.write_file(
        "src/retry.py",
        "def pay():\n    return 1\n\n\"\"\"docs\"\"\"\n",
    );
    log.prompt("try caching the key in src/retry.py");
    log.edit(
        "src/retry.py",
        "_last_key = None\ndef pay():\n    return 1\n\n\"\"\"docs\"\"\"\n",
    );
    log.edit(
        "src/retry.py",
        "_last_key = None\ndef pay():\n    return 1\n\n\"\"\"docs.\"\"\"\n",
    );
    log.git_restore(
        "git restore src/retry.py",
        &[(
            "src/retry.py",
            "def pay():\n    return 1\n\n\"\"\"docs\"\"\"\n",
        )],
    );
    let snap = log.snapshot();
    let d = &snap.dead_ends[0];
    assert_eq!(d.plus.as_deref(), Some("_last_key = None"), "{d:?}");
    assert_eq!(d.excerpt_edit, Some(d.edit_ids[0]));
}

/// One edit: unchanged behaviour.
#[test]
fn a_single_edit_dead_end_shows_that_edit() {
    let mut log = Log::new();
    log.env.write_file("a.py", "x = 0\n");
    log.prompt("set x to one in a.py");
    log.edit("a.py", "x = 1\n");
    log.edit("a.py", "x = 0\n");
    let snap = log.snapshot();
    let d = &snap.dead_ends[0];
    assert_eq!(
        (d.minus.as_deref(), d.plus.as_deref()),
        (Some("x = 0"), Some("x = 1"))
    );
}

/// Four attempts on one file and one on another, with long lines: the other
/// file keeps its slot, each dead end its own evidence, and a long line is
/// carried whole for the renderer to shorten.
#[test]
fn several_dead_ends_keep_their_own_evidence() {
    let long = format!("value = {}", "9".repeat(400));
    let mut log = Log::new();
    log.env.write_file("a.py", "value = 0\n");
    log.env.write_file("b.py", "flag = False\n");
    log.prompt("find a value for a.py that makes the check pass");
    log.edit("b.py", "flag = True\n");
    log.edit("b.py", "flag = False\n");
    for i in 1..=4 {
        let v = if i == 4 {
            long.clone()
        } else {
            format!("value = {i}")
        };
        log.edit("a.py", &format!("{v}\n"));
        log.edit("a.py", "value = 0\n");
    }
    let snap = log.snapshot();
    assert_eq!(snap.dead_end_total, 5);
    assert_eq!(snap.dead_ends.len(), 4);
    assert!(snap.dead_ends.iter().any(|d| d.path == "b.py"));
    let pluses: Vec<&str> = snap
        .dead_ends
        .iter()
        .filter(|d| d.path == "a.py")
        .filter_map(|d| d.plus.as_deref())
        .collect();
    assert!(pluses.contains(&long.as_str()), "{pluses:?}");
    assert!(snap
        .dead_ends
        .iter()
        .all(|d| d.excerpt_edit.is_some_and(|e| d.edit_ids.contains(&e))));
}

// ---------------------------------------------------------- 3. constraints

/// More rules than slots, stated in order: four requirements first, then a
/// prohibition in a later turn. The prohibition -- the rule a continuation can
/// break -- is kept; the carried rules stay in the order they were stated.
#[test]
fn a_later_prohibition_is_not_lost_to_earlier_requirements() {
    let mut log = Log::new();
    log.prompt(
        "Refactor the payment module in src/payments/retry.py.\n\n\
         - You must keep the public API stable.\n\
         - You must use the existing logger.\n\
         - You must keep type hints on every function.\n\
         - You must run the suite before finishing.",
    );
    log.prompt("Also, do not modify the tests.");
    let snap = log.snapshot();
    assert!(snap.constraint_total >= 4, "{:?}", texts(&snap.constraints));
    let kept = texts(&snap.constraints);
    assert!(kept.contains(&"Also, do not modify the tests."), "{kept:?}");
    assert_eq!(kept.len(), 3);
    assert!(
        snap.constraints.windows(2).all(|w| w[0].id < w[1].id),
        "chronological: {kept:?}"
    );
}

/// The same rule restated with other capitalisation or punctuation is one
/// rule, carried once, as first stated. (A restatement with words added --
/// "Remember: ..." -- stays its own rule: added words can be an exception, and
/// telling the two apart is not a lexical question.)
#[test]
fn a_restated_rule_takes_one_slot() {
    let mut log = Log::new();
    log.prompt("Fix the failing test in tests/test_retry.py. Do not modify the tests.");
    log.prompt("Do NOT modify the tests!");
    log.prompt("You must keep the public API stable.");
    let snap = log.snapshot();
    let kept = texts(&snap.constraints);
    let about_tests = kept
        .iter()
        .filter(|t| t.to_lowercase().contains("modify the tests"))
        .count();
    assert_eq!(about_tests, 1, "{kept:?}");
    assert_eq!(snap.constraint_total, 2, "{kept:?}");
    assert!(kept.contains(&"Do not modify the tests."), "{kept:?}");
    assert!(
        kept.contains(&"You must keep the public API stable."),
        "{kept:?}"
    );
}

// -------------------------------------------------------------- 4. failures

// The tail pytest prints for a failing assertion: the `E` lines, the
// location line, the short summary.
const FAIL_OUT: &str = "    def test_retry_keeps_key():\n\
    >       assert pay() == 1\n\
    E   AssertionError: key changed\n\
    \n\
    tests/test_retry.py:12: AssertionError\n\
    =========================== short test summary info ============================\n\
    FAILED tests/test_retry.py::test_retry_keeps_key - AssertionError: key changed\n\
    1 failed in 0.1s";

/// A focused failure, then a full-suite run that passes -- it ran the failing
/// test too. The failure is closed, as the test statuses already say.
#[test]
fn a_covering_pass_closes_the_failure() {
    let mut log = Log::new();
    log.env
        .write_file("tests/test_retry.py", "def test_retry_keeps_key(): ...\n");
    log.prompt("make tests/test_retry.py pass");
    log.command_fail(
        "python -m pytest tests/test_retry.py::test_retry_keeps_key",
        1,
        FAIL_OUT,
    );
    log.command_ok("python -m pytest", "12 passed in 0.4s");
    let snap = log.snapshot();
    assert!(snap.tests.iter().all(|t| !t.failing), "{:?}", snap.tests);
    assert!(snap.failure.is_none(), "stale failure: {:?}", snap.failure);
    assert_eq!(snap.failing_count, 0);
}

/// A pass that did not run the failing test leaves it open.
#[test]
fn a_pass_that_does_not_cover_the_test_leaves_the_failure_open() {
    let mut log = Log::new();
    log.env
        .write_file("tests/test_retry.py", "def test_retry_keeps_key(): ...\n");
    log.prompt("make tests/test_retry.py pass");
    log.command_fail("python -m pytest tests/test_retry.py", 1, FAIL_OUT);
    log.command_ok("python -m pytest tests/test_other.py", "3 passed in 0.1s");
    let snap = log.snapshot();
    let f = snap.failure.as_ref().expect("still failing");
    assert!(f.command.contains("tests/test_retry.py"), "{f:?}");
    assert_eq!(snap.failing_count, 1);
    assert!(snap
        .tests
        .iter()
        .any(|t| t.failing && t.id == "tests/test_retry.py::test_retry_keeps_key"));
}

/// The same test failing three times is one failure; the exact identifier,
/// its reason and the latest run are kept.
#[test]
fn a_repeated_failure_is_one_failure_with_its_exact_id() {
    let mut log = Log::new();
    log.env
        .write_file("tests/test_retry.py", "def test_retry_keeps_key(): ...\n");
    log.prompt("make tests/test_retry.py pass");
    for _ in 0..3 {
        log.command_fail("python -m pytest tests/test_retry.py", 1, FAIL_OUT);
    }
    let snap = log.snapshot();
    assert_eq!(snap.failing_count, 1);
    let t = &snap.tests[0];
    assert_eq!(t.id, "tests/test_retry.py::test_retry_keeps_key");
    assert!(t.failing);
    assert_eq!(t.detail.as_deref(), Some("AssertionError: key changed"));
    assert_eq!(
        snap.next_target.as_ref().map(|n| n.target.as_str()),
        Some("tests/test_retry.py:12")
    );
}

// --------------------------------------------------------- 5. working files

/// Two task files, each read once and one edited, against a sweep of
/// dependency and generated files read over and over. The task files are
/// kept; the noise ranks below every workspace source file.
#[test]
fn dependency_and_generated_reads_do_not_displace_task_files() {
    let mut log = Log::new();
    log.env.write_file("src/retry.py", "x = 0\n");
    log.env
        .write_file("tests/test_retry.py", "def test(): ...\n");
    log.prompt("fix the retry logic in src/retry.py");
    log.read("tests/test_retry.py");
    log.edit("src/retry.py", "x = 1\n");
    let noise = [
        "node_modules/lodash/index.js",
        "node_modules/react/index.js",
        ".venv/lib/python3.12/site-packages/requests/api.py",
        ".venv/lib/python3.12/site-packages/urllib3/util.py",
        "target/debug/build/out.rs",
        "target/release/deps/lib.d",
        "src/__pycache__/retry.cpython-312.pyc",
        ".pytest_cache/v/cache/lastfailed",
        "node_modules/a/b.js",
        "node_modules/c/d.js",
    ];
    for p in noise {
        log.env.write_file(p, "noise\n");
        for _ in 0..3 {
            log.read(p);
        }
    }
    let snap = log.snapshot();
    let files = working(&snap);
    assert_eq!(
        files[..2],
        ["src/retry.py", "tests/test_retry.py"],
        "{files:?}"
    );
}

/// Many unrelated modules each read once after the task files: first touch
/// wins the tie, so the files the session opened with stay (D59, under a
/// larger sweep than the existing fixture's).
#[test]
fn a_late_read_sweep_does_not_displace_the_first_files() {
    let mut log = Log::new();
    log.env.write_file("src/retry.py", "x = 0\n");
    log.env
        .write_file("tests/test_retry.py", "def test(): ...\n");
    log.prompt("find why tests/test_retry.py fails");
    log.read("tests/test_retry.py");
    log.read("src/retry.py");
    for i in 0..60 {
        let p = format!("src/unrelated/mod_{i:02}.py");
        log.env.write_file(&p, "pass\n");
        log.read(&p);
    }
    let snap = log.snapshot();
    let files = working(&snap);
    assert!(
        files.contains(&"src/retry.py") && files.contains(&"tests/test_retry.py"),
        "{files:?}"
    );
}

// ----------------------------------------------------------- 6. next target

/// The last edit of the session was an auto-memory note outside the
/// workspace. The next target is still the workspace file being changed.
#[test]
fn the_next_target_is_not_an_auto_memory_note() {
    let mut log = Log::new();
    log.env.write_file("src/retry.py", "x = 0\n");
    log.prompt("fix the retry logic in src/retry.py");
    log.edit("src/retry.py", "x = 1\n");
    let note = outside(&log, "projects/p/memory/retry_notes.md");
    log.write_tool(&note, "- retry keeps the key\n");
    let snap = log.snapshot();
    let t = snap.next_target.as_ref().expect("target");
    assert_eq!(t.target, "src/retry.py", "{t:?}");
    assert_eq!(t.rule, "last-active-edit");
}

/// With nothing in the workspace, an outside edit is still the best there is.
#[test]
fn an_outside_edit_is_a_target_only_when_nothing_else_is() {
    let mut log = Log::new();
    log.prompt("update my global notes about the retry work");
    let note = outside(&log, "projects/p/memory/retry_notes.md");
    log.write_tool(&note, "- retry keeps the key\n");
    let snap = log.snapshot();
    assert_eq!(
        snap.next_target.as_ref().map(|t| t.target.as_str()),
        Some(note.as_str())
    );
}

/// A dependency patched late does not outrank the task's own file.
#[test]
fn the_next_target_prefers_the_projects_own_file_over_a_dependency() {
    let mut log = Log::new();
    log.env.write_file("src/retry.py", "x = 0\n");
    log.env.write_file("node_modules/lib/index.js", "a\n");
    log.prompt("fix the retry logic in src/retry.py");
    log.edit("src/retry.py", "x = 1\n");
    log.edit("node_modules/lib/index.js", "b\n");
    let snap = log.snapshot();
    assert_eq!(
        snap.next_target.as_ref().map(|t| t.target.as_str()),
        Some("src/retry.py")
    );
}

// ------------------------------------------------------------ 9. determinism

/// The same logical session builds the same snapshot every time: every
/// ordering the selection uses ends in an explicit tie-break.
#[test]
fn identical_state_builds_identical_snapshots() {
    let build = || {
        let mut log = Log::new();
        log.env.write_file("a.py", "x = 0\n");
        log.env.write_file("b.py", "y = 0\n");
        log.prompt("try the change in a.py and b.py");
        log.edit("a.py", "x = 1\n");
        log.edit("b.py", "y = 1\n");
        log.edit("a.py", "x = 0\n");
        log.edit("b.py", "y = 0\n");
        for p in ["c.py", "d.py", "e.py"] {
            log.env.write_file(p, "z\n");
            log.read(p);
        }
        let mut snap = log.snapshot();
        snap.checkpoint_id.clear();
        snap.project_id.clear();
        snap.session_id.clear();
        snap.git = None;
        snap
    };
    let a = build();
    let b = build();
    assert_eq!(a, b);
}

// ------------------------------------------------- 8. the payment-retry task

const TEST_ID: &str = "tests/test_retry.py::test_retry_preserves_idempotency_key";
const RETRY: &str = "src/payments/retry.py";
const ORIGINAL: &str =
    "def build_retry_payload(request: dict) -> dict:\n    return {\"amount\": request[\"amount\"]}\n";
const PATCHED: &str = "_last_idempotency_key = None\n\ndef build_retry_payload(request: dict) -> dict:\n    global _last_idempotency_key\n    return {\"amount\": request[\"amount\"]}\n";
const PYTEST_FAIL: &str =
    "tests/test_retry.py::test_retry_preserves_idempotency_key FAILED         [ 50%]\n\
\n\
================================== FAILURES ===================================\n\
    def test_retry_preserves_idempotency_key():\n\
>       assert payload[\"idempotency_key\"] == \"idem-7f31\"\n\
E       KeyError: 'idempotency_key'\n\
\n\
tests/test_retry.py:18: KeyError\n\
=========================== short test summary info ===========================\n\
FAILED tests/test_retry.py::test_retry_preserves_idempotency_key - KeyError: 'idempotency_key'\n\
========================= 1 failed, 1 passed in 0.05s =========================";
/// The rejection clause lies past character 240 of the first message.
const LONG_TASK: &str =
    "We have a failing payment retry test in tests/test_retry.py and the on-call \
    team has asked for a careful, minimal change. Work on the task, but intentionally leave it \
    unfinished for a later continuation, because the reviewer wants to see the investigation \
    before the final patch lands. First inspect the failure and understand the code. Then try a \
    temporary workaround that stores the idempotency key in a module-level variable in \
    src/payments/retry.py. Run the tests. That workaround is considered a rejected approach for \
    this task, so revert it with git restore src/payments/retry.py, rerun the tests, and stop \
    without implementing the final fix. Do not modify the tests.";

/// The payment-retry session under pressure -- a read sweep through
/// dependencies and unrelated modules, unrelated live edits, an auto-memory
/// note written last, more rules, repeated failures -- keeps every piece of
/// state the continuation needs, selected from the right evidence.
#[test]
fn the_payment_retry_state_survives_a_crowded_session() {
    let mut log = Log::new();
    log.env.write_file(RETRY, ORIGINAL);
    log.env.write_file(
        "tests/test_retry.py",
        "def test_retry_preserves_idempotency_key(): ...\n",
    );
    log.prompt(LONG_TASK);
    log.read(RETRY);
    log.read("tests/test_retry.py");
    log.command_fail("python -m pytest tests/test_retry.py -v", 1, PYTEST_FAIL);
    log.edit(RETRY, PATCHED);
    log.command_ok(
        "python -m pytest tests/test_retry.py -v",
        "tests/test_retry.py::test_retry_preserves_idempotency_key PASSED\n2 passed in 0.03s",
    );
    log.git_restore_failed(
        "git restore src/payments/retry.py && python -m pytest tests/test_retry.py -v",
        1,
        PYTEST_FAIL,
        &[(RETRY, ORIGINAL)],
    );
    // Pressure.
    for i in 0..30 {
        let p = format!("node_modules/pkg{i}/index.js");
        log.env.write_file(&p, "x\n");
        log.read(&p);
        log.read(&p);
    }
    for i in 0..20 {
        let p = format!("src/unrelated/m{i:02}.py");
        log.env.write_file(&p, "x\n");
        log.read(&p);
    }
    log.prompt("You must keep the public API stable. You must use the existing logger.");
    let note = outside(&log, "projects/p/memory/payment_notes.md");
    log.write_tool(&note, "- tried a module-level key, reverted\n");

    let snap = log.snapshot();
    // Objective: the user's task.
    assert!(
        snap.root
            .as_ref()
            .is_some_and(|r| r.text.starts_with("We have a failing payment retry test")),
        "{:?}",
        snap.root
    );
    // Failure and exact test id.
    let f = snap.failure.as_ref().expect("failure");
    assert!(f.command.contains("pytest tests/test_retry.py"), "{f:?}");
    assert!(
        snap.tests.iter().any(|t| t.id == TEST_ID && t.failing),
        "{:?}",
        snap.tests
    );
    // The rule, kept over the later requirements.
    assert!(
        snap.constraints
            .iter()
            .any(|c| c.text == "Do not modify the tests."),
        "{:?}",
        texts(&snap.constraints)
    );
    // The user's rejection, beyond the first message's excerpt.
    assert!(
        snap.rejections
            .iter()
            .any(|r| r.text.contains("module-level variable")
                && r.text.contains("rejected approach")),
        "{:?}",
        snap.rejections
    );
    // The rejected route and its evidence.
    let d = snap
        .dead_ends
        .iter()
        .find(|d| d.path == RETRY)
        .expect("dead end");
    assert_eq!(
        d.plus.as_deref(),
        Some("_last_idempotency_key = None"),
        "{d:?}"
    );
    // Relevant files ahead of the sweep.
    let files = working(&snap);
    let mut top: Vec<&str> = files[..2].to_vec();
    top.sort_unstable();
    assert_eq!(top, [RETRY, "tests/test_retry.py"], "{files:?}");
    assert!(
        !files.iter().any(|f| f.starts_with("node_modules/")),
        "{files:?}"
    );
    // Next target: where the test fails, not the note.
    let t = snap.next_target.as_ref().expect("target");
    assert_eq!(t.target, "tests/test_retry.py:18", "{t:?}");
}

// ------------------------------------------------------------ 2. intents

/// A task, a subtask, dozens of background notifications and an IDE-only
/// prompt, then a real request: the snapshot's objective, subtask and latest
/// message are the user's.
#[test]
fn notifications_and_injected_prompts_do_not_move_the_intent() {
    let mut log = Log::new();
    log.prompt("Fix the retry logic so tests/test_retry.py passes without changing the tests.");
    log.prompt("subtask: first reproduce the failure with pytest -x");
    for i in 0..30 {
        log.prompt(&format!(
            "<task-notification><task-id>t{i}</task-id><status>completed</status>\
             <summary>Background build {i} finished</summary></task-notification>"
        ));
    }
    log.prompt("now check whether build_retry_payload copies the key");
    log.prompt(
        "<ide_opened_file>The user opened the file c:\\x\\readme.md in the IDE. This may or \
         may not be related to the current task.</ide_opened_file>",
    );
    let snap = log.snapshot();
    assert!(snap
        .root
        .as_ref()
        .is_some_and(|r| r.text.starts_with("Fix the retry logic")));
    assert!(snap
        .subtask
        .as_ref()
        .is_some_and(|s| s.text.contains("reproduce the failure")));
    assert_eq!(
        snap.latest.as_ref().map(|l| l.text.as_str()),
        Some("now check whether build_retry_payload copies the key")
    );
}

/// `task:` starts a new epoch: nothing selected from the previous task --
/// its rules, dead ends, failures -- is carried into the new one.
#[test]
fn a_new_task_does_not_inherit_the_previous_tasks_state() {
    let mut log = Log::new();
    log.env.write_file("a.py", "x = 0\n");
    log.prompt("Make a.py return one. Do not touch the CLI.");
    log.edit("a.py", "x = 1\n");
    log.edit("a.py", "x = 0\n");
    log.command_fail(
        "python -m pytest tests/test_a.py",
        1,
        "FAILED tests/test_a.py::test_a - x\n1 failed",
    );
    log.prompt("task: write the release notes for v2 in docs/RELEASE.md");
    let snap = log.snapshot();
    assert!(snap
        .root
        .as_ref()
        .is_some_and(|r| r.text.contains("release notes")));
    assert!(
        snap.constraints.is_empty(),
        "{:?}",
        texts(&snap.constraints)
    );
    assert_eq!(snap.constraint_total, 0);
    assert!(snap.dead_ends.is_empty());
    assert!(snap.failure.is_none());
    assert!(snap.tests.is_empty());
}

// ---------------------------------------------- 9. ingestion-order invariance

/// The semantic content of a snapshot, without row ids.
fn semantic(s: &Snapshot) -> String {
    let d: Vec<_> = s
        .dead_ends
        .iter()
        .map(|d| (&d.path, &d.minus, &d.plus, d.mechanism, d.edit_ids.len()))
        .collect();
    let t: Vec<_> = s.tests.iter().map(|t| (&t.id, t.failing)).collect();
    format!(
        "root={:?}\nsub={:?}\nlatest={:?}\nrules={:?}/{}\nrej={:?}\nfail={:?}\ntests={t:?}\n\
         dead={d:?}/{}\nattempts={:?}\nfiles={:?}\nnext={:?}",
        s.root.as_ref().map(|r| &r.text),
        s.subtask.as_ref().map(|r| &r.text),
        s.latest.as_ref().map(|r| &r.text),
        texts(&s.constraints),
        s.constraint_total,
        texts(&s.rejections),
        s.failure.as_ref().map(|f| (&f.command, &f.excerpt)),
        s.dead_end_total,
        s.attempts.iter().map(|a| &a.path).collect::<Vec<_>>(),
        working(s),
        s.next_target.as_ref().map(|n| (n.rule, &n.target)),
    )
}

/// One session, recorded twice: in order, and with its reverting edit and a
/// rule-bearing prompt spooled and ingested last. The snapshots select the
/// same state.
#[test]
fn a_snapshot_does_not_depend_on_ingestion_order() {
    fn session(late: bool) -> String {
        let mut log = Log::new();
        log.env.write_file("a.py", "x = 0\n");
        log.env.write_file("b.py", "y = 0\n");
        log.prompt("Make a.py and b.py agree on the value.");
        log.edit("a.py", "x = 1\n");
        log.edit("b.py", "y = 1\n");
        let rule = velra_core::event::Payload {
            prompt: Some("Do not rename either module.".into()),
            ..Default::default()
        };
        let pre = log.observe(&["a.py"])[0].clone();
        log.env.write_file("a.py", "x = 0\n");
        let post = log.observe(&["a.py"])[0].clone();
        let undo_pre = velra_core::event::Payload {
            path: Some("a.py".into()),
            pre_hash: Some(pre.hash),
            ..Default::default()
        };
        let undo_post = velra_core::event::Payload {
            path: Some("a.py".into()),
            post_hash: Some(post.hash),
            excerpt: Some("- x = 1\n+ x = 0".into()),
            ..Default::default()
        };
        let at = log.ts;
        if late {
            log.read("b.py");
            log.append_late("UserPromptSubmit", None, rule, at + 100);
            log.append_late("PreToolUse", Some("Edit"), undo_pre, at + 200);
            log.append_late("PostToolUse", Some("Edit"), undo_post, at + 300);
        } else {
            log.ts = at;
            log.append("UserPromptSubmit", None, rule);
            log.append("PreToolUse", Some("Edit"), undo_pre);
            log.append("PostToolUse", Some("Edit"), undo_post);
            log.read("b.py");
        }
        semantic(&log.snapshot())
    }
    assert_eq!(session(true), session(false));
}
