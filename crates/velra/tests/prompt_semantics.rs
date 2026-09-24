//! What a prompt means, through the real hook path (Phase 1B hardening).
//!
//! `tests/prompt_classification.rs` covers *which part* of a prompt the user
//! wrote. This file covers what is taken from it as durable state, and when
//! it must not be:
//!
//! 1. **the storage bound** -- the event log keeps at most `limits::PROMPT`
//!    bytes of a prompt. A constraint, a rejection, a request, a file or a
//!    failing test named past that point must still be carried: semantic
//!    extraction reads the whole prompt, and only the prose is bounded;
//! 2. **pasted material** -- test output, logs, compiler diagnostics, stack
//!    traces, JSON, code comments and quoted documentation are someone else's
//!    words. A cue word inside them (`must`, `do not`, `should`) is not the
//!    user's rule;
//! 3. **rejections** -- a rejected approach is recorded only when the user
//!    asserts it and, when the statement points back (`That workaround ...`),
//!    exactly one earlier sentence names what it points to. Anything less
//!    stays ordinary text rather than becoming invented state;
//! 4. **slash text and quote marks** -- `/etc is missing` is a sentence, not a
//!    command, and an inch mark does not hide the rest of a paragraph.
//!
//! Every assertion is about the ledger: which rows exist and what they hold.
//! None of it says anything about what a model does with them.

mod common;

use common::Env;
use serde_json::{json, Value};
use velra_core::event::limits;

fn submit(env: &Env, n: usize, prompt: &str) {
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!(prompt);
    p["prompt_id"] = json!(format!("prompt-{n}"));
    env.hook("user-prompt-submit", &p).assert_contract();
}

/// `(level, text, live)` of every intent row, in id order, once
/// `expected_events` events are visible and reduced.
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

/// `(text, cue)` of every constraint row, in id order.
fn constraints(env: &Env) -> Vec<(String, String)> {
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare("SELECT text, cue FROM constraints WHERE session_id = ?1 ORDER BY id")
        .expect("prepare");
    stmt.query_map([&env.session], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn constraint_texts(env: &Env) -> Vec<String> {
    constraints(env).into_iter().map(|(t, _)| t).collect()
}

/// Constraint rows whose cue is a rejection label.
fn rejections(env: &Env) -> Vec<String> {
    constraints(env)
        .into_iter()
        .filter(|(_, cue)| velra_core::constraint::is_rejection_cue(cue))
        .map(|(t, _)| t)
        .collect()
}

/// Every stored UserPromptSubmit payload, parsed.
fn payloads(env: &Env) -> Vec<Value> {
    env.drain_and_load_events(1)
        .into_iter()
        .filter(|e| e.hook_event == "UserPromptSubmit")
        .map(|e| e.json())
        .collect()
}

/// Everything a stored payload holds as text: the bounded prompt and every
/// string in its extraction record.
fn payload_text(p: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    walk(p, &mut out);
    out
}

/// Prose the user plausibly wrote, with no cue words and no identifiers,
/// paragraph-broken, of at least `bytes` bytes.
fn filler_prose(bytes: usize) -> String {
    let sentences = [
        "The retry path was written quickly during an incident and it shows.",
        "Several callers reach it through the gateway wrapper rather than directly.",
        "Reviewers asked for a careful write-up before anything lands.",
        "The history of the module is long and mostly unremarkable.",
        "Most of the churn came from logging changes over the last quarter.",
    ];
    let mut out = String::new();
    let mut i = 0;
    while out.len() < bytes {
        out.push_str(sentences[i % sentences.len()]);
        out.push(' ');
        i += 1;
        if i % 6 == 0 {
            out.push_str("\n\n");
        }
    }
    out.trim_end().to_string()
}

/// A pasted pytest failure report of at least `bytes` bytes, carrying cue
/// words in its assertion messages.
fn pytest_output(bytes: usize) -> String {
    let mut out = String::from(
        "============================= test session starts =============================\n\
         platform linux -- Python 3.12.1, pytest-8.1.1, pluggy-1.4.0\n\
         collected 48 items\n\n\
         tests/test_retry.py ..F.....                                             [ 16%]\n\n\
         =================================== FAILURES ===================================\n\
         _________________________ test_retry_keeps_the_key ____________________________\n\n",
    );
    let mut i = 0;
    while out.len() < bytes {
        out.push_str(&format!(
            "    def test_case_{i:03}():\n\
             >       assert charge(amount=-1) == {i}\n\
             E       AssertionError: amount must be positive and must not be retried\n\
             E       assert None == {i}\n\n\
             tests/test_retry.py:{}: AssertionError\n",
            40 + i
        ));
        i += 1;
    }
    out.push_str(
        "=========================== short test summary info ============================\n\
         FAILED tests/test_retry.py::test_retry_keeps_the_key - AssertionError: key must not change\n\
         ========================= 1 failed, 47 passed in 0.84s =========================",
    );
    out
}

// ------------------------------------------- 1. past the storage bound

/// A rule the user states after 4 KiB of their own prose is kept, verbatim.
#[test]
fn a_constraint_past_the_storage_bound_is_kept() {
    let env = Env::new();
    let head = format!(
        "Fix the payment retry in src/payments/retry.py.\n\n{}",
        filler_prose(limits::PROMPT + 600)
    );
    let rule = "Do not modify the tests under tests/payments at any point.";
    let prompt = format!("{head}\n\n{rule}");
    assert!(prompt.find(rule).unwrap() > limits::PROMPT);
    submit(&env, 1, &prompt);
    intents(&env, 1);
    assert!(
        constraint_texts(&env).iter().any(|t| t == rule),
        "{:?}",
        constraint_texts(&env)
    );
}

/// The rejected approach and the sentence it rejects, both past the bound.
#[test]
fn a_rejection_past_the_storage_bound_is_kept_with_what_it_rejects() {
    let env = Env::new();
    let prompt = format!(
        "Fix the payment retry in src/payments/retry.py.\n\n{}\n\nThen try a temporary \
         workaround that stores the idempotency key in a module-level variable. Run the \
         tests. That workaround is considered a rejected approach for this task.",
        filler_prose(limits::PROMPT + 200)
    );
    submit(&env, 1, &prompt);
    intents(&env, 1);
    let got = rejections(&env);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].starts_with("Then try a temporary workaround that stores the idempotency key"),
        "{got:?}"
    );
    assert!(got[0].ends_with("That workaround is considered a rejected approach for this task."));
}

