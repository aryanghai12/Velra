//! The Continuation Capsule (§16), `render_version = 1`.
//!
//! Rendering is a pure function of a [`Snapshot`] and a [`RenderConfig`]:
//! identical input yields byte-identical output on every platform.

use crate::git::GitInfo;
use crate::model::{CommandKind, Mechanism, Outcome, Trigger};
use crate::text::{estimate_tokens, truncate_chars, truncate_chars_front};
use crate::time::{hh_mm, rfc3339_utc};

pub const RENDER_VERSION: i64 = 1;
pub const DEFAULT_BUDGET_TOKENS: u32 = 800;
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
    pub subtask: Option<IntentView>,
    pub latest: Option<IntentView>,
    pub git: Option<GitInfo>,
    pub edit_count: u32,
    pub last_test: Option<CommandRef>,
    pub failure: Option<FailureView>,
    /// Distinct test/build/lint signatures whose latest run failed.
    pub failing_count: u32,
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
}

impl Limits {
    const FULL: Limits = Limits {
        working_max: 8,
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

const CONTEXT: &str = "Velra is a local tool that recorded this task state from Claude Code tool events before the conversation was compacted. Entries are observations of tool activity. Links between an edit and a later test result are unconfirmed unless stated. Files on disk are the current source of truth for code.";

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
        "<VELRA_CONTINUATION v=\"1\" checkpoint=\"{}\" captured=\"{}\" trigger=\"{}\">",
        s.checkpoint_id,
        rfc3339_utc(s.created_ms),
        s.trigger.as_str()
    ));
    o.plain("[CONTEXT]");
    o.plain(CONTEXT);

    match &s.root {
        Some(r) => {
            let src = format!("intents:{}", r.id);
            o.push(
                format!(
                    "[ROOT_TASK_OBJECTIVE] (OBSERVED | user prompt | {})",
                    hh_mm(r.ts_ms, tz)
                ),
                &src,
            );
            o.push(truncate_chars(&r.text, lim.root_chars), &src);
        }
        None => {
            o.push("[ROOT_TASK_OBJECTIVE]", "intents:none");
            o.push("(not captured)", "intents:none");
        }
    }
    if let Some(st) = s.subtask.as_ref().filter(|_| lim.subtask) {
        let src = format!("intents:{}", st.id);
        o.push(
            format!(
                "[ACTIVE_SUBTASK] (OBSERVED | subtask: prompt | {})",
                hh_mm(st.ts_ms, tz)
            ),
            &src,
        );
        o.push(truncate_chars(&st.text, lim.subtask_chars), &src);
    }
    if let Some(l) = s
        .latest
        .as_ref()
        .filter(|l| lim.latest && s.root.as_ref().is_none_or(|r| r.text != l.text))
    {
        let src = format!("intents:{}", l.id);
        o.push(
            format!(
                "[LATEST_REQUEST] (OBSERVED | user prompt | {})",
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
    o.push("[STATUS]", &status_src);
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

    if let Some(f) = &s.failure {
        let src = format!("commands:{}", f.id);
        o.push(
            format!(
                "[ACTIVE_FAILURE] (OBSERVED | {} run | {})",
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
        let n = f.excerpt.len();
        for line in &f.excerpt[n.saturating_sub(lim.failure_lines)..] {
            o.push(format!("  {line}"), &src);
        }
    }

    let dead_ends: Vec<&DeadEndView> = s.dead_ends.iter().take(lim.dead_ends_max).collect();
    if !dead_ends.is_empty() {
        let all: Vec<i64> = dead_ends.iter().map(|d| d.id).collect();
        o.push("[DEAD_ENDS] (OBSERVED)", &ids("dead_ends", &all));
        let n = dead_ends.len();
        for (i, d) in dead_ends.iter().enumerate() {
            let src = format!("dead_ends:{},{}", d.id, ids("edits", &d.edit_ids));
            o.push(
                format!(
                    "- {}{} | {} edit(s) | {} at {}",
                    path(&d.path),
                    if d.subagent { " (subagent)" } else { "" },
                    d.edit_ids.len(),
                    mechanism_text(d, lim.command_chars),
                    hh_mm(d.resolved_ms, tz)
                ),
                &src,
            );
            // Dead ends are listed most recent first; excerpts are removed oldest first.
            let excerpt_removed = n - i <= lim.dead_excerpts_removed;
            if !excerpt_removed {
                if let Some(m) = &d.minus {
                    o.push(format!("    - {m}"), &src);
                }
                if let Some(p) = &d.plus {
                    o.push(format!("    + {p}"), &src);
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

    let attempts: Vec<&AttemptView> = s.attempts.iter().take(lim.attempts_max).collect();
    if !attempts.is_empty() {
        let all: Vec<i64> = attempts.iter().map(|a| a.edit_id).collect();
        o.push("[RECENT_ATTEMPTS] (OBSERVED)", &ids("edits", &all));
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
            "[WORKING_FILES] (OBSERVED)",
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
            format!("[NEXT_KNOWN_TARGET] (INFERRED | {})", t.rule),
            &t.source,
        );
        o.push(path(&t.target), &t.source);
    }

    o.plain("[RECOVERY]");
    if s.preview {
        o.plain("Full detail for any section: `velra inspect --section <dead-ends|failure|files|attempts>`");
    } else {
        o.plain(format!(
            "Full detail for any section: `velra inspect --checkpoint {} --section <dead-ends|failure|files|attempts>`",
            s.checkpoint_id
        ));
    }
    o.plain("</VELRA_CONTINUATION>");
    o.lines.join("\n")
}

type Step = fn(&mut Limits, &Snapshot) -> bool;

/// Spec truncation order (§16.3). Each step returns false when it can no
/// longer change anything (so the loop moves on).
const SPEC_STEPS: &[Step] = &[
    |l, _| std::mem::replace(&mut l.working_max, 4) != 4,
    |l, _| std::mem::replace(&mut l.attempts_max, 2) != 2,
    |l, s| {
        // Remove one more dead end's excerpt lines, oldest first.
        if l.dead_excerpts_removed < s.dead_ends.len() {
            l.dead_excerpts_removed += 1;
            true
        } else {
            false
        }
    },
    |l, _| std::mem::replace(&mut l.failure_lines, 3) != 3,
    |l, _| std::mem::replace(&mut l.latest_chars, 120) != 120,
    |l, _| std::mem::replace(&mut l.root_chars, 160) != 160,
    |l, _| std::mem::replace(&mut l.working_max, 0) != 0,
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
const CEILING_STEPS: &[Step] = &[
    |l, _| std::mem::replace(&mut l.observed, false),
    |l, _| std::mem::replace(&mut l.next_target, false),
    |l, _| std::mem::replace(&mut l.attempts_max, 0) != 0,
    |l, _| std::mem::replace(&mut l.failure_lines, 0) != 0,
    |l, _| std::mem::replace(&mut l.subtask_chars, 80) != 80,
    |l, _| std::mem::replace(&mut l.path_chars, 60) != 60,
    |l, _| std::mem::replace(&mut l.command_chars, 80) != 80,
    |l, _| std::mem::replace(&mut l.latest, false),
    |l, _| std::mem::replace(&mut l.root_chars, 100) != 100,
    |l, _| std::mem::replace(&mut l.dead_ends_max, 2) != 2,
    |l, _| std::mem::replace(&mut l.subtask, false),
    |l, _| std::mem::replace(&mut l.dead_ends_max, 1) != 1,
    |l, _| std::mem::replace(&mut l.path_chars, 40) != 40,
    |l, _| std::mem::replace(&mut l.command_chars, 40) != 40,
    |l, _| std::mem::replace(&mut l.root_chars, 60) != 60,
];

/// Last-resort guarantee that the block never exceeds the ceiling (E2).
///
/// Every ladder step above is a judgement about which content matters least.
/// This is not a judgement — it is the backstop that makes "≤ 1,000 tokens and
/// ≤ 9,500 characters, always" a property rather than an expectation. It drops
/// whole lines from the end of the body, keeping the opening tag through
/// `[STATUS]` and the `[RECOVERY]` tail, so whatever survives is still a well
/// formed capsule that closes its own tag.
fn enforce_ceiling(text: String, ceiling: u32, max_chars: usize) -> String {
    let fits = |t: &str| estimate_tokens(t) <= ceiling && t.chars().count() <= max_chars;
    if fits(&text) {
        return text;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let recovery = lines
        .iter()
        .position(|l| l.starts_with("[RECOVERY]"))
        .unwrap_or(lines.len());
    // The protected head: the opening tag, [CONTEXT] and its paragraph, the
    // objective, and [STATUS] with its line.
    let head = lines
        .iter()
        .position(|l| l.starts_with("[STATUS]"))
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
    // Only reachable if the protected head alone is oversized, which needs a
    // pathological checkpoint id. Cut hard and close the tag.
    let mut out = truncate_chars(&candidate, max_chars.min(2_000)).into_owned();
    out.push_str("\n</VELRA_CONTINUATION>");
    out
}

fn run_steps(
    s: &Snapshot,
    lim: &mut Limits,
    steps: &[Step],
    target: u32,
    trace: bool,
    applied: &mut u32,
) -> String {
    let mut text = render_with(s, lim, trace);
    for step in steps {
        loop {
            if estimate_tokens(&text) <= target {
                return text;
            }
            if !step(lim, s) {
                break;
            }
            *applied += 1;
            text = render_with(s, lim, trace);
        }
    }
    text
}

fn render_impl(s: &Snapshot, cfg: &RenderConfig, trace: bool) -> Rendered {
    let target = cfg.budget_tokens.clamp(1, HARD_CEILING_TOKENS);
    let mut lim = Limits::FULL;
    let mut steps = 0;
    let mut text = run_steps(s, &mut lim, SPEC_STEPS, target, trace, &mut steps);
    if estimate_tokens(&text) > HARD_CEILING_TOKENS || text.chars().count() > ABSOLUTE_MAX_CHARS {
        text = run_steps(
            s,
            &mut lim,
            CEILING_STEPS,
            HARD_CEILING_TOKENS,
            trace,
            &mut steps,
        );
    }
    text = enforce_ceiling(text, HARD_CEILING_TOKENS, ABSOLUTE_MAX_CHARS);
    Rendered {
        tokens: estimate_tokens(&text),
        text,
        steps,
    }
}

/// Renders the capsule within budget (§16.3).
pub fn render(s: &Snapshot, cfg: &RenderConfig) -> Rendered {
    render_impl(s, cfg, false)
}

/// Debug render annotating each derived line with its source rows (E4).
pub fn render_traced(s: &Snapshot, cfg: &RenderConfig) -> Rendered {
    render_impl(s, cfg, true)
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
            subtask: None,
            latest: None,
            git: None,
            edit_count: 0,
            last_test: None,
            failure: None,
            failing_count: 0,
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
        let expected = "<VELRA_CONTINUATION v=\"1\" checkpoint=\"ckpt_01TEST\" captured=\"2026-09-12T10:04:05Z\" trigger=\"manual\">\n[CONTEXT]\n".to_string()
            + CONTEXT
            + "\n[ROOT_TASK_OBJECTIVE]\n(not captured)\n[STATUS]\nno git | 0 edits this task | last test run: none\n[RECOVERY]\nFull detail for any section: `velra inspect --checkpoint ckpt_01TEST --section <dead-ends|failure|files|attempts>`\n</VELRA_CONTINUATION>";
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
            .contains("[LATEST_REQUEST]"));
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
        assert!(r.text.contains("[DEAD_ENDS] (OBSERVED)"));
        assert!(r.text.contains("[ACTIVE_FAILURE]"));
        assert!(r.text.ends_with("</VELRA_CONTINUATION>"));
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
