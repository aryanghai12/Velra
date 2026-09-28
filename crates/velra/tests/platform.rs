//! Platform behaviour through the real binary (Phase 10 of the v0.1.2
//! hardening audit): what a Windows machine, its shells and its file system
//! do that a plain Linux temp directory never shows.
//!
//! * **Executable lookup.** `velra enable`, `status` and `doctor` ask
//!   `claude --version`. That lookup must not reach the current directory --
//!   the repository `velra` is run from.
//! * **Paths.** One directory spelled several ways (an 8.3 short name,
//!   forward slashes, a lower-case drive letter, a trailing separator, a `.`
//!   segment) is one workspace to the hook and to the CLI; spaces and
//!   non-ASCII names change nothing; a path next to the project whose name
//!   is not ASCII is recorded, not a panic.
//! * **The settings file.** A byte order mark is kept and does not stop the
//!   edit; a file Windows will not let us replace -- read-only, or held open
//!   by another program -- is left exactly as it was, and says why.

mod common;

use common::Env;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn status_json(out: &std::process::Output) -> Value {
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .unwrap_or_else(|e| panic!("status --json: {e}: {out:?}"))
}

/// A script that records that it ran (a file named `ran-<tag>` next to it)
/// and prints `version`. `name` is without extension; on Windows it is a
/// `.cmd`. The marker's path is written into the script, so recording it
/// needs no program from `PATH` -- the tests run it with a `PATH` that has
/// none (`dirname` was not found, and a script that ran left no trace).
fn fake_claude(dir: &Path, name: &str, tag: &str, version: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let marker = dir.join(format!("ran-{tag}"));
    if cfg!(windows) {
        let p = dir.join(format!("{name}.cmd"));
        let body = format!(
            "@echo off\r\necho ran> \"{}\"\r\necho {version}\r\n",
            marker.display()
        );
        std::fs::write(&p, body).unwrap();
        p
    } else {
        let p = dir.join(name);
        let body = format!(
            "#!/bin/sh\necho ran > '{}'\necho '{version}'\n",
            marker.display()
        );
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }
}

// ---------------------------------------------------------- executable lookup

/// Reproduced before the fix: `velra status` run in a directory holding a
/// `claude.cmd` ran it (`cmd /C claude` searches the current directory before
/// `PATH`) and reported the version it printed; `enable` chooses the hooks it
/// writes from that version. A relative or empty `PATH` entry means the
/// current directory too, on every platform.
#[test]
fn a_claude_in_the_working_directory_is_never_run() {
    let env = Env::new();
    fake_claude(&env.project, "claude", "planted", "9.9.9 (Claude Code)");
    let real_bin = env.dir.path().join("real bin");
    fake_claude(&real_bin, "claude", "real", "2.1.300 (Claude Code)");
    let empty_home = env.dir.path().join("user");
    std::fs::create_dir_all(&empty_home).unwrap();

    let run = |path_entries: Vec<PathBuf>| {
        let path = std::env::join_paths(path_entries).unwrap();
        env.cmd()
            .env_remove("VELRA_CLAUDE_VERSION")
            // Set by some hosts (Claude Code among them); a user's terminal
            // does not have it, and with it `cmd` would not search `.`.
            .env_remove("NoDefaultCurrentDirectoryInExePath")
            .env("PATH", path)
            .env("HOME", &empty_home)
            .env("USERPROFILE", &empty_home)
            .args(["status", "--json"])
            .output()
            .expect("run velra")
    };

    // Nothing on PATH but the current directory, spelled every way.
    let out = run(vec![
        PathBuf::new(),
        PathBuf::from("."),
        env.dir.path().join("none"),
    ]);
    assert!(
        !env.project.join("ran-planted").exists(),
        "a claude in the working directory was run"
    );
    let v = status_json(&out);
    let claude = v["claude_code"].as_str().unwrap_or_default();
    assert!(!claude.contains("9.9.9"), "{claude}");
    assert!(claude.starts_with("not detected"), "{claude}");

    // A claude on an absolute PATH entry is found and asked, `.cmd` included.
    let out = run(vec![PathBuf::from("."), real_bin.clone()]);
    assert_eq!(status_json(&out)["claude_code"], "2.1.300");
    let listing: Vec<_> = std::fs::read_dir(&real_bin)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert!(real_bin.join("ran-real").exists(), "{listing:?}");
    assert!(!env.project.join("ran-planted").exists());
}

