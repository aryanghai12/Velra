//! Exact test identifiers: which tests a run reported as failing, and whether a
//! later passing run covered them. All pure.
//!
//! A command's outcome belongs to the *command*. What a session continues from
//! is usually a *test*: `tests/test_reconcile.py::test_march_window_totals`,
//! not "`pytest -q` failed". Before this module the ledger kept test names only
//! as lines of a failure excerpt, so they reached the capsule only while the
//! renderer could afford excerpt lines, and a test that failed and was then
//! fixed vanished entirely once a newer run of the same command failed on
//! something else. The v0.1.2 Token-Burn qualification lost the target test of
//! all four of its restores that way (see DECISIONS.md, D64).
//!
//! Nothing here reads the database. The snapshot feeds it stored excerpts and
//! command lines, which is why the parsing is deliberately literal: it
//! recognises the summary line each common runner prints for a failed test and
//! nothing else.

use crate::commands::classify;
use crate::model::CommandKind;
use crate::paths::to_slash;
use crate::shell::{command_words, exe_basename};
use crate::text::truncate_chars;

/// Longest identifier kept. Real ones are far shorter; this only bounds junk.
const ID_MAX_CHARS: usize = 200;
/// Longest one-line reason kept from the runner's summary line.
const DETAIL_MAX_CHARS: usize = 120;

/// One test a runner reported as failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingTest {
    /// The identifier exactly as the runner printed it (slashes normalised).
    pub id: String,
    /// The runner's own one-line reason, when the summary line carries one.
    pub detail: Option<String>,
}

fn clean_id(raw: &str) -> Option<String> {
    let id = raw.trim().trim_end_matches([':', ',']);
    if id.is_empty() || id.chars().count() > ID_MAX_CHARS {
        return None;
    }
    Some(to_slash(id))
}

fn clean_detail(raw: &str) -> Option<String> {
    let d = raw.trim();
    (!d.is_empty()).then(|| truncate_chars(d, DETAIL_MAX_CHARS).into_owned())
}

/// pytest: `FAILED <nodeid> - <reason>` / `ERROR <nodeid>` in the short summary,
/// or `<nodeid> FAILED [ 33%]` in verbose output.
fn pytest(line: &str) -> Option<FailingTest> {
    for prefix in ["FAILED ", "ERROR "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let (id, detail) = match rest.split_once(" - ") {
                Some((id, d)) => (id, clean_detail(d)),
                None => (rest, None),
            };
            let id = id.trim();
            let looks_like_node =
                id.contains("::") || id.contains('/') || id.contains('\\') || id.ends_with(".py");
            if looks_like_node && !id.contains(' ') {
                return Some(FailingTest {
                    id: clean_id(id)?,
                    detail,
                });
            }
        }
    }
    let (id, rest) = line.split_once(' ')?;
    if id.contains("::") && rest.trim_start().starts_with("FAILED") {
        return Some(FailingTest {
            id: clean_id(id)?,
            detail: None,
        });
    }
    None
}

/// cargo test: `test <path> ... FAILED`.
fn cargo(line: &str) -> Option<FailingTest> {
    let rest = line.strip_prefix("test ")?;
    let id = rest.strip_suffix("... FAILED")?.trim();
    (!id.is_empty() && !id.contains(' ')).then(|| FailingTest {
        id: id.to_string(),
        detail: None,
    })
}

/// go test: `--- FAIL: TestName (0.00s)`.
fn go(line: &str) -> Option<FailingTest> {
    let rest = line.strip_prefix("--- FAIL: ")?;
    let id = rest.split_whitespace().next()?;
    Some(FailingTest {
        id: clean_id(id)?,
        detail: None,
    })
}

/// unittest: `FAIL: test_x (pkg.mod.Class.test_x)` / `ERROR: ...`.
fn unittest(line: &str) -> Option<FailingTest> {
    let rest = line
        .strip_prefix("FAIL: ")
        .or_else(|| line.strip_prefix("ERROR: "))?;
    let (name, qualified) = match rest.split_once(" (") {
        Some((n, q)) => (n.trim(), q.trim_end_matches(')').trim()),
        None => (rest.trim(), ""),
    };
    let id = if qualified.contains('.') && !qualified.contains(' ') {
        qualified
    } else {
        name
    };
    (!id.is_empty() && !id.contains(' ')).then(|| FailingTest {
        id: id.to_string(),
        detail: None,
    })
}

