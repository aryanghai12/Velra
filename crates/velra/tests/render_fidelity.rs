//! What the rendered capsule says about the selected state (Phase 5).
//!
//! The snapshot is the selected state; these tests read the *rendered text*
//! and check that the state a continuation needs is on the page -- and that
//! nothing on the page claims more than the snapshot holds. They test the
//! capsule as a data product, at several budgets. They do not show that a
//! model reading it understands it; that is the manual GUI check.

mod common;

use common::Log;
use velra_core::render::{self, RenderConfig, Snapshot, DEFAULT_BUDGET_TOKENS};
use velra_core::text::estimate_tokens;

const TASK: &str = "We have a failing payment retry test. Work on the task, but intentionally \
    leave it unfinished for a later continuation. First inspect the failure and understand \
    the code. Then try a temporary workaround that stores the idempotency key in a \
    module-level variable in src/payments/retry.py. Run the tests. That workaround is \
    considered a rejected approach for this task, so revert it with git restore \
    src/payments/retry.py, rerun the tests, and stop without implementing the final fix. \
    Do not modify the tests.";
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

fn payment(log: &mut Log) {
    log.env.write_file(RETRY, ORIGINAL);
    log.env.write_file(
        "tests/test_retry.py",
        "def test_retry_preserves_idempotency_key(): ...\n",
    );
    log.prompt(TASK);
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
}

fn render_at(snap: &Snapshot, budget: u32) -> String {
    render::render(
        snap,
        &RenderConfig {
            budget_tokens: budget,
        },
    )
    .text
}

/// The body lines of a section, until the next header.
fn section<'a>(capsule: &'a str, name: &str) -> Vec<&'a str> {
    capsule
        .lines()
        .skip_while(|l| !l.starts_with(&format!("[{name}]")))
        .skip(1)
        .take_while(|l| !l.starts_with('[') && !l.starts_with("</"))
        .collect()
}

fn header<'a>(capsule: &'a str, name: &str) -> Option<&'a str> {
    capsule
        .lines()
        .find(|l| l.starts_with(&format!("[{name}]")))
}

// -------------------------------------------------- 2. the payment-retry task

/// The failing session, Snapshot -> capsule at the default budget: every fact
/// a fresh continuation needs is in the rendered text.
#[test]
fn the_payment_retry_capsule_carries_what_a_continuation_needs() {
    let mut log = Log::new();
    payment(&mut log);
    let c = log.capsule();
    assert!(estimate_tokens(&c) <= DEFAULT_BUDGET_TOKENS);
    let first = section(&c, "FIRST_MESSAGE").join("\n");
    // What the task is, and that it is to be left unfinished.
    assert!(
        first.starts_with("We have a failing payment retry test."),
        "{c}"
    );
    assert!(first.contains("intentionally leave it unfinished"), "{c}");
    // That the workaround was rejected, in the user's words.
    let rejected = section(&c, "REJECTED_APPROACHES").join("\n");
    assert!(rejected.contains("Then try a temporary work"), "{c}");
    assert!(
        rejected.contains("That workaround is considered a rejected approach"),
        "{c}"
    );
    // That it was attempted, what it added, and that the file was restored.
    let reverted = section(&c, "REVERTED_EDITS").join("\n");
    assert!(
        reverted.contains("- src/payments/retry.py | 1 edit(s) | reverted via `git restore src/payments/retry.py`"),
        "{c}"
    );
    assert!(reverted.contains("+ _last_idempotency_key = None"), "{c}");
    // That the tests were rerun after the restore, and still fail.
    assert!(
        c.contains(
            "Command: git restore src/payments/retry.py && python -m pytest tests/test_retry.py -v"
        ),
        "{c}"
    );
    assert!(c.contains(&format!("- FAIL {TEST_ID}")), "{c}");
    // The rule.
    assert!(c.contains("\"Do not modify the tests.\""), "{c}");
}

