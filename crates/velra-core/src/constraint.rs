//! Deterministic constraint extraction from user prompts.
//!
//! # What this is, and what it deliberately is not
//!
//! Claude Code's `UserPromptSubmit` hook delivers the user's prompt verbatim
//! (`tests/fixtures/claude-code/2.1.268/user_prompt_submit.json`), so the
//! sentences a user wrote are observable. Their *meaning* is not. This module
//! therefore does exactly one thing: it selects the sentences of a prompt that
//! contain an explicit requirement or prohibition cue, and hands them back
//! verbatim together with the cue that selected them.
//!
//! It does not paraphrase, summarise, rank by importance, decide whether a
//! constraint is still in force, or promote a preference to a rule. Every one
//! of those would be an assertion the event log does not support, and the
//! capsule would then be telling the next agent something nobody said. A
//! quoted sentence with its cue and its turn number is the whole claim.
//!
//! # Time-scoped sentences are not constraints
//!
//! "Do not change any code yet" is a real instruction and a real prohibition,
//! and it is also worthless — worse than worthless — fifteen turns and a
//! compaction later. A record that replays it after the boundary tells the next
//! agent not to do the thing it was just asked to do. So a sentence carrying a
//! time-scoping word (`yet`, `for now`, `first`, `right now`, `at this point`,
//! `before we`, `until`) is excluded. The rule is about the shape of the
//! sentence, applies to every prompt equally, and is tested on prose that has
//! nothing to do with any benchmark.
//!
//! # Other people's words, and code, are not the user's rules
//!
//! A cue inside a double-quoted span, a code span or text introduced as a
//! quotation (`The README says: …`) is someone else's rule being discussed,
//! and pasted material -- fenced code, test output, logs, diagnostics, JSON,
//! comments, quoted documentation (`crate::material`) -- is not the user
//! speaking at all. Neither is quoted as a constraint. The prompt is read
//! paragraph by paragraph and list item by list item, so a rule written one
//! per line without a full stop is still its own sentence.
//!
//! # Rejected approaches
//!
//! A user who asserts that a route is rejected (`X is a dead end`, `Rejected
//! approach: X`) has stated a constraint as surely as one who writes `do
//! not`. The bar is higher than for a rule, because a rejected approach is
//! replayed as a fact about the work: the label must be asserted (present
//! tense, not negated, not a question, not hedged or historical), and a
//! sentence that only points back (`That workaround is …`) is kept only when
//! exactly one of the sentences just before it names the same approach -- and
//! then quoted with it, verbatim and contiguous. Anything less stays in the
//! user's message as ordinary text; see [`rejection`].

use crate::text::collapse_whitespace;

/// Which cue class selected a sentence. Not a probability — a description of
/// the evidence, which is all the log can support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConstraintKind {
    /// The user labelled it: "constraint:", "invariant:", "hard rule", ….
    Labelled,
    /// A prohibition: "do not", "never", "must not", "without changing", ….
    Prohibition,
    /// A requirement: "must", "always", "has to", "preserve", ….
    Requirement,
}

impl ConstraintKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConstraintKind::Labelled => "labelled",
            ConstraintKind::Prohibition => "prohibition",
            ConstraintKind::Requirement => "requirement",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "labelled" => Some(ConstraintKind::Labelled),
            "prohibition" => Some(ConstraintKind::Prohibition),
            "requirement" => Some(ConstraintKind::Requirement),
            _ => None,
        }
    }
}

/// Why a sentence was selected: the evidence, recorded with the quote so a
/// reader can tell a rule the user labelled from one Velra joined to its
/// antecedent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// A label, prohibition or requirement cue outside quotes and pasted
    /// material.
    Cue,
    /// An asserted rejection label about a subject the sentence names itself.
    Rejection,
    /// An asserted rejection that points back (`That workaround …`), quoted
    /// from the one earlier sentence of its paragraph naming the same
    /// approach through the rejection itself.
    RejectionWithAntecedent,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::Cue => "cue",
            Basis::Rejection => "rejection",
            Basis::RejectionWithAntecedent => "rejection+antecedent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cue" => Some(Basis::Cue),
            "rejection" => Some(Basis::Rejection),
            "rejection+antecedent" => Some(Basis::RejectionWithAntecedent),
            _ => None,
        }
    }
}

/// One sentence of a prompt, quoted, with the evidence that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The sentence exactly as the user wrote it, whitespace-collapsed.
    pub text: String,
    /// The literal cue that matched, lowercased.
    pub cue: &'static str,
    pub kind: ConstraintKind,
    /// Byte offset, in the text given to [`extract`], of the paragraph or
    /// list item the quote was taken from.
    pub at: usize,
    pub basis: Basis,
}

/// Most rules (non-rejection constraints) per prompt. A prompt that trips the
/// cue list a dozen times is prose, not a specification, and carrying all of
/// it would crowd the capsule out of everything else it has to say.
pub const MAX_PER_PROMPT: usize = 3;

/// Most rejections per prompt, counted apart from the rules: a rejected
/// approach is stated alongside rules (the payment task has both), and a
/// third rule must not be what pushes it out.
pub const MAX_REJECTIONS_PER_PROMPT: usize = 2;

/// Sentences longer than this are not quoted: a cue buried in a paragraph is
/// not an isolable constraint, and truncating one to fit changes what it says.
pub const MAX_SENTENCE_CHARS: usize = 320;

/// Sentences shorter than this carry no information once quoted out of context.
const MIN_SENTENCE_CHARS: usize = 12;

