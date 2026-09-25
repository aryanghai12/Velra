//! The in-session `/compact` continuation, end to end: PreCompact checkpoint,
//! native compaction, delivery on one of three channels, confirmation, and
//! everything that can go wrong in between (Phase 7 of the v0.1.2 hardening
//! audit; DECISIONS D110-D115).
//!
//! What these tests establish is the *state*: which continuation row exists,
//! in which state, how many injections were recorded and what the hook wrote
//! to stdout. None of them can say whether a model read the capsule -- that is
//! the human end-to-end check, and `CONFIRMED` is defined accordingly (D113):
//! the session went on after the delivery, nothing more.

mod common;

use common::{Env, Log};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use velra_core::continuation::{self, Delivery, DeliveryRequest, PENDING_TTL_MS};
use velra_core::db::{Db, DbError, Role};
use velra_core::event::Payload;
use velra_core::model::{hook_event as he, Channel};

const CHANNELS: [Channel; 3] = [
    Channel::SessionStart,
    Channel::PostTool,
    Channel::UserPrompt,
];

/// One `continuations` row, as the assertions read it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    checkpoint_id: String,
    session_id: String,
    state: String,
    attach_count: i64,
    attached_ms: Option<i64>,
    attached_channel: Option<String>,
    confirmed_ms: Option<i64>,
    confirm_event_id: Option<i64>,
}

fn rows(db: &Db) -> Vec<Row> {
    db.conn
        .prepare(
            "SELECT checkpoint_id, session_id, state, attach_count, attached_ms, attached_channel, \
             confirmed_ms, confirm_event_id FROM continuations ORDER BY rowid",
        )
        .expect("prepare")
        .query_map([], |r| {
            Ok(Row {
                checkpoint_id: r.get(0)?,
                session_id: r.get(1)?,
                state: r.get(2)?,
                attach_count: r.get(3)?,
                attached_ms: r.get(4)?,
                attached_channel: r.get(5)?,
                confirmed_ms: r.get(6)?,
                confirm_event_id: r.get(7)?,
            })
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn row(db: &Db, checkpoint_id: &str) -> Row {
    rows(db)
        .into_iter()
        .find(|r| r.checkpoint_id == checkpoint_id)
        .unwrap_or_else(|| panic!("no continuation for {checkpoint_id}"))
}

fn injections(db: &Db, checkpoint_id: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM injections WHERE checkpoint_id = ?1",
            [checkpoint_id],
            |r| r.get(0),
        )
        .expect("injections")
}

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).expect(sql)
}

fn created_ms(db: &Db, checkpoint_id: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT created_ms FROM checkpoints WHERE checkpoint_id = ?1",
            [checkpoint_id],
            |r| r.get(0),
        )
        .expect("checkpoint")
}

fn capsule_of(db: &Db, checkpoint_id: &str) -> String {
    velra_core::checkpoint::load(&db.conn, checkpoint_id)
        .expect("load")
        .expect("checkpoint")
        .capsule
}

/// Delivers for the log's current session and counts how many times the
/// consumer was asked to write the capsule out.
fn deliver_counted(
    log: &mut Log,
    channel: Channel,
    key: &str,
    ts: i64,
) -> (Option<Delivery>, usize) {
    let session = log.session();
    deliver_for(log, &session, channel, key, ts)
}

fn deliver_for(
    log: &mut Log,
    session: &str,
    channel: Channel,
    key: &str,
    ts: i64,
) -> (Option<Delivery>, usize) {
    let mut emits = 0usize;
    let req = DeliveryRequest {
        session_id: session,
        channel,
        delivery_key: key,
        ts_ms: ts,
    };
    let d = continuation::deliver(&mut log.db.conn, &req, |_| {
        emits += 1;
        true
    })
    .expect("deliver");
    (d, emits)
}

fn reconcile(log: &mut Log, ts: i64) {
    let session = log.session();
    continuation::reconcile(&mut log.db.conn, &session, ts).expect("reconcile");
}

/// A session with state worth saving, reduced, and nothing checkpointed yet.
fn working_session() -> Log {
    let mut log = Log::new();
    log.env.write_file("src/a.rs", "v0\n");
    log.prompt("fix the flaky logout test and keep the cookie behaviour intact");
    log.edit("src/a.rs", "v1\n");
    log.command_fail("pytest -x", 1, "FAILED tests/test_a.py::test_x\n1 failed");
    log.reduce();
    log
}

/// `working_session` with a checkpoint frozen at PreCompact: returns the log,
/// the checkpoint and its creation time.
fn pending() -> (Log, String, i64) {
    let mut log = working_session();
    let id = log.checkpoint();
    let created = created_ms(&log.db, &id);
    // The log's own clock stays behind the checkpoint until moved on; every
    // test below states its delivery times relative to `created`.
    log.ts = created;
    assert_eq!(row(&log.db, &id).state, "PENDING");
    (log, id, created)
}

fn late_request(log: &mut Log, ts_ms: i64) -> i64 {
    log.append_late(
        he::CHECKPOINT_REQUEST,
        None,
        Payload {
            trigger: Some("manual".into()),
            partial: Some(true),
            ..Default::default()
        },
        ts_ms,
    )
}

// ------------------------------------------------------ transitions (7B, 7L)

