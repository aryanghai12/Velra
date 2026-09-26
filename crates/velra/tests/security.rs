//! J1–J3: redaction, sensitive paths and the dependency audit (§19).

mod common;

use common::{Env, Log};
use serde_json::json;
use velra_core::redact::redact;

/// Realistic secrets that must never be stored (≥ 50).
fn secret_corpus() -> Vec<String> {
    let mut out = vec![
        "AKIAIOSFODNN7EXAMPLE".to_string(),
        "ASIAY34FZKBOKMUTVV7A".to_string(),
        "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        "AWS_SECRET_ACCESS_KEY=\"je7MtGbClwBF/2Zp9Utk/h3yCo8nvbEXAMPLEKEY\"".to_string(),
        "secret_access_key: abcdefghijklmnopqrstuvwxyz0123456789ABCD".to_string(),
        "ghp_16C7e42F292c6912E7710c838347Ae178B4a".to_string(),
        "gho_16C7e42F292c6912E7710c838347Ae178B4aXX".to_string(),
        "ghu_16C7e42F292c6912E7710c838347Ae178B4aYY".to_string(),
        "ghs_16C7e42F292c6912E7710c838347Ae178B4aZZ".to_string(),
        "ghr_16C7e42F292c6912E7710c838347Ae178B4aWW".to_string(),
        "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyzABCDEF".to_string(),
        "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        "sk-proj-abcdefghijklmnopqrstuvwxyz0123".to_string(),
        "sk-abcdefghijklmnopqrstuvwxyz012345".to_string(),
        "xoxb-123456789012-1234567890123-abcdefghijklmnopqrstuvwx".to_string(),
        "xoxp-123456789012-123456789012-abcdefghijklmnop".to_string(),
        "xoxa-2-123456789012-abcdefghijklmnop".to_string(),
        "xoxr-123456789012-abcdefghijklmnop".to_string(),
        "AIzaSyA1234567890abcdefghijklmnopqrstuvw".to_string(),
        "sk_live_4eC39HqLyjWDarjtT1zdp7dc".to_string(),
        "rk_live_51H8xAbCdEfGhIjKlMnOpQrSt".to_string(),
        "sk_test_4eC39HqLyjWDarjtT1zdp7dc".to_string(),
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U".to_string(),
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA1234\n-----END RSA PRIVATE KEY-----".to_string(),
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n-----END OPENSSH PRIVATE KEY-----".to_string(),
        "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIB\n-----END EC PRIVATE KEY-----".to_string(),
        "postgres://admin:hunter2hunter@db.internal:5432/app".to_string(),
        "https://user:s3cr3tpassword@example.com/repo.git".to_string(),
        "mongodb://root:verysecretvalue@10.0.0.5:27017".to_string(),
        "redis://default:abcd1234efgh@cache:6379".to_string(),
        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        "authorization: bearer eyJabcdefghijklmnopqrstuvwxyz123456".to_string(),
    ];
    // Generic assignments across common spellings.
    for key in [
        "api_key", "apiKey", "API-KEY", "secret", "token", "password", "passwd", "auth",
    ] {
        for value in ["s3cr3tvalue123", "abcdefgh12345678", "Zm9vYmFyYmF6cXV4"] {
            out.push(format!("{key}={value}"));
            out.push(format!("\"{key}\": \"{value}\""));
        }
    }
    out
}

