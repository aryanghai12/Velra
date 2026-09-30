//! Phase 12: the whole automated lifecycle, through the real binary.
//!
//! ```text
//! source session --hooks--> ledger / spool --reduce--> snapshot --render-->
//!   velra restore --stage--> staged record --SessionStart(startup) claim-->
//!   the JSON a brand-new destination session is handed
//! ```
//!
//! Every other suite pins one of these layers. This one records two real
//! source sessions through `velra hook`, concurrently, with a spooled event, a
//! duplicated hook call, a secret and a transcript line the capsule must not
//! replay, then restores one of them with `velra restore` and reads the text
//! a fresh session receives. The assertions are on that text and on the
//! identity of every session involved -- not on database rows alone.
//!
//! No model is involved. What these tests prove is what Velra hands over,
//! not what a model does with it.

mod common;

use common::{Env, HookOutput};
use serde_json::{json, Value};
use velra_core::db::{Db, Role};
use velra_core::event::{NewEvent, ProjectInfo};
use velra_core::render::{self, RenderConfig, DEFAULT_BUDGET_TOKENS};
use velra_core::staging;
use velra_core::text::estimate_tokens;

/// Claude Code session ids are UUIDs; the capsule names a source by the first
/// eight characters, so the two sources differ there.
const A: &str = "a1b2c3d4-1111-4aaa-8aaa-aaaaaaaaaaaa";
const B: &str = "b9c8d7e6-2222-4bbb-8bbb-bbbbbbbbbbbb";

const ROUNDING: &str = "src/billing/rounding.py";
const CSV: &str = "src/export/csv_writer.py";

const ORIGINAL: &str = "from decimal import Decimal, ROUND_HALF_UP\n\nCENT = Decimal(\"0.01\")\n\n\
def round_total(amount):\n    return amount.quantize(CENT, rounding=ROUND_HALF_UP)\n";
const HALF_UP_LINE: &str = "    return amount.quantize(CENT, rounding=ROUND_HALF_UP)";
const HALF_DOWN_LINE: &str = "    return amount.quantize(CENT, rounding=ROUND_HALF_DOWN)";
const HALF_EVEN_LINE: &str = "    return amount.quantize(CENT, rounding=ROUND_HALF_EVEN)";

/// A GitHub token, planted in a prompt and in test output.
const CANARY: &str = "ghp_CanaryLifecycle0001abcdefghijklmnopqrstu";
/// Assistant prose that exists only in the source's transcript.
const TRANSCRIPT_ONLY: &str = "TranscriptOnlyReasoning0001";

const TASK_A: &str = "Invoice totals drift by a cent on half-cent amounts. Fix round_total in \
    src/billing/rounding.py so that the totals match the ledger. Do not modify the tests.";
const SECRET_A: &str = "The deploy token is ghp_CanaryLifecycle0001abcdefghijklmnopqrstu and \
    it must never be committed.";
const REJECT_A: &str = "Rounding half down is a rejected approach: the ledger rounds half to even.";
const NEXT_A: &str = "Next, make round_total handle negative amounts in src/billing/rounding.py.";
const TASK_B: &str = "Speed up the CSV export in src/export/csv_writer.py; large reports stream \
    too slowly.";

const TEST_HALF: &str = "tests/test_rounding.py::test_half_cent_rounds_to_even";
const TEST_NEG: &str = "tests/test_rounding.py::test_negative_half_cent";

fn pytest_failure(test_id: &str, detail: &str, filler: usize) -> String {
    let mut out = String::new();
    for i in 0..filler {
        out.push_str(&format!(
            "tests/test_rounding.py::test_case_{i:03} PASSED\n"
        ));
    }
    out.push_str(&format!(
        "{test_id} FAILED\n\n=== FAILURES ===\n>       {detail}\nE       AssertionError\n\n\
         env: DEPLOY_TOKEN={CANARY}\ntests/test_rounding.py:9: AssertionError\n\
         === short test summary info ===\nFAILED {test_id} - AssertionError\n1 failed, 3 passed in 0.04s"
    ));
    out
}

