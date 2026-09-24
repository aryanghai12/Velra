//! Retention priority under a bounded capsule (v0.1.2, found after the
//! requalification freeze by a real VS Code session; D70-D74).
//!
//! The session under test is the one that failed. A VS Code user asked for a
//! payment retry fix, a module-level workaround was tried and reverted with
//! `git restore`, and the next session was given the restore capsule. The
//! ledger held everything. The capsule did not carry the task, because
//!
//! 1. the extension had put `<ide_opened_file>…</ide_opened_file>` in front of
//!    the prompt, so the stored objective *began* with the editor tab, and
//! 2. the ladder cut the objective to 160 characters and the reverted edit's
//!    excerpt while a pytest `=====` banner, an "observed afterward" line and
//!    two already-named working files survived.
//!
//! `payment_session` reproduces that session through the product's own event
//! path, with the same objective text and the same shape of pytest output.
//! Nothing here drives VS Code: the IDE block is fed in exactly as the
//! `UserPromptSubmit` hook delivers it, which is what the product sees.

mod common;

use common::{Env, Log};
use serde_json::json;
use velra_core::model::Trigger;
use velra_core::provenance::{self, Presence, TraceInputs};
use velra_core::render::{self, RenderConfig, DEFAULT_BUDGET_TOKENS};
use velra_core::restore::{self, RestoreRequest};
use velra_core::snapshot::{self, IntentRow, SnapshotMeta};
use velra_core::text::estimate_tokens;

const IDE: &str =
    "<ide_opened_file>The user opened the file c:\\Users\\dev\\test repo\\readme.md in \
                   the IDE. This may or may not be related to the current task.</ide_opened_file>";

const TASK: &str = "We have a failing payment retry test. Work on the task, but intentionally leave \
                    it unfinished for a later continuation. First inspect the failure and understand \
                    the code. Then try a temporary workaround that stores the idempotency key in a \
                    module-level variable in src/payments/retry.py. Run the tests. That workaround is \
                    considered a rejected approach for this task, so revert it with git restore \
                    src/payments/retry.py, rerun the tests, and stop without implementing the final \
                    fix. Do not modify the tests.";

const TASK_HEAD: &str = "We have a failing payment retry test";
const TEST_ID: &str = "tests/test_retry.py::test_retry_preserves_idempotency_key";
const RETRY: &str = "src/payments/retry.py";
const WORKAROUND: &str = "_last_idempotency_key";

const ORIGINAL: &str = "def build_retry_payload(request: dict) -> dict:\n    return {\"amount\": request[\"amount\"]}\n";
const PATCHED: &str = "_last_idempotency_key = None\n\ndef build_retry_payload(request: dict) -> dict:\n    global _last_idempotency_key\n    return {\"amount\": request[\"amount\"]}\n";

/// The shape of `pytest -v` output the real session produced, banners included.
const PYTEST_FAIL: &str =
    "tests/test_retry.py::test_retry_preserves_idempotency_key FAILED         [ 50%]\n\
\n\
================================== FAILURES ===================================\n\
_____________________ test_retry_preserves_idempotency_key ______________________\n\
\n\
    def test_retry_preserves_idempotency_key():\n\
>       assert payload[\"idempotency_key\"] == \"idem-7f31\"\n\
E       KeyError: 'idempotency_key'\n\
\n\
tests/test_retry.py:18: KeyError\n\
=========================== short test summary info ===========================\n\
FAILED tests/test_retry.py::test_retry_preserves_idempotency_key - KeyError: 'idempotency_key'\n\
========================= 1 failed, 1 passed in 0.05s =========================";

const DEFAULT: RenderConfig = RenderConfig {
    budget_tokens: DEFAULT_BUDGET_TOKENS,
};

fn prompt_text(with_ide: bool, text: &str) -> String {
    if with_ide {
        format!("{IDE}\n{text}")
    } else {
        text.to_string()
    }
}

/// The failing VS Code session, through the product's own event path.
fn payment_session(log: &mut Log, with_ide: bool) {
    payment_session_with(log, &prompt_text(with_ide, TASK));
}

/// A longer statement of the same task: the rejection clause lies past the
/// widest objective excerpt the renderer ever prints (240 characters), so a
/// capsule that carries it cannot be doing so through the objective line.
const LONG_TASK: &str =
    "We have a failing payment retry test in tests/test_retry.py and the on-call \
                         team has asked for a careful, minimal change. Work on the task, but \
                         intentionally leave it unfinished for a later continuation, because the \
                         reviewer wants to see the investigation before the final patch lands. \
                         First inspect the failure and understand the code. Then try a temporary \
                         workaround that stores the idempotency key in a module-level variable in \
                         src/payments/retry.py. Run the tests. That workaround is considered a \
                         rejected approach \
                         for this task, so revert it with git restore src/payments/retry.py, \
                         rerun the tests, and stop without implementing the final fix. Do not \
                         modify the tests.";

