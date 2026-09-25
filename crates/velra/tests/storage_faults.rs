//! Storage under interruption, contention and malformed input (Phase 8 of the
//! v0.1.2 hardening audit; DECISIONS D116-D119).
//!
//! Every test states the durable state it expects after the fault, not only
//! that nothing panicked: which rows exist, how many, and whether the reducer
//! can still move.

mod common;

use common::{Env, Log};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use velra_core::db::{self, Db, Role};
use velra_core::event::{NewEvent, Payload};
use velra_core::{eventlog, reducer, spool};

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).expect(sql)
}

fn age(path: &Path, by: Duration) {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open to age");
    f.set_modified(SystemTime::now() - by).expect("age");
}

fn event(env: &Env, key: &str, hook_event: &str, ts_ms: i64, payload: &str) -> NewEvent {
    NewEvent {
        dedupe_key: key.into(),
        session_id: env.session.clone(),
        project_id: env.project_id(),
        agent_id: None,
        hook_event: hook_event.into(),
        tool_name: None,
        tool_use_id: None,
        ts_ms,
        payload: payload.into(),
        project: Some(env.project_info()),
    }
}

fn prompt_payload(text: &str) -> String {
    Payload {
        prompt: Some(text.into()),
        ..Default::default()
    }
    .to_json()
}

