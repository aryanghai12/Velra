//! Claim safety of the staged capsule (Phase 6), on the real filesystem.
//!
//! Each race is opened deterministically -- a second actor runs inside the
//! first one's `emit`, or in `staging::BEFORE_CLAIM` between its validation
//! and its claim -- rather than hoped for with sleeps. The crash boundary is
//! the real hook binary killed by its own watchdog between writing the
//! capsule to stdout and settling the claim.
//!
//! Before Phase 6, every test in the first three sections failed: a restage
//! during a delivery was deleted by the older claim's cleanup, an expired
//! claim marker was broken so that a slow holder and the breaker (or two
//! breakers) both delivered, and a hook killed after its emit had its capsule
//! delivered a second time once the marker aged.

mod common;

use common::Env;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use velra_core::staging::{self, source::STARTUP, ClaimError, Slot, StagedCapsule};

const WS: &str = "0123456789abcdef";

fn sample(ws: &str, created_ms: i64, who: &str) -> StagedCapsule {
    let capsule =
        format!("<VELRA_WORKSPACE_STATE v=\"1\">\n[FIRST_MESSAGE] {who}\n</VELRA_WORKSPACE_STATE>");
    StagedCapsule {
        version: staging::STAGED_VERSION,
        workspace_id: ws.into(),
        workspace_root: "/p".into(),
        intent: staging::NEW_SESSION.name.into(),
        deliver_on: staging::NEW_SESSION.deliver_on(),
        source_session_id: who.into(),
        source_checkpoint_id: None,
        created_ms,
        render_version: 1,
        tokens: 1,
        content_hash: StagedCapsule::compute_hash(&capsule),
        summary: who.into(),
        capsule,
    }
}

fn now() -> i64 {
    velra_core::time::now_ms()
}

fn dir(home: &Path) -> PathBuf {
    staging::staged_dir(home, WS)
}

fn files(home: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir(home))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Runs `f` once, in this thread, between the next claimant's validation and
/// its claim.
fn before_next_claim(f: impl FnOnce() + 'static) {
    let f = RefCell::new(Some(f));
    staging::BEFORE_CLAIM.with(|h| {
        *h.borrow_mut() = Some(Box::new(move || {
            if let Some(f) = f.borrow_mut().take() {
                f()
            }
        }))
    });
}

fn clear_hook() {
    staging::BEFORE_CLAIM.with(|h| *h.borrow_mut() = None);
}

// ------------------------------------------------------------ 1. restage

/// Stage A, begin delivering A, stage B before A's cleanup, finish A: A is
/// delivered once and B is still staged, then delivered to the next session.
#[test]
fn a_restage_during_a_delivery_survives_the_older_claims_cleanup() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let t = now();
    staging::stage(&dir(home), &sample(WS, t, "A")).unwrap();

    let got = staging::claim_with(home, WS, STARTUP, t + 1, |c| {
        assert_eq!(c.source_session_id, "A");
        staging::stage(&dir(home), &sample(WS, t + 2, "B")).unwrap();
        true
    })
    .expect("A delivered");
    assert_eq!(got.source_session_id, "A");
    assert_eq!(
        staging::peek(&dir(home)).map(|c| c.source_session_id),
        Some("B".to_string()),
        "B must survive A's cleanup: {:?}",
        files(home)
    );
    assert_eq!(
        staging::claim(home, WS, STARTUP, t + 3)
            .unwrap()
            .source_session_id,
        "B"
    );
    assert!(files(home).is_empty(), "{:?}", files(home));
}

/// A restage between a claimant's reading of A and its claim supersedes A:
/// the claimant's claim on A leads nowhere, and it delivers B instead.
#[test]
fn a_restage_before_the_claim_is_delivered_in_the_superseded_ones_place() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_path_buf();
    let t = now();
    staging::stage(&dir(&home), &sample(WS, t, "A")).unwrap();
    let h = home.clone();
    before_next_claim(move || {
        staging::stage(&dir(&h), &sample(WS, t + 2, "B")).unwrap();
    });
    let emitted = RefCell::new(Vec::new());
    let got = staging::claim_with(&home, WS, STARTUP, t + 3, |c| {
        emitted.borrow_mut().push(c.source_session_id.clone());
        true
    });
    clear_hook();
    assert_eq!(got.map(|c| c.source_session_id), Ok("B".to_string()));
    assert_eq!(
        *emitted.borrow(),
        ["B"],
        "A is never emitted once superseded"
    );
    assert!(files(&home).is_empty(), "{:?}", files(&home));
}