/// jest / vitest: `✕ name (3 ms)` or `× name`.
fn jest(line: &str) -> Option<FailingTest> {
    let rest = line
        .strip_prefix('\u{2715}')
        .or_else(|| line.strip_prefix('\u{00d7}'))?
        .trim();
    let name = match rest.rsplit_once(" (") {
        Some((n, t)) if t.ends_with("ms)") || t.ends_with(" s)") => n,
        _ => rest,
    };
    Some(FailingTest {
        id: truncate_chars(name.trim(), ID_MAX_CHARS).into_owned(),
        detail: None,
    })
    .filter(|t| !t.id.is_empty())
}

/// rspec: `rspec ./spec/x_spec.rb:12 # description`.
fn rspec(line: &str) -> Option<FailingTest> {
    let rest = line.strip_prefix("rspec ")?;
    let (loc, desc) = match rest.split_once(" # ") {
        Some((l, d)) => (l, clean_detail(d)),
        None => (rest, None),
    };
    let loc = loc.trim().trim_start_matches("./");
    loc.contains(':').then(|| FailingTest {
        id: to_slash(loc),
        detail: desc,
    })
}

/// dotnet test: `Failed Namespace.Class.Method [12 ms]`.
fn dotnet(line: &str) -> Option<FailingTest> {
    let rest = line.strip_prefix("Failed ")?;
    let id = rest.split(" [").next()?.trim();
    (id.contains('.') && !id.contains(' ')).then(|| FailingTest {
        id: id.to_string(),
        detail: None,
    })
}

/// Every failing test named in `lines`, in order of first appearance, without
/// duplicates. A later line naming the same test only fills in a missing
/// detail.
pub fn failing_tests<S: AsRef<str>>(lines: &[S]) -> Vec<FailingTest> {
    let mut out: Vec<FailingTest> = Vec::new();
    for line in lines {
        let line = line.as_ref().trim();
        let parsed = pytest(line)
            .or_else(|| cargo(line))
            .or_else(|| go(line))
            .or_else(|| unittest(line))
            .or_else(|| jest(line))
            .or_else(|| rspec(line))
            .or_else(|| dotnet(line));
        let Some(t) = parsed else { continue };
        match out.iter_mut().find(|o| o.id == t.id) {
            Some(existing) => {
                if existing.detail.is_none() {
                    existing.detail = t.detail;
                }
            }
            None => out.push(t),
        }
    }
    out
}

/// Flags that narrow a run to part of the suite. A passing run that carries one
/// is only taken to cover a test whose identifier contains the flag's value.
const SELECTION_FLAGS: &[&str] = &[
    "-k",
    "-m",
    "-t",
    "--testNamePattern",
    "--testPathPattern",
    "-run",
    "--run",
    "--filter",
    "--grep",
    "-g",
    "--tests",
    "-R",
    "--lf",
    "--last-failed",
    "--sw",
    "--stepwise",
    "--deselect",
];

/// Runners whose positional arguments are build targets, not test selections.
const TARGET_RUNNERS: &[&str] = &[
    "make", "mvn", "mvnw", "gradle", "gradlew", "tox", "nox", "ctest",
];

/// How a test command line reads: which runner, and what it selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    /// Runner identity, stable across flags: `pytest`, `cargo test`, `npm test`.
    pub runner: String,
    /// Positional arguments after the runner (paths, name filters, packages).
    pub positionals: Vec<String>,
    /// `(flag, value)` for every selection flag present.
    pub selections: Vec<(String, Option<String>)>,
}

/// Reads a test command line. `None` when it is not a test command.
pub fn test_run(command: &str) -> Option<TestRun> {
    let classified = classify(command);
    if classified.kind != CommandKind::Test {
        return None;
    }
    let words = command_words(&classified.subcommand);
    let w0 = exe_basename(words.first()?);
    let arg = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let (runner, start) = match w0.as_str() {
        "python" | "python3" | "py" if arg(1) == "-m" => (arg(2).to_string(), 3),
        "py.test" => ("pytest".to_string(), 1),
        "npm" | "pnpm" | "yarn" | "bun" if arg(1) == "run" => (format!("{w0} run {}", arg(2)), 3),
        "npm" | "pnpm" | "yarn" | "bun" | "deno" | "dotnet" | "swift" | "mix" | "rails" | "go" => {
            (format!("{w0} {}", arg(1)), 2)
        }
        "cargo" if arg(1) == "nextest" => ("cargo nextest".to_string(), 3),
        "cargo" => (format!("cargo {}", arg(1)), 2),
        _ => (w0.clone(), 1),
    };
    let mut positionals = Vec::new();
    let mut selections = Vec::new();
    let mut i = start;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        // Redirections and separators the tokenizer leaves as words.
        if w.contains('>') || w.contains('<') || w == "--" {
            continue;
        }
        if w.starts_with('-') {
            let (flag, inline) = match w.split_once('=') {
                Some((f, v)) => (f, Some(v.to_string())),
                None => (w, None),
            };
            let is_selection = SELECTION_FLAGS.contains(&flag) || flag.starts_with("-Dtest");
            if is_selection {
                let takes_value = !matches!(flag, "--lf" | "--last-failed" | "--sw" | "--stepwise");
                let value = match inline {
                    Some(v) => Some(v),
                    None if takes_value && i < words.len() => {
                        i += 1;
                        Some(words[i - 1].clone())
                    }
                    None => None,
                };
                selections.push((flag.to_string(), value));
            }
            continue;
        }
        if TARGET_RUNNERS.contains(&w0.as_str()) {
            continue;
        }
        positionals.push(w.to_string());
    }
    Some(TestRun {
        runner,
        positionals,
        selections,
    })
}