/// Explicit rejection labels: the user naming a route as ruled out. Stored as
/// a labelled constraint -- the user did the work of saying so -- when the
/// sentence asserts it ([`rejection`]); a sentence that points back (`That
/// workaround is a rejected approach …`) is quoted with the sentence it points
/// back to, because on its own it does not say which route was rejected.
///
/// Before this, a rejection the user stated in prose existed only inside the
/// objective's text, and survived only while it happened to fall within the
/// objective excerpt the renderer prints.
const REJECTIONS: &[&str] = &[
    "rejected approach",
    "rejected workaround",
    "rejected fix",
    "rejected solution",
    "rejected route",
    "dead end",
    "dead-end",
];

/// Explicit labels. A user who writes one of these has done the work of saying
/// "this is a rule", so it outranks an inferred cue.
const LABELS: &[&str] = &[
    "constraint",
    "invariant",
    "non-negotiable",
    "nonnegotiable",
    "hard rule",
    "ground rule",
    "requirement:",
    "rule:",
];

/// Prohibition cues, longest-first so "must not" wins over "must".
const PROHIBITIONS: &[&str] = &[
    "without modifying",
    "without changing",
    "without touching",
    "must never",
    "should never",
    "no changes to",
    "must not",
    "cannot ",
    "can not ",
    "shouldn't",
    "should not",
    "don't ",
    "do not ",
    "never ",
    "can't ",
];

/// Requirement cues.
const REQUIREMENTS: &[&str] = &[
    "make sure",
    "has to ",
    "have to ",
    "needs to ",
    "must ",
    "always ",
    "preserve ",
    "maintain ",
    "keep ",
    "ensure ",
    "only use ",
    "stick to ",
];

/// Time-scoping words. See the module docs: a sentence containing one of these
/// describes this turn, not the task.
const TIME_SCOPED: &[&str] = &[
    " yet",
    "for now",
    "right now",
    "at this point",
    "before we",
    "until ",
    " first,",
    " first.",
    " first ",
    "this turn",
    "next step",
];

/// Splits one paragraph ([`segments`]) on sentence boundaries: `.`, `!` or `?`
/// followed by whitespace or end of input. (A newline inside a paragraph is a
/// hard wrap and has already been collapsed; one that ends a paragraph or a
/// list item ended the segment.)
///
/// The whitespace requirement is load-bearing. Prompts are full of dotted
/// identifiers — `engine.settle`, `invoice.items`, `src/ledger/money.py` — and
/// a splitter that breaks on every period shreds exactly the sentences worth
/// quoting.
fn sentences(prompt: &str) -> Vec<&str> {
    let bytes = prompt.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &c) in bytes.iter().enumerate() {
        let terminator = match c {
            b'\n' | b'\r' => true,
            b'.' | b'!' | b'?' => bytes
                .get(i + 1)
                .is_none_or(|n| n.is_ascii_whitespace() || *n == b'"' || *n == b'\''),
            _ => false,
        };
        if !terminator {
            continue;
        }
        let end = if c == b'\n' || c == b'\r' { i } else { i + 1 };
        if start < end {
            out.push(&prompt[start..end]);
        }
        start = i + 1;
    }
    if start < prompt.len() {
        out.push(&prompt[start..]);
    }
    out
}

/// The first matching cue in `lower`, searched labels → prohibitions →
/// requirements so the strongest evidence wins, and rejections last. An
/// occurrence inside a quoted or code span (`quoted[i]`) is not evidence: see
/// [`quoted_mask`].
///
/// Rejections come last so that they only ever claim a sentence no other cue
/// selects: `Don't use the global cache, it is a dead end` stays the
/// prohibition it always was, and what the rejection rule adds is the
/// sentences that used to be dropped.
///
/// One pass: every cue is a pattern of one automaton, numbered in precedence
/// order, and the lowest-numbered pattern with an unquoted occurrence wins --
/// the same answer as trying the lists in turn, which searched each sentence
/// once per cue and dominated the hook's cost on a long prompt.
fn cue_for(lower: &str, quoted: &[bool]) -> Option<(&'static str, ConstraintKind)> {
    use aho_corasick::{AhoCorasick, MatchKind};
    use std::sync::OnceLock;
    static CUES: OnceLock<(AhoCorasick, Vec<(&'static str, ConstraintKind)>)> = OnceLock::new();
    let (ac, table) = CUES.get_or_init(|| {
        let table: Vec<(&'static str, ConstraintKind)> = LABELS
            .iter()
            .map(|c| (*c, ConstraintKind::Labelled))
            .chain(
                PROHIBITIONS
                    .iter()
                    .map(|c| (*c, ConstraintKind::Prohibition)),
            )
            .chain(
                REQUIREMENTS
                    .iter()
                    .map(|c| (*c, ConstraintKind::Requirement)),
            )
            .chain(REJECTIONS.iter().map(|c| (*c, ConstraintKind::Labelled)))
            .collect();
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(table.iter().map(|(c, _)| *c))
            .expect("static cue patterns are valid");
        (ac, table)
    });
    ac.find_overlapping_iter(lower)
        .filter(|m| !quoted[m.start()])
        .map(|m| m.pattern().as_usize())
        .min()
        .map(|i| table[i])
}

/// Whether a stored constraint's cue is a rejection label: the user ruling a
/// route out rather than stating a rule for the work. The snapshot keeps these
/// apart (`Snapshot::rejections`).
pub fn is_rejection_cue(cue: &str) -> bool {
    REJECTIONS.contains(&cue)
}

fn is_time_scoped(lower: &str) -> bool {
    TIME_SCOPED.iter().any(|w| lower.contains(w))
}

/// Byte offset of `part` inside `whole`; `part` must be a subslice of it.
fn offset_in(whole: &str, part: &str) -> usize {
    part.as_ptr() as usize - whole.as_ptr() as usize
}

/// A list item's text without its marker (`- `, `* `, `+ `, `• `, `1. `,
/// `1) `), or `None` when `line` (leading whitespace removed) is not one.
fn list_item(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ ", "\u{2022} "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        for marker in [". ", ") "] {
            if let Some(rest) = line[digits..].strip_prefix(marker) {
                return Some(rest);
            }
        }
    }
    None
}

