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

/// Shortest a test identifier is ever cut to by the ladder's path rungs.
const TEST_ID_MIN_CHARS: usize = 120;

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
    /// The edit `minus`/`plus` were taken from (`snapshot::dead_end_evidence`).
    pub excerpt_edit: Option<i64>,
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
    /// Constraint sentences of the epoch, oldest first
    /// (`snapshot::pick_constraints`).
    pub constraints: Vec<ConstraintView>,
    /// Distinct live rules in the epoch (`constraints` holds at most 3).
    pub constraint_total: u32,
    /// The user's own statements ruling a route out (`crate::constraint`'s
    /// rejection labels), oldest first, each quoted with the approach it
    /// rejects. Printed under `[REJECTED_APPROACHES]`.
    pub rejections: Vec<ConstraintView>,
    /// Live rejections in the epoch (`rejections` holds at most 4).
    pub rejection_total: u32,
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
    /// List working files the capsule already prints in an earlier section.
    working_named_above: bool,
    constraints_max: usize,
    constraint_chars: usize,
    rejections_max: usize,
    rejection_chars: usize,
    /// State in a section header how many stated rules it does not list.
    omitted_notes: bool,
    attempts_max: usize,
    dead_ends_max: usize,
    /// Oldest dead ends whose replaced (`-`) line is no longer printed.
    dead_original_removed: usize,
    /// Oldest dead ends whose attempted line is no longer printed.
    dead_attempt_removed: usize,
    dead_attempt_chars: usize,
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
    /// Print the earlier message only up to the sentence that names code.
    earlier_code_only: bool,
}

impl Limits {
    const FULL: Limits = Limits {
        tests_max: 6,
        test_detail: true,
        earlier: true,
        earlier_chars: 200,
        earlier_code_only: false,
        working_max: 8,
        working_named_above: true,
        constraints_max: 3,
        constraint_chars: 200,
        rejections_max: 4,
        rejection_chars: 200,
        omitted_notes: true,
        attempts_max: 4,
        dead_ends_max: 4,
        dead_original_removed: 0,
        dead_attempt_removed: 0,
        dead_attempt_chars: 160,
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

/// Retention class of a capsule line, lowest first: the order in which the
/// hard stop ([`enforce_ceiling`]) gives sections up once both ladders are
/// exhausted. It restates, for whole lines, the policy the ladders apply to
/// detail (see [`SPEC_STEPS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Keep {
    /// INFERRED, and derivable from `[TEST_RESULT]`.
    FailureLocation,
    RecentEdits,
    FileActivity,
    /// Regenerable by running the command again; `[TEST_STATUS]` keeps the id.
    TestResult,
    Subtask,
    Earlier,
    Latest,
    /// The rejected routes: not recoverable from disk once reverted.
    RevertedEdits,
    /// The user's own statement that a route is rejected.
    Rejections,
    TestStatus,
    Constraints,
    /// Never dropped by line: the frame, the objective and `[WORKSPACE_STATE]`.
    Frame,
}

/// [`Keep`] classes in the order the hard stop removes them.
const DROP_ORDER: &[Keep] = &[
    Keep::FailureLocation,
    Keep::RecentEdits,
    Keep::FileActivity,
    Keep::TestResult,
    Keep::Subtask,
    Keep::Earlier,
    Keep::Latest,
    Keep::RevertedEdits,
    Keep::Rejections,
    Keep::TestStatus,
    Keep::Constraints,
];

struct Line {
    text: String,
    keep: Keep,
    /// Index of the section the line belongs to; headers open a new one.
    section: usize,
    header: bool,
}

struct Out {
    lines: Vec<Line>,
    trace: bool,
    keep: Keep,
    section: usize,
}

impl Out {
    fn line(&mut self, line: impl Into<String>, src: &str, header: bool) {
        let mut text = line.into();
        if self.trace && !src.is_empty() {
            text.push_str("  #src=");
            text.push_str(src);
        }
        self.lines.push(Line {
            text,
            keep: self.keep,
            section: self.section,
            header,
        });
    }

    /// Opens a section of class `keep` with its header line.
    fn header(&mut self, keep: Keep, line: impl Into<String>, src: &str) {
        self.keep = keep;
        self.section += 1;
        self.line(line, src, true);
    }

    fn push(&mut self, line: impl Into<String>, src: &str) {
        self.line(line, src, false);
    }

    /// A frame line: tags, the preamble, the record-detail pointer.
    fn plain(&mut self, line: impl Into<String>) {
        self.keep = Keep::Frame;
        self.section += 1;
        self.line(line, "", true);
    }
}

/// The failure excerpt with rule lines compacted.
///
/// Test runners frame their output with banners: pytest's
/// `===== FAILURES =====` runs to 79 characters, and the estimator charges a
/// run of `=` about one token per character, so that one line cost 85 of the
/// capsule's 740 tokens in a real VS Code session -- more than the objective
/// that the ladder cut to make room for it. A run of four or more of the same
/// rule character is collapsed to three, which keeps the word it frames
/// (`=== FAILURES ===`, `___ test_x ___`), and a line that was nothing but a
/// rule is dropped. Presentation only: the snapshot and `inspect --section
/// failure` keep the output as captured.
fn compact_rules(line: &str) -> Option<std::borrow::Cow<'_, str>> {
    const RULE: &[char] = &['=', '-', '_', '*', '#', '~'];
    let mut out = String::with_capacity(line.len());
    let mut changed = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if RULE.contains(&c) {
            let mut run = 1;
            while chars.peek() == Some(&c) {
                chars.next();
                run += 1;
            }
            let keep = if run >= 4 {
                changed = true;
                3
            } else {
                run
            };
            out.extend(std::iter::repeat_n(c, keep));
        } else {
            out.push(c);
        }
    }
    if out.chars().all(|c| c.is_whitespace() || RULE.contains(&c)) {
        return None;
    }
    Some(if changed {
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(line)
    })
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
        // What was observed is that the file's content went back to an
        // earlier state, seen by a turn-end scan or across a command that
        // could not have done it. Who or what did it is not known: a
        // formatter, another subcommand, the user's editor, the agent's own
        // shell.
        Mechanism::External => "reverted by an unidentified change".to_string(),
    }
}

