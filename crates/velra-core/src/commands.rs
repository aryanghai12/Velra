//! Command & test tracking (§14): kind classification, signature, outcome,
//! failure excerpt and mentioned-path candidates. All pure.

use crate::model::{CommandKind, Outcome};
use crate::shell::{command_words, exe_basename, is_env_assignment, split_subcommands, tokenize};
use crate::text::{collapse_whitespace, truncate_chars};

/// Result of classifying a full command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub kind: CommandKind,
    /// The subcommand that determined `kind` (the whole command for `other`).
    pub subcommand: String,
}

fn classify_words(words: &[String]) -> CommandKind {
    use CommandKind::*;
    let Some(first) = words.first() else {
        return Other;
    };
    let w0 = exe_basename(first);
    let arg = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let has = |x: &str| words.iter().skip(1).any(|a| a == x);
    match w0.as_str() {
        "pytest" | "py.test" | "jest" | "vitest" | "mocha" | "rspec" | "phpunit" | "tox"
        | "nox" | "ctest" => Test,
        "python" | "python3" | "py" => {
            if arg(1) == "-m" && matches!(arg(2), "pytest" | "unittest") {
                Test
            } else {
                Other
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            let (a, b) = (arg(1), arg(2));
            if a == "test" || (a == "run" && b == "test") || (w0 == "npm" && a == "t") {
                Test
            } else if a == "build" || (a == "run" && b == "build") {
                Build
            } else {
                Other
            }
        }
        "deno" if arg(1) == "test" => Test,
        "node" if has("--test") => Test,
        "cargo" => match arg(1) {
            "test" | "nextest" => Test,
            "build" | "check" => Build,
            "clippy" => Lint,
            _ => Other,
        },
        "go" => match arg(1) {
            "test" => Test,
            "build" | "vet" => Build,
            _ => Other,
        },
        "rails" if arg(1) == "test" => Test,
        "dotnet" => match arg(1) {
            "test" => Test,
            "build" => Build,
            _ => Other,
        },
        "mvn" | "mvnw" => {
            if has("test") {
                Test
            } else if has("compile") || has("package") {
                Build
            } else {
                Other
            }
        }
        "gradle" | "gradlew" => {
            if has("test") {
                Test
            } else if has("build") {
                Build
            } else {
                Other
            }
        }
        "mix" if arg(1) == "test" => Test,
        "swift" => match arg(1) {
            "test" => Test,
            "build" => Build,
            _ => Other,
        },
        "make" => {
            if has("test") {
                return Test;
            }
            let targets: Vec<&String> = words
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with('-') && !is_env_assignment(a))
                .collect();
            if targets.is_empty() || targets.iter().all(|t| *t == "all" || *t == "build") {
                Build
            } else {
                Other
            }
        }
        "tsc" => Build,
        "eslint" | "ruff" | "flake8" | "mypy" | "pyright" | "golangci-lint" | "rubocop"
        | "biome" => Lint,
        "prettier" if has("--check") => Lint,
        "git" => Git,
        _ => Other,
    }
}

/// Classifies a command line. Across subcommands the highest-priority kind
/// wins (test > build > lint > git > other); ties keep the first subcommand.
pub fn classify(command: &str) -> Classified {
    let mut best: Option<(CommandKind, &str)> = None;
    for sub in split_subcommands(command) {
        let kind = classify_words(&command_words(sub));
        if best.is_none_or(|(k, _)| kind < k) {
            best = Some((kind, sub));
        }
    }
    match best {
        Some((kind, sub)) if kind != CommandKind::Other => Classified {
            kind,
            subcommand: sub.to_string(),
        },
        _ => Classified {
            kind: CommandKind::Other,
            subcommand: command.trim().to_string(),
        },
    }
}

/// Signature: kind + normalized subcommand (whitespace collapsed, env
/// assignments stripped, `--color*` / `--reporter*` flags removed).
pub fn signature(kind: CommandKind, subcommand: &str) -> String {
    let words = tokenize(subcommand);
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    let mut leading = true;
    while i < words.len() {
        let w = words[i].as_str();
        if leading && is_env_assignment(w) {
            i += 1;
            continue;
        }
        leading = false;
        if w.starts_with("--color") || w.starts_with("--reporter") {
            // `--reporter dot` form: drop the value too.
            if (w == "--reporter" || w == "--reporters")
                && words.get(i + 1).is_some_and(|v| !v.starts_with('-'))
            {
                i += 1;
            }
            i += 1;
            continue;
        }
        out.push(w);
        i += 1;
    }
    format!("{}:{}", kind.as_str(), collapse_whitespace(&out.join(" ")))
}

