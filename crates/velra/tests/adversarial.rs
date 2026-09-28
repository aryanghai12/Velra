//! Adversarial, property and fault tests (Phase 11 of the v0.1.2 hardening
//! audit). Each test names the invariant it attacks and states the exact
//! durable state it expects afterwards.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use velra_core::db::{self, Db, Role};
use velra_core::event::NewEvent;
use velra_core::eventlog;

fn bare_event(key: &str, ts_ms: i64) -> NewEvent {
    NewEvent {
        dedupe_key: key.into(),
        session_id: "s".into(),
        project_id: "p".into(),
        agent_id: None,
        hook_event: "Stop".into(),
        tool_name: None,
        tool_use_id: None,
        ts_ms,
        payload: "{}".into(),
        project: None,
    }
}

fn rotated_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.contains(".corrupt-") && !n.ends_with("-wal") && !n.ends_with("-shm")
        })
        .collect();
    out.sort();
    out
}

/// Events a file holds, when it is a readable Velra database.
fn events_in(path: &Path) -> Option<i64> {
    let db = Db::open_readonly(path).ok()?;
    db.conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .ok()
}

/// Suspected in Phase 8: many processes meet the same corrupt database at
/// once. Each one that saw it corrupt renames whatever is at the path, so a
/// late one can rotate aside the fresh database an earlier one just created
/// and wrote to.
///
/// Invariant: exactly one file is rotated (the corrupt one), and every event
/// a writer was told was stored is in the live database.
///
/// On Linux, before D137, 13 of 26 rounds rotated twice and one left the
/// live file malformed. With the lock and re-probe alone, about one round
/// in sixty still lost an acknowledged event (two threads were each handed
/// row id 1): a connection opened on the old file just before the rename
/// paired itself with the *new* database's `-wal`, which SQLite finds by
/// name. The identity check in `Db::open_once_at` closes that; disabling it
/// brought back 12 bad rounds of 300. Windows refuses to rename an open
/// database, so there this tests the lock and the re-probe.
#[test]
fn concurrent_opens_of_a_corrupt_database_rotate_it_once_and_lose_nothing() {
    let failures = open_storm(true);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The same storm on a database that does not exist yet: the first session
/// after `velra enable`, every hook creating it at once. Nothing is rotated
/// and nothing a writer was told was stored is lost.
#[test]
fn concurrent_first_opens_of_a_missing_database_lose_nothing() {
    let failures = open_storm(false);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The production shape of the corrupt-rotation race: hook *processes*,
/// each with one connection, meeting one corrupt database at once, with
/// the real hook deadline. On Linux, with the committed code, 12 or more of
/// 150 rounds rotated a second file -- the fresh database a first hook had
/// just created. With the lock and re-probe alone, run four at a time, 32
/// of 240 rounds still did: a hook still holding the old file checkpointed
/// the new database's `-wal` into it on close and left the new file empty
/// (the second file rotated was 0 bytes; the first had become 4 KiB of
/// SQLite). With the identity check and checkpoint-on-close off until it
/// passes: none of 240.
///
/// Invariants, per round: exactly one file rotated aside, and every event
/// sent is in the live database once the spool is drained.
#[test]
fn hook_processes_meeting_a_corrupt_database_lose_nothing() {
    let rounds: usize = std::env::var("VELRA_ROTATION_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let procs = 8usize;
    let mut failures = Vec::new();
    for round in 0..rounds {
        let env = common::Env::new();
        env.write_file("src/a.rs", "x\n");
        std::fs::write(env.db_path(), vec![b'x'; 64 * 1024]).unwrap();
        let children: Vec<_> = (0..procs)
            .map(|p| {
                let mut payload = env.base_payload("PostToolUse");
                payload["tool_name"] = serde_json::json!("Read");
                payload["tool_use_id"] = serde_json::json!(format!("r{round}-p{p}"));
                payload["tool_input"] =
                    serde_json::json!({"file_path": env.project.join("src/a.rs")});
                #[allow(deprecated)]
                let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
                    .env("VELRA_HOME", &env.home)
                    .env("CLAUDE_CONFIG_DIR", &env.config)
                    .env("CLAUDE_PROJECT_DIR", &env.project)
                    .current_dir(&env.project)
                    .args(["hook", "post-tool-use"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                (child.stdin.take().unwrap(), child, payload.to_string())
            })
            .collect();
        // All started; now all handed their input at once.
        let mut waiting = Vec::new();
        for (mut stdin, child, payload) in children {
            use std::io::Write as _;
            stdin.write_all(payload.as_bytes()).unwrap();
            drop(stdin);
            waiting.push(child);
        }
        for child in waiting {
            let out = child.wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(0));
            assert!(out.stderr.is_empty());
        }
        let rotated = rotated_files(&env.home);
        let rows = env.drain_and_load_events(procs);
        if rotated.len() != 1 || rows.len() != procs {
            failures.push(format!(
                "round {round}: {} of {procs} events, {} rotated",
                rows.len(),
                rotated.len()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `VELRA_ROTATION_ROUNDS` rounds of eight threads opening one database
/// path at once and appending an event each; the failures, one per round.
fn open_storm(corrupt: bool) -> Vec<String> {
    let rounds: usize = std::env::var("VELRA_ROTATION_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let threads = 8;
    let mut failures = Vec::new();
    for round in 0..rounds {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("velra.db");
        if corrupt {
            std::fs::write(&path, vec![b'x'; 64 * 1024]).unwrap();
        }
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let Ok(mut db) = Db::open(&path, Role::Reduce) else {
                        return None;
                    };
                    let key = format!("r{round}-t{t}");
                    match eventlog::append(&mut db.conn, &bare_event(&key, t as i64)) {
                        Ok(Some(_)) => Some(key),
                        _ => None,
                    }
                })
            })
            .collect();
        let stored: Vec<String> = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();
        let rotated = rotated_files(dir.path());
        let live = match Db::open_readonly(&path).and_then(|db| {
            db.conn
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .map_err(db::DbError::from)?;
            Ok(db)
        }) {
            Ok(db) => db,
            Err(e) => {
                failures.push(format!(
                    "round {round}: the live database is unreadable after the round: {e} \
                     ({} stored, {} rotated)",
                    stored.len(),
                    rotated.len()
                ));
                continue;
            }
        };
        let mut missing = Vec::new();
        for key in &stored {
            let n: i64 = live
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE dedupe_key = ?1",
                    [key],
                    |r| r.get(0),
                )
                .unwrap();
            if n != 1 {
                missing.push(key.clone());
            }
        }
        let healthy_rotated: Vec<(String, i64)> = rotated
            .iter()
            .filter_map(|p| events_in(p).map(|n| (p.display().to_string(), n)))
            .collect();
        if std::env::var_os("VELRA_ROTATION_TRACE").is_some() {
            eprintln!(
                "round {round}: stored {} of {threads}, rotated {}",
                stored.len(),
                rotated.len()
            );
        }
        let expected_rotations = usize::from(corrupt);
        if rotated.len() != expected_rotations || !missing.is_empty() || !healthy_rotated.is_empty()
        {
            failures.push(format!(
                "round {round}: {} stored, {} rotated, missing {missing:?}, healthy databases rotated aside {healthy_rotated:?}",
                stored.len(),
                rotated.len()
            ));
        }
        drop(live);
    }
    failures
}

// ------------------------------------------------- process / child lifetime

/// `velra status` in `env`, with a `PATH` holding only `bin` and no pinned
/// Claude Code version, so detection runs the `claude` found there.
#[cfg(windows)]
fn status_with_claude_in(env: &common::Env, bin: &Path) -> std::process::Command {
    let home = env.dir.path().join("user");
    std::fs::create_dir_all(&home).unwrap();
    #[allow(deprecated)]
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"));
    cmd.env("VELRA_HOME", &env.home)
        .env("CLAUDE_CONFIG_DIR", &env.config)
        .env("CLAUDE_PROJECT_DIR", &env.project)
        .env_remove("VELRA_CLAUDE_VERSION")
        .env("PATH", bin)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(&env.project)
        .args(["status", "--json"]);
    cmd
}

/// A `claude.cmd` that starts a grandchild (which leaves `marker` after
/// about `grandchild_s` seconds), then runs for `shim_s` seconds before it
/// prints a version -- an npm shim whose `node` hangs.
#[cfg(windows)]
fn hanging_shim(bin: &Path, marker: &Path, grandchild_s: u32, shim_s: u32) {
    std::fs::create_dir_all(bin).unwrap();
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let cmd = format!(r"{sys}\System32\cmd.exe");
    let ping = format!(r"{sys}\System32\PING.EXE");
    let body = format!(
        "@echo off\r\n\
         start \"\" /b \"{cmd}\" /c \"\"{ping}\" -n {g} 127.0.0.1 >nul & echo late> \"{m}\"\"\r\n\
         \"{ping}\" -n {s} 127.0.0.1 >nul\r\n\
         echo 2.1.300 (Claude Code)\r\n",
        g = grandchild_s + 1,
        s = shim_s + 1,
        m = marker.display()
    );
    std::fs::write(bin.join("claude.cmd"), body).unwrap();
}

/// Phase 10 left this open: `claude --version` through an npm `.cmd` shim
/// is `cmd.exe` running `node`, and killing `cmd.exe` at the 3 s deadline
/// does not reach `node`.
///
/// Measured, with a shim that starts a grandchild and then hangs for 30 s:
/// `velra status` itself exits on time, but the program reading its output
/// waited for the shim's descendants -- they held `velra`'s own stdout,
/// which Windows had handed down to them -- and the grandchild ran on.
#[cfg(windows)]
#[test]
fn a_hanging_shim_neither_holds_velras_output_nor_outlives_it() {
    use std::time::{Duration, Instant};
    let env = common::Env::new();
    let bin = env.dir.path().join("bin");
    let marker = bin.join("grandchild-ran");
    hanging_shim(&bin, &marker, 6, 30);

    // The process: exits near the deadline.
    let started = Instant::now();
    let status = status_with_claude_in(&env, &bin)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run velra");
    let exited = started.elapsed();
    eprintln!("velra status exited after {exited:?}");
    assert!(status.code().is_some(), "{status:?}");
    // Measured alone: 3.3 s; 5.9-6.1 s on the first launch of a freshly
    // built binary, which Windows scans, or beside the stress tests here.
    // The bound is generous for that; what it tells apart is the 30 s the
    // shim's descendants ran for before D138.
    let bound = Duration::from_secs(15);
    assert!(exited < bound, "velra ran for {exited:?}");

    // Its output: ends when it exits, not when the shim's descendants do.
    let _ = std::fs::remove_file(&marker);
    let started = Instant::now();
    let out = status_with_claude_in(&env, &bin)
        .output()
        .expect("run velra");
    let output_ended = started.elapsed();
    eprintln!("its output ended after {output_ended:?}");
    // `status` exits 1 when Velra is not enabled; the document is what counts.
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {out:?}"));
    assert!(
        doc["claude_code"]
            .as_str()
            .is_some_and(|c| c.starts_with("not detected")),
        "{doc}"
    );
    assert!(
        output_ended < bound,
        "velra's output stayed open for {output_ended:?} (the process exited after {exited:?})"
    );

    // The grandchild would leave its marker ~6 s after it started.
    std::thread::sleep(Duration::from_secs(9));
    assert!(
        !marker.exists(),
        "the shim's grandchild outlived `velra status`"
    );
}

// ----------------------------------------------------------- redaction cuts

/// Every file under `dir`, recursively, with its bytes.
fn files_under(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            files_under(&p, out);
        } else if let Ok(bytes) = std::fs::read(&p) {
            out.push((p, bytes));
        }
    }
}

/// The files under `dir` holding any of `needles`, with the needles found.
fn leaks(dir: &Path, needles: &[&str]) -> Vec<(PathBuf, Vec<String>)> {
    let mut files = Vec::new();
    files_under(dir, &mut files);
    files
        .into_iter()
        .filter_map(|(p, bytes)| {
            let text = String::from_utf8_lossy(&bytes);
            let found: Vec<String> = needles
                .iter()
                .filter(|n| text.contains(**n))
                .map(|n| n.to_string())
                .collect();
            (!found.is_empty()).then_some((p, found))
        })
        .collect()
}

/// A secret that straddles a length cap. Two fields were cut to size
/// *before* they were redacted: an edit's excerpt line (160 characters) and
/// a search pattern (200). A token cut short matches no detector, so the
/// part of it that fit was stored as it was.
///
/// Invariant: whatever is kept of a field was redacted as a whole.
#[test]
fn a_secret_cut_by_a_length_cap_leaves_no_fragment() {
    let env = common::Env::new();
    let vars = [("VELRA_LOG", "debug")];
    env.write_file("src/app.py", "value = 1\n");
    let file = env
        .project
        .join("src/app.py")
        .to_string_lossy()
        .into_owned();

    // The excerpt: the token starts 20 characters before the 160th.
    let line = format!(
        "{} ghp_CanaryExcerptCut01abcdefghijklmnopqrstuv",
        "x".repeat(139)
    );
    let edit = serde_json::json!({
        "file_path": file,
        "old_string": "value = 1",
        "new_string": line,
    });
    for (event, sub) in [
        ("PreToolUse", "pre-tool-use"),
        ("PostToolUse", "post-tool-use"),
    ] {
        if event == "PostToolUse" {
            env.write_file("src/app.py", &format!("{line}\n"));
        }
        let mut p = env.base_payload(event);
        p["tool_name"] = serde_json::json!("Edit");
        p["tool_use_id"] = serde_json::json!("e1");
        p["tool_input"] = edit.clone();
        p["tool_response"] = serde_json::json!({"filePath": file, "originalFile": "value = 1\n"});
        env.hook_with_env(sub, &p, &vars).assert_contract();
    }

    // The pattern: an API key 20 characters before the 200th.
    let pattern = format!(
        "{} sk-ant-CanaryGrepCut01abcdefghijklmnopqrstuv",
        "y".repeat(179)
    );
    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = serde_json::json!("Grep");
    p["tool_use_id"] = serde_json::json!("g1");
    p["tool_input"] = serde_json::json!({"pattern": pattern, "path": "src"});
    p["tool_response"] = serde_json::json!({"numFiles": 0, "filenames": []});
    env.hook_with_env("post-tool-use", &p, &vars)
        .assert_contract();

    env.reduce();
    let rows = env.drain_and_load_events(3);
    assert!(rows.len() >= 3, "{rows:?}");
    let found = leaks(&env.home, &["CanaryExcerpt", "CanaryGrep"]);
    assert!(found.is_empty(), "secret fragments persisted: {found:?}");
    // And the fields still say what they said, up to the secret.
    let stored: Vec<String> = rows.iter().map(|r| r.payload.clone()).collect();
    // The marker itself may be what the cap cuts.
    let marked = |field: &str| {
        rows.iter()
            .filter_map(|r| r.json()[field].as_str().map(str::to_string))
            .any(|v| v.contains("[REDACTED:"))
    };
    assert!(marked("excerpt"), "{stored:?}");
    assert!(marked("pattern"), "{stored:?}");
}

/// A token whose prefix follows a letter or digit with no break: URL
/// encoding (`token%3Dghp_…`, the `D` of `%3D`), a JSON escape
/// (`=AKIA…`), a literal `\n` in escaped output. The detectors began
/// with `\b`, which needs a non-word character before the prefix, so none of
/// these was redacted at all -- cut or not.
///
/// Invariant: the credential's body is gone, and the text around it stays.
#[test]
fn a_token_glued_to_an_escape_or_percent_encoding_is_redacted() {
    use velra_core::redact::redact;
    let cases = [
        (
            "callback?next=%2Fhome%26token%3Dghp_CanaryGlued01abcdefghijklmnopqrstuvwx",
            "CanaryGlued01",
        ),
        (
            "{\"env\":\"GH_TOKEN\\u003dghp_CanaryGlued02abcdefghijklmnopqrstuvwx\"}",
            "CanaryGlued02",
        ),
        (
            "\"log\": \"auth ok\\nghp_CanaryGlued03abcdefghijklmnopqrstuvwx\\n\"",
            "CanaryGlued03",
        ),
        (
            "AWSAccessKeyId%3DAKIACANARYGLUED04XYZ%26Expires",
            "CANARYGLUED04",
        ),
        ("hook%3Dxoxb-CanaryGlued05-abcdefghij", "CanaryGlued05"),
        (
            "maps%3Fkey%3DAIzaCanaryGlued06abcdefghijklmnopqrstuvw",
            "CanaryGlued06",
        ),
        (
            "Authorization%3A%20Bearer%20eyJCanaryGlued07.eyJabcdefghij.sigabcdefghij",
            "CanaryGlued07",
        ),
        ("stripe%3Dsk_live_CanaryGlued08abcdefgh", "CanaryGlued08"),
        (
            "key%3Dsk-ant-CanaryGlued09abcdefghijklmnop",
            "CanaryGlued09",
        ),
        // Found by `redaction_props`: no space before a token in Japanese
        // or Chinese text; a flag in quotes or a JSON argument list; a URL
        // after an escape; a key followed by a letter.
        (
            "トークンはghp_CanaryGlued10abcdefghijklmnopqrstuvwxです",
            "CanaryGlued10",
        ),
        ("令牌AKIACANARYGLUED11XYZ已泄露", "CANARYGLUED11"),
        ("& tool \"--token\" \"CanaryGlued12abc\"", "CanaryGlued12"),
        (
            "\"args\": [\"--api-key\", \"CanaryGlued13abc\"]",
            "CanaryGlued13",
        ),
        (
            "\"cmd\": \"tool\\n--password CanaryGlued14abc\"",
            "CanaryGlued14",
        ),
        (
            "next%3Dhttps://ci:CanaryGlued15abc@example.com/x",
            "CanaryGlued15",
        ),
        ("id AKIACANARYGLUED16XYZand more", "CANARYGLUED16"),
    ];
    let mut leaked = Vec::new();
    for (input, body) in cases {
        let out = redact(input);
        if out.contains(body) {
            leaked.push(format!("{input} -> {out}"));
        }
    }
    assert!(leaked.is_empty(), "not redacted:\n{}", leaked.join("\n"));
    // The encoding in front of the token is kept.
    assert_eq!(
        redact("next%3Dghp_CanaryGlued01abcdefghijklmnopqrstuvwx&x=1"),
        "next%3D[REDACTED:github_token]&x=1"
    );
    // Words that merely contain a prefix are not tokens.
    for benign in [
        "task-runner-configuration-file-v2",
        "desk_test_handles_everything_right",
        "laughp_is_not_a_token_at_all_really_no",
        "MALAYSIAPACIFICREGIONSERVERS",
        "the %3D sign is =, and \\n is a newline",
        "Add a \"--password\" option; see [\"--token\", \"flag\"] in the docs",
        "the `--token` flag and --secret-file are documented",
        "AKIAXXXXXXXXXXXXXXXXYZ_LONG_CONSTANT",
        "トークンの有効期限を確認してください",
        "https://github.com/org/repo and ftp://mirror.example/pub",
    ] {
        assert_eq!(redact(benign), benign);
    }
}

// ------------------------------------------------------- shell / path parsing

mod shell_props {
    use proptest::prelude::*;
    use velra_core::shell::{self, Dialect, Reach};

    const DIALECTS: [Dialect; 3] = [Dialect::Posix, Dialect::PowerShell, Dialect::Cmd];

    /// Characters that mean something to one of the three shells, plus
    /// multi-byte text and line endings.
    fn shellish() -> impl Strategy<Value = String> {
        let atoms = prop::sample::select(vec![
            "git",
            "restore",
            "checkout",
            "reset",
            "--hard",
            "--",
            "-C",
            "cd",
            "/d",
            "cmd",
            "/c",
            "//c",
            "bash",
            "-c",
            "pwsh",
            "-Command",
            "Set-Location",
            " ",
            "  ",
            "\t",
            "\n",
            "\r\n",
            "\r",
            "&",
            "&&",
            "|",
            "||",
            ";",
            "'",
            "\"",
            "\\",
            "`",
            "^",
            "$",
            "%",
            "*",
            "?",
            "~",
            ".",
            "..",
            "/",
            ":",
            "C:",
            "c:\\",
            "/c/",
            "a.rs",
            "src",
            "é",
            "日本",
            "ü ber",
            "RUNNER~1",
            "-p",
            ":/",
            "\u{0}",
        ]);
        prop::collection::vec(atoms, 0..40).prop_map(|v| v.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Arbitrary shell-like text never panics any parser, and every
        /// subcommand range is inside the line, on character boundaries, in
        /// order and non-overlapping.
        #[test]
        fn parsers_never_panic_and_ranges_are_well_formed(line in shellish()) {
            for d in DIALECTS {
                let mut last = 0usize;
                for (a, b) in shell::subcommand_ranges_in(&line, d) {
                    prop_assert!(a <= b && b <= line.len(), "{d:?} {a}..{b} of {}", line.len());
                    prop_assert!(line.is_char_boundary(a) && line.is_char_boundary(b));
                    prop_assert!(a >= last, "{d:?}: ranges overlap or go back");
                    last = b;
                    let _ = shell::tokenize_in(&line[a..b], d);
                    let _ = shell::command_words_in(&line[a..b], d);
                }
                let _ = shell::git_calls(&line, d);
                for t in shell::restore_targets(&line, d) {
                    for root in ["/home/u/proj", "C:/Users/me/proj"] {
                        let _ = shell::reaches(&t, "src/a.rs", Some(root), root);
                        let _ = shell::reaches(&t, "/elsewhere/a.rs", None, root);
                    }
                }
                let _ = shell::git_effects_in(&line, d, &|_| true);
            }
            let shown = shell::display_command(&line);
            prop_assert!(line.contains(shown));
        }
    }

    /// One way to write a restore of one file from the project root.
    #[derive(Debug, Clone)]
    struct Spelling {
        dialect: Dialect,
        line: String,
    }

    fn quote(word: &str, dialect: Dialect, style: u8) -> String {
        let spaced = word.contains(' ');
        match (dialect, style % 3) {
            (_, 0) if !spaced => word.to_string(),
            (Dialect::Posix, 1) => word.replace(' ', "\\ "),
            (Dialect::Cmd, _) => format!("\"{word}\""),
            (_, 2) => format!("'{word}'"),
            _ => format!("\"{word}\""),
        }
    }

    const ROOT_POSIX: &str = "/home/u/proj x";
    const ROOT_WIN: &str = "C:/Users/me/proj é";

    /// The file, as the command names it: relative, `./`, through `..`, or
    /// absolute; with backslashes where the dialect takes them.
    fn file_word(rel: &str, root: &str, dialect: Dialect, form: u8) -> String {
        let w = match form % 4 {
            0 => rel.to_string(),
            1 => format!("./{rel}"),
            2 => {
                let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
                let first = if dir.is_empty() {
                    "zz".to_string()
                } else {
                    dir.to_string()
                };
                if dir.is_empty() {
                    format!("{first}/../{name}")
                } else {
                    format!("{first}/sub/../{name}")
                }
            }
            _ => format!("{root}/{rel}"),
        };
        if dialect != Dialect::Posix && form % 2 == 1 {
            w.replace('/', "\\")
        } else {
            w
        }
    }

    fn cd_word(root: &str, dialect: Dialect, form: u8) -> String {
        let windows = root.as_bytes().get(1) == Some(&b':');
        match (dialect, form % 4) {
            (Dialect::Posix, 1) if windows => format!("/c{}", &root[2..]),
            (Dialect::Posix, 2) => format!("{root}/"),
            (Dialect::Posix, 3) if windows => format!("c:{}", &root[2..]),
            (Dialect::Posix, _) => root.to_string(),
            (_, 1) => root.replace('/', "\\"),
            (_, 2) => format!("{}\\", root.replace('/', "\\")),
            (_, 3) => format!("c:{}", &root[2..]),
            _ => root.to_string(),
        }
    }

    fn spelling(
        rel: &str,
        root: &str,
        dialect: Dialect,
        knobs: (u8, u8, u8, u8, u8, u8),
    ) -> Spelling {
        let (qstyle, fform, cdform, sep, git, prefix) = knobs;
        let git = match git % 3 {
            0 => "git",
            1 if dialect != Dialect::Posix => "GIT",
            1 => "git",
            _ => "git.exe",
        };
        let file = quote(&file_word(rel, root, dialect, fform), dialect, qstyle);
        let restore = match fform % 3 {
            0 => format!("{git} restore {file}"),
            1 => format!("{git} checkout -- {file}"),
            _ => format!("{git} restore --worktree -- {file}"),
        };
        let seps: &[&str] = match dialect {
            Dialect::Posix => &[" && ", " ; ", "\n", " || "],
            Dialect::PowerShell => &[" ; ", " && ", "\n", "\r\n"],
            Dialect::Cmd => &[" & ", " && ", "\r\n", " || "],
        };
        let sep = seps[usize::from(sep) % seps.len()];
        let cd = quote(
            &cd_word(root, dialect, cdform),
            dialect,
            qstyle.wrapping_add(1),
        );
        let cd = match (dialect, cdform % 2) {
            (Dialect::PowerShell, 1) => format!("Set-Location -Path {cd}"),
            (Dialect::Cmd, _) => format!("cd /d {cd}"),
            _ => format!("cd {cd}"),
        };
        let line = match prefix % 3 {
            0 => restore,
            1 => format!("{cd}{sep}{restore}"),
            _ => format!("{cd}{sep}{restore}{sep}echo done"),
        };
        Spelling { dialect, line }
    }

    fn rel_path() -> impl Strategy<Value = String> {
        let seg = prop::sample::select(vec!["src", "a b", "日本", "ü ber", "lib", "x.y"]);
        let name = prop::sample::select(vec!["a.rs", "b c.py", "é.txt", "Makefile", "日本.rs"]);
        (prop::collection::vec(seg, 0..3), name).prop_map(|(dirs, name)| {
            let mut parts: Vec<&str> = dirs;
            parts.push(name);
            parts.join("/")
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(768))]

        /// Equivalent spellings of one restore reach the same file; they
        /// never reach a different file, and never a file of another
        /// workspace.
        #[test]
        fn equivalent_restores_reach_the_same_file_and_nothing_else(
            rel in rel_path(),
            windows_root in any::<bool>(),
            dialect in prop::sample::select(DIALECTS.to_vec()),
            knobs in any::<(u8, u8, u8, u8, u8, u8)>(),
        ) {
            let root = if windows_root || dialect != Dialect::Posix { ROOT_WIN } else { ROOT_POSIX };
            let s = spelling(&rel, root, dialect, knobs);
            let targets = shell::restore_targets(&s.line, s.dialect);
            prop_assert_eq!(targets.len(), 1, "{:?}", s);
            let t = &targets[0];
            prop_assert!(matches!(t.reach, Reach::Paths(_)), "{:?} -> {:?}", s, t);

            let reached = shell::reaches(t, &rel, Some(root), root);
            prop_assert_eq!(reached, Some(true), "{:?} -> {:?}", s, t);

            // A different file: a longer name, a sibling, another directory's
            // file of the same name, and what follows a space in the name
            // (the second half of a word split at an escaped space, D142).
            let mut others = vec![format!("{rel}x"), format!("{rel}.bak"), format!("zz/{rel}")];
            if let Some((_, after)) = rel.rsplit_once(' ') {
                others.push(after.to_string());
            }
            for other in others {
                prop_assert_ne!(
                    shell::reaches(t, &other, Some(root), root), Some(true),
                    "{:?} reached {}", s, other
                );
            }
            // The same relative path in another workspace.
            let other_root = format!("{root}2");
            prop_assert_ne!(
                shell::reaches(t, &rel, Some(root), &other_root), Some(true),
                "{:?} reached {} in {}", s, rel, other_root
            );
        }
    }

    /// Case: Windows paths compare without regard to case, POSIX paths do
    /// not. Spelled in upper case, the pathspec reaches the file on Windows
    /// only.
    #[test]
    fn a_pathspec_in_another_case_reaches_the_file_on_windows_only() {
        let root = ROOT_WIN;
        for (line, d) in [
            ("git restore SRC/A.RS", Dialect::Posix),
            ("GIT restore SRC\\A.RS", Dialect::PowerShell),
            (
                r#"cd /d "C:\USERS\ME\PROJ É" && git restore src\a.rs"#,
                Dialect::Cmd,
            ),
        ] {
            let t = &shell::restore_targets(line, d)[0];
            let reached = shell::reaches(t, "src/a.rs", Some(root), root);
            let expected = if cfg!(windows) {
                Some(true)
            } else {
                Some(false)
            };
            assert_eq!(reached, expected, "{line}");
        }
    }

    /// An 8.3 short name spelled inside the command (`cd C:\Users\RUNNER~1`)
    /// is not the root to a lexical comparison. What matters is that it is
    /// never taken for some *other* directory's file: a restore through it
    /// is left uncredited, never credited to the wrong file.
    #[test]
    fn a_short_name_inside_a_command_is_never_credited_to_another_file() {
        let root = "C:/Users/runneradmin/proj";
        let line = r"cd C:\Users\RUNNER~1\proj && git restore src\a.rs";
        let t = &shell::restore_targets(line, Dialect::PowerShell)[0];
        assert_ne!(shell::reaches(t, "src/a.rs", Some(root), root), Some(true));
        assert_ne!(shell::reaches(t, "src/b.rs", Some(root), root), Some(true));
    }
}

mod shell_escaped_space {
    use velra_core::shell::{self, Dialect};

    /// A POSIX `\ ` is an escaped space: `git restore src/a\ b.rs` names one
    /// file, `src/a b.rs`. Split at the space instead, its second half is a
    /// pathspec of its own -- `b.rs` at the root, a different file, which a
    /// change to it was then credited to.
    #[test]
    fn an_escaped_space_is_part_of_the_word() {
        let root = "/home/u/proj";
        let t = &shell::restore_targets(r"git restore src/a\ b.rs", Dialect::Posix)[0];
        assert_eq!(
            shell::reaches(t, "b.rs", Some(root), root),
            Some(false),
            "{t:?}"
        );
        assert_eq!(
            shell::reaches(t, "src/a b.rs", Some(root), root),
            Some(true),
            "{t:?}"
        );
        assert_eq!(
            shell::tokenize(r#"cd /home/u/proj\ x && echo \"q\" \'s\'"#),
            vec!["cd", "/home/u/proj x", "&&", "echo", "\"q\"", "'s'"]
        );
        // A backslash before anything else is left alone: Windows paths in
        // a Bash tool line are read as written, as before.
        assert_eq!(
            shell::tokenize(r"type C:\Users\me\a.rs"),
            vec!["type", r"C:\Users\me\a.rs"]
        );
    }
}

// ----------------------------------------------- reducer: same events, same state

mod reducer_props {
    use super::common::Log;
    use proptest::prelude::*;
    use rusqlite::Connection;
    use std::path::Path;
    use velra_core::db::{Db, Role};
    use velra_core::event::{NewEvent, ProjectInfo};
    use velra_core::model::Trigger;
    use velra_core::{eventlog, reducer, spool};

    #[derive(Debug, Clone)]
    enum Op {
        Prompt(u8),
        Edit(u8, u8),
        Read(u8),
        Pass,
        Fail(u8),
        Restore(u8),
        Stop,
        Switch,
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..6).prop_map(Op::Prompt),
            (0u8..3, 1u8..5).prop_map(|(f, v)| Op::Edit(f, v)),
            (0u8..4).prop_map(Op::Read),
            Just(Op::Pass),
            (0u8..3).prop_map(Op::Fail),
            (0u8..3).prop_map(Op::Restore),
            Just(Op::Stop),
            Just(Op::Switch),
        ]
    }

    const FILES: [&str; 3] = ["src/money.py", "src/retry.py", "tests/test_pay.py"];
    const PROMPTS: [&str; 6] = [
        "Fix the failing payment retry test in tests/test_pay.py. Do not modify the tests.",
        "Task: make src/money.py round half-even; never touch the public API.",
        "try the module-level idempotency key instead",
        "<ide_opened_file>The user opened src/retry.py</ide_opened_file>",
        "Subtask: add a regression test for the retry backoff",
        "keep going",
    ];

    /// Runs `ops` against a real project directory and returns every event
    /// the log recorded, in the order it happened. The second session
    /// belongs to another workspace.
    fn record(ops: &[Op]) -> (Log, Vec<NewEvent>, ProjectInfo, ProjectInfo) {
        let mut log = Log::new();
        for f in FILES {
            log.env.write_file(f, "v0\n");
        }
        let a = log.env.project_info();
        let b = ProjectInfo {
            project_id: "b000000000000000".into(),
            root_path: "/elsewhere/other-workspace".into(),
            is_git: true,
        };
        let mut second = false;
        for op in ops {
            match op {
                Op::Prompt(i) => log.prompt(PROMPTS[usize::from(*i) % PROMPTS.len()]),
                Op::Edit(f, v) => log.edit(FILES[usize::from(*f) % 3], &format!("v{v}\n")),
                Op::Read(f) => log.read(FILES.get(usize::from(*f)).copied().unwrap_or("README.md")),
                Op::Pass => log.command_ok("python -m pytest -q", "3 passed in 0.10s"),
                Op::Fail(t) => log.command_fail(
                    "python -m pytest -q",
                    1,
                    &format!("FAILED tests/test_pay.py::test_retry_{t} - AssertionError\n1 failed"),
                ),
                Op::Restore(f) => {
                    let file = FILES[usize::from(*f) % 3];
                    log.git_restore(&format!("git restore {file}"), &[(file, "v0\n")]);
                }
                Op::Stop => log.stop(),
                Op::Switch => {
                    second = !second;
                    log.switch_session(if second { "session-b" } else { "test-session" });
                }
            }
        }
        let events = log
            .db
            .conn
            .prepare(
                "SELECT dedupe_key, session_id, project_id, agent_id, hook_event, tool_name, \
                 tool_use_id, ts_ms, payload FROM events ORDER BY id",
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
                    project: None,
                })
            })
            .unwrap()
            .map(|r| r.unwrap())
            .map(|mut ev| {
                let info = if ev.session_id == "session-b" {
                    b.clone()
                } else {
                    a.clone()
                };
                ev.project_id = info.project_id.clone();
                ev.project = Some(info);
                ev
            })
            .collect();
        (log, events, a, b)
    }

    fn fresh(dir: &Path, name: &str) -> Db {
        let db = Db::open(&dir.join(name), Role::Cli).unwrap();
        db.conn
            .busy_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        db
    }

    fn rows(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            use rusqlite::types::ValueRef;
            Ok((0..n)
                .map(|i| match r.get_ref(i).unwrap() {
                    ValueRef::Null => "NULL".to_string(),
                    ValueRef::Integer(v) => v.to_string(),
                    ValueRef::Real(v) => v.to_string(),
                    ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    ValueRef::Blob(b) => format!("{b:?}"),
                })
                .collect::<Vec<_>>()
                .join(" | "))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    /// Everything the reducer derived, with row ids replaced by the dedupe
    /// key of the event they came from, plus each session's capsule.
    fn state(conn: &Connection) -> String {
        let mut out = Vec::new();
        let ev = "(SELECT dedupe_key FROM events WHERE id = {})";
        let key = |col: &str| ev.replace("{}", col);
        for (name, sql) in [
            ("sessions", "SELECT session_id, project_id, epoch, ended_ms, end_reason FROM sessions ORDER BY session_id".to_string()),
            ("intents", format!("SELECT session_id, epoch, level, text, {}, superseded_ms IS NULL FROM intents ORDER BY session_id, {}, level", key("source_event_id"), key("source_event_id"))),
            ("constraints", format!("SELECT session_id, epoch, text, cue, kind, prompt_ordinal, {}, superseded_ms IS NULL FROM constraints ORDER BY session_id, text", key("source_event_id"))),
            ("edits", format!("SELECT session_id, epoch, {}, path, tool_name, pre_hash, post_hash, status, mechanism, {}, excerpt FROM edits ORDER BY {}", key("event_id"), key("resolved_event_id"), key("event_id"))),
            ("dead_ends", "SELECT session_id, epoch, path, mechanism, command_text, reapplied, (SELECT group_concat((SELECT dedupe_key FROM events WHERE id = (SELECT event_id FROM edits WHERE id = j.value)), ',') FROM json_each(edit_ids) j) FROM dead_ends ORDER BY session_id, path, 7".to_string()),
            ("commands", format!("SELECT session_id, epoch, {}, kind, signature, outcome, exit_code, mentioned_paths FROM commands ORDER BY {}", key("event_id"), key("event_id"))),
            ("file_stats", "SELECT session_id, epoch, path, reads, edits, in_failure, last_touch_ms, first_touch_ms FROM file_stats ORDER BY session_id, epoch, path".to_string()),
            // In id order within a file: the order versions were recorded in
            // is what the revert rules read.
            ("file_versions", format!("SELECT session_id, path, content_hash, source, {}, ts_ms FROM file_versions ORDER BY session_id, path, id", key("event_id"))),
        ] {
            out.push(format!("== {name}"));
            out.extend(rows(conn, &sql));
        }
        let sessions: Vec<String> = conn
            .prepare("SELECT session_id FROM sessions ORDER BY session_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for s in sessions {
            let meta = velra_core::snapshot::SnapshotMeta {
                checkpoint_id: "ckpt_01PROPERTY0000000000000000".into(),
                created_ms: super::common::BASE_MS + 10_000_000,
                trigger: Trigger::Manual,
                partial: false,
                preview: false,
                tz_offset_secs: 0,
            };
            let snap = velra_core::snapshot::build(conn, &s, &meta).unwrap();
            out.push(format!("== capsule {s}"));
            out.push(velra_core::render::render(&snap, &Default::default()).text);
        }
        out.join("\n")
    }

    fn append_all(db: &mut Db, events: &[NewEvent]) {
        for ev in events {
            eventlog::append(&mut db.conn, ev).unwrap();
        }
    }

    fn first_difference(a: &str, b: &str) -> String {
        a.lines()
            .zip(b.lines())
            .enumerate()
            .find(|(_, (x, y))| x != y)
            .map(|(i, (x, y))| format!("line {i}:\n  clean:   {x}\n  variant: {y}"))
            .unwrap_or_else(|| format!("lengths {} vs {}", a.lines().count(), b.lines().count()))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(40))]

        /// Same logical event set => same reduced state, however the events
        /// arrive: one reduction over all of them (the reference), one
        /// reduction per event, every event twice, a subset arriving late
        /// through the spool after the rest were reduced, and two reducers
        /// racing over the same database.
        #[test]
        fn the_reduced_state_depends_only_on_the_events(
            ops in prop::collection::vec(arb_op(), 1..28),
            late_mask in any::<u64>(),
        ) {
            let (log, events, _, _) = record(&ops);
            let dir = tempfile::tempdir().unwrap();

            let mut clean = fresh(dir.path(), "clean.db");
            append_all(&mut clean, &events);
            reducer::reduce_all(&mut clean.conn, None).unwrap();
            let reference = state(&clean.conn);

            // One event at a time.
            let mut inc = fresh(dir.path(), "incremental.db");
            for ev in &events {
                eventlog::append(&mut inc.conn, ev).unwrap();
                reducer::reduce_all(&mut inc.conn, None).unwrap();
            }
            let got = state(&inc.conn);
            prop_assert!(got == reference, "incremental: {}", first_difference(&reference, &got));

            // Every event twice, reduced in between: replay adds nothing.
            let mut dup = fresh(dir.path(), "duplicate.db");
            append_all(&mut dup, &events);
            reducer::reduce_all(&mut dup.conn, None).unwrap();
            append_all(&mut dup, &events);
            reducer::reduce_all(&mut dup.conn, None).unwrap();
            let got = state(&dup.conn);
            prop_assert!(got == reference, "duplicated: {}", first_difference(&reference, &got));
            let stored: i64 = dup.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
            prop_assert_eq!(stored as usize, events.len());

            // A subset late through the spool, after the rest were reduced.
            let spool_dir = dir.path().join("spool");
            let mut late = fresh(dir.path(), "late.db");
            let mut spooled = 0usize;
            for (i, ev) in events.iter().enumerate() {
                if late_mask >> (i % 64) & 1 == 1 {
                    spool::write(&spool_dir, ev).unwrap();
                    spooled += 1;
                } else {
                    eventlog::append(&mut late.conn, ev).unwrap();
                }
            }
            reducer::reduce_all(&mut late.conn, None).unwrap();
            let stats = reducer::reduce_all(&mut late.conn, Some(&spool_dir)).unwrap();
            prop_assert_eq!(stats.spool_ingested, spooled);
            prop_assert_eq!(spool::backlog(&spool_dir), 0);
            let got = state(&late.conn);
            prop_assert!(got == reference, "late spool ({spooled} of {}): {}", events.len(), first_difference(&reference, &got));

            // Two reducers at once.
            let path = dir.path().join("racing.db");
            {
                let mut db = fresh(dir.path(), "racing.db");
                append_all(&mut db, &events);
            }
            let racers: Vec<_> = (0..2)
                .map(|_| {
                    let path = path.clone();
                    std::thread::spawn(move || {
                        let mut db = Db::open(&path, Role::Cli).unwrap();
                        db.conn.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
                        reducer::reduce_all(&mut db.conn, None).map(|_| ())
                    })
                })
                .collect();
            for r in racers {
                r.join().unwrap().unwrap();
            }
            let racing = Db::open(&path, Role::Cli).unwrap();
            let got = state(&racing.conn);
            prop_assert!(got == reference, "two reducers: {}", first_difference(&reference, &got));
            drop(log);
        }
    }
}