#[test]
fn pending_attaches_once_and_confirms_on_later_session_activity() {
    let (mut log, id, created) = pending();
    let (d, emits) = deliver_counted(&mut log, Channel::SessionStart, "ss-1", created + 10);
    let d = d.expect("delivered");
    assert_eq!((emits, d.checkpoint_id.as_str()), (1, id.as_str()));
    let attached = row(&log.db, &id);
    assert_eq!(attached.state, "ATTACHED");
    assert_eq!(attached.attach_count, 1);
    assert_eq!(attached.attached_ms, Some(created + 10));
    assert_eq!(attached.attached_channel.as_deref(), Some("session_start"));
    assert_eq!(attached.confirmed_ms, None);

    // A tool call after the delivery: the session went on.
    log.ts = created + 1_000;
    log.command_ok("cargo build", "Finished");
    let evidence: i64 = log
        .db
        .conn
        .query_row("SELECT MAX(id) FROM events", [], |r| r.get(0))
        .unwrap();
    reconcile(&mut log, created + 5_000);
    let confirmed = row(&log.db, &id);
    assert_eq!(confirmed.state, "CONFIRMED");
    assert_eq!(confirmed.confirm_event_id, Some(evidence));
    assert_eq!(confirmed.confirmed_ms, Some(created + 2_000));
    assert_eq!((confirmed.attach_count, injections(&log.db, &id)), (1, 1));
}

#[test]
fn a_second_session_start_does_not_inject_again() {
    let (mut log, id, created) = pending();
    assert!(
        deliver_counted(&mut log, Channel::SessionStart, "ss-1", created + 10)
            .0
            .is_some()
    );
    let (again, emits) = deliver_counted(&mut log, Channel::SessionStart, "ss-2", created + 20);
    assert!(again.is_none());
    assert_eq!(emits, 0, "not even offered to the consumer");
    assert_eq!(injections(&log.db, &id), 1);
    assert_eq!(row(&log.db, &id).attach_count, 1);
}

/// D110. The same delivery key a second time is the same hook invocation run
/// twice -- two registrations of Velra's hook for one event, or two session
/// starts in one millisecond. It used to write the capsule out again (the
/// "replay" of §15.5), and it did so for a checkpoint that had since been
/// superseded as readily as for the live one.
#[test]
fn a_delivery_key_writes_the_capsule_at_most_once() {
    let (mut log, id, created) = pending();
    let (first, emits) = deliver_counted(&mut log, Channel::UserPrompt, "same", created + 10);
    assert!(first.is_some());
    assert_eq!(emits, 1);

    let (again, emits) = deliver_counted(&mut log, Channel::UserPrompt, "same", created + 20);
    assert!(again.is_none(), "a known key delivers nothing");
    assert_eq!(emits, 0);
    assert_eq!(injections(&log.db, &id), 1);
    assert_eq!(row(&log.db, &id).attach_count, 1);

    // Superseded by a later compaction: the old key must not bring the old
    // capsule back.
    log.ts = created + 60_000;
    log.edit("src/a.rs", "v2\n");
    let second = log.checkpoint();
    let (stale, emits) = deliver_counted(&mut log, Channel::UserPrompt, "same", created + 70_000);
    assert!(stale.is_none());
    assert_eq!(emits, 0);
    assert_eq!(row(&log.db, &id).state, "SUPERSEDED");
    assert_eq!(row(&log.db, &second).state, "PENDING", "left for a new key");
}

#[test]
fn confirmation_never_happens_without_a_delivery() {
    let (mut log, id, created) = pending();
    // Activity after the checkpoint, but no delivery: a failing tool call
    // (PostToolUseFailure never delivers) and a turn end.
    log.ts = created;
    log.command_fail("pytest -x", 1, "FAILED tests/test_a.py::test_x");
    log.stop();
    reconcile(&mut log, created + 10_000);
    assert_eq!(row(&log.db, &id).state, "PENDING");

    // A consumer that could not write the capsule: nothing is recorded, and
    // later activity confirms nothing.
    let session = log.session();
    let req = DeliveryRequest {
        session_id: &session,
        channel: Channel::SessionStart,
        delivery_key: "declined",
        ts_ms: created + 11_000,
    };
    assert!(continuation::deliver(&mut log.db.conn, &req, |_| false)
        .expect("deliver")
        .is_none());
    log.ts = created + 12_000;
    log.command_ok("cargo build", "Finished");
    reconcile(&mut log, created + 20_000);
    let r = row(&log.db, &id);
    assert_eq!((r.state.as_str(), r.attach_count), ("PENDING", 0));
    assert_eq!(injections(&log.db, &id), 0);
    assert_eq!(
        count(
            &log.db,
            "SELECT COUNT(*) FROM continuations WHERE state = 'CONFIRMED'"
        ),
        0
    );
}

#[test]
fn a_delivery_without_later_activity_stays_attached_not_confirmed() {
    let (mut log, id, created) = pending();
    assert!(
        deliver_counted(&mut log, Channel::SessionStart, "ss", created + 10)
            .0
            .is_some()
    );
    // Days pass with nothing from the session.
    reconcile(&mut log, created + 3 * 24 * 3600 * 1000);
    let r = row(&log.db, &id);
    assert_eq!(r.state, "ATTACHED");
    assert_eq!((r.confirmed_ms, r.confirm_event_id), (None, None));
}

#[test]
fn duplicate_confirmation_evidence_changes_nothing() {
    let (mut log, id, created) = pending();
    assert!(
        deliver_counted(&mut log, Channel::UserPrompt, "p1", created + 10)
            .0
            .is_some()
    );
    log.ts = created + 1_000;
    log.command_ok("cargo build", "Finished");
    reconcile(&mut log, created + 3_000);
    let confirmed = row(&log.db, &id);
    assert_eq!(confirmed.state, "CONFIRMED");

    for i in 0..3 {
        log.command_ok("cargo test", "ok");
        log.stop();
        let now = log.ts;
        reconcile(&mut log, now + 100);
        let (d, emits) =
            deliver_counted(&mut log, Channel::UserPrompt, &format!("p-{i}"), now + 200);
        assert!(d.is_none());
        assert_eq!(emits, 0);
    }
    assert_eq!(row(&log.db, &id), confirmed, "the first evidence stands");
    assert_eq!(injections(&log.db, &id), 1);
}

