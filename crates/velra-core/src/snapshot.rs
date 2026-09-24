//! Builds a render [`Snapshot`] from the materialized tables, applying the
//! deterministic content-selection rules of §16.2.

use crate::constraint::ConstraintKind;
use crate::git;
use crate::model::{CommandKind, IntentLevel, Mechanism, Outcome, Trigger};
use crate::paths;
use crate::reducer::{project_root, session_epoch};
use crate::render::{
    AttemptView, CommandRef, ConstraintView, DeadEndView, FailureView, IntentView, NextTarget,
    Snapshot, TestStatusView, WorkingFileView,
};
use crate::shell;
use crate::testids;
use crate::text::names_identifier;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

/// Identity and framing for a snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub checkpoint_id: String,
    pub created_ms: i64,
    pub trigger: Trigger,
    pub partial: bool,
    pub preview: bool,
    pub tz_offset_secs: i32,
}

const DEAD_ENDS_MAX: usize = 4;
/// Constraint sentences carried into a snapshot before the ladder trims them.
const CONSTRAINTS_MAX: usize = 3;
/// Rejection statements carried into a snapshot, oldest first.
const REJECTIONS_MAX: usize = 4;
const ATTEMPTS_MAX: usize = 4;
const WORKING_MAX: usize = 8;
/// Tests carried into a snapshot before the ladder trims them.
pub const TESTS_MAX: usize = 6;
/// Superseded messages searched, newest first, for an earlier one that names
/// code (see [`earlier_message`]).
const EARLIER_SCAN: i64 = 12;

fn session_project(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<String>> {
    let from_sessions = conn
        .prepare_cached("SELECT project_id FROM sessions WHERE session_id = ?1")?
        .query_row([session_id], |r| r.get::<_, String>(0))
        .optional()?;
    if from_sessions.is_some() {
        return Ok(from_sessions);
    }
    conn.prepare_cached(
        "SELECT project_id FROM events WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
    )?
    .query_row([session_id], |r| r.get(0))
    .optional()
}

fn command_ref(conn: &Connection, id: i64) -> rusqlite::Result<Option<CommandRef>> {
    conn.prepare_cached("SELECT command_text, outcome FROM commands WHERE id = ?1")?
        .query_row([id], |r| {
            let outcome: String = r.get(1)?;
            let command: String = r.get(0)?;
            Ok(CommandRef {
                id,
                command: shell::display_command(&command).to_string(),
                outcome: Outcome::parse(&outcome).unwrap_or(Outcome::Unknown),
            })
        })
        .optional()
}

/// One `file_stats` row plus whether the capsule already names the file.
///
/// Public because the ranking derived from it is a documented, tested property
/// of the ledger rather than a private detail of one query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileStat {
    pub path: String,
    pub edits: u32,
    pub reads: u32,
    pub in_failure: bool,
    pub first_touch_ms: i64,
    /// Named by a dead end, a live attempt, or the failing output.
    pub pinned: bool,
    /// Stored as an absolute path, i.e. outside the workspace root: an agent's
    /// own notes, a global config file, a sibling checkout.
    pub outside: bool,
}

/// Whether a stored path lies outside the workspace. The reducer stores
/// in-workspace paths relative to the root, so anything absolute or climbing
/// out with `..` is outside it.
pub fn is_outside_workspace(path: &str) -> bool {
    paths::is_absolute_str(path) || path.starts_with("../") || path.starts_with("..\\")
}

/// Rank of a file in `[FILE_ACTIVITY]`, highest first.
///
/// ```text
/// score = 6*pinned + 4*in_failure + 3*min(edits,3) + 2*[reads>=2] + min(reads,3)
/// ```
///
/// The weights separate scarce evidence from abundant evidence. Editing a file
/// or seeing it named in a failing test's output happens to a handful of files
/// per task; reading one happens to dozens, and a single read is the weakest
/// thing the log records — it is what a breadth-first sweep leaves behind.
/// Coming *back* to a file is worth more than the second read itself, because
/// that is the difference between skimming and working. Every term saturates,
/// so no single signal can be farmed: forty reads of one file rank it exactly
/// as three reads do.
///
/// The range is 0..=24 (6 + 4 + 9 + 2 + 3). Ties are broken, in order, by edits, then reads, then
/// *earliest* first touch, then path — see [`rank`].
pub fn working_score(s: &FileStat) -> u32 {
    6 * u32::from(s.pinned)
        + 4 * u32::from(s.in_failure)
        + 3 * s.edits.min(3)
        + 2 * u32::from(s.reads >= 2)
        + s.reads.min(3)
}