// ------------------------------------------------------------------ hooks

fn transcript(env: &Env, session: &str) -> std::path::PathBuf {
    env.dir.path().join(format!("{session}.jsonl"))
}

/// One hook invocation for `session`, held to the hook contract.
fn hook(env: &Env, session: &str, event: &str, f: impl FnOnce(&mut Value)) -> HookOutput {
    let sub = match event {
        "SessionStart" => "session-start",
        "UserPromptSubmit" => "user-prompt-submit",
        "PreToolUse" => "pre-tool-use",
        "PostToolUse" => "post-tool-use",
        "PostToolUseFailure" => "post-tool-use-failure",
        "Stop" => "stop",
        other => panic!("no hook for {other}"),
    };
    let mut p = env.base_payload(event);
    p["session_id"] = json!(session);
    p["transcript_path"] = json!(transcript(env, session));
    f(&mut p);
    let out = env.hook(sub, &p);
    out.assert_contract();
    out
}

fn start(env: &Env, session: &str) -> HookOutput {
    hook(env, session, "SessionStart", |p| {
        p["source"] = json!("startup")
    })
}

fn prompt(env: &Env, session: &str, id: &str, text: &str) {
    hook(env, session, "UserPromptSubmit", |p| {
        p["prompt"] = json!(text);
        p["prompt_id"] = json!(id);
    });
}

fn read(env: &Env, session: &str, id: &str, rel: &str) {
    hook(env, session, "PostToolUse", |p| {
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = json!({ "file_path": env.project.join(rel) });
    });
}

/// An `Edit` of one line: the pre-hook, the change on disk, the post-hook.
fn edit(env: &Env, session: &str, id: &str, rel: &str, old: &str, new: &str) {
    let file = env.project.join(rel);
    let input = json!({ "file_path": file, "old_string": old, "new_string": new });
    hook(env, session, "PreToolUse", |p| {
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = input.clone();
    });
    let before = std::fs::read_to_string(&file).expect("read file");
    assert!(before.contains(old), "{rel} does not contain {old:?}");
    env.write_file(rel, &before.replacen(old, new, 1));
    hook(env, session, "PostToolUse", |p| {
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = input;
        p["tool_response"] = json!({ "filePath": file, "originalFile": before });
    });
}

fn failed_command(env: &Env, session: &str, id: &str, command: &str, output: &str) {
    hook(env, session, "PostToolUseFailure", |p| {
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = json!({ "command": command });
        p["error"] = json!(format!("Exit code 1\n{output}"));
    });
}

fn passed_command(env: &Env, session: &str, id: &str, command: &str, stdout: &str) {
    hook(env, session, "PostToolUse", |p| {
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = json!({ "command": command });
        p["tool_response"] =
            json!({ "stdout": stdout, "stderr": "", "interrupted": false, "exitCode": 0 });
    });
}

/// `git restore <rel>` as the hooks see it: the pre-hook hashes the session's
/// files, git puts `content` back, the post-hook hashes them again.
fn git_restore(env: &Env, session: &str, id: &str, rel: &str, content: &str) {
    let command = format!("git restore {rel}");
    hook(env, session, "PreToolUse", |p| {
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = json!({ "command": command });
    });
    env.write_file(rel, content);
    passed_command(env, session, id, &command, "");
}

fn stop(env: &Env, session: &str) {
    hook(env, session, "Stop", |p| {
        p["stop_hook_active"] = json!(false)
    });
}

// ---------------------------------------------------------------- fixture