// --------------------------------------------------------- channels (7D)

#[test]
fn each_channel_delivers_a_pending_continuation_when_it_is_first() {
    for channel in CHANNELS {
        let (mut log, id, created) = pending();
        let (d, emits) = deliver_counted(&mut log, channel, "first", created + 10);
        let d = d.unwrap_or_else(|| panic!("{channel:?} did not deliver"));
        assert_eq!((d.channel, emits), (channel, 1));
        let r = row(&log.db, &id);
        assert_eq!(r.state, "ATTACHED");
        assert_eq!(r.attached_channel.as_deref(), Some(channel.as_str()));
    }
}

/// Once a channel has delivered, the others are no-ops. The one exception is
/// the documented re-emission on the prompt channel after a turn that left no
/// evidence (T4), bounded by `MAX_ATTACH`.
#[test]
fn once_attached_no_other_channel_injects() {
    for first in CHANNELS {
        let (mut log, id, created) = pending();
        assert!(deliver_counted(&mut log, first, "first", created + 10)
            .0
            .is_some());
        for (k, later) in CHANNELS.into_iter().enumerate() {
            let t4 = first == Channel::UserPrompt && later == Channel::UserPrompt;
            if t4 {
                continue;
            }
            let (d, emits) = deliver_counted(
                &mut log,
                later,
                &format!("later-{k}"),
                created + 20 + k as i64,
            );
            assert!(d.is_none(), "{later:?} injected after {first:?}");
            assert_eq!(emits, 0);
        }
        assert_eq!(injections(&log.db, &id), 1, "first = {first:?}");
    }
}

#[test]
fn a_post_tool_call_from_before_the_checkpoint_does_not_deliver() {
    let (mut log, id, created) = pending();
    for ts in [created - 1, created] {
        assert!(
            deliver_counted(&mut log, Channel::PostTool, &format!("old-{ts}"), ts)
                .0
                .is_none()
        );
    }
    assert_eq!(row(&log.db, &id).state, "PENDING");
}

/// Every channel racing for one continuation, from separate connections as
/// separate hook processes would: one delivery, one write to stdout.
///
/// One prompt among them, because a session submits its prompts one at a
/// time: several racing prompts would be the T4 re-emission of an aborted
/// turn (`once_attached_no_other_channel_injects`), not a race.
#[test]
fn racing_channels_deliver_exactly_one_capsule() {
    let (log, id, created) = pending();
    let env = log.into_env();
    let db_path = env.db_path();
    let session = env.session.clone();
    let emitted = Arc::new(AtomicUsize::new(0));
    let racers = [
        Channel::SessionStart,
        Channel::PostTool,
        Channel::SessionStart,
        Channel::PostTool,
        Channel::UserPrompt,
        Channel::PostTool,
        Channel::SessionStart,
    ];
    let barrier = Arc::new(Barrier::new(racers.len()));
    let handles: Vec<_> = (0..racers.len())
        .map(|i| {
            let (db_path, session, emitted, barrier) = (
                db_path.clone(),
                session.clone(),
                emitted.clone(),
                barrier.clone(),
            );
            std::thread::spawn(move || {
                let mut db = Db::open(&db_path, Role::HookDelivery).expect("open");
                barrier.wait();
                let key = format!("race-{i}");
                let req = DeliveryRequest {
                    session_id: &session,
                    channel: racers[i],
                    delivery_key: &key,
                    ts_ms: created + 1 + i as i64,
                };
                match continuation::deliver(&mut db.conn, &req, |_| {
                    emitted.fetch_add(1, Ordering::SeqCst);
                    true
                }) {
                    Ok(d) => usize::from(d.is_some()),
                    // The hook leaves a busy continuation for the next hook.
                    Err(DbError::Busy) => 0,
                    Err(e) => panic!("deliver: {e}"),
                }
            })
        })
        .collect();
    let delivered: usize = handles.into_iter().map(|h| h.join().expect("thread")).sum();
    assert_eq!(delivered, 1);
    assert_eq!(emitted.load(Ordering::SeqCst), 1);
    let db = env.open_db();
    assert_eq!(injections(&db, &id), 1);
    assert_eq!(row(&db, &id).attach_count, 1);
}

// ------------------------------------------------------ expiry (7E, 7J)

/// D112. A PENDING continuation past its TTL used to be expired only by
/// `reconcile`, which the PostToolUse channel does not run before delivering
/// -- so a session resumed after a week delivered a week-old capsule on its
/// first tool call.
#[test]
fn an_expired_pending_continuation_is_delivered_on_no_channel() {
    for channel in CHANNELS {
        let (mut log, id, created) = pending();
        let late = created + PENDING_TTL_MS + 1;
        let (d, emits) = deliver_counted(&mut log, channel, "late", late);
        assert!(d.is_none(), "{channel:?} delivered an expired continuation");
        assert_eq!(emits, 0);
        assert_eq!(row(&log.db, &id).state, "EXPIRED", "{channel:?}");
        assert_eq!(injections(&log.db, &id), 0);
    }
}

/// The boundary, stated on the injectable clock rather than by sleeping: a
/// pause of any length (a laptop asleep) is only a gap between two hook
/// timestamps, and the TTL decides.
#[test]
fn the_ttl_boundary_is_inclusive_of_its_last_millisecond() {
    for channel in CHANNELS {
        let (mut log, id, created) = pending();
        let (d, _) = deliver_counted(&mut log, channel, "edge", created + PENDING_TTL_MS);
        assert!(d.is_some(), "{channel:?}");
        assert_eq!(row(&log.db, &id).state, "ATTACHED");
    }
}

