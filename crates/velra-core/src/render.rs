//! The Continuation Capsule (§16), `render_version = 1`.
//!
//! Rendering is a pure function of a [`Snapshot`] and a [`RenderConfig`]:
//! identical input yields byte-identical output on every platform.

use crate::constraint::ConstraintKind;
use crate::git::GitInfo;
use crate::model::{CommandKind, Mechanism, Outcome, Trigger};
use crate::text::{estimate_tokens, truncate_chars, truncate_chars_front};
use crate::time::{hh_mm, rfc3339_utc};

pub const RENDER_VERSION: i64 = 1;

/// Target size in *estimated* tokens, and — since v0.1.2 — a hard stop rather
/// than an aspiration.
///
/// # Why this is not simply 800
///
/// The spec's budget is 800 *real* tokens on Anthropic's tokenizer. The
/// renderer can only measure [`estimate_tokens`], so the target has to leave
/// room for whatever the estimator gets wrong. Two things were wrong before
/// v0.1.2 and both are fixed here; the number moved because of them, not
/// instead of them.
///
/// **1. The ladder had no hard stop.** `run_steps` returned as soon as its
/// rungs were exhausted, whether or not the target had been met, so the only
/// bound actually enforced was [`HARD_CEILING_TOKENS`] — 1,000, not 730. A
/// capsule anywhere in that band was shipped without complaint. `render_impl`
/// now runs both ladders against the caller's target and then applies
/// [`enforce_ceiling`] at that target, which removes lines and finally
/// characters until the text fits. The returned text is therefore at or below
/// `budget_tokens` by construction, and
/// `render_never_exceeds_its_target_for_any_input` asserts it over generated
/// snapshots.
///
/// **2. The estimator under-read the current format.** Measured against the
/// eight capsules Claude Code actually received during the v0.1.1 efficacy
/// benchmark — `bench/results/v0.1.1/trials/*/delivered_capsule.txt` paired
/// with the tokenizer counts in `token_measurement.json` — the old walk came in
/// below the real cost every single time, by as much as 7.9% of its own
/// reading. That is why E4 failed with a capsule of 804 real tokens against an
/// 800 ceiling. The walk now charges prose at three letters per token instead
/// of four (English, as Claude's tokenizer sees it, is nearer three), and it
/// reads at or above the real count on all ten measured capsules, old format
/// and new. `estimator_is_above_real_tokenizer_counts` holds it there.
///
/// # What is and is not guaranteed
///
/// Guaranteed, as a property of the code: a rendered capsule never estimates
/// above `budget_tokens`, and never exceeds [`ABSOLUTE_MAX_CHARS`].
///
/// Not guaranteed, because it cannot be without shipping the tokenizer: that
/// the real count is at or below 800. What can be said is that the estimator
/// has never under-read a measured capsule, and that 740 leaves 60 tokens —
/// 8.1% — of slack on top of that. The benchmark measures the real count on
/// every delivered capsule rather than trusting this paragraph.
pub const DEFAULT_BUDGET_TOKENS: u32 = 740;

/// The spec's budget, kept as the figure the margin is measured against.
const SPEC_BUDGET_TOKENS: u32 = 800;

/// Slack between the render target and the spec budget, as a percentage of the
/// spec budget. It is head-room against the estimator being wrong in the
/// direction that costs money, on a format it has not seen yet.
const REQUIRED_SLACK_PCT: u32 = 7;

/// Raising the default towards the spec figure has to be deliberate: this fails
/// the build if the slack drops below what the estimator has ever needed.
const _: () = assert!(
    SPEC_BUDGET_TOKENS - DEFAULT_BUDGET_TOKENS >= SPEC_BUDGET_TOKENS * REQUIRED_SLACK_PCT / 100,
    "DEFAULT_BUDGET_TOKENS leaves less slack than the estimator's measured error allows"
);

/// Floor for `budget_tokens`.
///
/// The hard stop can always shrink a capsule, but it cannot shrink it below a
/// well formed block: an opening tag, a preamble and a closing tag cost what
/// they cost. Below this figure there is nothing meaningful to return, so the
/// target is clamped up to it and the guarantee "rendered tokens <= target"
/// holds for every budget at or above it.
pub const MIN_BUDGET_TOKENS: u32 = 64;

pub const HARD_CEILING_TOKENS: u32 = 1_000;
pub const ABSOLUTE_MAX_CHARS: usize = 9_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderConfig {
    /// Target size in estimated tokens (§16.3).
    pub budget_tokens: u32,
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            budget_tokens: DEFAULT_BUDGET_TOKENS,
        }
    }
}