/// `task:` and `subtask:` classify a long prompt exactly as they classify a
/// short one: the prefix decides, and rules past the bound still count.
#[test]
fn task_and_subtask_prefixes_with_long_bodies_keep_their_rules() {
    let env = Env::new();
    let body = filler_prose(limits::PROMPT + 300);
    submit(
        &env,
        1,
        &format!(
            "task: migrate the settings loader to TOML.\n\n{body}\n\nNever change the public \
             API of the loader."
        ),
    );
    submit(
        &env,
        2,
        &format!(
            "subtask: write the regression test for the loader.\n\n{body}\n\nKeep the old \
             JSON reader working for one release."
        ),
    );
    let rows = intents(&env, 2);
    let root = live(&rows, "ROOT").expect("root");
    assert!(
        root.starts_with("migrate the settings loader to TOML."),
        "{root}"
    );
    let sub = live(&rows, "SUBTASK").expect("subtask");
    assert!(
        sub.starts_with("write the regression test for the loader."),
        "{sub}"
    );
    let got = constraint_texts(&env);
    assert!(
        got.iter()
            .any(|t| t == "Never change the public API of the loader."),
        "{got:?}"
    );
    assert!(
        got.iter()
            .any(|t| t == "Keep the old JSON reader working for one release."),
        "{got:?}"
    );
}

/// `task:` past the bound is not a prefix, and was not one before the bound
/// existed either: classification of a long prompt is the classification
/// of the same text, never a consequence of where the cut fell.
#[test]
fn a_task_line_past_the_bound_classifies_as_it_would_in_a_short_prompt() {
    let env = Env::new();
    submit(
        &env,
        1,
        "Fix the flaky login test in tests/test_auth.py please.",
    );
    let long = format!(
        "Some background first.\n\n{}\n\ntask: rewrite the session cache",
        filler_prose(limits::PROMPT + 100)
    );
    submit(&env, 2, &long);
    let short = "Some background first.\n\ntask: rewrite the session cache";
    submit(&env, 3, short);
    let rows = intents(&env, 3);
    // Neither started an epoch: the root is still the first message.
    assert_eq!(
        live(&rows, "ROOT").as_deref(),
        Some("Fix the flaky login test in tests/test_auth.py please.")
    );
    let latest: Vec<&String> = rows
        .iter()
        .filter(|(l, _, _)| l == "LATEST")
        .map(|(_, t, _)| t)
        .collect();
    assert_eq!(latest.len(), 2, "{rows:?}");
    assert!(latest[0].starts_with("Some background first."));
    assert!(
        latest[0].contains("task: rewrite the session cache"),
        "{}",
        latest[0]
    );
}