/// With room, the full rejection -- the approach, "module-level", the label --
/// and the failure location are all printed.
#[test]
fn with_room_the_rejection_and_the_target_are_printed_whole() {
    let mut log = Log::new();
    payment(&mut log);
    let c = render_at(&log.snapshot(), 1_000);
    let rejected = section(&c, "REJECTED_APPROACHES").join("\n");
    assert!(
        rejected.contains(
            "stores the idempotency key in a module-level variable in src/payments/retry.py."
        ),
        "{c}"
    );
    assert!(rejected.contains("rejected approach for this task"), "{c}");
    assert_eq!(
        section(&c, "FAILURE_LOCATION"),
        ["tests/test_retry.py:18"],
        "{c}"
    );
}

// ------------------------------------------------ 1/6. rejections rendered

#[test]
fn rejections_are_rendered_and_absent_when_there_are_none() {
    let mut log = Log::new();
    payment(&mut log);
    let snap = log.snapshot();
    assert_eq!(snap.rejections.len(), 1);
    let c = render_at(&snap, 1_000);
    assert_eq!(
        header(&c, "REJECTED_APPROACHES"),
        Some("[REJECTED_APPROACHES] (OBSERVED | user prompt)")
    );
    // Nothing selected, nothing printed -- and no "not listed" claim.
    let mut none = snap.clone();
    none.rejections.clear();
    none.rejection_total = 0;
    assert!(header(&render_at(&none, 1_000), "REJECTED_APPROACHES").is_none());
}

/// More rejections than the snapshot carries: the header says how many are
/// not listed, and every listed quote is one the snapshot holds.
#[test]
fn rejections_omitted_are_counted_not_invented() {
    let mut log = Log::new();
    payment(&mut log);
    let mut snap = log.snapshot();
    snap.rejection_total = 6;
    let c = render_at(&snap, 1_000);
    assert_eq!(
        header(&c, "REJECTED_APPROACHES"),
        Some("[REJECTED_APPROACHES] (OBSERVED | user prompt | 5 more not listed)"),
        "{c}"
    );
    for line in section(&c, "REJECTED_APPROACHES") {
        let quoted = line.split('"').nth(1).expect("a quote");
        for piece in quoted.split(" ... ") {
            let piece = piece.trim_end_matches("...").trim();
            assert!(
                snap.rejections.iter().any(|r| r.text.contains(piece)),
                "{piece:?} is not the user's text"
            );
        }
    }
}

// ---------------------------------------------------- 5. constraint overflow

fn rules_session(log: &mut Log) {
    log.prompt(
        "Refactor the payment module in src/payments/retry.py.\n\n\
         - You must keep the public API stable.\n\
         - You must use the existing logger.\n\
         - You must keep type hints on every function.",
    );
    log.prompt("Also, do not modify the tests.");
    log.prompt("Never add a dependency for this.");
}

/// Five rules, three carried: the header says two more are not listed, the
/// prohibitions are among the listed, and the list is in stated order.
#[test]
fn a_capped_rule_list_says_how_many_it_does_not_list() {
    let mut log = Log::new();
    rules_session(&mut log);
    let snap = log.snapshot();
    let c = render_at(&snap, 1_000);
    let not_listed = snap.constraint_total as usize - snap.constraints.len();
    assert!(not_listed > 0);
    assert_eq!(
        header(&c, "STATED_CONSTRAINTS"),
        Some(
            format!(
                "[STATED_CONSTRAINTS] (OBSERVED | user prompt | quoted verbatim | {not_listed} more not listed)"
            )
            .as_str()
        ),
        "{c}"
    );
    let lines = section(&c, "STATED_CONSTRAINTS").join("\n");
    assert!(lines.contains("\"Also, do not modify the tests.\""), "{c}");
    assert!(
        lines.contains("\"Never add a dependency for this.\""),
        "{c}"
    );
}