fn payment_session_with(log: &mut Log, prompt: &str) {
    log.env.write_file(RETRY, ORIGINAL);
    log.env.write_file(
        "tests/test_retry.py",
        "def test_retry_preserves_idempotency_key(): ...\n",
    );
    log.prompt(prompt);
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

/// `payment_session` plus pressure: a wide read sweep, unrelated edits, a
/// second rejected route, several competing failing tests.
fn crowded_payment_session(log: &mut Log, with_ide: bool) {
    payment_session(log, with_ide);
    log.env
        .write_file("src/payments/gateway.py", "TIMEOUT = 5\n");
    log.edit("src/payments/gateway.py", "TIMEOUT = 50\n");
    log.git_restore(
        "git restore src/payments/gateway.py",
        &[("src/payments/gateway.py", "TIMEOUT = 5\n")],
    );
    for i in 0..25 {
        let path = format!("src/unrelated/module_{i:02}.py");
        log.env.write_file(&path, "x = 1\n");
        log.read(&path);
    }
    for i in 0..6 {
        let path = format!("src/unrelated/edited_{i}.py");
        log.env.write_file(&path, "y = 1\n");
        log.edit(&path, "y = 2\n");
    }
    let mut noisy = String::new();
    for i in 0..7 {
        noisy.push_str(&format!(
            "FAILED tests/test_other.py::test_unrelated_{i} - AssertionError: {}\n",
            "x".repeat(40)
        ));
    }
    noisy.push_str(PYTEST_FAIL);
    log.command_fail("python -m pytest -q", 1, &noisy);
}

fn meta(checkpoint: &str, created_ms: i64) -> SnapshotMeta {
    SnapshotMeta {
        checkpoint_id: checkpoint.to_string(),
        created_ms,
        trigger: Trigger::Cli,
        partial: false,
        preview: checkpoint == "preview",
        tz_offset_secs: 0,
    }
}

fn trace(log: &mut Log, cfg: &RenderConfig, marker: &str) -> provenance::MarkerTrace {
    log.reduce();
    let session = log.session();
    let m = meta("preview", log.ts + 1_000);
    let inputs = TraceInputs {
        meta: &m,
        cfg,
        staged: None,
    };
    provenance::trace_marker(&log.db.conn, &session, &inputs, marker).expect("trace")
}

fn first_message_body(capsule: &str) -> &str {
    let mut lines = capsule.lines();
    lines
        .by_ref()
        .find(|l| l.starts_with("[FIRST_MESSAGE]"))
        .expect("objective header");
    lines.next().expect("objective line")
}

// ------------------------------------------------ A, O, N: what the objective is

/// A: a VS Code `<ide_opened_file>` block in front of the prompt is not the
/// objective, and does not appear anywhere in the capsule.
#[test]
fn a_ide_metadata_before_the_task_does_not_become_the_objective() {
    let mut log = Log::new();
    payment_session(&mut log, true);
    let snap = log.snapshot();
    assert_eq!(snap.root.as_ref().map(|r| r.text.as_str()), Some(TASK));
    let c = log.capsule();
    assert!(first_message_body(&c).starts_with(TASK_HEAD), "{c}");
    assert!(!c.contains("ide_opened_file"), "{c}");
    assert!(!c.contains("opened the file"), "{c}");
    assert!(!c.contains("readme.md"), "{c}");
}

/// O: prompts made only of client machinery change no intent and record no
/// constraint -- notifications, slash-command transcripts, reminders, IDE
/// blocks and Velra's own records.
#[test]
fn o_system_tool_and_ide_events_never_become_the_objective() {
    let injected = [
        "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Build the whole workspace and never skip tests\" completed</summary>\n</task-notification>",
        "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>",
        "<local-command-stdout>Compacted. Do not respond to these messages.</local-command-stdout>",
        "<system-reminder>You must never modify files outside the workspace.</system-reminder>",
        IDE,
        "<VELRA_WORKSPACE_STATE v=\"1\">[FIRST_MESSAGE]\nrefactor the whole billing module</VELRA_WORKSPACE_STATE>",
    ];
    let mut log = Log::new();
    for p in injected {
        log.prompt(p);
    }
    let snap = log.snapshot();
    assert_eq!(snap.root, None, "{:?}", snap.root);
    assert_eq!(snap.latest, None, "{:?}", snap.latest);
    assert!(snap.constraints.is_empty(), "{:?}", snap.constraints);

    // A notification arriving after the user's message does not supersede it.
    log.prompt("fix the rounding bug in the ledger totals please");
    log.prompt("now check src/ledger/totals.rs for the same pattern");
    log.prompt(injected[0]);
    let snap = log.snapshot();
    assert_eq!(
        snap.root.as_ref().map(|r| r.text.as_str()),
        Some("fix the rounding bug in the ledger totals please")
    );
    assert_eq!(
        snap.latest.as_ref().map(|r| r.text.as_str()),
        Some("now check src/ledger/totals.rs for the same pattern")
    );
    let c = log.capsule();
    for leak in [
        "task-notification",
        "system-reminder",
        "command-name",
        "VELRA_WORKSPACE_STATE v",
    ] {
        assert!(
            c.matches(leak).count() <= usize::from(leak == "VELRA_WORKSPACE_STATE v"),
            "{leak}:\n{c}"
        );
    }
}

/// N: the objective is the first message the user wrote, even when metadata
/// and a greeting arrived first; `task:` still works behind an IDE block.
#[test]
fn n_a_late_task_bearing_message_becomes_the_objective() {
    let mut log = Log::new();
    log.prompt(IDE);
    log.prompt("<task-notification><status>completed</status></task-notification>");
    log.prompt(&prompt_text(true, "hi"));
    log.prompt(&prompt_text(true, TASK));
    let snap = log.snapshot();
    assert_eq!(snap.root.as_ref().map(|r| r.text.as_str()), Some(TASK));

    let mut log = Log::new();
    log.prompt(&prompt_text(true, TASK));
    log.prompt(&prompt_text(
        true,
        "task: migrate the settings loader to the new config format",
    ));
    let snap = log.snapshot();
    assert_eq!(
        snap.root.as_ref().map(|r| r.text.as_str()),
        Some("migrate the settings loader to the new config format"),
        "the `task:` prefix starts a new epoch behind an IDE block"
    );
}

/// Constraints come from the user's words only: a reminder's "never" is not
/// the user's rule, and the user's "Do not" still is.
#[test]
fn constraints_are_extracted_from_authored_text_only() {
    let mut log = Log::new();
    log.prompt(&format!(
        "<system-reminder>You must never touch the lockfile.</system-reminder>\n{}",
        prompt_text(true, TASK)
    ));
    let snap = log.snapshot();
    let texts: Vec<&str> = snap.constraints.iter().map(|c| c.text.as_str()).collect();
    // The reminder's "must never" is not among them; the user's own rule is.
    assert_eq!(texts, ["Do not modify the tests."]);
    // And so is the user's own rejection of the workaround, kept apart from
    // the rules and quoted from the sentence that describes the workaround.
    let rejections: Vec<&str> = snap.rejections.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        rejections,
        [
            "Then try a temporary workaround that stores the idempotency key in a module-level \
          variable in src/payments/retry.py. Run the tests. That workaround is considered a \
          rejected approach for this task, so revert it with git restore \
          src/payments/retry.py, rerun the tests, and stop without implementing the final fix."
        ]
    );
}