/// The request at the end of a long message -- the shape of every "here is
/// a lot of output, now please do X" -- is kept in the latest message.
#[test]
fn the_request_after_a_long_paste_is_kept_as_the_latest_message() {
    let env = Env::new();
    submit(
        &env,
        1,
        "Fix the flaky retry test in tests/test_retry.py please.",
    );
    let request = "Please make test_retry_keeps_the_key pass by changing src/payments/retry.py.";
    let prompt = format!(
        "Here is the output:\n\n{}\n\n{request}",
        pytest_output(limits::PROMPT + 2_000)
    );
    submit(&env, 2, &prompt);
    let rows = intents(&env, 2);
    let latest = live(&rows, "LATEST").expect("latest");
    assert!(latest.contains(request), "request lost: {latest}");
    assert!(latest.starts_with("Here is the output:"), "{latest}");
}

/// A file and a failing test named only past the bound -- one in the user's
/// prose, one inside the pasted report -- are still in the stored event.
#[test]
fn identifiers_named_past_the_bound_are_kept_in_the_event() {
    let env = Env::new();
    let prompt = format!(
        "Fix the payment retry.\n\n{}\n\n{}\n\nThe root cause is probably in \
         src/payments/ledger_sync.py.",
        filler_prose(limits::PROMPT),
        pytest_output(3_000)
    );
    submit(&env, 1, &prompt);
    intents(&env, 1);
    let p = &payloads(&env)[0];
    let text = payload_text(p);
    for id in [
        "src/payments/ledger_sync.py",
        "tests/test_retry.py::test_retry_keeps_the_key",
    ] {
        assert!(text.contains(id), "{id} not carried: {p}");
    }
    // What was left out of the stored prose is recorded, not silent.
    assert_eq!(p["prompt_truncated"], json!(true), "{p}");
    let root = live(&intents(&env, 1), "ROOT").expect("root");
    assert!(root.contains("src/payments/ledger_sync.py"), "{root}");
}

/// The stored prompt stays within the budget however long the input, and
/// the hook stores exactly what the pure storage function returns.
#[test]
fn the_hook_stores_exactly_what_for_storage_returns() {
    let env = Env::new();
    let prompts = [
        format!(
            "{}\n\nDo not modify the tests.",
            filler_prose(limits::PROMPT * 3)
        ),
        format!(
            "Here is the output:\n\n{}\n\nFix it.",
            pytest_output(limits::PROMPT * 2)
        ),
        format!(
            "<ide_selection>{}</ide_selection>\ntask: {}",
            "x\n".repeat(3_000),
            filler_prose(limits::PROMPT + 10)
        ),
        "short prompt about src/a.py that fits".to_string(),
    ];
    for (n, prompt) in prompts.iter().enumerate() {
        submit(&env, n, prompt);
    }
    let stored = payloads(&env);
    assert_eq!(stored.len(), prompts.len());
    for (prompt, p) in prompts.iter().zip(&stored) {
        let want = velra_core::prompt::for_storage(prompt, limits::PROMPT);
        assert_eq!(p["prompt"].as_str(), Some(want.text.as_str()));
        assert!(want.text.len() <= limits::PROMPT, "{}", want.text.len());
        // The extraction record too, field for field.
        let facts = serde_json::to_value(velra_core::prompt::facts_record(&want)).unwrap();
        assert_eq!(p["prompt_facts"], facts, "{p}");
    }
}

// ------------------------------------------------ 1b. the hook's deadline

