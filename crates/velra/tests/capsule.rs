//! E1–E4: the Continuation Capsule (§16).

mod common;

use common::{golden_capsule, Log};
use proptest::prelude::*;
use velra_core::git::GitInfo;
use velra_core::model::{CommandKind, Mechanism, Outcome, Trigger};
use velra_core::render::{
    self, AttemptView, CommandRef, DeadEndView, FailureView, IntentView, NextTarget, RenderConfig,
    Snapshot, WorkingFileView, ABSOLUTE_MAX_CHARS, HARD_CEILING_TOKENS,
};

const CAUSAL_WORDS: [&str; 4] = ["caused", "because", "due to", "led to"];

fn check(capsule: &str) {
    assert!(
        capsule.starts_with("<VELRA_CONTINUATION v=\"1\""),
        "{capsule}"
    );
    assert!(capsule.ends_with("</VELRA_CONTINUATION>"), "{capsule}");
    assert!(!capsule.contains('\r'), "line endings must be \\n only");
    assert!(
        !capsule.contains('\\'),
        "paths render with forward slashes: {capsule}"
    );
    // E3: no causal language.
    for word in CAUSAL_WORDS {
        assert!(
            !capsule.contains(word),
            "capsule must not contain {word:?}:\n{capsule}"
        );
    }
    let tokens = velra_core::text::estimate_tokens(capsule);
    assert!(
        tokens <= HARD_CEILING_TOKENS,
        "{tokens} tokens exceeds the hard ceiling"
    );
    assert!(capsule.chars().count() <= ABSOLUTE_MAX_CHARS);
}

// ------------------------------------------------------------- E1: goldens

#[test]
fn e1_empty_state() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.read("src/a.rs");
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("empty_state", &capsule);
}

#[test]
fn e1_objective_only() {
    let mut log = Log::new();
    log.prompt("fix the flaky logout test and keep the session cookie behaviour intact");
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("objective_only", &capsule);
}

#[test]
fn e1_failure_only() {
    let mut log = Log::new();
    log.prompt("fix the flaky logout test and keep the session cookie behaviour intact");
    log.command_fail(
        "pytest tests/test_auth.py -x",
        1,
        "E   assert response.cookies[\"session\"] is None\ntests/test_auth.py:88: AssertionError\n1 failed, 11 passed",
    );
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("failure_only", &capsule);
}

#[test]
fn e1_dead_end_inverse_edit() {
    let mut log = Log::new();
    log.env
        .write_file("src/auth/session.py", "max_age = None\n");
    log.prompt("stop the session cookie from surviving logout");
    log.edit("src/auth/session.py", "max_age = 0\n");
    log.edit("src/auth/session.py", "max_age = None\n");
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("dead_end_inverse_edit", &capsule);
}

#[test]
fn e1_dead_end_git_command() {
    let mut log = Log::new();
    log.env
        .write_file("src/auth/session.py", "max_age = None\n");
    log.prompt("stop the session cookie from surviving logout");
    log.edit("src/auth/session.py", "max_age = 0\n");
    log.command_fail(
        "pytest tests/test_auth.py -x",
        1,
        "FAILED tests/test_auth.py::test_logout\n1 failed",
    );
    log.git_restore(
        "git restore src/auth/session.py",
        &[("src/auth/session.py", "max_age = None\n")],
    );
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("dead_end_git_command", &capsule);
}

#[test]
fn e1_dead_end_rewrite() {
    let mut log = Log::new();
    log.env
        .write_file("src/config.ts", "export const retries = 1;\n");
    log.prompt("increase the retry count and see whether the suite stabilises");
    log.edit("src/config.ts", "export const retries = 5;\n");
    log.write_tool("src/config.ts", "export const retries = 1;\n");
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("dead_end_rewrite", &capsule);
}

#[test]
fn e1_dead_end_external() {
    let mut log = Log::new();
    log.env
        .write_file("src/config.ts", "export const retries = 1;\n");
    log.prompt("increase the retry count and see whether the suite stabilises");
    log.edit("src/config.ts", "export const retries = 5;\n");
    log.reduce();
    log.env
        .write_file("src/config.ts", "export const retries = 1;\n");
    log.stop();
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("dead_end_external", &capsule);
}