/// Squeezed below its length by the ladder, the list keeps the prohibitions
/// -- not "the oldest N" -- and the count grows with what it drops.
#[test]
fn the_ladder_keeps_prohibitions_when_it_shortens_the_rule_list() {
    let mut log = Log::new();
    rules_session(&mut log);
    // Pressure from a long latest message and a noisy failure.
    log.prompt(&"Please also keep an eye on the logging output. ".repeat(12));
    log.command_fail("python -m pytest -q", 1, &"E   noise\n".repeat(20));
    let snap = log.snapshot();
    for budget in [740u32, 600, 500, 420, 360] {
        let c = render_at(&snap, budget);
        let listed = section(&c, "STATED_CONSTRAINTS");
        let listed: Vec<&&str> = listed.iter().filter(|l| l.starts_with("- turn")).collect();
        if listed.is_empty() {
            continue;
        }
        assert!(
            listed
                .iter()
                .any(|l| l.contains("do not modify the tests")
                    || l.contains("Never add a dependency")),
            "budget {budget}: a requirement outlived every prohibition\n{c}"
        );
        if let Some(h) = header(&c, "STATED_CONSTRAINTS") {
            if let Some(n) = h
                .split(" | ")
                .find_map(|p| p.strip_suffix(" more not listed)"))
            {
                let n: usize = n.parse().expect("count");
                // Rules the first-message line prints word for word are shown
                // there, not omitted.
                let first = section(&c, "FIRST_MESSAGE").join("\n");
                let quoted = snap
                    .constraints
                    .iter()
                    .filter(|r| first.contains(r.text.as_str()))
                    .count();
                assert_eq!(
                    n,
                    snap.constraint_total as usize - listed.len() - quoted,
                    "budget {budget}: the count is exact\n{c}"
                );
            }
        }
    }
}

// ---------------------------------------------- 3. dead-end excerpt provenance

/// A dead end of two edits: the printed change is the one finally rejected,
/// marked as the second of two; the other file's dead end keeps its place.
#[test]
fn the_printed_dead_end_change_is_the_rejected_one() {
    let mut log = Log::new();
    log.env
        .write_file("src/money.py", "rounding = ROUND_HALF_UP\n");
    log.env.write_file("src/fees.py", "FEE = 1\n");
    log.prompt("the gap is one cent, so start with the rounding hypothesis in src/money.py");
    log.edit("src/fees.py", "FEE = 2\n");
    log.edit("src/fees.py", "FEE = 1\n");
    log.edit("src/money.py", "rounding = ROUND_HALF_UP  # ?\n");
    log.edit("src/money.py", "rounding = ROUND_HALF_EVEN\n");
    log.git_restore(
        "git restore src/money.py",
        &[("src/money.py", "rounding = ROUND_HALF_UP\n")],
    );
    let c = render_at(&log.snapshot(), 1_000);
    let reverted = section(&c, "REVERTED_EDITS").join("\n");
    assert!(
        reverted.contains("    + rounding = ROUND_HALF_EVEN  (edit 2 of 2)"),
        "{c}"
    );
    assert!(reverted.contains("    - rounding = ROUND_HALF_UP"), "{c}");
    assert!(
        !reverted.contains("# ?"),
        "the intermediate step is not the evidence:\n{c}"
    );
    assert!(
        reverted.contains("- src/fees.py | 1 edit(s) | reverted by a later edit"),
        "{c}"
    );
}

/// Without the edit the excerpt came from, no position is claimed.
#[test]
fn no_edit_position_is_claimed_without_provenance() {
    let mut log = Log::new();
    log.env
        .write_file("src/money.py", "rounding = ROUND_HALF_UP\n");
    log.prompt("try the rounding change in src/money.py");
    log.edit("src/money.py", "rounding = ROUND_HALF_UP  # ?\n");
    log.edit("src/money.py", "rounding = ROUND_HALF_EVEN\n");
    log.git_restore(
        "git restore src/money.py",
        &[("src/money.py", "rounding = ROUND_HALF_UP\n")],
    );
    let mut snap = log.snapshot();
    snap.dead_ends[0].excerpt_edit = None;
    let c = render_at(&snap, 1_000);
    assert!(c.contains("    + rounding = ROUND_HALF_EVEN\n"), "{c}");
    assert!(!c.contains("(edit "), "{c}");
}