// ------------------------------------------------ settings under concurrency

/// `velra enable` / `disable` while another program keeps saving the same
/// settings file -- Claude Code's `/config`, an editor. The other program
/// adds one key per save, each by read, modify, write-temp, rename.
///
/// Invariants: a reader never sees a file that does not parse; the user's
/// own settings survive; Velra's handlers are registered exactly once after
/// a final `enable`; no temp file is left behind; a run that gives up says
/// so. How many of the other program's saves were lost is measured and
/// printed (the remaining read-to-rename window, D143).
#[test]
fn settings_edits_racing_another_writer_lose_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let env = common::Env::new();
    let settings = env.settings_path();
    std::fs::write(
        &settings,
        "{\n  // user settings\n  \"model\": \"sonnet\",\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]}\n}\n",
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));

    // The other program.
    let writer = {
        let settings = settings.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut saved = 0usize;
            let mut attempts = 0usize;
            while !stop.load(Ordering::Relaxed) && saved < 400 {
                attempts += 1;
                let Ok(text) = std::fs::read_to_string(&settings) else {
                    continue;
                };
                let Some(close) = text.rfind('}') else {
                    continue;
                };
                let updated = format!(
                    "{},\n  \"other{saved}\": {saved}\n}}\n",
                    text[..close].trim_end()
                );
                let tmp = settings.with_extension(format!("other-{saved}"));
                if std::fs::write(&tmp, &updated).is_err() {
                    continue;
                }
                if std::fs::rename(&tmp, &settings).is_ok() {
                    saved += 1;
                } else {
                    let _ = std::fs::remove_file(&tmp);
                }
                std::thread::sleep(std::time::Duration::from_millis(12));
            }
            (saved, attempts)
        })
    };

    // Something reading it, as Claude Code does.
    let reader = {
        let settings = settings.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut unparseable = Vec::new();
            let mut reads = 0usize;
            while !stop.load(Ordering::Relaxed) {
                if let Ok(text) = std::fs::read_to_string(&settings) {
                    reads += 1;
                    let parsed = jsonc_parse(&text);
                    if !parsed {
                        unparseable.push(text);
                    }
                }
            }
            (reads, unparseable)
        })
    };

    let mut codes = Vec::new();
    for i in 0..40 {
        let sub = if i % 2 == 0 { "enable" } else { "disable" };
        let out = env
            .cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.269")
            .arg(sub)
            .output()
            .unwrap();
        codes.push((
            sub,
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        ));
    }
    stop.store(true, Ordering::Relaxed);
    let (saved, attempts) = writer.join().unwrap();
    let (reads, unparseable) = reader.join().unwrap();

    let out = env
        .cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.269")
        .arg("enable")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = std::fs::read_to_string(&settings).unwrap();
    let lost: Vec<usize> = (0..saved)
        .filter(|i| !text.contains(&format!("\"other{i}\"")))
        .collect();
    eprintln!(
        "other program saved {saved} times ({attempts} attempts); reader read {reads} times; \
         velra runs: {:?}",
        codes.iter().map(|(s, c, _)| (s, c)).collect::<Vec<_>>()
    );
    assert!(
        unparseable.is_empty(),
        "a reader saw {} unparseable files: {:?}",
        unparseable.len(),
        unparseable.first()
    );
    // Not asserted: between Velra's last read and its rename there is still
    // a window no file system lets us close (D143). The deterministic case
    // -- a save while the replacement is written -- is
    // `settings::tests::a_save_landing_while_the_replacement_is_written_is_kept`;
    // this reports how often the remaining window is hit.
    eprintln!("RESIDUAL lost {} of {saved} saves: {lost:?}", lost.len());
    assert!(
        text.contains("\"model\": \"sonnet\"") && text.contains("Bash(ls)"),
        "{text}"
    );
    let handlers = text.matches("\"hook\"").count();
    let copies = text.matches("\"stop\"").count();
    assert_eq!(copies, 1, "handlers: {handlers}\n{text}");
    let leftovers: Vec<String> = std::fs::read_dir(settings.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("velra-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    for (sub, code, stderr) in &codes {
        // A run that lost every retry says so and changes nothing.
        assert!(
            *code == Some(0) || stderr.contains("modified concurrently"),
            "{sub}: {code:?} {stderr}"
        );
    }
}

