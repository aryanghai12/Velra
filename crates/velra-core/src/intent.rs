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

/// Whether an already-normalized prompt is a slash command (rule 2).
///
/// A slash command is `/` immediately followed by a command *name*: a letter,
/// then letters, digits, `-`, `_` or `:` (plugin commands are
/// `/plugin:command`), then whitespace or the end. Anything else that happens
/// to begin with `/` is the user's text -- most often a path, `/tmp/build.log
/// shows the linker failing`, which used to be dropped as a command and never
/// became the objective. A path always carries a second `/`, a `.` or a `\`
/// in its first word, which a command name cannot.
///
/// A single root-level name has the shape of both: `/etc is missing`,
/// `/health returns 503`, `/api has no rate limit`. What separates them is
/// the word after the name. A command's arguments are what the command acts
/// on (`/review the auth module`, `/deploy staging`); a sentence about a path
/// or route goes on with the verb it is the subject of. When the next word is
/// an auxiliary, a modal or one of [`PREDICATES`], the text is a sentence and
/// the user's own words. Dropping a real sentence as a command loses the
/// user's message outright; reading a command as a message records one
/// message too many, which the next prompt supersedes.
///
/// What stays a command: a bare `/name`, and `/name` followed by anything
/// else. A root-level path followed by a verb this list does not know
/// (`/tmp fills up`) is still read as a command.
pub fn is_slash_command(normalized: &str) -> bool {
    let Some(rest) = normalized.strip_prefix('/') else {
        return false;
    };
    let mut words = rest.split(char::is_whitespace);
    let name = words.next().unwrap_or("");
    let mut chars = name.chars();
    let named = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'));
    if !named {
        return false;
    }
    let next = words.find(|w| !w.is_empty()).map(|w| {
        w.trim_end_matches([',', '.', ';', ':', '!', '?'])
            .to_ascii_lowercase()
    });
    !next.is_some_and(|w| PREDICATES.contains(&w.as_str()))
}

/// Words that, directly after `/name`, make `/name` the subject of a
/// sentence: auxiliaries, modals, negations and the verbs people use to
/// report what a path or an endpoint does.
pub const PREDICATES: &[&str] = &[
    "is",
    "isn't",
    "are",
    "aren't",
    "was",
    "wasn't",
    "were",
    "weren't",
    "has",
    "hasn't",
    "have",
    "haven't",
    "had",
    "does",
    "doesn't",
    "did",
    "didn't",
    "do",
    "don't",
    "can",
    "can't",
    "cannot",
    "could",
    "couldn't",
    "will",
    "won't",
    "would",
    "wouldn't",
    "should",
    "shouldn't",
    "must",
    "may",
    "might",
    "keeps",
    "kept",
    "seems",
    "looks",
    "appears",
    "returns",
    "returned",
    "fails",
    "failed",
    "throws",
    "threw",
    "crashes",
    "crashed",
    "breaks",
    "broke",
    "hangs",
    "hung",
    "gives",
    "gave",
    "shows",
    "showed",
    "contains",
    "points",
    "redirects",
    "responds",
    "responded",
    "leaks",
    "exists",
    "still",
    "no",
    "not",
    "never",
    "gets",
    "got",
    "needs",
    "lacks",
    "times",
    "serves",
    "loads",
];

/// Applies rules 2–6 to an already-normalized prompt.
pub fn classify_prompt(normalized: &str, has_live_root: bool) -> IntentAction {
    if is_slash_command(normalized) {
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

    #[test]
    fn slash_commands_are_names_and_paths_are_text() {
        for cmd in [
            "/compact",
            "/clear",
            "/review please look at auth",
            "/my-plugin:do-thing with args",
            "/fix_issue 123",
            "/Compact",
        ] {
            assert!(is_slash_command(cmd), "{cmd}");
            assert_eq!(classify_prompt(cmd, false), IntentAction::None, "{cmd}");
        }
        for text in [
            "/tmp/foo/build.log shows the linker failing",
            "/path/to/file.py raises KeyError",
            "/c/Users/dev/repo/src/app.ts has a type error",
            "/src/payments/retry.py: the key is dropped",
            "/ is the root route and it returns 404",
            "/.env is missing in the container image",
            "/9lives is not a command name",
            "\"/quoted text\" is printed by the CLI",
            "fix /tmp/foo",
            "",
            "/",
            // Root-level paths and routes as the subject of a sentence.
            "/etc is missing from the image",
            "/tmp isn't writable on the runner",
            "/health returns 503 after the deploy",
            "/api has no rate limit",
            "/login redirects twice, fix it",
            "/users can't be listed by an admin",
            "/Users/dev/My Project/src/app.ts fails",
            "/c/Program Files/Velra/velra.exe is not on PATH",
            "/v1.2 of the API is still served",
        ] {
            assert!(!is_slash_command(text), "{text}");
        }
        // A command with arguments, however they read, and a bare name.
        for cmd in [
            "/deploy staging",
            "/review the auth module",
            "/etc",
            "/test all of it",
        ] {
            assert!(is_slash_command(cmd), "{cmd}");
        }
        assert_eq!(
            classify_prompt("/tmp/foo/build.log shows the linker failing", false),
            IntentAction::Root("/tmp/foo/build.log shows the linker failing".into())
        );
    }
}
