//! G1–G4: fail-open behaviour and chaos (§18).

mod common;

use common::Env;
use serde_json::json;

fn payload(env: &Env) -> String {
    json!({
        "session_id": env.session,
        "hook_event_name": "PostToolUse",
        "cwd": env.project.to_string_lossy(),
        "tool_name": "Read",
        "tool_use_id": "toolu_1",
        "tool_input": { "file_path": env.project.join("src/a.rs") }
    })
    .to_string()
}

#[test]
fn g1_an_unusable_home_never_blocks_a_hook() {
    let env = Env::new();
    // A file where the directory should be: nothing can be created under it.
    let blocked = env.dir.path().join("not-a-directory");
    std::fs::write(&blocked, b"x").expect("write blocker");
    let home = blocked.join("velra-home");

    for subcommand in [
        "session-start",
        "post-tool-use",
        "pre-compact",
        "stop",
        "session-end",
    ] {
        let out = env
            .cmd()
            .env("VELRA_HOME", &home)
            .arg("hook")
            .arg(subcommand)
            .write_stdin(payload(&env))
            .output()
            .expect("run hook");
        assert!(
            out.status.success(),
            "{subcommand} exited {:?}",
            out.status.code()
        );
        assert!(
            out.stdout.is_empty(),
            "{subcommand} printed {:?}",
            out.stdout
        );
        assert!(out.stderr.is_empty(), "{subcommand} wrote to stderr");
    }
}

#[test]
#[cfg(unix)]
fn g1_read_only_home_never_blocks_a_hook() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    std::fs::create_dir_all(&env.home).expect("home");
    std::fs::set_permissions(&env.home, std::fs::Permissions::from_mode(0o500)).expect("read only");

    let out = env.hook_raw("post-tool-use", payload(&env).as_bytes());
    // Restore permissions before any assertion so the temp dir can be cleaned.
    std::fs::set_permissions(&env.home, std::fs::Permissions::from_mode(0o700)).expect("restore");
    out.assert_contract();
    assert!(out.stdout.is_empty());
}

#[test]
fn g2_an_injected_panic_exits_zero_and_is_logged() {
    let env = Env::new();
    for subcommand in [
        "session-start",
        "user-prompt-submit",
        "post-tool-use",
        "pre-compact",
        "stop",
    ] {
        let out = env
            .cmd()
            .env("VELRA_TEST_PANIC", subcommand)
            .arg("hook")
            .arg(subcommand)
            .write_stdin(payload(&env))
            .output()
            .expect("run hook");
        assert!(
            out.status.success(),
            "{subcommand}: exit {:?}",
            out.status.code()
        );
        assert!(
            out.stdout.is_empty(),
            "{subcommand}: stdout must stay empty"
        );
        assert!(
            out.stderr.is_empty(),
            "{subcommand}: a panic must never reach stderr"
        );
    }
    let log = std::fs::read_to_string(env.home.join("logs/errors.log")).expect("errors.log");
    assert!(log.contains("panic:"), "panics are logged: {log}");
    assert_eq!(log.lines().filter(|l| l.contains("panic:")).count(), 5);
}

/// The first execution of a freshly linked binary on Windows pays a one-off
/// loader/anti-virus cost of seconds, which would otherwise be attributed to
/// the watchdog. One throwaway invocation warms it.
fn warm_binary(env: &Env) {
    let _ = env.cmd().arg("--version").output();
}

#[test]
fn g3_the_watchdog_abandons_work_at_the_deadline() {
    let env = Env::new();
    warm_binary(&env);
    let started = std::time::Instant::now();
    let out = env
        .cmd()
        .env("VELRA_TEST_STALL_MS", "5000")
        .arg("hook")
        .arg("post-tool-use")
        .write_stdin(payload(&env))
        .output()
        .expect("run hook");
    let elapsed = started.elapsed();

    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "no output when work is abandoned");
    assert!(out.stderr.is_empty());
    // 250 ms watchdog plus process start; far below the injected 5 s stall.
    assert!(
        elapsed.as_millis() < 2_000,
        "watchdog did not fire: {elapsed:?}"
    );
}

#[test]
fn g3_the_reduce_watchdog_uses_the_async_budget() {
    let env = Env::new();
    warm_binary(&env);
    let started = std::time::Instant::now();
    let out = env
        .cmd()
        .env("VELRA_TEST_STALL_MS", "8000")
        .arg("reduce")
        .write_stdin("{}")
        .output()
        .expect("run reduce");
    let elapsed = started.elapsed();
    assert!(out.status.success());
    assert!(
        elapsed.as_millis() < 4_000,
        "async watchdog did not fire: {elapsed:?}"
    );
}

#[test]
fn g4_the_kill_switch_prevents_all_database_access() {
    let env = Env::new();
    let out = env
        .cmd()
        .env("VELRA_DISABLE", "1")
        .arg("hook")
        .arg("post-tool-use")
        .write_stdin(payload(&env))
        .output()
        .expect("run hook");
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!env.db_path().exists(), "no database file is created");
    assert!(!env.home.join("logs").exists(), "nothing is written at all");

    // The disabled marker file has the same effect.
    std::fs::create_dir_all(&env.home).expect("home");
    std::fs::write(env.home.join("disabled"), b"").expect("marker");
    let out = env.hook_raw("post-tool-use", payload(&env).as_bytes());
    out.assert_contract();
    assert!(
        !env.db_path().exists(),
        "the marker file disables the hook path"
    );
}

#[test]
fn a_missing_binary_is_reported_by_doctor_and_repaired_by_enable() {
    let env = Env::new();
    std::fs::write(env.settings_path(), "{}\n").expect("settings");
    assert!(env
        .cmd()
        .arg("enable")
        .output()
        .expect("enable")
        .status
        .success());

    // Simulate the binary having moved.
    let state_path = env.home.join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("state")).expect("json");
    state["bin_path"] = json!(env.dir.path().join("gone").join("velra").to_string_lossy());
    std::fs::write(&state_path, state.to_string()).expect("write state");

    let out = env.cmd().arg("doctor").output().expect("doctor");
    assert_eq!(out.status.code(), Some(1));
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("binary missing"), "{report}");

    assert!(env
        .cmd()
        .arg("enable")
        .output()
        .expect("enable")
        .status
        .success());
    let out = env.cmd().arg("doctor").output().expect("doctor");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn hooks_work_when_the_project_is_not_a_git_repository() {
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    env.hook("user-prompt-submit", &{
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!("fix the failing test in the parser module");
        p
    })
    .assert_contract();
    let file = env.project.join("src/a.rs");
    env.hook("post-tool-use", &{
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!("t1");
        p["tool_input"] = json!({ "file_path": file, "old_string": "v0", "new_string": "v1" });
        p["tool_response"] = json!({ "filePath": file, "originalFile": "v0\n" });
        p
    })
    .assert_contract();

    let out = env.cmd().arg("inspect").output().expect("inspect");
    let capsule = String::from_utf8_lossy(&out.stdout);
    assert!(capsule.contains("no git"), "{capsule}");
    assert!(capsule.contains("[ROOT_TASK_OBJECTIVE]"));
}