/// A ledger reduced before the fix stored the IDE block in the ROOT row. The
/// snapshot reads through it, so an existing session restores correctly.
#[test]
fn an_objective_stored_with_injected_context_is_read_through() {
    let mut log = Log::new();
    payment_session(&mut log, false);
    log.reduce();
    log.db
        .conn
        .execute(
            "UPDATE intents SET text = ?1 || ' ' || text WHERE level = 'ROOT'",
            [IDE],
        )
        .expect("simulate a pre-fix row");
    let snap = log.snapshot();
    assert_eq!(snap.root.as_ref().map(|r| r.text.as_str()), Some(TASK));

    // And the trace says why the IDE text stops at the snapshot.
    let t = trace(&mut log, &DEFAULT, "readme.md");
    assert_eq!(t.first_loss, Some("snapshot"), "{}", t.report());
    assert!(
        t.reason
            .as_deref()
            .unwrap_or("")
            .contains("injected context"),
        "{}",
        t.report()
    );
}

/// The selection rule for pre-fix rows, where a notification took ROOT and
/// then superseded the user's latest message.
#[test]
fn pre_fix_rows_that_are_only_injected_context_fall_back_to_the_users_words() {
    let row = |id: i64, level: &str, text: &str, live: bool| IntentRow {
        id,
        level: velra_core::model::IntentLevel::parse(level),
        text: text.to_string(),
        ts_ms: id,
        live,
    };
    let note = "<task-notification><task-id>x</task-id></task-notification>";
    let rows = [
        row(1, "ROOT", note, true),
        row(2, "LATEST", TASK, false),
        row(3, "LATEST", "now look at src/payments/retry.py", false),
        row(4, "LATEST", note, true),
    ];
    let picked = snapshot::select_intents(&rows);
    assert_eq!(picked.root.map(|r| r.id), Some(2));
    assert_eq!(picked.latest.map(|r| r.id), Some(3));
}

