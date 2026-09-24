//! Event order, replay and logical time (Phase 2 hardening).
//!
//! The reducer's cursor walks `events` in row-id order, which is the order
//! rows were *ingested*. A hook that cannot reach the database spools its
//! event, and the event is ingested later -- after rows that happened after
//! it. Every test here builds a ledger whose ingestion order differs from the
//! order things happened in, and asserts that the semantic state (intents,
//! constraints, file history, edits, dead ends, commands, turn numbers) is
//! the one the logical order gives -- compared as content, never as row ids.
//!
//! The model under test is `velra_core::order`: directly appended events keep
//! ingestion order; a spooled event (`Payload::spooled`) is placed by its
//! timestamp among the direct events ingested before it.

mod common;

use common::{Env, Log, BASE_MS};
use serde_json::json;
use velra_core::db::{Db, Role};
use velra_core::event::{dedupe_key, NewEvent, Payload, PromptConstraint, PromptFacts};
use velra_core::{eventlog, reducer};

// ------------------------------------------------------------- harness

/// Everything the projection says about a session, as content, in the
/// order the projection holds it.
#[derive(Debug, PartialEq, Eq)]
struct State {
    epoch: i64,
    intents: Vec<(i64, String, String, bool)>,
    constraints: Vec<(i64, String, String, i64)>,
    versions: Vec<(String, String, String)>,
    edits: Vec<(String, String, Option<String>)>,
    dead_ends: Vec<(String, String, i64)>,
    commands: Vec<(String, String)>,
    files: Vec<(String, i64, i64, i64)>,
}

