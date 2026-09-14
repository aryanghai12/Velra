//! C1–C5: storage, concurrency and recovery (§10, §18).

mod common;

use common::Env;
use serde_json::json;
use velra_core::db::{self, Db, DbError, Role};
use velra_core::{reducer, spool};

/// Scaled down by default; CI sets `VELRA_STRESS=1` for the full §21 C1 shape.
fn stress_shape() -> (usize, usize) {
    if std::env::var("VELRA_STRESS").is_ok() {
        (32, 500)
    } else {
        (8, 25)
    }
}

fn tool_payload(env: &Env, index: usize) -> String {
    json!({
        "session_id": env.session,
        "hook_event_name": "PostToolUse",
        "cwd": env.project.to_string_lossy(),
        "tool_name": "Read",
        "tool_use_id": format!("toolu_{index}"),
        "tool_input": { "file_path": env.project.join("src/a.rs") },
        "tool_response": { "ok": true }
    })
    .to_string()
}

#[test]
fn c1_parallel_processes_lose_no_events_and_create_no_duplicates() {
    let (processes, per_process) = stress_shape();
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");

    let mut handles = Vec::new();
    for p in 0..processes {
        let home = env.home.clone();
        let config = env.config.clone();
        let project = env.project.clone();
        let session = env.session.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..per_process {
                let index = p * 10_000 + i;
                let payload = json!({
                    "session_id": session,
                    "hook_event_name": "PostToolUse",
                    "cwd": project.to_string_lossy(),
                    "tool_name": "Read",
                    "tool_use_id": format!("toolu_{index}"),
                    "tool_input": { "file_path": project.join("src/a.rs") },
                    "tool_response": { "ok": true }
                })
                .to_string();
                let out = assert_cmd::Command::cargo_bin("velra")
                    .expect("binary")
                    .env("VELRA_HOME", &home)
                    .env("CLAUDE_CONFIG_DIR", &config)
                    .env("CLAUDE_PROJECT_DIR", &project)
                    .env("TZ", "UTC")
                    // The 250 ms watchdog is not under test here, and this
                    // storm saturates the machine enough to trip it.
                    .env("VELRA_TEST_WATCHDOG_MS", "60000")
                    .args(["hook", "post-tool-use"])
                    .write_stdin(payload)
                    .output()
                    .expect("run hook");
                assert!(out.status.success(), "hook exited {:?}", out.status.code());
                assert!(out.stderr.is_empty());
            }
        }));
    }
    for h in handles {
        h.join().expect("thread");
    }

    // Everything that fell back to the spool is ingested by the reduce pass
    // the helper runs; the retry covers a spool file still landing.
    let expected = (processes * per_process) as i64;
    let (events, distinct) = env.drain_and_query(
        |(events, _): &(i64, i64)| *events >= expected,
        |db| {
            (
                read_count(db, "SELECT COUNT(*) FROM events WHERE tool_name = 'Read'"),
                read_count(
                    db,
                    "SELECT COUNT(DISTINCT dedupe_key) FROM events WHERE tool_name = 'Read'",
                ),
            )
        },
    );
    assert_eq!(events, expected, "no events lost");
    assert_eq!(distinct, expected, "no duplicates");
    assert_eq!(spool::backlog(&env.spool_dir()), 0, "spool drained");
}

#[test]
fn c2_a_held_write_lock_sends_events_to_the_spool() {
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    // Create the database first so the hook does not have to migrate.
    drop(env.open_db());

    let blocker = Db::open(&env.db_path(), Role::Cli).expect("open");
    blocker.conn.execute_batch("BEGIN EXCLUSIVE").expect("lock");

    let started = std::time::Instant::now();
    for i in 0..5 {
        let out = env.hook_raw("post-tool-use", tool_payload(&env, i).as_bytes());
        out.assert_contract();
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < 5_000,
        "hooks must not wait on the lock: {elapsed:?}"
    );
    assert!(
        spool::backlog(&env.spool_dir()) > 0,
        "events land in the spool while locked"
    );

    blocker.conn.execute_batch("COMMIT").expect("unlock");
    drop(blocker);

    let events = env.drain_and_query(
        |n: &i64| *n >= 5,
        |db| read_count(db, "SELECT COUNT(*) FROM events WHERE tool_name = 'Read'"),
    );
    assert_eq!(events, 5, "spooled events are ingested later");
    assert_eq!(spool::backlog(&env.spool_dir()), 0);
}

