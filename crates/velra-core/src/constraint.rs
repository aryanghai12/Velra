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

/// One sentence of a prompt, quoted, with the evidence that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The sentence exactly as the user wrote it, whitespace-collapsed.
    pub text: String,
    /// The literal cue that matched, lowercased.
    pub cue: &'static str,
    pub kind: ConstraintKind,
}

/// Most constraints per prompt. A prompt that trips the cue list a dozen times
/// is prose, not a specification, and carrying all of it would crowd the
/// capsule out of everything else it has to say.
pub const MAX_PER_PROMPT: usize = 3;

/// Sentences longer than this are not quoted: a cue buried in a paragraph is
/// not an isolable constraint, and truncating one to fit changes what it says.
pub const MAX_SENTENCE_CHARS: usize = 320;

/// Sentences shorter than this carry no information once quoted out of context.
const MIN_SENTENCE_CHARS: usize = 12;

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

/// Splits on sentence boundaries: `.`, `!` or `?` followed by whitespace or
/// end of input, and hard line breaks.
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
/// requirements so the strongest evidence wins.
fn cue_for(lower: &str) -> Option<(&'static str, ConstraintKind)> {
    for label in LABELS {
        if lower.contains(label) {
            return Some((label, ConstraintKind::Labelled));
        }
    }
    for cue in PROHIBITIONS {
        if lower.contains(cue) {
            return Some((cue, ConstraintKind::Prohibition));
        }
    }
    for cue in REQUIREMENTS {
        if lower.contains(cue) {
            return Some((cue, ConstraintKind::Requirement));
        }
    }
    None
}

fn is_time_scoped(lower: &str) -> bool {
    TIME_SCOPED.iter().any(|w| lower.contains(w))
}

/// Every constraint sentence of one prompt, in the order the user wrote them,
/// capped at [`MAX_PER_PROMPT`] and deduplicated on the quoted text.
///
/// Slash commands yield nothing: `/compact` is addressed to Claude Code, not to
/// the task.
pub fn extract(prompt: &str) -> Vec<Extracted> {
    let normalized = collapse_whitespace(prompt);
    if normalized.starts_with('/') {
        return Vec::new();
    }
    let mut out: Vec<Extracted> = Vec::new();
    for raw in sentences(&normalized) {
        if out.len() >= MAX_PER_PROMPT {
            break;
        }
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
        let Some((cue, kind)) = cue_for(&lower) else {
            continue;
        };
        if out.iter().any(|e| e.text == text) {
            continue;
        }
        out.push(Extracted {
            text: text.to_string(),
            cue,
            kind,
        });
    }
    out
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
        // The newline ends a sentence, so only the tail carries the cue-bearing
        // clause; what matters is that nothing is rewritten.
        assert!(got.iter().all(|e| !e.text.contains('\n')));
        assert!(got.iter().all(|e| !e.text.contains("  ")));
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