/// The cleanup of a delivered record removes exactly that record and its
/// claim: every other name in the directory is left as it was.
#[test]
fn cleanup_removes_only_the_claimed_record() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let t = now();
    staging::stage(&dir(home), &sample(WS, t, "A")).unwrap();
    let mut during = Vec::new();
    staging::claim_with(home, WS, STARTUP, t + 1, |_| {
        staging::stage(&dir(home), &sample(WS, t + 2, "B")).unwrap();
        // An unrelated file a user or another tool left there.
        std::fs::write(dir(home).join("notes.txt"), "keep").unwrap();
        during = files(home);
        true
    })
    .unwrap();
    let b = staging::newest(&dir(home)).expect("B");
    let expected: Vec<String> = ["notes.txt".to_string()]
        .into_iter()
        .chain(b.path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let mut expected = expected;
    expected.sort();
    assert_eq!(files(home), expected, "during the emit: {during:?}");
}

// --------------------------------------------- 2. at most one claimant

/// Process A holds the claim and is slow; its claim is older than the
/// reporting threshold (a minute). Process B must not take it over: A
/// delivers, B does not, and nothing is delivered twice.
#[test]
fn a_slow_holder_past_the_threshold_is_never_joined_by_a_second_owner() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let t = now();
    staging::stage(&dir(home), &sample(WS, t, "A")).unwrap();
    let emits = Cell::new(0);
    let mut b = None;
    let a = staging::claim_with(home, WS, STARTUP, t, |_| {
        emits.set(emits.get() + 1);
        // B arrives long after A claimed.
        b = Some(staging::claim_with(
            home,
            WS,
            STARTUP,
            t + staging::CLAIM_LEASE_MS + 5_000,
            |_| {
                emits.set(emits.get() + 1);
                true
            },
        ));
        true
    });
    assert!(a.is_ok());
    assert!(
        matches!(b, Some(Err(ClaimError::Interrupted { .. }))),
        "{b:?}"
    );
    assert_eq!(emits.get(), 1, "delivered exactly once");
}

/// Two claimants find the same old claim (its holder died): neither takes it
/// over, so neither delivers. The old layout let both break the marker.
#[test]
fn two_claimants_of_a_dead_holders_record_both_refuse() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let t = now();
    staging::stage(&dir(home), &sample(WS, t, "A")).unwrap();
    let r = staging::newest(&dir(home)).unwrap();
    std::fs::write(r.claim_path(), format!("at={t} pid=999999")).unwrap();
    let later = t + staging::CLAIM_LEASE_MS + 1_000;
    let emits = Cell::new(0);
    let mut second = None;
    let first = staging::claim_with(home, WS, STARTUP, later, |_| {
        emits.set(emits.get() + 1);
        second = Some(staging::claim(home, WS, STARTUP, later));
        true
    });
    assert!(
        matches!(first, Err(ClaimError::Interrupted { .. })),
        "{first:?}"
    );
    assert_eq!(second, None, "the first never reached an emit");
    assert!(matches!(
        staging::claim(home, WS, STARTUP, later),
        Err(ClaimError::Interrupted { .. })
    ));
    assert_eq!(emits.get(), 0);
    assert!(r.path.is_file() && r.claim_path().is_file());
}

/// Many threads, one record, a restage part-way through: every record is
/// delivered at most once, and the newest one exactly once.
#[test]
fn concurrent_claimants_and_a_restage_deliver_each_record_at_most_once() {
    for round in 0..20 {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        let t = now();
        staging::stage(&dir(&home), &sample(WS, t, "A")).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let delivered = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for i in 0..8 {
                let (home, barrier, delivered) = (&home, barrier.clone(), &delivered);
                s.spawn(move || {
                    barrier.wait();
                    if let Ok(c) = staging::claim(home, WS, STARTUP, t + 1 + i) {
                        delivered.lock().unwrap().push(c.source_session_id);
                    }
                });
            }
            let (home, barrier) = (&home, barrier.clone());
            s.spawn(move || {
                barrier.wait();
                staging::stage(&dir(home), &sample(WS, t + 50, "B")).unwrap();
            });
        });
        // Whatever the interleaving, B is either delivered or still staged.
        if let Ok(c) = staging::claim(&home, WS, STARTUP, t + 100) {
            delivered.lock().unwrap().push(c.source_session_id);
        }
        let mut got = delivered.into_inner().unwrap();
        got.sort();
        let a = got.iter().filter(|s| *s == "A").count();
        let b = got.iter().filter(|s| *s == "B").count();
        assert!(a <= 1, "round {round}: A delivered {a} times: {got:?}");
        assert_eq!(b, 1, "round {round}: B delivered {b} times: {got:?}");
        assert!(files(&home).is_empty(), "round {round}: {:?}", files(&home));
    }
}