/// A deadline that fires while the prompt is being processed leaves a record
/// that a prompt of that size arrived -- never the prompt's unredacted text,
/// and never nothing.
///
/// Needs the `fault-injection` build (`VELRA_TEST_STALL_PROMPT_MS`,
/// `VELRA_TEST_WATCHDOG_MS`); without it the knobs are ignored and the test
/// checks the ordinary path instead.
#[test]
fn a_deadline_during_processing_records_that_a_prompt_arrived() {
    let env = Env::new();
    submit(
        &env,
        0,
        "Fix the flaky login test in tests/test_auth.py please.",
    );
    let prompt = "Do not modify the tests. token=abcdefgh12345678 is the key.";
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!(prompt);
    p["prompt_id"] = json!("prompt-late");
    env.hook_with_env(
        "user-prompt-submit",
        &p,
        &[
            ("VELRA_TEST_WATCHDOG_MS", "400"),
            ("VELRA_TEST_STALL_PROMPT_MS", "5000"),
        ],
    )
    .assert_contract();
    let rows = intents(&env, 2);
    let stored = payloads(&env);
    assert_eq!(stored.len(), 2, "{stored:?}");
    let last = &stored[1];
    if cfg!(feature = "fault-injection") {
        assert_eq!(last["prompt_facts"]["unprocessed"], json!(true), "{last}");
        assert_eq!(
            last["prompt_facts"]["authored_bytes"],
            json!(prompt.len()),
            "{last}"
        );
        assert!(last.get("prompt").is_none(), "{last}");
        assert!(!last.to_string().contains("abcdefgh12345678"));
        // It changes no intent and states no rule.
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(constraint_texts(&env).is_empty());
        // It was a turn: a rule stated next is the user's third message.
        submit(
            &env,
            2,
            "Do not modify the fixtures in tests/data at any point.",
        );
        intents(&env, 3);
        let db = env.open_db();
        let ordinal: i64 = db
            .conn
            .query_row("SELECT prompt_ordinal FROM constraints", [], |r| r.get(0))
            .expect("one constraint");
        assert_eq!(ordinal, 2);
    }
}

/// Input larger than the hook parses a prompt from is recorded, unparsed, as
/// a prompt of that size in the right session and workspace.
#[test]
fn a_prompt_too_large_to_parse_is_recorded_not_lost() {
    let env = Env::new();
    let big = "Do not modify the tests.\n\n".repeat((17 << 20) / 26);
    // Keys in the order Claude Code sends them
    // (`tests/fixtures/claude-code/2.1.268/user_prompt_submit.json`): the
    // prompt last, so everything else is inside the part that is read.
    let stdin = format!(
        "{{\"session_id\":{},\"prompt_id\":\"prompt-huge\",\"cwd\":{},\
         \"hook_event_name\":\"UserPromptSubmit\",\"prompt\":{}}}",
        json!(env.session),
        json!(env.project.to_string_lossy()),
        json!(big)
    );
    assert!(stdin.len() > 16 << 20);
    env.hook_raw("user-prompt-submit", stdin.as_bytes())
        .assert_contract();
    env.drain_and_load_events(1);
    env.drain();
    let db = env.open_db();
    let (hook_event, session, project, payload): (String, String, String, String) = db
        .conn
        .query_row(
            "SELECT hook_event, session_id, project_id, payload FROM events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("one event");
    assert_eq!(hook_event, "UserPromptSubmit");
    assert_eq!(session, env.session);
    assert_eq!(project, env.project_id(), "workspace salvaged from cwd");
    let v: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["prompt_facts"]["unprocessed"], json!(true), "{v}");
    assert_eq!(v["prompt_facts"]["authored_bytes"], json!(stdin.len()));
    assert_eq!(v["prompt_id"], json!("prompt-huge"));
    assert!(v.get("prompt").is_none());
    let rows = intents(&env, 1);
    assert!(rows.is_empty(), "{rows:?}");
    assert!(constraint_texts(&env).is_empty());

    // A field past the part that is read is simply not known; the event is
    // still recorded.
    let env = Env::new();
    let stdin = format!(
        "{{\"session_id\":{},\"cwd\":{},\"prompt\":{},\"prompt_id\":\"after\"}}",
        json!(env.session),
        json!(env.project.to_string_lossy()),
        json!(big)
    );
    env.hook_raw("user-prompt-submit", stdin.as_bytes())
        .assert_contract();
    let p = &payloads(&env)[0];
    assert_eq!(p["prompt_facts"]["unprocessed"], json!(true), "{p}");
    assert!(p.get("prompt_id").is_none(), "{p}");
}

