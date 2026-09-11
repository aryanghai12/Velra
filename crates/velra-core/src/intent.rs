//! Intent hierarchy rules (§12), as pure functions.

use crate::text::collapse_whitespace;

/// What a `UserPromptSubmit` does to the intent state of its epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentAction {
    /// Slash command or too short: no intent change.
    None,
    /// `task:` prefix: start a new epoch; ROOT = remainder (if non-empty).
    NewEpoch { root: Option<String> },
    /// `subtask:` prefix: replace SUBTASK.
    Subtask(String),
    /// First substantial prompt of an epoch without a live ROOT.
    Root(String),
    /// Any other prompt of at least 3 chars.
    Latest(String),
}

/// Minimum length (chars) for a prompt to become ROOT implicitly.
pub const ROOT_MIN_CHARS: usize = 20;
/// Minimum length (chars) for a prompt to replace LATEST.
pub const LATEST_MIN_CHARS: usize = 3;

/// Rule 1: trim and collapse whitespace.
pub fn normalize_prompt(prompt: &str) -> String {
    collapse_whitespace(prompt)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Applies rules 2–6 to an already-normalized prompt.
pub fn classify_prompt(normalized: &str, has_live_root: bool) -> IntentAction {
    if normalized.starts_with('/') {
        return IntentAction::None;
    }
    if let Some(rest) = strip_prefix_ci(normalized, "task:") {
        let rest = rest.trim();
        return IntentAction::NewEpoch {
            root: (!rest.is_empty()).then(|| rest.to_string()),
        };
    }
    if let Some(rest) = strip_prefix_ci(normalized, "subtask:") {
        let rest = rest.trim();
        if !rest.is_empty() {
            return IntentAction::Subtask(rest.to_string());
        }
        return IntentAction::None;
    }
    let chars = normalized.chars().count();
    if !has_live_root && chars >= ROOT_MIN_CHARS {
        return IntentAction::Root(normalized.to_string());
    }
    if chars >= LATEST_MIN_CHARS {
        return IntentAction::Latest(normalized.to_string());
    }
    IntentAction::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let n = normalize_prompt("  fix   the\nflaky   login test please ");
        assert_eq!(n, "fix the flaky login test please");
        assert_eq!(classify_prompt(&n, false), IntentAction::Root(n.clone()));
        assert_eq!(
            classify_prompt("why did that fail?", true),
            IntentAction::Latest("why did that fail?".into())
        );
        assert_eq!(
            classify_prompt("why did that fail? tell me more", true),
            IntentAction::Latest("why did that fail? tell me more".into())
        );
        assert_eq!(classify_prompt("/compact", false), IntentAction::None);
        assert_eq!(classify_prompt("ok", true), IntentAction::None);
        assert_eq!(
            classify_prompt("TASK: migrate db", true),
            IntentAction::NewEpoch {
                root: Some("migrate db".into())
            }
        );
        assert_eq!(
            classify_prompt("task:", true),
            IntentAction::NewEpoch { root: None }
        );
        assert_eq!(
            classify_prompt("Subtask: write tests", true),
            IntentAction::Subtask("write tests".into())
        );
        assert_eq!(
            classify_prompt("short one", false),
            IntentAction::Latest("short one".into())
        );
    }
}