// ------------------------------------------------- 4. external attribution

/// A revert nothing recorded explains is described as what was seen: the
/// content went back. No actor is named.
#[test]
fn an_external_revert_claims_no_actor() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("make the change in src/a.py; I may undo it");
    log.edit("src/a.py", "v1\n");
    log.env.write_file("src/a.py", "v0\n");
    log.stop();
    let c = log.capsule();
    assert!(
        c.contains("- src/a.py | 1 edit(s) | reverted by an unidentified change at"),
        "{c}"
    );
    for claim in [
        "outside the agent",
        "by the user",
        "by the agent",
        "manually",
    ] {
        assert!(!c.contains(claim), "{claim}:\n{c}");
    }
}

// ------------------------------------------------ 7. the latest request's tail

/// A long latest message whose request is at its end: the request is printed,
/// at every budget the message is printed at all.
#[test]
fn a_long_latest_message_keeps_its_closing_request() {
    let mut log = Log::new();
    log.prompt(
        "Fix the chunked-body parsing bug in the HTTP codec without touching the public API.",
    );
    let preamble =
        "Some background on how we got here and what the reviewers said last week. ".repeat(8);
    log.prompt(&format!(
        "{preamble}Now rename parse_chunk_size to parse_chunk_len in src/http/codec.rs."
    ));
    let snap = log.snapshot();
    for budget in [1_000u32, DEFAULT_BUDGET_TOKENS, 400, 300] {
        let c = render_at(&snap, budget);
        let latest = section(&c, "LATEST_MESSAGE").join("\n");
        if latest.is_empty() {
            continue;
        }
        assert!(
            latest.contains("parse_chunk_len in src/http/codec.rs."),
            "budget {budget}: the request was cut\n{c}"
        );
        assert!(
            latest.contains(" ... "),
            "budget {budget}: the elision is marked\n{c}"
        );
    }
    // A short message is printed whole, unmarked.
    let mut short = snap.clone();
    if let Some(l) = short.latest.as_mut() {
        l.text = "Now rename parse_chunk_size.".into();
    }
    let c = render_at(&short, 1_000);
    assert_eq!(
        section(&c, "LATEST_MESSAGE"),
        ["Now rename parse_chunk_size."]
    );
}

// ----------------------------------------------- 1. incidental distinctions

#[test]
fn files_that_are_not_the_projects_own_are_marked() {
    let mut log = Log::new();
    log.env.write_file("src/retry.py", "x = 0\n");
    log.env.write_file("node_modules/lib/index.js", "a\n");
    log.prompt("fix the retry logic in src/retry.py");
    log.edit("src/retry.py", "x = 1\n");
    log.edit("node_modules/lib/index.js", "b\n");
    let note = velra_core::paths::normalize_abs(
        &log.env.dir.path().join("memory/notes.md").to_string_lossy(),
    );
    log.write_tool(&note, "- notes\n");
    let c = render_at(&log.snapshot(), 1_000);
    assert!(
        c.contains("- node_modules/lib/index.js (dependency or build output) |"),
        "{c}"
    );
    assert!(c.contains(" (outside workspace) |"), "{c}");
    assert!(c.contains("- src/retry.py |"), "{c}");
    // The next target is the project's file, and is not called a failure
    // location.
    assert_eq!(
        header(&c, "NEXT_TARGET"),
        Some("[NEXT_TARGET] (INFERRED | last-active-edit)")
    );
    assert_eq!(section(&c, "NEXT_TARGET"), ["src/retry.py"]);
    assert!(header(&c, "FAILURE_LOCATION").is_none());
}