/// Writes a spool file as `spool::write` names it, with raw contents.
fn raw_spool(env: &Env, name: &str, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(env.spool_dir()).unwrap();
    let path = env.spool_dir().join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn cursor(db: &Db) -> i64 {
    db::cursor(&db.conn).expect("cursor")
}

fn max_event_id(db: &Db) -> i64 {
    count(db, "SELECT COALESCE(MAX(id), 0) FROM events")
}

// ================================================ malformed spool input

/// D116. A spool file whose JSON parses, but whose `payload` string is not
/// JSON, was stored as an event. The reducer's ordering and scan queries run
/// `json_extract` over every event of a session, and SQLite raises "malformed
/// JSON" on it: every reduction failed from then on, for every session,
/// because the cursor is shared.
#[test]
fn a_spooled_event_with_a_malformed_payload_does_not_stop_the_reducer() {
    let mut log = Log::new();
    log.prompt("fix the login test and keep the session cookie");
    log.reduce();
    let base = log.ts;

    // The poisoned file, and a good spooled event of the same session behind
    // it: the good one is what makes the reducer read the session's payloads.
    let bad = event(&log.env, "bad-payload", "Stop", base - 500, "{not json");
    let text = serde_json::to_string(&bad).unwrap();
    raw_spool(
        &log.env,
        &format!("{}-1-aaaa.jsonl", base - 500),
        text.as_bytes(),
    );
    let good = event(
        &log.env,
        "good-late",
        "UserPromptSubmit",
        base - 400,
        &prompt_payload("also never delete the audit log"),
    );
    spool::write(&log.env.spool_dir(), &good).unwrap();

    // A later direct event of another session must still be reduced.
    log.switch_session("other-session");
    log.prompt("an unrelated task in another session");

    let result = reducer::reduce_all(&mut log.db.conn, Some(&log.env.spool_dir()));
    assert!(result.is_ok(), "the reducer stopped: {result:?}");
    assert_eq!(
        cursor(&log.db),
        max_event_id(&log.db),
        "the cursor reached the end"
    );
    assert_eq!(
        count(
            &log.db,
            "SELECT COUNT(*) FROM events WHERE dedupe_key = 'bad-payload'"
        ),
        0,
        "a payload that is not JSON is not an event"
    );
    assert_eq!(
        count(
            &log.db,
            "SELECT COUNT(*) FROM events WHERE dedupe_key = 'good-late'"
        ),
        1
    );
    assert_eq!(
        count(
            &log.db,
            "SELECT COUNT(*) FROM intents WHERE session_id = 'other-session'"
        ),
        1,
        "the other session was reduced"
    );
    // Kept for inspection, out of the ingestion path.
    assert!(log.env.spool_dir().join("bad").is_dir());
    assert_eq!(spool::backlog(&log.env.spool_dir()), 0);
}

/// D116. The same poisoned payload already in `events` -- written by a build
/// before ingestion checked it -- must not wedge the reducer either.
#[test]
fn a_stored_event_with_a_malformed_payload_does_not_stop_the_reducer() {
    let mut log = Log::new();
    log.prompt("fix the login test");
    log.reduce();
    let base = log.ts;
    let mut bad = event(&log.env, "bad-row", "PostToolUse", base + 10, "{not json");
    bad.tool_name = Some("Edit".into());
    eventlog::append(&mut log.db.conn, &bad).unwrap();
    // The Stop hook's scan reads unreduced edit events' paths.
    velra_core::reducer::scan_paths(&log.db.conn, &log.session(), 64).expect("scan paths");
    let result = reducer::reduce_all(&mut log.db.conn, None);
    assert!(result.is_ok(), "{result:?}");

    // A spooled event that opens a batch: the reducer reads the session back
    // from it, and the first row it reaches is the malformed one.
    let good = event(
        &log.env,
        "good-late",
        "UserPromptSubmit",
        base + 20,
        &prompt_payload("also never delete the audit log"),
    );
    spool::write(&log.env.spool_dir(), &good).unwrap();
    let result = reducer::reduce_all(&mut log.db.conn, Some(&log.env.spool_dir()));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(cursor(&log.db), max_event_id(&log.db));
    assert_eq!(
        count(
            &log.db,
            "SELECT COUNT(*) FROM events WHERE dedupe_key = 'good-late'"
        ),
        1
    );
}

/// D117. A spool file that is not UTF-8 was skipped by every ingestion and
/// never quarantined: it stayed in the spool, re-read by every reducer, and
/// counted in the backlog `doctor` reports, forever.
#[test]
fn a_spool_file_that_is_not_utf8_is_quarantined_once_old() {
    let env = Env::new();
    let mut db = env.open_db();
    let path = raw_spool(&env, "1-1-ffff.jsonl", &[0xff, 0xfe, 0x7b, 0x00, 0xc3]);
    // Young: left alone -- it may still be being written.
    assert_eq!(
        spool::ingest(&mut db.conn, &env.spool_dir(), 10).unwrap(),
        0
    );
    assert!(path.exists());
    age(&path, Duration::from_secs(60));
    assert_eq!(
        spool::ingest(&mut db.conn, &env.spool_dir(), 10).unwrap(),
        0
    );
    assert!(!path.exists(), "still in the ingestion path");
    assert!(env.spool_dir().join("bad").join("1-1-ffff.jsonl").exists());
    assert_eq!(spool::backlog(&env.spool_dir()), 0);
}

/// Mandatory boundary 3: a spool append that stopped part way (the process
/// died inside `write_all`). The partial file is never ingested as an event,
/// is left while it could still be completing, and is quarantined after;
/// complete files around it are ingested normally.
#[test]
fn a_partially_written_spool_file_is_never_ingested() {
    let env = Env::new();
    let mut db = env.open_db();
    let whole = event(&env, "whole", "Stop", 10, "{}");
    let line = serde_json::to_string(&whole).unwrap();
    let cut = raw_spool(&env, "5-1-aaaa.jsonl", &line.as_bytes()[..line.len() / 2]);
    let empty = raw_spool(&env, "6-1-bbbb.jsonl", b"");
    // Complete but without its final newline: a whole record.
    let mut done = whole.clone();
    done.dedupe_key = "no-newline".into();
    raw_spool(
        &env,
        "7-1-cccc.jsonl",
        serde_json::to_string(&done).unwrap().as_bytes(),
    );
    spool::write(&env.spool_dir(), &whole).unwrap();

    assert_eq!(
        spool::ingest(&mut db.conn, &env.spool_dir(), 10).unwrap(),
        2
    );
    assert!(cut.exists() && empty.exists(), "young partial files wait");
    for p in [&cut, &empty] {
        age(p, Duration::from_secs(60));
    }
    assert_eq!(
        spool::ingest(&mut db.conn, &env.spool_dir(), 10).unwrap(),
        0
    );
    assert_eq!(spool::backlog(&env.spool_dir()), 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 2);
}

// ============================================== interruption boundaries

/// Mandatory boundary 1: a write that never committed leaves nothing, and the
/// same event written again is stored once.
#[test]
fn an_uncommitted_event_write_leaves_nothing_behind() {
    let env = Env::new();
    let mut db = env.open_db();
    let ev = event(&env, "k1", "Stop", 1, "{}");
    {
        let tx = db.conn.transaction().unwrap();
        eventlog::insert_event(&tx, &ev).unwrap();
        // Dropped: the process died before COMMIT.
    }
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 0);
    assert_eq!(eventlog::append(&mut db.conn, &ev).unwrap(), Some(1));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 1);
}