#[test]
fn e1_subagent() {
    let mut log = Log::new();
    log.env.write_file("src/parser.rs", "v0\n");
    log.prompt("have a subagent explore the parser rewrite while I review tests");
    log.edit_as("src/parser.rs", "v1\n", "agent-7");
    log.edit_as("src/parser.rs", "v0\n", "agent-7");
    let capsule = log.capsule();
    check(&capsule);
    assert!(capsule.contains("(subagent)"));
    golden_capsule("subagent", &capsule);
}

#[test]
fn e1_git_repository() {
    let mut log = Log::new();
    log.env.init_git(
        "feat/auth-refactor",
        "4f2a1c9e0b3d5a7c9e1f3a5b7d9f1e3a5c7b9d1f",
    );
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("refactor the auth module without changing behaviour");
    log.edit("src/a.rs", "v1\n");
    let capsule = log.capsule();
    check(&capsule);
    assert!(
        capsule.contains("feat/auth-refactor @ 4f2a1c9"),
        "{capsule}"
    );
    golden_capsule("git_repository", &capsule);
}

#[test]
fn e1_partial_capture() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("refactor the auth module without changing behaviour");
    log.edit("src/a.rs", "v1\n");
    let mut snapshot = log.snapshot();
    snapshot.partial = true;
    let capsule = render::render(&snapshot, &RenderConfig::default()).text;
    check(&capsule);
    assert!(capsule.contains("(partial capture)"));
    golden_capsule("partial_capture", &capsule);
}

#[test]
fn e1_sensitive_path() {
    let mut log = Log::new();
    log.env.write_file(".env", "API_KEY=old\n");
    log.prompt("rotate the API key in the environment file and redeploy");
    log.edit(".env", "API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwx\n");
    log.edit(".env", "API_KEY=old\n");
    let capsule = log.capsule();
    check(&capsule);
    assert!(capsule.contains(".env"), "the path is shown");
    assert!(
        !capsule.contains("sk-ant"),
        "no excerpt for sensitive paths:\n{capsule}"
    );
    assert!(
        !capsule.contains("API_KEY="),
        "no excerpt for sensitive paths:\n{capsule}"
    );
    golden_capsule("sensitive_path", &capsule);
}

#[test]
fn e1_long_prompts_are_truncated() {
    let mut log = Log::new();
    log.prompt(&format!(
        "refactor the authentication stack {}",
        "and keep every existing behaviour intact ".repeat(20)
    ));
    log.prompt(&format!(
        "now also check {}",
        "the session middleware and its tests ".repeat(20)
    ));
    let capsule = log.capsule();
    check(&capsule);
    golden_capsule("long_prompts", &capsule);
}

#[test]
fn e1_many_files() {
    let mut log = Log::new();
    log.prompt("touch every module in the package and keep the suite green");
    for i in 0..12 {
        let path = format!("src/mod{i}.rs");
        log.env.write_file(&path, "v0\n");
        log.edit(&path, &format!("v{i}\n"));
        log.read(&path);
    }
    log.command_fail(
        "cargo test",
        101,
        "error[E0308]: mismatched types\n  --> src/mod3.rs:1:1\n",
    );
    let capsule = log.capsule();
    check(&capsule);
    assert_eq!(
        capsule.matches("\n- src/mod").count(),
        4 + 8,
        "4 attempts + 8 working files"
    );
    golden_capsule("many_files", &capsule);
}

#[test]
fn e1_nested_paths() {
    let mut log = Log::new();
    log.prompt("fix the nested module the windows build complains about");
    log.env
        .write_file("src/deeply/nested/module/handler.rs", "v0\n");
    log.edit("src/deeply/nested/module/handler.rs", "v1\n");
    let capsule = log.capsule();
    check(&capsule);
    assert!(capsule.contains("src/deeply/nested/module/handler.rs"));
    golden_capsule("nested_paths", &capsule);
}