/// Benign strings that must survive untouched (≥ 50).
fn benign_corpus() -> Vec<String> {
    let mut out: Vec<String> = [
        "fn main() { println!(\"hello\"); }",
        "let tokens = tokenize(input);",
        "the author: someone famous",
        "keyboard shortcuts are documented in docs/keys.md",
        "task-runner-configuration-file-v2",
        "https://github.com/org/repo",
        "git clone https://github.com/org/repo.git",
        "npm install --save-dev typescript",
        "cargo test --workspace --all-features",
        "SELECT * FROM users WHERE id = 1",
        "export PATH=\"$HOME/.local/bin:$PATH\"",
        "test result: ok. 42 passed; 0 failed; 0 ignored",
        "error[E0308]: mismatched types",
        "src/auth/session.py:88: AssertionError",
        "The secret sauce is documentation.",
        "tokenizer.encode(text)",
        "authentication middleware",
        "password reset flow",
        "config.toml has a budget_tokens option",
        "warning: unused variable: `key`",
        "Authorization header is required",
        "risk-assessment-2026",
        "disk usage is 42%",
        "basketball",
        "sk",
        "let key = map.keys().next();",
        "docker compose up -d",
        "rustup toolchain install stable",
        "kubectl get pods -n default",
        "brew install ripgrep",
        "-- a/src/main.rs",
        "+++ b/src/main.rs",
        "@@ -1,4 +1,4 @@",
        "def add(a, b): return a + b",
        "class TokenBucket:",
        "interface AuthProvider {",
        "README.md updated",
        "2026-09-12T10:04:05Z",
        "0123456789abcdef",
        "http://localhost:3000/api/health",
        "assert_eq!(left, right)",
        "velra enable --dry-run",
        "the token bucket refills every second",
        "password strength meter",
        "auth = true",
        "key: value",
        "secret: yes",
        "token: abc",
        "COUNT(*) FROM events",
        "pytest -k \"auth and not slow\"",
        "eyJ is the start of a base64 JSON object",
        "AIza is a Google key prefix",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    out.extend((0..8).map(|i| format!("src/module{i}/handler.rs:{}: warning", i * 7)));
    out
}

#[test]
fn j1_every_secret_is_redacted() {
    let mut missed = Vec::new();
    for secret in secret_corpus() {
        let text = format!("leading context {secret} trailing context");
        let out = redact(&text);
        if !out.contains("[REDACTED:") {
            missed.push(secret);
        }
    }
    assert!(
        missed.is_empty(),
        "{} secrets not redacted: {missed:#?}",
        missed.len()
    );
}

#[test]
fn j1_benign_text_has_few_false_positives() {
    let corpus = benign_corpus();
    let flagged: Vec<&String> = corpus
        .iter()
        .filter(|s| redact(s).contains("[REDACTED:"))
        .collect();
    let rate = flagged.len() as f64 / corpus.len() as f64;
    assert!(
        rate <= 0.05,
        "false positive rate {:.1}% ({flagged:#?})",
        rate * 100.0
    );
}

#[test]
fn j1_secrets_never_reach_the_database_or_the_capsule() {
    let env = Env::new();
    env.write_file("src/config.py", "TOKEN = \"old\"\n");
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
    env.hook("user-prompt-submit", &{
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!(format!(
            "set the anthropic key to {secret} and run the tests"
        ));
        p
    })
    .assert_contract();
    env.hook("post-tool-use-failure", &{
        let mut p = env.base_payload("PostToolUseFailure");
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!("t1");
        p["tool_input"] =
            json!({ "command": format!("curl -H 'Authorization: Bearer {secret}' https://api") });
        p["error"] = json!(format!(
            "Exit code 1\nunauthorized for key {secret}\nAKIAIOSFODNN7EXAMPLE also leaked"
        ));
        p
    })
    .assert_contract();

    let all = env
        .drain_and_load_events(2)
        .iter()
        .map(|e| e.payload.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !all.contains(secret),
        "the api key must not be stored: {all}"
    );
    assert!(
        !all.contains("AKIAIOSFODNN7EXAMPLE"),
        "the aws key must not be stored"
    );
    assert!(all.contains("[REDACTED:"), "redaction markers are present");

    let out = env.cmd().arg("inspect").output().expect("inspect");
    let capsule = String::from_utf8_lossy(&out.stdout);
    assert!(
        !capsule.contains(secret),
        "the capsule must not leak it:\n{capsule}"
    );
}

#[test]
fn j2_sensitive_paths_store_only_the_path_and_hash() {
    let mut log = Log::new();
    log.env.write_file(".env", "API_KEY=old\n");
    log.prompt("rotate the api key in the environment file");
    log.edit(".env", "API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz\n");
    log.reduce();

    let excerpts: Vec<Option<String>> = log
        .db
        .conn
        .prepare("SELECT excerpt FROM edits")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .flatten()
        .collect();
    // The harness supplies an excerpt directly; the hook path is what enforces
    // the rule, so assert on the hook path too.
    let env = Env::new();
    let dotenv = env.project.join(".env");
    std::fs::write(&dotenv, "API_KEY=old\n").expect("write .env");
    env.hook("post-tool-use", &{
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!("t1");
        p["tool_input"] = json!({
            "file_path": dotenv,
            "old_string": "API_KEY=old",
            "new_string": "API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz"
        });
        p["tool_response"] = json!({ "filePath": dotenv, "originalFile": "API_KEY=old\n" });
        p
    })
    .assert_contract();

    let events = env.drain_and_load_events(1);
    let event = events
        .iter()
        .find(|e| e.tool_name.as_deref() == Some("Edit"))
        .unwrap_or_else(|| panic!("no Edit event: {events:#?}"));
    let payload = &event.payload;
    let value = event.json();
    assert_eq!(value["path"], json!(".env"));
    assert!(value["post_hash"].is_string(), "the hash is recorded");
    assert!(
        value.get("excerpt").is_none(),
        "no excerpt for a sensitive path: {payload}"
    );
    assert!(
        !payload.contains("sk-ant"),
        "no secret material at all: {payload}"
    );
    let _ = excerpts;
}

#[test]
fn j3_no_network_capable_crate_is_linked() {
    let lock = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
    )
    .expect("Cargo.lock");
    const DENY: [&str; 18] = [
        "reqwest",
        "hyper",
        "hyper-util",
        "h2",
        "curl",
        "curl-sys",
        "ureq",
        "isahc",
        "surf",
        "attohttpc",
        "minreq",
        "native-tls",
        "openssl",
        "rustls",
        "tokio",
        "async-std",
        "quinn",
        "trust-dns-resolver",
    ];
    let names: Vec<&str> = lock
        .lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .filter_map(|l| l.strip_suffix('"'))
        .collect();
    let found: Vec<&str> = DENY.iter().copied().filter(|d| names.contains(d)).collect();
    assert!(
        found.is_empty(),
        "network-capable crates in the dependency graph: {found:?}"
    );
}

