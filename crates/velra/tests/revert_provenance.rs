//! Revert, dead-end and file-version provenance (Phase 3 hardening).
//!
//! What a file's history proves, and what it does not. Every conclusion the
//! reducer draws about an attempt -- tried, reverted, discarded by a git
//! command, reapplied -- is drawn from observations of file content. These
//! tests pin which observation may support which conclusion:
//!
//! * a git restore-family command is credited only with the files its own
//!   pathspec reaches; another subcommand's change to another file is not its
//!   doing;
//! * a failing command's mentioned paths are what existed when it ran, never
//!   what the disk holds when the reducer happens to run;
//! * a fingerprint that is not a digest of the bytes (`large:…`, `absent`,
//!   `unreadable`) cannot establish that two states are the same;
//! * a delayed pre-effect observation cannot reopen or close anything.
//!
//! Most tests drive the real hook binary, so the observation set is the one
//! the product takes (every file the session edited), not the files a test
//! happened to list.

mod common;

use common::{Env, Log};
use serde_json::{json, Value};
use velra_core::event::{GitObservation, Payload};

// ------------------------------------------------------------- harness

fn prompt_hook(env: &Env, text: &str) {
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!(text);
    env.hook("user-prompt-submit", &p).assert_contract();
}

/// One Edit tool call through the real pre/post hooks.
fn edit_hooks(env: &Env, id: &str, rel: &str, after: &str) {
    let path = env.project.join(rel);
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    let file = path.to_string_lossy().into_owned();
    let input = json!({"file_path": file, "old_string": before, "new_string": after});
    let mut p = env.base_payload("PreToolUse");
    p["tool_name"] = json!("Edit");
    p["tool_use_id"] = json!(id);
    p["tool_input"] = input.clone();
    env.hook("pre-tool-use", &p).assert_contract();
    env.write_file(rel, after);
    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = json!("Edit");
    p["tool_use_id"] = json!(id);
    p["tool_input"] = input;
    p["tool_response"] =
        json!({"filePath": file, "originalFile": before, "oldString": before, "newString": after});
    env.hook("post-tool-use", &p).assert_contract();
}

/// One shell tool call through the real pre/post hooks. `effect` runs between
/// them, standing in for what the command does to the work tree.
fn shell_hooks(
    env: &Env,
    tool: &str,
    id: &str,
    command: &str,
    effect: impl FnOnce(&Env),
    result: Result<&str, &str>,
) {
    let mut p = env.base_payload("PreToolUse");
    p["tool_name"] = json!(tool);
    p["tool_use_id"] = json!(id);
    p["tool_input"] = json!({"command": command});
    env.hook("pre-tool-use", &p).assert_contract();
    effect(env);
    match result {
        Ok(stdout) => {
            let mut p = env.base_payload("PostToolUse");
            p["tool_name"] = json!(tool);
            p["tool_use_id"] = json!(id);
            p["tool_input"] = json!({"command": command});
            p["tool_response"] =
                json!({"stdout": stdout, "stderr": "", "interrupted": false, "exitCode": 0});
            env.hook("post-tool-use", &p).assert_contract();
        }
        Err(output) => {
            let mut p = env.base_payload("PostToolUseFailure");
            p["tool_name"] = json!(tool);
            p["tool_use_id"] = json!(id);
            p["tool_input"] = json!({"command": command});
            p["error"] = json!(format!("Exit code 1\n{output}"));
            p["is_interrupt"] = json!(false);
            env.hook("post-tool-use-failure", &p).assert_contract();
        }
    }
}

fn stop_hook(env: &Env) {
    let mut p = env.base_payload("Stop");
    p["stop_hook_active"] = json!(false);
    env.hook("stop", &p).assert_contract();
}