// ------------------------------------------------------ 8. the budget ladder

/// A crowded session at falling budgets. At each: the capsule fits; and a
/// section of lower retention value is never printed while one of higher
/// value is gone -- regenerable detail goes first, the task last.
#[test]
fn the_ladder_gives_up_detail_before_task_state() {
    let mut log = crowded(&mut Log::new());
    let snap = log.snapshot();
    let order = [
        // Lowest value first.
        "FAILURE_LOCATION",
        "RECENT_EDITS",
        "FILE_ACTIVITY",
        "TEST_RESULT",
        "REVERTED_EDITS",
        "REJECTED_APPROACHES",
        "TEST_STATUS",
        "STATED_CONSTRAINTS",
        "FIRST_MESSAGE",
    ];
    for budget in (240..=1_000).rev().step_by(20) {
        let c = render_at(&snap, budget);
        assert!(estimate_tokens(&c) <= budget, "budget {budget}");
        assert!(c.ends_with("</VELRA_WORKSPACE_STATE>"), "budget {budget}");
        for (i, low) in order.iter().enumerate() {
            if header(&c, low).is_none() {
                continue;
            }
            for high in &order[i + 1..] {
                assert!(
                    header(&c, high).is_some(),
                    "budget {budget}: [{low}] printed while [{high}] was dropped\n{c}"
                );
            }
        }
    }
    let _ = &mut log;
}

fn crowded(log: &mut Log) -> Log {
    let mut log = std::mem::replace(log, Log::new());
    payment(&mut log);
    rules_session(&mut log);
    for i in 0..6 {
        let p = format!("src/other/f{i}.py");
        log.env.write_file(&p, "a = 0\n");
        log.edit(&p, "a = 1\n");
        log.edit(&p, "a = 0\n");
    }
    for i in 0..30 {
        let p = format!("src/unrelated/m{i:02}.py");
        log.env.write_file(&p, "x\n");
        log.read(&p);
    }
    log.prompt(&format!(
        "{}Next, check whether build_retry_payload copies the key.",
        "Context on the retry path and the reviewers' notes. ".repeat(10)
    ));
    log
}

// -------------------------------------------------------- 9. determinism

#[test]
fn identical_input_renders_identically() {
    let mut log = crowded(&mut Log::new());
    let snap = log.snapshot();
    for budget in [1_000u32, DEFAULT_BUDGET_TOKENS, 500, 300] {
        let a = render_at(&snap, budget);
        let b = render_at(&snap.clone(), budget);
        assert_eq!(a, b, "budget {budget}");
        assert_eq!(
            render::render_ladder(
                &snap,
                &RenderConfig {
                    budget_tokens: budget
                }
            ),
            render::render_ladder(
                &snap,
                &RenderConfig {
                    budget_tokens: budget
                }
            )
        );
    }
    // A second build of the same ledger renders the same text.
    let again = log.snapshot();
    assert_eq!(
        render_at(&again, DEFAULT_BUDGET_TOKENS),
        render_at(&snap, DEFAULT_BUDGET_TOKENS)
    );
}

// --------------------------------------------------- 11. nothing invented

