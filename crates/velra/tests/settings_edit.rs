//! A2–A7: `velra enable` / `velra disable` against real settings files (§6).

mod common;

use common::Env;
use std::path::Path;

const PLAIN: &str = "{}\n";

const WITH_COMMENTS: &str = r#"{
  // keep my own hooks first
  "model": "opus",
  /* block comment */
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write",
        "hooks": [{ "type": "command", "command": "echo wrote" }]
      },
    ],
  },
}
"#;

const FOUR_SPACE: &str = r#"{
    "permissions": {
        "allow": ["Bash(git status)"]
    },
    "hooks": {
        "Stop": [
            {
                "hooks": [{ "type": "command", "command": "echo stopped" }]
            }
        ]
    }
}
"#;

const TAB_INDENT: &str = "{\n\t\"model\": \"opus\",\n\t\"hooks\": {\n\t\t\"SessionStart\": [\n\t\t\t{\n\t\t\t\t\"hooks\": [{ \"type\": \"command\", \"command\": \"echo start\" }]\n\t\t\t}\n\t\t]\n\t}\n}\n";

const NO_HOOKS_KEY: &str = "{\n  \"model\": \"opus\"\n}\n";

fn write_settings(env: &Env, text: &str) {
    std::fs::write(env.settings_path(), text).expect("write settings");
}

fn read_settings(env: &Env) -> String {
    std::fs::read_to_string(env.settings_path()).expect("read settings")
}

fn enable(env: &Env) -> std::process::Output {
    env.cmd().arg("enable").output().expect("run enable")
}

fn disable(env: &Env) -> std::process::Output {
    env.cmd().arg("disable").output().expect("run disable")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn golden_settings(name: &str, text: &str) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../../../tests/golden/settings");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| insta::assert_snapshot!(name, text));
}

/// The binary path appears verbatim in the file; replace it so goldens are
/// machine-independent.
fn normalize(text: &str) -> String {
    let exe = assert_cmd::cargo::cargo_bin("velra");
    let raw = exe.to_string_lossy().to_string();
    let escaped = serde_json::to_string(&raw).unwrap_or_default();
    let escaped = escaped.trim_matches('"');
    text.replace(escaped, "<VELRA_BIN>")
        .replace(&raw, "<VELRA_BIN>")
        .replace(&raw.replace('\\', "/"), "<VELRA_BIN>")
}

