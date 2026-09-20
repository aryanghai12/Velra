//! F1–F7: reverts, discards, commits, reapplication and turn-end scans (§13).

mod common;

use common::Log;
use velra_core::event::{GitObservation, Payload};
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
        !capsule.contains("[REVERTED_EDITS]"),
        "reapplied dead ends are not rendered:\n{capsule}"
    );
}

/// The defect that deleted `[REVERTED_EDITS]` from a real capsule, replayed offline.
///
/// In `saturated-velra-r1` of the v0.1 benchmark the agent tried a rounding
/// change, discarded it with `git restore`, and the capsule it received after
/// `/compact` had no `[REVERTED_EDITS]` section at all — the most distinctive thing
/// the product produces, silently gone. The dead end was in the database the
/// whole time; it was filtered out on the way to the page, because
/// `file_versions` held this exact sequence:
///
/// ```text
/// 1 pre_edit  original     4 post_edit  dead-end content
/// 2 post_edit intermediate 5 turn_scan  original      <- the revert, seen
/// 3 pre_edit  intermediate 6 git_pre    dead-end content
///                          7 git_post   original
/// ```
///
/// Row 6 is the `PreToolUse` snapshot taken *before* the restore ran, so it
/// carries the discarded content by construction. Its event could not reach the
/// database at the time and went to the spool, so it was ingested after row 5
/// and looked newer than it. Reapplication matched its hash against the dead
/// end post-edit hashes, marked the dead end `reapplied = 1`, and the renderer
/// selects `WHERE reapplied = 0`.
///
/// No Claude Code session and no network: the sequence is replayed straight
/// into the event log, and `append_late` reproduces the one thing that mattered
/// — an old hook timestamp arriving with a new row id.
#[test]
fn f5c_a_late_git_pre_row_does_not_resurrect_a_dead_end() {
    const ORIGINAL: &str = "rounding = ROUND_HALF_UP\n";
    const INTERMEDIATE: &str = "rounding = ROUND_HALF_UP  # ?\n";
    const DEAD_END: &str = "rounding = ROUND_HALF_EVEN\n";

    let mut log = Log::new();
    log.env.write_file("src/money.py", ORIGINAL);
    log.prompt("the gap is one cent, so start with the rounding hypothesis");

    // Rows 1-4: two edits arriving at the dead-end content.
    log.edit("src/money.py", INTERMEDIATE);
    log.edit("src/money.py", DEAD_END);
    let dead_end_files = log.observe(&["src/money.py"]);
    let restore_ts = log.ts + 1_000;

    // The restore runs. Its `PreToolUse` hook could not open the database and
    // spooled the event, so nothing of it is ingested yet.
    log.env.write_file("src/money.py", ORIGINAL);
    let restored_files = log.observe(&["src/money.py"]);

    // Row 5: the turn-end scan sees the file back at its original content and
    // opens the dead end.
    log.stop();
    log.reduce();
    let dead_ends = log.dead_ends();
    assert_eq!(
        dead_ends.len(),
        1,
        "the turn scan opens a dead end: {dead_ends:?}"
    );
    assert_eq!(dead_ends[0].3, 0, "and it is open");

    // Rows 6-7: the spool is drained. Both rows carry timestamps from before
    // the turn scan and row ids from after it.
    log.append_late(
        "PreToolUse",
        Some("Bash"),
        Payload {
            command: Some("git restore src/money.py".into()),
            git: Some(GitObservation {
                restore: Some("git restore src/money.py".into()),
                commit: false,
                files: dead_end_files,
            }),
            ..Default::default()
        },
        restore_ts,
    );
    log.append_late(
        "PostToolUse",
        Some("Bash"),
        Payload {
            command: Some("git restore src/money.py".into()),
            cwd: Some(log.env.project.to_string_lossy().into_owned()),
            stdout_tail: Some(String::new()),
            git: Some(GitObservation {
                restore: Some("git restore src/money.py".into()),
                commit: false,
                files: restored_files,
            }),
            ..Default::default()
        },
        restore_ts + 1,
    );
    log.reduce();

    // Every observation is still recorded.
    let sources: Vec<String> = log
        .versions("src/money.py")
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert!(
        sources.iter().any(|s| s == "git_pre") && sources.iter().any(|s| s == "git_post"),
        "the late rows are kept: {sources:?}"
    );

    // But they change no conclusion.
    let dead_ends = log.dead_ends();
    assert_eq!(
        dead_ends.len(),
        1,
        "still exactly one dead end: {dead_ends:?}"
    );
    assert_eq!(
        dead_ends[0].3, 0,
        "a pre-restore snapshot is not evidence the change came back"
    );
    assert!(
        !log.edits()
            .iter()
            .any(|(_, s, _)| s == EditStatus::Reapplied.as_str()),
        "no edit is marked REAPPLIED: {:?}",
        log.edits()
    );

    let capsule = log.capsule();
    assert!(
        capsule.contains("[REVERTED_EDITS]"),
        "the section reaches the capsule:\n{capsule}"
    );
    assert!(
        capsule.contains("src/money.py"),
        "naming the file that was tried and discarded:\n{capsule}"
    );
}

