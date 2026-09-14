//! B1–B4: the hook IPC contract (§8).

mod common;

use common::Env;
use proptest::prelude::*;
use serde_json::json;
use std::path::PathBuf;

const SUBCOMMANDS: [&str; 9] = [
    "session-start",
    "user-prompt-submit",
    "pre-tool-use",
    "post-tool-use",
    "post-tool-use-failure",
    "stop",
    "pre-compact",
    "post-compact",
    "session-end",
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/claude-code")
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let versions = std::fs::read_dir(fixture_dir()).expect("fixtures directory");
    for version in versions.flatten() {
        let Ok(files) = std::fs::read_dir(version.path()) else {
            continue;
        };
        for file in files.flatten() {
            if file.path().extension().is_some_and(|e| e == "json") {
                let name = format!(
                    "{}/{}",
                    version.file_name().to_string_lossy(),
                    file.file_name().to_string_lossy()
                );
                out.push((name, std::fs::read(file.path()).expect("read fixture")));
            }
        }
    }
    assert!(!out.is_empty(), "no recorded fixtures found");
    out
}

#[test]
fn b1_every_subcommand_honours_the_contract_for_every_fixture() {
    let env = Env::new();
    for (name, payload) in fixtures() {
        for subcommand in SUBCOMMANDS {
            let out = env.hook_raw(subcommand, &payload);
            assert_eq!(
                out.code, 0,
                "{subcommand} on {name}: exit {} stderr {}",
                out.code, out.stderr
            );
            assert!(
                out.stderr.is_empty(),
                "{subcommand} on {name}: stderr {}",
                out.stderr
            );
            out.assert_contract();
        }
    }
}

#[test]
fn b1_unknown_events_and_tools_are_recorded_minimally() {
    let env = Env::new();
    let payload = json!({
        "session_id": env.session,
        "hook_event_name": "SomethingNew",
        "cwd": env.project.to_string_lossy(),
        "tool_name": "mcp__memory__write",
        "tool_use_id": "toolu_x",
        "tool_input": { "anything": [1, 2, 3] },
        "tool_response": { "ok": true }
    });
    env.hook("post-tool-use", &payload).assert_contract();
    env.hook("not-a-real-event", &payload).assert_contract();

    let events = env.assert_event_count(2);
    let tools: Vec<&str> = events.iter().map(|e| e.label()).collect();
    assert!(tools.contains(&"mcp__memory__write"), "{tools:?}");
}

#[test]
fn b1_missing_session_id_is_a_no_op() {
    let env = Env::new();
    let out = env.hook(
        "post-tool-use",
        &json!({ "hook_event_name": "PostToolUse", "tool_name": "Read" }),
    );
    out.assert_contract();
    assert!(out.stdout.is_empty());
    assert!(
        !env.db_path().exists(),
        "no database is created without a session id"
    );
}

#[test]
fn b1_malformed_stdin_still_records_an_event_when_a_session_id_is_recoverable() {
    let env = Env::new();
    let broken = format!("{{\"session_id\": \"{}\", \"tool_name\": ", env.session);
    env.hook_raw("post-tool-use", broken.as_bytes())
        .assert_contract();

    let events = env.assert_event_count(1);
    assert_eq!(events[0].hook_event, "malformed");
}

#[test]
fn b1_empty_and_binary_stdin_are_no_ops() {
    let env = Env::new();
    for input in [
        b"".to_vec(),
        b"not json at all".to_vec(),
        vec![0xff, 0xfe, 0x00, 0x01],
    ] {
        for subcommand in SUBCOMMANDS {
            env.hook_raw(subcommand, &input).assert_contract();
        }
    }
}