#[test]
fn a2_enable_preserves_comments_trailing_commas_and_foreign_hooks() {
    let env = Env::new();
    write_settings(&env, WITH_COMMENTS);
    let out = enable(&env);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = read_settings(&env);

    assert!(
        after.contains("// keep my own hooks first"),
        "line comment preserved"
    );
    assert!(
        after.contains("/* block comment */"),
        "block comment preserved"
    );
    assert!(
        after.contains(r#""command": "echo wrote""#),
        "foreign handler preserved"
    );
    assert!(after.contains(r#""matcher": "Write""#));
    assert!(after.contains("velra"), "Velra handlers added");
    golden_settings("with_comments", &normalize(&after));
}

#[test]
fn a2_indentation_style_is_detected() {
    for (name, text, indent) in [
        ("four_space", FOUR_SPACE, "    "),
        ("tab_indent", TAB_INDENT, "\t"),
        ("plain", PLAIN, "  "),
    ] {
        let env = Env::new();
        write_settings(&env, text);
        assert!(enable(&env).status.success());
        let after = read_settings(&env);
        let velra_line = after
            .lines()
            .find(|l| l.contains("\"hook\"") || l.contains("\"reduce\""))
            .expect("a Velra args line");
        let leading: String = velra_line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        assert!(
            leading.starts_with(indent),
            "{name}: {leading:?} should start with {indent:?}"
        );
        golden_settings(name, &normalize(&after));
    }
}

#[test]
fn a3_enable_is_idempotent_and_disable_restores_bytes() {
    for (name, original) in [
        ("plain", PLAIN),
        ("comments", WITH_COMMENTS),
        ("four_space", FOUR_SPACE),
        ("tab", TAB_INDENT),
        ("no_hooks_key", NO_HOOKS_KEY),
    ] {
        let env = Env::new();
        write_settings(&env, original);

        assert!(enable(&env).status.success(), "{name}: first enable");
        let first = read_settings(&env);
        let second_out = enable(&env);
        assert!(second_out.status.success());
        assert_eq!(
            first,
            read_settings(&env),
            "{name}: enable twice must be byte-identical"
        );
        assert!(
            stdout(&second_out).contains("already enabled"),
            "{name}: {}",
            stdout(&second_out)
        );

        assert!(disable(&env).status.success(), "{name}: disable");
        assert_eq!(
            read_settings(&env),
            original,
            "{name}: disable must restore the original bytes"
        );
    }
}

#[test]
fn a4_invalid_jsonc_is_refused_without_touching_the_file() {
    let env = Env::new();
    let broken = "{\n  \"hooks\": {\n    \"Stop\": [ { \n}\n";
    write_settings(&env, broken);
    let out = enable(&env);
    assert_eq!(out.status.code(), Some(1));
    let message = stdout(&out);
    assert!(message.contains("Could not parse"), "{message}");
    assert!(message.contains("line"), "{message}");
    assert!(message.contains("column"), "{message}");
    assert!(message.contains("No changes made."), "{message}");
    assert_eq!(read_settings(&env), broken, "file untouched");
}

#[test]
fn a5_a_foreign_edit_between_runs_is_preserved() {
    let env = Env::new();
    write_settings(&env, PLAIN);
    assert!(enable(&env).status.success());

    // Someone else edits the file between Velra runs.
    let mut text = read_settings(&env);
    text = text.replacen(
        '{',
        "{\n  \"statusLine\": { \"type\": \"command\", \"command\": \"mine\" },",
        1,
    );
    std::fs::write(env.settings_path(), &text).expect("foreign edit");

    assert!(enable(&env).status.success());
    let after = read_settings(&env);
    assert!(
        after.contains("\"statusLine\""),
        "foreign edit survives re-enable"
    );

    assert!(disable(&env).status.success());
    let after_disable = read_settings(&env);
    assert!(
        after_disable.contains("\"statusLine\""),
        "foreign edit survives disable"
    );
    assert!(
        !after_disable.contains("velra"),
        "Velra handlers removed: {after_disable}"
    );
}

#[test]
fn a6_dry_run_writes_nothing_and_prints_a_diff() {
    let env = Env::new();
    write_settings(&env, FOUR_SPACE);
    let before = read_settings(&env);
    let out = env
        .cmd()
        .arg("enable")
        .arg("--dry-run")
        .output()
        .expect("dry run");
    assert!(out.status.success());
    let printed = stdout(&out);
    assert!(printed.contains("+++ b/"), "unified diff header: {printed}");
    assert!(printed.contains("velra"), "diff mentions the new handlers");
    assert!(printed.contains("dry run: nothing was written"));
    assert_eq!(read_settings(&env), before, "file untouched");
    assert!(
        !env.home.join("state.json").exists(),
        "no state written on a dry run"
    );
}

#[test]
fn a7_older_claude_code_gets_shell_form_and_no_if_field() {
    let env = Env::new();
    write_settings(&env, PLAIN);
    let out = env
        .cmd()
        .arg("enable")
        .env("VELRA_CLAUDE_VERSION", "2.1.80")
        .output()
        .expect("enable");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = read_settings(&env);

    assert!(
        !after.contains("\"args\""),
        "no exec form before 2.1.139:\n{after}"
    );
    assert!(
        after.contains("hook post-tool-use"),
        "shell form command string:\n{after}"
    );
    assert!(
        after.contains("\\\"") || after.contains("\" "),
        "path is quoted in the command string"
    );
    assert!(!after.contains("\"if\""), "no `if` field before 2.1.85");
    // Events the old version does not know are skipped.
    assert!(
        !after.contains("PostToolBatch"),
        "PostToolBatch skipped:\n{after}"
    );
    assert!(stdout(&out).contains("Skipped"), "{}", stdout(&out));
    golden_settings("old_claude_code", &normalize(&after));

    // Upgrading rewrites the handlers in place rather than duplicating them.
    let out = env
        .cmd()
        .arg("enable")
        .env("VELRA_CLAUDE_VERSION", "2.1.268")
        .output()
        .expect("enable");
    assert!(out.status.success());
    let upgraded = read_settings(&env);
    assert!(upgraded.contains("\"args\""), "exec form after upgrade");
    assert!(
        !upgraded.contains("hook post-tool-use"),
        "the shell form is gone:\n{upgraded}"
    );
    for role in [
        "post-tool-use",
        "post-tool-use-failure",
        "session-start",
        "pre-compact",
    ] {
        let needle = format!("\"{role}\"");
        assert_eq!(
            upgraded.matches(&needle).count(),
            1,
            "exactly one handler for {role}:\n{upgraded}"
        );
    }
}

#[test]
fn enable_creates_the_settings_file_when_claude_code_is_absent() {
    let env = Env::new();
    let nested = env.dir.path().join("no-claude-here");
    let out = env
        .cmd()
        .arg("enable")
        .env("CLAUDE_CONFIG_DIR", &nested)
        .output()
        .expect("enable");
    assert!(out.status.success());
    let printed = stdout(&out);
    assert!(printed.contains("Claude Code not detected"), "{printed}");
    let created = nested.join("settings.json");
    assert!(created.exists());
    let text = std::fs::read_to_string(&created).expect("read");
    assert!(text.contains("velra"));
}

#[test]
fn disable_removes_the_hooks_key_only_when_velra_created_it() {
    let env = Env::new();
    write_settings(&env, NO_HOOKS_KEY);
    assert!(enable(&env).status.success());
    assert!(read_settings(&env).contains("\"hooks\""));
    assert!(disable(&env).status.success());
    let after = read_settings(&env);
    assert!(
        !after.contains("\"hooks\""),
        "hooks key removed again: {after}"
    );
    assert_eq!(after, NO_HOOKS_KEY);

    // With a pre-existing hooks key, the key stays.
    let env = Env::new();
    write_settings(&env, FOUR_SPACE);
    assert!(enable(&env).status.success());
    assert!(disable(&env).status.success());
    let after = read_settings(&env);
    assert!(
        after.contains("\"hooks\""),
        "pre-existing hooks key kept: {after}"
    );
    assert!(after.contains("echo stopped"));
}

#[test]
fn backups_are_written_and_capped() {
    let env = Env::new();
    write_settings(&env, PLAIN);
    for _ in 0..12 {
        assert!(enable(&env).status.success());
        assert!(disable(&env).status.success());
    }
    let backups = env.home.join("backups");
    let count = std::fs::read_dir(&backups).expect("backups dir").count();
    assert!(count <= 10, "at most 10 backups are kept, found {count}");
    assert!(count > 0);
}

#[test]
#[cfg(unix)]
fn a2_symlinked_settings_file_is_edited_through_the_link() {
    let env = Env::new();
    let real = env.dir.path().join("dotfiles").join("claude-settings.json");
    std::fs::create_dir_all(real.parent().unwrap()).expect("dotfiles dir");
    std::fs::write(&real, FOUR_SPACE).expect("write real file");
    std::os::unix::fs::symlink(&real, env.settings_path()).expect("symlink");

    assert!(enable(&env).status.success());
    assert!(
        std::fs::symlink_metadata(env.settings_path())
            .expect("meta")
            .file_type()
            .is_symlink(),
        "the symlink itself must survive"
    );
    let through_link = std::fs::read_to_string(env.settings_path()).expect("read link");
    let target = std::fs::read_to_string(&real).expect("read target");
    assert_eq!(through_link, target);
    assert!(target.contains("velra"));

    assert!(disable(&env).status.success());
    assert_eq!(
        std::fs::read_to_string(&real).expect("read target"),
        FOUR_SPACE
    );
}

#[test]
fn status_and_doctor_report_the_installation() {
    let env = Env::new();
    write_settings(&env, PLAIN);
    let out = env.cmd().arg("status").output().expect("status");
    assert_eq!(out.status.code(), Some(1), "not enabled yet");
    assert!(stdout(&out).contains("Not enabled"));

    assert!(enable(&env).status.success());
    let out = env.cmd().arg("status").output().expect("status");
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("Enabled"));

    let out = env.cmd().arg("doctor").output().expect("doctor");
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let report = stdout(&out);
    assert!(report.contains("settings parse"));
    assert!(
        report.contains("hook handlers registered across"),
        "{report}"
    );
    // The count is handlers, not events: `Stop` alone carries two.
    assert!(
        report.contains("13 hook handlers registered across 10 events"),
        "{report}"
    );

    let out = env
        .cmd()
        .arg("doctor")
        .arg("--json")
        .output()
        .expect("doctor json");
    let value: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert_eq!(value["healthy"], serde_json::json!(true));
}

#[test]
fn the_stable_path_is_recorded_in_state_json() {
    let env = Env::new();
    write_settings(&env, PLAIN);
    assert!(enable(&env).status.success());
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.home.join("state.json")).expect("state"))
            .expect("json");
    let bin = state["bin_path"].as_str().expect("bin_path");
    assert!(
        Path::new(bin).exists(),
        "recorded binary path exists: {bin}"
    );
    assert_eq!(state["hooks_existed_before"], serde_json::json!(false));
}