/// `(path, status, mechanism)` of every edit, in id order, once `n` events
/// are in the ledger and reduced.
fn edits_after(env: &Env, n: usize) -> Vec<(String, String, Option<String>)> {
    env.drain_and_load_events(n);
    env.drain();
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare("SELECT path, status, mechanism FROM edits ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// `(path, mechanism, command_text, reapplied)` of every dead end.
fn dead_ends(env: &Env) -> Vec<(String, String, Option<String>, i64)> {
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare("SELECT path, mechanism, command_text, reapplied FROM dead_ends ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn in_failure(env: &Env) -> Vec<String> {
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare("SELECT path FROM file_stats WHERE in_failure = 1 ORDER BY path")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The distinct paths of the last failing command's mentions, in order.
fn mentioned_paths(env: &Env) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in mentions(env) {
        if let Some(p) = m["path"].as_str() {
            if !out.iter().any(|o| o == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn mentions(env: &Env) -> Vec<Value> {
    let db = env.open_db();
    let raw: Option<String> = db
        .conn
        .query_row(
            "SELECT mentioned_paths FROM commands WHERE outcome = 'FAIL' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    raw.map(|m| serde_json::from_str::<Vec<Value>>(&m).unwrap())
        .unwrap_or_default()
}

// ------------------------------------------- 6. git effect attribution

/// Two files edited; one compound command regenerates `a.rs` and restores
/// `b.rs`. Only `b.rs` is the restore's doing. The real hook hashes every file
/// the session edited around the command, so `a.rs` changed across it too.
#[test]
fn a_restore_is_not_credited_with_another_subcommands_change() {
    let env = Env::new();
    env.write_file("a.rs", "a0\n");
    env.write_file("b.rs", "b0\n");
    prompt_hook(&env, "change both modules and keep the tests passing");
    edit_hooks(&env, "t1", "a.rs", "a1\n");
    edit_hooks(&env, "t2", "b.rs", "b1\n");
    shell_hooks(
        &env,
        "Bash",
        "t3",
        "python tools/regen.py && git restore b.rs",
        |env| {
            env.write_file("a.rs", "a2 regenerated\n");
            env.write_file("b.rs", "b0\n");
        },
        Ok(""),
    );
    let edits = edits_after(&env, 7);
    assert_eq!(
        edits,
        vec![
            ("a.rs".into(), "ACTIVE".into(), None),
            (
                "b.rs".into(),
                "DISCARDED".into(),
                Some("git_command".into())
            ),
        ],
        "only the restored file's edit is discarded"
    );
    let dead = dead_ends(&env);
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].0, "b.rs");
    assert_eq!(dead[0].2.as_deref(), Some("git restore b.rs"));
}

/// The same shape, where the other subcommand puts `a.rs` back exactly: that
/// is a revert, but not the git command's. It is recorded with the mechanism
/// that claims nothing about the cause.
#[test]
fn a_revert_by_another_subcommand_is_not_attributed_to_git() {
    let env = Env::new();
    env.write_file("a.rs", "a0\n");
    env.write_file("b.rs", "b0\n");
    prompt_hook(&env, "change both modules and keep the tests passing");
    edit_hooks(&env, "t1", "a.rs", "a1\n");
    edit_hooks(&env, "t2", "b.rs", "b1\n");
    shell_hooks(
        &env,
        "Bash",
        "t3",
        "cp backup/a.rs a.rs && git restore b.rs",
        |env| {
            env.write_file("a.rs", "a0\n");
            env.write_file("b.rs", "b0\n");
        },
        Ok(""),
    );
    // The turn-end scan sees the same content and adds nothing.
    stop_hook(&env);
    edits_after(&env, 8);
    let mut dead = dead_ends(&env);
    dead.sort();
    assert_eq!(dead.len(), 2, "{dead:?}");
    assert_eq!(dead[0].0, "a.rs");
    assert_eq!(dead[0].1, "external", "{dead:?}");
    assert_eq!(
        dead[0].2, None,
        "no git command is named for a.rs: {dead:?}"
    );
    assert_eq!(dead[1].0, "b.rs");
    assert_eq!(dead[1].1, "git_command");
}

// ------------------------------- 5. failed commands and historical state

/// A failing test names a file that does not exist when it runs; the file is
/// created before the reducer runs. The mention is what the command saw.
#[test]
fn a_mention_is_checked_when_the_command_ran_not_when_it_was_reduced() {
    let env = Env::new();
    env.write_file("tests/test_pay.py", "def test_pay(): ...\n");
    prompt_hook(&env, "make tests/test_pay.py pass");
    let output = "tests/test_pay.py:3: in test_pay\nsrc/late.py:9: ImportError\n\
                  FAILED tests/test_pay.py::test_pay - ImportError\n1 failed";
    shell_hooks(
        &env,
        "Bash",
        "t1",
        "python -m pytest tests/test_pay.py",
        |_| {},
        Err(output),
    );
    // Created after the command ran, before anything reduced it.
    env.write_file("src/late.py", "x = 1\n");
    env.drain_and_load_events(3);
    env.drain();
    let paths = mentioned_paths(&env);
    assert_eq!(paths, ["tests/test_pay.py"], "{paths:?}");
    assert_eq!(in_failure(&env), ["tests/test_pay.py"]);
}

/// The converse: the file existed when the command ran and is gone by the
/// time the reducer runs. It was named by the failure, and stays named.
#[test]
fn a_mention_survives_the_file_being_removed_before_reduction() {
    let env = Env::new();
    env.write_file("tests/test_pay.py", "def test_pay(): ...\n");
    env.write_file("src/pay.py", "def pay(): ...\n");
    prompt_hook(&env, "make tests/test_pay.py pass");
    let output = "src/pay.py:1: in pay\nFAILED tests/test_pay.py::test_pay - boom\n1 failed";
    shell_hooks(
        &env,
        "Bash",
        "t1",
        "python -m pytest tests/test_pay.py",
        |_| {},
        Err(output),
    );
    std::fs::remove_file(env.project.join("src/pay.py")).unwrap();
    env.drain_and_load_events(3);
    env.drain();
    let paths = mentioned_paths(&env);
    assert_eq!(paths, ["src/pay.py", "tests/test_pay.py"], "{paths:?}");
}

// ---------------------------------------------- 4. path-mention provenance

/// A traceback that passes through installed packages inside the workspace
/// (`.venv/…/site-packages`, `node_modules`) names them before the project's
/// own file. They are not the task's files and are not the next target.
#[test]
fn third_party_frames_are_not_task_files() {
    let env = Env::new();
    env.write_file(
        ".venv/lib/python3.12/site-packages/_pytest/python.py",
        "# vendored\n",
    );
    env.write_file("node_modules/lib/index.js", "// vendored\n");
    env.write_file("tests/test_pay.py", "def test_pay(): ...\n");
    env.write_file("src/pay.py", "def pay(): ...\n");
    prompt_hook(&env, "make tests/test_pay.py pass");
    let output =
        ".venv/lib/python3.12/site-packages/_pytest/python.py:194: in pytest_pyfunc_call\n\
                  node_modules/lib/index.js:3: in helper\n\
                  src/pay.py:1: in pay\n\
                  FAILED tests/test_pay.py::test_pay - boom\n1 failed";
    shell_hooks(
        &env,
        "Bash",
        "t1",
        "python -m pytest tests/test_pay.py",
        |_| {},
        Err(output),
    );
    env.drain_and_load_events(3);
    env.drain();
    let paths = mentioned_paths(&env);
    assert_eq!(paths, ["src/pay.py", "tests/test_pay.py"], "{paths:?}");
    assert_eq!(in_failure(&env), ["src/pay.py", "tests/test_pay.py"]);
}

// --------------------------------------------- 3. file-state identity

/// Files over the hashing limit are identified by size and mtime, not bytes.
/// Two such fingerprints being equal says nothing about the content, so they
/// never make a revert or a reapplication.
#[test]
fn a_large_file_fingerprint_is_not_content_identity() {
    let mut log = Log::new();
    log.env.write_file("big.bin", "v0\n");
    log.prompt("patch the generated table in big.bin");
    let session = log.session();
    // Hand-built history: the pre-edit and a later scan see the same
    // fingerprint, as a file that grew while it was read, or whose mtime
    // could not be read, reports it.
    let fp = "large:5000000:0";
    log.append(
        "PreToolUse",
        Some("Edit"),
        Payload {
            path: Some("big.bin".into()),
            pre_hash: Some(fp.into()),
            ..Default::default()
        },
    );
    log.append(
        "PostToolUse",
        Some("Edit"),
        Payload {
            path: Some("big.bin".into()),
            post_hash: Some("large:5000100:0".into()),
            ..Default::default()
        },
    );
    log.append(
        "Stop",
        None,
        Payload {
            turn_scan: Some(vec![velra_core::event::FileObservation {
                path: "big.bin".into(),
                hash: fp.into(),
                size: 5_000_000,
            }]),
            ..Default::default()
        },
    );
    log.reduce();
    assert!(log.dead_ends().is_empty(), "{:?}", log.dead_ends());
    assert_eq!(log.edits()[0].1, "ACTIVE", "{:?} ({session})", log.edits());
}

// ------------------------------- 2. the known reapplication ordering case

/// `pre_edit post_edit pre_edit post_edit turn_scan git_pre git_post`, with the
/// restore's two rows ingested late -- in every order, duplicated, and with a
/// second session's events interleaved. None of it reopens, closes or
/// duplicates the dead end.
#[test]
fn the_delayed_restore_rows_never_change_the_conclusion() {
    const ORIGINAL: &str = "rounding = ROUND_HALF_UP\n";
    const INTERMEDIATE: &str = "rounding = ROUND_HALF_UP  # ?\n";
    const DEAD_END: &str = "rounding = ROUND_HALF_EVEN\n";
    #[derive(Clone, Copy, Debug)]
    enum Shape {
        PreThenPost,
        PostThenPre,
        DuplicatePre,
        DuplicatePost,
        RepeatedScan,
        OtherSessionBetween,
    }
    for shape in [
        Shape::PreThenPost,
        Shape::PostThenPre,
        Shape::DuplicatePre,
        Shape::DuplicatePost,
        Shape::RepeatedScan,
        Shape::OtherSessionBetween,
    ] {
        let mut log = Log::new();
        log.env.write_file("src/money.py", ORIGINAL);
        log.prompt("the gap is one cent, so start with the rounding hypothesis");
        log.edit("src/money.py", INTERMEDIATE);
        log.edit("src/money.py", DEAD_END);
        let dead_end_files = log.observe(&["src/money.py"]);
        let restore_ts = log.ts + 1_000;
        log.env.write_file("src/money.py", ORIGINAL);
        let restored_files = log.observe(&["src/money.py"]);
        log.stop();
        if matches!(shape, Shape::RepeatedScan) {
            log.stop();
        }
        log.reduce();
        let git = |files| {
            Some(GitObservation {
                restore: Some("git restore src/money.py".into()),
                commit: false,
                files,
            })
        };
        let pre = Payload {
            command: Some("git restore src/money.py".into()),
            git: git(dead_end_files.clone()),
            ..Default::default()
        };
        let post = Payload {
            command: Some("git restore src/money.py".into()),
            cwd: Some(log.env.project.to_string_lossy().into_owned()),
            stdout_tail: Some(String::new()),
            git: git(restored_files.clone()),
            ..Default::default()
        };
        if matches!(shape, Shape::OtherSessionBetween) {
            let own = log.session();
            log.switch_session("other-session");
            log.prompt("an unrelated task in the same workspace");
            log.edit("src/money.py", DEAD_END);
            log.switch_session(&own);
            log.env.write_file("src/money.py", ORIGINAL);
        }
        match shape {
            Shape::PostThenPre => {
                log.append_late("PostToolUse", Some("Bash"), post.clone(), restore_ts + 1);
                log.append_late("PreToolUse", Some("Bash"), pre.clone(), restore_ts);
            }
            _ => {
                log.append_late("PreToolUse", Some("Bash"), pre.clone(), restore_ts);
                if matches!(shape, Shape::DuplicatePre) {
                    log.append_late("PreToolUse", Some("Bash"), pre.clone(), restore_ts);
                }
                log.append_late("PostToolUse", Some("Bash"), post.clone(), restore_ts + 1);
                if matches!(shape, Shape::DuplicatePost) {
                    log.append_late("PostToolUse", Some("Bash"), post.clone(), restore_ts + 1);
                }
            }
        }
        log.reduce();
        let own_dead: Vec<_> = {
            let mut stmt = log
                .db
                .conn
                .prepare("SELECT path, reapplied FROM dead_ends WHERE session_id = ?1 ORDER BY id")
                .unwrap();
            stmt.query_map([log.session()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        assert_eq!(
            own_dead,
            vec![("src/money.py".to_string(), 0)],
            "{shape:?}: exactly one open dead end"
        );
        let reapplied: i64 = log
            .db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM edits WHERE session_id = ?1 AND status = 'REAPPLIED'",
                [log.session()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reapplied, 0, "{shape:?}");
        let capsule = log.capsule();
        assert!(capsule.contains("[REVERTED_EDITS]"), "{shape:?}\n{capsule}");
    }
}

// ------------------------------------ 6. shells, wrappers and directories

/// The PowerShell tool's quoting: `"C:\proj\"` is a complete string there, so
/// the `cd` that precedes the restore is read, and the restore is credited
/// with its own file and nothing else.
#[test]
fn a_powershell_restore_after_a_quoted_cd_is_attributed_to_its_file() {
    let env = Env::new();
    env.write_file("a.rs", "a0\n");
    env.write_file("b.rs", "b0\n");
    prompt_hook(&env, "change both modules and keep the tests passing");
    edit_hooks(&env, "t1", "a.rs", "a1\n");
    edit_hooks(&env, "t2", "b.rs", "b1\n");
    let project = env.project.to_string_lossy().into_owned();
    let sep = std::path::MAIN_SEPARATOR;
    let command = format!("cd \"{project}{sep}\" ; python gen.py ; git restore b.rs");
    shell_hooks(
        &env,
        "PowerShell",
        "t3",
        &command,
        |env| {
            env.write_file("a.rs", "a2 regenerated\n");
            env.write_file("b.rs", "b0\n");
        },
        Ok(""),
    );
    let edits = edits_after(&env, 7);
    assert_eq!(
        edits[0],
        ("a.rs".into(), "ACTIVE".into(), None),
        "{edits:?}"
    );
    assert_eq!(edits[1].1, "DISCARDED", "{edits:?}");
    let dead = dead_ends(&env);
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].2.as_deref(), Some("git restore b.rs"));
}

/// A restore inside `bash -c "…"` is seen by the hook (which takes the
/// observation) and credited by the reducer.
#[test]
fn a_restore_inside_a_shell_wrapper_is_observed_and_attributed() {
    let env = Env::new();
    env.write_file("src/a.py", "v0\n");
    prompt_hook(&env, "try the change in src/a.py, then undo it");
    edit_hooks(&env, "t1", "src/a.py", "v1\n");
    shell_hooks(
        &env,
        "Bash",
        "t2",
        "bash -lc \"cd src && git restore a.py\" && python -m pytest -q",
        |env| {
            env.write_file("src/a.py", "v0\n");
        },
        Err("FAILED tests/test_a.py::test_a - assert 0\n1 failed"),
    );
    let edits = edits_after(&env, 5);
    assert_eq!(
        edits,
        vec![(
            "src/a.py".into(),
            "DISCARDED".into(),
            Some("git_command".into())
        )]
    );
    let dead = dead_ends(&env);
    // The dead end quotes the git call that did it, not its wrapper.
    assert_eq!(dead[0].2.as_deref(), Some("git restore a.py"), "{dead:?}");
}

/// A restore run in some other checkout cannot have changed this project's
/// file, even when the file did change across the command.
#[test]
fn a_restore_in_another_directory_is_not_credited() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("try the change in src/a.py, then undo it");
    log.edit("src/a.py", "v1\n");
    let other = log.env.dir.path().join("other-checkout");
    std::fs::create_dir_all(&other).unwrap();
    let command = format!("cd \"{}\" && git restore src/a.py", other.to_string_lossy());
    log.git_restore(&command, &[("src/a.py", "v0\n")]);
    log.reduce();
    let dead = log.dead_ends();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].1, "external", "{dead:?}");
    assert_eq!(dead[0].2, None);
    assert_eq!(log.edits()[0].1, "REVERTED");
}

/// When the line does not say which files a restore reaches, the change is
/// recorded without crediting git: the revert is seen, its cause is not
/// invented.
#[test]
fn an_unreadable_pathspec_credits_nothing() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("try the change in src/a.py, then undo it");
    log.edit("src/a.py", "v1\n");
    log.git_restore(
        "git restore $(git diff --name-only)",
        &[("src/a.py", "v0\n")],
    );
    log.reduce();
    let dead = log.dead_ends();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].1, "external", "{dead:?}");
    assert_eq!(dead[0].2, None, "{dead:?}");
}

// ------------------------------------------ 7. same bytes, different cause

/// A file back at bytes it held before is a revert whatever wrote them; the
/// mechanism recorded is the one the observation can support, never more.
#[test]
fn the_cause_of_a_return_is_what_observed_it() {
    type Undo = fn(&mut Log);
    let cases: [(&str, Undo); 4] = [
        ("inverse_edit", |log| log.edit("src/a.py", "v0\n")),
        ("rewrite", |log| log.write_tool("src/a.py", "v0\n")),
        ("external", |log| {
            // `cp`, a generator, the user's editor: seen at the turn's end.
            log.env.write_file("src/a.py", "v0\n");
            log.stop();
        }),
        ("git_command", |log| {
            log.git_restore("git checkout HEAD -- src/a.py", &[("src/a.py", "v0\n")])
        }),
    ];
    for (mechanism, undo) in cases {
        let mut log = Log::new();
        log.env.write_file("src/a.py", "v0\n");
        log.prompt("try the change in src/a.py, then undo it");
        log.edit("src/a.py", "v1\n");
        undo(&mut log);
        log.reduce();
        let dead = log.dead_ends();
        assert_eq!(dead.len(), 1, "{mechanism}: {dead:?}");
        assert_eq!(dead[0].1, mechanism, "{dead:?}");
    }
}

// ------------------------------------------- 3. deletion and recreation

/// A file the session created and that is then deleted has returned to not
/// existing: a revert of the creation. Recreating it with the same bytes
/// brings the route back; with other bytes it does not.
#[test]
fn deletion_and_recreation_stay_distinguishable() {
    for (again, reapplied) in [("x = 1\n", 1), ("x = 2\n", 0)] {
        let mut log = Log::new();
        log.prompt("add a helper module in src/new.py");
        log.write_tool("src/new.py", "x = 1\n");
        std::fs::remove_file(log.env.project.join("src/new.py")).unwrap();
        log.stop();
        log.reduce();
        let dead = log.dead_ends();
        assert_eq!(dead.len(), 1, "{dead:?}");
        assert_eq!(dead[0].1, "external");
        assert_eq!(dead[0].3, 0);
        let absent: Vec<String> = log
            .versions("src/new.py")
            .into_iter()
            .map(|(_, h)| h)
            .collect();
        assert!(
            absent
                .iter()
                .filter(|h| *h == velra_core::hash::ABSENT)
                .count()
                == 2,
            "absent before the creation and after the deletion: {absent:?}"
        );

        log.write_tool("src/new.py", again);
        log.reduce();
        assert_eq!(log.dead_ends()[0].3, reapplied, "recreated as {again:?}");
    }
}

/// A turn scan that could not read the file observes nothing about its
/// content: it neither reverts the edit nor matches anything.
#[test]
fn an_unreadable_observation_changes_no_conclusion() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("change src/a.py");
    log.edit("src/a.py", "v1\n");
    for _ in 0..2 {
        log.append(
            "Stop",
            None,
            Payload {
                turn_scan: Some(vec![velra_core::event::FileObservation {
                    path: "src/a.py".into(),
                    hash: velra_core::hash::UNREADABLE.into(),
                    size: 0,
                }]),
                ..Default::default()
            },
        );
    }
    log.reduce();
    assert!(log.dead_ends().is_empty(), "{:?}", log.dead_ends());
    assert_eq!(log.edits()[0].1, "ACTIVE");
}

