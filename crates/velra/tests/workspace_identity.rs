//! Workspace identity across the two processes that must agree on it.
//!
//! The hook runs inside Claude Code, which sets `CLAUDE_PROJECT_DIR` to the
//! directory it was started in. `velra restore` runs in a terminal, which has
//! no such variable. Every test here drives the real binary twice, in two
//! environments built separately: the hook with the variable, as Claude Code
//! runs it, and the CLI with the variable removed and only its working
//! directory to go on. A CLI given the variable would agree with the hook by
//! construction and prove nothing about its own discovery.
//!
//! What must agree, per workspace: the id events are recorded under (the
//! `sessions` row), the id `velra restore` lists and stages under, and the
//! directory `SessionStart` claims from.

mod common;

use common::Env;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use velra_core::staging::{self, source::STARTUP};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// The hook as Claude Code runs it: started in `project_dir`.
fn hook(env: &Env, project_dir: &Path, event: &str, payload: Value) -> common::HookOutput {
    let out = env
        .cmd()
        .env("CLAUDE_PROJECT_DIR", project_dir)
        .current_dir(project_dir)
        .args(["hook", event])
        .write_stdin(payload.to_string())
        .output()
        .expect("run hook");
    common::HookOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The CLI as a terminal runs it: no `CLAUDE_PROJECT_DIR`, only `cwd`.
fn cli(env: &Env, cwd: &Path, args: &[&str]) -> std::process::Output {
    env.cmd()
        .env_remove("CLAUDE_PROJECT_DIR")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run velra")
}

fn cli_json(env: &Env, cwd: &Path, args: &[&str]) -> Value {
    let out = cli(env, cwd, args);
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .unwrap_or_else(|e| panic!("{args:?} from {}: {e}: {out:?}", cwd.display()))
}

/// A session recorded by the real hooks, started in `project_dir`, with a
/// prompt to restore.
fn record_session(env: &Env, project_dir: &Path, session: &str, prompt: &str) {
    let base = json!({
        "session_id": session,
        "cwd": project_dir.to_string_lossy(),
        "transcript_path": env.dir.path().join(format!("{session}.jsonl")).to_string_lossy(),
    });
    let mut start = base.clone();
    start["hook_event_name"] = json!("SessionStart");
    start["source"] = json!(STARTUP);
    assert_eq!(hook(env, project_dir, "session-start", start).code, 0);
    let mut submit = base;
    submit["hook_event_name"] = json!("UserPromptSubmit");
    submit["prompt"] = json!(prompt);
    assert_eq!(hook(env, project_dir, "user-prompt-submit", submit).code, 0);
}

/// The id the hook recorded a session under.
fn recorded_id(env: &Env, session: &str) -> String {
    env.drain_and_query(
        |r: &Option<String>| r.is_some(),
        |db| {
            db.conn
                .query_row(
                    "SELECT project_id FROM sessions WHERE session_id = ?1",
                    [session],
                    |r| r.get(0),
                )
                .ok()
        },
    )
    .expect("session recorded")
}

/// A brand-new session starting in `project_dir`; its stdout.
fn session_start(env: &Env, project_dir: &Path, session: &str) -> String {
    let out = hook(
        env,
        project_dir,
        "session-start",
        json!({
            "session_id": session,
            "hook_event_name": "SessionStart",
            "source": STARTUP,
            "cwd": project_dir.to_string_lossy(),
        }),
    );
    assert_eq!(out.code, 0);
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    out.stdout
}

fn delivered_context(stdout: &str) -> String {
    let v: Value = serde_json::from_str(stdout.trim_end()).expect("delivery json");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_string()
}

/// A repository with a package directory below its root.
fn repo(env: &Env) -> (PathBuf, PathBuf) {
    env.init_git("main", SHA);
    let pkg = env.project.join("packages/foo");
    std::fs::create_dir_all(pkg.join("src/deep")).expect("dirs");
    (env.project.clone(), pkg)
}

// ------------------------------------------------------------- 6D matrix

/// Claude Code started at the repository root; the terminal at the root, or
/// anywhere below it.
#[test]
fn claude_at_the_repository_root_and_the_cli_at_the_root_or_below_agree() {
    let env = Env::new();
    let (root, pkg) = repo(&env);
    record_session(&env, &root, "s-root", "fix the parser at the root");
    let hook_id = recorded_id(&env, "s-root");

    for cwd in [root.clone(), pkg.clone(), pkg.join("src/deep")] {
        let listed = cli_json(&env, &cwd, &["restore", "--list", "--json"]);
        assert_eq!(listed["workspace_id"], hook_id, "from {}", cwd.display());
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .filter_map(|s| s["session_id"].as_str())
            .collect();
        assert_eq!(ids, ["s-root"], "from {}", cwd.display());
    }
}

/// The case the shared mapping got wrong: Claude Code started in a package of
/// the repository records the package as the workspace, and a terminal in the
/// package resolved to the repository root. Reproduced before the fix: the
/// CLI listed the root's workspace, and a capsule it staged there was never
/// delivered to a session started in the package.
#[test]
fn claude_in_a_repository_subdirectory_and_the_cli_there_agree() {
    let env = Env::new();
    let (root, pkg) = repo(&env);
    record_session(&env, &root, "s-root", "work at the root");
    record_session(&env, &pkg, "s-pkg", "fix the package's parser");
    let root_id = recorded_id(&env, "s-root");
    let pkg_id = recorded_id(&env, "s-pkg");
    assert_ne!(
        root_id, pkg_id,
        "two workspaces, as Claude Code started them"
    );

    for cwd in [pkg.clone(), pkg.join("src/deep")] {
        let listed = cli_json(&env, &cwd, &["restore", "--list", "--json"]);
        assert_eq!(listed["workspace_id"], pkg_id, "from {}", cwd.display());
    }
    // The root is still the root's.
    let listed = cli_json(&env, &root, &["restore", "--list", "--json"]);
    assert_eq!(listed["workspace_id"], root_id);

    // Round trip: staged from the package's terminal, delivered to the next
    // session Claude Code starts in the package, and only there.
    let staged = cli_json(
        &env,
        &pkg.join("src/deep"),
        &["restore", "--session", "s-pkg", "--json"],
    );
    assert_eq!(staged["staged"], true);
    assert_eq!(staged["workspace_id"], pkg_id);
    assert!(session_start(&env, &root, "new-at-root").is_empty());
    let out = session_start(&env, &pkg, "new-in-pkg");
    assert!(delivered_context(&out).contains("fix the package"), "{out}");
}

/// With `CLAUDE_PROJECT_DIR` in the CLI's environment -- `velra restore` run
/// by the agent through Claude Code's shell -- the variable decides, exactly
/// as in the hook.
#[test]
fn a_cli_given_the_variable_uses_it() {
    let env = Env::new();
    let (root, pkg) = repo(&env);
    record_session(&env, &pkg, "s-pkg", "fix the package");
    let pkg_id = recorded_id(&env, "s-pkg");
    let out = env
        .cmd()
        .env("CLAUDE_PROJECT_DIR", &pkg)
        .current_dir(&root)
        .args(["restore", "--list", "--json"])
        .output()
        .expect("run velra");
    let v: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert_eq!(v["workspace_id"], pkg_id);
}

/// Outside a repository nothing bounds a search upwards: a terminal in a
/// subdirectory of a plain directory Claude Code was started in resolves to
/// the subdirectory, and says so rather than staging somewhere nothing reads.
#[test]
fn outside_a_repository_a_subdirectory_says_which_workspace_it_is() {
    let env = Env::new();
    let plain = env.project.clone();
    let sub = plain.join("src");
    std::fs::create_dir_all(&sub).expect("dirs");
    record_session(&env, &plain, "s-plain", "tidy the scripts");
    let plain_id = recorded_id(&env, "s-plain");

    let at_root = cli_json(&env, &plain, &["restore", "--list", "--json"]);
    assert_eq!(at_root["workspace_id"], plain_id);

    let below = cli_json(&env, &sub, &["restore", "--list", "--json"]);
    assert_ne!(below["workspace_id"], plain_id);
    let out = cli(&env, &sub, &["restore", "--list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let sub_root = below["workspace_root"].as_str().expect("root");
    assert!(
        text.contains("No previous sessions") && text.contains(sub_root),
        "{text}"
    );
}

/// `velra status` and `velra inspect` resolve the workspace the way
/// `restore` does: there is one CLI resolver, not one per command.
#[test]
fn status_and_inspect_use_the_same_workspace_as_restore() {
    let env = Env::new();
    let (root, pkg) = repo(&env);
    record_session(&env, &root, "s-root", "fix the tokenizer in the root crate");
    record_session(&env, &pkg, "s-pkg", "fix the parser in the foo package");
    let staged = cli_json(&env, &pkg, &["restore", "--session", "s-pkg", "--json"]);
    assert_eq!(staged["staged"], true);

    let status = cli(&env, &pkg, &["status"]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("Staged:"), "{text}");
    let pkg_root = staged["workspace_root"].as_str().expect("root");
    assert!(text.contains(pkg_root), "{text}");
    // From the root, the package's capsule is not this workspace's.
    let status = cli(&env, &root, &["status"]);
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("Staged:"),
        "{status:?}"
    );

    let inspect = cli(&env, &pkg, &["inspect"]);
    assert!(
        String::from_utf8_lossy(&inspect.stdout).contains("fix the parser in the foo package"),
        "{inspect:?}"
    );
}

// --------------------------------------------------- 6E cross-workspace

/// Two workspaces with distinct ids, one capsule staged in the first. A
/// session starting in the second -- even one reusing the source session's
/// id -- receives nothing, and the first's capsule is untouched.
#[test]
fn a_capsule_staged_in_one_workspace_is_never_delivered_to_another() {
    let env = Env::new();
    let a = env.dir.path().join("ws-a");
    let b = env.dir.path().join("ws-b");
    for d in [&a, &b] {
        std::fs::create_dir_all(d).expect("dir");
    }
    record_session(&env, &a, "shared-id", "fix the workspace a importer");
    let a_id = recorded_id(&env, "shared-id");
    let staged = cli_json(&env, &a, &["restore", "--session", "shared-id", "--json"]);
    assert_eq!(staged["workspace_id"], a_id);

    // B: a new session, then one reusing A's session id.
    assert!(session_start(&env, &b, "new-in-b").is_empty());
    assert!(session_start(&env, &b, "shared-id").is_empty());
    let a_dir = staging::staged_dir(&env.home, &a_id);
    assert_eq!(staging::records(&a_dir).len(), 1, "A's capsule untouched");

    // A's own next session still gets it.
    let out = session_start(&env, &a, "new-in-a");
    assert!(
        delivered_context(&out).contains("fix the workspace a importer"),
        "{out}"
    );
}

/// A record of workspace A that lands in workspace B's directory -- copied,
/// or a stale file at an overlapping path -- names A, and B refuses it.
#[test]
fn a_record_of_another_workspace_in_this_ones_directory_is_refused() {
    let env = Env::new();
    let a = env.dir.path().join("ws-a");
    let b = env.dir.path().join("ws-b");
    for d in [&a, &b] {
        std::fs::create_dir_all(d).expect("dir");
    }
    record_session(&env, &a, "s-a", "fix the workspace a importer");
    record_session(&env, &b, "s-b", "fix the workspace b exporter");
    let (a_id, b_id) = (recorded_id(&env, "s-a"), recorded_id(&env, "s-b"));
    let staged = cli_json(&env, &a, &["restore", "--session", "s-a", "--json"]);
    let a_record = PathBuf::from(staged["staged_path"].as_str().expect("path"));
    let b_dir = staging::staged_dir(&env.home, &b_id);
    std::fs::create_dir_all(&b_dir).expect("b dir");
    let copied = b_dir.join(a_record.file_name().expect("name"));
    std::fs::copy(&a_record, &copied).expect("copy");

    assert!(session_start(&env, &b, "new-in-b").is_empty());
    assert!(copied.is_file(), "kept as evidence, not consumed");
    assert_eq!(
        staging::claim(&env.home, &b_id, STARTUP, velra_core::time::now_ms()),
        Err(staging::ClaimError::WrongWorkspace { staged_for: a_id })
    );
}

/// `SessionStart` reads exactly its own workspace's directory: with a capsule
/// staged for the root and another for the package, each session gets its
/// own.
#[test]
fn session_start_claims_from_exactly_its_own_workspace() {
    let env = Env::new();
    let (root, pkg) = repo(&env);
    record_session(&env, &root, "s-root", "fix the tokenizer in the root crate");
    record_session(&env, &pkg, "s-pkg", "fix the parser in the foo package");
    cli_json(&env, &root, &["restore", "--session", "s-root", "--json"]);
    cli_json(&env, &pkg, &["restore", "--session", "s-pkg", "--json"]);

    let in_pkg = delivered_context(&session_start(&env, &pkg, "n1"));
    assert!(
        in_pkg.contains("fix the parser in the foo package"),
        "{in_pkg}"
    );
    let at_root = delivered_context(&session_start(&env, &root, "n2"));
    assert!(
        at_root.contains("fix the tokenizer in the root crate"),
        "{at_root}"
    );
}
