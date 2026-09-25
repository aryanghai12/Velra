//! Storage measurements (Phase 8 of the v0.1.2 hardening audit). Ignored by
//! default: they time the machine they run on, and assert only loose bounds
//! that a working build meets by a wide margin. Run with
//! `cargo test --release -p velra --test storage_bounds -- --ignored --nocapture`.

mod common;

use common::Env;
use std::time::{Duration, Instant};
use velra_core::db::{Db, Role};
use velra_core::event::{NewEvent, Payload};
use velra_core::{eventlog, reducer, spool};

fn ev(env: &Env, key: String, ts: i64, text: &str) -> NewEvent {
    NewEvent {
        dedupe_key: key,
        session_id: env.session.clone(),
        project_id: env.project_id(),
        agent_id: None,
        hook_event: "UserPromptSubmit".into(),
        tool_name: None,
        tool_use_id: None,
        ts_ms: ts,
        payload: Payload {
            prompt: Some(text.into()),
            ..Default::default()
        }
        .to_json(),
        project: Some(env.project_info()),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn pct(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

#[test]
#[ignore]
fn measure_direct_append_latency() {
    let env = Env::new();
    let mut db = Db::open(&env.db_path(), Role::HookAppend).unwrap();
    let mut t = Vec::new();
    for i in 0..2_000 {
        let e = ev(
            &env,
            format!("a{i}"),
            i,
            "a prompt of ordinary length for timing",
        );
        let s = Instant::now();
        eventlog::append(&mut db.conn, &e).unwrap();
        t.push(ms(s.elapsed()));
    }
    println!(
        "direct append x2000: p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
        pct(t.clone(), 0.5),
        pct(t.clone(), 0.99),
        pct(t, 1.0)
    );
}

#[test]
#[ignore]
fn measure_locked_append_is_bounded_by_the_role_budget() {
    let env = Env::new();
    drop(Db::open(&env.db_path(), Role::Cli).unwrap());
    let holder = Db::open(&env.db_path(), Role::Cli).unwrap();
    holder.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut db = Db::open(&env.db_path(), Role::HookAppend).unwrap();
    let mut t = Vec::new();
    for i in 0..20 {
        let e = ev(&env, format!("l{i}"), i, "locked");
        let s = Instant::now();
        let r = eventlog::append(&mut db.conn, &e);
        t.push(ms(s.elapsed()));
        assert!(r.is_err());
    }
    holder.conn.execute_batch("ROLLBACK").unwrap();
    let max = pct(t.clone(), 1.0);
    println!(
        "append against a held write lock x20: p50 {:.1} ms, max {:.1} ms (budget 100 ms)",
        pct(t, 0.5),
        max
    );
    assert!(max < 400.0);
}

#[test]
#[ignore]
fn measure_spool_append_and_replay() {
    for n in [1_000usize, 10_000] {
        let env = Env::new();
        drop(Db::open(&env.db_path(), Role::Cli).unwrap());
        let s = Instant::now();
        for i in 0..n {
            spool::write(
                &env.spool_dir(),
                &ev(&env, format!("s{i}"), common::BASE_MS + i as i64, "spooled"),
            )
            .unwrap();
        }
        let write = s.elapsed();
        let s = Instant::now();
        let listed = spool::pending(&env.spool_dir()).len();
        let list = s.elapsed();
        let mut db = Db::open(&env.db_path(), Role::Reduce).unwrap();
        let s = Instant::now();
        let first = spool::ingest(&mut db.conn, &env.spool_dir(), 500).unwrap();
        let one_batch = s.elapsed();
        let s = Instant::now();
        let stats = reducer::reduce_all(&mut db.conn, Some(&env.spool_dir())).unwrap();
        let rest = s.elapsed();
        println!(
            "spool n={n}: write {:.3} ms/file; list {listed} files {:.1} ms; first ingest of {first} {:.1} ms; \
             reduce_all of the rest ({} ingested, {} reduced) {:.1} ms",
            ms(write) / n as f64,
            ms(list),
            ms(one_batch),
            stats.spool_ingested,
            stats.processed,
            ms(rest)
        );
    }
}

/// A long session whose last events all arrive through the spool: every one
/// of them is checked for logical position against the whole session.
#[test]
#[ignore]
fn measure_reduction_of_a_long_session_with_a_spooled_tail() {
    for (direct, spooled) in [(5_000usize, 0usize), (5_000, 500), (20_000, 2_000)] {
        let env = Env::new();
        let mut db = Db::open(&env.db_path(), Role::Cli).unwrap();
        {
            let tx = db.conn.transaction().unwrap();
            for i in 0..direct {
                eventlog::insert_event(
                    &tx,
                    &ev(
                        &env,
                        format!("d{i}"),
                        common::BASE_MS + i as i64 * 10,
                        "task: work",
                    ),
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let s = Instant::now();
        reducer::reduce_all(&mut db.conn, None).unwrap();
        let base = s.elapsed();
        for j in 0..spooled {
            let ts = common::BASE_MS + (direct as i64) * 10 + j as i64;
            spool::write(
                &env.spool_dir(),
                &ev(&env, format!("t{j}"), ts, "a later note"),
            )
            .unwrap();
        }
        let s = Instant::now();
        let stats = reducer::reduce_all(&mut db.conn, Some(&env.spool_dir())).unwrap();
        let tail = s.elapsed();
        // What the async reducer (900 ms deadline) gets through in one run.
        println!(
            "session of {direct} direct: reduce {:.0} ms; then {spooled} spooled tail events: {:.0} ms ({:.2} ms/event, {} processed)",
            ms(base),
            ms(tail),
            if spooled > 0 { ms(tail) / spooled as f64 } else { 0.0 },
            stats.processed
        );
    }
}

/// Whole hook processes, production watchdog, while another connection holds
/// the write lock: the time Claude Code waits is bounded by the watchdog, not
/// by how long the lock is held.
#[test]
#[ignore]
fn measure_hooks_against_a_held_write_lock() {
    let env = Env::new();
    drop(Db::open(&env.db_path(), Role::Cli).unwrap());
    let run = |sub: &str, payload: &serde_json::Value| {
        let s = Instant::now();
        let out = env
            .cmd_with_real_watchdog()
            .args(["hook", sub])
            .write_stdin(payload.to_string())
            .output()
            .unwrap();
        (ms(s.elapsed()), out.status.code())
    };
    let mut free = Vec::new();
    for i in 0..10 {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = serde_json::json!("Read");
        p["tool_use_id"] = serde_json::json!(format!("f{i}"));
        free.push(run("post-tool-use", &p).0);
    }
    let holder = Db::open(&env.db_path(), Role::Cli).unwrap();
    holder.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut locked = Vec::new();
    for i in 0..10 {
        let mut p = env.base_payload("PostToolUse");
        p["tool_name"] = serde_json::json!("Read");
        p["tool_use_id"] = serde_json::json!(format!("l{i}"));
        let (t, code) = run("post-tool-use", &p);
        assert_eq!(code, Some(0));
        locked.push(t);
        let mut p = env.base_payload("SessionStart");
        p["source"] = serde_json::json!("compact");
        let (t, code) = run("session-start", &p);
        assert_eq!(code, Some(0));
        locked.push(t);
    }
    holder.conn.execute_batch("ROLLBACK").unwrap();
    println!(
        "hook wall time, lock free x10: p50 {:.0} ms max {:.0} ms; write lock held x20: p50 {:.0} ms max {:.0} ms (watchdog 250 ms)",
        pct(free.clone(), 0.5),
        pct(free, 1.0),
        pct(locked.clone(), 0.5),
        pct(locked, 1.0)
    );
    println!(
        "spool files after the locked hooks: {}",
        spool::backlog(&env.spool_dir())
    );
}