// -------------------------------------- 3. crash between emit and cleanup

/// The claimant dies after its emit and before its cleanup (a panic stands
/// in for the process ending): the record and its claim stay, and no later
/// claim delivers it again, before or after the threshold.
#[test]
fn a_claimant_that_dies_after_its_emit_is_not_followed_by_a_second_delivery() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let t = now();
    staging::stage(&dir(home), &sample(WS, t, "A")).unwrap();
    let emits = Cell::new(0);
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = staging::claim_with(home, WS, STARTUP, t + 1, |_| {
            emits.set(emits.get() + 1);
            panic!("the process ends after writing the capsule");
        });
    }));
    assert!(died.is_err());
    let r = staging::newest(&dir(home)).expect("the record stays");
    assert!(r.claim_path().is_file(), "and so does its claim");

    for (at, expect_interrupted) in [(t + 2, false), (t + staging::CLAIM_LEASE_MS + 2, true)] {
        let out = staging::claim_with(home, WS, STARTUP, at, |_| {
            emits.set(emits.get() + 1);
            true
        });
        match out {
            Err(ClaimError::Interrupted { .. }) if expect_interrupted => {}
            Err(ClaimError::Busy) if !expect_interrupted => {}
            other => panic!("at {at}: {other:?}"),
        }
    }
    assert_eq!(emits.get(), 1, "delivered once, never twice");
    assert!(matches!(
        staging::slot(&dir(home), t + staging::CLAIM_LEASE_MS + 2),
        Slot::Claimed {
            interrupted: true,
            capsule: Some(_)
        }
    ));
}

fn startup_payload(env: &Env, session: &str) -> Value {
    json!({
        "session_id": session,
        "hook_event_name": "SessionStart",
        "source": STARTUP,
        "cwd": env.project.to_string_lossy(),
    })
}

/// The same boundary through the real binary: the hook writes the capsule to
/// stdout, stalls, and its watchdog ends the process before the claim is
/// settled. The next session start -- with the claim aged past the threshold
/// -- delivers nothing, `velra status` says what happened, and a restage is
/// delivered normally.
#[test]
fn the_hook_killed_by_its_watchdog_after_emitting_does_not_deliver_twice() {
    let env = Env::new();
    let ws = env.project_id();
    let sdir = staging::staged_dir(&env.home, &ws);
    staging::stage(&sdir, &sample(&ws, now(), "A")).unwrap();

    let first = env
        .cmd()
        .env("VELRA_TEST_WATCHDOG_MS", "3000")
        .env("VELRA_TEST_STALL_AFTER_STAGED_EMIT_MS", "30000")
        .args(["hook", "session-start"])
        .write_stdin(startup_payload(&env, "dest-1").to_string())
        .output()
        .expect("run hook");
    assert_eq!(first.status.code(), Some(0));
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&first.stdout).trim_end())
        .expect("the first session start delivered");
    assert!(v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("[FIRST_MESSAGE] A"));
    let r = staging::newest(&sdir).expect("the watchdog left the record");
    assert!(r.claim_path().is_file(), "and its claim");

    // Age the claim past the threshold, as a minute's wait would.
    let text = std::fs::read_to_string(r.claim_path()).unwrap();
    let at: i64 = text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("at="))
        .and_then(|v| v.parse().ok())
        .expect("claim time");
    std::fs::write(
        r.claim_path(),
        format!("at={}", at - staging::CLAIM_LEASE_MS - 60_000),
    )
    .unwrap();

    let second = env
        .cmd()
        .args(["hook", "session-start"])
        .write_stdin(startup_payload(&env, "dest-2").to_string())
        .output()
        .expect("run hook");
    assert_eq!(second.status.code(), Some(0));
    assert!(second.stderr.is_empty());
    assert!(
        second.stdout.is_empty(),
        "delivered a second time: {}",
        String::from_utf8_lossy(&second.stdout)
    );

    let status = env.cmd().args(["status"]).output().expect("status");
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(
        text.contains("did not finish") && text.contains("velra restore"),
        "{text}"
    );

    staging::stage(&sdir, &sample(&ws, now(), "A again")).unwrap();
    let third = env
        .cmd()
        .args(["hook", "session-start"])
        .write_stdin(startup_payload(&env, "dest-3").to_string())
        .output()
        .expect("run hook");
    assert!(String::from_utf8_lossy(&third.stdout).contains("A again"));
    assert!(staging::records(&sdir).is_empty());
}