#[test]
fn e1_full_state() {
    let mut log = Log::new();
    log.env
        .init_git("main", "0123456789abcdef0123456789abcdef01234567");
    log.env
        .write_file("src/auth/session.py", "max_age = None\n");
    log.env
        .write_file("src/auth/cookies.py", "secure = False\n");
    log.env
        .write_file("tests/test_auth.py", "def test_logout(): ...\n");
    log.prompt("fix the flaky logout test and keep the session cookie behaviour intact");
    log.prompt("subtask: start with the cookie helper");
    log.read("tests/test_auth.py");
    log.edit("src/auth/session.py", "max_age = 0\n");
    log.command_fail(
        "pytest tests/test_auth.py -x",
        1,
        "E   assert response.cookies[\"session\"] is None\ntests/test_auth.py:88: AssertionError\n1 failed, 11 passed",
    );
    log.git_restore(
        "git restore src/auth/session.py",
        &[("src/auth/session.py", "max_age = None\n")],
    );
    log.edit("src/auth/cookies.py", "secure = True\n");
    log.prompt("what else could be keeping the cookie alive?");
    let capsule = log.capsule();
    check(&capsule);
    for section in [
        "[ROOT_TASK_OBJECTIVE]",
        "[ACTIVE_SUBTASK]",
        "[LATEST_REQUEST]",
        "[STATUS]",
        "[ACTIVE_FAILURE]",
        "[DEAD_ENDS]",
        "[RECENT_ATTEMPTS]",
        "[WORKING_FILES]",
        "[NEXT_KNOWN_TARGET]",
        "[RECOVERY]",
    ] {
        assert!(capsule.contains(section), "missing {section}:\n{capsule}");
    }
    golden_capsule("full_state", &capsule);
}

// -------------------------------------------------------------- E4: sources

#[test]
fn e4_every_line_traces_to_a_row() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("fix the flaky logout test and keep the cookie behaviour intact");
    log.edit("src/a.rs", "v1\n");
    log.command_fail("pytest -x", 1, "FAILED tests/test_a.py::test_x\n1 failed");
    log.edit("src/a.rs", "v0\n");
    let snapshot = log.snapshot();
    let traced = render::render_traced(&snapshot, &RenderConfig::default()).text;

    let exempt = |line: &str| {
        line.starts_with("<VELRA_CONTINUATION")
            || line == "</VELRA_CONTINUATION>"
            || line == "[CONTEXT]"
            || line.starts_with("Velra is a local tool")
            || line == "[RECOVERY]"
            || line.starts_with("Full detail for any section:")
    };
    for line in traced.lines() {
        if exempt(line) {
            continue;
        }
        assert!(line.contains("#src="), "untraceable line: {line}");
    }
}

// --------------------------------------------------------------- E2: budget

fn arb_text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![Just('a'), Just(' '), Just('/'), Just('.'), Just('9')],
        0..max,
    )
    .prop_map(|c| c.into_iter().collect())
}