/// Session A: an objective with a rule and a secret, a failing test, a
/// rejected attempt reverted with `git restore`, the user's rejection of it,
/// a second edit, a new failure, and the next step. One of its events is
/// forced through the spool, and one hook call is delivered twice.
fn record_a(env: &Env) {
    std::fs::write(
        transcript(env, A),
        format!(
            "{{\"type\":\"ai-title\",\"aiTitle\":\"Fix invoice half-cent rounding\"}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":\
             [{{\"type\":\"text\",\"text\":\"{TRANSCRIPT_ONLY}: the quantize call is the culprit.\"}}]}}}}\n"
        ),
    )
    .unwrap();
    start(env, A);
    prompt(env, A, "a-p1", &format!("{TASK_A} {SECRET_A}"));
    read(env, A, "a-t1", ROUNDING);
    let first = pytest_failure(
        TEST_HALF,
        "assert round_total(Decimal(\"0.125\")) == Decimal(\"0.12\")",
        120,
    );
    failed_command(
        env,
        A,
        "a-t2",
        "python -m pytest tests/test_rounding.py",
        &first,
    );
    // The same hook call run twice (Velra registered under two binary paths,
    // D110): one event, one command (D148).
    failed_command(
        env,
        A,
        "a-t2",
        "python -m pytest tests/test_rounding.py",
        &first,
    );
    edit(env, A, "a-t3", ROUNDING, HALF_UP_LINE, HALF_DOWN_LINE);
    failed_command(
        env,
        A,
        "a-t4",
        "python -m pytest tests/test_rounding.py",
        &pytest_failure(
            TEST_HALF,
            "assert round_total(Decimal(\"0.135\")) == Decimal(\"0.14\")",
            0,
        ),
    );
    prompt(env, A, "a-p2", REJECT_A);
    git_restore(env, A, "a-t5", ROUNDING, ORIGINAL);
    // The second edit reaches the ledger through the spool: its post-hook
    // meets a held write lock, gives up after its budget and spools. It is
    // ingested after the events that follow it, stamped with when it happened.
    {
        let blocker = env.open_db();
        blocker.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        edit(env, A, "a-t6", ROUNDING, HALF_UP_LINE, HALF_EVEN_LINE);
        assert!(
            velra_core::spool::backlog(&env.spool_dir()) > 0,
            "the edit must have been spooled, or this fixture tests nothing"
        );
        blocker.conn.execute_batch("ROLLBACK").unwrap();
    }
    failed_command(
        env,
        A,
        "a-t7",
        "python -m pytest tests/test_rounding.py",
        &pytest_failure(
            TEST_NEG,
            "assert round_total(Decimal(\"-0.125\")) == Decimal(\"-0.12\")",
            0,
        ),
    );
    prompt(env, A, "a-p3", NEXT_A);
    stop(env, A);
}

/// Session B, in the same workspace: a different task, passing tests.
fn record_b(env: &Env) {
    start(env, B);
    prompt(env, B, "b-p1", TASK_B);
    read(env, B, "b-t1", CSV);
    edit(env, B, "b-t2", CSV, "rows = list(reader)", "rows = reader");
    passed_command(
        env,
        B,
        "b-t3",
        "python -m pytest tests/test_export.py",
        "3 passed in 0.02s",
    );
    stop(env, B);
}

/// Both sessions, recorded at the same time by concurrent hook processes, and
/// drained.
fn record_two_sessions() -> Env {
    let env = Env::new();
    env.write_file(ROUNDING, ORIGINAL);
    env.write_file(
        "tests/test_rounding.py",
        "def test_half_cent_rounds_to_even(): ...\n",
    );
    env.write_file(CSV, "def write(reader):\n    rows = list(reader)\n");
    drop(env.open_db());
    std::thread::scope(|s| {
        let b = s.spawn(|| record_b(&env));
        record_a(&env);
        b.join().expect("session B");
    });
    env.drain();
    assert_eq!(
        velra_core::spool::backlog(&env.spool_dir()),
        0,
        "the spool drained"
    );
    env
}

// ------------------------------------------------------------ inspection

fn restore(env: &Env, args: &[&str]) -> std::process::Output {
    env.cmd()
        .arg("restore")
        .args(args)
        .output()
        .expect("run velra restore")
}

fn restore_json(env: &Env, args: &[&str]) -> Value {
    let out = restore(env, &[args, &["--json"]].concat());
    assert!(out.status.success(), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    serde_json::from_slice(&out.stdout).expect("restore --json")
}

/// The delivery a `SessionStart(startup)` of `session` produced, if any.
fn delivered(out: &HookOutput) -> Option<(String, String)> {
    if out.stdout.is_empty() {
        return None;
    }
    let v = out.json().expect("one JSON object");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    Some((
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additionalContext")
            .to_string(),
        v["systemMessage"]
            .as_str()
            .expect("systemMessage")
            .to_string(),
    ))
}

/// The body lines of a capsule section, until the next header.
fn section<'a>(capsule: &'a str, name: &str) -> Vec<&'a str> {
    capsule
        .lines()
        .skip_while(|l| !l.starts_with(&format!("[{name}]")))
        .skip(1)
        .take_while(|l| !l.starts_with('[') && !l.starts_with("</"))
        .collect()
}

fn count(env: &Env, sql: &str, session: &str) -> i64 {
    env.open_db()
        .conn
        .query_row(sql, [session], |r| r.get(0))
        .expect("count")
}

fn staged_dir(env: &Env) -> std::path::PathBuf {
    staging::staged_dir(&env.home, &env.project_id())
}

/// What `velra restore` stages for session A, checked section by section: the
/// state a fresh session needs to continue, and nothing that is not A's.
fn assert_carries_session_a(capsule: &str) {
    assert!(
        estimate_tokens(capsule) <= DEFAULT_BUDGET_TOKENS,
        "{} tokens:\n{capsule}",
        estimate_tokens(capsule)
    );
    assert!(capsule.chars().count() <= 9_500, "{capsule}");
    assert!(capsule.starts_with("<VELRA_WORKSPACE_STATE v=\"1\" "));
    assert!(capsule.ends_with("</VELRA_WORKSPACE_STATE>"));

    // Provenance: another session's record, and which one.
    let preamble = capsule.lines().nth(2).unwrap();
    assert!(preamble.contains("another session's prompts"), "{capsule}");
    assert!(!capsule.contains("this session"), "{capsule}");
    assert!(
        capsule.contains("`velra inspect --session a1b2c3d4 --section <name>`"),
        "{capsule}"
    );

    // Objective, verbatim.
    let first = section(capsule, "FIRST_MESSAGE").join("\n");
    assert!(
        first.starts_with("Invoice totals drift by a cent on half-cent amounts."),
        "{capsule}"
    );
    // The rule, quoted.
    let rules = section(capsule, "STATED_CONSTRAINTS").join("\n");
    assert!(
        rules.contains("\"Do not modify the tests.\"")
            || first.contains("Do not modify the tests."),
        "{capsule}"
    );
    // The rejection, in the user's words.
    let rejected = section(capsule, "REJECTED_APPROACHES").join("\n");
    assert!(
        rejected.contains("Rounding half down is a rejected approach"),
        "{capsule}"
    );
    // The dead end: which file's attempt was reverted, and how. At this
    // budget the ceiling ladder gives up the attempted line before any of the
    // user's words (D71); the rejection above names the approach, and the
    // detail command prints the line (checked by the callers).
    let reverted = section(capsule, "REVERTED_EDITS").join("\n");
    assert!(
        reverted.contains(&format!(
            "{ROUNDING} | 1 edit(s) | reverted via `git restore {ROUNDING}`"
        )),
        "{capsule}"
    );
    // The active failure, by its exact identifier.
    let tests = section(capsule, "TEST_STATUS").join("\n");
    assert!(tests.contains(&format!("- FAIL {TEST_NEG}")), "{capsule}");
    // The next step, in the user's words.
    assert!(
        capsule.contains("Next, make round_total handle negative amounts"),
        "{capsule}"
    );
    assert!(capsule.contains(ROUNDING), "{capsule}");

    // Not B's, not the transcript, not the secret, not the raw output.
    for foreign in ["csv", "CSV", "export", "b9c8d7e6"] {
        assert!(
            !capsule.contains(foreign),
            "B leaked ({foreign}):\n{capsule}"
        );
    }
    assert!(!capsule.contains(TRANSCRIPT_ONLY), "{capsule}");
    assert!(!capsule.contains(CANARY), "{capsule}");
    assert!(!capsule.contains("CanaryLifecycle"), "{capsule}");
    assert!(
        capsule.matches("test_case_").count() <= 2,
        "the runner's output is replayed:\n{capsule}"
    );
}

// ------------------------------------------------------------------ tests

/// The lifecycle end to end: two concurrent source sessions, `velra restore`
/// of one, delivery to a brand-new session, once.
#[test]
fn a_restored_session_reaches_a_fresh_session_once_with_its_state_and_nothing_else() {
    let env = record_two_sessions();

    // The ledger holds both sessions, separately, deduplicated.
    for s in [A, B] {
        assert!(
            count(
                &env,
                "SELECT COUNT(*) FROM sessions WHERE session_id = ?1",
                s
            ) == 1,
            "{s} recorded"
        );
    }
    assert_eq!(
        count(
            &env,
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND tool_use_id = 'a-t2'",
            A
        ),
        1,
        "a hook call delivered twice is one event"
    );
    let spooled = count(
        &env,
        "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND tool_use_id = 'a-t6' \
         AND json_extract(payload, '$.spooled') = 1",
        A,
    );
    assert_eq!(spooled, 2, "the spooled edit (pre and post) was ingested");

    // Without an explicit restore, a new session receives nothing.
    let before = start(&env, "c0000000-3333-4ccc-8ccc-cccccccccccc");
    assert!(before.stdout.is_empty(), "{}", before.stdout);

    // Both sources are offered, with state.
    let listed = restore_json(&env, &["--list"]);
    let offered: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["session_id"].as_str())
        .collect();
    assert!(offered.contains(&A) && offered.contains(&B), "{listed}");

    // A dry run prints what would be staged and stages nothing.
    let dry = restore(&env, &["--session", A, "--dry-run"]);
    assert!(dry.status.success(), "{dry:?}");
    let dry = String::from_utf8(dry.stdout).unwrap();
    assert!(staging::records(&staged_dir(&env)).is_empty());

    // Stage A.
    let staged_json = restore_json(&env, &["--session", A]);
    assert_eq!(staged_json["source_session_id"], A);
    assert_eq!(staged_json["workspace_id"], env.project_id());
    let staged = staging::peek(&staged_dir(&env)).expect("staged record");
    assert_eq!(
        staged.source_session_id, A,
        "the exact source id is recorded"
    );
    assert_eq!(staged.workspace_id, env.project_id());
    assert!(staged.hash_matches());
    assert!(staged.tokens <= DEFAULT_BUDGET_TOKENS);
    assert_carries_session_a(&staged.capsule);

    // The dry run and the staged record differ only in when they were
    // captured.
    let tail = |t: &str| {
        t.trim_end()
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(tail(&dry), tail(&staged.capsule));

    // The snapshot/render path, run here on the same ledger and clock, is the
    // staged capsule byte for byte: restore adds no state and loses none.
    let db = env.open_db();
    let checkpoint = velra_core::restore::source_checkpoint(&db.conn, A).unwrap();
    let meta = velra_core::restore::snapshot_meta(checkpoint.as_deref(), staged.created_ms, 0);
    let snap = velra_core::snapshot::build(&db.conn, A, &meta).unwrap();
    assert!(snap.restore);
    assert_eq!(snap.session_id, A);
    assert_eq!(snap.project_id, env.project_id());
    let rendered = render::render(&snap, &RenderConfig::default()).text;
    assert_eq!(velra_core::redact::redact(&rendered), staged.capsule);
    drop(db);

    let events_of_a = count(&env, "SELECT COUNT(*) FROM events WHERE session_id = ?1", A);

    // A brand-new session is handed exactly the staged capsule.
    let dest = "d0000000-4444-4ddd-8ddd-dddddddddddd";
    let out = start(&env, dest);
    let (capsule, message) = delivered(&out).expect("the staged capsule is delivered");
    assert_eq!(capsule, staged.capsule, "delivered byte for byte");
    assert_carries_session_a(&capsule);
    assert!(message.contains("from session a1b2c3d4"), "{message}");
    assert!(!out.stdout.contains(CANARY));
    assert!(
        staging::records(&staged_dir(&env)).is_empty(),
        "the record is consumed"
    );

    // The destination keeps its own identity and inherits no ledger state.
    env.drain();
    assert!(
        count(
            &env,
            "SELECT COUNT(*) FROM events WHERE session_id = ?1",
            dest
        ) >= 1
    );
    for table in [
        "intents",
        "edits",
        "commands",
        "dead_ends",
        "constraints",
        "continuations",
    ] {
        assert_eq!(
            count(
                &env,
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                dest
            ),
            0,
            "{table} rows for the destination"
        );
    }
    let dest_project: String = env
        .open_db()
        .conn
        .query_row(
            "SELECT project_id FROM events WHERE session_id = ?1 LIMIT 1",
            [dest],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dest_project, env.project_id(), "same workspace");
    assert_eq!(
        count(&env, "SELECT COUNT(*) FROM events WHERE session_id = ?1", A),
        events_of_a,
        "delivery writes nothing under the source's id"
    );

    // Once: the same session starting again, and the next new session, get
    // nothing.
    assert!(start(&env, dest).stdout.is_empty());
    assert!(start(&env, "e0000000-5555-4eee-8eee-eeeeeeeeeeee")
        .stdout
        .is_empty());

    // The capsule's own detail command reaches the source, not the reader:
    // the failure, and the reverted attempt's line the budget left out.
    let detail = |section: &str| {
        let out = env
            .cmd()
            .args(["inspect", "--session", "a1b2c3d4", "--section", section])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let failure = detail("failure");
    assert!(failure.contains(TEST_NEG), "{failure}");
    assert!(!failure.contains(CANARY));
    let dead = detail("dead-ends");
    assert!(dead.contains("ROUND_HALF_DOWN"), "{dead}");
    assert!(dead.contains(&format!("git restore {ROUNDING}")), "{dead}");

    // Every byte left on disk: the secret was redacted before persistence.
    let mut files = Vec::new();
    walk(&env.home, &mut files);
    assert!(files.iter().any(|(p, _)| p.ends_with("velra.db")));
    for (path, bytes) in &files {
        assert!(
            !String::from_utf8_lossy(bytes).contains("CanaryLifecycle"),
            "{} holds the secret",
            path.display()
        );
    }
}

fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else if let Ok(bytes) = std::fs::read(&p) {
            out.push((p, bytes));
        }
    }
}

/// The cross-session matrix on one ledger: B restores as B; restaging
/// replaces rather than mixes; another workspace in the same Velra home can
/// neither restore these sessions nor receive their capsule; an unknown or
/// abbreviated source is refused and stages nothing.
#[test]
fn restores_never_cross_sessions_or_workspaces() {
    let env = record_two_sessions();

    // B restores as B, into its own fresh session.
    restore_json(&env, &["--session", B]);
    let (b, message) =
        delivered(&start(&env, "f0000000-6666-4fff-8fff-ffffffffffff")).expect("B delivered");
    assert!(b.contains("Speed up the CSV export"), "{b}");
    assert!(b.contains("`velra inspect --session b9c8d7e6 "), "{b}");
    assert!(message.contains("from session b9c8d7e6"), "{message}");
    for foreign in [
        "Invoice",
        "round_total",
        "ROUND_HALF",
        "a1b2c3d4",
        "rounding",
    ] {
        assert!(!b.contains(foreign), "A leaked into B ({foreign}):\n{b}");
    }

    // Two restores in a row: the later replaces the earlier, whole.
    restore_json(&env, &["--session", A]);
    restore_json(&env, &["--session", B]);
    let (only, _) = delivered(&start(&env, "f1000000-6666-4fff-8fff-ffffffffffff"))
        .expect("the later restore is delivered");
    assert!(only.contains("Speed up the CSV export"), "{only}");
    assert!(!only.contains("Invoice"), "{only}");
    assert!(start(&env, "f2000000-6666-4fff-8fff-ffffffffffff")
        .stdout
        .is_empty());

    // Another workspace sharing this Velra home.
    let other = env.dir.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    let in_other = |args: &[&str]| {
        env.cmd()
            .env("CLAUDE_PROJECT_DIR", &other)
            .current_dir(&other)
            .args(args)
            .output()
            .unwrap()
    };
    let refused = in_other(&["restore", "--session", A]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stdout).contains(A));
    // A capsule staged here is not delivered there.
    restore_json(&env, &["--session", A]);
    let mut p = env.base_payload("SessionStart");
    p["session_id"] = json!("f3000000-6666-4fff-8fff-ffffffffffff");
    p["source"] = json!("startup");
    p["cwd"] = json!(other);
    let out = env
        .cmd()
        .env("CLAUDE_PROJECT_DIR", &other)
        .current_dir(&other)
        .args(["hook", "session-start"])
        .write_stdin(p.to_string())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        staging::peek(&staged_dir(&env))
            .expect("still staged here")
            .source_session_id,
        A
    );
    restore(&env, &["--clear"]);

    // Unknown and too-short ids stage nothing.
    for bad in [
        "no-such-session",
        "a1b2c3",
        "a1b2c3d4-1111-4aaa-8aaa-aaaaaaaaaaab",
    ] {
        let out = restore(&env, &["--session", bad]);
        assert!(!out.status.success(), "{bad}: {out:?}");
        assert!(staging::records(&staged_dir(&env)).is_empty(), "{bad}");
    }
    let short = env
        .cmd()
        .args(["inspect", "--session", "a1b2c"])
        .output()
        .unwrap();
    assert!(!short.status.success(), "a short prefix is not an id");
}