/// More injected blocks at an edge than the hook classifies: the rest is
/// not the user's text, and the record says what was left out.
#[test]
fn a_prompt_of_thousands_of_blocks_is_bounded_and_recorded() {
    let env = Env::new();
    let blocks = "<system-reminder>Never delete the audit log.</system-reminder>".repeat(1_100);
    submit(
        &env,
        1,
        &format!("{blocks}\nFix the retry in src/payments/retry.py."),
    );
    let rows = intents(&env, 1);
    assert!(rows.is_empty(), "unclassified text became intent: {rows:?}");
    assert!(constraint_texts(&env).is_empty());
    let p = &payloads(&env)[0];
    let facts = &p["prompt_facts"];
    assert_eq!(
        facts["omitted_blocks"],
        json!(velra_core::prompt::MAX_EDGE_BLOCKS),
        "{facts}"
    );
    assert!(facts["unparsed_bytes"].as_u64().unwrap_or(0) > 0, "{facts}");
    assert_eq!(
        p["prompt_omitted"].as_array().map(Vec::len),
        Some(velra_core::prompt::OMITTED_LISTED)
    );
    // Nothing unclassified is stored, redacted or not.
    assert!(
        !p["prompt"].as_str().unwrap_or("").contains("Fix the retry"),
        "{p}"
    );
}

/// A handful of small blocks that fit are kept, the task is the objective.
#[test]
fn many_small_blocks_that_fit_are_kept() {
    let env = Env::new();
    let blocks = "<system-reminder>be brief</system-reminder>\n".repeat(40);
    let task = "Fix the retry in src/payments/retry.py so the key survives.";
    submit(&env, 1, &format!("{blocks}{task}"));
    let rows = intents(&env, 1);
    assert_eq!(live(&rows, "ROOT").as_deref(), Some(task));
    let p = &payloads(&env)[0];
    assert!(p.get("prompt_omitted").is_none(), "{p}");
    // Nothing of the user's text was left out, and the record says so.
    assert!(p.get("prompt_truncated").is_none(), "{p}");
    assert!(p["prompt_facts"].get("elided_bytes").is_none(), "{p}");
    assert_eq!(
        p["prompt_facts"]["authored_bytes"],
        json!(task.len()),
        "{p}"
    );
    assert_eq!(
        velra_core::prompt::split(p["prompt"].as_str().unwrap())
            .injected
            .len(),
        40
    );
}

// --------------------------------------------------- 2. pasted material

/// Unfenced pasted output of every common kind, introduced the way people
/// introduce it. Not one of its cue words is the user's rule.
#[test]
fn pasted_output_does_not_become_a_constraint() {
    let cases: Vec<String> = vec![
        format!("Here is the output:\n{}\nWhat is going on?", pytest_output(900)),
        format!("test output:\n\n{}", pytest_output(700)),
        "command output:\n$ ./deploy.sh --dry-run\nerror: you must not deploy from a dirty tree\n\
         hint: do not use --force here\nexit status 1\n\nWhy did that happen?"
            .to_string(),
        "logs:\n2026-09-12T10:04:05Z WARN retry: the client should not retry after a 409\n\
         2026-09-12T10:04:06Z ERROR gateway: idempotency key must be unique per charge\n\n\
         Can you find the cause?"
            .to_string(),
        "compiler output:\nerror[E0382]: borrow of moved value: `key`\n  --> src/retry.rs:41:9\n   \
         |\n41 |     send(key);\n   |          --- value moved here\n   = note: move occurs \
         because `key` must be cloned before the loop\n\nFix the build."
            .to_string(),
        "Traceback (most recent call last):\n  File \"src/retry.py\", line 12, in charge\n    \
         raise ValueError(\"amount must not be negative\")\nValueError: amount must not be \
         negative\n\nWhy does charge() get a negative amount?"
            .to_string(),
        "The API answered with:\n{\"error\": \"the key must be unique\", \"hint\": \"never reuse \
         a key\"}\n\nIs retry.py reusing it?"
            .to_string(),
        "Response body:\n{\n  \"error\": \"amount must be positive\",\n  \"detail\": \"do not \
         send zero\"\n}\n\nwhere does zero come from?"
            .to_string(),
        "Here's the error I get\n    at Object.charge (src/retry.js:12:9)\n    at \
         processTicksAndRejections (node:internal/process/task_queues:95:5)\nError: key must \
         not be null\n\nany idea?"
            .to_string(),
        "src/retry.c:41:9: warning: value must be checked before use [-Wunused-result]\n\
         src/retry.c:58:1: error: control reaches end of non-void function\n\nThese came from \
         make."
            .to_string(),
    ];
    for (n, prompt) in cases.iter().enumerate() {
        let env = Env::new();
        submit(&env, n, prompt);
        intents(&env, 1);
        assert!(
            constraint_texts(&env).is_empty(),
            "case {n} made a constraint of pasted output: {:?}\n{prompt}",
            constraint_texts(&env)
        );
    }
}