/// Sorts `stats` into `[FILE_ACTIVITY]` order, highest rank first.
///
/// Files inside the workspace always rank above files outside it. In the
/// v0.1.2 Token-Burn qualification the agent wrote Claude Code memory notes
/// under `~/.claude/projects/…/memory/` in six of eight source sessions, and
/// those notes took two of the eight working-file slots and both surviving
/// `[RECENT_EDITS]` slots from the source files the task was about. They are
/// still listed, below every workspace file.
///
/// Recency decides nothing but the very last tie, and deliberately in the
/// *older* direction. Ranking ties by "most recently touched" hands the whole
/// list to whatever the agent did last: in the v0.1 benchmark an audit sweep
/// across 84 unrelated modules, each read exactly once, filled the section and
/// pushed out the two files the failing test ran through. Among files with
/// equally thin evidence the ones the session opened with are the ones that
/// framed it, so first touch wins (D59).
///
/// A pure function of the rows, so the ordering is reproducible from the ledger
/// alone and can be tested without a database.
pub fn rank(stats: &mut [FileStat]) {
    stats.sort_by(|a, b| {
        a.outside
            .cmp(&b.outside)
            .then(working_score(b).cmp(&working_score(a)))
            .then(b.edits.cmp(&a.edits))
            .then(b.reads.cmp(&a.reads))
            .then(a.first_touch_ms.cmp(&b.first_touch_ms))
            .then(a.path.cmp(&b.path))
    });
}

/// Splits a stored `- old\n+ new` excerpt into its two lines.
pub fn split_excerpt(excerpt: &str) -> (Option<String>, Option<String>) {
    let mut minus = None;
    let mut plus = None;
    for line in excerpt.lines() {
        if let Some(m) = line.strip_prefix("- ") {
            minus.get_or_insert_with(|| m.to_string());
        } else if let Some(p) = line.strip_prefix("+ ") {
            plus.get_or_insert_with(|| p.to_string());
        }
    }
    (minus, plus)
}