/// Parses Claude Code's `Exit code N` first line of a failed shell tool.
/// Returns the exit code and the remaining output.
pub fn parse_exit_code_prefix(error: &str) -> (Option<i64>, &str) {
    let (first, rest) = match error.split_once('\n') {
        Some((f, r)) => (f, r),
        None => (error, ""),
    };
    match first.trim().strip_prefix("Exit code ") {
        Some(n) => match n.trim().parse::<i64>() {
            Ok(code) => (Some(code), rest),
            Err(_) => (None, error),
        },
        None => (None, error),
    }
}

/// Neutralizes zero-count summaries ("0 failed", "0 errors") so they do not
/// read as failure markers.
fn without_zero_counts(output: &str) -> String {
    let mut s = output.to_string();
    for pat in [
        " 0 failed",
        " 0 failures",
        " 0 errors",
        " 0 error",
        "\t0 failed",
        "(0 failed",
        " 0 FAILED",
    ] {
        if s.contains(pat) {
            s = s.replace(pat, " ");
        }
    }
    if s.starts_with("0 failed") {
        s.replace_range(..8, "");
    }
    s
}

const STRONG_FAIL: &[&str] = &[
    "FAILED",
    "--- FAIL",
    "test result: FAILED",
    "error[",
    "error TS",
    "Traceback",
    "\u{2717}",
    "\u{2715}",
];
const WEAK_FAIL: &[&str] = &["FAIL ", "failed", "Error:", "ERRORS"];

fn has_word(s: &str, word: &str) -> bool {
    s.match_indices(word).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

fn has_pass_marker(s: &str) -> bool {
    s.contains("passed")
        || has_word(s, "PASS")
        || has_word(s, "ok")
        || s.contains("test result: ok")
        || s.contains("0 failed")
        || s.lines()
            .any(|l| l.trim_start().starts_with("Tests:") && l.contains("passed"))
}

/// A pass summary that outranks weak failure markers such as `Error:` in logs.
fn has_strong_pass_summary(s: &str) -> bool {
    s.contains("test result: ok")
        || s.lines().any(|l| {
            let t = l.trim();
            (t.starts_with("Tests:") && t.contains("passed") && !t.contains("failed"))
                || (t.contains(" passed") && !t.contains("failed") && !t.contains("error"))
        })
}

/// Inputs to [`outcome`].
#[derive(Debug, Clone, Copy)]
pub struct OutcomeInput<'a> {
    pub kind: CommandKind,
    /// Event was `PostToolUseFailure`.
    pub failure_event: bool,
    /// Event was `PostToolUse` (Claude Code reports non-zero exits as failures).
    pub success_event: bool,
    pub exit_code: Option<i64>,
    pub interrupted: bool,
    /// Combined output tail (stdout then stderr, or the failure text).
    pub output: &'a str,
}

/// Outcome per §14, with zero-count neutralization and a strong-pass override
/// for weak markers on successful runs (see DECISIONS.md).
pub fn outcome(i: OutcomeInput<'_>) -> Outcome {
    if i.interrupted {
        return Outcome::Interrupted;
    }
    if i.failure_event {
        return Outcome::Fail;
    }
    if i.exit_code.is_some_and(|c| c != 0) {
        return Outcome::Fail;
    }
    let runner = matches!(
        i.kind,
        CommandKind::Test | CommandKind::Build | CommandKind::Lint
    );
    if !runner {
        return Outcome::Unknown;
    }
    let text = without_zero_counts(i.output);
    let strong_fail = STRONG_FAIL.iter().any(|m| text.contains(m))
        || text.lines().any(|l| {
            l.contains(" failed")
                && l.split_whitespace()
                    .any(|w| w.parse::<u64>().is_ok_and(|n| n > 0))
        });
    let weak_fail = WEAK_FAIL.iter().any(|m| text.contains(m));
    let exit_ok = i.exit_code == Some(0) || i.success_event;
    if strong_fail || (weak_fail && !(exit_ok && has_strong_pass_summary(&text))) {
        return Outcome::Fail;
    }
    if has_pass_marker(i.output) || exit_ok {
        return Outcome::Pass;
    }
    Outcome::Unknown
}