/// The paragraphs and list items of a prompt, each whitespace-collapsed and
/// paired with the byte offset where it starts, with pasted material left
/// out.
///
/// Whitespace used to be collapsed over the whole prompt before splitting, so
/// a line break never ended anything: a list of rules written one per line
/// without full stops came out as one "sentence" beginning `Rules: - never
/// change …`, and past 320 characters it was not quoted at all. A line break
/// is still not a boundary on its own -- people hard-wrap sentences, and
/// cutting `Do not\nchange the public API` at the wrap would quote half a
/// rule. It ends a segment only where the text says so: a blank line, or a
/// new line that starts a list item.
///
/// Material (`crate::material`) -- fenced code, pasted output, logs, code,
/// blockquotes, text introduced as a quotation -- is not prose, and a line of
/// it ends the segment it interrupts. A `# must not be None` comment in a
/// pasted snippet is not a rule the user stated.
fn segments(prompt: &str, kinds: &[crate::material::Line]) -> Vec<(usize, String)> {
    fn flush(cur: &mut String, at: &mut Option<usize>, out: &mut Vec<(usize, String)>) {
        // Too short to hold a sentence worth quoting: not collapsed, not kept.
        if cur.trim().len() >= MIN_SENTENCE_CHARS {
            let collapsed = collapse_whitespace(cur);
            if let (false, Some(a)) = (collapsed.is_empty(), *at) {
                out.push((a, collapsed));
            }
        }
        cur.clear();
        *at = None;
    }
    let kinds = kinds.iter().copied();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut at: Option<usize> = None;
    for (line, kind) in prompt.lines().zip(kinds) {
        if kind != crate::material::Line::Prose {
            flush(&mut cur, &mut at, &mut out);
            continue;
        }
        let t = line.trim_start();
        if let Some(item) = list_item(t) {
            flush(&mut cur, &mut at, &mut out);
            at = Some(offset_in(prompt, item));
            cur.push_str(item);
        } else {
            at.get_or_insert(offset_in(prompt, t));
            cur.push_str(line);
        }
        cur.push(' ');
    }
    flush(&mut cur, &mut at, &mut out);
    out
}

/// Byte offset just past the end of the sentence containing byte `from` of
/// `s` (a collapsed segment): the first `.`, `!` or `?` at or after it that
/// [`sentences`] would end a sentence on, or the end of `s`.
fn sentence_end(s: &str, from: usize) -> usize {
    let b = s.as_bytes();
    for i in from..b.len() {
        if matches!(b[i], b'.' | b'!' | b'?')
            && b.get(i + 1)
                .is_none_or(|n| n.is_ascii_whitespace() || *n == b'"' || *n == b'\'')
        {
            return i + 1;
        }
    }
    b.len()
}

/// Verbs that introduce someone else's words on the same line: `The README
/// says: never call the API directly.`
const QUOTING_VERBS: &[&str] = &[
    "says:", "said:", "say:", "reads:", "states:", "stated:", "wrote:", "writes:", "warns:",
    "warned:",
];