// ------------------------------------- 2. a genuine later settled state

/// The guard against a delayed `git_pre` does not stop a real return: the
/// discarded content seen again by a turn scan, or put back by a git command
/// that reaches the file, closes the dead end.
#[test]
fn a_later_settled_observation_does_reapply() {
    const ORIGINAL: &str = "rounding = ROUND_HALF_UP\n";
    const DEAD_END: &str = "rounding = ROUND_HALF_EVEN\n";
    for via in ["turn_scan", "git_post"] {
        let mut log = Log::new();
        log.env.write_file("src/money.py", ORIGINAL);
        log.prompt("the gap is one cent, so start with the rounding hypothesis");
        log.edit("src/money.py", DEAD_END);
        log.git_restore("git restore src/money.py", &[("src/money.py", ORIGINAL)]);
        log.reduce();
        assert_eq!(log.dead_ends()[0].3, 0, "{via}");
        match via {
            "turn_scan" => {
                log.env.write_file("src/money.py", DEAD_END);
                log.stop();
            }
            _ => log.git_restore(
                "git checkout stash@{0} -- src/money.py",
                &[("src/money.py", DEAD_END)],
            ),
        }
        log.reduce();
        assert_eq!(log.dead_ends().len(), 1, "{via}");
        assert_eq!(log.dead_ends()[0].3, 1, "{via}: reapplied");
    }
}