// ------------------------------------------- H, G: independence and determinism

/// H: the same work with and without the IDE block yields the same capsule,
/// byte for byte, at every budget.
#[test]
fn h_ide_metadata_does_not_change_what_is_retained() {
    for budget in [DEFAULT_BUDGET_TOKENS, 600, 500, 400] {
        let mut with = Log::new();
        crowded_payment_session(&mut with, true);
        let mut without = Log::new();
        crowded_payment_session(&mut without, false);
        assert_eq!(
            with.capsule_at(budget),
            without.capsule_at(budget),
            "budget {budget}"
        );
    }
}

/// G: rendering is a pure function of the ledger.
#[test]
fn g_rendering_is_deterministic() {
    let mut log = Log::new();
    crowded_payment_session(&mut log, true);
    let snap = log.snapshot();
    let first = render::render(&snap, &DEFAULT).text;
    for _ in 0..5 {
        assert_eq!(render::render(&snap, &DEFAULT).text, first);
        assert_eq!(log.snapshot(), snap);
    }
    let mut again = Log::new();
    crowded_payment_session(&mut again, true);
    assert_eq!(again.capsule(), first, "a second identical ledger");
}

// --------------------------------------- B, C, D, E: critical state under pressure

/// B, C, D, E at the default budget, on the session that failed.
#[test]
fn the_failing_vs_code_session_now_carries_its_task_state() {
    let mut log = Log::new();
    payment_session(&mut log, true);
    let c = log.capsule();
    assert!(estimate_tokens(&c) <= DEFAULT_BUDGET_TOKENS);
    // The objective is carried to the full first-message width, not cut to 160.
    let body = first_message_body(&c);
    assert!(body.starts_with(TASK_HEAD), "{c}");
    assert!(body.contains("stores the idempotency key"), "{c}");
    assert!(c.contains(&format!("FAIL {TEST_ID}")), "{c}");
    assert!(c.contains("Do not modify the tests."), "{c}");
    assert!(
        c.contains(
            "src/payments/retry.py | 1 edit(s) | reverted via `git restore src/payments/retry.py`"
        ),
        "{c}"
    );
    assert!(
        c.contains(&format!("+ {WORKAROUND} = None")),
        "the rejected route:\n{c}"
    );
    // The user's own statement that the workaround is rejected is state of its
    // own, not only a phrase that happens to fit inside the objective excerpt.
    let snap = log.snapshot();
    assert!(
        snap.rejections
            .iter()
            .any(|r| r.text.contains("rejected approach")
                && r.text
                    .contains("stores the idempotency key in a module-level variable")),
        "{:?}",
        snap.rejections
    );
}