/// Fail-open at the delivery boundary: with the database's write lock held,
/// a new session still receives the staged capsule, the hook stays inside
/// its contract, and its own event goes through the spool under its own id.
#[test]
fn a_locked_database_at_session_start_still_delivers_and_loses_nothing() {
    let env = record_two_sessions();
    restore_json(&env, &["--session", A]);
    let staged = staging::peek(&staged_dir(&env)).expect("staged");

    let dest = "d1000000-7777-4ddd-8ddd-dddddddddddd";
    let blocker = env.open_db();
    blocker.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = start(&env, dest);
    assert!(
        velra_core::spool::backlog(&env.spool_dir()) > 0,
        "the session start was spooled"
    );
    blocker.conn.execute_batch("ROLLBACK").unwrap();
    drop(blocker);

    let (capsule, _) = delivered(&out).expect("delivered while the database was locked");
    assert_eq!(capsule, staged.capsule);
    env.drain();
    let rows: Vec<(String, String)> = env
        .open_db()
        .conn
        .prepare("SELECT hook_event, project_id FROM events WHERE session_id = ?1")
        .unwrap()
        .query_map([dest], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [("SessionStart".to_string(), env.project_id())],
        "the destination's own event, recorded once, under its own id"
    );
    assert!(start(&env, dest).stdout.is_empty(), "delivered once");
}