prop_compose! {
    fn arb_snapshot()(
        root in prop::option::of(arb_text(600)),
        subtask in prop::option::of(arb_text(400)),
        latest in prop::option::of(arb_text(400)),
        branch in prop::option::of(arb_text(120)),
        edits in 0u32..9999,
        failure_lines in prop::collection::vec(arb_text(300), 0..12),
        command in arb_text(400),
        dead_ends in 0usize..6,
        attempts in 0usize..6,
        files in 0usize..12,
        partial in any::<bool>(),
    ) -> Snapshot {
        let intent = |text: Option<String>, id: i64| text.map(|t| IntentView { id, text: t, ts_ms: common::BASE_MS });
        Snapshot {
            checkpoint_id: "ckpt_01PROPTEST".into(),
            created_ms: common::BASE_MS,
            trigger: Trigger::Auto,
            partial,
            preview: false,
            session_id: "s".into(),
            project_id: "p".into(),
            epoch: 1,
            root: intent(root, 1),
            subtask: intent(subtask, 2),
            latest: intent(latest, 3),
            git: branch.map(|b| GitInfo { branch: Some(b), head: Some("a".repeat(40)) }),
            edit_count: edits,
            last_test: Some(CommandRef { id: 1, command: "pytest".into(), outcome: Outcome::Fail }),
            failure: Some(FailureView {
                id: 1,
                kind: CommandKind::Test,
                command,
                exit_code: Some(1),
                excerpt: failure_lines,
                ts_ms: common::BASE_MS,
            }),
            failing_count: 1,
            dead_ends: (0..dead_ends).map(|i| DeadEndView {
                id: i as i64,
                path: format!("src/{}.rs", "d".repeat(60)),
                subagent: i % 2 == 0,
                edit_ids: vec![1, 2],
                mechanism: Mechanism::GitCommand,
                command: Some("git restore ".to_string() + &"x".repeat(200)),
                resolved_ms: common::BASE_MS,
                minus: Some("-".repeat(160)),
                plus: Some("+".repeat(160)),
                observed_after: Some(CommandRef { id: 2, command: "y".repeat(200), outcome: Outcome::Fail }),
            }).collect(),
            dead_end_total: dead_ends as u32,
            attempts: (0..attempts).map(|i| AttemptView {
                edit_id: i as i64,
                path: format!("src/attempt{}.rs", "a".repeat(60)),
                subagent: false,
                added: Some(12),
                removed: None,
                ts_ms: common::BASE_MS,
                afterward: None,
            }).collect(),
            working_files: (0..files).map(|i| WorkingFileView {
                path: format!("src/file{i}{}.rs", "w".repeat(60)),
                edits: 3,
                reads: 9,
                in_failure: true,
            }).collect(),
            next_target: Some(NextTarget { rule: "failure-location", target: "src/x.rs:12".into(), source: "commands:1".into() }),
            tz_offset_secs: 0,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// E2: the hard ceiling and character cap hold for every state.
    #[test]
    fn e2_budget_always_holds(snapshot in arb_snapshot()) {
        let rendered = render::render(&snapshot, &RenderConfig::default());
        prop_assert!(rendered.tokens <= HARD_CEILING_TOKENS, "{} tokens", rendered.tokens);
        prop_assert!(rendered.text.chars().count() <= ABSOLUTE_MAX_CHARS);
        prop_assert!(rendered.text.ends_with("</VELRA_CONTINUATION>"));
        // Sections that are never removed.
        prop_assert!(rendered.text.contains("[CONTEXT]"));
        prop_assert!(rendered.text.contains("[STATUS]"));
        prop_assert!(rendered.text.contains("[RECOVERY]"));
    }

    /// E2: a state that fits the target is rendered in full.
    #[test]
    fn e2_small_states_stay_under_target(root in arb_text(120)) {
        let mut snapshot = arb_snapshot_minimal();
        snapshot.root = Some(IntentView { id: 1, text: root, ts_ms: common::BASE_MS });
        let rendered = render::render(&snapshot, &RenderConfig::default());
        prop_assert!(rendered.tokens <= 800, "{} tokens", rendered.tokens);
        prop_assert_eq!(rendered.steps, 0);
    }
}

fn arb_snapshot_minimal() -> Snapshot {
    Snapshot {
        checkpoint_id: "ckpt_01MIN".into(),
        created_ms: common::BASE_MS,
        trigger: Trigger::Manual,
        partial: false,
        preview: false,
        session_id: "s".into(),
        project_id: "p".into(),
        epoch: 1,
        root: None,
        subtask: None,
        latest: None,
        git: None,
        edit_count: 0,
        last_test: None,
        failure: None,
        failing_count: 0,
        dead_ends: vec![],
        dead_end_total: 0,
        attempts: vec![],
        working_files: vec![],
        next_target: None,
        tz_offset_secs: 0,
    }
}

#[test]
fn e2_truncation_ladder_runs_in_order() {
    let mut snapshot = arb_snapshot_minimal();
    snapshot.working_files = (0..8)
        .map(|i| WorkingFileView {
            path: format!("src/file{i}.rs"),
            edits: 2,
            reads: 3,
            in_failure: false,
        })
        .collect();
    snapshot.attempts = (0..4)
        .map(|i| AttemptView {
            edit_id: i,
            path: format!("src/attempt{i}.rs"),
            subagent: false,
            added: Some(4),
            removed: Some(2),
            ts_ms: common::BASE_MS,
            afterward: None,
        })
        .collect();
    snapshot.root = Some(IntentView {
        id: 1,
        text: "r".repeat(240),
        ts_ms: common::BASE_MS,
    });

    let full = render::render(&snapshot, &RenderConfig::default());
    assert_eq!(full.steps, 0, "fits the default budget");
    assert_eq!(full.text.matches("\n- src/file").count(), 8);

    // A tight budget drops working files first, then attempts.
    let tight = render::render(&snapshot, &RenderConfig { budget_tokens: 120 });
    assert!(tight.steps > 0);
    assert!(tight.text.matches("\n- src/file").count() <= 4);
    assert!(tight.tokens <= HARD_CEILING_TOKENS);
}

/// E2/H3: the budget estimator must stay at or above what the real tokenizer
/// charges, or the truncation ladder stops while the block is still over
/// budget.
///
/// The fixtures are two `<VELRA_CONTINUATION>` blocks Claude Code actually
/// received during the v0.1 benchmark, alongside the token count each one
/// really cost — measured, not modelled, by differencing billed input tokens
/// between two otherwise identical minimal sessions. With the original
/// `ceil(chars / 3.2)` estimator those blocks read as 643 and 743 tokens
/// against a declared budget of 800, so nothing was ever trimmed and Velra
/// shipped 951 and 1,147.
#[test]
fn estimator_is_above_real_tokenizer_counts() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tokenizer");
    let measured: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("measured.json")).expect("measured"),
    )
    .expect("json");
    let mut checked = 0;
    for (name, entry) in measured.as_object().expect("object") {
        let text = std::fs::read_to_string(dir.join(name)).expect("fixture");
        let real = entry["measured_tokens"].as_u64().expect("count") as u32;
        let est = velra_core::text::estimate_tokens(&text);
        assert!(
            est >= real,
            "{name}: estimate {est} is below the {real} tokens it really costs"
        );
        assert!(
            est <= real + real / 4,
            "{name}: estimate {est} overshoots {real} by more than a quarter, \
             which spends budget on content the capsule could have carried"
        );
        checked += 1;
    }
    assert_eq!(checked, 2, "both fixtures were checked");
}