/// Whether `text` parses as JSON with comments, the way Claude Code reads
/// its settings (a trailing comma in an object is tolerated there too).
fn jsonc_parse(text: &str) -> bool {
    let body = text.trim_start_matches('\u{feff}');
    let stripped: String = body
        .lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str::<serde_json::Value>(&stripped).is_ok()
}

// ------------------------------------------------ settings: killed mid-write

/// Velra temp files (`.<name>.velra-tmp-<pid>`) in `dir`.
#[cfg(feature = "fault-injection")]
fn velra_temps(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(".velra-tmp-")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `velra enable` killed at each point of its two atomic writes -- the
/// backup, then the settings file -- partway through writing the temp file
/// and after it is written but before the rename.
///
/// Invariants: the settings file is byte for byte what it was; no backup is
/// ever a truncated copy; the next `enable` succeeds, keeps the user's own
/// settings and clears the temp files the killed run left.
#[cfg(feature = "fault-injection")]
#[test]
fn enable_killed_at_every_point_of_a_write_leaves_the_settings_intact() {
    use std::time::{Duration, Instant, SystemTime};
    let original = "{\n  // mine\n  \"model\": \"opus\",\n  \"env\": {\"A\": \"1\"}\n}\n";
    for (stall, target) in [
        ("VELRA_TEST_STALL_MID_TEMP_WRITE_MS", ".bak"),
        ("VELRA_TEST_STALL_BEFORE_REPLACE_MS", ".bak"),
        ("VELRA_TEST_STALL_MID_TEMP_WRITE_MS", "settings.json"),
        ("VELRA_TEST_STALL_BEFORE_REPLACE_MS", "settings.json"),
    ] {
        let env = common::Env::new();
        let settings = env.settings_path();
        std::fs::write(&settings, original).unwrap();
        let backups = env.home.join("backups");
        let where_ = if target == ".bak" {
            backups.clone()
        } else {
            env.config.clone()
        };
        #[allow(deprecated)]
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
            .env("VELRA_HOME", &env.home)
            .env("CLAUDE_CONFIG_DIR", &env.config)
            .env("CLAUDE_PROJECT_DIR", &env.project)
            .env("VELRA_CLAUDE_VERSION", "2.1.269")
            .env(stall, "30000")
            .env("VELRA_TEST_STALL_TARGET", target)
            .current_dir(&env.project)
            .arg("enable")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Wait for the temp file of the write being interrupted.
        let deadline = Instant::now() + Duration::from_secs(20);
        while velra_temps(&where_).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let temps = velra_temps(&where_);
        assert!(
            !temps.is_empty(),
            "{stall} {target}: never reached the write"
        );
        // Let the stall begin, then kill.
        std::thread::sleep(Duration::from_millis(200));
        child.kill().unwrap();
        child.wait().unwrap();

        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            original,
            "{stall} {target}: the settings file changed"
        );
        for b in std::fs::read_dir(&backups)
            .map(|r| r.flatten().collect::<Vec<_>>())
            .unwrap_or_default()
        {
            let name = b.file_name().to_string_lossy().into_owned();
            if name.ends_with(".bak") {
                assert_eq!(
                    std::fs::read_to_string(b.path()).unwrap(),
                    original,
                    "{stall} {target}: backup {name} is not a whole copy"
                );
            }
        }

        // Next run, once the killed run's temp file is old enough to be
        // known abandoned.
        for t in velra_temps(&where_) {
            let f = std::fs::OpenOptions::new().write(true).open(&t).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(120))
                .unwrap();
        }
        let out = env
            .cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.269")
            .arg("enable")
            .output()
            .unwrap();
        assert!(out.status.success(), "{stall} {target}: {out:?}");
        let text = std::fs::read_to_string(&settings).unwrap();
        assert!(
            text.contains("\"model\": \"opus\"") && text.contains("// mine"),
            "{text}"
        );
        assert!(text.contains("\"hook\""), "{text}");
        assert!(
            velra_temps(&env.config).is_empty(),
            "{:?}",
            velra_temps(&env.config)
        );
        assert!(
            velra_temps(&backups).is_empty(),
            "{:?}",
            velra_temps(&backups)
        );
    }
}