#[test]
fn b3_delivery_json_matches_the_specified_shape() {
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    let base = |event: &str| env.base_payload(event);

    env.hook("session-start", &{
        let mut p = base("SessionStart");
        p["source"] = json!("startup");
        p
    })
    .assert_contract();
    env.hook("user-prompt-submit", &{
        let mut p = base("UserPromptSubmit");
        p["prompt"] = json!("fix the flaky logout test and keep cookies intact");
        p["prompt_id"] = json!("p1");
        p
    })
    .assert_contract();
    let file = env.project.join("src/a.rs");
    env.hook("post-tool-use", &{
        let mut p = base("PostToolUse");
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!("t1");
        p["tool_input"] = json!({ "file_path": file, "old_string": "v0", "new_string": "v1" });
        p["tool_response"] = json!({ "filePath": file, "originalFile": "v0\n" });
        p
    })
    .assert_contract();
    env.hook("post-tool-use-failure", &{
        let mut p = base("PostToolUseFailure");
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!("t2");
        p["tool_input"] = json!({ "command": "pytest -x" });
        p["error"] = json!("Exit code 1\nFAILED tests/test_a.py::test_x\n1 failed");
        p
    })
    .assert_contract();

    // PreCompact freezes a checkpoint and reports it.
    let pre = env.hook("pre-compact", &{
        let mut p = base("PreCompact");
        p["trigger"] = json!("manual");
        p
    });
    pre.assert_contract();
    let message = pre.json().expect("checkpoint message");
    assert!(
        message["systemMessage"]
            .as_str()
            .expect("systemMessage")
            .starts_with("\u{26a1} Velra checkpoint saved:"),
        "{message}"
    );

    // Channel 1: SessionStart(compact).
    let delivery = env.hook("session-start", &{
        let mut p = base("SessionStart");
        p["source"] = json!("compact");
        p
    });
    delivery.assert_contract();
    let value = delivery.json().expect("delivery json");
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"],
        json!("SessionStart")
    );
    let capsule = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("capsule");
    assert!(capsule.starts_with("<VELRA_CONTINUATION"));
    assert!(
        capsule.len() <= 9_500,
        "capsule stays under the hook output limit"
    );
    let system = value["systemMessage"].as_str().expect("systemMessage");
    assert!(system.starts_with("\u{26a1} Velra restored: "), "{system}");
    assert!(system.ends_with(" tokens)"), "{system}");
    assert_eq!(
        value.as_object().expect("object").len(),
        2,
        "only the two documented keys"
    );
}

#[test]
fn b3_post_tool_and_user_prompt_channels_report_their_own_event_name() {
    // Channel 2: the first PostToolUse after the checkpoint.
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    env.hook("user-prompt-submit", &{
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!("fix the flaky logout test and keep cookies intact");
        p
    })
    .assert_contract();
    env.hook("pre-compact", &{
        let mut p = env.base_payload("PreCompact");
        p["trigger"] = json!("auto");
        p
    })
    .assert_contract();
    let out = env.hook("post-tool-use", &{
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!("t-after-compact");
        p["tool_input"] = json!({ "file_path": env.project.join("src/a.rs") });
        p
    });
    out.assert_contract();
    let value = out.json().expect("delivery");
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"],
        json!("PostToolUse")
    );

    // Channel 3: UserPromptSubmit.
    let env = Env::new();
    env.hook("user-prompt-submit", &{
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!("fix the flaky logout test and keep cookies intact");
        p
    })
    .assert_contract();
    env.hook("pre-compact", &{
        let mut p = env.base_payload("PreCompact");
        p["trigger"] = json!("manual");
        p
    })
    .assert_contract();
    let out = env.hook("user-prompt-submit", &{
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!("what should we try next?");
        p["prompt_id"] = json!("p-after");
        p
    });
    out.assert_contract();
    let value = out.json().expect("delivery");
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"],
        json!("UserPromptSubmit")
    );
}

/// B4: an 8 MiB tool response is normalized down to the payload budget.
///
/// Gated behind `fault-injection` because it depends on the harness watchdog
/// override (`VELRA_TEST_WATCHDOG_MS`), which the binary honours only under
/// that feature. Without it the 250 ms sync watchdog stays live, and
/// normalizing 8 MiB can reach that deadline on a slow or loaded runner —
/// the event is armed for the spool only *after* normalizing, so a deadline
/// that fires first drops it by design (§4) and the assertion below races.
/// An unthrottled `cargo test` therefore skips this case rather than
/// flaking; `--features fault-injection` runs it deterministically.
#[cfg(feature = "fault-injection")]
#[test]
fn b4_huge_tool_response_is_retained_within_budget() {
    let env = Env::new();
    let huge = "y".repeat(8 * 1024 * 1024);
    let payload = json!({
        "session_id": env.session,
        "hook_event_name": "PostToolUse",
        "cwd": env.project.to_string_lossy(),
        "tool_name": "Bash",
        "tool_use_id": "toolu_huge",
        "tool_input": { "command": "cargo test" },
        "tool_response": { "stdout": huge, "stderr": "", "interrupted": false }
    });
    let started = std::time::Instant::now();
    // The harness runs every subprocess with `common::TEST_WATCHDOG_MS`, which
    // keeps the 250 ms sync deadline from landing mid-normalize and sending
    // the event to the spool. What is measured here is the payload budget.
    env.hook("post-tool-use", &payload).assert_contract();
    let elapsed = started.elapsed();

    let events = env.assert_event_count(1);
    let stored = &events[0].payload;
    assert_eq!(events[0].tool_name.as_deref(), Some("Bash"));
    assert!(
        stored.len() <= 16 * 1024,
        "retained payload is {} bytes",
        stored.len()
    );
    assert!(stored.contains("cargo test"));
    // Generous ceiling for a debug build on shared CI; §4 measures release.
    assert!(elapsed.as_millis() < 5_000, "took {elapsed:?}");
}