fn norm_selector(p: &str) -> String {
    let p = to_slash(p.trim_matches(['"', '\'']));
    let p = p.strip_prefix("./").unwrap_or(&p).to_string();
    p.trim_end_matches('/').to_string()
}

/// Whether a *passing* run observed test `id`, first reported failing by a run
/// of `runner`.
///
/// The rule is literal and errs towards "not covered": the run must use the
/// same runner, and then either select nothing at all (the whole suite passed),
/// or name the test itself, its file, or a directory above it, or — for runners
/// whose positionals are name filters — a substring of it. A selection flag
/// counts only when its value appears in the identifier. When coverage cannot
/// be read off the command line the test simply keeps its last observed state,
/// which the capsule labels with the time it was observed.
pub fn run_covers(run: &TestRun, runner: &str, id: &str) -> bool {
    if run.runner != runner {
        return false;
    }
    for (_, value) in &run.selections {
        match value {
            Some(v) if !v.is_empty() && id.contains(v.trim_matches(['"', '\''])) => {}
            _ => return false,
        }
    }
    if run.positionals.is_empty() {
        return true;
    }
    // `go test` positionals are packages, and a Go test name does not say which
    // package it lives in. Only "every package" or an explicit `-run` that
    // already matched above can be read as covering it.
    if runner.starts_with("go ") {
        return !run.selections.is_empty()
            || run.positionals.iter().any(|p| {
                let p = norm_selector(p);
                p == "..." || p == "." || p.is_empty()
            });
    }
    let file = id.split("::").next().unwrap_or(id);
    let name_filters = runner.starts_with("cargo");
    run.positionals.iter().any(|p| {
        let p = norm_selector(p);
        if p.is_empty() || p == "." || p == "..." {
            return true;
        }
        if name_filters {
            return id.contains(&p);
        }
        id == p
            || file == p
            || id.starts_with(&format!("{p}::"))
            || file.starts_with(&format!("{p}/"))
    })
}