/// One constraint sentence, as the user wrote it.
///
/// `kind` and `cue` are the evidence that selected the sentence, not a claim
/// about how important it is; see `crate::constraint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintView {
    pub id: i64,
    pub text: String,
    pub kind: ConstraintKind,
    pub cue: String,
    /// 0-based index of the user prompt it came from.
    pub prompt_ordinal: i64,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentView {
    pub id: i64,
    pub text: String,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureView {
    pub id: i64,
    pub kind: CommandKind,
    pub command: String,
    pub exit_code: Option<i64>,
    pub excerpt: Vec<String>,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRef {
    pub id: i64,
    pub command: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadEndView {
    pub id: i64,
    pub path: String,
    pub subagent: bool,
    pub edit_ids: Vec<i64>,
    pub mechanism: Mechanism,
    pub command: Option<String>,
    pub resolved_ms: i64,
    /// Excerpt lines (without the `- ` / `+ ` markers); `None` for sensitive paths.
    pub minus: Option<String>,
    pub plus: Option<String>,
    pub observed_after: Option<CommandRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub edit_id: i64,
    pub path: String,
    pub subagent: bool,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    pub ts_ms: i64,
    pub afterward: Option<CommandRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingFileView {
    pub path: String,
    pub edits: u32,
    pub reads: u32,
    pub in_failure: bool,
}

/// One test's status across the session's runs (see `crate::testids`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestStatusView {
    /// Exact identifier as the runner printed it, e.g. `tests/x.py::test_y`.
    pub id: String,
    /// The latest run that covered it reported it failing.
    pub failing: bool,
    /// The runner's one-line reason from the most recent failure.
    pub detail: Option<String>,
    /// The most recent run that reported it failing.
    pub last_fail: CommandRef,
    pub last_fail_ms: i64,
    /// When no longer failing: the passing run that covered it.
    pub passed: Option<CommandRef>,
    pub passed_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextTarget {
    /// `failure-location` or `last-active-edit`.
    pub rule: &'static str,
    pub target: String,
    /// Traceability, e.g. `commands:12`.
    pub source: String,
}

/// Everything the capsule needs, already selected per §16.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub checkpoint_id: String,
    pub created_ms: i64,
    pub trigger: Trigger,
    pub partial: bool,
    /// Preview renders (inspect without a checkpoint) omit `--checkpoint`.
    pub preview: bool,
    pub session_id: String,
    pub project_id: String,
    pub epoch: i64,
    pub root: Option<IntentView>,
    /// Constraint sentences of the epoch, oldest first.
    pub constraints: Vec<ConstraintView>,
    pub subtask: Option<IntentView>,
    pub latest: Option<IntentView>,
    /// The most recent superseded message that names code, carried only when
    /// `latest` names none (`snapshot::earlier_message`).
    pub earlier: Option<IntentView>,
    pub git: Option<GitInfo>,
    pub edit_count: u32,
    pub last_test: Option<CommandRef>,
    pub failure: Option<FailureView>,
    /// Distinct test/build/lint signatures whose latest run failed.
    pub failing_count: u32,
    /// Per-test status, highest focus first (`snapshot::test_statuses`).
    pub tests: Vec<TestStatusView>,
    pub dead_ends: Vec<DeadEndView>,
    /// All non-reapplied dead ends in the epoch (`dead_ends` holds at most 4).
    pub dead_end_total: u32,
    pub attempts: Vec<AttemptView>,
    pub working_files: Vec<WorkingFileView>,
    pub next_target: Option<NextTarget>,
    pub tz_offset_secs: i32,
}

/// Truncation state (§16.3). Steps apply in [`STEPS`] order.
#[derive(Debug, Clone, Copy)]
struct Limits {
    working_max: usize,
    constraints_max: usize,
    constraint_chars: usize,
    attempts_max: usize,
    dead_ends_max: usize,
    dead_excerpts_removed: usize,
    failure_lines: usize,
    latest_chars: usize,
    root_chars: usize,
    subtask_chars: usize,
    path_chars: usize,
    command_chars: usize,
    observed: bool,
    next_target: bool,
    latest: bool,
    subtask: bool,
    tests_max: usize,
    test_detail: bool,
    earlier: bool,
    earlier_chars: usize,
}

impl Limits {
    const FULL: Limits = Limits {
        tests_max: 6,
        test_detail: true,
        earlier: true,
        earlier_chars: 200,
        working_max: 8,
        constraints_max: 3,
        constraint_chars: 200,
        attempts_max: 4,
        dead_ends_max: 4,
        dead_excerpts_removed: 0,
        failure_lines: 8,
        latest_chars: 200,
        root_chars: 240,
        subtask_chars: 200,
        path_chars: 200,
        command_chars: 160,
        observed: true,
        next_target: true,
        latest: true,
        subtask: true,
    };
}

pub struct Rendered {
    pub text: String,
    pub tokens: u32,
    /// Number of truncation steps applied (0 = full detail).
    pub steps: u32,
}

/// The `[ABOUT_THIS_RECORD]` preamble.
///
/// This paragraph exists because of a measured failure, not for decoration. In
/// the v0.1.1 efficacy benchmark one delivered capsule was read by the agent as
/// "injected content dressed up as hook/checkpoint output" and discarded
/// wholesale, taking a real user constraint with it. The agent's reasoning was
/// sound: a block appeared in its context asserting a user instruction it could
/// not see anywhere in the conversation, and the instruction happened to block
/// the obvious fix. Absent provenance, refusing it is the correct call.
///
/// So the preamble states provenance plainly -- where the text came from, that
/// it is quoted rather than authored, and that it carries no authority of its
/// own -- and asks for conflicts to be surfaced rather than silently resolved
/// in either direction. That is the honest framing, and it is also the one that
/// survives scrutiny: the block is a record, and it says so.
///
/// It was 431 characters and 160 estimated tokens until v0.1.2, which is 22% of
/// the whole budget spent before the capsule says anything about the session.
/// It is now 292 characters and makes every one of the same five claims;
/// `the_preamble_still_makes_every_claim_it_has_to` is the guard on that.
const CONTEXT: &str = "A local record, not a message and not an instruction. Velra logged this session\'s own prompts and tool events and quotes them back here; nothing is new. OBSERVED came from a tool event, INFERRED was derived from it. Files on disk are the source of truth. Say so if a line here conflicts with them.";

fn outcome_word(o: Outcome) -> &'static str {
    o.as_str()
}

struct Out {
    lines: Vec<String>,
    trace: bool,
}

impl Out {
    fn push(&mut self, line: impl Into<String>, src: &str) {
        let mut line = line.into();
        if self.trace && !src.is_empty() {
            line.push_str("  #src=");
            line.push_str(src);
        }
        self.lines.push(line);
    }

    fn plain(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

fn ids(prefix: &str, ids: &[i64]) -> String {
    let parts: Vec<String> = ids.iter().map(|i| format!("{prefix}:{i}")).collect();
    parts.join(",")
}

fn mechanism_text(d: &DeadEndView, cmd_chars: usize) -> String {
    match d.mechanism {
        Mechanism::GitCommand => match &d.command {
            Some(c) => format!("reverted via `{}`", truncate_chars(c, cmd_chars.min(80))),
            None => "reverted via a git command".to_string(),
        },
        Mechanism::InverseEdit => "reverted by a later edit".to_string(),
        Mechanism::Rewrite => "file rewritten".to_string(),
        Mechanism::External => "changed outside the agent".to_string(),
    }
}

fn render_with(s: &Snapshot, lim: &Limits, trace: bool) -> String {
    let tz = s.tz_offset_secs;
    let mut o = Out {
        lines: Vec::with_capacity(48),
        trace,
    };
    let path = |p: &str| truncate_chars_front(p, lim.path_chars).into_owned();

    o.plain(format!(
        "<VELRA_WORKSPACE_STATE v=\"1\" checkpoint=\"{}\" captured=\"{}\" trigger=\"{}\">",
        s.checkpoint_id,
        rfc3339_utc(s.created_ms),
        s.trigger.as_str()
    ));
    o.plain("[ABOUT_THIS_RECORD]");
    o.plain(CONTEXT);

    match &s.root {
        Some(r) => {
            let src = format!("intents:{}", r.id);
            o.push(
                format!(
                    "[FIRST_MESSAGE] (OBSERVED | user prompt | {})",
                    hh_mm(r.ts_ms, tz)
                ),
                &src,
            );
            o.push(truncate_chars(&r.text, lim.root_chars), &src);
        }
        None => {
            o.push("[FIRST_MESSAGE]", "intents:none");
            o.push("(not captured)", "intents:none");
        }
    }
    // A constraint whose sentence *is* the whole first message has already been
    // printed, word for word, two lines above. Reprinting it buys nothing and
    // costs the capsule budget twice. Anything narrower than the whole message
    // is kept: that is the case the section exists for, where a rule sits in
    // one sentence of a longer prompt and the ladder is about to trim the rest
    // of that prompt away.
    let root_text = s.root.as_ref().map(|r| r.text.as_str());
    let constraints: Vec<&ConstraintView> = s
        .constraints
        .iter()
        .filter(|c| root_text != Some(c.text.as_str()))
        .take(lim.constraints_max)
        .collect();
    if !constraints.is_empty() {
        let all: Vec<i64> = constraints.iter().map(|c| c.id).collect();
        o.push(
            "[STATED_CONSTRAINTS] (OBSERVED | user prompt | quoted verbatim)",
            &ids("constraints", &all),
        );
        for c in constraints {
            o.push(
                format!(
                    "- turn {} {} | \"{}\"",
                    c.prompt_ordinal,
                    hh_mm(c.ts_ms, tz),
                    truncate_chars(&c.text, lim.constraint_chars)
                ),
                &format!("constraints:{}", c.id),
            );
        }
    }

    if let Some(st) = s.subtask.as_ref().filter(|_| lim.subtask) {
        let src = format!("intents:{}", st.id);
        o.push(
            format!(
                "[SUBTASK_MESSAGE] (OBSERVED | subtask: prompt | {})",
                hh_mm(st.ts_ms, tz)
            ),
            &src,
        );
        o.push(truncate_chars(&st.text, lim.subtask_chars), &src);
    }
    // Chronological: the earlier message is printed above the latest one.
    if let Some(e) = s.earlier.as_ref().filter(|_| lim.earlier) {
        let src = format!("intents:{}", e.id);
        o.push(
            format!(
                "[EARLIER_MESSAGE] (OBSERVED | user prompt | {})",
                hh_mm(e.ts_ms, tz)
            ),
            &src,
        );
        o.push(truncate_chars(&e.text, lim.earlier_chars), &src);
    }
    if let Some(l) = s
        .latest
        .as_ref()
        .filter(|l| lim.latest && s.root.as_ref().is_none_or(|r| r.text != l.text))
    {
        let src = format!("intents:{}", l.id);
        o.push(
            format!(
                "[LATEST_MESSAGE] (OBSERVED | user prompt | {})",
                hh_mm(l.ts_ms, tz)
            ),
            &src,
        );
        o.push(truncate_chars(&l.text, lim.latest_chars), &src);
    }

    let status_src = format!(
        "sessions:{}/epoch:{}{}",
        s.session_id,
        s.epoch,
        s.last_test
            .as_ref()
            .map(|t| format!(",commands:{}", t.id))
            .unwrap_or_default()
    );
    o.push("[WORKSPACE_STATE]", &status_src);
    let git = match &s.git {
        Some(g) => {
            let branch = truncate_chars(g.branch.as_deref().unwrap_or("detached"), 60).into_owned();
            match g.short_sha() {
                Some(sha) => format!("{branch} @ {sha}"),
                None => branch,
            }
        }
        None => "no git".to_string(),
    };
    let last_test = s
        .last_test
        .as_ref()
        .map_or("none", |t| outcome_word(t.outcome));
    o.push(
        format!(
            "{git} | {} edits this task | last test run: {last_test}{}",
            s.edit_count,
            if s.partial { " (partial capture)" } else { "" }
        ),
        &status_src,
    );

    // Exact test identifiers, each with its latest covered status. This is the
    // section the ladder protects longest among the failure detail: an
    // identifier is what lets the next session run the one test that matters,
    // and it costs a line where the excerpt that used to carry it costs eight.
    let tests: Vec<&TestStatusView> = s.tests.iter().take(lim.tests_max).collect();
    if !tests.is_empty() {
        let mut cmd_ids: Vec<i64> = Vec::new();
        for t in &tests {
            for id in std::iter::once(t.last_fail.id).chain(t.passed.as_ref().map(|p| p.id)) {
                if !cmd_ids.contains(&id) {
                    cmd_ids.push(id);
                }
            }
        }
        o.push(
            "[TEST_STATUS] (OBSERVED | latest run covering each)",
            &ids("commands", &cmd_ids),
        );
        for t in &tests {
            let line = if t.failing {
                let detail = match (&t.detail, lim.test_detail) {
                    (Some(d), true) => format!(" | {}", truncate_chars(d, 60)),
                    _ => String::new(),
                };
                format!("- FAIL {}{detail}", path(&t.id))
            } else {
                let how = match (&t.passed, lim.test_detail) {
                    (Some(p), true) => {
                        format!(
                            " in `{}`",
                            truncate_chars(&p.command, lim.command_chars.min(60))
                        )
                    }
                    _ => String::new(),
                };
                format!(
                    "- PASS {} | failed {}, then passed {}{how}",
                    path(&t.id),
                    hh_mm(t.last_fail_ms, tz),
                    t.passed_ms
                        .map_or_else(|| "?".to_string(), |ms| hh_mm(ms, tz)),
                )
            };
            let src = match &t.passed {
                Some(p) if !t.failing => format!("commands:{},commands:{}", t.last_fail.id, p.id),
                _ => format!("commands:{}", t.last_fail.id),
            };
            o.push(line, &src);
        }
    }

    if let Some(f) = &s.failure {
        let src = format!("commands:{}", f.id);
        o.push(
            format!(
                "[TEST_RESULT] (OBSERVED | {} run | {})",
                f.kind.as_str(),
                hh_mm(f.ts_ms, tz)
            ),
            &src,
        );
        o.push(
            format!("Command: {}", truncate_chars(&f.command, lim.command_chars)),
            &src,
        );
        o.push(
            match f.exit_code {
                Some(c) => format!("Result: FAIL (exit {c})"),
                None => "Result: FAIL".to_string(),
            },
            &src,
        );
        // A line that only names a test already listed under [TEST_STATUS]
        // says nothing new; the budget goes to the assertion context instead.
        let excerpt: Vec<&String> = f
            .excerpt
            .iter()
            .filter(|line| {
                let line = line.replace('\\', "/");
                !tests.iter().any(|t| line.contains(t.id.as_str()))
            })
            .collect();
        let n = excerpt.len();
        for line in &excerpt[n.saturating_sub(lim.failure_lines)..] {
            o.push(format!("  {line}"), &src);
        }
    }

    // One line per path. Several reverted attempts on the same file used to
    // cost a full line each -- path, count, mechanism and time -- so a file
    // retried three times spent three headers saying the same path. Grouping
    // keeps every attempt's excerpt and observation; only the header is shared.
    let mut groups: Vec<Vec<(usize, &DeadEndView)>> = Vec::new();
    for (i, d) in s.dead_ends.iter().enumerate() {
        match groups.iter_mut().find(|g| g[0].1.path == d.path) {
            Some(g) => g.push((i, d)),
            None => groups.push(vec![(i, d)]),
        }
    }
    groups.truncate(lim.dead_ends_max);
    if !groups.is_empty() {
        let all: Vec<i64> = groups.iter().flatten().map(|(_, d)| d.id).collect();
        o.push("[REVERTED_EDITS] (OBSERVED)", &ids("dead_ends", &all));
        let n = s.dead_ends.len();
        for group in &groups {
            let (_, last) = group[0];
            let member_ids: Vec<i64> = group.iter().map(|(_, d)| d.id).collect();
            let edit_ids: Vec<i64> = group
                .iter()
                .flat_map(|(_, d)| d.edit_ids.iter().copied())
                .collect();
            let src = format!(
                "{},{}",
                ids("dead_ends", &member_ids),
                ids("edits", &edit_ids)
            );
            let subagent = if group.iter().any(|(_, d)| d.subagent) {
                " (subagent)"
            } else {
                ""
            };
            let header = if group.len() == 1 {
                format!(
                    "- {}{subagent} | {} edit(s) | {} at {}",
                    path(&last.path),
                    edit_ids.len(),
                    mechanism_text(last, lim.command_chars),
                    hh_mm(last.resolved_ms, tz)
                )
            } else {
                format!(
                    "- {}{subagent} | {} reverts, {} edit(s) | last {} at {}",
                    path(&last.path),
                    group.len(),
                    edit_ids.len(),
                    mechanism_text(last, lim.command_chars),
                    hh_mm(last.resolved_ms, tz)
                )
            };
            o.push(header, &src);
            for (i, d) in group {
                let member_src = format!("dead_ends:{},{}", d.id, ids("edits", &d.edit_ids));
                // Dead ends are listed most recent first; excerpts are removed
                // oldest first, counted across every dead end, not per group.
                let excerpt_removed = n - i <= lim.dead_excerpts_removed;
                if !excerpt_removed {
                    if let Some(m) = &d.minus {
                        o.push(format!("    - {m}"), &member_src);
                    }
                    if let Some(p) = &d.plus {
                        o.push(format!("    + {p}"), &member_src);
                    }
                }
                if let (true, Some(c)) = (lim.observed, &d.observed_after) {
                    o.push(
                        format!(
                            "  Observed afterward: `{}` {}. Causal link: UNCONFIRMED.",
                            truncate_chars(&c.command, lim.command_chars.min(80)),
                            outcome_word(c.outcome)
                        ),
                        &format!("dead_ends:{},commands:{}", d.id, c.id),
                    );
                }
            }
        }
    }

    let attempts: Vec<&AttemptView> = s.attempts.iter().take(lim.attempts_max).collect();
    if !attempts.is_empty() {
        let all: Vec<i64> = attempts.iter().map(|a| a.edit_id).collect();
        o.push("[RECENT_EDITS] (OBSERVED)", &ids("edits", &all));
        for a in attempts {
            let num = |v: Option<u32>| v.map_or_else(|| "?".to_string(), |n| n.to_string());
            let after = match &a.afterward {
                Some(c) => format!(
                    "`{}` {}",
                    truncate_chars(&c.command, lim.command_chars.min(80)),
                    outcome_word(c.outcome)
                ),
                None => "no test run yet".to_string(),
            };
            let src = match &a.afterward {
                Some(c) => format!("edits:{},commands:{}", a.edit_id, c.id),
                None => format!("edits:{}", a.edit_id),
            };
            o.push(
                format!(
                    "- {}{} | +{}/-{} lines | {} | afterward: {after}",
                    path(&a.path),
                    if a.subagent { " (subagent)" } else { "" },
                    num(a.added),
                    num(a.removed),
                    hh_mm(a.ts_ms, tz)
                ),
                &src,
            );
        }
    }

    let working: Vec<&WorkingFileView> = s.working_files.iter().take(lim.working_max).collect();
    if !working.is_empty() {
        o.push(
            "[FILE_ACTIVITY] (OBSERVED)",
            &format!("file_stats:{}/epoch:{}", s.session_id, s.epoch),
        );
        for w in working {
            o.push(
                format!(
                    "- {} | edited {}x, read {}x{}",
                    path(&w.path),
                    w.edits,
                    w.reads,
                    if w.in_failure {
                        ", in failure output"
                    } else {
                        ""
                    }
                ),
                &format!("file_stats:{}", w.path),
            );
        }
    }

    if let (true, Some(t)) = (lim.next_target, &s.next_target) {
        o.push(
            format!("[FAILURE_LOCATION] (INFERRED | {})", t.rule),
            &t.source,
        );
        o.push(path(&t.target), &t.source);
    }

    o.plain("[RECORD_DETAIL]");
    // Neither the checkpoint id nor the list of section names is repeated here.
    // The id is already in the opening tag, and spelling it twice cost about 30
    // tokens of dense ULID; the four section names cost another 40 to say what
    // `velra inspect --section` prints when given a name it does not know.
    o.plain("Full detail for any section: `velra inspect --section <name>`");
    o.plain("</VELRA_WORKSPACE_STATE>");
    o.lines.join("\n")
}

/// One rung of the truncation ladder: a name for diagnostics, and a move that
/// returns false when it can no longer change anything (so the loop moves on).
struct Rung {
    name: &'static str,
    apply: fn(&mut Limits, &Snapshot) -> bool,
}

/// Spec truncation order (§16.3), revised in v0.1.2 for identifier retention.
///
/// The ladder removes the least information per token first. The v0.1.2
/// Token-Burn qualification showed the old order doing the opposite: all four
/// restored capsules kept "Observed afterward" lines' worth of budget and the
/// inferred `[FAILURE_LOCATION]` right up to the ceiling ladder, while
/// `[FILE_ACTIVITY]` went to zero and the failure excerpt -- the only place the
/// failing test's name lived -- went from eight lines to none. Two of the
/// moves that follow were ceiling-only before and now run here: prose that an
/// agent can regenerate (the observed-afterward lines, the inferred location,
/// excerpt context whose identifiers `[TEST_STATUS]` already carries) goes
/// before any exact identifier does. See DECISIONS.md, D64.
const SPEC_STEPS: &[Rung] = &[
    Rung {
        name: "working_files 8->4",
        apply: |l, _| std::mem::replace(&mut l.working_max, 4) != 4,
    },
    Rung {
        name: "recent_edits 4->2",
        apply: |l, _| std::mem::replace(&mut l.attempts_max, 2) != 2,
    },
    Rung {
        name: "reverted_edits excerpt, oldest first",
        apply: |l, s| {
            if l.dead_excerpts_removed < s.dead_ends.len() {
                l.dead_excerpts_removed += 1;
                true
            } else {
                false
            }
        },
    },
    Rung {
        name: "test_result excerpt 8->3",
        apply: |l, _| std::mem::replace(&mut l.failure_lines, 3) != 3,
    },
    Rung {
        name: "latest_message 200->120 chars",
        apply: |l, _| std::mem::replace(&mut l.latest_chars, 120) != 120,
    },
    Rung {
        name: "first_message 240->160 chars",
        apply: |l, _| std::mem::replace(&mut l.root_chars, 160) != 160,
    },
    Rung {
        name: "observed_afterward off",
        apply: |l, _| std::mem::replace(&mut l.observed, false),
    },
    Rung {
        name: "test_result excerpt 3->0",
        apply: |l, _| std::mem::replace(&mut l.failure_lines, 0) != 0,
    },
    Rung {
        name: "failure_location off",
        apply: |l, _| std::mem::replace(&mut l.next_target, false),
    },
    // The rung that used to be missing. Before v0.1.2 the ladder stepped
    // `working_max` from 4 straight to 0, so `[FILE_ACTIVITY]` went from four
    // files to none in one move -- and section 18 of the v0.1 report records the
    // result: the section absent from 4 of 4 delivered capsules, every one of
    // them well under the ceiling. Two files is most of what the section is
    // worth and costs two lines.
    Rung {
        name: "working_files 4->2",
        apply: |l, _| std::mem::replace(&mut l.working_max, 2) != 2,
    },
    Rung {
        name: "constraints 200->140 chars",
        apply: |l, _| std::mem::replace(&mut l.constraint_chars, 140) != 140,
    },
    Rung {
        name: "test_status detail off",
        apply: |l, _| std::mem::replace(&mut l.test_detail, false),
    },
    Rung {
        name: "recent_edits 2->0",
        apply: |l, _| std::mem::replace(&mut l.attempts_max, 0) != 0,
    },
    Rung {
        name: "earlier_message 200->120 chars",
        apply: |l, _| std::mem::replace(&mut l.earlier_chars, 120) != 120,
    },
    Rung {
        name: "test_status 6->3",
        apply: |l, _| std::mem::replace(&mut l.tests_max, 3) != 3,
    },
];

/// Additional steps applied only above the hard ceiling (see DECISIONS.md).
///
/// §16.3 protects `CONTEXT`, the `ROOT` line, `STATUS`, the `ACTIVE_FAILURE`
/// header, the `DEAD_ENDS` header and `RECOVERY` from removal. Everything else
/// may go, and the last few steps here go further than the spec's ladder
/// because the ladder alone cannot always reach the ceiling: a path or a
/// command made almost entirely of separators costs close to one token per
/// character, so four dead-end lines at 200 characters each are 800 tokens on
/// their own. Narrowing the section to its most recent entry keeps the header
/// and one example, which is what the section is protected for.
///
/// The last working files, the last test identifier and the earlier message go
/// before the latest message and the constraints: those two are the user's own
/// words about what to do next and what not to do.
const CEILING_STEPS: &[Rung] = &[
    Rung {
        name: "subtask 200->80 chars",
        apply: |l, _| std::mem::replace(&mut l.subtask_chars, 80) != 80,
    },
    Rung {
        name: "paths 200->60 chars",
        apply: |l, _| std::mem::replace(&mut l.path_chars, 60) != 60,
    },
    Rung {
        name: "commands 160->80 chars",
        apply: |l, _| std::mem::replace(&mut l.command_chars, 80) != 80,
    },
    Rung {
        name: "reverted_edits 4->2 files",
        apply: |l, _| std::mem::replace(&mut l.dead_ends_max, 2) != 2,
    },
    Rung {
        name: "working_files 2->0",
        apply: |l, _| std::mem::replace(&mut l.working_max, 0) != 0,
    },
    Rung {
        name: "test_status 3->1",
        apply: |l, _| std::mem::replace(&mut l.tests_max, 1) != 1,
    },
    Rung {
        name: "first_message 160->100 chars",
        apply: |l, _| std::mem::replace(&mut l.root_chars, 100) != 100,
    },
    // Shorten a message before dropping it: a whole message is 50-90 tokens,
    // and the ladder is rarely more than a few dozen over by this point.
    Rung {
        name: "latest_message 120->80 chars",
        apply: |l, _| std::mem::replace(&mut l.latest_chars, 80) != 80,
    },
    Rung {
        name: "earlier_message 120->100 chars",
        apply: |l, _| std::mem::replace(&mut l.earlier_chars, 100) != 100,
    },
    // An earlier message exists only when the latest one names no code, so of
    // the two it is the latest that carries less a later session can act on.
    Rung {
        name: "latest_message off (an earlier message names code)",
        apply: |l, s| s.earlier.is_some() && std::mem::replace(&mut l.latest, false),
    },
    Rung {
        name: "constraints 3->2",
        apply: |l, _| std::mem::replace(&mut l.constraints_max, 2) != 2,
    },
    Rung {
        name: "constraints 140->100 chars",
        apply: |l, _| std::mem::replace(&mut l.constraint_chars, 100) != 100,
    },
    Rung {
        name: "earlier_message off",
        apply: |l, _| std::mem::replace(&mut l.earlier, false),
    },
    Rung {
        name: "latest_message off",
        apply: |l, _| std::mem::replace(&mut l.latest, false),
    },
    Rung {
        name: "constraints 2->1",
        apply: |l, _| std::mem::replace(&mut l.constraints_max, 1) != 1,
    },
    Rung {
        name: "subtask off",
        apply: |l, _| std::mem::replace(&mut l.subtask, false),
    },
    Rung {
        name: "reverted_edits 2->1 files",
        apply: |l, _| std::mem::replace(&mut l.dead_ends_max, 1) != 1,
    },
    Rung {
        name: "paths 60->40 chars",
        apply: |l, _| std::mem::replace(&mut l.path_chars, 40) != 40,
    },
    Rung {
        name: "commands 80->40 chars",
        apply: |l, _| std::mem::replace(&mut l.command_chars, 40) != 40,
    },
    Rung {
        name: "first_message 100->60 chars",
        apply: |l, _| std::mem::replace(&mut l.root_chars, 60) != 60,
    },
];

/// Last-resort guarantee that the block never exceeds `ceiling` (E2).
///
/// Every ladder step above is a judgement about which content matters least.
/// This is not a judgement -- it is the backstop that turns "at or below the
/// target, always" from an expectation into a property. It works in two moves,
/// and only the second one is unconditional:
///
///  1. Drop whole lines from the end of the body, keeping the opening tag
///     through `[WORKSPACE_STATE]` and the `[RECORD_DETAIL]` tail, so whatever
///     survives is still a well formed capsule that closes its own tag.
///  2. If even the protected head is too large -- a pathological checkpoint id,
///     a 300-character constraint, an estimator that reads the head alone above
///     the target -- shrink the head by binary search on characters until it
///     fits, then close the tag.
///
/// Step 2 is what makes the guarantee real. It used to truncate to a fixed
/// 2,000 characters and hope, which meant a small `budget_tokens` could still
/// return something above it.
fn enforce_ceiling(text: String, ceiling: u32, max_chars: usize) -> String {
    let fits = |t: &str| estimate_tokens(t) <= ceiling && t.chars().count() <= max_chars;
    if fits(&text) {
        return text;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    // Everything from [RECORD_DETAIL] on is the tail that must survive, closing tag
    // included. If the marker is somehow absent, keep the last line, which is
    // that tag: a capsule that does not close itself is worse than a short one.
    let recovery = lines
        .iter()
        .position(|l| l.starts_with("[RECORD_DETAIL]"))
        .unwrap_or_else(|| lines.len().saturating_sub(1));
    // The protected head: the opening tag, [ABOUT_THIS_RECORD] and its paragraph, the
    // objective, and [WORKSPACE_STATE] with its line.
    let head = lines
        .iter()
        .position(|l| l.starts_with("[WORKSPACE_STATE]"))
        .map_or(6, |i| i + 2)
        .min(recovery);
    let joined = |end: usize| {
        let mut out: Vec<&str> = lines[..end].to_vec();
        out.extend_from_slice(&lines[recovery..]);
        out.join("\n")
    };
    for end in (head..recovery).rev() {
        let candidate = joined(end);
        if fits(&candidate) {
            return candidate;
        }
    }
    let candidate = joined(head);
    if fits(&candidate) {
        return candidate;
    }
    hard_trim(&candidate, ceiling, max_chars)
}

/// The unconditional shrink. Halves the character budget until the result fits,
/// then walks back up, so the returned text is the longest prefix-with-tag that
/// satisfies both bounds.
///
/// Terminates because `fits` is true at length zero: an empty body plus the
/// closing tag estimates a handful of tokens, and `ceiling` is clamped to at
/// least that by `render_impl`.
fn hard_trim(text: &str, ceiling: u32, max_chars: usize) -> String {
    const CLOSE: &str = "</VELRA_WORKSPACE_STATE>";
    let build = |chars: usize| {
        let mut out = truncate_chars(text, chars).trim_end().to_string();
        if !out.ends_with(CLOSE) {
            out.push('\n');
            out.push_str(CLOSE);
        }
        out
    };
    let fits = |t: &str| estimate_tokens(t) <= ceiling && t.chars().count() <= max_chars;

    let mut lo = 0usize;
    let mut hi = text.chars().count().min(max_chars);
    // `build(0)` is the closing tag alone, which always fits; if even that does
    // not, the caller's ceiling is below a single tag and the tag wins.
    if !fits(&build(lo)) {
        return CLOSE.to_string();
    }
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(&build(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

/// One point on the way from full detail to the rendered capsule: the rung
/// just applied and the text it produced. See [`render_ladder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LadderStep {
    pub rung: &'static str,
    pub text: String,
}

fn run_steps(
    s: &Snapshot,
    lim: &mut Limits,
    steps: &[Rung],
    target: u32,
    trace: bool,
    applied: &mut u32,
    mut trail: Option<&mut Vec<LadderStep>>,
) -> String {
    let mut text = render_with(s, lim, trace);
    for step in steps {
        loop {
            if estimate_tokens(&text) <= target {
                return text;
            }
            if !(step.apply)(lim, s) {
                break;
            }
            *applied += 1;
            text = render_with(s, lim, trace);
            if let Some(t) = trail.as_deref_mut() {
                t.push(LadderStep {
                    rung: step.name,
                    text: text.clone(),
                });
            }
        }
    }
    text
}

fn render_impl(
    s: &Snapshot,
    cfg: &RenderConfig,
    trace: bool,
    mut trail: Option<&mut Vec<LadderStep>>,
) -> Rendered {
    let target = cfg
        .budget_tokens
        .clamp(MIN_BUDGET_TOKENS, HARD_CEILING_TOKENS);
    let mut lim = Limits::FULL;
    if let Some(t) = trail.as_deref_mut() {
        t.push(LadderStep {
            rung: "full detail",
            text: render_with(s, &lim, trace),
        });
    }
    let mut steps = 0;
    let mut text = run_steps(
        s,
        &mut lim,
        SPEC_STEPS,
        target,
        trace,
        &mut steps,
        trail.as_deref_mut(),
    );
    // Both ladders now run against the caller's target rather than against the
    // ceiling. Before v0.1.2 the second ladder was skipped unless the text was
    // already above 1,000 estimated tokens, so anything between the target and
    // the ceiling was shipped untouched and `budget_tokens` meant nothing once
    // the first ladder ran out of rungs.
    if estimate_tokens(&text) > target || text.chars().count() > ABSOLUTE_MAX_CHARS {
        text = run_steps(
            s,
            &mut lim,
            CEILING_STEPS,
            target,
            trace,
            &mut steps,
            trail.as_deref_mut(),
        );
    }
    // The hard stop. After this line the text is at or below `target` estimated
    // tokens and at or below ABSOLUTE_MAX_CHARS characters, for every input.
    let before = text.len();
    text = enforce_ceiling(text, target, ABSOLUTE_MAX_CHARS);
    if let Some(t) = trail {
        if text.len() != before {
            t.push(LadderStep {
                rung: "hard stop (whole lines from the end)",
                text: text.clone(),
            });
        }
    }
    debug_assert!(estimate_tokens(&text) <= target);
    Rendered {
        tokens: estimate_tokens(&text),
        text,
        steps,
    }
}

/// Renders the capsule within budget (§16.3).
pub fn render(s: &Snapshot, cfg: &RenderConfig) -> Rendered {
    render_impl(s, cfg, false, None)
}

/// Debug render annotating each derived line with its source rows (E4).
pub fn render_traced(s: &Snapshot, cfg: &RenderConfig) -> Rendered {
    render_impl(s, cfg, true, None)
}

/// Every intermediate text of one render, in order: full detail first, then
/// the text after each rung the budget forced. The last entry is exactly what
/// [`render`] returns. This is what `velra inspect --trace` walks to say which
/// rung removed a line; it is not on any hook path.
pub fn render_ladder(s: &Snapshot, cfg: &RenderConfig) -> Vec<LadderStep> {
    let mut trail = Vec::new();
    render_impl(s, cfg, false, Some(&mut trail));
    trail
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Comma-joined non-zero summary items (§8.4).
pub fn summary(s: &Snapshot) -> String {
    let mut items = Vec::new();
    if s.root.is_some() {
        items.push("objective".to_string());
    }
    if s.failing_count > 0 {
        items.push(plural(s.failing_count, "failing test", "failing tests"));
    }
    if s.dead_end_total > 0 {
        items.push(plural(s.dead_end_total, "dead end", "dead ends"));
    }
    let files = s.working_files.len() as u32;
    if files > 0 {
        items.push(plural(files, "file", "files"));
    }
    items.join(", ")
}

/// Summary counts stored in `checkpoints.summary_json`.
pub fn summary_json(s: &Snapshot, tokens: u32) -> String {
    serde_json::json!({
        "summary": summary(s),
        "objective": s.root.is_some(),
        "failing": s.failing_count,
        "dead_ends": s.dead_end_total,
        "files": s.working_files.len(),
        "tokens": tokens,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn base() -> Snapshot {
        Snapshot {
            checkpoint_id: "ckpt_01TEST".into(),
            created_ms: 1_789_207_445_000,
            trigger: Trigger::Manual,
            partial: false,
            preview: false,
            session_id: "s1".into(),
            project_id: "p1".into(),
            epoch: 1,
            root: None,
            constraints: vec![],
            subtask: None,
            latest: None,
            earlier: None,
            git: None,
            edit_count: 0,
            last_test: None,
            failure: None,
            failing_count: 0,
            tests: vec![],
            dead_ends: vec![],
            dead_end_total: 0,
            attempts: vec![],
            working_files: vec![],
            next_target: None,
            tz_offset_secs: 0,
        }
    }

    #[test]
    fn minimal_capsule() {
        let r = render(&base(), &RenderConfig::default());
        let expected = "<VELRA_WORKSPACE_STATE v=\"1\" checkpoint=\"ckpt_01TEST\" captured=\"2026-09-12T10:04:05Z\" trigger=\"manual\">\n[ABOUT_THIS_RECORD]\n".to_string()
            + CONTEXT
            + "\n[FIRST_MESSAGE]\n(not captured)\n[WORKSPACE_STATE]\nno git | 0 edits this task | last test run: none\n[RECORD_DETAIL]\nFull detail for any section: `velra inspect --section <name>`\n</VELRA_WORKSPACE_STATE>";
        assert_eq!(r.text, expected);
        assert_eq!(r.steps, 0);
    }

    #[test]
    fn latest_omitted_when_identical_to_root() {
        let mut s = base();
        s.root = Some(IntentView {
            id: 1,
            text: "fix the login bug please".into(),
            ts_ms: s.created_ms,
        });
        s.latest = Some(IntentView {
            id: 1,
            text: "fix the login bug please".into(),
            ts_ms: s.created_ms,
        });
        assert!(!render(&s, &RenderConfig::default())
            .text
            .contains("LATEST_REQUEST"));
        s.latest.as_mut().unwrap().text = "and the tests".into();
        assert!(render(&s, &RenderConfig::default())
            .text
            .contains("[LATEST_MESSAGE]"));
    }

    #[test]
    fn no_causal_language_in_template() {
        for word in ["caused", "because", "due to", "led to"] {
            assert!(!CONTEXT.contains(word));
        }
    }

    #[test]
    fn budget_is_enforced_for_huge_input() {
        let mut s = base();
        let long = "x".repeat(5000);
        s.root = Some(IntentView {
            id: 1,
            text: long.clone(),
            ts_ms: 0,
        });
        s.subtask = Some(IntentView {
            id: 2,
            text: long.clone(),
            ts_ms: 0,
        });
        s.latest = Some(IntentView {
            id: 3,
            text: long.clone(),
            ts_ms: 0,
        });
        s.git = Some(GitInfo {
            branch: Some(long.clone()),
            head: Some("a".repeat(40)),
        });
        s.failure = Some(FailureView {
            id: 1,
            kind: CommandKind::Test,
            command: long.clone(),
            exit_code: Some(1),
            excerpt: vec!["y".repeat(200); 8],
            ts_ms: 0,
        });
        for i in 0..4 {
            s.dead_ends.push(DeadEndView {
                id: i,
                path: long.clone(),
                subagent: true,
                edit_ids: vec![1, 2, 3],
                mechanism: Mechanism::GitCommand,
                command: Some(long.clone()),
                resolved_ms: 0,
                minus: Some("m".repeat(160)),
                plus: Some("p".repeat(160)),
                observed_after: Some(CommandRef {
                    id: 9,
                    command: long.clone(),
                    outcome: Outcome::Fail,
                }),
            });
        }
        s.attempts = (0..4)
            .map(|i| AttemptView {
                edit_id: i,
                path: long.clone(),
                subagent: false,
                added: Some(1),
                removed: None,
                ts_ms: 0,
                afterward: None,
            })
            .collect();
        s.working_files = (0..8)
            .map(|i| WorkingFileView {
                path: format!("{long}{i}"),
                edits: 1,
                reads: 1,
                in_failure: true,
            })
            .collect();
        s.next_target = Some(NextTarget {
            rule: "last-active-edit",
            target: long.clone(),
            source: "edits:1".into(),
        });
        let r = render(&s, &RenderConfig::default());
        assert!(r.tokens <= HARD_CEILING_TOKENS, "{}", r.tokens);
        assert!(r.text.chars().count() <= ABSOLUTE_MAX_CHARS);
        assert!(r.text.contains("[REVERTED_EDITS] (OBSERVED)"));
        assert!(r.text.contains("[TEST_RESULT]"));
        assert!(r.text.ends_with("</VELRA_WORKSPACE_STATE>"));
    }

    #[test]
    fn summary_items() {
        let mut s = base();
        assert_eq!(summary(&s), "");
        s.root = Some(IntentView {
            id: 1,
            text: "x".into(),
            ts_ms: 0,
        });
        s.failing_count = 1;
        s.dead_end_total = 2;
        s.working_files = vec![WorkingFileView {
            path: "a".into(),
            edits: 1,
            reads: 0,
            in_failure: false,
        }];
        assert_eq!(
            summary(&s),
            "objective, 1 failing test, 2 dead ends, 1 file"
        );
    }
}