#[test]
fn an_expired_attached_continuation_is_not_emitted_again() {
    let (mut log, id, created) = pending();
    assert!(
        deliver_counted(&mut log, Channel::UserPrompt, "p1", created + 10)
            .0
            .is_some()
    );
    let (d, emits) = deliver_counted(
        &mut log,
        Channel::UserPrompt,
        "p2",
        created + PENDING_TTL_MS + 1,
    );
    assert!(d.is_none());
    assert_eq!(emits, 0);
    assert_eq!(row(&log.db, &id).state, "EXPIRED");
    assert_eq!(injections(&log.db, &id), 1);
}

#[test]
fn reconcile_expires_a_pending_continuation_past_its_ttl() {
    let (mut log, id, created) = pending();
    reconcile(&mut log, created + PENDING_TTL_MS + 1);
    assert_eq!(row(&log.db, &id).state, "EXPIRED");
    let (d, _) = deliver_counted(
        &mut log,
        Channel::SessionStart,
        "after",
        created + PENDING_TTL_MS + 2,
    );
    assert!(d.is_none());
}

// --------------------------------------------- repeated compaction (7B.7)

#[test]
fn repeated_compaction_keeps_each_checkpoint_to_itself() {
    let (mut log, a, created_a) = pending();
    let capsule_a = capsule_of(&log.db, &a);
    assert!(
        deliver_counted(&mut log, Channel::SessionStart, "a-ss", created_a + 10)
            .0
            .is_some()
    );
    log.ts = created_a + 1_000;
    log.command_ok("cargo build", "Finished");
    reconcile(&mut log, created_a + 3_000);
    let confirmed_a = row(&log.db, &a);
    assert_eq!(confirmed_a.state, "CONFIRMED");

    // More work, then a second compaction.
    log.edit("src/a.rs", "v2\n");
    log.prompt("now make the retry idempotent too");
    let b = log.checkpoint();
    let created_b = created_ms(&log.db, &b);
    assert_ne!(a, b);
    assert!(created_b > created_a);
    assert_eq!(row(&log.db, &b).state, "PENDING");
    assert_eq!(
        row(&log.db, &a),
        confirmed_a,
        "B does not touch a terminal A"
    );

    // Activity between B's creation and its delivery is not B's evidence.
    log.ts = created_b;
    log.command_ok("cargo build", "Finished");
    reconcile(&mut log, created_b + 1_500);
    assert_eq!(row(&log.db, &b).state, "PENDING");

    let (d, _) = deliver_counted(&mut log, Channel::SessionStart, "b-ss", created_b + 2_000);
    let d = d.expect("B delivered");
    assert_eq!(d.checkpoint_id, b);
    assert_ne!(d.capsule, capsule_a);
    assert!(d.capsule.contains("idempotent"), "{}", d.capsule);
    assert_eq!(injections(&log.db, &a), 1);
    assert_eq!(injections(&log.db, &b), 1);
    assert_eq!(capsule_of(&log.db, &a), capsule_a);
}

#[test]
fn a_compaction_before_delivery_supersedes_and_the_old_capsule_never_arrives() {
    let (mut log, a, created_a) = pending();
    log.ts = created_a + 5_000;
    log.edit("src/a.rs", "v2\n");
    let b = log.checkpoint();
    assert_eq!(row(&log.db, &a).state, "SUPERSEDED");
    for (k, channel) in CHANNELS.into_iter().enumerate() {
        let now = log.ts;
        let (d, _) = deliver_counted(&mut log, channel, &format!("k{k}"), now + 2_000 + k as i64);
        if let Some(d) = d {
            assert_eq!(d.checkpoint_id, b);
        }
    }
    assert_eq!(injections(&log.db, &a), 0);
    assert_eq!(injections(&log.db, &b), 1);
}

// ---------------------------------- the spool fallback of PreCompact (D111)