// ------------------------------------------- 6F version, hash, staleness

/// Each invalid newest record, the exact outcome, and what stays on disk.
/// None of them is delivered, and none falls back to an older record.
#[test]
fn every_invalid_record_has_one_stated_outcome_and_no_fallback() {
    let t = now();
    type Setup = fn(&Path, i64) -> PathBuf;
    type Check = fn(&ClaimError) -> bool;
    type Case = (&'static str, Setup, Check, bool);
    let cases: [Case; 6] = [
        (
            "malformed json",
            |d, t| over(d, t, "{ not json"),
            |e| *e == ClaimError::Malformed,
            false,
        ),
        (
            "unsupported version",
            |d, t| over(d, t, r#"{"version":999}"#),
            |e| *e == ClaimError::Malformed,
            false,
        ),
        (
            "hash mismatch",
            |d, t| {
                let mut c = sample(WS, t, "tampered");
                c.capsule.push_str("\nINJECTED");
                staging::stage(d, &c).unwrap()
            },
            |e| *e == ClaimError::Corrupt,
            true,
        ),
        (
            "expired capsule",
            |d, t| staging::stage(d, &sample(WS, t - staging::STAGED_TTL_MS - 1, "old")).unwrap(),
            |e| matches!(e, ClaimError::Stale { .. }),
            false,
        ),
        (
            "wrong workspace",
            |d, t| staging::stage(d, &sample("ffffffffffffffff", t, "foreign")).unwrap(),
            |e| matches!(e, ClaimError::WrongWorkspace { .. }),
            true,
        ),
        (
            "expired claim",
            |d, t| {
                let p = staging::stage(d, &sample(WS, t, "claimed")).unwrap();
                let r = staging::newest(d).unwrap();
                std::fs::write(
                    r.claim_path(),
                    format!("at={}", t - staging::CLAIM_LEASE_MS - 1),
                )
                .unwrap();
                p
            },
            |e| matches!(e, ClaimError::Interrupted { .. }),
            true,
        ),
    ];
    for (name, setup, is_expected, kept) in cases {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        // An older, valid record the newest one superseded.
        let older = staging::stage(&dir(home), &sample(WS, t, "older")).unwrap();
        let older_bytes = std::fs::read(&older).unwrap();
        let newest = setup(&dir(home), t);
        std::fs::write(&older, &older_bytes).unwrap();

        let emitted = Cell::new(false);
        let out = staging::claim_with(home, WS, STARTUP, t + 1, |_| {
            emitted.set(true);
            true
        });
        let err = out.expect_err(name);
        assert!(is_expected(&err), "{name}: {err:?}");
        assert!(!emitted.get(), "{name}: emitted");
        assert_eq!(newest.is_file(), kept, "{name}: newest kept?");
        // Never a fallback: the older record is not delivered on a retry.
        let again = staging::claim(home, WS, STARTUP, t + 2);
        assert!(
            again
                .as_ref()
                .map_or(true, |c| c.source_session_id != "older"),
            "{name}: fell back to the older record: {again:?}"
        );
    }
}

/// A stale record with a newer restage: the restage is delivered and the
/// stale one goes with it.
#[test]
fn a_stale_capsule_with_a_newer_restage_delivers_the_restage() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let t = now();
    staging::stage(
        &dir(home),
        &sample(WS, t - staging::STAGED_TTL_MS - 1, "stale"),
    )
    .unwrap();
    staging::stage(&dir(home), &sample(WS, t, "fresh")).unwrap();
    assert_eq!(
        staging::claim(home, WS, STARTUP, t + 1)
            .unwrap()
            .source_session_id,
        "fresh"
    );
    assert!(files(home).is_empty(), "{:?}", files(home));
}

fn over(d: &Path, t: i64, text: &str) -> PathBuf {
    let p = staging::stage(d, &sample(WS, t, "overwritten")).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}