/// Code comments and quoted documentation, unfenced.
#[test]
fn pasted_code_and_quoted_documentation_do_not_become_constraints() {
    let cases = [
        "This is the current check in src/validate.py:\n    # the value must not be None\n    \
         if value is None:\n        raise ValueError(\"must never be empty\")\nWhy does it \
         reject zero?",
        "// must not be called from the UI thread\nfn charge(key: &Key) -> Result<()> {\n    \
         // callers should always pass a fresh key\n}\nIs that comment still true?",
        "The runbook says:\n\n1. You must drain the node before a restart.\n2. Never restart \
         during business hours.\n\nCan you automate the first step?",
        "From the Stripe docs:\n\n## Idempotent requests\n\nYou must provide an idempotency \
         key for every POST request.\n\nKeys should be unique per operation.",
        "> Do not retry a request that returned 409.\n> Always log the idempotency key.\n\nIs \
         that what src/payments/retry.py does?",
        "The README says: never call the payments API directly from the web tier. Is that \
         still true?",
    ];
    for (n, prompt) in cases.iter().enumerate() {
        let env = Env::new();
        submit(&env, n, prompt);
        intents(&env, 1);
        assert!(
            constraint_texts(&env).is_empty(),
            "case {n}: {:?}\n{prompt}",
            constraint_texts(&env)
        );
    }
}

/// The user's own rule after pasted output is still the user's rule: the
/// output region ends where the text stops looking like output.
#[test]
fn the_users_rule_after_pasted_output_is_kept() {
    let env = Env::new();
    let prompt = format!(
        "Here is the output:\n\n{}\n\nPlease fix it. Do not modify the tests.",
        pytest_output(600)
    );
    submit(&env, 1, &prompt);
    intents(&env, 1);
    assert_eq!(constraint_texts(&env), vec!["Do not modify the tests."]);
}

// ----------------------------------------------------- 3. rejections

/// Prose that mentions an approach, even with a rejection label, is not a
/// rejected approach unless the user asserts it and says which one.
#[test]
fn rejection_heuristics_do_not_invent_rejected_approaches() {
    let cases = [
        // Historical discussion.
        "Last year we tried a workaround with a global cache. That workaround was a dead end \
         back then, but the new cache layer may change that.",
        // Explanatory prose with no rejection at all.
        "The retry module uses a backoff approach. That approach is common in payment \
         clients and works well enough for us.",
        // Negated.
        "Try a workaround that stores the key in the request context. That workaround is not \
         a dead end, it just needs tests.",
        // A question.
        "Try a workaround that stores the key in a global. Is that workaround a dead end?",
        // Conditional.
        "Try a workaround that stores the key in a global. If that workaround breaks the \
         batch path, it is a dead end.",
        // Two candidate antecedents.
        "Try a workaround that stores the key in a global. Or try a workaround that stores it \
         in a thread-local. That workaround is a rejected approach.",
        // A bare pronoun names nothing.
        "Run the tests. That is a dead end.",
        // The noun only appears inside quotes and code.
        "The docs call it \"the retry workaround\". See `workaround = True` in the config. \
         That workaround is a rejected approach.",
        // Too far back: four sentences separate them.
        "Try a workaround that stores the key in a global. Run the suite. Read the log. Check \
         the gateway. That workaround is a rejected approach.",
        // The noun only in pasted output.
        "test output:\nE   workaround applied: key cached in module global\nE   assert None \
         == 'k1'\n\nThat workaround is a rejected approach.",
        // this method / that behavior, no label.
        "This method retries too often. That behavior is what we need to change.",
        // A possibility, not a decision.
        "The global cache might be a dead end, we should measure it.",
    ];
    for (n, prompt) in cases.iter().enumerate() {
        let env = Env::new();
        submit(&env, n, prompt);
        intents(&env, 1);
        assert!(
            rejections(&env).is_empty(),
            "case {n} invented a rejection: {:?}\n{prompt}",
            rejections(&env)
        );
    }
}