/// A genuine reapplication still closes the dead end, so the guard above did
/// not simply switch the feature off.
#[test]
fn f5d_a_later_edit_that_restores_the_content_still_reapplies() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("try the approach, undo it, then put it back");
    log.edit("src/a.rs", "v1\n");
    log.edit("src/a.rs", "v0\n");
    log.reduce();
    assert_eq!(log.dead_ends()[0].3, 0);

    log.edit("src/a.rs", "v1\n");
    log.reduce();
    assert_eq!(log.dead_ends()[0].3, 1, "a real reapplication still counts");
}

/// The second way the v0.1 benchmark lost a revert: the agent ran
/// `git restore … && pytest`, the suite still failed, and Claude Code reported
/// the whole call as a failure. Velra read git effects only from calls that
/// succeeded, so no `git_post` observation was taken; the turn-end scan picked
/// the revert up later and called it "changed outside the agent", with no
/// command text, for a command the agent had just run itself.
#[test]
fn f2b_a_restore_chained_with_a_failing_command_is_still_attributed() {
    let mut log = Log::new();
    log.env.write_file("src/money.py", "ROUND_HALF_UP\n");
    log.prompt("that made things worse, discard it and run the suite again");
    log.edit("src/money.py", "ROUND_HALF_EVEN\n");
    log.git_restore_failed(
        "cd \"/tmp/proj\" && git restore src/money.py && python -m pytest -q",
        1,
        "FAILED tests/test_engine.py::test_exact_payment\n1 failed, 5 passed",
        &[("src/money.py", "ROUND_HALF_UP\n")],
    );
    log.reduce();

    let dead_ends = log.dead_ends();
    assert_eq!(dead_ends.len(), 1, "{dead_ends:?}");
    assert_eq!(
        dead_ends[0].1, "git_command",
        "attributed to the git command, not to something outside the agent"
    );
    assert!(
        dead_ends[0]
            .2
            .as_deref()
            .is_some_and(|c| c.contains("git restore")),
        "and it records which command did it: {:?}",
        dead_ends[0].2
    );
    assert_eq!(statuses(&log), vec![EditStatus::Discarded.as_str()]);

    // The capsule quotes the command without the `cd` that preceded it.
    let capsule = log.capsule();
    assert!(capsule.contains("[REVERTED_EDITS]"), "{capsule}");
    assert!(
        !capsule.contains("cd \"/tmp/proj\""),
        "the working-directory prefix is not worth capsule budget:\n{capsule}"
    );
}

/// A failed `git commit` must not mark edits COMMITTED. Unlike a restore, a
/// commit leaves no trace in the content of the file, so a call that failed
/// proves nothing about it either way.
#[test]
fn f4b_a_failed_commit_leaves_edits_active() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("apply the fix and commit it");
    log.edit("src/a.rs", "v1\n");
    log.git_commit_failed(
        "git commit -am fix && npm test",
        1,
        "1 failing",
        &["src/a.rs"],
    );
    log.reduce();

    assert_eq!(
        statuses(&log),
        vec![EditStatus::Active.as_str()],
        "the edit is still live"
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
    assert!(capsule.contains("[TEST_RESULT]"), "{capsule}");

    log.command_ok(
        "pytest tests/test_auth.py",
        "test result: ok. 12 passed; 0 failed",
    );
    let capsule = log.capsule();
    assert!(
        !capsule.contains("[TEST_RESULT]"),
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
    assert!(capsule.contains("[FIRST_MESSAGE]"));
    assert!(capsule.contains("fix the flaky logout test in the auth module"));
    assert!(capsule.contains("[LATEST_MESSAGE]"), "{capsule}");
    assert!(capsule.contains("why did that fail?"), "{capsule}");
}
