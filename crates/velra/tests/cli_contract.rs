//! The user-facing CLI's output contract (Phase 9).
//!
//! `docs/CLI.md`: human output goes to stdout; `--json` output is JSON on
//! stdout; a usage error exits 2. What these tests pin is the part a script
//! depends on -- the benchmark harness itself parses `velra restore --json`
//! and records `{}` for anything that does not parse:
//!
//! * with `--json`, stdout is one JSON document whatever happened, an error
//!   included (`{"error": …}`, exit 1), and a warning never shares it;
//! * a reader that goes away (`velra status | head -1`) ends the output, not
//!   the process with a panic.

mod common;

use common::{Env, Log};
use serde_json::Value;
use std::process::{Command, Stdio};

fn run(env: &Env, args: &[&str]) -> std::process::Output {
    env.cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.269")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .expect("run velra")
}

/// Stdout parsed as one JSON document, or a failure that shows what it was.
fn json_stdout(out: &std::process::Output, what: &str) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!(
            "{what}: stdout is not one JSON document ({e}):\n{text}\n--- stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// A workspace with one recorded session, reduced, and one event after it
/// that is not, so that a command catching the reducer up has work to do.
fn unreduced_workspace() -> Env {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("fix the retry test in tests/test_retry.py without touching the API");
    log.edit("src/a.py", "v1\n");
    log.command_fail(
        "python -m pytest tests/test_retry.py",
        1,
        "FAILED tests/test_retry.py::test_retry - AssertionError\n1 failed",
    );
    log.reduce();
    log.prompt("and keep the retry budget where it is");
    log.into_env()
}

/// The CLI catches the reducer up before it reads. When that fails -- here
/// another process holds the write lock -- it says so and carries on. With
/// `--json` the remark went to stdout ahead of the document, and every JSON
/// reader got nothing: `velra restore --json` in the benchmark's own harness
/// recorded no staged path at all.
#[test]
fn a_warning_never_shares_stdout_with_json() {
    let env = unreduced_workspace();
    let session = env.session.clone();
    let blocker = env.open_db();
    blocker
        .conn
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold the write lock");
    for args in [
        vec!["inspect", "--json"],
        vec!["restore", "--list", "--json"],
        vec!["restore", "--session", &session, "--dry-run", "--json"],
    ] {
        let out = run(&env, &args);
        let value = json_stdout(&out, &args.join(" "));
        assert!(value.is_object(), "{args:?}: {value}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Could not catch up the reducer"),
            "{args:?}: the warning is still reported, on stderr: {stderr}"
        );
    }
    blocker.conn.execute_batch("ROLLBACK").expect("release");
}

/// Every failure a `--json` command can report is itself JSON: an `error`
/// string on stdout, exit 1. Before, each printed its human line (`✗ No
/// checkpoint x.`), which no JSON reader can tell from a crash.
///
/// `--session` naming a session the ledger never recorded is one of those
/// failures. It used to render a preview of nothing and exit 0, which reads
/// as "this session has no state" rather than "there is no such session".
#[test]
fn a_json_command_that_fails_says_so_in_json() {
    // No database at all.
    let empty = Env::new();
    for args in [
        vec!["inspect", "--json"],
        vec!["restore", "--list", "--json"],
        vec!["restore", "--session", "s", "--json"],
    ] {
        let out = run(&empty, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let value = json_stdout(&out, &args.join(" "));
        assert!(
            value["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{args:?}: {value}"
        );
    }

    let env = unreduced_workspace();
    for args in [
        vec!["inspect", "--json", "--section", "nonsense"],
        vec!["inspect", "--json", "--checkpoint", "ckpt_missing"],
        vec!["inspect", "--json", "--session", "no-such-session"],
        vec!["restore", "--session", "no-such-session", "--json"],
    ] {
        let out = run(&env, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let value = json_stdout(&out, &args.join(" "));
        assert!(
            value["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{args:?}: {value}"
        );
    }
}

/// The two `--json` forms that printed plain text on success.
#[test]
fn every_json_form_prints_json() {
    let env = unreduced_workspace();
    let out = run(&env, &["inspect", "--json", "--section", "failure"]);
    assert_eq!(out.status.code(), Some(0));
    let value = json_stdout(&out, "inspect --section --json");
    assert_eq!(value["section"], "failure", "{value}");
    assert!(value["detail"].as_str().is_some(), "{value}");

    for _ in 0..2 {
        let out = run(&env, &["restore", "--clear", "--json"]);
        assert_eq!(out.status.code(), Some(0));
        let value = json_stdout(&out, "restore --clear --json");
        assert!(value["cleared"].is_boolean(), "{value}");
        assert!(value["workspace_root"].is_string(), "{value}");
    }
}

/// A human-mode failure still reports on stdout, as documented, with the
/// exit status saying it failed; a usage error is clap's, exit 2.
#[test]
fn human_output_and_exit_codes_are_unchanged() {
    let env = Env::new();
    let out = run(&env, &["inspect"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("No Velra database yet"));
    assert!(out.stderr.is_empty());
    let out = run(&env, &["inspect", "--no-such-flag"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
}

/// `velra doctor | head -1`: the reader leaves after one line. Writing the
/// rest failed with a broken pipe, which `println!` turns into a panic --
/// `thread 'main' panicked … failed printing to stdout` on stderr and exit
/// 101 -- on every platform (Rust ignores SIGPIPE, so the write returns the
/// error rather than ending the process).
#[test]
fn a_reader_that_goes_away_ends_the_output_not_the_process() {
    let env = unreduced_workspace();
    for args in [
        vec!["doctor"],
        vec!["status"],
        vec!["inspect"],
        vec!["inspect", "--json"],
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_velra"));
        cmd.env("VELRA_HOME", &env.home)
            .env("CLAUDE_CONFIG_DIR", &env.config)
            .env("CLAUDE_PROJECT_DIR", &env.project)
            .env("VELRA_CLAUDE_VERSION", "2.1.269")
            .env_remove("VELRA_DISABLE")
            .env_remove("VELRA_LOG")
            .current_dir(&env.project)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn velra");
        // Close the read end before the child has written anything.
        drop(child.stdout.take());
        let out = child.wait_with_output().expect("wait");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        assert_ne!(out.status.code(), Some(101), "{args:?}: {stderr}");
    }
}

/// `status --json` reports the staged slot the human output reports; it
/// used to leave it out, so a script could not tell that the next session
/// would start with restored state, or that a staged capsule was unreadable.
#[test]
fn status_json_reports_the_staged_slot() {
    let env = unreduced_workspace();
    let session = env.session.clone();
    let staged = |env: &Env| {
        json_stdout(&run(env, &["status", "--json"]), "status --json")["staged"].clone()
    };
    assert_eq!(staged(&env)["state"], "empty");

    let out = run(&env, &["restore", "--session", &session, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let path = json_stdout(&out, "restore --json")["staged_path"]
        .as_str()
        .expect("staged_path")
        .to_string();
    let now = staged(&env);
    assert_eq!(now["state"], "staged", "{now}");
    assert_eq!(
        now["capsule"]["source_session_id"],
        session.as_str(),
        "{now}"
    );

    std::fs::write(&path, b"{ not a capsule").unwrap();
    let now = staged(&env);
    assert_eq!(now["state"], "unreadable", "{now}");
    // Human output says the same.
    let text = String::from_utf8_lossy(&run(&env, &["status"]).stdout).into_owned();
    assert!(text.contains("unreadable"), "{text}");
}