/// Derived state after interrupted reducer runs equals a clean replay (C3).
#[test]
fn c3_interrupted_reduce_replays_identically() {
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    // Build a realistic event log through the hook path.
    for i in 0..40 {
        let file = env.project.join("src/a.rs");
        let payload = json!({
            "session_id": env.session,
            "hook_event_name": "PostToolUse",
            "cwd": env.project.to_string_lossy(),
            "tool_name": "Edit",
            "tool_use_id": format!("t{i}"),
            "tool_input": { "file_path": file, "old_string": format!("v{i}"), "new_string": format!("v{}", i + 1) },
            "tool_response": { "filePath": file, "originalFile": format!("v{i}\n") }
        });
        env.write_file("src/a.rs", &format!("v{}\n", i + 1));
        env.hook("post-tool-use", &payload).assert_contract();
    }

    // Kill reduce repeatedly while it works.
    for _ in 0..10 {
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
            .args(["reduce"])
            .env("VELRA_HOME", &env.home)
            .env("CLAUDE_PROJECT_DIR", &env.project)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn reduce");
        std::thread::sleep(std::time::Duration::from_micros(700));
        let _ = child.kill();
        let _ = child.wait();
    }
    // The helper reduces first, so the kills above cannot leave the log short.
    let interrupted = env.drain_and_query(|rows: &Vec<String>| !rows[0].is_empty(), derived_state);

    // Clean replay into a fresh database.
    let replay = Env::new();
    let mut fresh = replay.open_db();
    {
        let source = env.open_db();
        let mut stmt = source
            .conn
            .prepare("SELECT dedupe_key, session_id, project_id, agent_id, hook_event, tool_name, tool_use_id, ts_ms, payload FROM events ORDER BY id")
            .expect("prepare");
        let rows: Vec<velra_core::event::NewEvent> = stmt
            .query_map([], |r| {
                Ok(velra_core::event::NewEvent {
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
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        let project: (String, String) = source
            .conn
            .query_row(
                "SELECT project_id, root_path FROM projects LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("project");
        fresh
            .conn
            .execute(
                "INSERT INTO projects (project_id, root_path, is_git, created_ms) VALUES (?1, ?2, 0, 0)",
                rusqlite::params![project.0, project.1],
            )
            .expect("project row");
        for ev in rows {
            velra_core::eventlog::append(&mut fresh.conn, &ev).expect("append");
        }
    }
    reducer::reduce_all(&mut fresh.conn, None).expect("replay");
    assert_eq!(
        interrupted,
        derived_state(&fresh),
        "interrupted runs must equal a clean replay"
    );
}

fn read_count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).expect("count")
}

fn derived_state(db: &Db) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT path || '|' || status || '|' || COALESCE(mechanism, '-') FROM edits ORDER BY event_id",
        "SELECT path || '|' || mechanism || '|' || reapplied FROM dead_ends ORDER BY id",
        "SELECT kind || '|' || outcome || '|' || command_text FROM commands ORDER BY event_id",
        "SELECT level || '|' || text FROM intents ORDER BY id",
        "SELECT path || '|' || edits || '|' || reads FROM file_stats ORDER BY path",
        "SELECT path || '|' || source || '|' || content_hash FROM file_versions ORDER BY id",
    ] {
        let mut stmt = db.conn.prepare(sql).expect("prepare");
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .flatten()
            .collect();
        out.push(rows.join("\n"));
    }
    out
}

#[test]
fn c4_a_corrupt_database_is_rotated_aside_and_recreated() {
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    std::fs::create_dir_all(&env.home).expect("home");
    std::fs::write(env.db_path(), vec![0x42u8; 16 * 1024]).expect("corrupt file");

    env.hook_raw("post-tool-use", tool_payload(&env, 1).as_bytes())
        .assert_contract();

    let rotated: Vec<String> = std::fs::read_dir(&env.home)
        .expect("home")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".corrupt-"))
        .collect();
    assert_eq!(
        rotated.len(),
        1,
        "the corrupt file is kept aside: {rotated:?}"
    );

    let db = env.open_db();
    assert_eq!(
        db::user_version(&db.conn).expect("version"),
        db::SCHEMA_VERSION
    );
    drop(db);
    env.assert_event_count(1);

    let out = env.cmd().arg("doctor").output().expect("doctor");
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("corrupt"), "doctor reports it: {report}");
}