fn state(db: &Db, session: &str) -> State {
    let conn = &db.conn;
    let rows = |sql: &str| -> Vec<Vec<rusqlite::types::Value>> {
        let mut stmt = conn.prepare(sql).expect("prepare");
        let n = stmt.column_count();
        stmt.query_map([session], |r| {
            (0..n)
                .map(|i| r.get::<_, rusqlite::types::Value>(i))
                .collect()
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
    };
    use rusqlite::types::Value as V;
    let s = |v: &V| match v {
        V::Text(t) => t.clone(),
        V::Null => String::new(),
        other => format!("{other:?}"),
    };
    let i = |v: &V| match v {
        V::Integer(n) => *n,
        _ => -1,
    };
    let o = |v: &V| match v {
        V::Text(t) => Some(t.clone()),
        _ => None,
    };
    State {
        epoch: rows("SELECT epoch FROM sessions WHERE session_id = ?1")
            .first()
            .map_or(0, |r| i(&r[0])),
        intents: rows(
            "SELECT epoch, level, text, superseded_ms IS NULL FROM intents WHERE session_id = ?1 ORDER BY id",
        )
        .iter()
        .map(|r| (i(&r[0]), s(&r[1]), s(&r[2]), i(&r[3]) == 1))
        .collect(),
        constraints: rows(
            "SELECT epoch, text, cue, prompt_ordinal FROM constraints WHERE session_id = ?1 ORDER BY id",
        )
        .iter()
        .map(|r| (i(&r[0]), s(&r[1]), s(&r[2]), i(&r[3])))
        .collect(),
        versions: rows(
            "SELECT path, source, content_hash FROM file_versions WHERE session_id = ?1 ORDER BY id",
        )
        .iter()
        .map(|r| (s(&r[0]), s(&r[1]), s(&r[2])))
        .collect(),
        edits: rows("SELECT path, status, mechanism FROM edits WHERE session_id = ?1 ORDER BY id")
            .iter()
            .map(|r| (s(&r[0]), s(&r[1]), o(&r[2])))
            .collect(),
        dead_ends: rows(
            "SELECT path, mechanism, reapplied FROM dead_ends WHERE session_id = ?1 ORDER BY id",
        )
        .iter()
        .map(|r| (s(&r[0]), s(&r[1]), i(&r[2])))
        .collect(),
        commands: rows("SELECT signature, outcome FROM commands WHERE session_id = ?1 ORDER BY id")
            .iter()
            .map(|r| (s(&r[0]), s(&r[1])))
            .collect(),
        files: rows(
            "SELECT path, reads, edits, in_failure FROM file_stats WHERE session_id = ?1 ORDER BY path",
        )
        .iter()
        .map(|r| (s(&r[0]), i(&r[1]), i(&r[2]), i(&r[3])))
        .collect(),
    }
}

/// Appends an event with an explicit clock, directly (not spooled).
fn append_at(log: &mut Log, hook_event: &str, payload: Payload, ts: i64, key: &str) -> i64 {
    append_raw(log, hook_event, &payload.to_json(), ts, key)
}

/// Appends an event whose payload is given as raw JSON.
fn append_raw(log: &mut Log, hook_event: &str, payload: &str, ts: i64, key: &str) -> i64 {
    let session = log.env.session.clone();
    let ev = NewEvent {
        dedupe_key: dedupe_key(hook_event, &session, None, Some(key), ts, None),
        session_id: session,
        project_id: log.env.project_id(),
        agent_id: None,
        hook_event: hook_event.to_string(),
        tool_name: None,
        tool_use_id: None,
        ts_ms: ts,
        payload: payload.to_string(),
        project: Some(log.env.project_info()),
    };
    eventlog::append(&mut log.db.conn, &ev)
        .expect("append")
        .unwrap_or(0)
}

fn prompt(text: &str) -> Payload {
    Payload {
        prompt: Some(text.into()),
        ..Default::default()
    }
}

fn spooled_prompt(text: &str) -> Payload {
    Payload {
        spooled: Some(true),
        ..prompt(text)
    }
}

fn live(st: &State, level: &str) -> Option<String> {
    st.intents
        .iter()
        .rev()
        .find(|(e, l, _, live)| *e == st.epoch && l == level && *live)
        .map(|(_, _, t, _)| t.clone())
}

/// Clears the projection and the cursor, as a fresh reducer over the same
/// event log would find it.
fn reset_projection(db: &Db) {
    db.conn
        .execute_batch(
            "DELETE FROM intents; DELETE FROM constraints; DELETE FROM file_versions; \
             DELETE FROM edits; DELETE FROM dead_ends; DELETE FROM commands; \
             DELETE FROM file_stats; DELETE FROM sessions; \
             UPDATE reducer_cursor SET last_event_id = 0;",
        )
        .expect("reset");
}

const A: &str = "task: Fix the flaky login test in tests/test_auth.py. Do not modify the fixtures.";
const B: &str = "Also check tests/test_session.py for the same failure mode.";
const C: &str = "subtask: write the regression test for the login flow";

// ------------------------------------------------ 2/4. out-of-order intent

/// Logical A -> B -> C, ingested B, C, A (A spooled).
#[test]
fn a_spooled_task_ingested_after_later_prompts_keeps_its_place() {
    let mut base = Log::new();
    append_at(&mut base, "UserPromptSubmit", prompt(A), BASE_MS + 10, "a");
    append_at(&mut base, "UserPromptSubmit", prompt(B), BASE_MS + 20, "b");
    append_at(&mut base, "UserPromptSubmit", prompt(C), BASE_MS + 30, "c");
    base.reduce();
    let want = state(&base.db, &base.session());

    let mut log = Log::new();
    append_at(&mut log, "UserPromptSubmit", prompt(B), BASE_MS + 20, "b");
    append_at(&mut log, "UserPromptSubmit", prompt(C), BASE_MS + 30, "c");
    log.reduce(); // B and C are reduced before A arrives.
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt(A),
        BASE_MS + 10,
        "a",
    );
    log.reduce();
    let got = state(&log.db, &log.session());

    assert_eq!(got, want);
    assert_eq!(got.epoch, 2);
    assert_eq!(
        live(&got, "ROOT").as_deref(),
        Some("Fix the flaky login test in tests/test_auth.py. Do not modify the fixtures.")
    );
    assert_eq!(live(&got, "LATEST").as_deref(), Some(B));
    assert_eq!(
        live(&got, "SUBTASK").as_deref(),
        Some("write the regression test for the login flow")
    );
    // The rule is the user's first turn, and it belongs to A's epoch.
    assert_eq!(
        got.constraints,
        vec![(2, "Do not modify the fixtures.".into(), "do not ".into(), 0)]
    );
}

/// Logical A -> B, ingested B -> A: the objective is A, not the prompt that
/// happened to be reduced first.
#[test]
fn a_late_first_prompt_is_still_the_objective() {
    let first = "Fix the retry bug in src/payments/retry.py so the key survives.";
    let second = "Look at src/payments/gateway.py next, it calls the retry.";
    let mut log = Log::new();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt(second),
        BASE_MS + 20,
        "b",
    );
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt(first),
        BASE_MS + 10,
        "a",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    assert_eq!(live(&got, "ROOT").as_deref(), Some(first));
    assert_eq!(live(&got, "LATEST").as_deref(), Some(second));
}