// --------------------------------------------- 8. out-of-order reverts

/// Two files, each edited and then edited back. In one ledger the events
/// arrive in order; in the other the reverting edits were spooled and are
/// ingested after the turn-end scan that saw their result. Both fold to the
/// same edits and dead ends, attributed to the edits that did the reverting.
#[test]
fn delayed_reverting_edits_fold_to_the_in_order_state() {
    type Edits = Vec<(String, String, Option<String>)>;
    type DeadEnds = Vec<(String, String)>;
    fn session(late: bool) -> (Edits, DeadEnds) {
        let mut log = Log::new();
        log.env.write_file("a.py", "a0\n");
        log.env.write_file("b.py", "b0\n");
        log.prompt("try the two-module change, then back it out");
        log.edit("a.py", "a1\n");
        log.edit("b.py", "b1\n");
        // The reverting edits, as their hooks recorded them.
        let mut undo = Vec::new();
        for (rel, before, after) in [("a.py", "a1\n", "a0\n"), ("b.py", "b1\n", "b0\n")] {
            let pre = log.observe(&[rel])[0].clone();
            log.env.write_file(rel, after);
            let post = log.observe(&[rel])[0].clone();
            undo.push((
                rel,
                Payload {
                    path: Some(rel.into()),
                    pre_hash: Some(pre.hash),
                    size: Some(pre.size),
                    ..Default::default()
                },
                Payload {
                    path: Some(rel.into()),
                    post_hash: Some(post.hash),
                    size: Some(post.size),
                    excerpt: Some(format!("- {}\n+ {}", before.trim(), after.trim())),
                    ..Default::default()
                },
            ));
        }
        let ts0 = log.ts;
        if late {
            log.stop();
            for (i, (_, pre, post)) in undo.into_iter().enumerate() {
                let ts = ts0 + 100 + 10 * i as i64;
                log.append_late("PreToolUse", Some("Edit"), pre, ts);
                log.append_late("PostToolUse", Some("Edit"), post, ts + 1);
            }
        } else {
            for (_, pre, post) in undo {
                log.append("PreToolUse", Some("Edit"), pre);
                log.append("PostToolUse", Some("Edit"), post);
            }
            log.stop();
        }
        log.reduce();
        let dead: Vec<(String, String)> = log
            .dead_ends()
            .into_iter()
            .map(|(p, m, _, _)| (p, m))
            .collect();
        let edits = log.edits();
        (edits, dead)
    }
    let in_order = session(false);
    let late = session(true);
    assert_eq!(
        in_order.1,
        vec![
            ("a.py".to_string(), "inverse_edit".to_string()),
            ("b.py".to_string(), "inverse_edit".to_string())
        ]
    );
    assert_eq!(late, in_order);
}