// ---------------------------------------------------------------------- paths

/// The short (8.3) form of `p`, when the volume keeps short names.
#[cfg(windows)]
fn short_name(p: &Path) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .raw_arg(format!(
            "/d /c chcp 65001>nul & for %I in (\"{}\") do @echo %~sI",
            p.display()
        ))
        .output()
        .ok()?;
    let short = String::from_utf8(out.stdout).ok()?;
    let short = PathBuf::from(short.trim());
    (short.is_dir() && short != p).then_some(short)
}

/// Every spelling of `dir` this platform accepts for the same directory.
fn spellings(dir: &Path) -> Vec<PathBuf> {
    let s = dir.to_string_lossy().into_owned();
    let mut out = vec![
        dir.to_path_buf(),
        PathBuf::from(format!("{s}{}", std::path::MAIN_SEPARATOR)),
    ];
    let parent = dir.parent().unwrap();
    let name = dir.file_name().unwrap();
    out.push(parent.join(".").join(name));
    #[cfg(windows)]
    {
        out.push(PathBuf::from(s.replace('\\', "/")));
        let b = s.as_bytes();
        if b.len() > 1 && b[1] == b':' {
            let lower = format!("{}{}", s[..1].to_ascii_lowercase(), &s[1..]);
            out.push(PathBuf::from(lower));
        }
        match short_name(dir) {
            Some(short) => out.push(short),
            None => eprintln!("note: no 8.3 short name for {s}; that spelling is not exercised"),
        }
        // A junction: a directory alias any user can make (`mklink /J`).
        let link = parent.join("alias-junction");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(dir)
            .output()
            .expect("run mklink");
        assert!(made.status.success(), "mklink /J: {made:?}");
        out.push(link);
    }
    #[cfg(unix)]
    {
        let link = parent.join("alias-link");
        std::os::unix::fs::symlink(dir, &link).unwrap();
        out.push(link);
    }
    out
}