/// Turn 0 = task A, turn 1 = task B, ingested turn 1 then turn 0: the live
/// task is B. A late ingestion artifact never becomes newer intent.
#[test]
fn an_older_task_arriving_late_does_not_replace_the_newer_one() {
    let mut log = Log::new();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("task: migrate the settings loader to TOML"),
        BASE_MS + 20,
        "b",
    );
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("task: fix the flaky login test in tests/test_auth.py"),
        BASE_MS + 10,
        "a",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    assert_eq!(got.epoch, 3);
    assert_eq!(
        live(&got, "ROOT").as_deref(),
        Some("migrate the settings loader to TOML")
    );
    // The older task is recorded in the epoch before it, where it happened.
    assert!(got.intents.iter().any(|(e, l, t, _)| *e == 2
        && l == "ROOT"
        && t == "fix the flaky login test in tests/test_auth.py"));
}

/// A rephrased objective and the prompt that follows it, ingested backwards.
#[test]
fn a_late_prompt_between_two_others_is_folded_in_between() {
    let mut log = Log::new();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the login test in tests/test_auth.py please."),
        BASE_MS + 10,
        "a",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Now check the session cookie handling."),
        BASE_MS + 30,
        "c",
    );
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("Actually, start with the logout path."),
        BASE_MS + 20,
        "b",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    let latest: Vec<&str> = got
        .intents
        .iter()
        .filter(|(_, l, _, _)| l == "LATEST")
        .map(|(_, _, t, _)| t.as_str())
        .collect();
    assert_eq!(
        latest,
        vec![
            "Actually, start with the logout path.",
            "Now check the session cookie handling."
        ]
    );
    assert_eq!(
        live(&got, "LATEST").as_deref(),
        Some("Now check the session cookie handling.")
    );
}

// ------------------------------------------------------ 3. prompt ordinal

/// The turn number is the prompt's position among the user's turns in
/// logical order: injected-only prompts are not turns, an unprocessed prompt
/// is, a late prompt is numbered where it happened, and a duplicate is one.
#[test]
fn prompt_ordinal_counts_turns_in_logical_order() {
    let mut log = Log::new();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("<task-notification><task-id>t1</task-id></task-notification>"),
        BASE_MS + 5,
        "n",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the retry in src/payments/retry.py."),
        BASE_MS + 10,
        "p1",
    );
    let unprocessed = Payload {
        prompt_truncated: Some(true),
        prompt_facts: Some(PromptFacts {
            v: 1,
            authored_bytes: 20 << 20,
            unprocessed: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    append_at(&mut log, "UserPromptSubmit", unprocessed, BASE_MS + 30, "u");
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Never delete the audit log."),
        BASE_MS + 40,
        "p3",
    );
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("Do not modify the fixtures in tests/data."),
        BASE_MS + 20,
        "p2",
    );
    // The same late event spooled twice: one row.
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("Do not modify the fixtures in tests/data."),
        BASE_MS + 20,
        "p2",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    let ordinals: Vec<(String, i64)> = got.constraints.iter().map(|c| (c.1.clone(), c.3)).collect();
    assert_eq!(
        ordinals,
        vec![
            ("Do not modify the fixtures in tests/data.".to_string(), 1),
            ("Never delete the audit log.".to_string(), 3),
        ]
    );
}

// ------------------------------------------------------ 5. prompt_facts