/// D111. PreCompact that cannot reach the database spools a
/// `checkpoint_request`. When the session had moved on by the time the spool
/// was ingested -- SessionStart(compact) itself reaches the database a moment
/// later -- the request was not logically last, the session was rebuilt, and
/// the rebuild skipped every request as one it had "already acted on". It had
/// not: the checkpoint was never made and the continuation silently lost.
#[test]
fn a_spooled_checkpoint_request_behind_a_later_event_still_checkpoints() {
    let mut log = working_session();
    let boundary = log.ts + 400;
    // SessionStart(compact) reached the database; there was nothing to deliver.
    log.append(
        "SessionStart",
        None,
        Payload {
            source: Some("compact".into()),
            ..Default::default()
        },
    );
    log.reduce();
    log.append_late(
        "PreCompact",
        None,
        Payload {
            trigger: Some("manual".into()),
            ..Default::default()
        },
        boundary,
    );
    let request = late_request(&mut log, boundary);
    log.reduce();

    let all = rows(&log.db);
    assert_eq!(all.len(), 1, "one checkpoint for one compaction: {all:#?}");
    assert_eq!(all[0].state, "PENDING");
    let (created, watermark): (i64, i64) = log
        .db
        .conn
        .query_row(
            "SELECT created_ms, event_watermark FROM checkpoints",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("checkpoint");
    assert_eq!((created, watermark), (boundary, request));
    assert!(capsule_of(&log.db, &all[0].checkpoint_id).contains("flaky logout test"));

    // Delivered on the next channel.
    let now = log.ts;
    let (d, _) = deliver_counted(&mut log, Channel::PostTool, "next-tool", now + 1_000);
    assert!(d.is_some());
}

#[test]
fn a_rebuild_never_acts_on_a_checkpoint_request_twice() {
    let mut log = working_session();
    let boundary = log.ts + 400;
    log.append(
        "SessionStart",
        None,
        Payload {
            source: Some("compact".into()),
            ..Default::default()
        },
    );
    late_request(&mut log, boundary);
    log.reduce();
    assert_eq!(count(&log.db, "SELECT COUNT(*) FROM checkpoints"), 1);
    // Another late event forces the session to be rebuilt again, and again.
    for k in 0..3 {
        log.append_late(
            "Stop",
            None,
            Payload {
                stop_hook_active: Some(false),
                ..Default::default()
            },
            boundary - 100 + k,
        );
        log.reduce();
    }
    assert_eq!(count(&log.db, "SELECT COUNT(*) FROM checkpoints"), 1);
    assert_eq!(count(&log.db, "SELECT COUNT(*) FROM continuations"), 1);
}

/// D111. A request ingested after a *newer* direct checkpoint superseded that
/// checkpoint and put an older compaction's continuation in its place.
#[test]
fn a_late_checkpoint_request_never_supersedes_a_newer_checkpoint() {
    let (mut log, b, created_b) = pending();
    late_request(&mut log, created_b - 500);
    log.reduce();
    assert_eq!(count(&log.db, "SELECT COUNT(*) FROM checkpoints"), 1);
    assert_eq!(row(&log.db, &b).state, "PENDING");
    let live = continuation::live(&log.db.conn, &log.session())
        .expect("live")
        .expect("one live");
    assert_eq!(live.checkpoint_id, b);
}

#[test]
fn a_late_checkpoint_request_never_revives_a_delivered_compaction() {
    let (mut log, b, created_b) = pending();
    assert!(
        deliver_counted(&mut log, Channel::SessionStart, "ss", created_b + 10)
            .0
            .is_some()
    );
    log.ts = created_b + 1_000;
    log.command_ok("cargo build", "Finished");
    reconcile(&mut log, created_b + 3_000);
    assert_eq!(row(&log.db, &b).state, "CONFIRMED");

    late_request(&mut log, created_b - 500);
    log.reduce();
    assert!(continuation::live(&log.db.conn, &log.session())
        .expect("live")
        .is_none());
    let now = log.ts;
    let (d, _) = deliver_counted(&mut log, Channel::PostTool, "later", now + 5_000);
    assert!(
        d.is_none(),
        "an older compaction's capsule arrived after the newer one"
    );
}

/// T6 holds for a request that arrives late: a compaction before `/clear` is
/// not delivered into the cleared context.
#[test]
fn a_late_checkpoint_request_is_not_delivered_after_clear() {
    for end in ["SessionStart", "SessionEnd"] {
        let mut log = working_session();
        let boundary = log.ts + 400;
        let payload = if end == "SessionStart" {
            Payload {
                source: Some("clear".into()),
                ..Default::default()
            }
        } else {
            Payload {
                reason: Some("clear".into()),
                ..Default::default()
            }
        };
        log.append(end, None, payload);
        log.reduce();
        late_request(&mut log, boundary);
        log.reduce();
        assert!(
            continuation::live(&log.db.conn, &log.session())
                .expect("live")
                .is_none(),
            "{end}(clear) left a live continuation from before it"
        );
        let now = log.ts;
        let (d, _) = deliver_counted(&mut log, Channel::PostTool, "after-clear", now + 5_000);
        assert!(d.is_none(), "{end}");
    }
}

// ---------------------------------------------- immutability (7F)

#[test]
fn a_frozen_checkpoint_never_changes_and_a_later_one_is_distinct() {
    let (mut log, a, created_a) = pending();
    let frozen = capsule_of(&log.db, &a);
    let hash = blake3::hash(frozen.as_bytes());

    // Later events, a rebuild forced by a late one, and a fresh snapshot of
    // the changed state.
    log.ts = created_a + 1_000;
    log.edit("src/a.rs", "v2\n");
    log.prompt("switch to exponential backoff");
    log.command_fail("pytest -x", 1, "FAILED tests/test_b.py::test_y");
    log.append_late(
        "Stop",
        None,
        Payload {
            stop_hook_active: Some(false),
            ..Default::default()
        },
        created_a - 100,
    );
    log.reduce();
    let current = log.capsule();
    assert!(
        current.contains("exponential backoff") && !frozen.contains("exponential backoff"),
        "the live state did move on:\n{current}"
    );

    for _ in 0..3 {
        let again = capsule_of(&log.db, &a);
        assert_eq!(blake3::hash(again.as_bytes()), hash);
    }
    let now = log.ts;
    let (d, _) = deliver_counted(&mut log, Channel::SessionStart, "ss", now + 1_000);
    assert_eq!(
        d.expect("delivered").capsule,
        frozen,
        "the frozen bytes are delivered"
    );

    let b = log.checkpoint();
    assert_ne!(a, b);
    assert_ne!(capsule_of(&log.db, &b), frozen);
    assert_eq!(capsule_of(&log.db, &a), frozen);
    assert!(log
        .db
        .conn
        .execute(
            "UPDATE checkpoints SET capsule = 'x' WHERE checkpoint_id = ?1",
            [&a]
        )
        .is_err());
}

// ----------------------------------------------- session boundary (7G)

#[test]
fn a_continuation_is_never_delivered_to_another_session() {
    let (mut log, a, created) = pending();
    let session_a = log.session();
    let before = row(&log.db, &a);

    // Session B starts in the same workspace, right away, and asks on every
    // channel.
    log.switch_session("session-b");
    log.prompt("an unrelated task in the same repository");
    for (k, channel) in CHANNELS.into_iter().enumerate() {
        for ts in [created + 1, created + 10_000] {
            let (d, emits) =
                deliver_for(&mut log, "session-b", channel, &format!("b-{k}-{ts}"), ts);
            assert!(d.is_none(), "{channel:?}");
            assert_eq!(emits, 0);
        }
    }
    continuation::reconcile(&mut log.db.conn, "session-b", created + 20_000).expect("reconcile");
    assert_eq!(row(&log.db, &a), before, "A's row is untouched by B");
    assert_eq!(
        count(
            &log.db,
            "SELECT COUNT(*) FROM continuations WHERE session_id = 'session-b'"
        ),
        0
    );

    // A, still its own, delivers to A only.
    let (d, _) = deliver_for(
        &mut log,
        &session_a,
        Channel::SessionStart,
        "a",
        created + 30_000,
    );
    assert_eq!(d.expect("A").checkpoint_id, a);
    assert_eq!(row(&log.db, &a).session_id, session_a);
}

// ================================================= through the real binary

fn base(env: &Env, session: &str, event: &str) -> Value {
    let mut p = env.base_payload(event);
    p["session_id"] = json!(session);
    p
}

/// Records, through the hooks, a session with an objective, an edit and a
/// failing test, and drains it.
fn record_work(env: &Env, session: &str) {
    env.write_file("src/a.rs", "v0\n");
    let file = env.project.join("src/a.rs");
    env.hook("user-prompt-submit", &{
        let mut p = base(env, session, "UserPromptSubmit");
        p["prompt"] = json!("fix the flaky logout test and keep the cookie behaviour intact");
        p["prompt_id"] = json!(format!("{session}-p1"));
        p
    })
    .assert_contract();
    env.write_file("src/a.rs", "v1\n");
    env.hook("post-tool-use", &{
        let mut p = base(env, session, "PostToolUse");
        p["tool_name"] = json!("Edit");
        p["tool_use_id"] = json!(format!("{session}-t1"));
        p["tool_input"] = json!({ "file_path": file, "old_string": "v0", "new_string": "v1" });
        p["tool_response"] = json!({ "filePath": file, "originalFile": "v0\n" });
        p
    })
    .assert_contract();
    env.hook("post-tool-use-failure", &{
        let mut p = base(env, session, "PostToolUseFailure");
        p["tool_name"] = json!("Bash");
        p["tool_use_id"] = json!(format!("{session}-t2"));
        p["tool_input"] = json!({ "command": "pytest -x" });
        p["error"] = json!("Exit code 1\nFAILED tests/test_a.py::test_x\n1 failed");
        p
    })
    .assert_contract();
    env.drain();
}

fn pre_compact(env: &Env, session: &str) -> common::HookOutput {
    let out = env.hook("pre-compact", &{
        let mut p = base(env, session, "PreCompact");
        p["trigger"] = json!("manual");
        p
    });
    out.assert_contract();
    out
}

fn session_start_with(
    env: &Env,
    session: &str,
    source: &str,
    vars: &[(&str, &str)],
) -> common::HookOutput {
    let out = env.hook_with_env(
        "session-start",
        &{
            let mut p = base(env, session, "SessionStart");
            p["source"] = json!(source);
            p
        },
        vars,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stderr.is_empty(), "{}", out.stderr);
    out
}

fn session_start(env: &Env, session: &str, source: &str) -> common::HookOutput {
    let out = session_start_with(env, session, source, &[]);
    out.assert_contract();
    out
}

fn tool_call(env: &Env, session: &str, id: &str) -> common::HookOutput {
    let out = env.hook("post-tool-use", &{
        let mut p = base(env, session, "PostToolUse");
        p["tool_name"] = json!("Read");
        p["tool_use_id"] = json!(id);
        p["tool_input"] = json!({ "file_path": env.project.join("src/a.rs") });
        p
    });
    out.assert_contract();
    out
}

fn prompt(env: &Env, session: &str, id: &str) -> common::HookOutput {
    let out = env.hook("user-prompt-submit", &{
        let mut p = base(env, session, "UserPromptSubmit");
        p["prompt"] = json!("what should we try next?");
        p["prompt_id"] = json!(id);
        p
    });
    out.assert_contract();
    out
}

fn only_checkpoint(db: &Db) -> String {
    let ids: Vec<String> = db
        .conn
        .prepare("SELECT checkpoint_id FROM checkpoints ORDER BY created_ms")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids.into_iter().next().unwrap()
}

fn delivered_capsule(out: &common::HookOutput) -> Option<String> {
    out.json().and_then(|v| {
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .map(str::to_string)
    })
}

/// 7H: PreCompact -> stored checkpoint -> SessionStart(compact) -> injection
/// -> the next tool call confirms. Records every observable at each step.
/// It proves what the hooks wrote and stored, not that a model read it.
#[test]
fn the_real_hook_path_checkpoints_delivers_once_and_confirms() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);

    let pre = pre_compact(&env, &s);
    let saved = pre.json().expect("checkpoint message");
    assert!(saved["systemMessage"]
        .as_str()
        .unwrap()
        .starts_with("\u{26a1} Velra checkpoint saved:"));
    let db = env.open_db();
    let id = only_checkpoint(&db);
    let stored = capsule_of(&db, &id);
    assert_eq!(row(&db, &id).state, "PENDING");
    drop(db);

    let start = session_start(&env, &s, "compact");
    let v = start.json().expect("delivery");
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        json!("SessionStart")
    );
    assert_eq!(delivered_capsule(&start).as_deref(), Some(stored.as_str()));
    assert!(v["systemMessage"]
        .as_str()
        .unwrap()
        .starts_with("\u{26a1} Velra restored: "));
    let db = env.open_db();
    let attached = row(&db, &id);
    assert_eq!(attached.state, "ATTACHED");
    assert_eq!(attached.attach_count, 1);
    assert_eq!(attached.attached_channel.as_deref(), Some("session_start"));
    assert_eq!(injections(&db, &id), 1);
    drop(db);

    // A duplicate start, the next prompt: nothing more is injected.
    assert!(session_start(&env, &s, "compact").stdout.is_empty());
    assert!(prompt(&env, &s, "after-1").stdout.is_empty());

    // The next tool call is the evidence that the session went on.
    assert!(tool_call(&env, &s, "t-after").stdout.is_empty());
    let db = env.open_db();
    let confirmed = row(&db, &id);
    assert_eq!(confirmed.state, "CONFIRMED", "{confirmed:#?}");
    let evidence: (String, i64) = db
        .conn
        .query_row(
            "SELECT hook_event, ts_ms FROM events WHERE id = ?1",
            [confirmed.confirm_event_id.expect("evidence id")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("evidence row");
    assert!(
        ["UserPromptSubmit", "PostToolUse", "Stop"].contains(&evidence.0.as_str())
            && evidence.1 > attached.attached_ms.unwrap(),
        "{evidence:?}"
    );
    assert_eq!(injections(&db, &id), 1);
    assert_eq!(confirmed.attach_count, 1);
    drop(db);

    // `velra status` reports it, and says what CONFIRMED does not prove.
    let status = |args: &[&str]| {
        let out = env
            .cmd()
            .env("VELRA_CLAUDE_VERSION", "2.1.280")
            .arg("status")
            .args(args)
            .output()
            .expect("status");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let v: Value = serde_json::from_str(&status(&["--json"])).expect("status json");
    let latest = &v["latest_continuation"];
    assert_eq!(latest["checkpoint"], json!(id));
    assert_eq!(latest["state"], json!("CONFIRMED"));
    assert_eq!(latest["channel"], json!("session_start"));
    assert_eq!(latest["attach_count"], json!(1));
    assert!(latest["meaning"]
        .as_str()
        .unwrap()
        .contains("not proof that a model read it"));
    let text = status(&[]);
    assert!(
        text.contains("Last continuation: ") && text.contains("CONFIRMED on session_start"),
        "{text}"
    );
}

/// 7B.8, D114. A session start that the watchdog ends after the capsule
/// reached stdout, and before the delivery was committed, used to leave the
/// continuation PENDING -- and the next hook delivered it again as if it were
/// fresh.
#[cfg(feature = "fault-injection")]
#[test]
fn a_hook_ended_between_its_write_and_its_commit_does_not_deliver_twice() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);
    pre_compact(&env, &s);
    let id = only_checkpoint(&env.open_db());

    let start = session_start_with(
        &env,
        &s,
        "compact",
        &[
            ("VELRA_TEST_WATCHDOG_MS", "400"),
            ("VELRA_TEST_STALL_AFTER_CONTINUATION_EMIT_MS", "1500"),
        ],
    );
    start.assert_contract();
    assert!(
        delivered_capsule(&start).is_some(),
        "the capsule was written"
    );

    let db = env.open_db();
    let r = row(&db, &id);
    assert_eq!(r.state, "ATTACHED", "a written capsule is recorded: {r:#?}");
    assert_eq!(injections(&db, &id), 1);
    drop(db);
    let next = tool_call(&env, &s, "t-next");
    assert!(
        delivered_capsule(&next).is_none(),
        "delivered a second time: {}",
        next.stdout
    );
}