/// One directory with spaces and non-ASCII characters in its name, reached by
/// every spelling the platform has, is one workspace: the hook records every
/// session under the same id, and the CLI run from any spelling lists them
/// all under it.
#[test]
fn every_spelling_of_a_directory_is_one_workspace() {
    let env = Env::new();
    let project = env.dir.path().join("Long Project Dir").join("proj é 日本");
    std::fs::create_dir_all(project.join(".git/refs/heads")).unwrap();
    std::fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let canonical_id = velra_core::workspace::resolve(Some(&project.to_string_lossy())).0;

    let spelled = spellings(&project);
    eprintln!("spellings: {spelled:#?}");
    for (i, dir) in spelled.iter().enumerate() {
        let session = format!("s-{i}");
        let base = json!({
            "session_id": session,
            "cwd": dir.to_string_lossy(),
            "transcript_path": env.dir.path().join(format!("{session}.jsonl")).to_string_lossy(),
        });
        let mut start = base.clone();
        start["hook_event_name"] = json!("SessionStart");
        start["source"] = json!("startup");
        let mut submit = base;
        submit["hook_event_name"] = json!("UserPromptSubmit");
        submit["prompt"] = json!(format!("fix the importer, attempt {i}"));
        for (event, payload) in [("session-start", start), ("user-prompt-submit", submit)] {
            let out = env
                .cmd()
                .env("CLAUDE_PROJECT_DIR", dir)
                .current_dir(dir)
                .args(["hook", event])
                .write_stdin(payload.to_string())
                .output()
                .expect("run hook");
            assert_eq!(out.status.code(), Some(0), "{}", dir.display());
            assert!(out.stderr.is_empty(), "{}: {:?}", dir.display(), out.stderr);
        }
    }
    let ids: Vec<(String, String)> = env.drain_and_query(
        |rows: &Vec<(String, String)>| rows.len() >= spelled.len(),
        |db| {
            db.conn
                .prepare("SELECT session_id, project_id FROM sessions ORDER BY session_id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        },
    );
    assert_eq!(ids.len(), spelled.len(), "{ids:?}");
    for (session, id) in &ids {
        assert_eq!(id, &canonical_id, "{session}: spellings {spelled:#?}");
    }

    for dir in &spelled {
        let out = env
            .cmd()
            .env_remove("CLAUDE_PROJECT_DIR")
            .current_dir(dir)
            .args(["restore", "--list", "--json"])
            .output()
            .expect("run velra");
        let v: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
            .unwrap_or_else(|e| panic!("{}: {e}: {out:?}", dir.display()));
        assert_eq!(
            v["workspace_id"],
            canonical_id.as_str(),
            "from {}",
            dir.display()
        );
        assert_eq!(
            v["sessions"].as_array().map(Vec::len),
            Some(spelled.len()),
            "from {}",
            dir.display()
        );
    }

    // A different directory whose name differs only past the first byte of a
    // multi-byte character is a different workspace.
    let other = env.dir.path().join("Long Project Dir").join("proj é 日");
    std::fs::create_dir_all(&other).unwrap();
    assert_ne!(
        velra_core::workspace::resolve(Some(&other.to_string_lossy())).0,
        canonical_id
    );
}

/// Reproduced before the fix: the prefix test that decides whether a path is
/// inside the project cut the path at the root's length in bytes. For a
/// sibling whose name puts a multi-byte character across that length
/// (`…/project` against `…/日本語/x.rs`) the cut fell inside the character,
/// the hook panicked, and -- failing open -- recorded nothing.
#[test]
fn a_file_beside_the_project_with_a_non_ascii_name_is_recorded() {
    let env = Env::new();
    let sibling = env.dir.path().join("日本語").join("x.rs");
    std::fs::create_dir_all(sibling.parent().unwrap()).unwrap();
    std::fs::write(&sibling, "fn main() {}\n").unwrap();
    let inside = env.write_file("src/ü ber/日本.rs", "fn f() {}\n");

    for (i, path) in [&sibling, &inside].into_iter().enumerate() {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!(format!("toolu_{i}"));
        p["tool_input"] = json!({ "file_path": path.to_string_lossy() });
        p["tool_response"] = json!({ "ok": true });
        env.hook("post-tool-use", &p).assert_contract();
    }
    let rows = env.assert_event_count(2);
    let payloads: Vec<String> = rows.iter().map(|r| r.payload.clone()).collect();
    assert!(
        payloads[0].contains("日本語/x.rs"),
        "the sibling is recorded by its absolute path: {payloads:?}"
    );
    assert!(
        payloads[1].contains("\"src/ü ber/日本.rs\""),
        "a file inside is recorded relative to the project: {payloads:?}"
    );
}

// ------------------------------------------------------------- settings file

/// Reproduced before the fix: `enable` refused a settings file that starts
/// with a UTF-8 byte order mark -- "Could not parse … at line 1, column 1:
/// Unexpected token" -- and `status` and `doctor` reported its hooks as
/// missing. Now the mark is kept, every other byte the user wrote is kept,
/// and `disable` puts the file back as it was.
#[test]
fn a_settings_file_with_a_byte_order_mark_keeps_it_through_enable_and_disable() {
    let env = Env::new();
    let original =
        "\u{feff}{\r\n  // mine\r\n  \"model\": \"opus\",\r\n  \"env\": { \"A\": \"1\" }\r\n}\r\n";
    std::fs::write(env.settings_path(), original).unwrap();
    let velra = |args: &[&str]| {
        env.cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.280")
            .args(args)
            .output()
            .expect("run velra")
    };

    let out = velra(&["enable"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let enabled = std::fs::read_to_string(env.settings_path()).unwrap();
    assert!(enabled.starts_with('\u{feff}'), "the mark is kept");
    assert_eq!(enabled.matches('\u{feff}').count(), 1, "and not doubled");
    for kept in [
        "// mine",
        "\"model\": \"opus\"",
        "\"env\": { \"A\": \"1\" }",
        "\r\n",
    ] {
        assert!(enabled.contains(kept), "{kept:?} lost: {enabled}");
    }
    assert!(enabled.contains("\"hooks\""));

    let status = status_json(&velra(&["status", "--json"]));
    assert_eq!(status["enabled"], true, "{status}");
    assert!(status["handlers"].as_u64().unwrap_or(0) > 0, "{status}");
    let doctor = velra(&["doctor"]);
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(!text.contains("parse failed"), "{text}");
    assert!(!text.contains("no Velra hooks registered"), "{text}");

    let out = velra(&["disable"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(env.settings_path()).unwrap(),
        original
    );
}

/// Names in the settings directory other than the settings file: temp files
/// an update left behind.
#[cfg(windows)]
fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "settings.json")
        .collect()
}

/// Reproduced before the fix: a read-only settings file failed with a bare
/// "Access is denied. (os error 5)" that named neither the file nor why.
#[cfg(windows)]
#[test]
fn a_read_only_settings_file_is_left_as_it_was_and_named() {
    let env = Env::new();
    let original = "{\n  \"model\": \"opus\"\n}\n";
    std::fs::write(env.settings_path(), original).unwrap();
    let writable = std::fs::metadata(env.settings_path())
        .unwrap()
        .permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(env.settings_path(), read_only).unwrap();

    // The first start of a freshly built binary can be held for seconds by a
    // virus scanner; that is not what is timed here.
    env.cmd().arg("--version").output().expect("warm up");
    let started = std::time::Instant::now();
    let out = env
        .cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.280")
        .arg("enable")
        .output()
        .expect("run velra");
    let elapsed = started.elapsed();
    std::fs::set_permissions(env.settings_path(), writable).unwrap();

    assert_eq!(out.status.code(), Some(1));
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains("settings.json"), "{said}");
    assert!(said.contains("read-only"), "{said}");
    assert!(said.contains("unchanged"), "{said}");
    assert_eq!(
        std::fs::read_to_string(env.settings_path()).unwrap(),
        original
    );
    assert!(
        leftovers(&env.config).is_empty(),
        "{:?}",
        leftovers(&env.config)
    );
    // Read-only does not change by waiting, so it is not retried.
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "{elapsed:?}"
    );
}

/// Opens `path` the way an editor or a scanner does that blocks replacement:
/// shared for reading only, not for deletion.
#[cfg(windows)]
fn hold_open(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .expect("hold the settings file open")
}

/// Windows refuses to replace a file another program holds open without
/// delete sharing. A holder that lets go within the retry window no longer
/// fails the edit; one that does not leaves the file as it was, and the
/// message says another program has it open.
#[cfg(windows)]
#[test]
fn a_settings_file_held_open_by_another_program() {
    let env = Env::new();
    let original = "{\n  \"model\": \"opus\"\n}\n";
    let enable = || {
        env.cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.280")
            .arg("enable")
            .output()
            .expect("run velra")
    };

    // Held for the whole run.
    std::fs::write(env.settings_path(), original).unwrap();
    let held = hold_open(&env.settings_path());
    let out = enable();
    drop(held);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let said =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("another program has it open"), "{said}");
    assert!(said.contains("unchanged"), "{said}");
    assert_eq!(
        std::fs::read_to_string(env.settings_path()).unwrap(),
        original
    );
    assert!(
        leftovers(&env.config).is_empty(),
        "{:?}",
        leftovers(&env.config)
    );

    // Held briefly, as a scanner does.
    let path = env.settings_path();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let held = hold_open(&path);
        tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(held);
    });
    rx.recv().unwrap();
    let out = enable();
    holder.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let after = std::fs::read_to_string(env.settings_path()).unwrap();
    assert!(
        after.contains("\"hooks\"") && after.contains("\"model\": \"opus\""),
        "{after}"
    );
    assert!(
        leftovers(&env.config).is_empty(),
        "{:?}",
        leftovers(&env.config)
    );
}