/// Every rule and rejection line the capsule prints is the snapshot's; the
/// next target is the snapshot's; no section appears for state it lacks.
#[test]
fn the_capsule_claims_nothing_the_snapshot_lacks() {
    let mut log = crowded(&mut Log::new());
    let snap = log.snapshot();
    for budget in [1_000u32, DEFAULT_BUDGET_TOKENS, 400] {
        let c = render_at(&snap, budget);
        for line in section(&c, "STATED_CONSTRAINTS") {
            let q = line.split('"').nth(1).expect("quoted");
            let q = q.trim_end_matches("...");
            assert!(
                snap.constraints.iter().any(|r| r.text.starts_with(q)),
                "budget {budget}: {q:?}"
            );
        }
        for body in section(&c, "NEXT_TARGET")
            .into_iter()
            .chain(section(&c, "FAILURE_LOCATION"))
        {
            let target = snap.next_target.as_ref().expect("a target").target.as_str();
            assert!(body.starts_with(target) || target.ends_with(body.trim_start_matches("...")));
        }
    }
    let mut bare = snap.clone();
    bare.rejections.clear();
    bare.rejection_total = 0;
    bare.dead_ends.clear();
    bare.dead_end_total = 0;
    bare.next_target = None;
    let c = render_at(&bare, 1_000);
    for absent in [
        "[REJECTED_APPROACHES]",
        "[REVERTED_EDITS]",
        "[NEXT_TARGET]",
        "[FAILURE_LOCATION]",
    ] {
        assert!(!c.contains(absent), "{absent}:\n{c}");
    }
    let _ = &mut log;
}

// ------------------------------------------------------ 10. measured sizes

/// The estimated size of each fixture at full detail and as rendered at the
/// default budget, printed for the record (`--nocapture`). Estimated tokens
/// are the renderer's own measure (`text::estimate_tokens`), not a tokenizer
/// count.
#[test]
fn rendered_sizes_are_within_budget() {
    let mut fixtures: Vec<(&str, Log)> = Vec::new();
    let mut l = Log::new();
    payment(&mut l);
    fixtures.push(("normal (payment retry)", l));
    let l = crowded(&mut Log::new());
    fixtures.push(("crowded", l));
    let mut l = crowded(&mut Log::new());
    for i in 0..12 {
        l.prompt(&format!(
            "Never touch module_{i}.py. That approach {i} is a dead end."
        ));
    }
    for i in 0..40 {
        let p = format!("src/very/deep/package/path/module_{i:02}.py");
        l.env.write_file(&p, "x\n");
        l.read(&p);
        l.read(&p);
    }
    fixtures.push(("very crowded", l));
    let mut l = Log::new();
    l.prompt("Fix the codec.");
    l.prompt(&format!("{}Now rename x to y.", "background ".repeat(400)));
    fixtures.push(("long latest message", l));
    let mut l = Log::new();
    l.prompt("Sweep the codebase for the retry helper.");
    for i in 0..120 {
        let p = format!("src/pkg{}/mod_{i:03}.py", i % 7);
        l.env.write_file(&p, "x\n");
        l.read(&p);
    }
    fixtures.push(("many files", l));
    let mut l = Log::new();
    l.prompt("Try several rounding strategies in src/money.py.");
    l.env.write_file("src/money.py", "r = 0\n");
    for i in 1..=10 {
        l.edit("src/money.py", &format!("r = {i}\n"));
        l.edit("src/money.py", "r = 0\n");
    }
    fixtures.push(("many dead ends", l));
    let mut l = Log::new();
    rules_session(&mut l);
    for i in 0..10 {
        l.prompt(&format!(
            "Try caching approach {i} in src/cache.py. That approach is considered a rejected approach."
        ));
    }
    fixtures.push(("many rules and rejections", l));

    for (name, mut log) in fixtures {
        let snap = log.snapshot();
        let full = render_at(&snap, 1_000);
        let full_detail = render::render_ladder(&snap, &RenderConfig::default())[0]
            .text
            .clone();
        let r = render::render(&snap, &RenderConfig::default());
        assert!(r.tokens <= DEFAULT_BUDGET_TOKENS, "{name}");
        assert!(estimate_tokens(&full) <= 1_000, "{name}");
        eprintln!(
            "SIZE {name:<28} full-detail={:>5}  at-1000={:>4}  at-default={:>4} ({} rungs)",
            estimate_tokens(&full_detail),
            estimate_tokens(&full),
            r.tokens,
            r.steps
        );
    }
}
