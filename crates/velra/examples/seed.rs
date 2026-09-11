//! Seeds a Velra database with synthetic events for benchmarking (H1).
//!
//!     cargo run --release -p velra --example seed -- <velra_home> <events> [project_root]
//!
//! Development tool only; it is not part of the shipped binary.

use std::path::{Path, PathBuf};
use velra_core::db::{Db, Role};
use velra_core::event::{dedupe_key, NewEvent, Payload, ProjectInfo};
use velra_core::eventlog::insert_event;
use velra_core::{hash, paths, reducer};

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(
        args.next()
            .expect("usage: seed <velra_home> <events> [project_root]"),
    );
    let count: usize = args
        .next()
        .map(|n| n.parse().expect("event count"))
        .unwrap_or(100_000);
    let project = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("project"));

    std::fs::create_dir_all(&home).expect("create velra home");
    std::fs::create_dir_all(project.join("src")).expect("create project");
    for i in 0..8 {
        let _ = std::fs::write(
            project.join("src").join(format!("mod{i}.rs")),
            format!("// module {i}\nfn f() {{}}\n"),
        );
    }

    let root_str = paths::normalize_abs(&project.to_string_lossy());
    let project_id = hash::hex_prefix(paths::identity(&root_str).as_bytes(), 16);
    let info = ProjectInfo {
        project_id: project_id.clone(),
        root_path: root_str,
        is_git: false,
    };

    let mut db = Db::open(&home.join("velra.db"), Role::Cli).expect("open database");
    let started = std::time::Instant::now();
    let base_ms = velra_core::time::now_ms() - (count as i64) * 10;

    let tx = db.conn.transaction().expect("begin");
    for i in 0..count {
        let ts = base_ms + (i as i64) * 10;
        let session = format!("bench-session-{}", i % 20);
        let ev = synthetic(i, &session, &project_id, ts, &project, &info);
        insert_event(&tx, &ev).expect("insert");
    }
    tx.commit().expect("commit");

    // Leave a realistic amount of unreduced tail work behind (§4 measures a
    // populated database, not a freshly reduced one).
    let mut opts = reducer::ReduceOptions {
        batch: 500,
        ..Default::default()
    };
    opts.spool_dir = None;
    reducer::reduce(&mut db.conn, &opts).expect("reduce");

    let events: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap_or(0);
    let bytes = std::fs::metadata(home.join("velra.db"))
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "seeded {events} events ({:.1} MiB) in {:.1}s",
        bytes as f64 / (1024.0 * 1024.0),
        started.elapsed().as_secs_f64()
    );
}

fn synthetic(
    i: usize,
    session: &str,
    project_id: &str,
    ts: i64,
    project: &Path,
    info: &ProjectInfo,
) -> NewEvent {
    let path = format!("src/mod{}.rs", i % 8);
    let abs = project.join(&path);
    let (hash_value, size) = hash::hash_file(&abs);
    let (hook_event, tool, payload) = match i % 10 {
        0 => (
            "UserPromptSubmit",
            None,
            Payload {
                prompt: Some(format!("make change number {i} and keep the tests green")),
                prompt_id: Some(format!("p{i}")),
                ..Default::default()
            },
        ),
        1 | 2 => (
            "PreToolUse",
            Some("Edit"),
            Payload {
                path: Some(path.clone()),
                pre_hash: Some(hash_value.clone()),
                size: Some(size),
                ..Default::default()
            },
        ),
        3 | 4 => (
            "PostToolUse",
            Some("Edit"),
            Payload {
                path: Some(path.clone()),
                post_hash: Some(hash_value.clone()),
                size: Some(size),
                lines_added: Some(2),
                lines_removed: Some(1),
                excerpt: Some("- old line\n+ new line".into()),
                ..Default::default()
            },
        ),
        5 | 6 => (
            "PostToolUse",
            Some("Read"),
            Payload {
                path: Some(path.clone()),
                ..Default::default()
            },
        ),
        7 => (
            "PostToolUse",
            Some("Bash"),
            Payload {
                command: Some("cargo test".into()),
                exit_code: Some(0),
                stdout_tail: Some("test result: ok. 12 passed; 0 failed".into()),
                ..Default::default()
            },
        ),
        8 => (
            "Stop",
            None,
            Payload {
                stop_hook_active: Some(false),
                ..Default::default()
            },
        ),
        _ => (
            "PostToolUse",
            Some("Grep"),
            Payload {
                pattern: Some("TODO".into()),
                path: Some("src".into()),
                ..Default::default()
            },
        ),
    };
    let tool_use_id = tool.map(|_| format!("toolu_seed_{i}"));
    NewEvent {
        dedupe_key: dedupe_key(hook_event, session, tool_use_id.as_deref(), None, ts, None),
        session_id: session.to_string(),
        project_id: project_id.to_string(),
        agent_id: None,
        hook_event: hook_event.to_string(),
        tool_name: tool.map(str::to_string),
        tool_use_id,
        ts_ms: ts,
        payload: payload.to_json(),
        project: Some(info.clone()),
    }
}
