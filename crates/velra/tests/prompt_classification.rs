//! Prompt classification through the real hook path (Phase 1 hardening).
//!
//! Every test here drives `velra hook user-prompt-submit` with stdin exactly as
//! Claude Code delivers it, then reduces and reads the classification back
//! from the ledger. That layer matters: `Log::prompt` in the shared harness
//! appends a payload straight into `events`, so it never exercises what the
//! hook does to a prompt before storing it -- the 4 KiB cap and redaction --
//! and every defect below lived in exactly that step.
//!
//! What is asserted is the *classification decision*: which text became the
//! objective, the subtask, the latest message or a constraint, and which text
//! did not. None of this says anything about what a model does with it.

mod common;

use common::Env;
use serde_json::json;
use velra_core::event::limits;

/// Submits one prompt through the real hook binary.
fn submit(env: &Env, n: usize, prompt: &str) {
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!(prompt);
    // A distinct prompt id per submission: the hook's dedupe key otherwise
    // rests on the millisecond clock alone.
    p["prompt_id"] = json!(format!("prompt-{n}"));
    env.hook("user-prompt-submit", &p).assert_contract();
}

/// `(level, text, live)` for every intent row of the session, in id order,
/// once `expected_events` prompts are visible and reduced.
fn intents(env: &Env, expected_events: usize) -> Vec<(String, String, bool)> {
    env.drain_and_load_events(expected_events);
    env.drain();
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare(
            "SELECT level, text, superseded_ms IS NULL FROM intents \
             WHERE session_id = ?1 ORDER BY id",
        )
        .expect("prepare");
    stmt.query_map([&env.session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn live(rows: &[(String, String, bool)], level: &str) -> Option<String> {
    rows.iter()
        .rev()
        .find(|(l, _, live)| l == level && *live)
        .map(|(_, t, _)| t.clone())
}

/// `(text, prompt_ordinal)` of every constraint row, in id order.
fn constraints(env: &Env) -> Vec<(String, i64)> {
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare("SELECT text, prompt_ordinal FROM constraints WHERE session_id = ?1 ORDER BY id")
        .expect("prepare");
    stmt.query_map([&env.session], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The `prompt` field of every stored UserPromptSubmit payload.
fn stored_prompts(env: &Env) -> Vec<String> {
    env.drain_and_load_events(1)
        .into_iter()
        .filter(|e| e.hook_event == "UserPromptSubmit")
        .map(|e| e.json()["prompt"].as_str().unwrap_or("").to_string())
        .collect()
}

/// An `<ide_selection>` block of roughly `bytes` bytes, shaped like the one
/// the VS Code extension sends.
fn ide_selection(bytes: usize) -> String {
    let head = "<ide_selection>The user selected the lines 1 to 400 from \
                c:\\repo\\src\\payments\\gateway.py:\n";
    let tail = "\n\nThis may or may not be related to the current task.</ide_selection>";
    let mut body = String::new();
    let mut i = 0;
    while head.len() + body.len() + tail.len() < bytes {
        body.push_str(&format!(
            "    charge_{i:04} = gateway.charge(amount, currency)\n"
        ));
        i += 1;
    }
    format!("{head}{body}{tail}")
}

const TASK: &str =
    "Fix the retry bug in src/payments/retry.py so the idempotency key survives a retry.";

// ------------------------------------------------ 1. the cap and injection

/// The live shape that matters most: a selection larger than the whole
/// prompt budget, glued in front of the user's task. The block is context,
/// the task is the objective -- whatever the sizes.
#[test]
fn a_selection_larger_than_the_prompt_cap_never_becomes_the_objective() {
    let env = Env::new();
    let block = ide_selection(limits::PROMPT + 1_500);
    assert!(block.len() > limits::PROMPT);
    submit(&env, 1, &format!("{block}\n{TASK}"));

    let rows = intents(&env, 1);
    let root = live(&rows, "ROOT").expect("the task must become the objective");
    assert_eq!(root, TASK);
    assert!(
        rows.iter().all(|(_, t, _)| !t.contains("ide_selection")),
        "injected context reached an intent: {rows:?}"
    );
    // What is stored is still classifiable as it was delivered: every block
    // it keeps is a complete element, and the task is intact.
    let stored = &stored_prompts(&env)[0];
    assert!(stored.len() <= limits::PROMPT, "{}", stored.len());
    assert_eq!(velra_core::prompt::authored(stored), TASK);
}

/// Sizes swept across the cap, so the closing tag lands just inside, exactly
/// on, and just outside the boundary. Truncation must never turn injected
/// context into the user's words.
#[test]
fn the_cap_boundary_never_changes_what_counts_as_authored() {
    let task = "Please fix the flaky login test in tests/test_auth.py.";
    // Candidate prompt sizes around the cap, measured on the whole prompt.
    for total in [
        limits::PROMPT - 64,
        limits::PROMPT - 1,
        limits::PROMPT,
        limits::PROMPT + 1,
        limits::PROMPT + 17,
        limits::PROMPT + task.len(),
        limits::PROMPT * 2,
    ] {
        let env = Env::new();
        let block = ide_selection(total.saturating_sub(task.len() + 1));
        let prompt = format!("{block}\n{task}");
        submit(&env, 1, &prompt);
        let rows = intents(&env, 1);
        assert_eq!(
            live(&rows, "ROOT").as_deref(),
            Some(task),
            "prompt of {} bytes: {rows:?}",
            prompt.len()
        );
    }
}

/// The closing tag split by the cap itself: the block ends one byte, a few
/// bytes, and a whole tag past the budget.
#[test]
fn a_closing_tag_cut_by_the_cap_does_not_leak_the_block() {
    let close = "</ide_selection>";
    for overhang in [1usize, 5, close.len() - 1, close.len(), close.len() + 1] {
        let env = Env::new();
        // Build a block whose closing tag straddles byte PROMPT by `overhang`.
        let mut block = ide_selection(limits::PROMPT);
        while block.len() < limits::PROMPT - close.len() + overhang {
            block.insert(block.len() - close.len(), '#');
        }
        while block.len() > limits::PROMPT - close.len() + overhang {
            let at = block.len() - close.len() - 1;
            block.remove(at);
        }
        let prompt = format!("{block}\n{TASK}");
        submit(&env, 1, &prompt);
        let rows = intents(&env, 1);
        assert_eq!(
            live(&rows, "ROOT").as_deref(),
            Some(TASK),
            "overhang {overhang}"
        );
        assert!(rows.iter().all(|(_, t, _)| !t.contains("charge_")));
    }
}

/// Multi-byte text and CRLF line ends at the boundary: the cut is on a char
/// boundary and the block is still recognised or dropped whole.
#[test]
fn unicode_and_crlf_near_the_cap_are_handled_whole() {
    let env = Env::new();
    let mut body = String::new();
    while body.len() < limits::PROMPT {
        body.push_str("  r\u{e9}sum\u{e9} = \u{8cc7}\u{6599}\u{1f4b3}\r\n");
    }
    let prompt = format!("<ide_selection>{body}</ide_selection>\r\n{TASK}");
    submit(&env, 1, &prompt);
    let rows = intents(&env, 1);
    assert_eq!(live(&rows, "ROOT").as_deref(), Some(TASK), "{rows:?}");
}

/// Redaction runs before anything is stored. A secret inside an injected
/// block must be removed without taking the block's closing tag with it --
/// a greedy value match that swallowed `</system-reminder>` would leave an
/// unclosed element, and an unclosed element is the user's text.
#[test]
fn redacting_a_secret_inside_a_block_keeps_the_block_injected() {
    // The first is caught by a specific detector; the second only by the
    // generic `key=value` rule, whose value class admits `<`, `/` and `>`.
    for (n, (secret, body)) in [
        (
            "4eC39HqLyjWDarjtT1zdp7dc",
            "api_key=sk_live_4eC39HqLyjWDarjtT1zdp7dc",
        ),
        ("correct-horse-battery", "password=correct-horse-battery"),
    ]
    .into_iter()
    .enumerate()
    {
        let env = Env::new();
        let prompt = format!("<system-reminder>{body}</system-reminder>\n{TASK}");
        submit(&env, n, &prompt);
        let rows = intents(&env, 1);
        assert_eq!(live(&rows, "ROOT").as_deref(), Some(TASK), "{rows:?}");
        let stored = &stored_prompts(&env)[0];
        assert!(!stored.contains(secret), "{stored}");
        assert!(stored.contains("</system-reminder>"), "{stored}");
    }
}

/// Text after the user's words: a trailing reminder larger than the budget
/// is dropped whole rather than cut open.
#[test]
fn a_large_trailing_reminder_never_joins_the_users_text() {
    let env = Env::new();
    let reminder = format!(
        "<system-reminder>{}</system-reminder>",
        "Background context the harness attached. ".repeat(150)
    );
    assert!(reminder.len() > limits::PROMPT);
    submit(&env, 1, &format!("{TASK}\n\n{reminder}"));
    let rows = intents(&env, 1);
    assert_eq!(live(&rows, "ROOT").as_deref(), Some(TASK), "{rows:?}");
}

// ------------------------------------------------- 2. slash-prefixed text

#[test]
fn a_leading_path_is_the_users_text_not_a_slash_command() {
    for (n, prompt) in [
        "/tmp/foo/build.log shows the linker failing; fix the build script please",
        "/path/to/file.py raises KeyError on line 12, find out why",
        "/c/Users/dev/repo/src/app.ts has a type error after the rename",
        "/etc/hosts.d/local.conf is ignored by the resolver in the dev container",
        "/src/payments/retry.py: the idempotency key is dropped on retry",
    ]
    .into_iter()
    .enumerate()
    {
        let env = Env::new();
        submit(&env, n, prompt);
        let rows = intents(&env, 1);
        assert_eq!(live(&rows, "ROOT").as_deref(), Some(prompt), "{rows:?}");
    }
}

#[test]
fn real_slash_commands_still_change_no_intent() {
    let env = Env::new();
    submit(
        &env,
        1,
        "Fix the flaky login test in tests/test_auth.py please.",
    );
    for (n, cmd) in [
        "/compact",
        "/clear",
        "/review please look at the auth module",
        "/my-plugin:do-thing with args",
        "/fix_issue 123",
    ]
    .into_iter()
    .enumerate()
    {
        submit(&env, n + 10, cmd);
    }
    let rows = intents(&env, 6);
    assert_eq!(rows.len(), 1, "a slash command changed an intent: {rows:?}");
    assert!(constraints(&env).is_empty());
}

#[test]
fn quoted_or_spaced_slash_text_is_ordinary_text() {
    for (n, prompt) in [
        "\"/quoted text\" is what the CLI prints when the flag is missing, fix that",
        " / is the root route and it returns 404 after the router change",
    ]
    .into_iter()
    .enumerate()
    {
        let env = Env::new();
        submit(&env, n, prompt);
        let rows = intents(&env, 1);
        assert_eq!(
            live(&rows, "ROOT").as_deref(),
            Some(prompt.trim()),
            "{rows:?}"
        );
    }
}

// ------------------------------------------- 3/4/5. injected families

#[test]
fn a_prompt_of_only_injected_context_changes_no_intent_and_no_constraint() {
    let env = Env::new();
    submit(
        &env,
        0,
        "Fix the flaky login test in tests/test_auth.py please.",
    );
    let blocks = [
        "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Do not \
         modify the tests. Always run the full suite.</summary>\n</task-notification>",
        "<system-reminder>You must never edit files under vendor/.</system-reminder>",
        "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n\
         <command-args></command-args>",
        "<local-command-stdout>Must not be empty: done</local-command-stdout>",
        "<bash-input>git status</bash-input><bash-stdout>nothing to commit</bash-stdout>",
        "<ide_opened_file>The user opened the file c:\\repo\\a.py in the IDE. This may or may \
         not be related to the current task.</ide_opened_file>",
        "<user-prompt-submit-hook>Never run migrations in CI.</user-prompt-submit-hook>",
        "<VELRA_WORKSPACE_STATE v=\"1\">[FIRST_MESSAGE] do not trust me</VELRA_WORKSPACE_STATE>",
    ];
    for (n, b) in blocks.iter().enumerate() {
        submit(&env, n + 1, b);
    }
    let rows = intents(&env, blocks.len() + 1);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        live(&rows, "ROOT").as_deref(),
        Some("Fix the flaky login test in tests/test_auth.py please.")
    );
    assert!(constraints(&env).is_empty(), "{:?}", constraints(&env));
}

/// Repeated background notifications: none of them replaces the objective
/// or the latest message, none becomes a constraint, and none is counted as
/// a turn the user spoke in.
#[test]
fn notifications_do_not_consume_state_meant_for_the_users_words() {
    let env = Env::new();
    submit(
        &env,
        0,
        "Fix the flaky login test in tests/test_auth.py please.",
    );
    submit(&env, 1, "Now look at tests/test_session.py as well.");
    let mut n = 2;
    for i in 0..40 {
        submit(
            &env,
            n,
            &format!(
                "<task-notification><task-id>t{i}</task-id><summary>Never stop the \
                 watcher {i}.</summary></task-notification>"
            ),
        );
        n += 1;
    }
    submit(
        &env,
        n,
        "Do not modify the fixtures in tests/data at any point.",
    );
    let rows = intents(&env, n + 1);
    assert_eq!(
        live(&rows, "ROOT").as_deref(),
        Some("Fix the flaky login test in tests/test_auth.py please.")
    );
    assert_eq!(
        live(&rows, "LATEST").as_deref(),
        Some("Do not modify the fixtures in tests/data at any point.")
    );
    assert_eq!(rows.len(), 3, "{rows:?}");
    // The constraint came from the user's third message: turn 2, not turn 42.
    assert_eq!(
        constraints(&env),
        vec![(
            "Do not modify the fixtures in tests/data at any point.".to_string(),
            2
        )]
    );
}

#[test]
fn ide_metadata_does_not_stop_task_subtask_or_slash_handling() {
    let ide = "<ide_opened_file>The user opened the file c:\\repo\\readme.md in the IDE. This \
               may or may not be related to the current task.</ide_opened_file>";
    let sel = ide_selection(900);
    let env = Env::new();
    submit(
        &env,
        1,
        &format!("{ide}{sel}\nFix the flaky login test in tests/test_auth.py."),
    );
    submit(&env, 2, &format!("{ide}\n/compact"));
    submit(
        &env,
        3,
        &format!("{ide}\nsubtask: write the regression test first thing"),
    );
    submit(
        &env,
        4,
        &format!("{sel}\ntask: migrate the settings loader to TOML"),
    );
    let rows = intents(&env, 4);
    assert_eq!(
        live(&rows, "ROOT").as_deref(),
        Some("migrate the settings loader to TOML"),
        "{rows:?}"
    );
    // The subtask belonged to the previous epoch; it is recorded, not live
    // in the new one, and it is the user's text without the IDE block.
    assert!(rows
        .iter()
        .any(|(l, t, _)| l == "SUBTASK" && t == "write the regression test first thing"));
    assert!(rows.iter().all(|(_, t, _)| !t.contains("ide_")), "{rows:?}");
}

/// Structure variants. Only a complete element of a known family at the edge
/// is context; everything else stays exactly as the user wrote it.
#[test]
fn malformed_unknown_and_user_written_tags_stay_authored() {
    for (n, prompt) in [
        // Unknown tag.
        "<plan>step one, step two</plan> then fix tests/test_auth.py",
        // Unclosed known tag.
        "<ide_selection>half a selection and the user's task: fix tests/test_auth.py",
        // Mismatched close.
        "<ide_selection>x</ide_opened_file> fix tests/test_auth.py",
        // Whitespace inside the closing tag.
        "<system-reminder>x</system-reminder > fix tests/test_auth.py",
        // Tag-like text in code the user pasted.
        "Why does `<system-reminder>` appear in my output? See src/render.rs.",
        "if a < b && c > d { return; } is wrong in src/cmp.rs, fix it",
        // A reminder quoted in the middle of the user's question.
        "why does <system-reminder>x</system-reminder> show up in src/prompt.rs?",
    ]
    .into_iter()
    .enumerate()
    {
        let env = Env::new();
        submit(&env, n, prompt);
        let rows = intents(&env, 1);
        assert_eq!(live(&rows, "ROOT").as_deref(), Some(prompt), "{rows:?}");
    }
}

/// A block that contains a nested element of its own name is one block, not
/// a block followed by its own tail as the user's text.
#[test]
fn a_nested_block_of_the_same_name_is_removed_whole() {
    let env = Env::new();
    let prompt = format!(
        "<system-reminder>outer <system-reminder>inner</system-reminder> outer tail \
         must never be quoted</system-reminder>\n{TASK}"
    );
    submit(&env, 1, &prompt);
    let rows = intents(&env, 1);
    assert_eq!(live(&rows, "ROOT").as_deref(), Some(TASK), "{rows:?}");
    assert!(constraints(&env).is_empty(), "{:?}", constraints(&env));
}

#[test]
fn case_and_whitespace_variants_of_known_blocks_are_removed() {
    for (n, prompt) in [
        format!("<IDE_OPENED_FILE>x</IDE_OPENED_FILE>{TASK}"),
        format!("\n\n  \t<ide_opened_file>x</ide_opened_file>\r\n\r\n  {TASK}  \n"),
        format!("<system-reminder\n  source=\"hook\">x</system-reminder>{TASK}"),
    ]
    .into_iter()
    .enumerate()
    {
        let env = Env::new();
        submit(&env, n, &prompt);
        let rows = intents(&env, 1);
        assert_eq!(live(&rows, "ROOT").as_deref(), Some(TASK), "{rows:?}");
    }
}

/// A paste is the user's own text, wrapped by the client: it stays authored,
/// and the rules in it are the user's rules.
#[test]
fn pasted_content_is_the_users_text() {
    let env = Env::new();
    let paste = "<pasted_content id=\"9\">\nRefactor the settings loader in src/settings.rs. \
                 Do not change the public API of the loader.\n</pasted_content id=\"9\">";
    submit(&env, 1, paste);
    let rows = intents(&env, 1);
    assert!(live(&rows, "ROOT").is_some_and(|r| r.contains("Refactor the settings loader")));
    assert_eq!(
        constraints(&env)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>(),
        vec!["Do not change the public API of the loader."]
    );
}

// -------------------------------------------- 6. what becomes a constraint

/// A rule the user quotes from somewhere else is not the user's rule.
#[test]
fn a_quoted_instruction_is_not_a_constraint() {
    let env = Env::new();
    submit(
        &env,
        1,
        "The README says \"Never call the payments API directly from the web tier.\" Is that \
         still true now that we have the gateway in src/gateway.py?",
    );
    submit(
        &env,
        2,
        "\u{201c}Do not retry on a 409 from the provider,\u{201d} the old runbook said. Check \
         whether src/payments/retry.py still does that.",
    );
    intents(&env, 2);
    assert!(constraints(&env).is_empty(), "{:?}", constraints(&env));
}

/// Code is not prose: a cue inside a code span or a fenced block is not a
/// rule the user stated.
#[test]
fn a_cue_inside_code_is_not_a_constraint() {
    let env = Env::new();
    submit(
        &env,
        1,
        "This check in src/validate.py is wrong:\n```python\n# must not be None\nassert \
         value is not None, \"value must never be empty\"\n```\nWhy does it reject zero?",
    );
    submit(
        &env,
        2,
        "The docstring says `must always be positive` but src/validate.py accepts -1. Fix it.",
    );
    intents(&env, 2);
    assert!(constraints(&env).is_empty(), "{:?}", constraints(&env));
}

/// A list the user wrote one rule per line, without full stops: each line
/// is its own sentence.
#[test]
fn list_items_without_full_stops_are_separate_constraints() {
    let env = Env::new();
    submit(
        &env,
        1,
        "Migrate the settings loader in src/settings.rs to TOML.\n\nRules:\n- never change the \
         public API of the loader\n- keep the old JSON reader working for one release\n- do \
         not add new dependencies",
    );
    intents(&env, 1);
    let got: Vec<String> = constraints(&env).into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        got,
        vec![
            "never change the public API of the loader",
            "keep the old JSON reader working for one release",
            "do not add new dependencies",
        ],
        "{got:?}"
    );
}

/// Hard-wrapped prose is one sentence across the break: splitting it would
/// quote half a rule.
#[test]
fn a_hard_wrapped_sentence_is_not_split_at_the_wrap() {
    let env = Env::new();
    submit(
        &env,
        1,
        "Migrate the settings loader in src/settings.rs to TOML. Do not\nchange the public API \
         of the loader while doing it.",
    );
    intents(&env, 1);
    let got: Vec<String> = constraints(&env).into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        got,
        vec!["Do not change the public API of the loader while doing it."]
    );
}

// ------------------------------------------------ 8. payment-retry, deep

/// The rejected-approach clause of the payment task, placed well past the
/// objective's longest excerpt. The objective line may be shortened; the
/// rejection is the user's own statement and must survive as state of its
/// own, quoted verbatim with the approach it rejects.
#[test]
fn a_rejected_approach_past_the_objective_excerpt_is_kept_as_state() {
    let env = Env::new();
    let preamble = "We have a failing payment retry test in tests/test_retry.py and the on-call \
                    team has asked for a careful, minimal change. Work on the task, but \
                    intentionally leave it unfinished for a later continuation, because the \
                    reviewer wants to see the investigation before the final patch lands. ";
    let task = format!(
        "{preamble}First inspect the failure and understand the code. Then try a temporary \
         workaround that stores the idempotency key in a module-level variable in \
         src/payments/retry.py. Run the tests. That workaround is considered a rejected \
         approach for this \
         task, so revert it with git restore src/payments/retry.py, rerun the tests, and stop \
         without implementing the final fix. Do not modify the tests."
    );
    let clause_at = task.find("rejected approach").expect("clause");
    // Past the longest objective excerpt the renderer ever prints.
    assert!(clause_at > 240, "{clause_at}");
    submit(&env, 1, &task);
    let rows = intents(&env, 1);
    assert!(live(&rows, "ROOT").is_some());
    let got: Vec<String> = constraints(&env).into_iter().map(|(t, _)| t).collect();
    let rejection = got
        .iter()
        .find(|t| t.contains("rejected approach"))
        .unwrap_or_else(|| panic!("the rejection was not kept as state: {got:?}"));
    // Quoted with its antecedent, so it names what was rejected -- the
    // sentence that describes the workaround, not "Run the tests.", which is
    // the sentence right before the rejection.
    assert!(
        rejection.contains("stores the idempotency key in a module-level variable"),
        "{rejection}"
    );
    assert!(
        rejection.starts_with("Then try a temporary workaround"),
        "{rejection}"
    );
    assert!(
        task.contains(rejection.as_str()),
        "not verbatim: {rejection}"
    );
    assert!(
        got.iter().any(|t| t == "Do not modify the tests."),
        "{got:?}"
    );
}

// --------------------------------------------------- 9. property-style

/// The runs of a stored digest between Velra's `[velra: N bytes not stored]`
/// markers, trimmed.
fn omission_runs(stored: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut rest = stored;
    while let Some(at) = rest.find("[velra: ") {
        runs.push(rest[..at].trim());
        let after = &rest[at..];
        let end = after.find(" bytes not stored]").expect("a marker closes") + 18;
        rest = &after[end..];
    }
    runs.push(rest.trim());
    runs
}

/// Deterministic sweep over prompt shapes around the cap and malformed
/// structure. Properties: the hook never breaks its contract, the stored
/// prompt fits the budget, anything classified as authored is the same text
/// the user wrote (never injected context), and the same input classifies
/// the same way twice.
#[test]
fn classification_properties_hold_over_a_deterministic_sweep() {
    let pieces = [
        "<ide_selection>",
        "</ide_selection>",
        "<system-reminder>",
        "</system-reminder>",
        "<task-notification>",
        "</task-notification>",
        "<ide_opened_file>x</ide_opened_file>",
        "<b>",
        "</b>",
        "`code`",
        "\"quoted\"",
        " fix tests/test_a.py ",
        "\r\n",
        "\u{e9}\u{1f4b3}",
        "Do not touch src/x.rs. ",
    ];
    // A fixed linear congruential generator: the sweep is the same on every run.
    let mut state: u64 = 0x5eed_1234_abcd_0001;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let env = Env::new();
    for round in 0..40usize {
        let mut prompt = String::new();
        let count = 1 + next() % 12;
        for _ in 0..count {
            prompt.push_str(pieces[next() % pieces.len()]);
        }
        // Push most of them over the cap, the filler landing either inside a
        // leading block (the shape that loses the block's closing tag) or at
        // an arbitrary point.
        match round % 3 {
            0 => {
                let filler = "y".repeat(limits::PROMPT);
                prompt = format!("<ide_selection>{filler}</ide_selection>{prompt}");
            }
            1 => {
                let filler = "y".repeat(limits::PROMPT);
                let mut at = (next() % (prompt.len() + 1)).min(prompt.len());
                while !prompt.is_char_boundary(at) {
                    at -= 1;
                }
                prompt.insert_str(at, &filler);
            }
            _ => {}
        }
        let expected = velra_core::prompt::authored(&prompt).to_string();
        submit(&env, round, &prompt);
        // One stored event per round. A hook that panics is fail-open and
        // stores nothing, and comparing against the previous round's event
        // would then hide it -- which is how a panic on multi-byte text next
        // to a quote mark first went unnoticed here.
        let mut all = stored_prompts(&env);
        assert_eq!(all.len(), round + 1, "round {round} stored no event");
        let stored = all.pop().expect("stored");
        assert!(stored.len() <= limits::PROMPT, "{}", stored.len());
        let got = velra_core::prompt::authored(&stored);
        // Never more than the user wrote: the stored authored text begins as
        // the user's text does, and every run between Velra's omission
        // markers is the user's text, verbatim.
        let runs = omission_runs(got);
        assert!(
            expected.starts_with(runs[0]),
            "stored authored text does not begin as the user's: {got:?} vs {expected:?} \
             (stored {stored:?}, prompt {:?})",
            velra_core::text::prefix_bytes(&prompt, 600)
        );
        for run in &runs {
            assert!(
                expected.contains(run),
                "stored authored text is not the user's: {run:?} vs {expected:?}"
            );
        }
        // Deterministic, and the hook stores exactly what the pure function
        // decides: the same input gives the same bytes on every call.
        let once = velra_core::prompt::for_storage(&prompt, limits::PROMPT);
        let twice = velra_core::prompt::for_storage(&prompt, limits::PROMPT);
        assert_eq!(once, twice);
        assert_eq!(once.text, stored);
        // Every block kept is a complete element of a known family.
        for block in velra_core::prompt::split(&stored).injected {
            assert!(velra_core::prompt::origin(block.tag).is_some());
            assert!(block.text.ends_with(&format!("</{}>", block.tag)));
        }
    }
}