/// `text` up to and including its first sentence that names code
/// ([`crate::text::names_identifier`]); all of it when no sentence does.
///
/// The earlier message is carried *because* it names code (see
/// `snapshot::earlier_message`), so under pressure that sentence is what it is
/// for, and whatever follows it is the first thing to go -- typically a
/// time-scoped aside like "Don't change it yet", the kind of sentence
/// `crate::constraint` already declines to carry across a boundary.
fn through_code_sentence(text: &str) -> &str {
    let mut start = 0;
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        let boundary = matches!(c, '.' | '?' | '!') && text[end..].starts_with(char::is_whitespace);
        if boundary {
            if crate::text::names_identifier(&text[start..end]) {
                return &text[..end];
            }
            start = end;
        }
    }
    text
}

/// Whether `line` prints `path` as a whole path: not as the tail of a longer
/// one (`src/a.py` does not name `a.py`) nor the head of one (`a.pyc`).
fn names_path(line: &str, path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\');
    line.match_indices(path).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + path.len()..].chars().next();
        !before.is_some_and(part)
            && !after.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
    })
}

/// A marker for a path that is not one of the project's own files: outside
/// the workspace (an auto-memory note, a sibling checkout) or in an installed,
/// generated or cache directory (`snapshot::is_incidental`). Empty otherwise.
fn where_is(path: &str) -> &'static str {
    if crate::snapshot::is_outside_workspace(path) {
        " (outside workspace)"
    } else if crate::snapshot::is_incidental(path) {
        " (dependency or build output)"
    } else {
        ""
    }
}

/// A section header's note that `n` more items were stated than it lists --
/// cut by the snapshot's cap or the ladder. The count only: what was left out
/// is not paraphrased.
fn not_listed(n: usize, shown: bool) -> String {
    if n == 0 || !shown {
        String::new()
    } else {
        format!(" | {n} more not listed")
    }
}

/// The rules a section prints, in the order they were stated, and how many
/// the objective line already quotes word for word (those are not repeated).
///
/// With `by_class`, a section cut below its length by the ladder keeps
/// prohibitions and labelled rules before requirements, earliest first within
/// a class -- the snapshot's rule (`snapshot::pick_constraints`, D94), so the
/// ladder cannot quietly bring back "the oldest N". Without it, the earliest.
fn pick_rules<'a>(
    rules: &'a [ConstraintView],
    max: usize,
    quoted: &dyn Fn(&str) -> bool,
    by_class: bool,
) -> (Vec<&'a ConstraintView>, usize) {
    let open: Vec<(usize, &ConstraintView)> = rules
        .iter()
        .enumerate()
        .filter(|(_, c)| !quoted(&c.text))
        .collect();
    let quoted_count = rules.len() - open.len();
    let class = |c: &ConstraintView| match c.kind {
        ConstraintKind::Prohibition | ConstraintKind::Labelled => 0,
        ConstraintKind::Requirement => u8::from(by_class),
    };
    let mut order: Vec<(usize, &ConstraintView)> = open;
    order.sort_by_key(|(i, c)| (class(c), *i));
    order.truncate(max);
    order.sort_by_key(|(i, _)| *i);
    (order.into_iter().map(|(_, c)| c).collect(), quoted_count)
}

/// A rejection quoted within `n` characters (D78's quote runs from the
/// sentence naming the approach through the one ruling it out).
///
/// Too long whole, it keeps the two parts that carry its meaning: the first
/// sentence, which names the approach, and the clause holding the rejection
/// label `cue` (`rejected approach`, `dead end`, ...), with `...` for what is
/// left out -- "Run the tests." between them, what the rejection goes on to
/// ask for after it. Only then, if that is still too long, is it cut at both
/// ends ([`truncate_middle`]). Nothing is added: every word shown is the
/// user's, in their order.
fn quote_rejection<'a>(text: &'a str, cue: &str, n: usize) -> std::borrow::Cow<'a, str> {
    if text.chars().count() <= n {
        return std::borrow::Cow::Borrowed(text);
    }
    let Some(at) = find_ascii_ci(text, cue) else {
        return truncate_middle(text, n);
    };
    let cue_end = at + cue.len();
    let is_end = |b: &[u8], i: usize| {
        matches!(b[i], b'.' | b'?' | b'!') && b.get(i + 1).is_none_or(|c| c.is_ascii_whitespace())
    };
    let b = text.as_bytes();
    // Start of the sentence holding the cue, end of the first sentence.
    let start = (0..at).rev().find(|&i| is_end(b, i)).map_or(0, |i| i + 1);
    let first_end = (0..b.len())
        .find(|&i| is_end(b, i))
        .map_or(b.len(), |i| i + 1);
    let clause_end = (cue_end..b.len())
        .find(|&i| matches!(b[i], b',' | b';') || is_end(b, i))
        .map_or(b.len(), |i| if is_end(b, i) { i + 1 } else { i });
    let clause = text[start..clause_end].trim();
    let more = if clause_end < b.len() { " ..." } else { "" };
    if start < first_end {
        // The label is in the first sentence: the approach and its rejection
        // are one sentence, kept from its start.
        let whole = format!("{}{more}", text[..clause_end].trim());
        return if whole.chars().count() <= n {
            std::borrow::Cow::Owned(whole)
        } else {
            std::borrow::Cow::Owned(truncate_chars(&whole, n).into_owned())
        };
    }
    let approach = text[..first_end].trim();
    let candidate = format!("{approach} ... {clause}{more}");
    if candidate.chars().count() <= n {
        return std::borrow::Cow::Owned(candidate);
    }
    // Still too long: the rejection clause is kept whole and the approach
    // sentence is cut from its end -- it names the approach first ("try a
    // workaround that stores the key in ...").
    let fixed = " ... ".len() + clause.chars().count() + more.len();
    if n >= fixed + 24 {
        let head = truncate_chars(approach, n - fixed);
        return std::borrow::Cow::Owned(format!("{head} {clause}{more}"));
    }
    std::borrow::Cow::Owned(truncate_middle(&candidate, n).into_owned())
}