/// H2: a session in which an approach was tried and discarded must render a
/// `[DEAD_ENDS]` section. This is the product feature the benchmark found
/// missing from a delivered capsule, so it is asserted end to end rather than
/// only at the unit level.
#[test]
fn a_discarded_approach_always_reaches_the_capsule() {
    let mut log = Log::new();
    log.env.write_file("src/money.py", "ROUND_HALF_UP\n");
    log.prompt("the gap is one cent, so try the rounding hypothesis first");
    log.edit("src/money.py", "ROUND_HALF_EVEN\n");
    log.command_fail(
        "python -m pytest -q",
        1,
        "FAILED tests/test_engine.py::test_exact_payment\n2 failed, 4 passed",
    );
    log.git_restore(
        "git restore src/money.py",
        &[("src/money.py", "ROUND_HALF_UP\n")],
    );

    let capsule = log.capsule();
    assert!(capsule.contains("[DEAD_ENDS]"), "{capsule}");
    assert!(capsule.contains("src/money.py"), "{capsule}");
    assert!(
        capsule.contains("reverted via `git restore src/money.py`"),
        "the mechanism and the command that caused it:\n{capsule}"
    );
}

/// The `[WORKING_FILES]` defect the benchmark found, reproduced offline.
///
/// In `saturated-velra-r1` the agent spent eight turns auditing 84 unrelated
/// modules after finding the failure. Every one of those reads scored the same
/// single point as the three modules the bug actually lived in, ties went to
/// whatever was touched most recently, and the capsule listed
/// `validation/invoice_number_gb.py` and friends while `engine.py` and
/// `rules.py` did not appear at all.
#[test]
fn an_unrelated_read_sweep_does_not_evict_the_files_the_task_is_about() {
    let mut log = Log::new();
    for f in ["src/money.py", "src/rules.py", "src/engine.py"] {
        log.env.write_file(f, "x = 1\n");
    }
    log.prompt("a test is failing by one cent; find out why and fix it");
    log.command_fail(
        "python -m pytest -q",
        1,
        "E       AssertionError: assert 'UNDERPAID' == 'SETTLED'\n\
         tests/test_engine.py:54: AssertionError\n1 failed, 5 passed",
    );
    // The three modules the task is about, read early.
    for f in ["src/money.py", "src/rules.py", "src/engine.py"] {
        log.read(f);
    }
    // The rounding hypothesis, tried and discarded.
    log.edit("src/money.py", "x = 2\n");
    log.git_restore("git restore src/money.py", &[("src/money.py", "x = 1\n")]);

    // Then an audit: forty unrelated modules, each read exactly once, each of
    // them more recently than anything above.
    for i in 0..40 {
        let path = format!("src/validation/rule_{i:02}.py");
        log.env.write_file(&path, "y = 1\n");
        log.read(&path);
    }

    let capsule = log.capsule();
    let working: Vec<&str> = capsule
        .lines()
        .skip_while(|l| !l.starts_with("[WORKING_FILES]"))
        .skip(1)
        .take_while(|l| l.starts_with("- "))
        .collect();
    assert!(!working.is_empty(), "{capsule}");
    for f in ["src/money.py", "src/engine.py", "src/rules.py"] {
        assert!(
            working.iter().any(|l| l.contains(f)),
            "{f} must survive the sweep, got {working:#?}\n{capsule}"
        );
    }
    // The sweep may fill what is left, but not the front of the list.
    assert!(
        working[0].contains("src/money.py"),
        "the discarded file ranks first: {working:#?}"
    );
}