/// 7I: the database is locked when the session starts. Claude Code goes on
/// (exit 0, no output) and the continuation is still there to deliver --
/// neither attached nor confirmed by a delivery that did not happen.
#[test]
fn a_locked_database_fails_open_and_keeps_the_continuation() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);
    pre_compact(&env, &s);
    let id = only_checkpoint(&env.open_db());

    let lock = env.open_db();
    lock.conn.execute_batch("BEGIN EXCLUSIVE").expect("lock");
    let start = session_start(&env, &s, "compact");
    assert!(start.stdout.is_empty(), "{}", start.stdout);
    lock.conn.execute_batch("ROLLBACK").expect("unlock");
    drop(lock);

    let db = env.open_db();
    assert_eq!(row(&db, &id).state, "PENDING");
    assert_eq!(injections(&db, &id), 0);
    drop(db);
    // The next channel delivers it.
    let next = tool_call(&env, &s, "t-after-lock");
    assert!(delivered_capsule(&next).is_some());
    assert_eq!(row(&env.open_db(), &id).state, "ATTACHED");
}

/// 7I: continuation state that is not what the state machine writes -- a
/// continuation whose checkpoint is gone, and one ATTACHED with no delivery
/// time. Every channel fails open, nothing is written, nothing is attached or
/// confirmed, and the session's next compaction is not wedged behind it.
#[test]
fn corrupt_continuation_state_is_never_delivered_or_confirmed() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);
    let db = env.open_db();
    db.conn
        .execute(
            "INSERT INTO continuations (checkpoint_id, session_id, state, attach_count, updated_ms) \
             VALUES ('ckpt_missing', ?1, 'PENDING', 0, 0)",
            [&s],
        )
        .expect("orphan continuation");
    drop(db);

    assert!(session_start(&env, &s, "compact").stdout.is_empty());
    assert!(tool_call(&env, &s, "t1-orphan").stdout.is_empty());
    assert!(prompt(&env, &s, "p1-orphan").stdout.is_empty());
    let db = env.open_db();
    let r = row(&db, "ckpt_missing");
    assert_eq!((r.state.as_str(), r.attach_count), ("PENDING", 0));
    assert_eq!(injections(&db, "ckpt_missing"), 0);
    drop(db);

    // The next compaction supersedes the orphan and is delivered normally.
    pre_compact(&env, &s);
    let db = env.open_db();
    assert_eq!(row(&db, "ckpt_missing").state, "SUPERSEDED");
    let id = db
        .conn
        .query_row("SELECT checkpoint_id FROM checkpoints", [], |r| {
            r.get::<_, String>(0)
        })
        .expect("checkpoint");
    drop(db);

    // ATTACHED with no delivery time: nothing can be evidence *after* it.
    let db = env.open_db();
    db.conn
        .execute(
            "UPDATE continuations SET state = 'ATTACHED', attach_count = 1, attached_ms = NULL, \
             attached_channel = 'user_prompt' WHERE checkpoint_id = ?1",
            [&id],
        )
        .expect("corrupt");
    drop(db);
    tool_call(&env, &s, "t2-corrupt");
    env.hook("stop", &base(&env, &s, "Stop")).assert_contract();
    let r = row(&env.open_db(), &id);
    assert_eq!(r.state, "ATTACHED", "{r:#?}");
    assert_eq!(r.confirm_event_id, None);
}