/// The same restore event delivered twice (a spool file written, then
/// written again by a retry) is one event: one discard, one dead end.
#[test]
fn a_restore_delivered_twice_is_one_restore() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("try the change in src/a.py, then undo it");
    log.edit("src/a.py", "v1\n");
    let before = log.observe(&["src/a.py"]);
    log.env.write_file("src/a.py", "v0\n");
    let after = log.observe(&["src/a.py"]);
    let git = |files| {
        Some(GitObservation {
            restore: Some("git restore src/a.py".into()),
            commit: false,
            files,
        })
    };
    let session = log.session();
    let spool = log.env.spool_dir();
    let ts = log.ts + 1_000;
    for (event, payload, at) in [
        (
            "PreToolUse",
            Payload {
                command: Some("git restore src/a.py".into()),
                git: git(before),
                ..Default::default()
            },
            ts,
        ),
        (
            "PostToolUse",
            Payload {
                command: Some("git restore src/a.py".into()),
                cwd: Some(log.env.project.to_string_lossy().into_owned()),
                stdout_tail: Some(String::new()),
                git: git(after),
                ..Default::default()
            },
            ts + 1,
        ),
    ] {
        let ev = velra_core::event::NewEvent {
            dedupe_key: velra_core::event::dedupe_key(
                event,
                &session,
                Some("toolu_restore"),
                None,
                at,
                None,
            ),
            session_id: session.clone(),
            project_id: log.env.project_id(),
            agent_id: None,
            hook_event: event.into(),
            tool_name: Some("Bash".into()),
            tool_use_id: Some("toolu_restore".into()),
            ts_ms: at,
            payload: payload.to_json(),
            project: Some(log.env.project_info()),
        };
        velra_core::spool::write(&spool, &ev).unwrap();
        velra_core::spool::write(&spool, &ev).unwrap();
    }
    log.reduce();
    log.reduce();
    let dead = log.dead_ends();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].1, "git_command");
    assert_eq!(log.edits()[0].1, "DISCARDED");
    let rows: i64 = log
        .db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE tool_use_id = 'toolu_restore'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2, "one pre and one post");
}