#[test]
fn state_files_are_private_on_posix() {
    let env = Env::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The harness created $VELRA_HOME with `create_dir_all`, so it carries
        // the runner's umask (0755 under the usual 022). Pin it wide on purpose:
        // what follows then measures velra's own tightening rather than whatever
        // umask the runner happened to have.
        std::fs::set_permissions(&env.home, std::fs::Permissions::from_mode(0o755))
            .expect("widen home");
    }
    env.hook("stop", &env.base_payload("Stop"))
        .assert_contract();
    assert!(env.db_path().exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| {
            std::fs::metadata(p)
                .unwrap_or_else(|e| panic!("metadata for {}: {e}", p.display()))
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&env.home), 0o700, "home is 0700");
        assert_eq!(mode(&env.db_path()), 0o600, "velra.db is 0600");
        // SQLite takes these from the database file's own mode.
        for suffix in ["-wal", "-shm"] {
            let side = env.home.join(format!("velra.db{suffix}"));
            if side.exists() {
                assert_eq!(mode(&side), 0o600, "velra.db{suffix} is 0600");
            }
        }
        let spool = env.spool_dir();
        if spool.is_dir() {
            assert_eq!(mode(&spool), 0o700, "spool is 0700");
        }
    }
}

// ------------------------------------------------ Phase 9: every surface