/// The same session with the rejection clause past character 240, the widest
/// objective excerpt the renderer prints. The objective line is shortened and
/// cannot show the clause; the rejection is kept as selected state of its own,
/// quoted verbatim together with the approach it rejects, and the trace
/// reports honestly how far it gets.
///
/// This pins the extraction and selection layers. Whether the capsule prints
/// the rejection is the renderer's decision, and is asserted there.
#[test]
fn a_rejection_past_the_objective_excerpt_is_kept_as_selected_state() {
    let clause_at = LONG_TASK.find("rejected approach").expect("clause");
    assert!(clause_at > 240, "{clause_at}");
    let mut log = Log::new();
    payment_session_with(&mut log, &prompt_text(true, LONG_TASK));
    let c = log.capsule();
    assert!(estimate_tokens(&c) <= DEFAULT_BUDGET_TOKENS);
    let body = first_message_body(&c);
    assert!(
        !body.contains("rejected approach"),
        "the objective line must not be what carries the clause: {body}"
    );

    let snap = log.snapshot();
    let rejection = snap
        .rejections
        .iter()
        .find(|r| r.text.contains("rejected approach"))
        .unwrap_or_else(|| panic!("the rejection is not selected state: {snap:?}"));
    assert!(
        rejection
            .text
            .contains("stores the idempotency key in a module-level variable"),
        "the rejection must name what it rejects: {}",
        rejection.text
    );
    assert!(
        rejection
            .text
            .starts_with("Then try a temporary workaround"),
        "quoted from the sentence that describes the workaround, not the one \
         before the rejection: {}",
        rejection.text
    );
    assert!(LONG_TASK.contains(rejection.text.as_str()), "verbatim");
    // A rejection does not take a rule's slot.
    assert_eq!(
        snap.constraints
            .iter()
            .map(|k| k.text.as_str())
            .collect::<Vec<_>>(),
        ["Do not modify the tests."]
    );
    // The capsule's other critical state is unchanged by the long prompt.
    assert!(c.contains("Do not modify the tests."), "{c}");
    assert!(c.contains(&format!("FAIL {TEST_ID}")), "{c}");
    assert!(
        c.contains(&format!("- {RETRY} | 1 edit(s) | reverted via")),
        "{c}"
    );

    // The trace finds it at every layer through the snapshot.
    let t = trace(&mut log, &DEFAULT, "rejected approach for this task");
    for layer in ["normalized_state", "ledger", "snapshot"] {
        let l = t.layers.iter().find(|l| l.layer == layer).expect(layer);
        assert_eq!(l.presence, Presence::Present, "{layer}: {}", t.report());
    }
    assert!(
        !matches!(
            t.first_loss,
            Some("normalized_state" | "ledger" | "snapshot")
        ),
        "{}",
        t.report()
    );
}

/// B, C, D, E across budgets. Every critical item of this state at its
/// narrowest -- the objective at sixty characters, both rejected routes, the
/// test id, the constraint, the frame's ~250 tokens -- measures 629 estimated
/// tokens, so from 630 up all of them must be present. Below that nothing can
/// hold them all, and what is asserted is the order they are given up in.
#[test]
fn b_c_d_e_critical_state_survives_budget_pressure() {
    let mut log = Log::new();
    crowded_payment_session(&mut log, true);
    for budget in (630..=DEFAULT_BUDGET_TOKENS).rev().step_by(10) {
        let c = log.capsule_at(budget);
        assert!(estimate_tokens(&c) <= budget);
        let why = format!("budget {budget}:\n{c}");
        assert!(first_message_body(&c).starts_with(TASK_HEAD), "B {why}");
        assert!(c.contains(&format!("FAIL {TEST_ID}")), "D {why}");
        assert!(c.contains(RETRY), "E {why}");
        assert!(c.contains("Do not modify the tests."), "constraint {why}");
        assert!(
            c.contains(&format!("- {RETRY} | 1 edit(s) | reverted via")),
            "C: the rejected route's header {why}"
        );
        assert!(
            c.contains("- src/payments/gateway.py | 1 edit(s) | reverted via"),
            "C: the other rejected route {why}"
        );
    }
    for budget in (300..630).rev().step_by(10) {
        let c = log.capsule_at(budget);
        let why = format!("budget {budget}:\n{c}");
        assert!(estimate_tokens(&c) <= budget, "{why}");
        if budget >= 340 {
            assert!(first_message_body(&c).starts_with(TASK_HEAD), "B {why}");
        }
        let has = |s: &str| c.contains(s);
        if has("[TEST_RESULT]") {
            assert!(
                has("[REVERTED_EDITS]"),
                "regenerable output outlived a route: {why}"
            );
        }
        if has("[REVERTED_EDITS]") {
            assert!(has("[TEST_STATUS]"), "a route outlived the test id: {why}");
        }
        if has("[TEST_STATUS]") {
            assert!(
                has("[STATED_CONSTRAINTS]"),
                "a test id outlived the user's rule: {why}"
            );
        }
    }
    // At the default budget, the route's own line too.
    assert!(log.capsule().contains(WORKAROUND), "{}", log.capsule());
}

/// C: a group of reverts of one file keeps its header, and at full width the
/// attempted line of every member; the replaced line goes first.
#[test]
fn c_dead_end_attempted_lines_outlive_replaced_lines() {
    let mut log = Log::new();
    crowded_payment_session(&mut log, true);
    let ladder = render::render_ladder(&log.snapshot(), &RenderConfig { budget_tokens: 400 });
    let at = |needle: &str| {
        ladder
            .iter()
            .position(|s| !s.text.contains(needle))
            .unwrap_or(usize::MAX)
    };
    let replaced_gone = at("    - def build_retry_payload");
    let attempted_gone = at(&format!("    + {WORKAROUND}"));
    assert!(
        replaced_gone < attempted_gone,
        "the replaced line ({replaced_gone}) must go before the attempted one ({attempted_gone}): {:?}",
        ladder.iter().map(|s| s.rung).collect::<Vec<_>>()
    );
}