/// Byte offset of the first ASCII-case-insensitive match of `needle`.
fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || !needle.is_ascii() {
        return None;
    }
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    (0..=h.len().checked_sub(n.len())?)
        .find(|&i| hay.is_char_boundary(i) && h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// `text` within `n` characters with both of its ends: the opening and the
/// close, joined by ` ... `. The close gets the larger share and starts at a
/// word. For text whose point is at the end -- the latest message's request
/// after a long preamble, a rejection that names the approach first and rules
/// it out last -- where a head-only cut keeps the part that matters least.
fn truncate_middle(text: &str, n: usize) -> std::borrow::Cow<'_, str> {
    const SEP: &str = " ... ";
    let total = text.chars().count();
    if total <= n {
        return std::borrow::Cow::Borrowed(text);
    }
    if n < 24 {
        return truncate_chars(text, n);
    }
    let keep = n - SEP.len();
    let tail_n = keep * 3 / 5;
    let head_n = keep - tail_n;
    let head: String = text.chars().take(head_n).collect();
    let mut tail: String = text.chars().skip(total - tail_n).collect();
    // A cut inside a word moves to the next word when one begins soon after.
    let mid_word = text
        .chars()
        .nth(total - tail_n - 1)
        .is_some_and(|c| !c.is_whitespace())
        && tail.chars().next().is_some_and(|c| !c.is_whitespace());
    if mid_word {
        if let Some((at, _)) = tail
            .char_indices()
            .take(20)
            .find(|(_, c)| c.is_whitespace())
        {
            tail = tail[at..].to_string();
        }
    }
    let tail = tail.trim_start();
    std::borrow::Cow::Owned(format!("{}{SEP}{tail}", head.trim_end()))
}

fn render_with(s: &Snapshot, lim: &Limits, trace: bool) -> String {
    join(&render_lines(s, lim, trace))
}

fn join(lines: &[Line]) -> String {
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    texts.join("\n")
}