/// 7I: a panic inside SessionStart. Claude Code goes on; Velra's state is
/// exactly what it was.
#[cfg(feature = "fault-injection")]
#[test]
fn a_panic_in_session_start_fails_open_and_changes_no_state() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);
    pre_compact(&env, &s);
    let id = only_checkpoint(&env.open_db());
    let out = session_start_with(
        &env,
        &s,
        "compact",
        &[("VELRA_TEST_PANIC", "session-start")],
    );
    assert!(out.stdout.is_empty());
    let db = env.open_db();
    assert_eq!(row(&db, &id).state, "PENDING");
    assert_eq!(injections(&db, &id), 0);
}

/// 7G through the hooks: A compacts; B starts in the same workspace (as
/// `compact`, `resume` and `startup`) before A's delivery; A ends. B receives
/// nothing, A's continuation is untouched, and A still gets it on resume.
#[test]
fn another_session_in_the_workspace_never_receives_the_continuation() {
    let env = Env::new();
    let a = env.session.clone();
    record_work(&env, &a);
    pre_compact(&env, &a);
    let id = only_checkpoint(&env.open_db());

    for source in ["compact", "resume", "startup"] {
        let out = session_start(&env, "session-b", source);
        assert!(
            out.stdout.is_empty(),
            "B({source}) received: {}",
            out.stdout
        );
    }
    assert!(tool_call(&env, "session-b", "b-t1").stdout.is_empty());
    assert!(prompt(&env, "session-b", "b-p1").stdout.is_empty());

    // A ends (not by /clear): its continuation waits.
    env.hook("session-end", &{
        let mut p = base(&env, &a, "SessionEnd");
        p["reason"] = json!("prompt_input_exit");
        p
    })
    .assert_contract();
    let db = env.open_db();
    assert_eq!(row(&db, &id).state, "PENDING");
    assert_eq!(injections(&db, &id), 0);
    drop(db);

    let resumed = session_start(&env, &a, "resume");
    assert!(delivered_capsule(&resumed).is_some());
    assert_eq!(row(&env.open_db(), &id).session_id, a);
}