// ----------------------------------- 5. mentions: legacy and late events

/// An event written before the hook recorded mentions has none: the reducer
/// does not check its paths against the disk as it is when it runs.
#[test]
fn an_event_without_recorded_mentions_names_no_file() {
    let mut log = Log::new();
    log.env.write_file("src/pay.py", "def pay(): ...\n");
    log.prompt("make tests/test_pay.py pass");
    log.append(
        "PostToolUseFailure",
        Some("Bash"),
        Payload {
            command: Some("python -m pytest tests/test_pay.py".into()),
            cwd: Some(log.env.project.to_string_lossy().into_owned()),
            error: Some("Exit code 1\nsrc/pay.py:1: in pay\n1 failed".into()),
            tool_name: Some("Bash".into()),
            ..Default::default()
        },
    );
    log.reduce();
    let raw: Option<String> = log
        .db
        .conn
        .query_row("SELECT mentioned_paths FROM commands", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, None);
    let flagged: i64 = log
        .db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM file_stats WHERE in_failure = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 0);
}

/// A failing run spooled and ingested late -- after its file was deleted,
/// and folded by a rebuild -- keeps the mentions it was recorded with.
#[test]
fn a_late_failing_run_keeps_the_mentions_it_was_recorded_with() {
    let mut log = Log::new();
    log.env.write_file("src/pay.py", "def pay(): ...\n");
    log.prompt("make tests/test_pay.py pass");
    let ts = log.ts + 500;
    let payload = log.with_mentions(
        Payload {
            command: Some("python -m pytest tests/test_pay.py".into()),
            cwd: Some(log.env.project.to_string_lossy().into_owned()),
            error: Some("Exit code 1\nsrc/pay.py:1: in pay\n1 failed".into()),
            tool_name: Some("Bash".into()),
            ..Default::default()
        },
        true,
    );
    log.prompt("and keep the public API unchanged");
    log.reduce();
    std::fs::remove_file(log.env.project.join("src/pay.py")).unwrap();
    log.append_late("PostToolUseFailure", Some("Bash"), payload, ts);
    log.reduce();
    let raw: Option<String> = log
        .db
        .conn
        .query_row("SELECT mentioned_paths FROM commands", [], |r| r.get(0))
        .unwrap();
    let raw = raw.expect("mentions kept");
    assert!(raw.contains("\"src/pay.py\""), "{raw}");
}

