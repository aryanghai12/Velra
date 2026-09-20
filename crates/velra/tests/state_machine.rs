//! D1–D7: the continuation delivery state machine (§15).

mod common;

use common::Log;
use proptest::prelude::*;
use velra_core::continuation::{self, Delivery, DeliveryRequest};
use velra_core::db::{Db, DbError, Role};
use velra_core::model::{Channel, ContinuationState};

fn deliver(log: &mut Log, channel: Channel, key: &str, ts: i64) -> Option<Delivery> {
    let session = log.env.session.clone();
    let req = DeliveryRequest {
        session_id: &session,
        channel,
        delivery_key: key,
        ts_ms: ts,
    };
    continuation::deliver(&mut log.db.conn, &req, |_| true).expect("deliver")
}

fn state(log: &Log) -> Option<ContinuationState> {
    continuation::live(&log.db.conn, &log.env.session)
        .expect("live")
        .map(|l| l.state)
}

fn injections(log: &Log) -> i64 {
    log.db
        .conn
        .query_row("SELECT COUNT(*) FROM injections", [], |r| r.get(0))
        .unwrap_or(0)
}

/// A session with a PENDING continuation ready to deliver.
fn pending() -> Log {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("fix the flaky logout test and keep the cookie behaviour intact");
    log.edit("src/a.rs", "v1\n");
    log.command_fail("pytest -x", 1, "FAILED tests/test_a.py::test_x\n1 failed");
    log.checkpoint();
    assert_eq!(state(&log), Some(ContinuationState::Pending));
    log
}

#[test]
fn d2_session_start_delivers_once_and_the_next_prompt_does_not_redeliver() {
    let mut log = pending();
    let ts = log.ts + 10_000;
    let first =
        deliver(&mut log, Channel::SessionStart, "key-session-start", ts).expect("delivered");
    assert!(first.capsule.contains("[FIRST_MESSAGE]"));
    assert_eq!(state(&log), Some(ContinuationState::Attached));
    assert_eq!(injections(&log), 1);

    // T5: context injected at session start stays in the conversation.
    assert!(deliver(&mut log, Channel::UserPrompt, "key-prompt-1", ts + 1_000).is_none());
    assert_eq!(injections(&log), 1);
}

#[test]
fn d3_first_post_tool_use_delivers_when_session_start_never_fires() {
    let mut log = pending();
    let created: i64 = log
        .db
        .conn
        .query_row("SELECT created_ms FROM checkpoints", [], |r| r.get(0))
        .expect("checkpoint");

    // A tool call from before the checkpoint must not deliver.
    assert!(deliver(&mut log, Channel::PostTool, "key-old", created - 1).is_none());
    assert_eq!(state(&log), Some(ContinuationState::Pending));

    let delivered =
        deliver(&mut log, Channel::PostTool, "key-new", created + 1).expect("delivered");
    assert_eq!(delivered.channel, Channel::PostTool);
    assert_eq!(state(&log), Some(ContinuationState::Attached));
}

#[test]
fn d4_aborted_turn_redelivers_then_confirms() {
    let mut log = pending();
    let ts = log.ts + 10_000;
    assert!(deliver(&mut log, Channel::UserPrompt, "prompt-1", ts).is_some());
    assert_eq!(injections(&log), 1);

    // No evidence the turn progressed: the next prompt re-emits (T4).
    assert!(deliver(&mut log, Channel::UserPrompt, "prompt-2", ts + 1_000).is_some());
    assert_eq!(injections(&log), 2);
    assert_eq!(state(&log), Some(ContinuationState::Attached));

    // A tool call is evidence; reconcile confirms (T3/T7).
    log.ts = ts + 2_000;
    log.command_ok("cargo build", "Finished");
    continuation::reconcile(&mut log.db.conn, &log.env.session.clone(), log.ts + 100)
        .expect("reconcile");
    assert_eq!(state(&log), None, "CONFIRMED is terminal");
    let confirmed: i64 = log
        .db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM continuations WHERE state = 'CONFIRMED'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(confirmed, 1);
}

#[test]
fn d4b_attach_count_is_capped_at_three() {
    let mut log = pending();
    let mut ts = log.ts + 10_000;
    for i in 0..3 {
        assert!(
            deliver(&mut log, Channel::UserPrompt, &format!("prompt-{i}"), ts).is_some(),
            "attach {i}"
        );
        ts += 1_000;
    }
    assert!(
        deliver(&mut log, Channel::UserPrompt, "prompt-4", ts).is_none(),
        "fourth attach expires instead"
    );
    assert_eq!(state(&log), None);
    let expired: i64 = log
        .db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM continuations WHERE state = 'EXPIRED'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(expired, 1);
}