/// Mandatory boundary 2: the commit landed but the process died before it
/// could disarm its pending event, so the watchdog spooled it as well. The
/// event is stored once.
#[test]
fn an_event_both_committed_and_spooled_is_stored_once() {
    let env = Env::new();
    let mut db = env.open_db();
    let ev = event(&env, "k1", "Stop", 1, "{}");
    eventlog::append(&mut db.conn, &ev).unwrap();
    spool::write(&env.spool_dir(), &ev).unwrap();
    reducer::reduce_all(&mut db.conn, Some(&env.spool_dir())).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 1);
    // The directly written row is the one kept: it was not spooled.
    let spooled: i64 = db
        .conn
        .query_row(
            "SELECT COALESCE(json_extract(payload, '$.spooled'), 0) FROM events",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(spooled, 0);
}

/// Mandatory boundary 4: spool replay interrupted before its commit (nothing
/// stored, files kept) and after it (stored, files not yet deleted). Replaying
/// again stores each event exactly once and folds to the same state as one
/// clean ingestion.
#[test]
fn spool_replay_interrupted_on_either_side_of_its_commit_is_idempotent() {
    let clean = spooled_session();
    let mut clean_db = clean.open_db();
    reducer::reduce_all(&mut clean_db.conn, Some(&clean.spool_dir())).unwrap();
    let expected = state_of(&clean_db);

    let env = spooled_session();
    let files: Vec<(PathBuf, Vec<u8>)> = spool::pending(&env.spool_dir())
        .into_iter()
        .map(|p| {
            let b = std::fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    let mut db = env.open_db();
    // Before the commit: the transaction is dropped.
    {
        let tx = db.conn.transaction().unwrap();
        for (p, _) in &files {
            let ev: NewEvent = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            eventlog::insert_event(&tx, &ev).unwrap();
        }
    }
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 0);
    // After the commit, before the deletions: every file comes back.
    reducer::reduce_all(&mut db.conn, Some(&env.spool_dir())).unwrap();
    for (p, b) in &files {
        std::fs::write(p, b).unwrap();
    }
    for _ in 0..3 {
        reducer::reduce_all(&mut db.conn, Some(&env.spool_dir())).unwrap();
    }
    assert_eq!(state_of(&db), expected);
    assert_eq!(spool::backlog(&env.spool_dir()), 0);
}

/// A session whose events all went to the spool, in no particular order.
fn spooled_session() -> Env {
    let env = Env::new();
    drop(env.open_db());
    let texts = [
        "fix the flaky logout test",
        "do not modify the tests",
        "try the retry decorator instead",
    ];
    for (i, t) in texts.iter().enumerate().rev() {
        let ev = event(
            &env,
            &format!("p{i}"),
            "UserPromptSubmit",
            common::BASE_MS + 1_000 * i as i64,
            &prompt_payload(t),
        );
        spool::write(&env.spool_dir(), &ev).unwrap();
    }
    env
}

/// The derived state a reduction produces, without row ids.
fn state_of(db: &Db) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT dedupe_key || '|' || ts_ms FROM events ORDER BY dedupe_key",
        "SELECT level || '|' || text || '|' || COALESCE(superseded_ms, -1) FROM intents ORDER BY level, text",
        "SELECT kind || '|' || text FROM constraints ORDER BY text",
        "SELECT session_id || '|' || epoch FROM sessions ORDER BY session_id",
        "SELECT 'cursor|' || (last_event_id = (SELECT MAX(id) FROM events)) FROM reducer_cursor",
    ] {
        let mut stmt = db.conn.prepare(sql).unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        out.extend(rows.map(|r| r.unwrap()));
    }
    out
}