/// A byte mask over a collapsed segment `s`: `true` inside a double-quoted
/// span (`"…"` or `“…”`), an inline code span (`` `…` ``), or a quotation a
/// verb introduces (`says: …`, to the end of its sentence).
///
/// A cue there is not the user's rule. `The README says "Never call the API
/// directly." Is that still true?` is a question about a rule somebody else
/// wrote, and the old extractor, which also stripped the quote marks from the
/// sentence it kept, recorded it as the user's own prohibition. Single quotes
/// are not spans -- they are apostrophes far more often (`don't`).
///
/// A straight `"` is punctuation as often as it is a quote: an inch mark
/// (`a 5" pipe`, `12" x 8"`), an escaped quote (`\"`), a stray mark. So a `"`
/// opens a span only where a quotation can start -- at the start, or after
/// whitespace or an opening bracket, and before a non-space -- and it closes
/// at the next `"` that ends a word. A mark that opens nothing, or an opening
/// with no close before the next opening, masks at most the rest of its own
/// sentence: a punctuation mark never hides the rest of a paragraph, and
/// until the text says whose words those are, that one sentence is not
/// promoted to a rule.
fn quoted_mask(s: &str) -> Vec<bool> {
    let b = s.as_bytes();
    let mut mask = vec![false; s.len()];
    let fill = |mask: &mut Vec<bool>, from: usize, to: usize| {
        for m in &mut mask[from..to.min(b.len())] {
            *m = true;
        }
    };
    let escaped = |i: usize| i > 0 && b[i - 1] == b'\\';
    let opens = |i: usize| {
        !escaped(i)
            && (i == 0 || b[i - 1].is_ascii_whitespace() || b"([{=:,".contains(&b[i - 1]))
            && b.get(i + 1).is_some_and(|c| !c.is_ascii_whitespace())
    };
    let closes = |i: usize| !escaped(i) && i > 0 && !b[i - 1].is_ascii_whitespace();
    const OPEN_CURLY: &str = "\u{201c}";
    const CLOSE_CURLY: &str = "\u{201d}";
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let end = s[i + 1..]
                .find('`')
                .map(|j| i + 1 + j + 1)
                .unwrap_or_else(|| sentence_end(s, i));
            fill(&mut mask, i, end);
            i = end;
            continue;
        }
        if b[i] == b'"' && opens(i) {
            let mut end = None;
            let mut j = i + 1;
            while j < b.len() {
                if b[j] == b'"' {
                    if closes(j) {
                        end = Some(j + 1);
                    }
                    // An opening before any close: this one never closed.
                    break;
                }
                j += 1;
            }
            let end = end.unwrap_or_else(|| sentence_end(s, i));
            fill(&mut mask, i, end);
            i = end;
            continue;
        }
        // Byte comparisons: `i` and `j` step one byte at a time and are not
        // always char boundaries, and slicing a `str` there panics -- which
        // on the hook path means the prompt is silently not stored.
        if b[i..].starts_with(OPEN_CURLY.as_bytes()) {
            let mut depth = 0usize;
            let mut j = i;
            let mut end = None;
            while j < b.len() {
                if b[j..].starts_with(OPEN_CURLY.as_bytes()) {
                    depth += 1;
                    j += OPEN_CURLY.len();
                } else if b[j..].starts_with(CLOSE_CURLY.as_bytes()) {
                    depth -= 1;
                    j += CLOSE_CURLY.len();
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            let end = end.unwrap_or_else(|| sentence_end(s, i));
            fill(&mut mask, i, end);
            i = end;
            continue;
        }
        i += 1;
    }
    let lower = s.to_ascii_lowercase();
    for verb in QUOTING_VERBS {
        for (at, _) in lower.match_indices(verb) {
            let word_start = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
            if word_start {
                let from = at + verb.len();
                fill(&mut mask, from, sentence_end(s, from));
            }
        }
    }
    mask
}

/// Sentences searched backwards for the one a rejection points back to.
const ANTECEDENT_SCAN: usize = 3;

/// Words that open a subject by pointing back at something said earlier.
const POINTERS: &[&str] = &["that", "this", "these", "those", "such", "it"];

/// Approach nouns a pointing subject can name.
const APPROACH_NOUNS: &[&str] = &[
    "workaround",
    "approach",
    "fix",
    "change",
    "attempt",
    "route",
    "idea",
    "hack",
    "solution",
    "patch",
    "edit",
];

/// Words that make a statement hedged, conditional or historical rather than
/// a decision about this work: `if that fails, it is a dead end`, `it might
/// be a dead end`, `it was a dead end back then`.
const HEDGES: &[&str] = &[
    "if",
    "whether",
    "unless",
    "might",
    "may",
    "could",
    "would",
    "maybe",
    "perhaps",
    "possibly",
    "probably",
    "suppose",
    "supposing",
    "assuming",
    "previously",
    "historically",
    "originally",
    "formerly",
];

/// Multi-word hedges.
const HEDGE_PHRASES: &[&str] = &[
    "in case",
    "last year",
    "last time",
    "back then",
    "in the past",
    "years ago",
    "at the time",
    "used to",
];

/// Words skipped between a rejection label and the verb that asserts it:
/// `is considered a rejected approach`, `is definitely a dead end`.
const FILLERS: &[&str] = &[
    "a",
    "an",
    "the",
    "considered",
    "definitely",
    "clearly",
    "now",
    "also",
    "already",
    "really",
    "basically",
    "simply",
    "just",
    "officially",
    "our",
    "my",
    "another",
];

fn words(lower: &str) -> Vec<&str> {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect()
}

/// The approach noun a pointing subject names (`That workaround …` gives
/// `workaround`), within the three words after the pointer, or `None`.
fn pointed_noun(subject: &[&str]) -> Option<&'static str> {
    subject.iter().skip(1).take(3).find_map(|w| {
        APPROACH_NOUNS
            .iter()
            .copied()
            .find(|n| *w == *n || w.strip_suffix('s') == Some(n))
    })
}

/// Whether `noun` (or its plural) occurs as a word of `s` outside the masked
/// bytes.
fn names_unmasked(s: &str, mask: &[bool], noun: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let w = &lower[start..i];
        if (w == noun || w.strip_suffix('s') == Some(noun)) && !mask[start] {
            return true;
        }
    }
    false
}