#[test]
fn d5_parallel_post_tool_use_delivers_exactly_once() {
    let log = pending();
    let created: i64 = log
        .db
        .conn
        .query_row("SELECT created_ms FROM checkpoints", [], |r| r.get(0))
        .unwrap();
    // Close the setup connection but keep the temp dir: the threads below open
    // this path themselves, and dropping the whole `Log` would delete the
    // database out from under them. POSIX unlinks it right away; Windows leaves
    // it behind, which is the only reason this ever passed there.
    let env = log.into_env();
    let db_path = env.db_path();
    let session = env.session.clone();

    let emitted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for i in 0..8 {
        let (db_path, session, emitted, barrier) = (
            db_path.clone(),
            session.clone(),
            emitted.clone(),
            barrier.clone(),
        );
        handles.push(std::thread::spawn(move || {
            let mut db = Db::open(&db_path, Role::HookDelivery).expect("open");
            barrier.wait();
            let key = format!("race-{i}");
            let req = DeliveryRequest {
                session_id: &session,
                channel: Channel::PostTool,
                delivery_key: &key,
                ts_ms: created + 1 + i as i64,
            };
            match continuation::deliver(&mut db.conn, &req, |_| {
                emitted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                true
            }) {
                Ok(d) => usize::from(d.is_some()),
                // What the hook itself does with this (§15): losing the write
                // lock inside the role's 100 ms budget leaves the continuation
                // deliverable for the next hook. It is not a delivery, and it
                // is not a failure either — with eight threads on one lock it
                // is the expected outcome for the seven that lose.
                Err(DbError::Busy) => 0,
                Err(e) => panic!("deliver: {e}"),
            }
        }));
    }
    let delivered: usize = handles.into_iter().map(|h| h.join().expect("thread")).sum();
    assert_eq!(delivered, 1, "exactly one process delivers");
    assert_eq!(
        emitted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one emission"
    );
}