#[test]
fn b4_oversized_stdin_is_drained_without_blocking() {
    let env = Env::new();
    // Larger than the 64 MiB cap: the hook must keep reading and still exit 0.
    let mut payload = format!(
        "{{\"session_id\":\"{}\",\"hook_event_name\":\"Stop\",\"pad\":\"",
        env.session
    )
    .into_bytes();
    payload.extend(std::iter::repeat_n(b'x', 65 * 1024 * 1024));
    payload.extend(b"\"}");
    let out = env.hook_raw("stop", &payload);
    out.assert_contract();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// B2: random, truncated and oversized stdin never breaks the contract.
    #[test]
    fn b2_fuzz_stdin_never_breaks_the_contract(
        bytes in prop::collection::vec(any::<u8>(), 0..4096),
        subcommand in prop::sample::select(SUBCOMMANDS.as_slice()),
    ) {
        let env = Env::new();
        let out = env.hook_raw(subcommand, &bytes);
        prop_assert_eq!(out.code, 0);
        prop_assert!(out.stderr.is_empty());
        if !out.stdout.is_empty() {
            let line = out.stdout.trim_end_matches('\n');
            prop_assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }

    /// B2: valid JSON with wrong types for every field.
    #[test]
    fn b2_fuzz_wrong_types_never_break_the_contract(
        session in prop::option::of("[a-z0-9-]{1,32}"),
        weird in prop::sample::select(vec!["null", "123", "true", "[1,2]", "{\"a\":1}", "\"\""]),
    ) {
        let env = Env::new();
        let payload = format!(
            "{{\"session_id\":{},\"prompt\":{weird},\"tool_name\":{weird},\"tool_input\":{weird},\
              \"tool_response\":{weird},\"trigger\":{weird},\"source\":{weird},\"error\":{weird}}}",
            session.map(|s| format!("\"{s}\"")).unwrap_or_else(|| "null".into())
        );
        for subcommand in SUBCOMMANDS {
            let out = env.hook_raw(subcommand, payload.as_bytes());
            prop_assert_eq!(out.code, 0, "{}", out.stderr);
            prop_assert!(out.stderr.is_empty());
        }
    }
}

/// Long-running fuzz for CI (`VELRA_FUZZ_SECONDS` per subcommand).
#[test]
#[ignore = "long fuzz; run with --ignored in CI"]
fn b2_extended_fuzz() {
    let seconds: u64 = std::env::var("VELRA_FUZZ_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let env = Env::new();
    let mut state: u64 = 0x2545F4914F6CDD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for subcommand in SUBCOMMANDS {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        let mut runs = 0u64;
        while std::time::Instant::now() < deadline {
            let len = (next() % 8192) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (next() % 256) as u8).collect();
            let out = env.hook_raw(subcommand, &bytes);
            assert_eq!(out.code, 0, "{subcommand}: stderr {}", out.stderr);
            assert!(out.stderr.is_empty());
            runs += 1;
        }
        println!("{subcommand}: {runs} fuzz runs");
    }
}

/// A `git restore` that is not the first word of the command line, observed
/// through the real binary.
///
/// Both halves of this failed in the v0.1 benchmark. `PreToolUse` was
/// registered with an `if` rule of `Bash(git *)`, a prefix match that never
/// saw `cd "..." && git restore ...`; and `PostToolUse` only read git effects
/// from calls that succeeded, while `git restore x && pytest` is reported as a
/// failure whenever the suite still fails. Between them, Velra could observe
/// nothing at all about a revert the agent had just performed.
#[test]
fn b1_a_chained_git_restore_is_observed_on_both_sides_and_on_failure() {
    let env = Env::new();
    env.write_file("src/money.py", "ROUND_HALF_EVEN\n");
    let command = format!(
        "cd \"{}\" && git restore src/money.py && python -m pytest -q",
        env.project.to_string_lossy()
    );

    let pre = json!({
        "session_id": env.session,
        "hook_event_name": "PreToolUse",
        "cwd": env.project.to_string_lossy(),
        "tool_name": "Bash",
        "tool_use_id": "toolu_chain",
        "tool_input": { "command": command },
    });
    env.hook("pre-tool-use", &pre).assert_contract();

    // The suite still fails, so Claude Code reports the whole call as failed.
    env.write_file("src/money.py", "ROUND_HALF_UP\n");
    let post = json!({
        "session_id": env.session,
        "hook_event_name": "PostToolUseFailure",
        "cwd": env.project.to_string_lossy(),
        "tool_name": "Bash",
        "tool_use_id": "toolu_chain",
        "tool_input": { "command": command },
        "error": "Exit code 1\nFAILED tests/test_engine.py::test_exact_payment\n1 failed",
    });
    env.hook("post-tool-use-failure", &post).assert_contract();

    let events = env.assert_event_count(2);
    for event in &events {
        assert_eq!(
            event.json()["git"]["restore"].as_str(),
            Some("git restore src/money.py"),
            "{} records the restore subcommand, not the whole line: {}",
            event.hook_event,
            event.payload
        );
    }
}