/// Mandatory boundary 7: a checkpoint whose transaction never committed. No
/// checkpoint, no continuation, no compaction row, and the continuation that
/// was live is still live -- not superseded by a checkpoint that does not
/// exist.
#[test]
fn an_uncommitted_checkpoint_changes_nothing() {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("fix the flaky logout test");
    log.edit("src/a.rs", "v1\n");
    let first = log.checkpoint();
    let before = (
        count(&log.db, "SELECT COUNT(*) FROM checkpoints"),
        count(&log.db, "SELECT COUNT(*) FROM compactions"),
    );
    log.edit("src/a.rs", "v2\n");
    log.reduce();
    let session = log.session();
    {
        let tx = log.db.conn.transaction().unwrap();
        velra_core::checkpoint::create_in_tx(
            &tx,
            &velra_core::checkpoint::CheckpointRequest {
                session_id: &session,
                trigger: velra_core::model::Trigger::Manual,
                created_ms: log.ts + 1_000,
                partial: false,
                watermark: 0,
            },
            &Default::default(),
        )
        .unwrap()
        .expect("something to save");
        // Dropped before COMMIT.
    }
    assert_eq!(
        (
            count(&log.db, "SELECT COUNT(*) FROM checkpoints"),
            count(&log.db, "SELECT COUNT(*) FROM compactions"),
        ),
        before
    );
    let live = velra_core::continuation::live(&log.db.conn, &session)
        .unwrap()
        .expect("still live");
    assert_eq!(live.checkpoint_id, first);
}

/// Mandatory boundary 5: a reduction interrupted before its batch commits
/// (here by a panic at the commit seam) leaves cursor and projection exactly
/// as they were, and the next reduction reaches the state an uninterrupted
/// one does.
#[cfg(feature = "fault-injection")]
#[test]
fn a_reduction_interrupted_before_its_commit_leaves_no_partial_state() {
    let build = || {
        let mut log = Log::new();
        log.env.write_file("src/a.rs", "v0\n");
        log.prompt("fix the flaky logout test and do not modify the tests");
        for i in 1..6 {
            log.edit("src/a.rs", &format!("v{i}\n"));
            log.command_fail("pytest -x", 1, "FAILED tests/test_a.py::test_x");
        }
        log
    };
    let mut clean = build();
    clean.reduce();
    let expected = state_of(&clean.db);

    let mut log = build();
    let before = state_of(&log.db);
    velra_core::reducer::BEFORE_BATCH_COMMIT.with(|h| {
        *h.borrow_mut() = Some(Box::new(|| panic!("interrupted before commit")));
    });
    let spool_dir = log.env.spool_dir();
    let conn = &mut log.db.conn;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reducer::reduce_all(conn, Some(&spool_dir))
    }));
    assert!(r.is_err(), "the seam fired");
    assert_eq!(state_of(&log.db), before, "nothing of the batch is visible");
    assert_eq!(cursor(&log.db), 0);
    log.reduce();
    assert_eq!(state_of(&log.db), expected);
}

// ========================================================= schema / health

fn enabled_env() -> Env {
    let env = Env::new();
    std::fs::write(env.settings_path(), "{}\n").unwrap();
    let out = env
        .cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.280")
        .arg("enable")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    env
}

fn status_json(env: &Env) -> (i32, Value) {
    let out = env
        .cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.280")
        .args(["status", "--json"])
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        serde_json::from_slice(&out.stdout).expect("status json"),
    )
}