#[test]
fn d6_duplicate_delivery_key_reemits_without_a_new_injection() {
    let mut log = pending();
    let ts = log.ts + 10_000;
    let first = deliver(&mut log, Channel::UserPrompt, "same-key", ts).expect("delivered");
    assert_eq!(injections(&log), 1);

    let again = deliver(&mut log, Channel::UserPrompt, "same-key", ts + 5_000).expect("re-emitted");
    assert_eq!(again.capsule, first.capsule, "identical capsule");
    assert!(again.replay);
    assert_eq!(injections(&log), 1, "no new injection row");
    let attach: i64 = log
        .db
        .conn
        .query_row("SELECT attach_count FROM continuations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(attach, 1, "attach_count unchanged");
}

#[test]
fn d7_clear_expires_the_continuation() {
    let mut log = pending();
    continuation::expire_live(&log.db.conn, &log.env.session.clone(), log.ts + 1).expect("expire");
    assert_eq!(state(&log), None);
    let ts = log.ts + 2;
    assert!(deliver(&mut log, Channel::SessionStart, "after-clear", ts).is_none());
}

#[test]
fn a_second_compaction_supersedes_the_older_continuation() {
    let mut log = pending();
    let first: String = log
        .db
        .conn
        .query_row("SELECT checkpoint_id FROM continuations", [], |r| r.get(0))
        .unwrap();
    log.ts += 60_000;
    log.edit("src/a.rs", "v2\n");
    let second = log.checkpoint();
    assert_ne!(first, second);

    let live = continuation::live(&log.db.conn, &log.env.session)
        .unwrap()
        .expect("live");
    assert_eq!(live.checkpoint_id, second);
    let superseded: i64 = log
        .db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM continuations WHERE state = 'SUPERSEDED'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(superseded, 1);
    // The newest capsule is delivered.
    let ts = log.ts + 1;
    let delivered =
        deliver(&mut log, Channel::SessionStart, "after-second", ts).expect("delivered");
    assert_eq!(delivered.checkpoint_id, second);
}

#[test]
fn continuations_never_cross_sessions() {
    let mut log = pending();
    let session = "a-different-session";
    let req = DeliveryRequest {
        session_id: session,
        channel: Channel::SessionStart,
        delivery_key: "other-session",
        ts_ms: log.ts + 1,
    };
    assert!(continuation::deliver(&mut log.db.conn, &req, |_| true)
        .expect("deliver")
        .is_none());
    assert_eq!(state(&log), Some(ContinuationState::Pending), "untouched");
}

#[test]
fn checkpoints_are_immutable() {
    let log = pending();
    let err = log
        .db
        .conn
        .execute("UPDATE checkpoints SET capsule = 'tampered'", []);
    assert!(err.is_err(), "the immutability trigger must reject updates");
}

// ------------------------------------------------------------ D1: model test

#[derive(Debug, Clone)]
enum Op {
    PreCompact,
    SessionStartCompact,
    SessionStartClear,
    UserPrompt,
    PostToolUse,
    Stop,
    SessionEndClear,
    Reconcile,
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::PreCompact),
        Just(Op::SessionStartCompact),
        Just(Op::SessionStartClear),
        Just(Op::UserPrompt),
        Just(Op::PostToolUse),
        Just(Op::Stop),
        Just(Op::SessionEndClear),
        Just(Op::Reconcile),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// D1: invariants hold over random interleavings.
    #[test]
    fn d1_invariants_hold(ops in prop::collection::vec(arb_op(), 1..24)) {
        let mut log = Log::new();
        log.env.write_file("src/a.rs", "v0\n");
        log.prompt("fix the flaky logout test and keep the cookie behaviour intact");
        log.edit("src/a.rs", "v1\n");
        log.reduce();
        let session = log.env.session.clone();
        let mut checkpoints = 0i64;
        let mut key = 0u32;

        for op in ops {
            log.ts += 1_000;
            let ts = log.ts;
            key += 1;
            match op {
                Op::PreCompact => {
                    log.checkpoint();
                    checkpoints += 1;
                }
                Op::SessionStartCompact => {
                    let k = format!("k{key}");
                    let req = DeliveryRequest { session_id: &session, channel: Channel::SessionStart, delivery_key: &k, ts_ms: ts };
                    continuation::deliver(&mut log.db.conn, &req, |_| true).expect("deliver");
                }
                Op::UserPrompt => {
                    let k = format!("k{key}");
                    let req = DeliveryRequest { session_id: &session, channel: Channel::UserPrompt, delivery_key: &k, ts_ms: ts };
                    continuation::deliver(&mut log.db.conn, &req, |_| true).expect("deliver");
                }
                Op::PostToolUse => {
                    let k = format!("k{key}");
                    let req = DeliveryRequest { session_id: &session, channel: Channel::PostTool, delivery_key: &k, ts_ms: ts };
                    continuation::deliver(&mut log.db.conn, &req, |_| true).expect("deliver");
                    log.command_ok("cargo build", "Finished");
                }
                Op::Stop => log.stop(),
                Op::SessionStartClear | Op::SessionEndClear => {
                    continuation::expire_live(&log.db.conn, &session, ts).expect("expire");
                }
                Op::Reconcile => {
                    continuation::reconcile(&mut log.db.conn, &session, ts).expect("reconcile");
                }
            }

            // At most one live continuation per session.
            let live: i64 = log.db.conn
                .query_row("SELECT COUNT(*) FROM continuations WHERE session_id = ?1 AND state IN ('PENDING','ATTACHED')", [&session], |r| r.get(0))
                .unwrap();
            prop_assert!(live <= 1, "{live} live continuations");

            // attach_count never exceeds the cap.
            let max_attach: i64 = log.db.conn
                .query_row("SELECT COALESCE(MAX(attach_count), 0) FROM continuations", [], |r| r.get(0))
                .unwrap();
            prop_assert!(max_attach <= continuation::MAX_ATTACH, "attach_count {max_attach}");

            // CONFIRMED only after an injection for that checkpoint.
            let bad_confirm: i64 = log.db.conn
                .query_row(
                    "SELECT COUNT(*) FROM continuations c WHERE c.state = 'CONFIRMED' \
                     AND NOT EXISTS (SELECT 1 FROM injections i WHERE i.checkpoint_id = c.checkpoint_id)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            prop_assert_eq!(bad_confirm, 0, "confirmed without an injection");

            // Checkpoints are only ever added.
            let stored: i64 = log.db.conn.query_row("SELECT COUNT(*) FROM checkpoints", [], |r| r.get(0)).unwrap();
            prop_assert_eq!(stored, checkpoints);
        }
    }
}