/// L: low-value content goes first. Whenever the objective is shortened,
/// every LOW item is already gone.
#[test]
fn l_low_priority_content_is_sacrificed_before_the_objective() {
    let mut log = Log::new();
    crowded_payment_session(&mut log, true);
    let snap = log.snapshot();
    let full_objective = velra_core::text::truncate_chars(TASK, 240).into_owned();
    for budget in (300..=1000).rev().step_by(10) {
        let c = render::render(
            &snap,
            &RenderConfig {
                budget_tokens: budget,
            },
        )
        .text;
        if c.contains(&full_objective) {
            continue;
        }
        let why = format!("objective shortened at {budget}, yet:\n{c}");
        assert!(!c.contains("Observed afterward"), "{why}");
        assert!(!c.contains("[FAILURE_LOCATION]"), "{why}");
        assert!(!c.contains("src/unrelated/module_"), "{why}");
        assert!(!c.contains("    - def build_retry_payload"), "{why}");
        assert!(
            !c.contains("KeyError: 'idempotency_key'\n"),
            "excerpt: {why}"
        );
        assert!(!c.contains("[RECENT_EDITS]"), "{why}");
    }
}

/// M: a long, noisy objective is carried from its start, and runner banners
/// never cost a line of it.
#[test]
fn m_long_noisy_objective_and_banner_output() {
    let mut log = Log::new();
    let long = format!(
        "{TASK} {}",
        "Also keep the audit log format stable. ".repeat(40)
    );
    log.prompt(&prompt_text(true, &long));
    log.command_fail("python -m pytest -v", 1, PYTEST_FAIL);
    let c = log.capsule_at(900);
    let body = first_message_body(&c);
    assert!(body.starts_with(TASK_HEAD), "{c}");
    assert!(body.ends_with("..."), "{c}");
    assert!(!c.contains("===="), "banners are compacted: {c}");
    assert!(c.contains("=== FAILURES ==="), "{c}");
    // The snapshot keeps the output as captured.
    let snap = log.snapshot();
    assert!(snap
        .failure
        .expect("failure")
        .excerpt
        .iter()
        .any(|l| l.contains("=====")));
}

/// A constraint the objective already prints is not printed twice, and comes
/// back when the objective is cut short of it.
#[test]
fn a_constraint_is_not_printed_twice_but_returns_when_the_objective_is_cut() {
    let mut log = Log::new();
    log.prompt("Fix the retry payload. Do not modify the tests.");
    let c = log.capsule();
    assert_eq!(c.matches("Do not modify the tests.").count(), 1, "{c}");
    assert!(!c.contains("[STATED_CONSTRAINTS]"), "{c}");

    let mut log = Log::new();
    payment_session(&mut log, true);
    let c = log.capsule();
    // The objective's 240 characters stop before its last sentence.
    assert!(c.contains("[STATED_CONSTRAINTS]"), "{c}");
    assert!(c.contains("\"Do not modify the tests.\""), "{c}");
}

// ------------------------------------------------------ I, J: dead ends and ids

/// I: a file reverted over and over shares one header and does not hide an
/// older rejected route on another file.
#[test]
fn i_repeated_reverts_do_not_hide_another_rejected_route() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "a = 1\n");
    log.env.write_file("src/b.py", "b = 1\n");
    log.prompt("make the settlement job idempotent across retries");
    log.edit("src/b.py", "b = 2\n");
    log.git_restore("git restore src/b.py", &[("src/b.py", "b = 1\n")]);
    for v in 2..7 {
        log.edit("src/a.py", &format!("a = {v}\n"));
        log.edit("src/a.py", "a = 1\n");
    }
    let snap = log.snapshot();
    assert_eq!(snap.dead_end_total, 6);
    assert!(
        snap.dead_ends.iter().any(|d| d.path == "src/b.py"),
        "{:?}",
        snap.dead_ends
    );
    let c = log.capsule();
    assert!(
        c.contains("- src/b.py | 1 edit(s) | reverted via `git restore src/b.py`"),
        "{c}"
    );
    let reverted: Vec<&str> = c
        .lines()
        .skip_while(|l| !l.starts_with("[REVERTED_EDITS]"))
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .collect();
    assert_eq!(
        reverted
            .iter()
            .filter(|l| l.starts_with("- src/a.py |"))
            .count(),
        1,
        "one shared header:\n{c}"
    );
    assert!(c.contains("3 reverts"), "{c}");
}