fn excerpt_match(line: &str) -> bool {
    line.starts_with("E  ")
        || [
            "FAIL",
            "Error",
            "error:",
            "Expected",
            "Received",
            "expected",
            "assert",
            "panicked",
            "AssertionError",
            "Traceback",
        ]
        .iter()
        .any(|m| line.contains(m))
}

/// Up to 8 lines matching failure patterns (preferring the last occurrences),
/// else the last 6 non-empty lines; original order; each ≤ 200 chars.
pub fn failure_excerpt(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output.lines().map(str::trim_end).collect();
    let matched: Vec<usize> = (0..lines.len())
        .filter(|&i| !lines[i].trim().is_empty() && excerpt_match(lines[i]))
        .collect();
    let chosen: Vec<usize> = if matched.is_empty() {
        let non_empty: Vec<usize> = (0..lines.len())
            .filter(|&i| !lines[i].trim().is_empty())
            .collect();
        non_empty[non_empty.len().saturating_sub(6)..].to_vec()
    } else {
        matched[matched.len().saturating_sub(8)..].to_vec()
    };
    chosen
        .into_iter()
        .map(|i| truncate_chars(lines[i].trim(), 200).into_owned())
        .collect()
}

/// A `path(:line(:col)?)?` token found in command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathToken {
    pub raw: String,
    pub path: String,
    pub line: Option<u32>,
}

fn split_path_line(tok: &str) -> (String, Option<u32>) {
    // Keep a Windows drive prefix intact.
    let (drive, rest) =
        if tok.len() >= 3 && tok.as_bytes()[1] == b':' && tok.as_bytes()[0].is_ascii_alphabetic() {
            tok.split_at(2)
        } else {
            ("", tok)
        };
    let mut pieces = rest.split(':');
    let path = pieces.next().unwrap_or("");
    let line = pieces.next().and_then(|l| l.parse::<u32>().ok());
    (format!("{drive}{path}"), line)
}

/// Candidate path tokens in `text`, in order of appearance, de-duplicated,
/// at most `limit`. Resolution against the filesystem is the caller's job.
pub fn path_tokens(text: &str, limit: usize) -> Vec<PathToken> {
    let mut out: Vec<PathToken> = Vec::new();
    let seps = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '<'
                    | '>'
                    | '{'
                    | '}'
                    | ','
                    | ';'
                    | '`'
                    | '|'
                    | '='
            )
    };
    for raw in text.split(seps) {
        if out.len() >= limit {
            break;
        }
        let tok = raw.trim_end_matches(['.', ':']).trim_start_matches("./");
        if tok.len() < 3
            || tok.len() > 300
            || tok.starts_with("http:")
            || tok.starts_with("https:")
            || tok.starts_with('-')
        {
            continue;
        }
        let (path, line) = split_path_line(tok);
        let looks_like_file = path.contains('/')
            || path.contains('\\')
            || path.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty()
                    && !ext.is_empty()
                    && ext.len() <= 8
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
            });
        if !looks_like_file || path.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        if out.iter().any(|t| t.path == path && t.line == line) {
            continue;
        }
        out.push(PathToken {
            raw: tok.to_string(),
            path,
            line,
        });
    }
    out
}

/// Mentioned paths kept per command.
pub const MENTION_LIMIT: usize = 16;

/// Directory names whose files are installed or generated, not the project's
/// own source. A traceback that runs through the test runner or a dependency
/// names them before the project's file, and a virtualenv or `node_modules`
/// usually lives inside the workspace.
const THIRD_PARTY_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "site-packages",
    "dist-packages",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".gradle",
    ".git",
];

/// Whether a project-relative path lies in an installed or generated
/// directory ([`THIRD_PARTY_DIRS`]).
pub fn is_third_party(rel: &str) -> bool {
    rel.split(['/', '\\'])
        .any(|c| THIRD_PARTY_DIRS.iter().any(|d| c.eq_ignore_ascii_case(d)))
}