#[test]
fn c5_a_newer_schema_makes_hooks_no_ops() {
    let env = Env::new();
    {
        let db = env.open_db();
        db.conn
            .execute_batch("PRAGMA user_version = 99")
            .expect("bump");
    }
    let out = env.hook_raw("post-tool-use", tool_payload(&env, 1).as_bytes());
    out.assert_contract();
    assert!(out.stdout.is_empty());

    let db = Db::open(&env.db_path(), Role::Cli);
    assert!(
        matches!(db, Err(DbError::NewerSchema(99))),
        "the database is left untouched"
    );

    let db = Db::open_readonly(&env.db_path()).expect("read only");
    let events: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .expect("count");
    assert_eq!(events, 0, "nothing was written");
    let errors = std::fs::read_to_string(env.home.join("logs/errors.log")).unwrap_or_default();
    assert!(
        errors.contains("newer than this binary"),
        "logged once: {errors}"
    );
}

#[test]
fn spooled_events_keep_their_original_timestamps_and_dedupe_keys() {
    let env = Env::new();
    let db = env.open_db();
    drop(db);
    let ev = velra_core::event::NewEvent {
        dedupe_key: "fixed-key".into(),
        session_id: env.session.clone(),
        project_id: env.project_id(),
        agent_id: None,
        hook_event: "Stop".into(),
        tool_name: None,
        tool_use_id: None,
        ts_ms: 1_234_567,
        payload: "{}".into(),
        project: Some(env.project_info()),
    };
    spool::write(&env.spool_dir(), &ev).expect("spool");
    spool::write(&env.spool_dir(), &ev).expect("spool duplicate");

    let mut db = env.open_db();
    let ingested = spool::ingest(&mut db.conn, &env.spool_dir(), 100).expect("ingest");
    assert_eq!(ingested, 2, "both files consumed");
    let rows: Vec<(String, i64)> = db
        .conn
        .prepare("SELECT dedupe_key, ts_ms FROM events")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(
        rows,
        vec![("fixed-key".to_string(), 1_234_567)],
        "deduplicated, original timestamp kept"
    );
}

/// C5: a database written by the previous schema is migrated forward in place,
/// carrying its rows with it.
///
/// `file_stats.first_touch_ms` arrived in schema v2. Nothing in the acceptance
/// suite exercises a real upgrade otherwise, and a migration that fails on a
/// populated database fails only in the field, where the damage is a hook that
/// silently stops recording.
#[test]
fn c6_a_v1_database_migrates_forward_with_its_rows() {
    let env = Env::new();
    // The parts of schema v1 this migration touches, in their v1 shape:
    // `meta`, which every migration stamps, and `file_stats`, which gains a
    // column.
    {
        let conn = rusqlite::Connection::open(env.db_path()).expect("create");
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
             INSERT INTO meta VALUES ('created_by', 'velra 0.1.0');
             CREATE TABLE file_stats (
               session_id TEXT NOT NULL, epoch INTEGER NOT NULL, path TEXT NOT NULL,
               reads INTEGER NOT NULL DEFAULT 0, edits INTEGER NOT NULL DEFAULT 0,
               in_failure INTEGER NOT NULL DEFAULT 0, last_touch_ms INTEGER NOT NULL,
               PRIMARY KEY (session_id, epoch, path)
             ) STRICT;
             INSERT INTO file_stats VALUES ('s1', 1, 'src/a.rs', 2, 1, 0, 1700000000000);
             PRAGMA user_version = 1;",
        )
        .expect("v1 schema");
    }

    let db = Db::open(&env.db_path(), Role::Cli).expect("migrate");
    assert_eq!(
        db::user_version(&db.conn).expect("version"),
        db::SCHEMA_VERSION,
        "the file is now at the current schema"
    );
    let (reads, first, last): (i64, i64, i64) = db
        .conn
        .query_row(
            "SELECT reads, first_touch_ms, last_touch_ms FROM file_stats WHERE path = 'src/a.rs'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row survives");
    assert_eq!(reads, 2, "existing counts are untouched");
    assert_eq!(
        first, last,
        "a row with no recorded first touch is seeded from its last touch"
    );
}
