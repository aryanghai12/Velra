//! F1–F7: reverts, discards, commits, reapplication and turn-end scans (§13).

mod common;

use common::Log;
use velra_core::model::EditStatus;

fn statuses(log: &Log) -> Vec<String> {
    log.edits()
        .into_iter()
        .map(|(_, status, _)| status)
        .collect()
}

#[test]
fn f1_inverse_edit_creates_one_dead_end() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "fn a() { 1 }\n");
    log.prompt("make the function return two instead of one");
    log.edit("src/a.rs", "fn a() { 2 }\n");
    log.edit("src/a.rs", "fn a() { 1 }\n");
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 1, "exactly one dead end: {dead_ends:?}");
    assert_eq!(dead_ends[0].0, "src/a.rs");
    assert_eq!(dead_ends[0].1, "inverse_edit");
    assert_eq!(
        statuses(&log),
        vec![EditStatus::Reverted.as_str(), EditStatus::Active.as_str()]
    );
}

#[test]
fn f2_git_restore_discards_edits_and_records_the_command() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "original\n");
    log.prompt("fix the failing assertion in the auth module");
    log.edit("src/a.rs", "attempt one\n");
    log.git_restore("git restore src/a.rs", &[("src/a.rs", "original\n")]);
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 1, "{dead_ends:?}");
    assert_eq!(dead_ends[0].1, "git_command");
    assert_eq!(dead_ends[0].2.as_deref(), Some("git restore src/a.rs"));
    assert_eq!(statuses(&log), vec![EditStatus::Discarded.as_str()]);
}

#[test]
fn f3_git_reset_hard_discards_every_touched_file() {
    let mut log = Log::new();
    log.env.write_file("a.rs", "a0\n");
    log.env.write_file("b.rs", "b0\n");
    log.prompt("try the alternative approach across both modules");
    log.edit("a.rs", "a1\n");
    log.edit("b.rs", "b1\n");
    log.git_restore(
        "git reset --hard HEAD",
        &[("a.rs", "a0\n"), ("b.rs", "b0\n")],
    );
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 2, "{dead_ends:?}");
    assert!(dead_ends.iter().all(|d| d.1 == "git_command"));
    assert_eq!(statuses(&log), vec![EditStatus::Discarded.as_str(); 2]);
}

#[test]
fn f3b_restore_of_another_file_leaves_unrelated_edits_active() {
    let mut log = Log::new();
    log.env.write_file("a.rs", "a0\n");
    log.env.write_file("b.rs", "b0\n");
    log.prompt("change both modules and keep the tests passing");
    log.edit("a.rs", "a1\n");
    log.edit("b.rs", "b1\n");
    // Only b.rs is restored; a.rs is untouched by the command.
    log.git_restore("git restore b.rs", &[("a.rs", ""), ("b.rs", "b0\n")]);
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 1, "{dead_ends:?}");
    assert_eq!(dead_ends[0].0, "b.rs");
    let edits = log.edits();
    assert_eq!(edits[0].1, EditStatus::Active.as_str(), "a.rs stays active");
    assert_eq!(edits[1].1, EditStatus::Discarded.as_str());
}

#[test]
fn f4_commit_marks_edits_committed_without_a_dead_end() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("apply the fix and commit it once the tests pass");
    log.edit("src/a.rs", "v1\n");
    log.git_commit("git commit -am fix", &["src/a.rs"]);
    log.reduce();

    assert!(log.dead_ends().is_empty(), "commits are not dead ends");
    assert_eq!(statuses(&log), vec![EditStatus::Committed.as_str()]);
}

#[test]
fn f5_reapplied_dead_ends_are_excluded_from_the_capsule() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("retry the approach that was reverted earlier today");
    log.edit("src/a.rs", "v1\n");
    log.edit("src/a.rs", "v0\n"); // revert → dead end
    log.reduce();
    assert_eq!(log.dead_ends().len(), 1);

    log.edit("src/a.rs", "v1\n"); // reapply
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 1, "no new dead end for the reapplication");
    assert_eq!(dead_ends[0].3, 1, "marked reapplied");
    assert!(log
        .edits()
        .iter()
        .any(|(_, s, _)| s == EditStatus::Reapplied.as_str()));

    let capsule = log.capsule();
    assert!(
        !capsule.contains("[DEAD_ENDS]"),
        "reapplied dead ends are not rendered:\n{capsule}"
    );
}

#[test]
fn f6_external_change_is_recorded_by_the_turn_end_scan() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("update the module and run the formatter afterwards");
    log.edit("src/a.rs", "v1\n");
    log.reduce();

    // Something outside Claude Code rewrites the file (sed, a formatter, the user).
    log.env.write_file("src/a.rs", "v2-formatted\n");
    log.stop();
    log.reduce();

    let sources: Vec<String> = log
        .versions("src/a.rs")
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert!(
        sources.contains(&"turn_scan".to_string()),
        "turn scan recorded: {sources:?}"
    );
}