/// Whether a run *named* test `id` -- its node id, its file, or a filter that
/// matches it -- rather than merely covering it through a directory or the
/// whole suite. A session that keeps running one test by name is working on
/// it; the snapshot uses this to rank tests by focus.
pub fn run_names(run: &TestRun, runner: &str, id: &str) -> bool {
    if run.runner != runner {
        return false;
    }
    let file = id.split("::").next().unwrap_or(id);
    let by_selection = run.selections.iter().any(|(_, v)| {
        v.as_deref()
            .map(|v| v.trim_matches(['"', '\'']))
            .is_some_and(|v| !v.is_empty() && id.contains(v))
    });
    by_selection
        || run.positionals.iter().any(|p| {
            let p = norm_selector(p);
            !p.is_empty()
                && (p == id
                    || p == file
                    || id.starts_with(&format!("{p}::"))
                    || (runner.starts_with("cargo") && id.contains(&p)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(lines: &[&str]) -> Vec<String> {
        failing_tests(lines).into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn parses_each_runner_summary_line() {
        assert_eq!(
            failing_tests(&[
                "FAILED tests/test_reconcile.py::test_march_window_totals - assert 0 == 100"
            ]),
            vec![FailingTest {
                id: "tests/test_reconcile.py::test_march_window_totals".into(),
                detail: Some("assert 0 == 100".into()),
            }]
        );
        assert_eq!(
            ids(&["tests/test_retry.py::test_backoff FAILED   [ 33%]"]),
            ["tests/test_retry.py::test_backoff"]
        );
        assert_eq!(ids(&["ERROR tests/test_io.py"]), ["tests/test_io.py"]);
        assert_eq!(
            ids(&["test parser::tests::rejects_empty ... FAILED"]),
            ["parser::tests::rejects_empty"]
        );
        assert_eq!(ids(&["--- FAIL: TestLogin (0.01s)"]), ["TestLogin"]);
        assert_eq!(
            ids(&["FAIL: test_expiry (auth.tests.TokenTests.test_expiry)"]),
            ["auth.tests.TokenTests.test_expiry"]
        );
        assert_eq!(
            ids(&["\u{2715} rejects an expired token (4 ms)"]),
            ["rejects an expired token"]
        );
        assert_eq!(
            ids(&["rspec ./spec/models/user_spec.rb:12 # User validates email"]),
            ["spec/models/user_spec.rb:12"]
        );
        assert_eq!(
            ids(&["Failed Billing.Tests.InvoiceTests.RoundsHalfEven [12 ms]"]),
            ["Billing.Tests.InvoiceTests.RoundsHalfEven"]
        );
    }

    #[test]
    fn ignores_lines_that_only_look_like_failures() {
        assert!(failing_tests(&[
            "E       assert 0 == 100",
            "tests\\test_reconcile.py:25: AssertionError",
            "FAILED (failures=2)",
            "=== 2 failed, 3 passed in 0.10s ===",
            "test result: FAILED. 2 passed; 1 failed",
            "Error: something broke",
        ])
        .is_empty());
    }

    #[test]
    fn dedupes_and_backfills_detail() {
        let got = failing_tests(&[
            "tests/a.py::test_x FAILED",
            "FAILED tests/a.py::test_x - assert 1 == 2",
        ]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].detail.as_deref(), Some("assert 1 == 2"));
    }

    #[test]
    fn windows_separators_are_normalised() {
        assert_eq!(
            ids(&["FAILED tests\\test_a.py::test_x - boom"]),
            ["tests/test_a.py::test_x"]
        );
    }

    fn covers(command: &str, runner: &str, id: &str) -> bool {
        run_covers(&test_run(command).expect("a test command"), runner, id)
    }

    #[test]
    fn a_run_covers_its_own_file_directory_or_node() {
        let id = "tests/test_retry.py::test_retry_preserves_idempotency_key";
        assert!(covers(
            "python -m pytest tests/test_retry.py -v",
            "pytest",
            id
        ));
        assert!(covers("pytest tests/ -q", "pytest", id));
        assert!(covers("pytest ./tests", "pytest", id));
        assert!(covers(&format!("pytest {id}"), "pytest", id));
        assert!(covers("cd /repo && python -m pytest -q 2>&1", "pytest", id));
        assert!(!covers("pytest tests/test_ledger.py", "pytest", id));
        assert!(!covers(
            "pytest tests/test_retry.py::test_backoff",
            "pytest",
            id
        ));
    }

    #[test]
    fn narrowed_or_foreign_runs_do_not_cover() {
        let id = "tests/test_retry.py::test_retry_preserves_idempotency_key";
        assert!(!covers("pytest -k backoff", "pytest", id));
        assert!(covers("pytest -k idempotency", "pytest", id));
        assert!(!covers("pytest --lf", "pytest", id));
        assert!(!covers("cargo test", "pytest", id));
        assert!(test_run("git status").is_none());
    }

    #[test]
    fn name_filter_runners() {
        assert!(covers(
            "cargo test",
            "cargo test",
            "parser::tests::rejects_empty"
        ));
        assert!(covers(
            "cargo test rejects",
            "cargo test",
            "parser::tests::rejects_empty"
        ));
        assert!(!covers(
            "cargo test lexer",
            "cargo test",
            "parser::tests::rejects_empty"
        ));
        assert!(covers("go test ./...", "go test", "TestLogin"));
        assert!(covers(
            "go test -run TestLogin ./auth",
            "go test",
            "TestLogin"
        ));
        assert!(!covers(
            "go test -run TestLogout ./auth",
            "go test",
            "TestLogin"
        ));
        assert!(!covers("go test ./billing", "go test", "TestLogin"));
    }

    #[test]
    fn naming_is_narrower_than_covering() {
        let id = "tests/test_retry.py::test_key";
        let names = |c: &str| run_names(&test_run(c).expect("test"), "pytest", id);
        assert!(names("pytest tests/test_retry.py"));
        assert!(names(&format!("pytest {id}")));
        assert!(names("pytest -k test_key"));
        assert!(
            !names("pytest tests/"),
            "a directory covers but does not name"
        );
        assert!(
            !names("pytest -q"),
            "the whole suite covers but does not name"
        );
    }

    #[test]
    fn target_runners_ignore_their_targets() {
        assert!(covers("make test", "make", "anything"));
        assert!(covers("mvn -q test", "mvn", "anything"));
    }
}