fn render_lines(s: &Snapshot, lim: &Limits, trace: bool) -> Vec<Line> {
    let tz = s.tz_offset_secs;
    let mut o = Out {
        lines: Vec::with_capacity(48),
        trace,
        keep: Keep::Frame,
        section: 0,
    };
    let path = |p: &str| truncate_chars_front(p, lim.path_chars).into_owned();
    // A test identifier is only useful whole: `...:test_x` names no file to run.
    // The path rungs that shorten file paths therefore stop at a floor for it,
    // and a pathological identifier is left to the hard stop.
    let test_id =
        |p: &str| truncate_chars_front(p, lim.path_chars.max(TEST_ID_MIN_CHARS)).into_owned();

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
            o.header(
                Keep::Frame,
                format!(
                    "[FIRST_MESSAGE] (OBSERVED | user prompt | {})",
                    hh_mm(r.ts_ms, tz)
                ),
                &src,
            );
            o.push(truncate_chars(&r.text, lim.root_chars), &src);
        }
        None => {
            o.header(Keep::Frame, "[FIRST_MESSAGE]", "intents:none");
            o.push("(not captured)", "intents:none");
        }
    }
    // A constraint sentence the objective line above already prints, word for
    // word, is not printed twice: that costs the budget twice and, in the D64
    // busy session, a 52-token duplicate outlived a real constraint the ceiling
    // ladder dropped to pay for it. The test is against the objective *as
    // rendered*: once the ladder trims the objective, a sentence it no longer
    // shows comes back here, which is the case the section exists for -- a rule
    // in one sentence of a longer prompt whose tail is about to be cut.
    let shown_root = s
        .root
        .as_ref()
        .map(|r| truncate_chars(&r.text, lim.root_chars));
    let quoted_by_root = |text: &str| shown_root.as_deref().is_some_and(|r| r.contains(text));
    let (constraints, constraints_quoted) =
        pick_rules(&s.constraints, lim.constraints_max, &quoted_by_root, true);
    // Rules the snapshot selected or counted that neither this section nor the
    // objective line prints: the snapshot's own cap (D94) and this ladder's.
    // The count is stated so that a missing rule reads as "not shown here",
    // never as "there was none"; its text is not invented.
    let constraints_omitted = (s.constraint_total as usize)
        .max(s.constraints.len())
        .saturating_sub(constraints.len() + constraints_quoted);
    if !constraints.is_empty() || constraints_omitted > 0 {
        let all: Vec<i64> = constraints.iter().map(|c| c.id).collect();
        o.header(
            Keep::Constraints,
            format!(
                "[STATED_CONSTRAINTS] (OBSERVED | user prompt | quoted verbatim{})",
                not_listed(constraints_omitted, lim.omitted_notes)
            ),
            &ids("constraints", &all),
        );
        for c in &constraints {
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
        if constraints.is_empty() {
            o.push("- (none listed here)", "constraints:omitted");
        }
    }

    // The user's own statement that a route is ruled out, quoted with the
    // approach it rejects (D78). It is printed on its own rather than left to
    // the objective line, which carries it only while the clause happens to
    // fall inside the objective's excerpt. The quote keeps both its ends --
    // the approach is named at the start, the rejection at the close.
    let (rejections, rejections_quoted) =
        pick_rules(&s.rejections, lim.rejections_max, &quoted_by_root, false);
    let rejections_omitted = (s.rejection_total as usize)
        .max(s.rejections.len())
        .saturating_sub(rejections.len() + rejections_quoted);
    if !rejections.is_empty() || rejections_omitted > 0 {
        let all: Vec<i64> = rejections.iter().map(|c| c.id).collect();
        o.header(
            Keep::Rejections,
            format!(
                "[REJECTED_APPROACHES] (OBSERVED | user prompt{})",
                not_listed(rejections_omitted, lim.omitted_notes)
            ),
            &ids("constraints", &all),
        );
        for c in &rejections {
            o.push(
                format!(
                    "- turn {} | \"{}\"",
                    c.prompt_ordinal,
                    quote_rejection(&c.text, &c.cue, lim.rejection_chars)
                ),
                &format!("constraints:{}", c.id),
            );
        }
        if rejections.is_empty() {
            o.push("- (none listed here)", "constraints:omitted");
        }
    }

    if let Some(st) = s.subtask.as_ref().filter(|_| lim.subtask) {
        let src = format!("intents:{}", st.id);
        o.header(
            Keep::Subtask,
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
        o.header(
            Keep::Earlier,
            format!(
                "[EARLIER_MESSAGE] (OBSERVED | user prompt | {})",
                hh_mm(e.ts_ms, tz)
            ),
            &src,
        );
        let text = if lim.earlier_code_only {
            through_code_sentence(&e.text)
        } else {
            &e.text
        };
        o.push(truncate_chars(text, lim.earlier_chars), &src);
    }
    if let Some(l) = s
        .latest
        .as_ref()
        .filter(|l| lim.latest && s.root.as_ref().is_none_or(|r| r.text != l.text))
    {
        let src = format!("intents:{}", l.id);
        o.header(
            Keep::Latest,
            format!(
                "[LATEST_MESSAGE] (OBSERVED | user prompt | {})",
                hh_mm(l.ts_ms, tz)
            ),
            &src,
        );
        // Both ends: a long latest message usually closes with the request
        // (after a pasted log, a quoted spec, a long preamble), and a head-only
        // cut spent the whole allowance on the preamble.
        o.push(truncate_middle(&l.text, lim.latest_chars), &src);
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
    o.header(Keep::Frame, "[WORKSPACE_STATE]", &status_src);
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
        o.header(
            Keep::TestStatus,
            "[TEST_STATUS] (OBSERVED | latest run covering each)",
            &ids("commands", &cmd_ids),
        );
        for t in &tests {
            let line = if t.failing {
                let detail = match (&t.detail, lim.test_detail) {
                    (Some(d), true) => format!(" | {}", truncate_chars(d, 60)),
                    _ => String::new(),
                };
                format!("- FAIL {}{detail}", test_id(&t.id))
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
                    test_id(&t.id),
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
        o.header(
            Keep::TestResult,
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
        let excerpt: Vec<std::borrow::Cow<'_, str>> = f
            .excerpt
            .iter()
            .filter_map(|line| compact_rules(line))
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
    // Cut to fewer routes, the ones kept are those on a file the task's own
    // words name -- the objective, a stated rule or rejection -- and then the
    // most recent. "The most recent" alone kept an unrelated later revert and
    // dropped the route the task was about.
    let named = |p: &str| {
        s.root
            .iter()
            .map(|r| r.text.as_str())
            .chain(s.rejections.iter().map(|c| c.text.as_str()))
            .chain(s.constraints.iter().map(|c| c.text.as_str()))
            .any(|t| names_path(t, p))
    };
    if groups.len() > lim.dead_ends_max {
        let mut order: Vec<usize> = (0..groups.len()).collect();
        order.sort_by_key(|&i| (!named(&groups[i][0].1.path), i));
        order.truncate(lim.dead_ends_max);
        order.sort_unstable();
        groups = order.into_iter().map(|i| groups[i].clone()).collect();
    }
    if !groups.is_empty() {
        let all: Vec<i64> = groups.iter().flatten().map(|(_, d)| d.id).collect();
        o.header(
            Keep::RevertedEdits,
            "[REVERTED_EDITS] (OBSERVED)",
            &ids("dead_ends", &all),
        );
        let n = s.dead_ends.len();
        // The order excerpt lines are given up in, last first: routes on a
        // file the task names are kept longest, then the most recent. `rank[i]`
        // is dead end `i`'s place in that order.
        let mut preference: Vec<usize> = (0..n).collect();
        preference.sort_by_key(|&i| (!named(&s.dead_ends[i].path), i));
        let mut rank = vec![0usize; n];
        for (r, &i) in preference.iter().enumerate() {
            rank[i] = r;
        }
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
                    "- {}{}{subagent} | {} edit(s) | {} at {}",
                    path(&last.path),
                    where_is(&last.path),
                    edit_ids.len(),
                    mechanism_text(last, lim.command_chars),
                    hh_mm(last.resolved_ms, tz)
                )
            } else {
                format!(
                    "- {}{}{subagent} | {} reverts, {} edit(s) | last {} at {}",
                    path(&last.path),
                    where_is(&last.path),
                    group.len(),
                    edit_ids.len(),
                    mechanism_text(last, lim.command_chars),
                    hh_mm(last.resolved_ms, tz)
                )
            };
            o.push(header, &src);
            for (i, d) in group {
                let member_src = format!("dead_ends:{},{}", d.id, ids("edits", &d.edit_ids));
                // Dead ends are listed most recent first; excerpt lines are
                // removed oldest first, counted across every dead end, not per
                // group. The line an attempt *added* is what identifies the
                // rejected route, and after a revert it exists nowhere but here;
                // the line it replaced is back on disk. So the replaced line goes
                // first, and the added one is shortened long before it is
                // dropped. An attempt that only deleted has no added line, and
                // then the deleted line is the attempt.
                let (original, attempted) = match (&d.minus, &d.plus) {
                    (m, Some(p)) => (m.as_ref(), Some(('+', p))),
                    (Some(m), None) => (None, Some(('-', m))),
                    (None, None) => (None, None),
                };
                let age = n - rank[*i];
                // Which of the dead end's edits the excerpt is from, when it
                // grouped several: the snapshot picks the change that was
                // finally rejected, not the first step towards it (D93), and
                // the line says so. No position is claimed without the edit.
                let from = match (d.excerpt_edit, d.edit_ids.len()) {
                    (Some(e), n) if n > 1 => d
                        .edit_ids
                        .iter()
                        .position(|&x| x == e)
                        .map(|p| format!("  (edit {} of {n})", p + 1)),
                    _ => None,
                };
                let member_src = match d.excerpt_edit {
                    Some(e) => format!("{member_src},excerpt=edits:{e}"),
                    None => member_src,
                };
                if let Some(m) = original.filter(|_| age > lim.dead_original_removed) {
                    o.push(format!("    - {m}"), &member_src);
                }
                if let Some((sign, a)) = attempted.filter(|_| age > lim.dead_attempt_removed) {
                    o.push(
                        format!(
                            "    {sign} {}{}",
                            truncate_chars(a, lim.dead_attempt_chars),
                            from.as_deref().unwrap_or("")
                        ),
                        &member_src,
                    );
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
        o.header(
            Keep::RecentEdits,
            "[RECENT_EDITS] (OBSERVED)",
            &ids("edits", &all),
        );
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
                    "- {}{}{} | +{}/-{} lines | {} | afterward: {after}",
                    path(&a.path),
                    where_is(&a.path),
                    if a.subagent { " (subagent)" } else { "" },
                    num(a.added),
                    num(a.removed),
                    hh_mm(a.ts_ms, tz)
                ),
                &src,
            );
        }
    }

    // A file the capsule already prints above -- as a test id's file, a
    // reverted file, a recent edit, inside a command -- adds only its read and
    // edit counts here. Those are the cheapest detail in the capsule and go
    // before any line of the objective does.
    let printed: Vec<&str> = o.lines.iter().map(|l| l.text.as_str()).collect();
    let working: Vec<&WorkingFileView> = s
        .working_files
        .iter()
        .take(lim.working_max)
        .filter(|w| {
            lim.working_named_above || !printed.iter().any(|l| names_path(l, &path(&w.path)))
        })
        .collect();
    if !working.is_empty() {
        o.header(
            Keep::FileActivity,
            "[FILE_ACTIVITY] (OBSERVED)",
            &format!("file_stats:{}/epoch:{}", s.session_id, s.epoch),
        );
        for w in working {
            o.push(
                format!(
                    "- {}{} | edited {}x, read {}x{}",
                    path(&w.path),
                    where_is(&w.path),
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
        // Only a location taken from the failing output is a failure location;
        // the most recent live edit is a place to pick up, and is not called
        // one.
        let section = if t.rule == "failure-location" {
            "FAILURE_LOCATION"
        } else {
            "NEXT_TARGET"
        };
        o.header(
            Keep::FailureLocation,
            format!("[{section}] (INFERRED | {})", t.rule),
            &t.source,
        );
        o.push(
            format!("{}{}", path(&t.target), where_is(&t.target)),
            &t.source,
        );
    }

    o.plain("[RECORD_DETAIL]");
    // Neither the checkpoint id nor the list of section names is repeated here.
    // The id is already in the opening tag, and spelling it twice cost about 30
    // tokens of dense ULID; the four section names cost another 40 to say what
    // `velra inspect --section` prints when given a name it does not know.
    o.plain("Full detail for any section: `velra inspect --section <name>`");
    o.plain("</VELRA_WORKSPACE_STATE>");
    o.lines
}

/// One rung of the truncation ladder: a name for diagnostics, and a move that
/// returns false when it can no longer change anything (so the loop moves on).
struct Rung {
    name: &'static str,
    apply: fn(&mut Limits, &Snapshot) -> bool,
}

/// Spec truncation order (§16.3), revised in v0.1.2 for retention priority.
///
/// # The retention policy
///
/// The capsule exists so that a new session can carry on without re-deriving
/// the task. What it must never lose is what the next session cannot get back
/// by reading the repository; what it should lose first is what it can. Every
/// rung below, and the hard stop's section order ([`DROP_ORDER`]), applies
/// that one rule:
///
/// | class | state | why |
/// |---|---|---|
/// | CRITICAL | objective (`[FIRST_MESSAGE]`), stated constraints, exact failing test ids, the active failure's command and result, each rejected route's header (its file, that it was reverted, and how), the top working files | only the session knew them |
/// | IMPORTANT | the line a rejected attempt added, later messages (the earlier one naming the next target above the latest), the failure's assertion context, test detail, recent edits, the inferred failure location | useful, but narrower or partly re-derivable; the attempt's line is off disk once reverted, which is why it outlives everything else here in the spec ladder |
/// | LOW | lists past their first entries, verbose runner output and its banners, "observed afterward" prose, the line a reverted edit replaced, a constraint the objective already quotes | regenerable or redundant: re-run the command, read the file |
///
/// Injected metadata (the IDE's open-file notice, task notifications) is not
/// in the table because it never reaches a snapshot: `crate::prompt` removes it
/// before a message becomes an intent.
///
/// Every CRITICAL item is shortened only after every LOW and IMPORTANT
/// reduction in the spec ladder has run, and removed only in the ceiling
/// ladder or by the hard stop. A rung that would save nothing is not taken.
///
/// # What moved, and why
///
/// A real VS Code session (1 edit, 1 revert, 2 files, 1 failing test) crossed
/// the 740-token target, and the ladder cut the objective from 240 to 160
/// characters at its sixth rung and the reverted edit's excerpt at its third,
/// while a 79-character pytest `=====` banner (about 85 estimated tokens), the
/// "observed afterward" line and the inferred location all survived. The
/// objective's loss was compounded by the IDE block taking its first 150
/// characters, fixed in `crate::prompt`; the order was wrong on its own:
///
/// * `first_message 240->160` now runs after every non-critical rung;
/// * the reverted-edit excerpt is split: the replaced line (on disk again) goes
///   early, the attempted line is shortened last in the spec ladder and
///   dropped only in the ceiling ladder, before any of the user's own words;
/// * the failure excerpt's rule lines are compacted before it is budgeted
///   ([`compact_rules`]), so a banner no longer costs a line of the objective.
///
/// The v0.1.2 identifier rules stand (D64): prose an agent can regenerate goes
/// before any exact identifier does.
const SPEC_STEPS: &[Rung] = &[
    // LOW: bulk and regenerable detail.
    Rung {
        name: "working_files 8->4",
        apply: |l, _| std::mem::replace(&mut l.working_max, 4) != 4,
    },
    Rung {
        name: "recent_edits 4->2",
        apply: |l, _| std::mem::replace(&mut l.attempts_max, 2) != 2,
    },
    Rung {
        name: "test_result excerpt 8->3",
        apply: |l, _| std::mem::replace(&mut l.failure_lines, 3) != 3,
    },
    Rung {
        name: "observed_afterward off",
        apply: |l, _| std::mem::replace(&mut l.observed, false),
    },
    Rung {
        name: "reverted_edits replaced line, oldest first",
        apply: |l, s| {
            if l.dead_original_removed < s.dead_ends.len() {
                l.dead_original_removed += 1;
                true
            } else {
                false
            }
        },
    },
    Rung {
        name: "working_files already named above off",
        apply: |l, _| std::mem::replace(&mut l.working_named_above, false),
    },
    Rung {
        name: "failure_location off",
        apply: |l, _| std::mem::replace(&mut l.next_target, false),
    },
    // IMPORTANT: shortened, then the regenerable parts removed.
    Rung {
        name: "latest_message 200->120 chars",
        apply: |l, _| std::mem::replace(&mut l.latest_chars, 120) != 120,
    },
    Rung {
        name: "test_result excerpt 3->0",
        apply: |l, _| std::mem::replace(&mut l.failure_lines, 0) != 0,
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
    // CRITICAL: shortened last, never removed here.
    Rung {
        name: "reverted_edits attempted line 160->80 chars",
        apply: |l, _| std::mem::replace(&mut l.dead_attempt_chars, 80) != 80,
    },
    Rung {
        name: "first_message 240->160 chars",
        apply: |l, _| std::mem::replace(&mut l.root_chars, 160) != 160,
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
/// words about what to do next and what not to do. The objective is the user's
/// own words about what the task is, so it is shortened only once the later
/// messages have been shortened and dropped.
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
        name: "working_files 2->0",
        apply: |l, _| std::mem::replace(&mut l.working_max, 0) != 0,
    },
    Rung {
        name: "test_status 3->1",
        apply: |l, _| std::mem::replace(&mut l.tests_max, 1) != 1,
    },
    // Shorten a message before dropping it: a whole message is 50-90 tokens,
    // and the ladder is rarely more than a few dozen over by this point.
    // The count of rules a section does not list is bookkeeping: it goes
    // before any of the user's own words do.
    Rung {
        name: "omitted-rule counts off",
        apply: |l, _| std::mem::replace(&mut l.omitted_notes, false),
    },
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
    // A rejection keeps both ends as it shortens -- the approach it names and
    // the clause that rules it out -- and is shortened before the line a
    // reverted attempt added is dropped: that line is a single identifier the
    // rejection's prose does not spell, at a fraction of the cost.
    Rung {
        name: "rejections 200->140 chars",
        apply: |l, _| std::mem::replace(&mut l.rejection_chars, 140) != 140,
    },
    Rung {
        name: "rejections 140->100 chars",
        apply: |l, _| std::mem::replace(&mut l.rejection_chars, 100) != 100,
    },
    // A rejected route keeps its header -- the file, that it was reverted, and
    // how -- which is the claim the section exists to make. The literal line
    // the attempt added is detail on top of that claim, so it goes before the
    // user's own words: any stated constraint, the earlier message that names
    // the next target, the objective.
    Rung {
        name: "reverted_edits attempted line off, oldest first",
        apply: |l, s| {
            if l.dead_attempt_removed < s.dead_ends.len() {
                l.dead_attempt_removed += 1;
                true
            } else {
                false
            }
        },
    },
    Rung {
        name: "earlier_message to its sentence naming code",
        apply: |l, _| !std::mem::replace(&mut l.earlier_code_only, true),
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
    // Whole rejected routes go only after every message but the objective
    // has been shortened or dropped: a route's header is the one claim the
    // section exists to make.
    Rung {
        name: "reverted_edits 4->2 files",
        apply: |l, _| std::mem::replace(&mut l.dead_ends_max, 2) != 2,
    },
    Rung {
        name: "rejections 4->2",
        apply: |l, _| std::mem::replace(&mut l.rejections_max, 2) != 2,
    },
    Rung {
        name: "first_message 160->100 chars",
        apply: |l, _| std::mem::replace(&mut l.root_chars, 100) != 100,
    },
    Rung {
        name: "constraints 2->1",
        apply: |l, _| std::mem::replace(&mut l.constraints_max, 1) != 1,
    },
    Rung {
        name: "rejections 2->1",
        apply: |l, _| std::mem::replace(&mut l.rejections_max, 1) != 1,
    },
    Rung {
        name: "subtask off",
        apply: |l, _| std::mem::replace(&mut l.subtask, false),
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
    // Last: a whole rejected route is one fact, and the objective at sixty
    // characters still states the task; paths and commands are already short.
    Rung {
        name: "reverted_edits 2->1 files",
        apply: |l, _| std::mem::replace(&mut l.dead_ends_max, 1) != 1,
    },
];

/// Last-resort guarantee that the block never exceeds `ceiling` (E2).
///
/// Every ladder step above is a judgement about which content matters least.
/// This is the backstop that turns "at or below the target, always" from an
/// expectation into a property. It works in two moves, and only the second one
/// is unconditional:
///
///  1. Drop whole lines, lowest retention class first ([`DROP_ORDER`]), and
///     within a class from the end of the capsule. A section's header goes with
///     its last body line, so no header is left standing alone. The frame, the
///     objective and `[WORKSPACE_STATE]` are never dropped here, so whatever
///     survives is still a well formed capsule that closes its own tag.
///  2. If even that is too large -- a pathological checkpoint id, an estimator
///     that reads the frame alone above the target -- shrink by binary search
///     on characters until it fits, then close the tag.
///
/// Until v0.1.2 the first move dropped lines by *position*, from the end of the
/// body up, so `[REVERTED_EDITS]` went before any line of `[TEST_RESULT]`
/// simply because it is printed later -- the opposite of the policy the ladders
/// apply.
fn enforce_ceiling(lines: &[Line], ceiling: u32, max_chars: usize) -> String {
    let fits = |t: &str| estimate_tokens(t) <= ceiling && t.chars().count() <= max_chars;
    let mut alive = vec![true; lines.len()];
    let kept = |alive: &[bool]| {
        let texts: Vec<&str> = lines
            .iter()
            .zip(alive)
            .filter(|(_, a)| **a)
            .map(|(l, _)| l.text.as_str())
            .collect();
        texts.join("\n")
    };
    let text = kept(&alive);
    if fits(&text) {
        return text;
    }
    for &class in DROP_ORDER {
        for i in (0..lines.len()).rev() {
            if !alive[i] || lines[i].keep != class {
                continue;
            }
            alive[i] = false;
            let section = lines[i].section;
            let body_left = lines
                .iter()
                .zip(&alive)
                .any(|(l, a)| *a && l.section == section && !l.header);
            if !body_left {
                for (j, l) in lines.iter().enumerate() {
                    if l.section == section {
                        alive[j] = false;
                    }
                }
            }
            let candidate = kept(&alive);
            if fits(&candidate) {
                return candidate;
            }
        }
    }
    hard_trim(&kept(&alive), ceiling, max_chars)
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
            let before = *lim;
            if !(step.apply)(lim, s) {
                break;
            }
            // A rung with nothing to remove in this state -- the working-file
            // cap when two files are listed -- is not a step. Counting it made a
            // two-file session report twelve rungs applied.
            let next = render_with(s, lim, trace);
            if next == text {
                continue;
            }
            // Nor is one that saves nothing: shortening the objective past a
            // constraint sentence it was carrying brings that sentence back
            // under [STATED_CONSTRAINTS], and the objective would be cut for
            // no budget at all.
            if estimate_tokens(&next) >= estimate_tokens(&text) {
                *lim = before;
                break;
            }
            *applied += 1;
            text = next;
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
    text = enforce_ceiling(&render_lines(s, &lim, trace), target, ABSOLUTE_MAX_CHARS);
    if let Some(t) = trail {
        if text.len() != before {
            t.push(LadderStep {
                rung: "hard stop (whole lines, lowest retention class first)",
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
            rejections: vec![],
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
            constraint_total: 0,
            rejection_total: 0,
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
                excerpt_edit: None,
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
    fn rule_lines_are_compacted_and_bare_rules_dropped() {
        let c = |l: &str| compact_rules(l).map(|c| c.into_owned());
        assert_eq!(
            c("================================== FAILURES ==================================="),
            Some("=== FAILURES ===".into())
        );
        assert_eq!(
            c("_____ test_retry_preserves_idempotency_key _____"),
            Some("___ test_retry_preserves_idempotency_key ___".into())
        );
        assert_eq!(c("=========="), None);
        assert_eq!(c("   "), None);
        assert_eq!(c(""), None);
        // Short runs and ordinary text are untouched, and borrowed.
        assert!(matches!(
            compact_rules("E   assert a == b -- x"),
            Some(std::borrow::Cow::Borrowed(_))
        ));
        assert_eq!(c("a_b__c"), Some("a_b__c".into()));
    }

    #[test]
    fn a_path_is_named_only_as_a_whole_path() {
        assert!(names_path("- FAIL tests/x.py::test_y", "tests/x.py"));
        assert!(names_path(
            "reverted via `git restore src/a.py` at",
            "src/a.py"
        ));
        assert!(names_path("see src/a.py.", "src/a.py"));
        assert!(!names_path("- src/a.py | 1 edit", "a.py"));
        assert!(!names_path("src/a.pyc", "src/a.py"));
        assert!(!names_path("src/a.py/b", "src/a.py"));
        assert!(!names_path("anything", ""));
    }

    #[test]
    fn the_earlier_message_is_cut_after_the_sentence_that_names_code() {
        assert_eq!(
            through_code_sentence(
                "Next, look at parse_chunk_size in src/http/codec.rs. Don't change it yet."
            ),
            "Next, look at parse_chunk_size in src/http/codec.rs."
        );
        assert_eq!(
            through_code_sentence("Right. Now check retry_backoff() please. Thanks!"),
            "Right. Now check retry_backoff() please."
        );
        assert_eq!(
            through_code_sentence("no code here. none."),
            "no code here. none."
        );
        assert_eq!(through_code_sentence("src/a.py"), "src/a.py");
    }

    /// The backstop removes by retention class, not by position: with the
    /// ladders unable to reach the target, regenerable output goes before the
    /// rejected routes, and those before the exact test ids.
    #[test]
    fn the_hard_stop_drops_the_lowest_class_first() {
        let mut s = base();
        s.root = Some(IntentView {
            id: 1,
            text: "fix the ledger rounding".into(),
            ts_ms: 0,
        });
        s.tests = vec![TestStatusView {
            id: "tests/test_a.py::test_round".into(),
            failing: true,
            detail: None,
            last_fail: CommandRef {
                id: 1,
                command: "pytest".into(),
                outcome: Outcome::Fail,
            },
            last_fail_ms: 0,
            passed: None,
            passed_ms: None,
        }];
        s.failure = Some(FailureView {
            id: 1,
            kind: CommandKind::Test,
            command: "pytest -q".into(),
            exit_code: Some(1),
            excerpt: vec![],
            ts_ms: 0,
        });
        s.dead_ends = vec![DeadEndView {
            id: 1,
            path: "src/money.py".into(),
            subagent: false,
            edit_ids: vec![1],
            mechanism: Mechanism::InverseEdit,
            command: None,
            resolved_ms: 0,
            minus: None,
            plus: None,
            excerpt_edit: None,
            observed_after: None,
        }];
        let full = render(
            &s,
            &RenderConfig {
                budget_tokens: 1000,
            },
        )
        .text;
        let full_tokens = estimate_tokens(&full);
        let test_result = estimate_tokens(
            "\n[TEST_RESULT] (OBSERVED | test run | 00:00)\nCommand: pytest -q\nResult: FAIL (exit 1)",
        );
        // Just enough pressure that one section must go.
        let tight = render(
            &s,
            &RenderConfig {
                budget_tokens: full_tokens - test_result / 2,
            },
        )
        .text;
        // The last line of [TEST_RESULT] went; nothing printed after it did.
        assert!(!tight.contains("Result: FAIL (exit 1)"), "{tight}");
        assert!(tight.contains("[REVERTED_EDITS]"), "{tight}");
        assert!(tight.contains("- src/money.py |"), "{tight}");
        assert!(tight.contains("tests/test_a.py::test_round"), "{tight}");
        assert!(tight.contains("fix the ledger rounding"), "{tight}");
    }

    /// A rung that would cost as much as it saves is not taken: shortening the
    /// objective past a constraint it quotes brings the constraint back.
    #[test]
    fn a_rung_that_saves_nothing_is_not_taken() {
        let mut s = base();
        let text = format!(
            "{} Never change the public API of the server module.",
            "Find why the parser disagrees with the spec and fix it. ".repeat(3)
        );
        s.root = Some(IntentView {
            id: 1,
            text: text.clone(),
            ts_ms: 0,
        });
        s.constraints = vec![ConstraintView {
            id: 1,
            text: "Never change the public API of the server module.".into(),
            kind: ConstraintKind::Prohibition,
            cue: "never ".into(),
            prompt_ordinal: 0,
            ts_ms: 0,
        }];
        let full = render(
            &s,
            &RenderConfig {
                budget_tokens: 1000,
            },
        );
        assert!(!full.text.contains("[STATED_CONSTRAINTS]"), "{}", full.text);
        let ladder = render_ladder(
            &s,
            &RenderConfig {
                budget_tokens: full.tokens - 2,
            },
        );
        for w in ladder.windows(2) {
            assert!(
                estimate_tokens(&w[1].text) < estimate_tokens(&w[0].text),
                "rung `{}` did not reduce the estimate",
                w[1].rung
            );
        }
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

    #[test]
    fn a_middle_cut_keeps_both_ends_within_the_limit() {
        let text = format!("{} Now rename x to y.", "background ".repeat(40));
        for n in [24usize, 40, 80, 200] {
            let cut = truncate_middle(&text, n);
            assert!(cut.chars().count() <= n, "{n}: {cut}");
            assert!(cut.starts_with("backg"), "{n}: {cut}");
            assert!(cut.contains(" ... "), "{n}: {cut}");
        }
        assert!(truncate_middle(&text, 80).ends_with("Now rename x to y."));
        // Short text is untouched; a tiny limit falls back to a head cut.
        assert_eq!(truncate_middle("short", 80), "short");
        assert!(truncate_middle(&text, 10).chars().count() <= 10);
        // Multi-byte text is cut on character boundaries.
        let wide = "é".repeat(300);
        assert!(truncate_middle(&wide, 50).chars().count() <= 50);
    }

    #[test]
    fn a_rejection_is_quoted_by_its_approach_and_its_label() {
        let text = "Then try a temporary workaround that stores the idempotency key in a \
                    module-level variable in src/payments/retry.py. Run the tests. That \
                    workaround is considered a rejected approach for this task, so revert it \
                    with git restore src/payments/retry.py, rerun the tests, and stop.";
        let full = quote_rejection(text, "rejected approach", 1_000);
        assert_eq!(full, text);
        let q = quote_rejection(text, "rejected approach", 200);
        assert_eq!(
            q,
            "Then try a temporary workaround that stores the idempotency key in a \
             module-level variable in src/payments/retry.py. ... That workaround is \
             considered a rejected approach for this task ..."
        );
        // Tighter: the label's clause whole, the approach from its start.
        let q = quote_rejection(text, "rejected approach", 140);
        assert!(q.chars().count() <= 140, "{q}");
        assert!(q.starts_with("Then try a temporary workaround that"), "{q}");
        assert!(
            q.contains("considered a rejected approach for this task"),
            "{q}"
        );
        // Every limit is honoured, down to a head cut.
        for n in [20usize, 60, 100, 120, 180] {
            assert!(
                quote_rejection(text, "rejected approach", n)
                    .chars()
                    .count()
                    <= n
            );
        }
        // A label in the first sentence keeps that sentence from its start.
        let one = "Caching the key in the request object is a dead end here, and the \
                   reason is that the object is rebuilt on every retry attempt.";
        assert_eq!(
            quote_rejection(one, "dead end", 80),
            "Caching the key in the request object is a dead end here ..."
        );
        // A label that is not found is a two-ended cut.
        assert!(quote_rejection(text, "nothing like it", 80).contains(" ... "));
    }

    #[test]
    fn a_shortened_rule_list_keeps_prohibitions_in_stated_order() {
        let rule = |id: i64, text: &str, kind: ConstraintKind| ConstraintView {
            id,
            text: text.into(),
            kind,
            cue: String::new(),
            prompt_ordinal: id,
            ts_ms: id,
        };
        let rules = vec![
            rule(1, "You must keep the API.", ConstraintKind::Requirement),
            rule(2, "You must use the logger.", ConstraintKind::Requirement),
            rule(3, "Do not modify the tests.", ConstraintKind::Prohibition),
        ];
        let none = |_: &str| false;
        let (kept, quoted) = pick_rules(&rules, 2, &none, true);
        assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(quoted, 0);
        let (kept, _) = pick_rules(&rules, 1, &none, true);
        assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), [3]);
        // Without classes, the earliest.
        let (kept, _) = pick_rules(&rules, 1, &none, false);
        assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), [1]);
        // A rule the objective already quotes is counted, not listed.
        let api = |t: &str| t.contains("API");
        let (kept, quoted) = pick_rules(&rules, 3, &api, true);
        assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(quoted, 1);
    }
}