// ----------------------------------- continuation lifecycle through the hooks

#[cfg(feature = "fault-injection")]
mod lifecycle_model {
    use super::common::Env;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::{BufRead, Write};
    use std::time::Duration;
    use velra_core::continuation::MAX_ATTACH;

    struct Xorshift(u64);
    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Op {
        PreCompact,
        Start(&'static str),
        Prompt,
        Tool,
        Stop,
        End(&'static str),
        /// A session start killed after its capsule reached stdout and
        /// before the delivery committed.
        KillStart,
        /// The same, on the prompt channel.
        KillPrompt,
    }

    const OPS: [Op; 12] = [
        Op::PreCompact,
        Op::Start("compact"),
        Op::Start("clear"),
        Op::Start("resume"),
        Op::Start("startup"),
        Op::Prompt,
        Op::Tool,
        Op::Stop,
        Op::End("clear"),
        Op::End("prompt_input_exit"),
        Op::KillStart,
        Op::KillPrompt,
    ];

    fn payload(env: &Env, session: &str, op: Op, n: u64) -> (&'static str, Value) {
        let mut p = env.base_payload("x");
        p["session_id"] = json!(session);
        let (sub, event) = match op {
            Op::PreCompact => ("pre-compact", "PreCompact"),
            Op::Start(_) | Op::KillStart => ("session-start", "SessionStart"),
            Op::Prompt | Op::KillPrompt => ("user-prompt-submit", "UserPromptSubmit"),
            Op::Tool => ("post-tool-use", "PostToolUse"),
            Op::Stop => ("stop", "Stop"),
            Op::End(_) => ("session-end", "SessionEnd"),
        };
        p["hook_event_name"] = json!(event);
        match op {
            Op::PreCompact => p["trigger"] = json!("manual"),
            Op::Start(src) => p["source"] = json!(src),
            Op::KillStart => p["source"] = json!("compact"),
            Op::Prompt | Op::KillPrompt => {
                p["prompt"] = json!("what should we try next?");
                p["prompt_id"] = json!(format!("{session}-p{n}"));
            }
            Op::Tool => {
                p["tool_name"] = json!("Read");
                p["tool_use_id"] = json!(format!("{session}-t{n}"));
                p["tool_input"] = json!({"file_path": env.project.join("src/a.rs")});
            }
            Op::End(reason) => p["reason"] = json!(reason),
            Op::Stop => p["stop_hook_active"] = json!(false),
        }
        (sub, p)
    }

    /// Runs a hook; for a `Kill*` op, kills it once its first line of
    /// output arrives (it is then stalled between the write and the
    /// commit). Returns its stdout and whether it was killed.
    fn run(env: &Env, sub: &str, p: &Value, kill: bool) -> (String, bool) {
        if !kill {
            let out = env.hook(sub, p);
            assert_eq!(out.code, 0, "{sub}: {}", out.stderr);
            assert!(out.stderr.is_empty(), "{sub}: {}", out.stderr);
            return (out.stdout, false);
        }
        #[allow(deprecated)]
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
            .env("VELRA_HOME", &env.home)
            .env("CLAUDE_CONFIG_DIR", &env.config)
            .env("CLAUDE_PROJECT_DIR", &env.project)
            .env("VELRA_TEST_WATCHDOG_MS", "60000")
            .env("VELRA_TEST_STALL_AFTER_CONTINUATION_EMIT_MS", "30000")
            .current_dir(&env.project)
            .args(["hook", sub])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(p.to_string().as_bytes())
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            // Nothing to deliver: it ends by itself, with no output.
            Ok(line) if line.is_empty() => {
                child.wait().unwrap();
                (String::new(), false)
            }
            Ok(line) => {
                child.kill().unwrap();
                child.wait().unwrap();
                (line, true)
            }
            Err(_) => {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{sub}: no output and no exit within 10 s");
            }
        }
    }

    fn delivered(stdout: &str) -> Option<String> {
        let v: Value = serde_json::from_str(stdout.trim_end()).ok()?;
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .map(str::to_string)
    }

    fn scalar(env: &Env, sql: &str) -> i64 {
        env.open_db().conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// A checkpoint's session, capsule and continuation state.
    type Checkpoint = (String, String, Option<String>);

    /// Checkpoint id -> its session, capsule and state.
    fn checkpoints(env: &Env) -> HashMap<String, Checkpoint> {
        let db = env.open_db();
        let mut stmt = db
            .conn
            .prepare(
                "SELECT k.checkpoint_id, k.session_id, k.capsule, c.state FROM checkpoints k \
                 LEFT JOIN continuations c ON c.checkpoint_id = k.checkpoint_id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows
    }

    fn work(env: &Env, session: &str) {
        let file = env.project.join("src/a.rs");
        let mut p = env.base_payload("UserPromptSubmit");
        p["session_id"] = json!(session);
        p["prompt"] = json!(format!(
            "fix the flaky logout test ({session}); keep the cookie behaviour"
        ));
        p["prompt_id"] = json!(format!("{session}-first"));
        env.hook("user-prompt-submit", &p).assert_contract();
        let mut p = env.base_payload("PostToolUse");
        p["session_id"] = json!(session);
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!(format!("{session}-edit"));
        p["tool_input"] = json!({"file_path": file, "old_string": "v0", "new_string": "v1"});
        p["tool_response"] = json!({"filePath": file, "originalFile": "v0\n"});
        env.hook("post-tool-use", &p).assert_contract();
    }

    /// The accepted worst case of emit-then-commit (D36, §18), measured with
    /// a hard kill rather than the watchdog (which D114 made safe): a
    /// session start killed after its capsule reached stdout, before the
    /// delivery committed. Its transaction rolls back, so the continuation
    /// is still PENDING and the next channel writes it once more -- and
    /// then never again.
    #[test]
    fn a_hard_kill_between_write_and_commit_costs_exactly_one_more_write() {
        let env = Env::new();
        env.write_file("src/a.rs", "v1\n");
        work(&env, "session-a");
        let (sub, p) = payload(&env, "session-a", Op::PreCompact, 0);
        run(&env, sub, &p, false);
        let (sub, p) = payload(&env, "session-a", Op::KillStart, 1);
        let (first, killed) = run(&env, sub, &p, true);
        assert!(killed && delivered(&first).is_some(), "{first}");
        assert_eq!(
            scalar(&env, "SELECT COUNT(*) FROM injections"),
            0,
            "rolled back"
        );
        let state = |env: &Env| {
            env.open_db()
                .conn
                .query_row("SELECT state FROM continuations", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap()
        };
        assert_eq!(state(&env), "PENDING");
        let mut written = 1;
        for (n, op) in [
            Op::Tool,
            Op::Prompt,
            Op::Tool,
            Op::Start("compact"),
            Op::Prompt,
        ]
        .into_iter()
        .enumerate()
        {
            let (sub, p) = payload(&env, "session-a", op, 10 + n as u64);
            let (out, _) = run(&env, sub, &p, false);
            if delivered(&out).is_some() {
                written += 1;
            }
        }
        assert_eq!(written, 2, "the capsule was written {written} times");
        assert_eq!(scalar(&env, "SELECT COUNT(*) FROM injections"), 1);
        assert_eq!(state(&env), "CONFIRMED");
    }

    /// The process that started the hook -- Claude Code -- is gone: nobody
    /// reads the hook's stdout. Writing the capsule fails; the delivery is
    /// rolled back, not recorded, and the capsule is still there for the
    /// session's next hook. The hook itself still exits 0 without a word on
    /// stderr (nobody would read that either).
    #[test]
    fn a_session_start_whose_reader_is_gone_delivers_nothing_and_keeps_the_capsule() {
        let env = Env::new();
        env.write_file("src/a.rs", "v1\n");
        work(&env, "session-a");
        let (sub, p) = payload(&env, "session-a", Op::PreCompact, 0);
        run(&env, sub, &p, false);
        let (sub, p) = payload(&env, "session-a", Op::Start("compact"), 1);
        #[allow(deprecated)]
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
            .env("VELRA_HOME", &env.home)
            .env("CLAUDE_CONFIG_DIR", &env.config)
            .env("CLAUDE_PROJECT_DIR", &env.project)
            .env("VELRA_TEST_WATCHDOG_MS", "60000")
            .current_dir(&env.project)
            .args(["hook", sub])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // The reader goes away before the hook has written anything.
        drop(child.stdout.take());
        child
            .stdin
            .take()
            .unwrap()
            .write_all(p.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        assert!(
            out.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(scalar(&env, "SELECT COUNT(*) FROM injections"), 0);
        let state: String = env
            .open_db()
            .conn
            .query_row("SELECT state FROM continuations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "PENDING");
        let (sub, p) = payload(&env, "session-a", Op::Tool, 2);
        let (out, _) = run(&env, sub, &p, false);
        assert!(
            delivered(&out).is_some(),
            "the next hook delivers it: {out}"
        );
        assert_eq!(scalar(&env, "SELECT COUNT(*) FROM injections"), 1);
    }

    /// Random lifecycles of two sessions in one workspace, through the real
    /// hook binary, with hard kills between a capsule's write and its
    /// commit. After every step:
    ///
    /// * a capsule written in a session's hook is one of *that* session's
    ///   checkpoints, and one that was live (PENDING / ATTACHED) before the
    ///   step -- never another session's, never a delivered-and-confirmed,
    ///   superseded or expired one;
    /// * at most one live continuation per session; `attach_count` within
    ///   `MAX_ATTACH`; CONFIRMED only after an injection;
    /// * no orphan rows: every injection and continuation belongs to a
    ///   checkpoint of the same session;
    /// * a checkpoint's capsule is written at most once per recorded
    ///   injection plus once per kill that interrupted its commit.
    #[test]
    fn random_lifecycles_with_hard_kills_keep_every_continuation_invariant() {
        let seeds: Vec<u64> = std::env::var("VELRA_LIFECYCLE_SEEDS")
            .ok()
            .map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect())
            .unwrap_or_else(|| vec![0x9E37_79B9, 0xC0FF_EE11, 0x1234_5678, 0xDEAD_BEEF]);
        let steps: u64 = std::env::var("VELRA_LIFECYCLE_STEPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16);
        for seed in seeds {
            let env = Env::new();
            env.write_file("src/a.rs", "v1\n");
            let sessions = ["session-a", "session-b"];
            for s in sessions {
                work(&env, s);
            }
            let mut rng = Xorshift(seed | 1);
            let mut emitted: HashMap<String, i64> = HashMap::new();
            let mut kills: HashMap<String, i64> = HashMap::new();
            let mut trace = Vec::new();
            for n in 0..steps {
                let session = sessions[rng.below(2) as usize];
                let op = OPS[rng.below(OPS.len() as u64) as usize];
                trace.push(format!("{session}:{op:?}"));
                let before = checkpoints(&env);
                let (sub, p) = payload(&env, session, op, n);
                let kill = matches!(op, Op::KillStart | Op::KillPrompt);
                let (stdout, killed) = run(&env, sub, &p, kill);
                let ctx = || format!("seed {seed:#x}, steps {}", trace.join(" -> "));

                if let Some(capsule) = delivered(&stdout) {
                    // Which checkpoint it is: the capsule is the stored one.
                    let hit: Vec<(&String, &Checkpoint)> = before
                        .iter()
                        .filter(|(_, (_, c, _))| *c == capsule)
                        .collect();
                    // A continuation delivers only checkpoints that already
                    // existed; SessionStart has no other source here.
                    assert_eq!(hit.len(), 1, "unknown capsule written: {}", ctx());
                    let (id, (owner, _, state)) = hit[0];
                    assert_eq!(owner, session, "another session's capsule: {}", ctx());
                    assert!(
                        matches!(state.as_deref(), Some("PENDING" | "ATTACHED")),
                        "a {state:?} continuation was written: {}",
                        ctx()
                    );
                    *emitted.entry(id.clone()).or_default() += 1;
                    if killed {
                        *kills.entry(id.clone()).or_default() += 1;
                    }
                }

                let live = scalar(
                    &env,
                    "SELECT COALESCE(MAX(n), 0) FROM (SELECT COUNT(*) AS n FROM continuations \
                     WHERE state IN ('PENDING','ATTACHED') GROUP BY session_id)",
                );
                assert!(live <= 1, "{live} live continuations: {}", ctx());
                let attach = scalar(
                    &env,
                    "SELECT COALESCE(MAX(attach_count), 0) FROM continuations",
                );
                assert!(attach <= MAX_ATTACH, "attach_count {attach}: {}", ctx());
                let bad = scalar(
                    &env,
                    "SELECT COUNT(*) FROM continuations c WHERE c.state = 'CONFIRMED' AND NOT EXISTS \
                     (SELECT 1 FROM injections i WHERE i.checkpoint_id = c.checkpoint_id)",
                );
                assert_eq!(bad, 0, "confirmed without an injection: {}", ctx());
                let orphans = scalar(
                    &env,
                    "SELECT (SELECT COUNT(*) FROM injections i WHERE NOT EXISTS \
                       (SELECT 1 FROM continuations c WHERE c.checkpoint_id = i.checkpoint_id)) + \
                     (SELECT COUNT(*) FROM continuations c WHERE NOT EXISTS \
                       (SELECT 1 FROM checkpoints k WHERE k.checkpoint_id = c.checkpoint_id \
                        AND k.session_id = c.session_id))",
                );
                assert_eq!(orphans, 0, "orphan rows: {}", ctx());
                for (id, n) in &emitted {
                    let injected = scalar(
                        &env,
                        &format!("SELECT COUNT(*) FROM injections WHERE checkpoint_id = '{id}'"),
                    );
                    let k = kills.get(id).copied().unwrap_or(0);
                    assert!(
                        *n <= injected + k,
                        "{id} written {n} times, {injected} injections, {k} kills: {}",
                        ctx()
                    );
                }
            }
            eprintln!(
                "seed {seed:#x}: {} steps, {} capsules written, {} killed mid-commit",
                steps,
                emitted.values().sum::<i64>(),
                kills.values().sum::<i64>()
            );
        }
    }
}

// ------------------------------------------------- spool and storage stress

/// Writers spooling while several ingesters race to drain the spool: every
/// event is stored exactly once, the spool ends empty, nothing is
/// quarantined. Counted, not eyeballed.
#[test]
fn spool_writers_racing_ingesters_store_every_event_exactly_once() {
    let rounds: usize = std::env::var("VELRA_SPOOL_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    for round in 0..rounds {
        let dir = tempfile::tempdir().unwrap();
        let spool_dir = dir.path().join("spool");
        let db_path = dir.path().join("velra.db");
        drop(Db::open(&db_path, Role::Cli).unwrap());
        let (writers, per_writer, ingesters) = (6usize, 50usize, 3usize);
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for w in 0..writers {
            let spool_dir = spool_dir.clone();
            let done = done.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..per_writer {
                    let ev = bare_event(
                        &format!("w{w}-e{i}"),
                        1_789_000_000_000 + (w * 1000 + i) as i64,
                    );
                    velra_core::spool::write(&spool_dir, &ev).unwrap();
                    // The same event again, as a retried hook would: stored once.
                    if i % 10 == 0 {
                        velra_core::spool::write(&spool_dir, &ev).unwrap();
                    }
                }
                done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        let mut ingested = Vec::new();
        for _ in 0..ingesters {
            let spool_dir = spool_dir.clone();
            let db_path = db_path.clone();
            let done = done.clone();
            ingested.push(std::thread::spawn(move || {
                let mut db = Db::open(&db_path, Role::Cli).unwrap();
                db.conn
                    .busy_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                let mut n = 0usize;
                loop {
                    let finished = done.load(std::sync::atomic::Ordering::SeqCst) == writers;
                    match velra_core::spool::ingest(&mut db.conn, &spool_dir, 37) {
                        Ok(k) => n += k,
                        Err(db::DbError::Busy) => {}
                        Err(e) => panic!("{e}"),
                    }
                    if finished && velra_core::spool::backlog(&spool_dir) == 0 {
                        return n;
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let consumed: usize = ingested.into_iter().map(|h| h.join().unwrap()).sum();
        let db = Db::open(&db_path, Role::Cli).unwrap();
        let stored: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        let distinct: i64 = db
            .conn
            .query_row("SELECT COUNT(DISTINCT dedupe_key) FROM events", [], |r| {
                r.get(0)
            })
            .unwrap();
        let written = writers * per_writer + writers * per_writer.div_ceil(10);
        let quarantined = std::fs::read_dir(spool_dir.join("bad"))
            .map(|r| r.count())
            .unwrap_or(0);
        eprintln!(
            "round {round}: {written} files written, {consumed} consumed by ingesters, \
             {stored} rows, {quarantined} quarantined"
        );
        assert_eq!(stored as usize, writers * per_writer, "round {round}");
        assert_eq!(distinct, stored, "round {round}");
        assert_eq!(velra_core::spool::backlog(&spool_dir), 0, "round {round}");
        assert_eq!(quarantined, 0, "round {round}");
        // A file is consumed once; two ingesters can both read one before
        // either deletes it, but the second insert is a duplicate and adds
        // no row.
        assert!(
            consumed >= writers * per_writer,
            "round {round}: {consumed}"
        );
    }
}

/// Many hook processes at once against one database, with a reducer
/// running beside them: every Read event sent is stored exactly once
/// (directly or through the spool), and the reduced read counts equal the
/// stored events per file -- nothing lost, nothing counted twice.
#[test]
fn concurrent_hook_processes_and_a_reducer_lose_and_duplicate_nothing() {
    let env = common::Env::new();
    let (procs, per_proc) = (6usize, 15usize);
    for p in 0..procs {
        env.write_file(&format!("src/f{p}.rs"), "x\n");
    }
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Created before anyone contends for it.
    drop(env.open_db());
    std::thread::scope(|scope| {
        // Holds the write lock past a hook's 100 ms budget, over and over, so
        // hooks meet a locked database and take the spool.
        let locker = scope.spawn(|| {
            let mut held = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let db = env.open_db();
                if db.conn.execute_batch("BEGIN IMMEDIATE").is_ok() {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    db.conn.execute_batch("COMMIT").unwrap();
                    held += 1;
                }
                drop(db);
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            held
        });
        let reducer = scope.spawn(|| {
            let mut runs = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let out = env.reduce();
                assert_eq!(out.code, 0, "{}", out.stderr);
                runs += 1;
            }
            runs
        });
        let senders: Vec<_> = (0..procs)
            .map(|p| {
                let env = &env;
                scope.spawn(move || {
                    for i in 0..per_proc {
                        let mut payload = env.base_payload("PostToolUse");
                        payload["tool_name"] = serde_json::json!("Read");
                        payload["tool_use_id"] = serde_json::json!(format!("p{p}-r{i}"));
                        payload["tool_input"] = serde_json::json!({
                            "file_path": env.project.join(format!("src/f{p}.rs"))
                        });
                        // Real deadline: under contention a hook spools.
                        let out = env
                            .cmd_with_real_watchdog()
                            .args(["hook", "post-tool-use"])
                            .write_stdin(payload.to_string())
                            .output()
                            .unwrap();
                        assert_eq!(out.status.code(), Some(0));
                    }
                })
            })
            .collect();
        for s in senders {
            s.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let runs = reducer.join().unwrap();
        let held = locker.join().unwrap();
        eprintln!("{runs} concurrent reductions, write lock held {held} times");
    });
    let spooled_before_drain = velra_core::spool::backlog(&env.spool_dir());
    let spooled_total: i64 = {
        let rows = env.drain_and_load_events(procs * per_proc);
        rows.iter()
            .filter(|r| r.json()["spooled"] == serde_json::json!(true))
            .count() as i64
    };
    let total = procs * per_proc;
    let rows = env.drain_and_load_events(total);
    assert_eq!(rows.len(), total, "stored events");
    let db = env.open_db();
    let distinct: i64 = db
        .conn
        .query_row("SELECT COUNT(DISTINCT dedupe_key) FROM events", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(distinct as usize, total);
    for p in 0..procs {
        let reads: i64 = db
            .conn
            .query_row(
                "SELECT COALESCE(SUM(reads), 0) FROM file_stats WHERE path = ?1",
                [format!("src/f{p}.rs")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reads as usize, per_proc, "reads of src/f{p}.rs");
    }
    assert_eq!(velra_core::spool::backlog(&env.spool_dir()), 0);
    eprintln!(
        "{total} events: {spooled_total} went through the spool,          {spooled_before_drain} still there when the senders finished"
    );
    assert!(spooled_total > 0, "the lock never sent a hook to the spool");
}

// --------------------------------------------------- redaction: any context

mod redaction_props {
    use proptest::prelude::*;
    use velra_core::redact::redact;

    /// A secret as `(text to embed, the body that must not survive)`.
    fn secret() -> impl Strategy<Value = (String, String)> {
        prop_oneof![
            "[A-Za-z0-9]{36}".prop_map(|b| (format!("ghp_{b}"), b)),
            "[A-Za-z0-9_]{30}".prop_map(|b| (format!("github_pat_{b}"), b)),
            "[A-Z0-9]{16}".prop_map(|b| (format!("AKIA{b}"), b)),
            "[A-Za-z0-9]{30}".prop_map(|b| (format!("sk-ant-{b}"), b)),
            "[A-Za-z0-9]{20}".prop_map(|b| (format!("xoxb-{b}"), b)),
            "[A-Za-z0-9]{35}".prop_map(|b| (format!("AIza{b}"), b)),
            "[A-Za-z0-9]{24}".prop_map(|b| (format!("sk_live_{b}"), b)),
            ("[A-Za-z0-9]{12}", "[A-Za-z0-9]{12}", "[A-Za-z0-9]{16}")
                .prop_map(|(a, b, c)| (format!("eyJ{a}.eyJ{b}.{c}"), format!("{b}.{c}"))),
            "[A-Za-z0-9]{14}".prop_map(|b| (format!("password={b}"), b)),
            "[A-Za-z0-9]{24}".prop_map(|b| (format!("Authorization: Basic {b}"), b)),
            "[A-Za-z0-9]{14}".prop_map(|b| (format!("https://ci:{b}@example.com/x"), b)),
            "[A-Z][A-Za-z0-9]{13}".prop_map(|b| (format!("--token {b}"), b)),
            "[A-Za-z0-9+/]{48}".prop_map(|b| (
                format!("-----BEGIN RSA PRIVATE KEY-----\n{b}\n-----END RSA PRIVATE KEY-----"),
                b
            )),
        ]
    }

    /// What may stand right before a secret: a break, a quote, an
    /// assignment, a bracket, or an escape that ends in a word character.
    fn before() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec![
            " ", "\n", "\t", "\"", "'", "=", ":", "(", "[", ",", ";", "%3D", "%20", "\\n",
            "\\u003d", "\r\n", "`",
        ])
    }

    fn after() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec![
            " ", "\n", "\"", "'", ",", ";", ")", "]", "&x=1", "%26", "",
        ])
    }

    fn surrounding() -> impl Strategy<Value = String> {
        "[a-z é日本_./:-]{0,40}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// No 12-character run of a secret's body survives redaction,
        /// whatever stands around it.
        #[test]
        fn no_part_of_a_secret_survives_in_any_context(
            (text, body) in secret(),
            pre in surrounding(), b in before(), a in after(), post in surrounding(),
        ) {
            let input = format!("{pre}{b}{text}{a}{post}");
            let out = redact(&input);
            for i in 0..body.len().saturating_sub(11) {
                let w = &body[i..i + 12];
                prop_assert!(!out.contains(w), "{w:?} of {text:?} survived in {out:?}");
            }
            // Redacting again changes nothing.
            let twice = redact(&out).into_owned();
            prop_assert_eq!(twice, out.into_owned());
        }
    }
}

// ------------------------------------ a deadline while the database opens

/// Reproduced under load (`storage.rs` c1 in a default build, about one run
/// in thirteen): hooks reached their 250 ms deadline while opening the
/// database, before they had armed their event, and exited 0 having
/// recorded nothing -- 20 of 200 events in one run, with nothing spooled
/// and nothing logged (D145). Here the open is stalled past a short
/// deadline, deterministically.
///
/// Invariants: the event reaches the spool, and from it the ledger; a
/// PreCompact still becomes a checkpoint.
#[cfg(feature = "fault-injection")]
#[test]
fn a_deadline_while_the_database_opens_still_records_the_event() {
    let env = common::Env::new();
    env.write_file("src/a.rs", "v0\n");
    drop(env.open_db());
    let stall = [
        ("VELRA_TEST_WATCHDOG_MS", "300"),
        ("VELRA_TEST_STALL_OPEN_DB_MS", "3000"),
    ];

    // A prompt first, so the session has something worth a checkpoint.
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = serde_json::json!("fix the retry test; do not modify the tests");
    env.hook("user-prompt-submit", &p).assert_contract();

    let mut read = env.base_payload("PostToolUse");
    read["tool_name"] = serde_json::json!("Read");
    read["tool_use_id"] = serde_json::json!("toolu_read_1");
    read["tool_input"] = serde_json::json!({"file_path": env.project.join("src/a.rs")});
    let started = std::time::Instant::now();
    env.hook_with_env("post-tool-use", &read, &stall)
        .assert_contract();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the deadline ended it"
    );

    let mut stop = env.base_payload("Stop");
    stop["stop_hook_active"] = serde_json::json!(false);
    env.hook_with_env("stop", &stop, &stall).assert_contract();

    let mut pre = env.base_payload("PreCompact");
    pre["trigger"] = serde_json::json!("auto");
    env.hook_with_env("pre-compact", &pre, &stall)
        .assert_contract();

    assert!(
        velra_core::spool::backlog(&env.spool_dir()) >= 3,
        "spooled on the way out"
    );
    let rows = env.drain_and_load_events(4);
    let kinds: Vec<(String, Option<String>)> = rows
        .iter()
        .map(|r| (r.hook_event.clone(), r.tool_name.clone()))
        .collect();
    for want in ["PostToolUse", "Stop", "PreCompact", "checkpoint_request"] {
        assert!(
            kinds.iter().any(|(h, _)| h == want),
            "{want} missing from {kinds:?}"
        );
    }
    let db = env.open_db();
    let checkpoints: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM checkpoints", [], |r| r.get(0))
        .unwrap();
    assert_eq!(checkpoints, 1, "the spooled request became the checkpoint");
    let errors = std::fs::read_to_string(env.home.join("logs/errors.log")).unwrap_or_default();
    assert!(!errors.contains("with no event armed"), "{errors}");
}

/// The other side of D145: a deadline before the handler armed anything is
/// no longer silent. The stall here is before the event exists at all.
#[cfg(feature = "fault-injection")]
#[test]
fn a_deadline_before_any_event_is_armed_is_logged() {
    let env = common::Env::new();
    let mut read = env.base_payload("PostToolUse");
    read["tool_name"] = serde_json::json!("Read");
    read["tool_use_id"] = serde_json::json!("toolu_read_2");
    read["tool_input"] = serde_json::json!({"file_path": env.project.join("src/a.rs")});
    env.hook_with_env(
        "post-tool-use",
        &read,
        &[
            ("VELRA_TEST_WATCHDOG_MS", "300"),
            ("VELRA_TEST_STALL_MS", "3000"),
        ],
    )
    .assert_contract();
    let errors = std::fs::read_to_string(env.home.join("logs/errors.log")).unwrap_or_default();
    assert!(
        errors.contains("deadline (300 ms) reached in reading its input with no event armed"),
        "{errors}"
    );
}