/// Per-test status across the epoch's test runs, highest focus first, at most
/// [`TESTS_MAX`].
///
/// Runs are replayed in order. A failing run sets every test its output names
/// to failing; a passing run clears every tracked test it covers
/// ([`testids::run_covers`]). The result is the one fact the capsule used to
/// lose: which *tests* were failing, which of them the session got passing, and
/// their exact identifiers — independent of which command happened to run last.
///
/// Ranking is by focus: each run that observed a test adds 2 when the run
/// named it (its file, its node id, a filter it matches) and 1 when it only
/// covered it through a directory or the whole suite ([`testids::run_names`]). A test the session kept running on purpose therefore
/// outranks a pre-existing failure that only shows up in full-suite runs.
/// Ties go to the test still failing, then the most recent failure, then the
/// first one seen.
pub fn test_statuses(
    conn: &Connection,
    session_id: &str,
    epoch: i64,
) -> rusqlite::Result<Vec<TestStatusView>> {
    struct Acc {
        view: TestStatusView,
        runner: String,
        focus: u32,
        first_seen: usize,
    }
    let rows: Vec<(i64, String, String, Option<String>, i64)> = conn
        .prepare_cached(
            "SELECT id, command_text, outcome, excerpt, ts_ms FROM commands \
             WHERE session_id = ?1 AND epoch = ?2 AND kind = 'test' ORDER BY ts_ms, id",
        )?
        .query_map(params![session_id, epoch], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut acc: Vec<Acc> = Vec::new();
    for (id, command, outcome, excerpt, ts_ms) in rows {
        let Some(run) = testids::test_run(&command) else {
            continue;
        };
        let reference = CommandRef {
            id,
            command: shell::display_command(&command).to_string(),
            outcome: Outcome::parse(&outcome).unwrap_or(Outcome::Unknown),
        };
        match reference.outcome {
            Outcome::Fail => {
                let lines: Vec<&str> = excerpt.as_deref().unwrap_or("").lines().collect();
                for t in testids::failing_tests(&lines) {
                    let slot = match acc.iter().position(|a| a.view.id == t.id) {
                        Some(i) => i,
                        None => {
                            acc.push(Acc {
                                view: TestStatusView {
                                    id: t.id.clone(),
                                    failing: true,
                                    detail: None,
                                    last_fail: reference.clone(),
                                    last_fail_ms: ts_ms,
                                    passed: None,
                                    passed_ms: None,
                                },
                                runner: run.runner.clone(),
                                focus: 0,
                                first_seen: acc.len(),
                            });
                            acc.len() - 1
                        }
                    };
                    let a = &mut acc[slot];
                    a.view.failing = true;
                    if t.detail.is_some() {
                        a.view.detail = t.detail;
                    }
                    a.view.last_fail = reference.clone();
                    a.view.last_fail_ms = ts_ms;
                    a.view.passed = None;
                    a.view.passed_ms = None;
                    a.runner = run.runner.clone();
                    a.focus += if testids::run_names(&run, &run.runner, &t.id) {
                        2
                    } else {
                        1
                    };
                }
            }
            Outcome::Pass => {
                for a in acc.iter_mut() {
                    if testids::run_covers(&run, &a.runner, &a.view.id) {
                        a.view.failing = false;
                        a.view.passed = Some(reference.clone());
                        a.view.passed_ms = Some(ts_ms);
                        a.focus += if testids::run_names(&run, &a.runner, &a.view.id) {
                            2
                        } else {
                            1
                        };
                    }
                }
            }
            Outcome::Interrupted | Outcome::Unknown => {}
        }
    }
    acc.sort_by(|a, b| {
        b.focus
            .cmp(&a.focus)
            .then(b.view.failing.cmp(&a.view.failing))
            .then(b.view.last_fail_ms.cmp(&a.view.last_fail_ms))
            .then(a.first_seen.cmp(&b.first_seen))
    });
    Ok(acc.into_iter().take(TESTS_MAX).map(|a| a.view).collect())
}

/// The most recent earlier user message that names code, when the latest one
/// does not.
///
/// The intent model keeps one `LATEST` message and supersedes it with every
/// new prompt of three characters or more. That is right for "what was I just
/// asked", and wrong when the last thing said is a sign-off: in the v0.1.2
/// Token-Burn qualification "Right. The next thing to look at is retry_backoff
/// …" was superseded one prompt later by "I'm going to leave this session
/// here", and the only statement of the next action never reached the
/// snapshot. The superseded row was still in the ledger.
///
/// So when the latest message names no identifier ([`names_identifier`]), the
/// newest superseded `LATEST`/`SUBTASK` message that does is carried alongside
/// it. Nothing is carried when the latest message already names code, which
/// keeps the section from turning into a transcript.
pub fn earlier_message(
    conn: &Connection,
    session_id: &str,
    epoch: i64,
    latest: Option<&IntentView>,
    exclude: &[&str],
) -> rusqlite::Result<Option<IntentView>> {
    let Some(latest) = latest.filter(|l| !names_identifier(&l.text)) else {
        return Ok(None);
    };
    let rows: Vec<IntentView> = conn
        .prepare_cached(
            "SELECT id, text, created_ms FROM intents \
             WHERE session_id = ?1 AND epoch = ?2 AND level IN ('LATEST', 'SUBTASK') \
             AND superseded_ms IS NOT NULL AND id < ?3 ORDER BY id DESC LIMIT ?4",
        )?
        .query_map(params![session_id, epoch, latest.id, EARLIER_SCAN], |r| {
            Ok(IntentView {
                id: r.get(0)?,
                text: r.get(1)?,
                ts_ms: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .map(|v| IntentView {
            text: user_text(&v.text),
            ..v
        })
        .find(|v| {
            names_identifier(&v.text)
                && v.text != latest.text
                && !exclude.contains(&v.text.as_str())
        }))
}

/// Which dead ends a snapshot carries, given their paths most recent first:
/// indices into `paths`, in the same order, at most `max`.
///
/// Every distinct file gets a slot before any file gets a second one. The
/// capsule's promise about dead ends is that a rejected route is not taken
/// again, and a route is identified by the file it changed; a fourth retry of
/// one file tells the next session less than a single revert of another file
/// it has never heard of. Until v0.1.2 this was simply "the `max` most recent",
/// so a file reverted four times in a row hid every earlier dead end while
/// `dead_end_total` still counted it. Within that, recency decides.
pub fn pick_dead_ends(paths: &[&str], max: usize) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        if chosen.len() < max && !chosen.iter().any(|&c| paths[c] == *p) {
            chosen.push(i);
        }
    }
    for i in 0..paths.len() {
        if chosen.len() >= max {
            break;
        }
        if !chosen.contains(&i) {
            chosen.push(i);
        }
    }
    chosen.sort_unstable();
    chosen
}

/// One `intents` row as the snapshot reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRow {
    pub id: i64,
    pub level: Option<IntentLevel>,
    /// As stored. Rows written before v0.1.2 may still carry injected context.
    pub text: String,
    pub ts_ms: i64,
    /// Not superseded.
    pub live: bool,
}

/// The messages a snapshot carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Intents {
    pub root: Option<IntentView>,
    pub subtask: Option<IntentView>,
    pub latest: Option<IntentView>,
}