/// The shapes where the text itself establishes the rejection.
#[test]
fn clearly_stated_rejections_are_kept_verbatim() {
    let cases: [(&str, &str); 4] = [
        (
            "Try a workaround that caches the key in a global. That workaround is a rejected \
             approach.",
            "Try a workaround that caches the key in a global. That workaround is a rejected \
             approach.",
        ),
        (
            "Caching the idempotency key in a module global is a dead end here.",
            "Caching the idempotency key in a module global is a dead end here.",
        ),
        (
            "Rejected approach: caching the idempotency key in a module-level variable.",
            "Rejected approach: caching the idempotency key in a module-level variable.",
        ),
        (
            "Then try a temporary workaround that stores the key in a module-level variable. \
             Run the tests. That workaround is considered a rejected approach for this task.",
            "Then try a temporary workaround that stores the key in a module-level variable. \
             Run the tests. That workaround is considered a rejected approach for this task.",
        ),
    ];
    for (n, (prompt, want)) in cases.iter().enumerate() {
        let env = Env::new();
        submit(&env, n, prompt);
        intents(&env, 1);
        assert_eq!(rejections(&env), vec![want.to_string()], "case {n}");
    }
}

// ------------------------------------------- 4. slash text, quote marks

#[test]
fn root_level_paths_and_routes_are_sentences_not_commands() {
    for (n, prompt) in [
        "/etc is missing from the container image, find out why",
        "/tmp is full on the CI runner and the build fails",
        "/health returns 503 after the deploy, investigate",
        "/api has no rate limit, add one in src/server/limits.rs",
        "/usr/local/bin/velra is missing after the install",
        "/Users/dev/My Project/src/app.ts fails to compile after the rename",
        "/c/Program Files/Velra/velra.exe is not on PATH",
        "/v1.2 of the API is still served, retire it",
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
fn genuine_slash_commands_are_still_commands() {
    let env = Env::new();
    submit(
        &env,
        0,
        "Fix the flaky login test in tests/test_auth.py please.",
    );
    let cmds = [
        "/compact",
        "/clear",
        "/review please look at the auth module",
        "/my-plugin:do-thing with args",
        "/fix_issue 123",
        "/deploy staging",
        "/etc",
    ];
    for (n, cmd) in cmds.iter().enumerate() {
        submit(&env, n + 1, cmd);
    }
    let rows = intents(&env, cmds.len() + 1);
    assert_eq!(rows.len(), 1, "a slash command changed an intent: {rows:?}");
}

/// Inch marks, apostrophes and a stray quote do not hide the user's rule.
#[test]
fn punctuation_quotes_do_not_hide_the_users_rules() {
    for (n, (prompt, want)) in [
        (
            "The mount takes a 5\" pipe. Do not change the flange spec in src/spec.rs.",
            "Do not change the flange spec in src/spec.rs.",
        ),
        (
            "Resize the 12\" x 8\" preview. Never upscale images past their source size.",
            "Never upscale images past their source size.",
        ),
        (
            "The user's session isn't refreshed. Don't change the token format in auth.rs.",
            "Don't change the token format in auth.rs.",
        ),
        (
            "He wrote \"retry later and left. Do not retry on a 409 in src/retry.py.",
            "Do not retry on a 409 in src/retry.py.",
        ),
        (
            "Escape it as \\\" in the JSON. Never emit a raw quote in the log line.",
            "Never emit a raw quote in the log line.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let env = Env::new();
        submit(&env, n, prompt);
        intents(&env, 1);
        assert_eq!(
            constraint_texts(&env),
            vec![want.to_string()],
            "case {n}: {prompt}"
        );
    }
}