// ------------------------------------------------------------- hook stdin

/// What a shell pipes into the hook is not always what Claude Code sends: a
/// payload file written by a Windows tool starts with a UTF-8 byte order mark
/// (`type payload.json | velra hook ...`), and a line from any Windows shell
/// ends in CRLF. Reproduced before the fix: with the mark, the hook exited 0
/// as it must -- and recorded nothing.
#[test]
fn a_payload_with_a_byte_order_mark_or_crlf_is_recorded() {
    let env = Env::new();
    for (i, framing) in ["bom", "crlf", "bom+crlf"].iter().enumerate() {
        let mut p = env.base_payload("UserPromptSubmit");
        p["session_id"] = json!(format!("s-{i}"));
        p["prompt"] = json!(format!("fix the parser, case {framing} ü 日本"));
        let mut raw = Vec::new();
        if framing.starts_with("bom") {
            raw.extend_from_slice(b"\xef\xbb\xbf");
        }
        raw.extend_from_slice(p.to_string().as_bytes());
        if framing.ends_with("crlf") {
            raw.extend_from_slice(b"\r\n");
        }
        env.hook_raw("user-prompt-submit", &raw).assert_contract();
    }
    let rows = env.assert_event_count(3);
    for (row, framing) in rows.iter().zip(["bom", "crlf", "bom+crlf"]) {
        assert_eq!(
            row.json()["prompt"],
            format!("fix the parser, case {framing} ü 日本"),
            "{row:?}"
        );
    }
}