/// The output a shell event stored, as the reducer reads it: a failed call's
/// error text after its `Exit code N` line, else stdout then stderr; and the
/// exit code the event reports.
pub fn stored_output(p: &crate::event::Payload, failure: bool) -> (Option<i64>, String) {
    if failure {
        let err = p.error.as_deref().unwrap_or("");
        let (code, rest) = parse_exit_code_prefix(err);
        (p.exit_code.or(code), rest.to_string())
    } else {
        let mut out = p.stdout_tail.clone().unwrap_or_default();
        if let Some(e) = p.stderr_tail.as_deref().filter(|e| !e.is_empty()) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(e);
        }
        (p.exit_code, out)
    }
}

/// The workspace files `output` names, in order of appearance, at most
/// [`MENTION_LIMIT`]: each candidate token ([`path_tokens`]) that is a file on
/// disk *now*, inside `root`, and not in an installed or generated directory
/// ([`is_third_party`]).
///
/// "Now" is the point: this is called by the hook as the command returns, and
/// what it finds is carried in the event (`Payload::mentioned`). Checked by the
/// reducer instead, whenever it happened to run, a file created after the
/// command read as named by its failure, and one deleted after it did not.
pub fn mentioned_files(
    output: &str,
    cwd: Option<&std::path::Path>,
    root: &std::path::Path,
) -> Vec<crate::event::PathMention> {
    use crate::paths;
    use std::path::PathBuf;
    let root_str = root.to_string_lossy().into_owned();
    let mut out: Vec<crate::event::PathMention> = Vec::new();
    for tok in path_tokens(output, 40) {
        if out.len() >= MENTION_LIMIT {
            break;
        }
        let candidates: Vec<PathBuf> = if paths::is_absolute_str(&tok.path) {
            vec![PathBuf::from(&tok.path)]
        } else {
            cwd.map(|c| c.join(&tok.path))
                .into_iter()
                .chain(std::iter::once(root.join(&tok.path)))
                .collect()
        };
        let Some(found) = candidates.into_iter().find(|c| c.is_file()) else {
            continue;
        };
        let rel = paths::relative_to_root_resolved(&found.to_string_lossy(), &root_str);
        if paths::is_absolute_str(&rel) {
            continue; // outside the project root
        }
        let rel = rel.replace("/./", "/");
        if is_third_party(&rel) {
            continue;
        }
        if out.iter().any(|m| m.path == rel && m.line == tok.line) {
            continue;
        }
        out.push(crate::event::PathMention {
            path: rel,
            line: tok.line,
            raw: tok.raw,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CommandKind::*;

    #[test]
    fn third_party_directories() {
        for p in [
            "node_modules/x/index.js",
            ".venv/lib/python3.12/site-packages/_pytest/python.py",
            "venv/Lib/Site-Packages/a.py",
            "src/__pycache__/a.cpython-312.pyc",
            ".tox/py312/lib/a.py",
        ] {
            assert!(is_third_party(p), "{p}");
        }
        for p in [
            "src/pay.py",
            "tests/test_pay.py",
            "packages/site/a.ts",
            "node_modules_shim.js",
        ] {
            assert!(!is_third_party(p), "{p}");
        }
    }

    fn kind(c: &str) -> CommandKind {
        classify(c).kind
    }

    #[test]
    fn kinds() {
        for c in [
            "pytest -x",
            "python -m pytest tests",
            "python3 -m unittest",
            "npx jest",
            "vitest run",
            "npm test",
            "npm run test",
            "pnpm test",
            "yarn test",
            "bun test",
            "deno test",
            "node --test",
            "cargo test",
            "cargo nextest run",
            "go test ./...",
            "bundle exec rspec",
            "rails test",
            "phpunit",
            "dotnet test",
            "mvn -q test",
            "./gradlew test",
            "mix test",
            "swift test",
            "ctest",
            "make test",
            "tox",
            "nox",
            "cd web && FOO=1 npm test",
        ] {
            assert_eq!(kind(c), Test, "{c}");
        }
        for c in [
            "tsc -p .",
            "cargo build",
            "cargo check",
            "go vet ./...",
            "npm run build",
            "mvn package",
            "gradle build",
            "dotnet build",
            "make",
            "make -j8 all",
            "swift build",
        ] {
            assert_eq!(kind(c), Build, "{c}");
        }
        for c in [
            "eslint .",
            "ruff check",
            "flake8",
            "mypy src",
            "pyright",
            "cargo clippy",
            "golangci-lint run",
            "rubocop",
            "biome check",
            "prettier --check .",
        ] {
            assert_eq!(kind(c), Lint, "{c}");
        }
        assert_eq!(kind("git status"), Git);
        assert_eq!(kind("ls -la"), Other);
        assert_eq!(kind("make install"), Other);
        let c = classify("git stash && npm test");
        assert_eq!((c.kind, c.subcommand.as_str()), (Test, "npm test"));
    }

    #[test]
    fn signatures() {
        assert_eq!(
            signature(Test, "CI=1  npm   test --color=always"),
            "test:npm test"
        );
        assert_eq!(
            signature(Test, "jest --reporter dot -t x"),
            "test:jest -t x"
        );
    }

    #[test]
    fn exit_prefix() {
        assert_eq!(
            parse_exit_code_prefix("Exit code 2\nboom"),
            (Some(2), "boom")
        );
        assert_eq!(
            parse_exit_code_prefix("spawn failed"),
            (None, "spawn failed")
        );
    }

    fn oc(
        kind: CommandKind,
        success: bool,
        failure: bool,
        exit: Option<i64>,
        out: &str,
    ) -> Outcome {
        outcome(OutcomeInput {
            kind,
            failure_event: failure,
            success_event: success,
            exit_code: exit,
            interrupted: false,
            output: out,
        })
    }

    #[test]
    fn outcomes() {
        assert_eq!(oc(Test, false, true, Some(1), "x"), Outcome::Fail);
        assert_eq!(
            oc(
                Test,
                true,
                false,
                None,
                "test result: ok. 3 passed; 0 failed;"
            ),
            Outcome::Pass
        );
        assert_eq!(
            oc(Test, true, false, None, "=== 2 failed, 3 passed ==="),
            Outcome::Fail
        );
        assert_eq!(
            oc(Test, true, false, None, "FAILED tests/test_a.py::test_x"),
            Outcome::Fail
        );
        assert_eq!(
            oc(
                Test,
                true,
                false,
                None,
                "Error: expected\nTests: 5 passed, 5 total"
            ),
            Outcome::Pass
        );
        assert_eq!(oc(Build, true, false, None, ""), Outcome::Pass);
        assert_eq!(oc(Test, false, false, None, "nothing"), Outcome::Unknown);
        assert_eq!(oc(Other, true, false, None, "FAILED"), Outcome::Unknown);
        assert_eq!(
            outcome(OutcomeInput {
                kind: Test,
                failure_event: true,
                success_event: false,
                exit_code: None,
                interrupted: true,
                output: ""
            }),
            Outcome::Interrupted
        );
    }

    #[test]
    fn excerpts() {
        let out = "running 3 tests\ntest a ... ok\n\nthread 'b' panicked at src/lib.rs:10:5:\nassertion failed: x == 2\n  left: 1\n right: 2\ntest result: FAILED. 2 passed; 1 failed";
        assert_eq!(
            failure_excerpt(out),
            vec![
                "thread 'b' panicked at src/lib.rs:10:5:",
                "assertion failed: x == 2",
                "test result: FAILED. 2 passed; 1 failed"
            ]
        );
        assert_eq!(failure_excerpt("a\nb\n\nc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn path_candidates() {
        let toks = path_tokens("  at Object.<anonymous> (src/foo.test.js:12:5)\nthread panicked at src/lib.rs:10:5:\nFile \"C:\\p\\x.py\", line 3\nsee https://x.io/a.js", 10);
        let paths: Vec<(&str, Option<u32>)> =
            toks.iter().map(|t| (t.path.as_str(), t.line)).collect();
        assert!(paths.contains(&("src/foo.test.js", Some(12))));
        assert!(paths.contains(&("src/lib.rs", Some(10))));
        assert!(paths.contains(&("C:\\p\\x.py", None)));
        assert!(!paths.iter().any(|(p, _)| p.contains("x.io")));
    }
}