#[test]
fn f6b_turn_scan_detects_an_external_revert() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("make the change; I may undo it in my editor");
    log.edit("src/a.rs", "v1\n");
    log.reduce();

    log.env.write_file("src/a.rs", "v0\n"); // undone outside the agent
    log.stop();
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 1, "{dead_ends:?}");
    assert_eq!(dead_ends[0].1, "external");
}

#[test]
fn f7_test_run_between_attempt_and_revert_is_observed_not_blamed() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("fix the failing auth test without touching the test file");
    log.edit("src/a.rs", "v1\n");
    log.command_fail(
        "pytest tests/test_auth.py",
        1,
        "E   assert False\ntests/test_auth.py:12: AssertionError\n1 failed",
    );
    log.git_restore("git restore src/a.rs", &[("src/a.rs", "v0\n")]);
    log.reduce();

    let capsule = log.capsule();
    assert!(
        capsule.contains("Observed afterward: `pytest tests/test_auth.py` FAIL."),
        "{capsule}"
    );
    assert!(capsule.contains("Causal link: UNCONFIRMED."), "{capsule}");
    for causal in ["caused", "because", "due to", "led to"] {
        assert!(
            !capsule.contains(causal),
            "capsule must not assert causality: {causal}"
        );
    }
}

#[test]
fn commands_are_classified_and_outcomes_tracked() {
    let mut log = Log::new();
    log.prompt("run the suite and the linter to see where we stand");
    log.command_fail(
        "pytest -x",
        1,
        "FAILED tests/test_a.py::test_x\n1 failed, 4 passed",
    );
    log.command_ok("cargo build", "Finished dev target");
    log.command_ok("npm test", "Tests: 12 passed, 12 total");
    log.command_ok("git status", "nothing to commit");
    log.reduce();

    let commands = log.commands();
    assert_eq!(
        commands[0],
        ("test".into(), "FAIL".into(), "pytest -x".into())
    );
    assert_eq!(
        commands[1],
        ("build".into(), "PASS".into(), "cargo build".into())
    );
    assert_eq!(
        commands[2],
        ("test".into(), "PASS".into(), "npm test".into())
    );
    assert_eq!(commands[3].0, "git");
}

#[test]
fn a_later_pass_clears_the_active_failure() {
    let mut log = Log::new();
    log.prompt("fix the failing test in the session module");
    log.command_fail(
        "pytest tests/test_auth.py",
        1,
        "FAILED tests/test_auth.py::test_x\n1 failed",
    );
    let capsule = log.capsule();
    assert!(capsule.contains("[ACTIVE_FAILURE]"), "{capsule}");

    log.command_ok(
        "pytest tests/test_auth.py",
        "test result: ok. 12 passed; 0 failed",
    );
    let capsule = log.capsule();
    assert!(
        !capsule.contains("[ACTIVE_FAILURE]"),
        "a later PASS of the same signature clears it:\n{capsule}"
    );
}

#[test]
fn subagent_edits_are_tagged() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("delegate the refactor of the auth module to a subagent");
    log.edit_as("src/a.rs", "v1\n", "agent-123");
    log.edit_as("src/a.rs", "v0\n", "agent-123");
    log.reduce();

    let capsule = log.capsule();
    assert!(capsule.contains("(subagent)"), "{capsule}");
}

#[test]
fn task_prefix_starts_a_new_epoch() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("fix the flaky logout test in the auth module");
    log.edit("src/a.rs", "v1\n");
    log.reduce();
    assert!(log.capsule().contains("fix the flaky logout test"));

    log.prompt("task: migrate the database to the new schema");
    log.reduce();
    let capsule = log.capsule();
    assert!(capsule.contains("migrate the database"), "{capsule}");
    assert!(
        !capsule.contains("flaky logout"),
        "previous epoch is not rendered:\n{capsule}"
    );
    assert!(
        capsule.contains("0 edits this task"),
        "edit count resets with the epoch:\n{capsule}"
    );
}

#[test]
fn follow_up_prompts_do_not_replace_the_root_objective() {
    let mut log = Log::new();
    log.prompt("fix the flaky logout test in the auth module");
    log.prompt("why did that fail?");
    log.reduce();

    let capsule = log.capsule();
    assert!(capsule.contains("[ROOT_TASK_OBJECTIVE]"));
    assert!(capsule.contains("fix the flaky logout test in the auth module"));
    assert!(capsule.contains("[LATEST_REQUEST]"), "{capsule}");
    assert!(capsule.contains("why did that fail?"), "{capsule}");
}