/// Decides whether the sentence `parts[k]` of a segment, which carries the
/// rejection label `cue`, is a rejection the user asserted, and what to
/// quote: `None` to leave it as ordinary text.
///
/// The label must be **asserted**:
///
/// * not a question, not hedged, conditional or historical ([`HEDGES`]);
/// * not negated in the words before it (`is not a dead end`);
/// * either the label opens the sentence as a heading (`Rejected approach:
///   caching the key in a global`), or it is predicated by `is` / `are` /
///   `'s` / `as` (`X is considered a rejected approach`, `treat it as a dead
///   end`). `X was a dead end` is history, not a decision.
///
/// The subject must be **identified**. A subject that names itself (`Caching
/// the key in a global is a dead end`) is quoted alone. A subject that points
/// back (`That workaround …`, `It …`) names nothing on its own: it is kept
/// only when it names an approach noun and exactly one of the
/// [`ANTECEDENT_SCAN`] sentences before it in the same paragraph uses that
/// noun outside quotes and code, and is not itself a question. The quote is
/// then that sentence through the rejection, verbatim and contiguous, within
/// [`MAX_SENTENCE_CHARS`]. No antecedent, two candidates, a bare pronoun, a
/// quote too long to keep whole: the sentence stays in the user's message,
/// where it was, and no rejected approach is recorded. A rejection Velra
/// cannot attribute is not one it may assert.
fn rejection(
    seg: &str,
    mask: &[bool],
    parts: &[&str],
    k: usize,
    text: &str,
    cue: &str,
) -> Option<(String, Basis)> {
    let lower = text.to_ascii_lowercase();
    let at = offset_in(seg, text);
    let label_at = lower
        .match_indices(cue)
        .map(|(i, _)| i)
        .find(|&i| !mask[at + i])?;
    if lower.trim_end().ends_with('?') {
        return None;
    }
    let all = words(&lower);
    if all.iter().any(|w| HEDGES.contains(w)) || HEDGE_PHRASES.iter().any(|p| lower.contains(p)) {
        return None;
    }
    let before = words(&lower[..label_at]);
    let negated = before
        .iter()
        .rev()
        .take(5)
        .any(|w| matches!(*w, "not" | "no" | "never" | "longer") || w.ends_with("n't"));
    if negated {
        return None;
    }

    // A heading: `Rejected approach: <subject>`.
    if before.is_empty() {
        let rest = lower[label_at + cue.len()..].trim_start();
        let subject = words(rest.strip_prefix(':')?);
        if subject.len() < 2 || POINTERS.contains(&subject[0]) {
            return None;
        }
        return Some((text.to_string(), Basis::Rejection));
    }

    // A predicate: `<subject> is (considered) a <label>`.
    let mut idx = before.len();
    while idx > 0 && FILLERS.contains(&before[idx - 1]) {
        idx -= 1;
    }
    let verb = *before.get(idx.checked_sub(1)?)?;
    let asserted = matches!(verb, "is" | "are" | "as") || verb.ends_with("'s");
    if !asserted {
        return None;
    }
    let subject: Vec<&str> = if let Some(stem) = verb.strip_suffix("'s") {
        let mut s = before[..idx - 1].to_vec();
        s.push(stem);
        s
    } else {
        before[..idx - 1].to_vec()
    };
    // Where the subject points back, if it does: its first word (`That
    // workaround is …`), or the object of `treat it as` / `consider that
    // workaround as`.
    let pointer = if subject.first().is_some_and(|w| POINTERS.contains(w))
        || lower.starts_with("the above ")
    {
        Some(0)
    } else if verb == "as" && subject.get(1).is_some_and(|w| POINTERS.contains(w)) {
        Some(1)
    } else {
        None
    };
    let Some(p) = pointer else {
        return (!subject.is_empty()).then(|| (text.to_string(), Basis::Rejection));
    };
    let noun = pointed_noun(&subject[p..])?;
    let candidates: Vec<usize> = (k.saturating_sub(ANTECEDENT_SCAN)..k)
        .filter(|&j| {
            let p = parts[j];
            let from = offset_in(seg, p);
            !p.trim_end().ends_with('?') && names_unmasked(p, &mask[from..from + p.len()], noun)
        })
        .collect();
    let [j] = candidates[..] else {
        return None;
    };
    let from = offset_in(seg, parts[j]);
    let joined = seg[from..at + text.len()]
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'')
        .trim();
    (joined.chars().count() <= MAX_SENTENCE_CHARS)
        .then(|| (joined.to_string(), Basis::RejectionWithAntecedent))
}

/// Every constraint sentence of one prompt, in the order the user wrote them,
/// at most [`MAX_PER_PROMPT`] rules and [`MAX_REJECTIONS_PER_PROMPT`]
/// rejections, deduplicated on the quoted text.
///
/// `prompt` is the user's own text (`crate::prompt::authored`). Slash commands
/// yield nothing: `/compact` is addressed to Claude Code, not to the task.
///
/// Every returned text is a contiguous run of the whitespace-collapsed prompt,
/// never a paraphrase; a list item is quoted without its bullet.
pub fn extract(prompt: &str) -> Vec<Extracted> {
    extract_counted(prompt).0
}

/// [`extract`], and how many sentences qualified in all -- more than were
/// returned when a cap was reached. The count is what lets a record say that
/// a prompt held more rules than were kept, instead of holding fewer silently.
pub fn extract_counted(prompt: &str) -> (Vec<Extracted>, usize) {
    if opens_with_slash_command(prompt) {
        return (Vec::new(), 0);
    }
    extract_part(prompt)
}

/// Whether the prompt is a slash command. The command name and the word
/// after it are all the test reads, so only the opening is collapsed.
pub fn opens_with_slash_command(prompt: &str) -> bool {
    let opening = crate::text::prefix_bytes(prompt, 1024);
    crate::intent::is_slash_command(&collapse_whitespace(opening))
}

/// [`extract_counted`] over a later part of a prompt -- the tail that
/// `crate::prompt::for_storage` examines past its scan bound -- where a
/// leading `/` is not the start of the message and says nothing about a
/// command.
pub fn extract_part(prompt: &str) -> (Vec<Extracted>, usize) {
    extract_classified(prompt, &crate::material::classify(prompt))
}