// ------------------------------------------------------------ --json output

/// `--json` documents are ASCII, with every other character escaped, and
/// mean the same. Reproduced before: PowerShell (7.6 and 5.1) decodes a
/// program's output with the console code page, and `velra restore --list
/// --json | ConvertFrom-Json` gave a `workspace_root` that was not the
/// directory for a project named `proj é 日本`.
#[test]
fn json_output_is_ascii_and_keeps_non_ascii_values() {
    let env = Env::new();
    let project = env.dir.path().join("proj é 日本");
    std::fs::create_dir_all(&project).unwrap();
    let mut start = env.base_payload("SessionStart");
    start["cwd"] = json!(project.to_string_lossy());
    start["source"] = json!("startup");
    let out = env
        .cmd()
        .env("CLAUDE_PROJECT_DIR", &project)
        .current_dir(&project)
        .args(["hook", "session-start"])
        .write_stdin(start.to_string())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));

    // A document, a status, and an error naming a session that does not
    // exist -- by a non-ASCII id.
    let documents: [&[&str]; 3] = [
        &["restore", "--list", "--json"],
        &["status", "--json"],
        &["inspect", "--session", "sé-日本", "--json"],
    ];
    for args in documents {
        let out = env
            .cmd()
            .env_remove("CLAUDE_PROJECT_DIR")
            .env("VELRA_CLAUDE_VERSION", "2.1.280")
            .current_dir(&project)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.stdout.is_ascii(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let v: Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        match args[0] {
            "restore" => {
                let root = v["workspace_root"].as_str().unwrap();
                assert!(root.ends_with("/proj é 日本"), "{root}");
            }
            "inspect" => {
                let error = v["error"].as_str().unwrap_or_else(|| panic!("{v}"));
                assert!(error.contains("sé-日本"), "{error}");
            }
            _ => {}
        }
    }
}