#[test]
fn prompt_facts_of_an_unknown_version_are_not_read_as_known() {
    let mut log = Log::new();
    // A newer build's record: this build must not take its constraint list
    // as a version-1 list. It falls back to the stored prompt.
    let newer = Payload {
        prompt: Some("Do not touch the vendored code in third_party.".into()),
        prompt_facts: Some(PromptFacts {
            v: velra_core::event::PROMPT_FACTS_VERSION + 1,
            constraints: vec![PromptConstraint {
                text: "Invented by a newer build.".into(),
                cue: "must ".into(),
                kind: "requirement".into(),
                at: 0,
                basis: "cue".into(),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    append_at(&mut log, "UserPromptSubmit", newer, BASE_MS + 10, "v2");
    // A newer build's "unprocessed" flag is not counted as a turn either.
    let newer_unprocessed = Payload {
        prompt_facts: Some(PromptFacts {
            v: velra_core::event::PROMPT_FACTS_VERSION + 1,
            unprocessed: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    append_at(
        &mut log,
        "UserPromptSubmit",
        newer_unprocessed,
        BASE_MS + 20,
        "v2u",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Never delete the audit log."),
        BASE_MS + 30,
        "p",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    let texts: Vec<(&str, i64)> = got
        .constraints
        .iter()
        .map(|c| (c.1.as_str(), c.3))
        .collect();
    assert_eq!(
        texts,
        vec![
            ("Do not touch the vendored code in third_party.", 0),
            ("Never delete the audit log.", 1),
        ]
    );
}

#[test]
fn malformed_prompt_facts_do_not_discard_the_prompt() {
    let mut log = Log::new();
    for (n, facts) in [
        json!({"v": "one", "constraints": []}),
        json!({"constraints": [{"text": "x"}]}),
        json!("not an object"),
        json!({"v": 0, "authored_bytes": 5, "scanned_bytes": 5,
               "constraints": [{"text": "Zero is not a version.", "cue": "must ", "kind": "requirement", "at": 0, "basis": "cue"}]}),
    ]
    .into_iter()
    .enumerate()
    {
        let payload = json!({
            "prompt": format!("Fix the retry in src/payments/retry.py, attempt {n}. Do not rename the module."),
            "prompt_facts": facts,
        });
        append_raw(&mut log, "UserPromptSubmit", &payload.to_string(), BASE_MS + 10 * n as i64, &format!("m{n}"));
    }
    log.reduce();
    let got = state(&log.db, &log.session());
    assert_eq!(
        live(&got, "ROOT").as_deref(),
        Some("Fix the retry in src/payments/retry.py, attempt 0. Do not rename the module.")
    );
    // Every prompt was read, and its rule extracted from the stored text; the
    // version-0 record's invented rule was not taken.
    let texts: Vec<&str> = got.constraints.iter().map(|c| c.1.as_str()).collect();
    assert_eq!(texts, vec!["Do not rename the module."], "{got:?}");
    assert_eq!(
        got.intents.len(),
        4,
        "all four prompts became intents: {:?}",
        got.intents
    );
}

#[test]
fn old_and_new_events_mix_in_one_ledger() {
    let mut log = Log::new();
    // Written before prompt_facts existed.
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the retry. Never delete the audit log."),
        BASE_MS + 10,
        "old",
    );
    // Written by this build: the facts carry a rule past the stored text.
    let stored = velra_core::prompt::for_storage(
        &format!(
            "Keep going.\n\n{}\n\nDo not modify the tests.",
            "Background sentence without rules in it. ".repeat(200)
        ),
        velra_core::event::limits::PROMPT,
    );
    let new = Payload {
        prompt: Some(stored.text.clone()),
        prompt_facts: Some(velra_core::prompt::facts_record(&stored)),
        ..Default::default()
    };
    append_at(
        &mut log,
        "UserPromptSubmit",
        new.clone(),
        BASE_MS + 20,
        "new",
    );
    // Replayed: the same event again (same dedupe key), and the whole log
    // reduced a second time from scratch.
    append_at(&mut log, "UserPromptSubmit", new, BASE_MS + 20, "new");
    log.reduce();
    let first = state(&log.db, &log.session());
    let texts: Vec<(&str, i64)> = first
        .constraints
        .iter()
        .map(|c| (c.1.as_str(), c.3))
        .collect();
    assert_eq!(
        texts,
        vec![
            ("Never delete the audit log.", 0),
            ("Do not modify the tests.", 1)
        ]
    );
    reset_projection(&log.db);
    log.reduce();
    assert_eq!(state(&log.db, &log.session()), first);
}

// ---------------------------------------------- 6/7. replay, idempotence

/// A session with edits, a revert, failures, a pass and turn scans.
fn rich_session(log: &mut Log) {
    log.env
        .write_file("src/money.py", "rounding = ROUND_HALF_UP\n");
    log.env
        .write_file("tests/test_money.py", "def test_x(): pass\n");
    log.prompt("Fix the rounding bug in src/money.py. Do not modify the tests.");
    log.read("src/money.py");
    log.command_fail(
        "pytest tests/test_money.py",
        1,
        "FAILED tests/test_money.py::test_x - assert 1 == 2\ntests/test_money.py:3: AssertionError",
    );
    log.edit("src/money.py", "rounding = ROUND_HALF_EVEN\n");
    log.command_fail(
        "pytest tests/test_money.py",
        1,
        "FAILED tests/test_money.py::test_x - assert 1 == 3\ntests/test_money.py:3: AssertionError",
    );
    log.stop();
    log.prompt("That approach is wrong, undo it and try the quantize path instead.");
    log.git_restore(
        "git restore src/money.py",
        &[("src/money.py", "rounding = ROUND_HALF_UP\n")],
    );
    log.edit(
        "src/money.py",
        "rounding = ROUND_HALF_UP\nquantize = True\n",
    );
    log.command_ok("pytest tests/test_money.py", "1 passed");
    log.stop();
}

#[test]
fn reducing_the_same_log_again_gives_the_same_state() {
    let mut log = Log::new();
    rich_session(&mut log);
    log.reduce();
    let first = state(&log.db, &log.session());
    assert!(!first.dead_ends.is_empty(), "{first:?}");
    // Again, with nothing new: a no-op.
    log.reduce();
    assert_eq!(state(&log.db, &log.session()), first);
    // From scratch, over the same log.
    reset_projection(&log.db);
    log.reduce();
    assert_eq!(state(&log.db, &log.session()), first);
}

/// Every row of `db`, as the event it was.
fn events_of(db: &Db) -> Vec<(NewEvent, bool)> {
    let project: (String, String, bool) = db
        .conn
        .query_row(
            "SELECT project_id, root_path, is_git FROM projects",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("project");
    let mut stmt = db
        .conn
        .prepare(
            "SELECT dedupe_key, session_id, project_id, agent_id, hook_event, tool_name, tool_use_id, \
             ts_ms, payload FROM events ORDER BY id",
        )
        .expect("prepare");
    stmt.query_map([], |r| {
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
            project: Some(velra_core::event::ProjectInfo {
                project_id: project.0.clone(),
                root_path: project.1.clone(),
                is_git: project.2,
            }),
        })
    })
    .expect("query")
    .map(|e| (e.expect("row"), false))
    .collect()
}

/// Property: whatever subset of events is spooled, and wherever later each
/// spooled event is ingested -- with reductions in between, duplicates of it
/// spooled twice, and a second session interleaved -- the fold is the state
/// of the in-order log.
#[test]
fn spooled_events_in_any_ingestion_order_fold_to_the_in_order_state() {
    let mut base = Log::new();
    rich_session(&mut base);
    base.switch_session("other-session");
    base.prompt("Unrelated work in the other session on src/other.py.");
    base.env.write_file("src/other.py", "x = 1\n");
    base.edit("src/other.py", "x = 2\n");
    base.stop();
    base.reduce();
    let sessions = ["test-session", "other-session"];
    let want: Vec<State> = sessions.iter().map(|s| state(&base.db, s)).collect();
    let events = events_of(&base.db);

    let mut seed: u64 = 0x0dd5_eed0_1234_5678;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for round in 0..24 {
        // Choose which events were spooled, and how much later each was
        // ingested; direct events keep their order.
        let mut ingest: Vec<(usize, NewEvent)> = Vec::new();
        for (k, (ev, _)) in events.iter().enumerate() {
            let mut ev = ev.clone();
            let slot = if next() % 3 == 0 {
                let mut p = Payload::from_json(&ev.payload);
                p.spooled = Some(true);
                ev.payload = p.to_json();
                k * 10 + 5 + 10 * (1 + next() % (events.len() - k))
            } else {
                k * 10
            };
            ingest.push((slot, ev));
        }
        ingest.sort_by_key(|(slot, _)| *slot);
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = Db::open(&dir.path().join("v.db"), Role::Cli).expect("open");
        for (n, (_, ev)) in ingest.iter().enumerate() {
            eventlog::append(&mut db.conn, ev).expect("append");
            // Spooled twice, sometimes: the duplicate is the same event.
            if next() % 7 == 0 {
                eventlog::append(&mut db.conn, ev).expect("append duplicate");
            }
            if n % (2 + next() % 5) == 0 {
                reducer::reduce_all(&mut db.conn, None).expect("reduce");
            }
        }
        reducer::reduce_all(&mut db.conn, None).expect("reduce");
        for (s, want) in sessions.iter().zip(&want) {
            assert_eq!(&state(&db, s), want, "round {round}, session {s}");
        }
    }
}

/// A rebuild that does not fit a reduction's deadline is deferred, not cut
/// short by the watchdog (which would roll it back and retry it forever,
/// holding the shared cursor): the late event is folded where it arrived,
/// the session is marked, and the next reduction with the time rebuilds it.
#[test]
fn a_rebuild_that_does_not_fit_the_deadline_is_deferred_then_done() {
    let mut base = Log::new();
    let mut log = Log::new();
    for (n, l) in [&mut base, &mut log].into_iter().enumerate() {
        for i in 0..400i64 {
            if n == 0 && i == 201 {
                append_at(
                    l,
                    "UserPromptSubmit",
                    prompt("task: the task that was spooled"),
                    BASE_MS + 2_005,
                    "late",
                );
            }
            append_at(
                l,
                "UserPromptSubmit",
                prompt(&format!("Look at src/mod_{i}.py for the retry path.")),
                BASE_MS + 10 * i,
                &format!("p{i}"),
            );
        }
    }
    base.reduce();
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("task: the task that was spooled"),
        BASE_MS + 2_005,
        "late",
    );
    // 400 events at the measured rebuild cost do not fit in 5 ms.
    let opts = reducer::ReduceOptions {
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(5)),
        ..Default::default()
    };
    reducer::reduce(&mut log.db.conn, &opts).expect("reduce");
    let cursor = velra_core::db::cursor(&log.db.conn).unwrap();
    let max: i64 = log
        .db
        .conn
        .query_row("SELECT MAX(id) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cursor, max, "the cursor moved past the late event");
    assert_eq!(
        reducer::pending_rebuilds(&log.db.conn).unwrap(),
        vec![log.session()]
    );
    // Folded where it arrived, for now: not yet the logical state.
    assert_ne!(
        state(&log.db, &log.session()),
        state(&base.db, &base.session())
    );
    // A reduction with no deadline -- `velra restore`, `velra inspect` -- does it.
    log.reduce();
    assert!(reducer::pending_rebuilds(&log.db.conn).unwrap().is_empty());
    assert_eq!(
        state(&log.db, &log.session()),
        state(&base.db, &base.session())
    );
}

// ------------------------------------------------------- 8. clock anomalies

#[test]
fn direct_events_keep_ingestion_order_whatever_the_clock_says() {
    let mut log = Log::new();
    let first = "Fix the retry bug in src/payments/retry.py so the key survives.";
    // Equal timestamps, then a clock that runs backwards, then far ahead.
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt(first),
        BASE_MS + 100,
        "a",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Then the gateway in src/payments/gateway.py."),
        BASE_MS + 100,
        "b",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Then the ledger sync in src/ledger/sync.py."),
        BASE_MS + 50,
        "c",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Then the audit log writer."),
        BASE_MS + 10_000_000_000,
        "d",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Finally, the docs for all of it."),
        BASE_MS + 60,
        "e",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    assert_eq!(live(&got, "ROOT").as_deref(), Some(first));
    assert_eq!(
        live(&got, "LATEST").as_deref(),
        Some("Finally, the docs for all of it.")
    );
    let order: Vec<&str> = got.intents.iter().map(|(_, _, t, _)| t.as_str()).collect();
    assert_eq!(order.len(), 5);
    assert_eq!(order[4], "Finally, the docs for all of it.");
    // A spooled event older than everything is placed first; one from far in
    // the future of a spooled clock is placed last among what preceded its
    // ingestion, never before events it could not have preceded.
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("task: a genuinely earlier task that was spooled"),
        BASE_MS + 1,
        "z",
    );
    log.reduce();
    let got = state(&log.db, &log.session());
    assert_eq!(
        got.epoch, 2,
        "the spooled task starts an epoch before the others"
    );
    assert_eq!(
        got.intents
            .first()
            .map(|(_, l, t, _)| (l.as_str(), t.as_str())),
        Some(("ROOT", "a genuinely earlier task that was spooled"))
    );
}

#[test]
fn a_spool_file_with_a_malformed_timestamp_is_not_ingested() {
    let env = Env::new();
    drop(env.open_db());
    std::fs::create_dir_all(env.spool_dir()).unwrap();
    let bad = json!({
        "dedupe_key": "k", "session_id": env.session, "project_id": env.project_id(),
        "agent_id": null, "hook_event": "UserPromptSubmit", "tool_name": null,
        "tool_use_id": null, "ts_ms": "yesterday", "payload": "{\"prompt\":\"task: evil\"}"
    });
    std::fs::write(env.spool_dir().join("1-1-abcd.jsonl"), bad.to_string()).unwrap();
    let mut db = env.open_db();
    let n = velra_core::spool::ingest(&mut db.conn, &env.spool_dir(), 10).expect("ingest");
    assert_eq!(n, 0);
    let rows: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

// --------------------------------------------------- 12. session isolation

#[test]
fn a_late_event_of_one_session_does_not_touch_another() {
    let mut log = Log::new();
    let a = log.session();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the login test in tests/test_auth.py please."),
        BASE_MS + 10,
        "a1",
    );
    log.switch_session("session-b");
    // Identical prompt text and timestamp, different session.
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the login test in tests/test_auth.py please."),
        BASE_MS + 10,
        "a1",
    );
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Never delete the audit log."),
        BASE_MS + 30,
        "b2",
    );
    log.reduce();
    let b_rows = |db: &Db| -> Vec<(i64, String, String)> {
        db.conn
            .prepare(
                "SELECT id, level, text FROM intents WHERE session_id = 'session-b' ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let b_before = b_rows(&log.db);
    let b_state = state(&log.db, "session-b");
    // Session A's late prompt rebuilds A, and only A: B's rows are untouched,
    // row ids included.
    log.switch_session(&a);
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("task: an earlier task in session A"),
        BASE_MS + 5,
        "a0",
    );
    log.reduce();
    assert_eq!(b_rows(&log.db), b_before);
    assert_eq!(state(&log.db, "session-b"), b_state);
    let a_state = state(&log.db, &a);
    assert_eq!(a_state.epoch, 2);
    assert_eq!(
        live(&a_state, "ROOT").as_deref(),
        Some("an earlier task in session A")
    );
    // A's own prompt came after the late task, in the task's epoch.
    assert_eq!(
        live(&a_state, "LATEST").as_deref(),
        Some("Fix the login test in tests/test_auth.py please.")
    );
}

// ----------------------------------------- 13. constraints and rejections

/// An older rule arriving late is older: it takes its place ahead of newer
/// rules, including in the capped selection the snapshot makes.
#[test]
fn an_older_rule_arriving_late_keeps_its_precedence() {
    let mut log = Log::new();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the retry in src/payments/retry.py."),
        BASE_MS + 10,
        "p0",
    );
    for (n, rule) in [
        "Always add a migration for schema changes.",
        "Keep the CLI flags stable.",
        "Never rename the package.",
    ]
    .iter()
    .enumerate()
    {
        append_at(
            &mut log,
            "UserPromptSubmit",
            prompt(rule),
            BASE_MS + 30 + n as i64,
            &format!("r{n}"),
        );
    }
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Try a workaround that caches the key in a global. That workaround is a rejected approach."),
        BASE_MS + 40,
        "rj2",
    );
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt(
            "Do not modify the tests. Caching the key in the request object is a dead end here.",
        ),
        BASE_MS + 20,
        "r_late",
    );
    let snap = log.snapshot();
    let rules: Vec<&str> = snap.constraints.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        rules,
        vec![
            "Do not modify the tests.",
            "Always add a migration for schema changes.",
            "Keep the CLI flags stable.",
        ]
    );
    let rejections: Vec<&str> = snap.rejections.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        rejections,
        vec![
            "Caching the key in the request object is a dead end here.",
            "Try a workaround that caches the key in a global. That workaround is a rejected approach.",
        ]
    );
    let ordinals: Vec<i64> = snap.constraints.iter().map(|c| c.prompt_ordinal).collect();
    assert_eq!(ordinals, vec![1, 2, 3]);
}