/// J: among many failing tests, the one the session worked on keeps its exact
/// id at every budget, ahead of a full-suite run's other failures.
#[test]
fn j_the_focused_test_id_wins_among_competing_ids() {
    let mut log = Log::new();
    crowded_payment_session(&mut log, true);
    let snap = log.snapshot();
    assert_eq!(snap.tests.first().map(|t| t.id.as_str()), Some(TEST_ID));
    for budget in [DEFAULT_BUDGET_TOKENS, 630, 560] {
        let c = log.capsule_at(budget);
        assert!(
            c.contains(&format!("FAIL {TEST_ID}")),
            "budget {budget}:\n{c}"
        );
    }
}

// --------------------------------------------------------- F, K: trace, restore

/// F: the trace reports each protected marker as carried, a budget loss at the
/// renderer with its rung, an injected-only string at the ledger with the
/// reason, and a never-captured string at the capture layer.
#[test]
fn f_trace_distinguishes_capture_selection_and_budget_losses() {
    let mut log = Log::new();
    payment_session(&mut log, true);
    for marker in [TASK_HEAD, TEST_ID, RETRY, WORKAROUND] {
        let t = trace(&mut log, &DEFAULT, marker);
        assert_eq!(t.first_loss, None, "{}", t.report());
        for layer in [
            "normalized_state",
            "ledger",
            "snapshot",
            "renderer",
            "restore",
        ] {
            let l = t.layers.iter().find(|l| l.layer == layer).expect("layer");
            assert_eq!(l.presence, Presence::Present, "{layer}: {}", t.report());
        }
    }

    // Budget: at a tight target the attempted line is cut, and said so.
    let t = trace(&mut log, &RenderConfig { budget_tokens: 330 }, WORKAROUND);
    assert_eq!(t.first_loss, Some("renderer"), "{}", t.report());
    assert!(
        t.reason
            .as_deref()
            .unwrap_or("")
            .starts_with("budgeted out: removed by ladder rung `"),
        "{}",
        t.report()
    );

    // Injected context: stored in the event, never the user's words.
    let t = trace(&mut log, &DEFAULT, "readme.md");
    assert_eq!(t.first_loss, Some("ledger"), "{}", t.report());
    let reason = t.reason.clone().unwrap_or_default();
    assert!(
        reason.contains("injected context") && reason.contains("ide_opened_file"),
        "{}",
        t.report()
    );

    // Capture: in the transcript, never in a hook payload.
    let transcript = log.env.dir.path().join("t.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"assistant\",\"message\":{\"content\":\"I suspect retry_backoff_jitter\"}}\n",
    )
    .expect("transcript");
    log.db
        .conn
        .execute(
            "UPDATE sessions SET transcript_path = ?1",
            [transcript.to_string_lossy()],
        )
        .expect("path");
    let t = trace(&mut log, &DEFAULT, "retry_backoff_jitter");
    assert_eq!(t.first_loss, Some("normalized_state"), "{}", t.report());
    assert!(
        t.reason
            .as_deref()
            .unwrap_or("")
            .starts_with("not captured"),
        "{}",
        t.report()
    );
}

/// K: what `velra restore` stages agrees with the preview on every protected
/// marker, and the trace's restore layer renders with restore's own framing.
#[test]
fn k_preview_and_restore_agree_on_protected_state() {
    let mut log = Log::new();
    crowded_payment_session(&mut log, true);
    let checkpoint = log.checkpoint();
    let session = log.session();
    let now = log.ts + 10_000;
    let staged = restore::build(
        &log.db.conn,
        &RestoreRequest {
            workspace_id: &log.env.project_id(),
            workspace_root: "/workspace",
            source_session_id: &session,
            now_ms: now,
        },
        &DEFAULT,
    )
    .expect("restore");
    assert!(staged
        .capsule
        .contains(&format!("checkpoint=\"{checkpoint}\"")));
    let snap = snapshot::build(&log.db.conn, &session, &meta("preview", now)).expect("snap");
    let preview = render::render(&snap, &DEFAULT).text;
    for marker in [TASK_HEAD, TEST_ID, RETRY, "Do not modify the tests."] {
        assert!(
            preview.contains(marker),
            "preview lacks {marker}:\n{preview}"
        );
        assert!(
            staged.capsule.contains(marker),
            "restore lacks {marker}:\n{}",
            staged.capsule
        );
    }

    let m = meta("preview", now);
    let inputs = TraceInputs {
        meta: &m,
        cfg: &DEFAULT,
        staged: Some(&staged.capsule),
    };
    let t = provenance::trace_marker(&log.db.conn, &session, &inputs, TASK_HEAD).expect("trace");
    let restore_layer = t
        .layers
        .iter()
        .find(|l| l.layer == "restore")
        .expect("layer");
    assert!(
        restore_layer
            .evidence
            .iter()
            .any(|e| e.contains(&checkpoint)),
        "{}",
        t.report()
    );
    assert_eq!(t.first_loss, None, "{}", t.report());
}