/// The user's own words of a stored intent: injected context removed
/// (`crate::prompt`) and whitespace collapsed as the reducer does.
///
/// The reducer has classified only authored text since v0.1.2, so for rows it
/// wrote this is the identity. It matters for ledgers reduced earlier, where a
/// VS Code session's ROOT begins with the editor's `<ide_opened_file>` block and
/// a background task's `<task-notification>` can be the ROOT or the LATEST
/// message outright. The ledger is not rewritten; the snapshot reads through
/// it, so an existing session restores correctly without a migration.
pub fn user_text(stored: &str) -> String {
    crate::intent::normalize_prompt(crate::prompt::authored(stored))
}

/// Selects the objective, the subtask and the latest message from an epoch's
/// intent rows (all of them, superseded included, in id order).
///
/// For rows the current reducer wrote, this is exactly "the live row of each
/// level". The fallbacks exist for rows whose stored text turns out to be
/// nothing but injected context:
///
/// * **objective** -- the first live ROOT that has text of its own; failing
///   that, the earliest message of the epoch long enough to have become ROOT
///   under the intent rules (`intent::ROOT_MIN_CHARS`). That is the message the
///   reducer would have chosen had the injected prompt never arrived.
/// * **latest** -- the most recent LATEST row with text of its own, superseded
///   or not: a notification that superseded the user's message did not replace
///   what the user last said.
///
/// Pure, so the rule is tested without a database.
pub fn select_intents(rows: &[IntentRow]) -> Intents {
    let view = |r: &IntentRow| IntentView {
        id: r.id,
        text: user_text(&r.text),
        ts_ms: r.ts_ms,
    };
    let of = |level: IntentLevel| {
        rows.iter()
            .filter(move |r| r.level == Some(level))
            .map(move |r| (r, view(r)))
    };
    let root = of(IntentLevel::Root)
        .find(|(r, v)| r.live && !v.text.is_empty())
        .map(|(_, v)| v)
        .or_else(|| {
            rows.iter()
                .filter(|r| matches!(r.level, Some(IntentLevel::Root | IntentLevel::Latest)))
                .map(view)
                .find(|v| v.text.chars().count() >= crate::intent::ROOT_MIN_CHARS)
        });
    let subtask = of(IntentLevel::Subtask)
        .filter(|(r, v)| r.live && !v.text.is_empty())
        .map(|(_, v)| v)
        .next_back();
    let latest = of(IntentLevel::Latest)
        .filter(|(_, v)| v.text.chars().count() >= crate::intent::LATEST_MIN_CHARS)
        .map(|(_, v)| v)
        .next_back();
    Intents {
        root,
        subtask,
        latest,
    }
}