// -------------------------------------------- 9/10. turn scan and the disk

fn edit_hooks(env: &Env, rel: &str, before: &str, after: &str) {
    let path = env.write_file(rel, before);
    let file = path.to_string_lossy().into_owned();
    let input = json!({"file_path": file, "old_string": before, "new_string": after});
    let mut p = env.base_payload("PreToolUse");
    p["tool_name"] = json!("Edit");
    p["tool_use_id"] = json!("toolu_edit");
    p["tool_input"] = input.clone();
    env.hook("pre-tool-use", &p).assert_contract();
    env.write_file(rel, after);
    let mut p = env.base_payload("PostToolUse");
    p["tool_name"] = json!("Edit");
    p["tool_use_id"] = json!("toolu_edit");
    p["tool_input"] = input;
    p["tool_response"] =
        json!({"filePath": file, "originalFile": before, "oldString": before, "newString": after});
    env.hook("post-tool-use", &p).assert_contract();
}

fn stop_hook(env: &Env) {
    let mut p = env.base_payload("Stop");
    p["stop_hook_active"] = json!(false);
    env.hook("stop", &p).assert_contract();
}

fn versions_of(env: &Env) -> Vec<(String, String)> {
    let db = env.open_db();
    let mut stmt = db
        .conn
        .prepare("SELECT source, content_hash FROM file_versions ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The file changes after the turn ended, and the reducer runs only then.
/// What enters the ledger is the file as the Stop hook saw it, never what the
/// reducer finds on disk later.
#[test]
fn a_file_changed_after_stop_is_not_filed_as_the_turn_end_state() {
    let env = Env::new();
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!("change the value in src/a.py to one");
    p["prompt_id"] = json!("p0");
    env.hook("user-prompt-submit", &p).assert_contract();
    edit_hooks(&env, "src/a.py", "value = 0\n", "value = 1\n");
    stop_hook(&env);
    let after_stop = velra_core::hash::hash_file(&env.project.join("src/a.py")).0;
    env.write_file("src/a.py", "value = 2  # changed after the turn ended\n");
    let later = velra_core::hash::hash_file(&env.project.join("src/a.py")).0;
    env.drain_and_load_events(4);
    env.drain();
    let versions = versions_of(&env);
    assert!(
        versions.iter().all(|(_, h)| *h != later),
        "the disk after Stop was filed as history: {versions:?}"
    );
    // The Stop event carries its own observation, taken at Stop.
    let stop = env
        .drain_and_load_events(4)
        .into_iter()
        .find(|e| e.hook_event == "Stop")
        .expect("stop event");
    let scan = &stop.json()["turn_scan"];
    assert_eq!(scan[0]["hash"], json!(after_stop), "{scan}");
    // Nothing was reverted or discarded on the strength of the later change.
    let db = env.open_db();
    let dead: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM dead_ends", [], |r| r.get(0))
        .unwrap();
    assert_eq!(dead, 0);
}

/// The feature the scan exists for still works: a file put back outside the
/// agent during the turn is seen at Stop and recorded as an external revert.
#[test]
fn an_external_revert_during_the_turn_is_still_seen_at_stop() {
    let env = Env::new();
    let mut p = env.base_payload("UserPromptSubmit");
    p["prompt"] = json!("change the value in src/a.py to one");
    p["prompt_id"] = json!("p0");
    env.hook("user-prompt-submit", &p).assert_contract();
    edit_hooks(&env, "src/a.py", "value = 0\n", "value = 1\n");
    env.write_file("src/a.py", "value = 0\n");
    stop_hook(&env);
    env.write_file("src/a.py", "value = 7\n");
    env.drain_and_load_events(4);
    env.drain();
    let db = env.open_db();
    let mechanisms: Vec<String> = db
        .conn
        .prepare("SELECT mechanism FROM dead_ends")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(mechanisms, vec!["external"]);
}

/// A Stop event without an observation (the hook could not reach the
/// database; an event from an earlier build) records no turn-end state.
#[test]
fn a_stop_without_an_observation_records_no_turn_end_state() {
    let mut log = Log::new();
    log.env.write_file("src/a.py", "v0\n");
    log.prompt("change src/a.py please, carefully");
    log.edit("src/a.py", "v1\n");
    log.env.write_file("src/a.py", "v9\n");
    log.append(
        "Stop",
        None,
        Payload {
            stop_hook_active: Some(false),
            ..Default::default()
        },
    );
    log.reduce();
    let sources: Vec<String> = log
        .versions("src/a.py")
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert!(!sources.iter().any(|s| s == "turn_scan"), "{sources:?}");
}

// ------------------------------------------------------------- 11. trace

#[test]
fn trace_reports_occurrence_lateness_and_unreduced_events_truthfully() {
    let mut log = Log::new();
    append_at(
        &mut log,
        "UserPromptSubmit",
        prompt("Fix the retry in src/payments/retry.py."),
        BASE_MS + 30,
        "p1",
    );
    log.reduce();
    append_at(
        &mut log,
        "UserPromptSubmit",
        spooled_prompt("Look at src/payments/zebra_marker.py first."),
        BASE_MS + 20,
        "p0",
    );
    // Not reduced yet: the ledger is behind, and the trace must say that
    // rather than call the marker lost by extraction.
    let meta = velra_core::snapshot::SnapshotMeta {
        checkpoint_id: "ckpt_trace".into(),
        created_ms: BASE_MS + 60_000,
        trigger: velra_core::model::Trigger::Manual,
        partial: false,
        preview: true,
        tz_offset_secs: 0,
    };
    let cfg = velra_core::render::RenderConfig::default();
    let inputs = velra_core::provenance::TraceInputs {
        meta: &meta,
        cfg: &cfg,
        staged: None,
    };
    let t =
        velra_core::provenance::trace_marker(&log.db.conn, &log.session(), &inputs, "zebra_marker")
            .expect("trace");
    assert_eq!(t.first_loss, Some("ledger"), "{}", t.report());
    let reason = t.reason.clone().unwrap_or_default();
    assert!(reason.starts_with("not yet reduced"), "{}", t.report());
    let normalized = &t
        .layers
        .iter()
        .find(|l| l.layer == "normalized_state")
        .unwrap()
        .evidence[0];
    assert!(
        normalized.contains("spooled: ingested late"),
        "{normalized}"
    );
    assert!(
        normalized.contains("occurred 2026-09-12T10:04:05"),
        "{normalized}"
    );
    // Reduced, it is where it belongs and nothing is lost.
    log.reduce();
    let t =
        velra_core::provenance::trace_marker(&log.db.conn, &log.session(), &inputs, "zebra_marker")
            .expect("trace");
    assert_ne!(t.first_loss, Some("ledger"), "{}", t.report());
    let snapshot = &t
        .layers
        .iter()
        .find(|l| l.layer == "snapshot")
        .unwrap()
        .evidence;
    assert!(
        snapshot.iter().any(|e| e.starts_with("built ")),
        "{snapshot:?}"
    );
}