/// D118. With a database no hook can write to -- a newer schema, or a
/// migration that cannot complete -- `velra status` reported healthy (exit 0)
/// while every hook was a no-op.
#[test]
fn status_is_not_healthy_when_hooks_cannot_use_the_database() {
    let env = enabled_env();
    drop(env.open_db());
    let (code, v) = status_json(&env);
    assert_eq!((code, &v["healthy"]), (0, &json!(true)), "{v}");

    {
        let db = env.open_db();
        db.conn.execute_batch("PRAGMA user_version = 99").unwrap();
    }
    let (code, v) = status_json(&env);
    assert_eq!(v["healthy"], json!(false), "{v}");
    assert_eq!(code, 1);
    assert!(
        v["database_error"].as_str().unwrap_or("").contains("newer"),
        "{v}"
    );
    let text = String::from_utf8_lossy(
        &env.cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.280")
            .arg("status")
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(
        text.contains("Database:") && text.contains("newer"),
        "{text}"
    );
}

/// Builds a v2 database: the current schema without what v3 added (the
/// `constraints` table and its indexes), stamped 2.
fn v2_database(path: &Path) {
    assert_eq!(db::SCHEMA_VERSION, 3, "update this fixture with the schema");
    drop(Db::open(path, Role::Cli).unwrap());
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE constraints; PRAGMA user_version = 2;")
        .unwrap();
}

/// An old supported database migrates in place and keeps its rows.
#[test]
fn a_v2_database_migrates_to_the_current_schema_with_its_rows() {
    let env = Env::new();
    v2_database(&env.db_path());
    {
        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        conn.execute(
            "INSERT INTO events (dedupe_key, session_id, project_id, hook_event, ts_ms, payload) \
             VALUES ('old', 's', 'p', 'Stop', 1, '{}')",
            [],
        )
        .unwrap();
    }
    let db = Db::open(&env.db_path(), Role::HookAppend).expect("migrates");
    assert_eq!(db::user_version(&db.conn).unwrap(), db::SCHEMA_VERSION);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM constraints"), 0);
    assert_eq!(db::journal_mode(&db.conn).unwrap(), "wal");
}

/// A migration that cannot complete is rolled back whole: the file keeps its
/// old version and every row, hooks fail open and spool, and `status` and
/// `doctor` say so. The next binary that can migrate it does.
#[test]
fn a_failed_migration_leaves_the_old_database_intact_and_is_reported() {
    let env = enabled_env();
    v2_database(&env.db_path());
    {
        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        conn.execute(
            "INSERT INTO events (dedupe_key, session_id, project_id, hook_event, ts_ms, payload) \
             VALUES ('old', 's', 'p', 'Stop', 1, '{}')",
            [],
        )
        .unwrap();
        // Something v3 would create, already there in another shape.
        conn.execute_batch("CREATE TABLE constraints (x INTEGER);")
            .unwrap();
    }
    assert!(Db::open(&env.db_path(), Role::Cli).is_err());
    let check = rusqlite::Connection::open(env.db_path()).unwrap();
    assert_eq!(db::user_version(&check).unwrap(), 2, "not half migrated");
    assert_eq!(
        check
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(check);

    let hook = env.hook("stop", &{
        let mut p = env.base_payload("Stop");
        p["stop_hook_active"] = json!(false);
        p
    });
    hook.assert_contract();
    assert_eq!(
        spool::backlog(&env.spool_dir()),
        1,
        "the event waits in the spool"
    );

    let (code, v) = status_json(&env);
    assert_eq!((code, &v["healthy"]), (1, &json!(false)), "{v}");
    let doctor = env
        .cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.280")
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(doctor.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("database will not open"));

    // Repaired (the conflicting table removed): the migration completes and
    // the spooled event lands.
    {
        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        conn.execute_batch("DROP TABLE constraints;").unwrap();
    }
    env.drain();
    let db = env.open_db();
    assert_eq!(db::user_version(&db.conn).unwrap(), db::SCHEMA_VERSION);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM events"), 2);
}

/// A future schema is never rewritten: not by a hook, not by the CLI, not by
/// `reduce`.
#[test]
fn a_future_schema_is_never_rewritten_or_downgraded() {
    let env = Env::new();
    {
        let db = env.open_db();
        db.conn.execute_batch("PRAGMA user_version = 99").unwrap();
    }
    let before = std::fs::read(env.db_path()).unwrap();
    for (sub, payload) in [
        ("session-start", env.base_payload("SessionStart")),
        ("stop", env.base_payload("Stop")),
    ] {
        env.hook(sub, &payload).assert_contract();
    }
    let _ = env.reduce();
    let _ = env
        .cmd()
        .env("VELRA_CLAUDE_VERSION", "2.1.280")
        .args(["status", "--json"])
        .output();
    let _ = env.cmd().args(["restore", "--list"]).output();
    let check = rusqlite::Connection::open_with_flags(
        env.db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(db::user_version(&check).unwrap(), 99);
    drop(check);
    assert_eq!(
        std::fs::read(env.db_path()).unwrap(),
        before,
        "byte-identical"
    );
    assert!(
        spool::backlog(&env.spool_dir()) >= 2,
        "events kept for a later binary"
    );
}

// =============================================================== concurrency

/// Concurrent writers, each on its own connection with the hook's lock
/// budget, spooling on `Busy` as the hook does; then a replay. Every event is
/// stored exactly once, over several rounds.
#[test]
fn concurrent_writers_and_a_replaying_reducer_lose_and_duplicate_nothing() {
    for round in 0..3 {
        let env = Env::new();
        drop(env.open_db());
        let writers = 8;
        let per = 40;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(writers + 1));
        let mut handles = Vec::new();
        for w in 0..writers {
            let (db_path, spool_dir, barrier) = (env.db_path(), env.spool_dir(), barrier.clone());
            let evs: Vec<NewEvent> = (0..per)
                .map(|i| event(&env, &format!("r{round}-w{w}-e{i}"), "Stop", i as i64, "{}"))
                .collect();
            handles.push(std::thread::spawn(move || {
                let mut db = Db::open(&db_path, Role::HookAppend).expect("open");
                barrier.wait();
                let mut spooled = 0;
                for ev in evs {
                    match eventlog::append(&mut db.conn, &ev) {
                        Ok(_) => {}
                        Err(_) => {
                            spool::write(&spool_dir, &ev).expect("spool");
                            spooled += 1;
                        }
                    }
                }
                spooled
            }));
        }
        // A reducer draining the spool while the writers run.
        let (db_path, spool_dir) = (env.db_path(), env.spool_dir());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let reducer_thread = std::thread::spawn(move || {
            barrier.wait();
            let mut db = Db::open(&db_path, Role::Reduce).expect("open");
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = reducer::reduce_all(&mut db.conn, Some(&spool_dir));
            }
        });
        let spooled: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        reducer_thread.join().unwrap();
        let mut db = env.open_db();
        reducer::reduce_all(&mut db.conn, Some(&env.spool_dir())).unwrap();
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM events") as usize,
            writers * per,
            "round {round}: {spooled} spooled"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(DISTINCT dedupe_key) FROM events") as usize,
            writers * per
        );
        assert_eq!(spool::backlog(&env.spool_dir()), 0);
        assert_eq!(cursor(&db), max_event_id(&db));
    }
}