// ------------------------------------------------ 9. dead-end state quality

/// A dead end made of several edits keeps, in the ledger, every edit it
/// groups with what each changed: the content that was finally rejected
/// (`ROUND_HALF_EVEN`) as well as the intermediate step before it. What the
/// snapshot picks from them is a selection decision made downstream.
#[test]
fn a_dead_end_keeps_every_edit_it_groups() {
    let mut log = Log::new();
    log.env
        .write_file("src/money.py", "rounding = ROUND_HALF_UP\n");
    log.prompt("the gap is one cent, so start with the rounding hypothesis");
    log.edit("src/money.py", "rounding = ROUND_HALF_UP  # ?\n");
    log.edit("src/money.py", "rounding = ROUND_HALF_EVEN\n");
    log.git_restore(
        "git restore src/money.py",
        &[("src/money.py", "rounding = ROUND_HALF_UP\n")],
    );
    log.reduce();
    let ids: String = log
        .db
        .conn
        .query_row("SELECT edit_ids FROM dead_ends", [], |r| r.get(0))
        .unwrap();
    let ids: Vec<i64> = serde_json::from_str(&ids).unwrap();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let excerpts: Vec<String> = ids
        .iter()
        .map(|id| {
            log.db
                .conn
                .query_row("SELECT excerpt FROM edits WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        })
        .collect();
    assert!(
        excerpts
            .last()
            .unwrap()
            .contains("+ rounding = ROUND_HALF_EVEN"),
        "the rejected content is state: {excerpts:?}"
    );
    let snap = log.snapshot();
    assert_eq!(snap.dead_ends.len(), 1);
    assert_eq!(snap.dead_ends[0].edit_ids, ids);
}