/// Builds the snapshot for `session_id`'s current epoch.
// Row tuples are wide by nature here; naming each one would not make the
// selection rules clearer.
#[allow(clippy::type_complexity)]
pub fn build(
    conn: &Connection,
    session_id: &str,
    meta: &SnapshotMeta,
) -> rusqlite::Result<Snapshot> {
    let epoch = session_epoch(conn, session_id)?;
    let project_id = session_project(conn, session_id)?.unwrap_or_default();
    let root_path: Option<PathBuf> = project_root(conn, &project_id)?;
    let sid = session_id;

    // Intents.
    let rows: Vec<IntentRow> = conn
        .prepare_cached(
            "SELECT id, level, text, created_ms, superseded_ms IS NULL FROM intents \
             WHERE session_id = ?1 AND epoch = ?2 ORDER BY id",
        )?
        .query_map(params![sid, epoch], |r| {
            let level: String = r.get(1)?;
            Ok(IntentRow {
                id: r.get(0)?,
                level: IntentLevel::parse(&level),
                text: r.get(2)?,
                ts_ms: r.get(3)?,
                live: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let Intents {
        root,
        subtask,
        latest,
    } = select_intents(&rows);

    let shown: Vec<&str> = root
        .iter()
        .chain(subtask.iter())
        .map(|v: &IntentView| v.text.as_str())
        .collect();
    let earlier = earlier_message(conn, sid, epoch, latest.as_ref(), &shown)?;

    // Constraints (schema v3). Oldest first: the order the user stated them in
    // is the only ordering the log supports, and a later sentence never
    // silently outranks an earlier one.
    //
    // Rejections (`constraint::is_rejection_cue`) are kept apart: they are the
    // user ruling a route out, not a rule for the work, and they do not take
    // one of the rules' slots.
    let all_constraints: Vec<ConstraintView> = conn
        .prepare_cached(
            "SELECT id, text, kind, cue, prompt_ordinal, created_ms FROM constraints \
             WHERE session_id = ?1 AND epoch = ?2 AND superseded_ms IS NULL \
             ORDER BY id",
        )?
        .query_map(params![sid, epoch], |r| {
            let kind: String = r.get(2)?;
            Ok(ConstraintView {
                id: r.get(0)?,
                text: r.get(1)?,
                kind: ConstraintKind::parse(&kind).unwrap_or(ConstraintKind::Requirement),
                cue: r.get(3)?,
                prompt_ordinal: r.get(4)?,
                ts_ms: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let (mut rejections, mut constraints): (Vec<ConstraintView>, Vec<ConstraintView>) =
        all_constraints
            .into_iter()
            .partition(|c| crate::constraint::is_rejection_cue(&c.cue));
    constraints.truncate(CONSTRAINTS_MAX);
    rejections.truncate(REJECTIONS_MAX);

    let git = root_path.as_deref().and_then(git::head_info);
    let edit_count: i64 = conn
        .prepare_cached("SELECT COUNT(*) FROM edits WHERE session_id = ?1 AND epoch = ?2")?
        .query_row(params![sid, epoch], |r| r.get(0))?;
    let last_test = conn
        .prepare_cached(
            "SELECT id FROM commands WHERE session_id = ?1 AND epoch = ?2 AND kind = 'test' \
             ORDER BY ts_ms DESC, id DESC LIMIT 1",
        )?
        .query_row(params![sid, epoch], |r| r.get::<_, i64>(0))
        .optional()?;
    let last_test = match last_test {
        Some(id) => command_ref(conn, id)?,
        None => None,
    };

    // Active failure: most recent FAIL not followed by a PASS of the same signature.
    const OPEN_FAILURE: &str = "FROM commands c WHERE c.session_id = ?1 AND c.epoch = ?2 \
         AND c.kind IN ('test', 'build', 'lint') AND c.outcome = 'FAIL' \
         AND NOT EXISTS (SELECT 1 FROM commands d WHERE d.session_id = c.session_id AND d.epoch = c.epoch \
           AND d.signature = c.signature AND d.outcome = 'PASS' \
           AND (d.ts_ms > c.ts_ms OR (d.ts_ms = c.ts_ms AND d.id > c.id)))";
    let failure_row = conn
        .prepare_cached(&format!(
            "SELECT c.id, c.kind, c.command_text, c.exit_code, c.excerpt, c.ts_ms, c.mentioned_paths {OPEN_FAILURE} \
             ORDER BY c.ts_ms DESC, c.id DESC LIMIT 1"
        ))?
        .query_row(params![sid, epoch], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .optional()?;
    let failing_count: i64 = conn
        .prepare_cached(&format!(
            "SELECT COUNT(DISTINCT c.signature) {OPEN_FAILURE}"
        ))?
        .query_row(params![sid, epoch], |r| r.get(0))?;
    let mut failure_mentions: Vec<serde_json::Value> = Vec::new();
    let failure = failure_row.map(|(id, kind, command, exit_code, excerpt, ts_ms, mentions)| {
        failure_mentions = mentions
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default();
        FailureView {
            id,
            kind: CommandKind::parse(&kind).unwrap_or(CommandKind::Test),
            // The capsule quotes this under a character cap and cuts from the
            // right; a leading `cd "<absolute path>" &&` would spend the whole
            // allowance before naming the runner.
            command: shell::display_command(&command).to_string(),
            exit_code,
            excerpt: excerpt
                .map(|e| e.lines().map(str::to_string).collect())
                .unwrap_or_default(),
            ts_ms,
        }
    });

    let tests = test_statuses(conn, sid, epoch)?;

    // Dead ends.
    let dead_rows: Vec<(i64, String, String, String, Option<String>, i64, Option<i64>)> = conn
        .prepare_cached(
            "SELECT id, path, edit_ids, mechanism, command_text, resolved_ms, observed_after_command_id \
             FROM dead_ends WHERE session_id = ?1 AND epoch = ?2 AND reapplied = 0 \
             AND resolved_ms <= ?3 ORDER BY resolved_ms DESC, id DESC",
        )?
        .query_map(params![sid, epoch, meta.created_ms], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let dead_end_total = dead_rows.len() as u32;
    let chosen = pick_dead_ends(
        &dead_rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(),
        DEAD_ENDS_MAX,
    );
    let dead_rows: Vec<_> = dead_rows
        .into_iter()
        .enumerate()
        .filter(|(i, _)| chosen.contains(i))
        .map(|(_, row)| row)
        .collect();
    let mut dead_ends = Vec::new();
    for (id, path, edit_ids, mechanism, command, resolved_ms, observed) in dead_rows {
        let edit_ids: Vec<i64> = serde_json::from_str(&edit_ids).unwrap_or_default();
        let mut subagent = false;
        let mut first_excerpt: Option<String> = None;
        let mut stmt = conn.prepare_cached("SELECT agent_id, excerpt FROM edits WHERE id = ?1")?;
        for (i, eid) in edit_ids.iter().enumerate() {
            if let Some((agent, excerpt)) = stmt
                .query_row([eid], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                })
                .optional()?
            {
                subagent |= agent.is_some();
                if i == 0 {
                    first_excerpt = excerpt;
                }
            }
        }
        let (minus, plus) = if paths::is_sensitive(&path) {
            (None, None)
        } else {
            first_excerpt
                .as_deref()
                .map(split_excerpt)
                .unwrap_or((None, None))
        };
        let observed_after = match observed {
            Some(cid) => command_ref(conn, cid)?,
            None => None,
        };
        dead_ends.push(DeadEndView {
            id,
            path,
            subagent,
            edit_ids,
            mechanism: Mechanism::parse(&mechanism).unwrap_or(Mechanism::External),
            command: command.map(|c| shell::display_command(&c).to_string()),
            resolved_ms,
            minus,
            plus,
            observed_after,
        });
    }

    // Recent attempts: latest ACTIVE edit per path, excluding dead-end paths.
    let attempt_rows: Vec<(i64, String, Option<u32>, Option<u32>, i64, Option<String>)> = conn
        .prepare_cached(
            "SELECT e.id, e.path, e.lines_added, e.lines_removed, e.ts_ms, e.agent_id FROM edits e \
             WHERE e.session_id = ?1 AND e.epoch = ?2 AND e.status = 'ACTIVE' AND e.id = \
               (SELECT MAX(x.id) FROM edits x WHERE x.session_id = e.session_id AND x.epoch = e.epoch \
                AND x.path = e.path AND x.status = 'ACTIVE') \
             ORDER BY e.ts_ms DESC, e.id DESC",
        )?
        .query_map(params![sid, epoch], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // Workspace files first, most recent first within each group; see `rank`
    // for why edits outside the workspace go last.
    let mut attempt_rows: Vec<_> = attempt_rows
        .into_iter()
        .filter(|row| !dead_ends.iter().any(|d| d.path == row.1))
        .collect();
    attempt_rows.sort_by_key(|row| is_outside_workspace(&row.1));
    let mut attempts = Vec::new();
    for (edit_id, path, added, removed, ts_ms, agent) in attempt_rows {
        if attempts.len() >= ATTEMPTS_MAX {
            break;
        }
        let after_id = conn
            .prepare_cached(
                "SELECT id FROM commands WHERE session_id = ?1 AND epoch = ?2 AND kind IN ('test', 'build') \
                 AND ts_ms > ?3 ORDER BY ts_ms, id LIMIT 1",
            )?
            .query_row(params![sid, epoch, ts_ms], |r| r.get::<_, i64>(0))
            .optional()?;
        let afterward = match after_id {
            Some(id) => command_ref(conn, id)?,
            None => None,
        };
        attempts.push(AttemptView {
            edit_id,
            path,
            subagent: agent.is_some(),
            added,
            removed,
            ts_ms,
            afterward,
        });
    }

    // Working files (§16.2). Evidence comes first: a file the capsule already
    // talks about (a dead end, a live attempt, a path named in the failing
    // output) is pinned, and the rest are ranked by `working_score`.
    let pinned: std::collections::HashSet<&str> = dead_ends
        .iter()
        .map(|d| d.path.as_str())
        .chain(attempts.iter().map(|a| a.path.as_str()))
        .chain(failure_mentions.iter().filter_map(|m| m["path"].as_str()))
        .collect();
    let mut stats: Vec<FileStat> = conn
        .prepare_cached(
            "SELECT path, edits, reads, in_failure, last_touch_ms, first_touch_ms \
             FROM file_stats WHERE session_id = ?1 AND epoch = ?2",
        )?
        .query_map(params![sid, epoch], |r| {
            Ok(FileStat {
                path: r.get(0)?,
                edits: r.get::<_, i64>(1)? as u32,
                reads: r.get::<_, i64>(2)? as u32,
                in_failure: r.get::<_, i64>(3)? != 0,
                first_touch_ms: r.get(5)?,
                pinned: false,
                outside: false,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    if let Some(root) = &root_path {
        stats.retain(|s| paths::resolve(&s.path, root).exists());
    }
    for s in &mut stats {
        s.pinned = pinned.contains(s.path.as_str());
        s.outside = is_outside_workspace(&s.path);
    }
    rank(&mut stats);
    let working_files: Vec<WorkingFileView> = stats
        .into_iter()
        .take(WORKING_MAX)
        .map(|s| WorkingFileView {
            path: s.path,
            edits: s.edits,
            reads: s.reads,
            in_failure: s.in_failure,
        })
        .collect();

    // Next known target.
    let mut next_target = None;
    if let Some(f) = &failure {
        for m in &failure_mentions {
            let (Some(path), Some(line)) = (m["path"].as_str(), m["line"].as_u64()) else {
                continue;
            };
            let raw = m["raw"].as_str().unwrap_or(path);
            if f.excerpt.iter().any(|l| l.contains(raw)) {
                next_target = Some(NextTarget {
                    rule: "failure-location",
                    target: format!("{path}:{line}"),
                    source: format!("commands:{}", f.id),
                });
                break;
            }
        }
    }
    if next_target.is_none() {
        next_target = conn
            .prepare_cached(
                "SELECT id, path FROM edits WHERE session_id = ?1 AND epoch = ?2 AND status = 'ACTIVE' \
                 ORDER BY ts_ms DESC, id DESC LIMIT 1",
            )?
            .query_row(params![sid, epoch], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .optional()?
            .map(|(id, path)| NextTarget { rule: "last-active-edit", target: path, source: format!("edits:{id}") });
    }

    Ok(Snapshot {
        checkpoint_id: meta.checkpoint_id.clone(),
        created_ms: meta.created_ms,
        trigger: meta.trigger,
        partial: meta.partial,
        preview: meta.preview,
        session_id: sid.to_string(),
        project_id,
        epoch,
        root,
        constraints,
        rejections,
        subtask,
        latest,
        earlier,
        git,
        edit_count: edit_count as u32,
        last_test,
        failure,
        failing_count: failing_count as u32,
        tests,
        dead_ends,
        dead_end_total,
        attempts,
        working_files,
        next_target,
        tz_offset_secs: meta.tz_offset_secs,
    })
}

/// Whether the session's current epoch has anything worth checkpointing
/// (§15.2 step 4): a ROOT, a stated constraint, a command, or an edit.
pub fn has_state(conn: &Connection, session_id: &str) -> rusqlite::Result<bool> {
    let epoch = session_epoch(conn, session_id)?;
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM intents WHERE session_id = ?1 AND epoch = ?2 AND level = 'ROOT' AND superseded_ms IS NULL) \
             OR EXISTS (SELECT 1 FROM constraints WHERE session_id = ?1 AND epoch = ?2 AND superseded_ms IS NULL) \
             OR EXISTS (SELECT 1 FROM commands WHERE session_id = ?1 AND epoch = ?2) \
             OR EXISTS (SELECT 1 FROM edits WHERE session_id = ?1 AND epoch = ?2)",
    )?
    .query_row(params![session_id, epoch], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_gets_a_dead_end_slot_before_any_file_gets_a_second() {
        let paths = ["a", "a", "a", "a", "b", "c"];
        assert_eq!(pick_dead_ends(&paths, 4), [0, 1, 4, 5]);
        assert_eq!(pick_dead_ends(&["a", "b"], 4), [0, 1]);
        assert_eq!(pick_dead_ends(&["a", "a", "a"], 2), [0, 1]);
        let many = ["a", "b", "c", "d", "e"];
        assert_eq!(pick_dead_ends(&many, 4), [0, 1, 2, 3]);
        assert!(pick_dead_ends(&[], 4).is_empty());
    }

    #[test]
    fn intent_selection_is_the_live_row_for_rows_the_reducer_writes_now() {
        let row = |id: i64, level: IntentLevel, text: &str, live: bool| IntentRow {
            id,
            level: Some(level),
            text: text.into(),
            ts_ms: id,
            live,
        };
        let rows = [
            row(
                1,
                IntentLevel::Root,
                "fix the rounding bug in the ledger",
                true,
            ),
            row(2, IntentLevel::Latest, "first look at src/ledger.rs", false),
            row(3, IntentLevel::Subtask, "write the regression test", true),
            row(4, IntentLevel::Latest, "then run   the suite", true),
        ];
        let got = select_intents(&rows);
        assert_eq!(got.root.map(|r| r.id), Some(1));
        assert_eq!(got.subtask.map(|r| r.id), Some(3));
        let latest = got.latest.expect("latest");
        assert_eq!((latest.id, latest.text.as_str()), (4, "then run the suite"));
        assert_eq!(select_intents(&[]), Intents::default());
    }

    #[test]
    fn user_text_strips_injected_context_and_is_idempotent() {
        let raw =
            "<ide_opened_file>The user opened the file x in the IDE.</ide_opened_file> fix  it";
        assert_eq!(user_text(raw), "fix it");
        assert_eq!(user_text(&user_text(raw)), "fix it");
    }
}