/// The distinctive part of each planted secret. A surface that holds any of
/// these holds a secret.
const CANARIES: &[&str] = &[
    "CanaryPrompt0001",
    "CANARYKEYBODYCANARYKEYBODY",
    "CanaryPromptFlag01",
    "CanaryExcerpt0001",
    "CanaryPw0001",
    "Q2FuYXJ5QmFzaWNBdXRo",
    "CanaryStdoutTail0001",
    "CanaryStdoutCut0001",
    "CANARYCUTKEYBODY",
    "CanaryStderr0001",
    "CanaryBearer000000000001",
    "CanaryGrep0000000000001",
    "CANARY0000000001",
    "CanarySpool00000000000000000000000001",
    "CanaryTitle0001",
];

/// Every file under `dir`, recursively, with its bytes.
fn files_under(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
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

fn leaked_in(bytes: &[u8]) -> Vec<&'static str> {
    let text = String::from_utf8_lossy(bytes);
    CANARIES
        .iter()
        .copied()
        .filter(|c| text.contains(c))
        .collect()
}

/// REDACTION BEFORE PERSISTENCE, end to end.
///
/// Secrets are planted in every text field a hook accepts -- the prompt, a
/// private key in it, an edit's strings, a shell command's line, stdout
/// longer than the stored tail, stderr, a failure's error, a search
/// pattern, a compaction summary, the transcript a title is read from --
/// and one event is forced through the spool by holding the write lock.
/// Then every byte Velra left on disk (database, WAL, spool, logs, staged
/// capsule, checkpoints) and everything each command prints (inspect in
/// every form, status, doctor, restore list / dry run / stage, the capsule a
/// new session is handed) is searched for them.
///
/// Paths are not in scope: a path is a file's identity, stored as given,
/// and a secret spelled in one is persisted (see the Phase 9 report).
#[test]
fn a_planted_secret_reaches_no_persisted_or_printed_surface() {
    let env = Env::new();
    env.write_file("src/app.py", "api_key = load()\n");
    let debug = [("VELRA_LOG", "debug"), ("VELRA_CLAUDE_VERSION", "2.1.269")];
    let hook = |event: &str, p: &serde_json::Value| {
        let out = env.hook_with_env(event, p, &debug);
        out.assert_contract();
        out
    };
    let transcript = env.dir.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"ai-title\",\"aiTitle\":\"rotate token=CanaryTitle0001xyz for the api\"}\n",
    )
    .unwrap();

    let mut p = env.base_payload("SessionStart");
    p["source"] = json!("startup");
    hook("session-start", &p);

    let key = format!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----",
        "CANARYKEYBODYCANARYKEYBODY0123456789".repeat(3)
    );
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!(format!(
        "Fix the api client in src/app.py. The token is ghp_CanaryPrompt0001abcdefghijklmnopqrstu \
         and the deploy key is below; do not commit it.\n{key}\n\
         Run it with `deploy --password CanaryPromptFlag01`."
    ));
    hook("user-prompt-submit", &p);

    let file = env
        .project
        .join("src/app.py")
        .to_string_lossy()
        .into_owned();
    let edit = json!({
        "file_path": file,
        "old_string": "api_key = load()",
        "new_string": "api_key = \"CanaryExcerpt0001abcdef\"",
    });
    let mut p = env.base_payload("PreToolUse");
    p["tool_name"] = json!("Edit");
    p["tool_use_id"] = json!("e1");
    p["tool_input"] = edit.clone();
    hook("pre-tool-use", &p);
    env.write_file("src/app.py", "api_key = \"CanaryExcerpt0001abcdef\"\n");
    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = json!("Edit");
    p["tool_use_id"] = json!("e1");
    p["tool_input"] = edit;
    p["tool_response"] = json!({"filePath": file, "originalFile": "api_key = load()\n"});
    hook("post-tool-use", &p);

    // A command line with credentials, and output longer than the stored
    // tail with secrets on both sides of where it is cut.
    let command = "curl -u admin:CanaryPw0001 -H 'Authorization: Basic Q2FuYXJ5QmFzaWNBdXRoMDE=' \
                   https://api.local/v1 && git restore src/app.py";
    let cut_key = format!(
        "-----BEGIN RSA PRIVATE KEY-----\n{}-----END RSA PRIVATE KEY-----\n",
        "CANARYCUTKEYBODYCANARYCUTKEYBODY0123456789abcdefghijklmnopqrstu\n".repeat(40)
    );
    let stdout = format!(
        "{}\ncut here: ghp_CanaryStdoutCut0001abcdefghijklmnopqrstu\n{cut_key}{}\nfinal token=CanaryStdoutTail0001\n",
        "progress line\n".repeat(700),
        "more output\n".repeat(400)
    );
    assert!(stdout.len() > 2 * velra_core::event::limits::OUTPUT_TAIL);
    let mut p = env.base_payload("PreToolUse");
    p["tool_name"] = json!("Bash");
    p["tool_use_id"] = json!("b1");
    p["tool_input"] = json!({"command": command});
    hook("pre-tool-use", &p);
    env.write_file("src/app.py", "api_key = load()\n");
    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = json!("Bash");
    p["tool_use_id"] = json!("b1");
    p["tool_input"] = json!({"command": command});
    p["tool_response"] = json!({
        "stdout": stdout,
        "stderr": "warning: token=CanaryStderr0001 is about to expire",
        "interrupted": false,
        "exitCode": 0,
    });
    hook("post-tool-use", &p);

    let mut p = env.base_payload("PostToolUseFailure");
    p["tool_name"] = json!("Bash");
    p["tool_use_id"] = json!("b2");
    p["tool_input"] = json!({"command": "python -m pytest tests/test_api.py"});
    p["error"] = json!(
        "Exit code 1\nrequest failed: Authorization: Bearer CanaryBearer000000000001\nFAILED tests/test_api.py::test_auth"
    );
    hook("post-tool-use-failure", &p);

    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = json!("Grep");
    p["tool_use_id"] = json!("g1");
    p["tool_input"] = json!({"pattern": "sk-ant-api03-CanaryGrep0000000000001", "path": "src"});
    hook("post-tool-use", &p);

    let mut p = env.base_payload("Stop");
    p["stop_hook_active"] = json!(false);
    hook("stop", &p);
    let mut p = env.base_payload("PreCompact");
    p["trigger"] = json!("manual");
    p["custom_instructions"] = json!("");
    hook("pre-compact", &p);
    let mut p = env.base_payload("PostCompact");
    p["trigger"] = json!("manual");
    p["compact_summary"] = json!("The user rotated AKIACANARY0000000001 and fixed the client.");
    hook("post-compact", &p);

    // One event forced through the spool: the write lock is held, so the
    // hook's append gives up and the event is written to a spool file.
    let blocker = env.open_db();
    blocker.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = json!("Bash");
    p["tool_use_id"] = json!("b3");
    p["tool_input"] = json!({"command": "echo ok"});
    p["tool_response"] = json!({
        "stdout": "export GH=ghp_CanarySpool00000000000000000000000001",
        "stderr": "",
        "interrupted": false,
        "exitCode": 0,
    });
    hook("post-tool-use", &p);
    let mut spooled = Vec::new();
    files_under(&env.spool_dir(), &mut spooled);
    assert!(
        !spooled.is_empty(),
        "the event must have gone to the spool, or this part tests nothing"
    );
    // Every leak is collected and reported together, so a failure shows the
    // whole of what escaped rather than its first instance.
    let mut leaks: Vec<String> = Vec::new();
    for (path, bytes) in &spooled {
        for c in leaked_in(bytes) {
            leaks.push(format!("spool {}: {c}", path.display()));
        }
    }
    blocker.conn.execute_batch("ROLLBACK").unwrap();
    drop(blocker);
    env.drain();

    // Every command a user or a script reads.
    let mut printed: Vec<(String, std::process::Output)> = Vec::new();
    let mut run = |args: &[&str]| {
        let out = env
            .cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.269")
            .args(args)
            .output()
            .expect("run velra");
        printed.push((args.join(" "), out.clone()));
        out
    };
    let checkpoint = {
        let db = env.open_db();
        db.conn
            .query_row(
                "SELECT checkpoint_id FROM checkpoints ORDER BY created_ms DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .expect("pre-compact wrote a checkpoint")
    };
    for args in [
        vec!["inspect"],
        vec!["inspect", "--json"],
        vec!["inspect", "--checkpoint", &checkpoint],
        vec!["inspect", "--checkpoint", &checkpoint, "--json"],
        vec!["inspect", "--trace", "src/app.py"],
        vec!["inspect", "--trace", "src/app.py", "--json"],
        vec!["status"],
        vec!["status", "--json"],
        vec!["doctor"],
        vec!["doctor", "--json"],
        vec!["restore", "--list"],
        vec!["restore", "--list", "--json"],
        vec!["restore", "--session", &env.session, "--dry-run"],
        vec!["restore", "--session", &env.session, "--dry-run", "--json"],
        vec!["restore", "--session", &env.session],
    ] {
        run(&args);
    }
    for section in ["dead-ends", "failure", "files", "attempts"] {
        run(&["inspect", "--section", section]);
        run(&["inspect", "--section", section, "--json"]);
    }
    let listing = printed
        .iter()
        .find(|(a, _)| a == "restore --list")
        .map(|(_, o)| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap();
    assert!(
        listing.contains("[REDACTED:"),
        "the title is read, and redacted: {listing}"
    );
    // The staged capsule, handed to a brand-new session.
    let mut p = env.base_payload("SessionStart");
    p["session_id"] = json!("next-session");
    p["source"] = json!("startup");
    let delivered = env.hook_with_env("session-start", &p, &debug);
    delivered.assert_contract();
    assert!(
        delivered.stdout.contains("VELRA_WORKSPACE_STATE"),
        "the staged capsule was delivered: {}",
        delivered.stdout
    );
    for (what, out) in &printed {
        for (stream, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
            for c in leaked_in(bytes) {
                leaks.push(format!("`velra {what}` {stream}: {c}"));
            }
        }
    }
    for c in leaked_in(delivered.stdout.as_bytes()) {
        leaks.push(format!("delivered capsule: {c}"));
    }

    // Every byte left on disk, the database's own pages and WAL included.
    env.drain();
    let mut files = Vec::new();
    files_under(&env.home, &mut files);
    assert!(files.iter().any(|(p, _)| p.ends_with("velra.db")));
    assert!(
        files
            .iter()
            .any(|(p, _)| p.to_string_lossy().contains("debug.log")),
        "debug logging was on, so its file is part of what is searched"
    );
    for (path, bytes) in &files {
        for c in leaked_in(bytes) {
            leaks.push(format!("{}: {c}", path.display()));
        }
    }
    assert!(
        leaks.is_empty(),
        "{} leaks:\n{}",
        leaks.len(),
        leaks.join("\n")
    );
    // And the secrets did reach Velra: the ledger holds their markers.
    let db = env.open_db();
    let payloads: String = db
        .conn
        .prepare("SELECT payload FROM events")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join("\n");
    for kind in [
        "github_token",
        "private_key",
        "cli_credential",
        "basic_auth",
        "generic_secret",
        "bearer_token",
        "api_key",
        "aws_access_key",
    ] {
        assert!(
            payloads.contains(&format!("[REDACTED:{kind}]")),
            "no {kind} marker in the ledger:\n{payloads}"
        );
    }
}