/// [`extract_part`] with the lines of `prompt` already classified
/// (`crate::material::classify`), for a caller that needs the classification
/// itself too and must not pay for it twice on the hook path.
pub fn extract_classified(
    prompt: &str,
    kinds: &[crate::material::Line],
) -> (Vec<Extracted>, usize) {
    let mut out: Vec<Extracted> = Vec::new();
    let (mut rules, mut rejections, mut found) = (0usize, 0usize, 0usize);
    for (seg_at, seg) in segments(prompt, kinds) {
        let mask = quoted_mask(&seg);
        let parts = sentences(&seg);
        for (k, raw) in parts.iter().enumerate() {
            let text = raw.trim().trim_matches(|c: char| c == '"' || c == '\'');
            let text = text.trim();
            let chars = text.chars().count();
            if !(MIN_SENTENCE_CHARS..=MAX_SENTENCE_CHARS).contains(&chars) {
                continue;
            }
            let lower = text.to_ascii_lowercase();
            if is_time_scoped(&lower) {
                continue;
            }
            let at = offset_in(&seg, text);
            let Some((cue, kind)) = cue_for(&lower, &mask[at..at + text.len()]) else {
                continue;
            };
            let is_rejection = is_rejection_cue(cue);
            let (quoted, basis) = if is_rejection {
                match rejection(&seg, &mask, &parts, k, text, cue) {
                    Some(r) => r,
                    None => continue,
                }
            } else {
                (text.to_string(), Basis::Cue)
            };
            if out.iter().any(|e| e.text == quoted) {
                continue;
            }
            found += 1;
            let room = if is_rejection {
                rejections < MAX_REJECTIONS_PER_PROMPT
            } else {
                rules < MAX_PER_PROMPT
            };
            if !room {
                continue;
            }
            if is_rejection {
                rejections += 1;
            } else {
                rules += 1;
            }
            out.push(Extracted {
                text: quoted,
                cue,
                kind,
                at: seg_at,
                basis,
            });
        }
    }
    (out, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(prompt: &str) -> Vec<String> {
        extract(prompt).into_iter().map(|e| e.text).collect()
    }

    #[test]
    fn dotted_identifiers_do_not_split_a_sentence() {
        assert_eq!(
            sentences("engine.settle reads invoice.items from src/a.py. Next."),
            vec!["engine.settle reads invoice.items from src/a.py.", " Next."]
        );
    }

    #[test]
    fn a_labelled_rule_outranks_a_bare_cue() {
        let got = extract("Invariant for this work: the public API must stay as it is.");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, ConstraintKind::Labelled);
        assert_eq!(got[0].cue, "invariant");
        assert_eq!(
            got[0].text,
            "Invariant for this work: the public API must stay as it is."
        );
    }

    #[test]
    fn prohibitions_outrank_requirements_in_the_same_sentence() {
        let got =
            extract("You must not touch the database schema; always add a migration instead.");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, ConstraintKind::Prohibition);
        assert_eq!(got[0].cue, "must not");
    }

    #[test]
    fn plain_requirements_are_captured_verbatim() {
        let got = extract("Please keep the retry loop in place while you refactor.");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, ConstraintKind::Requirement);
        assert_eq!(
            got[0].text,
            "Please keep the retry loop in place while you refactor."
        );
    }

    #[test]
    fn sentences_without_a_cue_are_left_alone() {
        assert!(
            extract("Run the test suite and tell me which assertion fails and on which line.")
                .is_empty()
        );
    }

    #[test]
    fn time_scoped_instructions_are_not_constraints() {
        // Each of these is a real instruction about this turn and a lie about
        // the task once fifteen turns have passed.
        for prompt in [
            "Do not change any code yet.",
            "Read the module first, we will edit it afterwards.",
            "For now, do not run the migration.",
            "Don't touch the parser right now.",
            "Let's not edit anything until the audit is done.",
        ] {
            assert!(
                extract(prompt).is_empty(),
                "captured a time-scoped sentence: {prompt}"
            );
        }
    }

    #[test]
    fn a_durable_prohibition_beside_a_transient_one_survives_alone() {
        let got = texts(
            "Do not modify the public API at any point in this task. \
             Do not change any code yet.",
        );
        assert_eq!(
            got,
            vec!["Do not modify the public API at any point in this task."]
        );
    }

    #[test]
    fn slash_commands_yield_nothing() {
        assert!(extract("/compact").is_empty());
        assert!(extract("/clear keep the schema").is_empty());
    }

    #[test]
    fn capped_and_deduplicated() {
        let prompt = "Never delete the audit log. Always write a migration. \
                      You must keep the CLI flags. Do not rename the package. \
                      Never delete the audit log.";
        let got = texts(prompt);
        assert_eq!(got.len(), MAX_PER_PROMPT);
        assert_eq!(got[0], "Never delete the audit log.");
        assert_eq!(got[1], "Always write a migration.");
        assert_eq!(got[2], "You must keep the CLI flags.");
    }

    #[test]
    fn a_cue_buried_in_a_paragraph_is_not_quoted() {
        let long = format!("We must {} finish this.", "think about it ".repeat(40));
        assert!(long.chars().count() > MAX_SENTENCE_CHARS);
        assert!(extract(&long).is_empty());
    }

    #[test]
    fn very_short_sentences_are_ignored() {
        assert!(extract("Never.").is_empty());
        assert!(extract("Must fix.").is_empty());
    }

    #[test]
    fn whitespace_is_collapsed_but_words_are_not_changed() {
        let got = extract("Always   keep\n\tthe\n  loop.");
        // A hard-wrapped sentence is one sentence; nothing is rewritten.
        assert_eq!(
            texts("Always   keep\n\tthe\n  loop."),
            vec!["Always keep the loop."]
        );
        assert!(got.iter().all(|e| !e.text.contains('\n')));
        assert!(got.iter().all(|e| !e.text.contains("  ")));
    }

    #[test]
    fn list_items_and_paragraphs_are_separate_sentences() {
        let prompt = "Migrate the loader to TOML.\n\nRules:\n- never change the public API of the \
                      loader\n* keep the old JSON reader working\n2) do not add new dependencies";
        assert_eq!(
            texts(prompt),
            vec![
                "never change the public API of the loader",
                "keep the old JSON reader working",
                "do not add new dependencies",
            ]
        );
        // A paragraph break ends a sentence that has no full stop.
        assert_eq!(
            texts("Never delete the audit log\n\nThe rest is up to you"),
            vec!["Never delete the audit log"]
        );
    }

    #[test]
    fn a_cue_inside_quotes_or_code_is_not_the_users_rule() {
        for prompt in [
            "The README says \"Never call the payments API directly.\" Is that still true?",
            "\u{201c}Do not retry on a 409,\u{201d} the old runbook said. Check retry.py.",
            "The docstring says `must always be positive` but it accepts -1.",
            "This check is wrong:\n```python\n# must not be None\nassert x, \"must never be empty\"\n```\nWhy?",
            "An unclosed fence:\n```\n# never do this\nand the rest never gets quoted either",
        ] {
            assert!(extract(prompt).is_empty(), "{prompt}: {:?}", texts(prompt));
        }
        // The same cue outside the quotes still counts, and the sentence is
        // quoted whole, marks included.
        assert_eq!(
            texts("Do not rename the \"legacy\" flag in the CLI parser."),
            vec!["Do not rename the \"legacy\" flag in the CLI parser."]
        );
    }

    #[test]
    fn a_rejection_that_points_back_is_quoted_with_what_it_rejects() {
        let prompt = "Try a workaround that caches the key in a module-level global. That \
                      workaround is a rejected approach for this task. Do not modify the tests.";
        let got = extract(prompt);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].cue, "rejected approach");
        assert!(is_rejection_cue(got[0].cue));
        assert!(!is_rejection_cue(got[1].cue));
        assert_eq!(got[0].kind, ConstraintKind::Labelled);
        assert_eq!(
            got[0].text,
            "Try a workaround that caches the key in a module-level global. That workaround is \
             a rejected approach for this task."
        );
        assert!(collapse_whitespace(prompt).contains(&got[0].text));
        assert_eq!(got[1].text, "Do not modify the tests.");

        // The antecedent is the sentence that uses the same noun, not merely
        // the previous one.
        let got = texts(
            "Try a workaround that caches the key in a global. Run the tests. That workaround \
             is a rejected approach.",
        );
        assert_eq!(
            got,
            vec![
                "Try a workaround that caches the key in a global. Run the tests. That \
                 workaround is a rejected approach."
            ]
        );
        // No earlier sentence uses the noun: the rejection names nothing, so
        // nothing is recorded and nothing is guessed. It stays in the
        // message as the user wrote it.
        assert!(
            texts("Cache the key in a global. Run the tests. That approach is a dead end.")
                .is_empty()
        );
        // A sentence another cue already selects keeps that cue: the
        // rejection rule only adds sentences that used to be dropped.
        let got = extract("Don't use the global cache, that is a dead end for us.");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, ConstraintKind::Prohibition);
        assert_eq!(got[0].cue, "don't ");
        // A rejection that names its own route is quoted alone.
        assert_eq!(
            texts("Caching the key in a global is a dead end here. Do it properly."),
            vec!["Caching the key in a global is a dead end here."]
        );
        // The first sentence has nothing before it to point back to.
        assert!(texts("That approach is a dead end, as we saw yesterday.").is_empty());
        // Too long together: neither cut nor quoted without what it rejects.
        let long = format!(
            "Try a workaround that {} in a global. That workaround is a rejected approach.",
            "keeps extending the cache and the lookup table ".repeat(6)
        );
        assert!(texts(&long).is_empty());
    }

    #[test]
    fn a_rejection_is_recorded_only_when_asserted_and_attributable() {
        let basis = |p: &str| extract(p).into_iter().map(|e| e.basis).collect::<Vec<_>>();
        // Asserted about a subject it names: alone.
        assert_eq!(
            basis("Caching the key in a module global is a dead end here."),
            vec![Basis::Rejection]
        );
        assert_eq!(
            basis("Rejected approach: caching the key in a module global."),
            vec![Basis::Rejection]
        );
        assert_eq!(
            basis("Try a workaround that caches the key. Treat that workaround as a dead end."),
            vec![Basis::RejectionWithAntecedent]
        );
        for p in [
            // Negated, questioned, hedged, historical, past tense.
            "Caching the key in a global is not a dead end.",
            "Caching the key in a global isn't a rejected approach.",
            "Is caching the key in a global a dead end?",
            "Caching the key in a global might be a dead end.",
            "If it breaks batching, caching the key in a global is a dead end.",
            "Caching the key in a global was a dead end back then.",
            "Caching the key in a global was a dead end.",
            // A heading that points back, or names nothing.
            "Rejected approach: that one.",
            "Rejected approach:",
            // Bare pronouns.
            "Try a global cache. That's a dead end.",
            "Try a global cache. It is a rejected approach.",
            // The label only inside quotes.
            "The old ticket called it \"a dead end\" and moved on to other work.",
        ] {
            assert!(extract(p).is_empty(), "{p}: {:?}", texts(p));
        }
    }

    #[test]
    fn the_position_of_a_quote_is_its_paragraph() {
        let prompt = "Fix the retry.\n\nRules:\n- never change the public API\n\nDo not \
                      touch tests.";
        let got = extract(prompt);
        assert_eq!(got.len(), 2);
        assert_eq!(&prompt[got[0].at..got[0].at + 5], "never");
        assert_eq!(&prompt[got[1].at..got[1].at + 6], "Do not");
        assert!(got.iter().all(|e| e.basis == Basis::Cue));
    }

    #[test]
    fn rules_and_rejections_have_separate_caps_and_the_overflow_is_counted() {
        let prompt = "Never delete the audit log. Always write a migration. You must keep the \
                      CLI flags. Do not rename the package. Caching the key in a global is a \
                      dead end.";
        let (got, found) = extract_counted(prompt);
        assert_eq!(got.len(), 4, "{got:?}");
        assert_eq!(found, 5);
        assert!(is_rejection_cue(got[3].cue));
    }

    #[test]
    fn quote_marks_that_are_punctuation_hide_nothing() {
        for (prompt, want) in [
            (
                "It is a 5\" pipe. Do not change the flange spec.",
                "Do not change the flange spec.",
            ),
            (
                "Crop to 12\" x 8\" first thing. Never upscale past the source.",
                "Never upscale past the source.",
            ),
            (
                "He wrote \"retry later and left. Do not retry on a 409 here.",
                "Do not retry on a 409 here.",
            ),
            (
                "Escape it as \\\" in JSON. Never emit a raw quote in the log.",
                "Never emit a raw quote in the log.",
            ),
            (
                "A stray ` mark. Never emit a raw quote in the log.",
                "Never emit a raw quote in the log.",
            ),
            (
                "\u{201c}open and never closed. Do not rename the package.",
                "Do not rename the package.",
            ),
        ] {
            assert_eq!(texts(prompt), vec![want], "{prompt}");
        }
        // An opening that never closes still hides the rest of its own
        // sentence: those may be someone else's words.
        assert!(texts("He wrote \"never retry on a 409 and left.").is_empty());
        // Two quotes: the text between them is not inside either.
        assert_eq!(
            texts("Set \"a\" and never rename the \"b\" flag in the parser."),
            vec!["Set \"a\" and never rename the \"b\" flag in the parser."]
        );
        // A quotation introduced by a verb on the same line.
        assert!(texts("The README says: never call the payments API directly.").is_empty());
    }

    proptest::proptest! {
        /// Fail-open: whatever the text, extraction never panics, every quote
        /// is a contiguous run of the collapsed prompt, and the caps hold.
        /// The alphabet is the one that broke it: multi-byte characters next
        /// to straight, curly and back quotes.
        #[test]
        fn extraction_never_panics_and_quotes_verbatim(
            raw in "(\u{201c}|\u{201d}|\"|`|\\\\|'|\u{e9}|\u{1f4b3}|\u{8cc7}|\n|\n\n|\\. |\\? |do not |must |never |that workaround |is a dead end|try a workaround |[a-z]{1,6} |- |> |E   |logs:\n){0,40}"
        ) {
            let (got, found) = extract_counted(&raw);
            let collapsed = collapse_whitespace(&raw);
            proptest::prop_assert!(found >= got.len());
            let rules = got.iter().filter(|e| !is_rejection_cue(e.cue)).count();
            proptest::prop_assert!(rules <= MAX_PER_PROMPT);
            proptest::prop_assert!(got.len() - rules <= MAX_REJECTIONS_PER_PROMPT);
            for e in &got {
                proptest::prop_assert!(collapsed.contains(&e.text), "{:?}", e.text);
                proptest::prop_assert!(e.at <= raw.len());
            }
        }
    }

    #[test]
    fn multibyte_text_beside_quote_marks_does_not_panic() {
        for prompt in [
            "\u{e9}\u{1f4b3} \u{201c}never\u{201d} \u{e9}",
            "\u{8cc7}\u{201c}\u{e9}",
            "\u{e9}\"\u{1f4b3}\" do not \u{201d}\u{201c}",
            "`\u{e9}` must \u{1f4b3}` never",
        ] {
            let _ = extract(prompt);
        }
    }

    #[test]
    fn pasted_material_yields_no_rules_and_prose_around_it_still_does() {
        let prompt = "Here is the output:\nE   AssertionError: amount must be positive\n\
                      tests/test_retry.py:40: AssertionError\n\nPlease fix it. Do not modify \
                      the tests.";
        assert_eq!(texts(prompt), vec!["Do not modify the tests."]);
        let prompt = "This check is wrong:\n    # the value must not be None\n    if value is \
                      None:\nWhy?";
        assert!(texts(prompt).is_empty(), "{:?}", texts(prompt));
    }

    #[test]
    fn paths_that_begin_with_a_slash_still_yield_their_rules() {
        assert_eq!(
            texts("/src/api/routes.py must not change its public signature."),
            vec!["/src/api/routes.py must not change its public signature."]
        );
    }

    #[test]
    fn extraction_is_a_pure_function_of_the_prompt() {
        let prompt = "Constraint: preserve backward compatibility for v1 clients.";
        assert_eq!(extract(prompt), extract(prompt));
    }

    #[test]
    fn kind_strings_round_trip() {
        for k in [
            ConstraintKind::Labelled,
            ConstraintKind::Prohibition,
            ConstraintKind::Requirement,
        ] {
            assert_eq!(ConstraintKind::parse(k.as_str()), Some(k));
        }
    }
}