/// 7B.6: a session that ends before confirmation. `/clear` and logout expire
/// the continuation, and a resumed session receives nothing; any other end
/// leaves it attached, and the resumed session's activity confirms it.
#[test]
fn a_session_ending_before_confirmation_is_expired_only_by_clear_or_logout() {
    for (reason, expected) in [
        ("clear", "EXPIRED"),
        ("logout", "EXPIRED"),
        ("prompt_input_exit", "ATTACHED"),
        ("other", "ATTACHED"),
    ] {
        let env = Env::new();
        let s = env.session.clone();
        record_work(&env, &s);
        pre_compact(&env, &s);
        let id = only_checkpoint(&env.open_db());
        assert!(delivered_capsule(&session_start(&env, &s, "compact")).is_some());
        env.hook("session-end", &{
            let mut p = base(&env, &s, "SessionEnd");
            p["reason"] = json!(reason);
            p
        })
        .assert_contract();
        assert_eq!(row(&env.open_db(), &id).state, expected, "{reason}");

        let resumed = session_start(&env, &s, "resume");
        assert!(resumed.stdout.is_empty(), "{reason}: {}", resumed.stdout);
        tool_call(&env, &s, &format!("t-{reason}"));
        let after = row(&env.open_db(), &id);
        let want = if expected == "ATTACHED" {
            "CONFIRMED"
        } else {
            "EXPIRED"
        };
        assert_eq!(after.state, want, "{reason}");
        assert_eq!(injections(&env.open_db(), &id), 1);
    }
}

/// D110 through the hooks: the same prompt delivered to two copies of the
/// hook (two registrations of Velra for one event -- different binary paths
/// in user and project settings are not deduplicated by Claude Code). One
/// writes the capsule; the other writes nothing.
#[test]
fn a_duplicated_hook_invocation_writes_the_capsule_once() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);
    pre_compact(&env, &s);
    let first = prompt(&env, &s, "p-dup");
    let second = prompt(&env, &s, "p-dup");
    assert!(delivered_capsule(&first).is_some());
    assert!(second.stdout.is_empty(), "{}", second.stdout);
    let db = env.open_db();
    let id = only_checkpoint(&db);
    assert_eq!(injections(&db, &id), 1);
    assert_eq!(row(&db, &id).attach_count, 1);
}

/// The model-facing record says it is "this session's own" prompts. On the
/// continuation path that has to be literally true: the capsule delivered to
/// a session is a checkpoint of that session, in that workspace.
#[test]
fn the_delivered_capsule_is_this_sessions_own_checkpoint() {
    let env = Env::new();
    let s = env.session.clone();
    record_work(&env, &s);
    pre_compact(&env, &s);
    let start = session_start(&env, &s, "compact");
    let capsule = delivered_capsule(&start).expect("delivered");
    let db = env.open_db();
    let (session, project, text): (String, String, String) = db
        .conn
        .query_row(
            "SELECT session_id, project_id, capsule FROM checkpoints",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("checkpoint");
    assert_eq!(
        (session.as_str(), project.as_str()),
        (s.as_str(), env.project_id().as_str())
    );
    assert_eq!(text, capsule);
    let checkpoint_attr = format!("checkpoint=\"{}\"", only_checkpoint(&db));
    assert!(capsule.contains(&checkpoint_attr), "{capsule}");
}