// --------------------------------------------------------- headless lifecycle

/// The product lifecycle end to end through the real binary: an IDE-prefixed
/// prompt and the session's tool events arrive through the hooks, `velra
/// restore` stages a capsule, and a new session's `SessionStart(startup)` hook
/// delivers it. Headless: this proves what the hooks and the CLI do with the
/// payloads VS Code sends, not how the VS Code extension behaves.
#[test]
fn headless_lifecycle_carries_the_objective_and_the_rejected_route() {
    let env = Env::new();
    env.write_file(RETRY, ORIGINAL);
    env.write_file(
        "tests/test_retry.py",
        "def test_retry_preserves_idempotency_key(): ...\n",
    );
    let retry = env.project.join(RETRY);
    let hook = |event: &str, name: &str, fill: &dyn Fn(&mut serde_json::Value)| {
        let mut p = env.base_payload(name);
        fill(&mut p);
        env.hook(event, &p).assert_contract();
    };

    hook("session-start", "SessionStart", &|p| {
        p["source"] = json!("startup")
    });
    hook("user-prompt-submit", "UserPromptSubmit", &|p| {
        p["prompt"] = json!(prompt_text(true, TASK));
    });
    hook("post-tool-use-failure", "PostToolUseFailure", &|p| {
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!("t1");
        p["tool_input"] = json!({ "command": "python -m pytest tests/test_retry.py -v" });
        p["error"] = json!(format!("Exit code 1\n{PYTEST_FAIL}"));
    });
    hook("pre-tool-use", "PreToolUse", &|p| {
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!("t2");
        p["tool_input"] = json!({ "file_path": retry, "old_string": "def build_retry_payload(request: dict) -> dict:", "new_string": "_last_idempotency_key = None\n\ndef build_retry_payload(request: dict) -> dict:" });
    });
    std::fs::write(&retry, PATCHED).expect("edit");
    hook("post-tool-use", "PostToolUse", &|p| {
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!("t2");
        p["tool_input"] = json!({ "file_path": retry, "old_string": "def build_retry_payload(request: dict) -> dict:", "new_string": "_last_idempotency_key = None\n\ndef build_retry_payload(request: dict) -> dict:" });
        p["tool_response"] = json!({ "filePath": retry, "originalFile": ORIGINAL });
    });
    let restore_cmd =
        "git restore src/payments/retry.py && python -m pytest tests/test_retry.py -v";
    hook("pre-tool-use", "PreToolUse", &|p| {
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!("t3");
        p["tool_input"] = json!({ "command": restore_cmd });
    });
    std::fs::write(&retry, ORIGINAL).expect("restore");
    hook("post-tool-use-failure", "PostToolUseFailure", &|p| {
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!("t3");
        p["tool_input"] = json!({ "command": restore_cmd });
        p["error"] = json!(format!("Exit code 1\n{PYTEST_FAIL}"));
    });
    env.drain();

    let out = env
        .cmd()
        .args(["restore", "--session", &env.session, "--json"])
        .output()
        .expect("restore");
    assert!(out.status.success(), "{out:?}");

    let mut start = env.base_payload("SessionStart");
    start["source"] = json!("startup");
    start["session_id"] = json!("destination-session");
    let delivered = env.hook("session-start", &start);
    delivered.assert_contract();
    let value = delivered.json().expect("delivery");
    let capsule = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("a capsule is delivered: {value}"));

    assert!(
        first_message_body(capsule).starts_with(TASK_HEAD),
        "{capsule}"
    );
    assert!(!capsule.contains("ide_opened_file"), "{capsule}");
    assert!(capsule.contains(TEST_ID), "{capsule}");
    assert!(
        capsule.contains(&format!("- {RETRY} | 1 edit(s) | reverted via")),
        "{capsule}"
    );
    assert!(capsule.contains(WORKAROUND), "{capsule}");
    assert!(capsule.contains("Do not modify the tests."), "{capsule}");
}