/// Concurrent reducers over one ledger that includes late spooled events:
/// the projection equals a single reducer's, over several rounds.
#[test]
fn concurrent_reducers_fold_to_the_single_reducer_state() {
    let expected = {
        let mut log = mixed_ledger();
        log.reduce();
        state_of(&log.db)
    };
    for _round in 0..4 {
        let log = mixed_ledger();
        let env = log.into_env();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (db_path, spool_dir, barrier) =
                    (env.db_path(), env.spool_dir(), barrier.clone());
                std::thread::spawn(move || {
                    let mut db = Db::open(&db_path, Role::Reduce).expect("open");
                    db.conn.busy_timeout(Duration::from_secs(10)).unwrap();
                    barrier.wait();
                    for _ in 0..3 {
                        reducer::reduce_all(&mut db.conn, Some(&spool_dir)).expect("reduce");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let db = env.open_db();
        assert_eq!(state_of(&db), expected);
    }
}

/// Direct events, with spooled ones that belong earlier waiting in the spool.
fn mixed_ledger() -> Log {
    let mut log = Log::new();
    log.prompt("fix the flaky logout test");
    log.prompt("do not modify the tests");
    let t = log.ts;
    for (i, text) in [
        "also keep the cookie behaviour",
        "never delete the audit log",
    ]
    .iter()
    .enumerate()
    {
        let ev = event(
            &log.env,
            &format!("late{i}"),
            "UserPromptSubmit",
            t - 1_500 + i as i64,
            &prompt_payload(text),
        );
        spool::write(&log.env.spool_dir(), &ev).unwrap();
    }
    log.prompt("now make the retry idempotent");
    log
}

/// A spooled checkpoint request and a direct checkpoint racing, from
/// separate connections: whichever order they land in, one live continuation
/// is left, and it is the newest compaction's.
#[test]
fn a_checkpoint_request_racing_a_newer_checkpoint_never_leaves_the_older_live() {
    for round in 0..6 {
        let mut log = Log::new();
        log.env.write_file("src/a.rs", "v0\n");
        log.prompt("fix the flaky logout test");
        log.edit("src/a.rs", "v1\n");
        log.reduce();
        let older = log.ts + 100;
        let newer = log.ts + 900;
        let req = event(
            &log.env,
            "req",
            velra_core::model::hook_event::CHECKPOINT_REQUEST,
            older,
            &Payload {
                trigger: Some("manual".into()),
                partial: Some(true),
                ..Default::default()
            }
            .to_json(),
        );
        spool::write(&log.env.spool_dir(), &req).unwrap();
        let session = log.session();
        let env = log.into_env();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (p1, p2, s1, b1, b2) = (
            env.db_path(),
            env.db_path(),
            env.spool_dir(),
            barrier.clone(),
            barrier,
        );
        let sess = session.clone();
        let direct = std::thread::spawn(move || {
            let mut db = Db::open(&p1, Role::Cli).unwrap();
            b1.wait();
            if round % 2 == 0 {
                std::thread::yield_now();
            }
            let tx = db
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            velra_core::checkpoint::create_in_tx(
                &tx,
                &velra_core::checkpoint::CheckpointRequest {
                    session_id: &sess,
                    trigger: velra_core::model::Trigger::Manual,
                    created_ms: newer,
                    partial: false,
                    watermark: 0,
                },
                &Default::default(),
            )
            .unwrap();
            tx.commit().unwrap();
        });
        let replay = std::thread::spawn(move || {
            let mut db = Db::open(&p2, Role::Cli).unwrap();
            b2.wait();
            reducer::reduce_all(&mut db.conn, Some(&s1)).unwrap();
        });
        direct.join().unwrap();
        replay.join().unwrap();
        let db = env.open_db();
        let live: Vec<i64> = db
            .conn
            .prepare(
                "SELECT k.created_ms FROM continuations c JOIN checkpoints k USING (checkpoint_id) \
                 WHERE c.session_id = ?1 AND c.state IN ('PENDING','ATTACHED')",
            )
            .unwrap()
            .query_map([&session], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(live, vec![newer], "round {round}");
    }
}

/// Hook processes writing concurrently while others are killed mid-flight:
/// the database stays intact, nothing is duplicated, and every hook that
/// finished has its event stored.
#[test]
fn killed_and_finished_hooks_together_leave_an_intact_deduplicated_ledger() {
    let env = Env::new();
    drop(env.open_db());
    let n = 24;
    let mut children = Vec::new();
    for i in 0..n {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!(format!("t{i}"));
        p["tool_input"] = json!({ "file_path": env.project.join(format!("f{i}.rs")) });
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
            .args(["hook", "post-tool-use"])
            .env("VELRA_HOME", &env.home)
            .env("CLAUDE_PROJECT_DIR", &env.project)
            .env("VELRA_TEST_WATCHDOG_MS", "60000")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        use std::io::Write;
        let _ = child
            .stdin
            .take()
            .unwrap()
            .write_all(p.to_string().as_bytes());
        children.push(child);
    }
    let mut finished = Vec::new();
    for (i, mut c) in children.into_iter().enumerate() {
        if i % 3 == 0 {
            let _ = c.kill();
            let _ = c.wait();
        } else {
            let st = c.wait().unwrap();
            assert_eq!(st.code(), Some(0));
            finished.push(format!("t{i}"));
        }
    }
    env.drain();
    let db = env.open_db();
    let ok: String = db
        .conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM events"),
        count(&db, "SELECT COUNT(DISTINCT tool_use_id) FROM events"),
        "no duplicate logical events"
    );
    for t in finished {
        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE tool_use_id = ?1",
                [&t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{t} finished but is missing");
    }
}

// ============================== confirmation evidence that went to the spool

fn hook_ok(env: &Env, sub: &str, payload: Value) -> common::HookOutput {
    let out = env.hook(sub, &payload);
    out.assert_contract();
    out
}

fn capsule_written(out: &common::HookOutput) -> bool {
    out.json()
        .is_some_and(|v| v["hookSpecificOutput"]["additionalContext"].is_string())
}

/// D120. A continuation delivered on a prompt is written again on the next
/// prompt when the turn in between left no evidence (T4). Evidence that went
/// to the spool -- the tool call's hook met a locked database -- was not
/// evidence: the next prompt re-emitted a capsule the model had already
/// acted on.
#[test]
fn spooled_confirmation_evidence_prevents_a_re_emission() {
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    let file = env.project.join("src/a.rs");
    hook_ok(&env, "user-prompt-submit", {
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!("fix the flaky logout test and keep the cookie behaviour");
        p["prompt_id"] = json!("p0");
        p
    });
    env.write_file("src/a.rs", "v1\n");
    hook_ok(&env, "post-tool-use", {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!("t1");
        p["tool_input"] = json!({ "file_path": file, "old_string": "v0", "new_string": "v1" });
        p["tool_response"] = json!({ "filePath": file, "originalFile": "v0\n" });
        p
    });
    env.drain();
    hook_ok(&env, "pre-compact", {
        let mut p = env.base_payload("PreCompact");
        p["trigger"] = json!("manual");
        p
    });
    let prompt = |id: &str| {
        hook_ok(&env, "user-prompt-submit", {
            let mut p = env.base_payload("UserPromptSubmit");
            p["prompt"] = json!("what should we try next?");
            p["prompt_id"] = json!(id);
            p
        })
    };
    assert!(capsule_written(&prompt("p1")), "delivered on the prompt");

    // The turn goes on, but its tool call meets a locked database.
    let lock = env.open_db();
    lock.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    hook_ok(&env, "post-tool-use", {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!("t-during-lock");
        p["tool_input"] = json!({ "file_path": file });
        p
    });
    lock.conn.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    assert!(
        spool::backlog(&env.spool_dir()) >= 1,
        "the evidence was spooled"
    );

    let next = prompt("p2");
    assert!(
        !capsule_written(&next),
        "re-emitted although the turn went on: {}",
        next.stdout
    );
    let db = env.open_db();
    let (state, attach): (String, i64) = db
        .conn
        .query_row("SELECT state, attach_count FROM continuations", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((state.as_str(), attach), ("CONFIRMED", 1));
}

/// The residual D114 names: a session start killed from outside (not by its
/// watchdog) after its capsule reached stdout and before its commit. The
/// ledger is left valid -- the uncommitted delivery is rolled back whole, the
/// continuation is PENDING with no injection -- and the next delivery point
/// writes the capsule once more and records it once. A bounded repetition,
/// never corruption and never a delivery recorded that did not happen.
#[cfg(feature = "fault-injection")]
#[test]
fn a_session_start_killed_between_write_and_commit_leaves_a_valid_ledger() {
    use std::io::{BufRead, Write};
    let env = Env::new();
    env.write_file("src/a.rs", "v0\n");
    hook_ok(&env, "user-prompt-submit", {
        let mut p = env.base_payload("UserPromptSubmit");
        p["prompt"] = json!("fix the flaky logout test and keep the cookie behaviour");
        p["prompt_id"] = json!("p0");
        p
    });
    env.drain();
    hook_ok(&env, "pre-compact", {
        let mut p = env.base_payload("PreCompact");
        p["trigger"] = json!("manual");
        p
    });

    let mut p = env.base_payload("SessionStart");
    p["source"] = json!("compact");
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("velra"))
        .args(["hook", "session-start"])
        .env("VELRA_HOME", &env.home)
        .env("CLAUDE_PROJECT_DIR", &env.project)
        .env("VELRA_TEST_WATCHDOG_MS", "60000")
        .env("VELRA_TEST_STALL_AFTER_CONTINUATION_EMIT_MS", "30000")
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
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("additionalContext"), "written: {line}");
    child.kill().unwrap();
    child.wait().unwrap();

    let db = env.open_db();
    let ok: String = db
        .conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let row = |db: &Db| -> (String, i64, i64) {
        db.conn
            .query_row(
                "SELECT state, attach_count, (SELECT COUNT(*) FROM injections) FROM continuations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    assert_eq!(row(&db), ("PENDING".into(), 0, 0), "nothing recorded");
    drop(db);

    let next = hook_ok(&env, "post-tool-use", {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!("t-after-kill");
        p["tool_input"] = json!({ "file_path": env.project.join("src/a.rs") });
        p
    });
    assert!(capsule_written(&next), "written once more");
    assert_eq!(row(&env.open_db()), ("ATTACHED".into(), 1, 1));
}