/// Deterministic replay: the recorded events, appended in order to an empty
/// ledger and reduced, give the same snapshot and the same capsule; reducing
/// again changes nothing.
#[test]
fn replaying_the_recorded_events_rebuilds_the_same_capsule() {
    let env = record_two_sessions();
    // After every recorded event: a snapshot leaves out what happened after it.
    let meta = velra_core::restore::snapshot_meta(None, velra_core::time::now_ms() + 60_000, 0);
    let capsule_of = |db: &Db, session: &str| {
        let snap = velra_core::snapshot::build(&db.conn, session, &meta).unwrap();
        render::render(&snap, &RenderConfig::default()).text
    };
    let original = env.open_db();
    let (a, b) = (capsule_of(&original, A), capsule_of(&original, B));
    assert_carries_session_a(&velra_core::redact::redact(&a));

    let events: Vec<NewEvent> = original
        .conn
        .prepare(
            "SELECT e.dedupe_key, e.session_id, e.project_id, e.agent_id, e.hook_event, \
             e.tool_name, e.tool_use_id, e.ts_ms, e.payload, p.root_path, p.is_git \
             FROM events e JOIN projects p USING (project_id) ORDER BY e.id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok(NewEvent {
                dedupe_key: r.get(0)?,
                session_id: r.get(1)?,
                project_id: r.get(2)?,
                agent_id: r.get(3)?,
                hook_event: r.get(4)?,
                tool_name: r.get(5)?,
                tool_use_id: r.get(6)?,
                ts_ms: r.get(7)?,
                payload: r.get(8)?,
                project: Some(ProjectInfo {
                    project_id: r.get(2)?,
                    root_path: r.get(9)?,
                    is_git: r.get::<_, i64>(10)? != 0,
                }),
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(original);
    assert!(events.len() > 20, "{}", events.len());

    let dir = tempfile::tempdir().unwrap();
    let mut replay = Db::open(&dir.path().join("replay.db"), Role::Cli).unwrap();
    for ev in &events {
        velra_core::eventlog::insert_event(&replay.conn, ev).unwrap();
        // Delivered again: the dedupe key keeps it one row.
        assert!(velra_core::eventlog::insert_event(&replay.conn, ev)
            .unwrap()
            .is_none());
    }
    velra_core::reducer::reduce_all(&mut replay.conn, None).unwrap();
    assert_eq!(capsule_of(&replay, A), a, "A rebuilt");
    assert_eq!(capsule_of(&replay, B), b, "B rebuilt");
    velra_core::reducer::reduce_all(&mut replay.conn, None).unwrap();
    assert_eq!(capsule_of(&replay, A), a, "reducing again changes nothing");
}

/// D148: Velra registered twice for one event (user and project settings
/// under different binary paths) runs twice for every tool call, a few
/// milliseconds apart. Each tool call is still one event and one piece of
/// state: one edit, one command, a file edited once.
#[test]
fn a_hook_run_twice_for_one_tool_call_records_it_once() {
    let env = Env::new();
    env.write_file(ROUNDING, ORIGINAL);
    let s = A;
    start(&env, s);
    prompt(&env, s, "p1", TASK_A);
    let file = env.project.join(ROUNDING);
    let input =
        json!({ "file_path": file, "old_string": HALF_UP_LINE, "new_string": HALF_EVEN_LINE });
    for _ in 0..2 {
        hook(&env, s, "PreToolUse", |p| {
            p["tool_name"] = json!("Edit");
            p["tool_use_id"] = json!("t-edit");
            p["tool_input"] = input.clone();
        });
    }
    env.write_file(
        ROUNDING,
        &ORIGINAL.replacen(HALF_UP_LINE, HALF_EVEN_LINE, 1),
    );
    for _ in 0..2 {
        hook(&env, s, "PostToolUse", |p| {
            p["tool_name"] = json!("Edit");
            p["tool_use_id"] = json!("t-edit");
            p["tool_input"] = input.clone();
            p["tool_response"] = json!({ "filePath": file, "originalFile": ORIGINAL });
        });
    }
    let failure = pytest_failure(TEST_NEG, "assert round_total(x) == y", 0);
    for _ in 0..2 {
        failed_command(
            &env,
            s,
            "t-test",
            "python -m pytest tests/test_rounding.py",
            &failure,
        );
    }
    env.drain();

    for (table, expected) in [("edits", 1), ("commands", 1)] {
        assert_eq!(
            count(
                &env,
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                s
            ),
            expected,
            "{table}"
        );
    }
    assert_eq!(
        count(
            &env,
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND tool_use_id IS NOT NULL",
            s
        ),
        3,
        "one pre, one post, one failure"
    );
    let preview = env
        .cmd()
        .args(["inspect", "--session", s])
        .output()
        .unwrap();
    let preview = String::from_utf8_lossy(&preview.stdout);
    assert!(preview.contains("1 edits this task"), "{preview}");
    assert!(preview.contains(&format!("- FAIL {TEST_NEG}")), "{preview}");
}
