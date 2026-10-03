//! Which lines of the user's text are pasted material rather than prose the
//! user is addressing to the assistant.
//!
//! # Why this exists
//!
//! A prompt is the user's, but not every line in it is the user speaking.
//! People paste test reports, logs, compiler diagnostics, stack traces, API
//! responses, source code and documentation straight into the message,
//! usually without a fence. Those lines are full of the words the constraint
//! extractor looks for -- `AssertionError: amount must be positive`, `hint:
//! do not use --force`, `// must not be called from the UI thread`, `You must
//! provide an idempotency key` from a vendor's docs -- and before v0.1.2 every
//! one of them could become a durable "rule the user stated".
//!
//! # The rule, and why it errs the way it does
//!
//! A line is material when the text itself gives deterministic evidence:
//!
//! * it is inside a fenced block (```` ``` ```` / `~~~`), or a blockquote
//!   (`>`);
//! * it has the shape of machine output or code ([`machine_line`]): a pytest
//!   `E   ` line, a traceback frame, a `path:line:` diagnostic, a timestamped
//!   log line, a JSON field, a comment, a statement ending in `:` / `{` / `)`;
//! * most lines of its paragraph have that shape;
//! * it follows a lead-in that says output or someone else's words come next
//!   (`Here is the output:`, `logs:`, `The runbook says:`) -- see
//!   [`lead_in`].
//!
//! Material is not discarded: it stays in the prompt, the objective and the
//! latest message exactly as written. What it loses is the ability to create
//! *durable* state on its own -- a constraint, a rejected approach -- which is
//! the one place a mistake outlives the turn it was made in. When the text
//! cannot say whose words a line holds, the line is treated as material: a
//! rule missed stays readable in the user's message; a rule invented is
//! replayed as the user's instruction after every compaction.
//!
//! Output ends where the text stops looking like output: after a blank line,
//! a line with no output shape ends an output region, so `Please fix it. Do
//! not modify the tests.` after a pasted report is the user's again. Quoted
//! prose -- documentation, a runbook -- looks exactly like the user's prose,
//! so nothing in the text can end it; a quotation lead-in covers the rest of
//! the message.

/// What one line of the user's text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// The user's own prose, or a line nothing marks as anything else.
    Prose,
    Blank,
    /// A fence delimiter, or a line inside a fenced block.
    Fenced,
    /// Machine output, logs, diagnostics, stack traces, JSON, code.
    Output,
    /// Someone else's words: a blockquote, or text introduced as quoted.
    Quoted,
}

impl Line {
    /// Whether the line is somebody else's words or code rather than the
    /// user's prose.
    pub fn is_material(self) -> bool {
        matches!(self, Line::Fenced | Line::Output | Line::Quoted)
    }
}

/// Which kind of region a lead-in opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Output follows; it ends where the text stops looking like output.
    Output,
    /// Someone else's prose follows; nothing in the text can end it.
    Quotation,
}

/// Width of leading whitespace, a tab counting four.
fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// Whether `t` (leading whitespace removed) starts a list item.
pub fn is_list_item(t: &str) -> bool {
    if ["- ", "* ", "+ ", "\u{2022} "]
        .iter()
        .any(|m| t.starts_with(m))
    {
        return true;
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    (1..=3).contains(&digits) && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

fn starts_with_word(t: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        t.strip_prefix(w)
            .is_some_and(|rest| rest.is_empty() || !rest.as_bytes()[0].is_ascii_alphanumeric())
    })
}

/// `name.ext:12:` / `name.ext:12:3:` / `name.ext(12):` / `name.ext(12,3):` at
/// the start of `t`: the location prefix of a compiler, linter or test
/// runner diagnostic.
fn location_prefix(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b':' && b[i] != b'(' {
        i += 1;
    }
    let path = &t[..i];
    // A file name with an extension: `retry.py`, `src/retry.c`, `C:` excluded.
    let has_ext = path.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=6).contains(&ext.len())
            && ext.bytes().all(|c| c.is_ascii_alphanumeric())
    });
    if !has_ext || i >= b.len() {
        return false;
    }
    let rest = &t[i..];
    let digits_then = |s: &str, end: &[u8]| -> bool {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        n > 0 && s.as_bytes().get(n).is_some_and(|c| end.contains(c))
    };
    if let Some(r) = rest.strip_prefix(':') {
        return digits_then(r, b":");
    }
    if let Some(r) = rest.strip_prefix('(') {
        return digits_then(r, b"),");
    }
    false
}

/// `SomethingError:` / `pkg.SomethingException` at the start of `t`: the
/// last line of a Python traceback, a Java exception, a JS `TypeError`.
fn exception_line(t: &str) -> bool {
    let word: &str = t
        .split(|c: char| c.is_whitespace() || c == ':')
        .next()
        .unwrap_or("");
    const SUFFIXES: &[&str] = &[
        "Error",
        "Exception",
        "Failure",
        "Fault",
        "Panic",
        "Interrupt",
    ];
    let ident = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'));
    ident
        && word.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && SUFFIXES
            .iter()
            .any(|s| word.len() > s.len() && word.ends_with(s))
        && (t.len() == word.len() || t[word.len()..].starts_with(':'))
}

/// `2026-09-12T10:04:05`, `2026-09-12 10:04`, `[10:04:05]`, `10:04:05.123`.
fn timestamp_prefix(t: &str) -> bool {
    let t = t.strip_prefix('[').unwrap_or(t);
    let b = t.as_bytes();
    let d = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let date = (0..4).all(d)
        && b.get(4) == Some(&b'-')
        && d(5)
        && d(6)
        && b.get(7) == Some(&b'-')
        && d(8)
        && d(9);
    if date {
        return true;
    }
    d(0) && d(1) && b.get(2) == Some(&b':') && d(3) && d(4) && b.get(5) == Some(&b':') && d(6)
}

/// Whether one line, on its own, has the shape of machine output or code.
///
/// Every rule is a shape prose does not take: a pytest `E   ` prefix, a
/// `path:line:` location, a code keyword *and* code punctuation. A keyword
/// alone is not enough -- `return early if the list is empty` is prose.
pub fn machine_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let lead = indent(line);
    // pytest / unittest.
    if t.starts_with("E  ") || t.starts_with("E\t") {
        return true;
    }
    if starts_with_word(
        t,
        &[
            "FAILED", "PASSED", "ERROR", "SKIPPED", "XFAIL", "XPASS", "FAIL", "PASS",
        ],
    ) && t.split_whitespace().count() >= 2
    {
        return true;
    }
    if t.starts_with("Traceback (most recent call last)") {
        return true;
    }
    if t.starts_with("File \"") && t.contains("\", line ") {
        return true;
    }
    // Separator rules: `=====`, `_____ test_x _____`, `-----`.
    if t.len() >= 3 {
        let first = t.as_bytes()[0];
        if matches!(first, b'=' | b'_' | b'-' | b'~' | b'*')
            && t.bytes().take(3).all(|c| c == first)
        {
            return true;
        }
    }
    // pytest's progress line: `tests/test_retry.py ..F..  [ 16%]`.
    if let Some((file, marks)) = t.split_once(' ') {
        let marks = marks.trim();
        let marks = marks
            .rsplit_once('[')
            .map(|(m, pct)| if pct.ends_with("%]") { m.trim() } else { marks })
            .unwrap_or(marks);
        let named = file.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && ext.bytes().any(|c| c.is_ascii_alphabetic())
        });
        if named
            && !marks.is_empty()
            && marks
                .chars()
                .all(|c| matches!(c, '.' | 'F' | 'E' | 's' | 'x' | 'X'))
        {
            return true;
        }
    }
    if location_prefix(t) || exception_line(t) || timestamp_prefix(t) {
        return true;
    }
    // Log levels, upper case as loggers print them.
    let unbracketed = t.trim_start_matches('[');
    if starts_with_word(
        unbracketed,
        &[
            "TRACE", "DEBUG", "INFO", "WARN", "WARNING", "ERROR", "FATAL", "CRITICAL",
        ],
    ) && (unbracketed.contains(']') || unbracketed.split_whitespace().count() >= 2)
    {
        return true;
    }
    // Compiler and tool prefixes, lower case as tools print them. `note:` and
    // `help:` are left out on purpose: `note: never push to main` is how
    // people write.
    for p in [
        "error:", "error[", "warning:", "warning[", "fatal:", "hint:",
    ] {
        if t.starts_with(p) {
            return true;
        }
    }
    // rustc's source gutter.
    if t.starts_with("--> ") || t == "|" || t.starts_with("| ") && lead > 0 {
        return true;
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && t[digits..].trim_start().starts_with('|') {
        return true;
    }
    if t.starts_with("= note:") || t.starts_with("= help:") {
        return true;
    }
    // Shell prompts and REPLs.
    for p in ["$ ", "> $ ", ">>> ", "\u{276f} ", "PS "] {
        if t.starts_with(p) && (p != "PS " || t.contains(":\\")) {
            return true;
        }
    }
    let b = t.as_bytes();
    if b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' && t.contains('>')
    {
        return true;
    }
    // JS / Java stack frames.
    if let Some(frame) = t.strip_prefix("at ") {
        let tail_loc = frame
            .trim_end_matches(')')
            .rsplit(':')
            .take(2)
            .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()));
        if tail_loc || (frame.contains('(') && frame.ends_with(')') && !frame.contains(' ')) {
            return true;
        }
    }
    // JSON.
    if matches!(t, "{" | "}" | "[" | "]" | "}," | "],") {
        return true;
    }
    if t.starts_with('{') && (t.ends_with('}') || t.ends_with(',') || t.contains("\":")) {
        return true;
    }
    // A JSON field: `"key": …`.
    if let Some((_, after)) = t.strip_prefix('"').and_then(|k| k.split_once('"')) {
        if after.trim_start().starts_with(':') {
            return true;
        }
    }
    // Comments.
    for p in ["//", "/*", "*/", "<!--", "#!", "-- ", ";;"] {
        if t.starts_with(p) {
            return true;
        }
    }
    if let Some(rest) = t.strip_prefix('#') {
        let rest = rest.trim_start();
        // `# must not be None`, `#include`; a Markdown heading is capitalised.
        if rest.starts_with(|c: char| c.is_ascii_lowercase()) {
            return true;
        }
    }
    // Diffs.
    for p in ["diff --git ", "@@ ", "+++ ", "--- a/", "index "] {
        if t.starts_with(p) && (p != "index " || t.contains("..")) {
            return true;
        }
    }
    // Code: a keyword, and code punctuation to go with it.
    const KEYWORDS: &[&str] = &[
        "def",
        "class",
        "fn",
        "pub",
        "async",
        "impl",
        "struct",
        "enum",
        "trait",
        "import",
        "from",
        "return",
        "raise",
        "assert",
        "if",
        "elif",
        "else",
        "for",
        "while",
        "try",
        "except",
        "finally",
        "with",
        "function",
        "const",
        "var",
        "package",
        "using",
        "#include",
        "public",
        "private",
        "protected",
        "static",
        "void",
        "func",
        "interface",
        "export",
        "match",
        "switch",
        "case",
        "throw",
        "throws",
        "yield",
        "await",
        "self.",
        "this.",
    ];
    // A call: an identifier directly followed by `(`. `(see above)` in prose
    // has a space before it.
    let call = t
        .as_bytes()
        .windows(2)
        .any(|w| (w[0].is_ascii_alphanumeric() || w[0] == b'_') && w[1] == b'(');
    let code_end = t.ends_with(':') || t.ends_with('{') || t.ends_with(';') || t.ends_with("=>");
    if starts_with_word(t, KEYWORDS) && (code_end || call || t.contains(" = ")) {
        return true;
    }
    t == "}" || t == "};" || t == ")" || t == ");"
}

/// Words that name output as what follows a lead-in.
const OUTPUT_NOUNS: &[&str] = &[
    "output",
    "outputs",
    "log",
    "logs",
    "trace",
    "traceback",
    "stacktrace",
    "backtrace",
    "stack",
    "stderr",
    "stdout",
    "error",
    "errors",
    "failure",
    "failures",
    "result",
    "results",
    "report",
    "response",
    "body",
    "dump",
    "transcript",
    "printed",
    "prints",
    "returned",
    "returns",
    "answered",
    "diagnostics",
    "warning",
    "warnings",
    "console",
    "terminal",
    "shows",
    "showed",
];

/// Words that name someone else's prose as what follows.
const QUOTATION_NOUNS: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "readme",
    "runbook",
    "playbook",
    "wiki",
    "guide",
    "manual",
    "spec",
    "specification",
    "comment",
    "comments",
    "docstring",
    "says",
    "said",
    "say",
    "reads",
    "states",
    "wrote",
    "writes",
    "quote",
    "quoted",
    "excerpt",
    "ticket",
    "email",
    "policy",
    "changelog",
];

/// Words that make a colon line a heading for the user's own rules, never a
/// lead-in: `Rules for the log output:` introduces rules.
const RULE_WORDS: &[&str] = &[
    "rule",
    "rules",
    "constraint",
    "constraints",
    "requirement",
    "requirements",
    "invariant",
    "invariants",
];

/// The region a line opens when it introduces pasted material, or `None`.
///
/// A lead-in is a line of prose that names output or someone else's words
/// and then stops: it ends with `:` (`test output:`, `The runbook says:`),
/// or opens with `here is` / `here's` / `below is` (`Here's the error I
/// get`). A line that also names rules (`Rules for the log output:`) is a
/// heading for the user's own rules and opens nothing.
///
/// `says` / `said` name both: what follows is quoted, and the quotation
/// region is the conservative one.
pub fn lead_in(line: &str) -> Option<Region> {
    // Every line of prose is asked, so the shape is checked before anything
    // is allocated: a lead-in ends with `:` or opens with here / below /
    // attached.
    let t = line.trim();
    let opens = |p: &str| t.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
    if !t.ends_with(':') && !opens("here") && !opens("below") && !opens("attached") {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|w| RULE_WORDS.contains(w)) {
        return None;
    }
    let colon = lower.ends_with(':');
    let here = [
        "here is",
        "here's",
        "here are",
        "below is",
        "below are",
        "attached is",
    ]
    .iter()
    .any(|p| lower.starts_with(p));
    if !colon && !here {
        return None;
    }
    if words.iter().any(|w| QUOTATION_NOUNS.contains(w)) {
        return Some(Region::Quotation);
    }
    if words.iter().any(|w| OUTPUT_NOUNS.contains(w)) {
        return Some(Region::Output);
    }
    None
}

/// Classifies every line of `text` (as `str::lines` splits it).
pub fn classify(text: &str) -> Vec<Line> {
    let lines: Vec<&str> = text.lines().collect();
    let mut kinds = vec![Line::Prose; lines.len()];

    // Fences and per-line shapes.
    let mut in_fence = false;
    let mut indented_block = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let starts_block = i == 0 || kinds[i - 1] == Line::Blank || indented_block;
        indented_block = false;
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            kinds[i] = Line::Fenced;
            continue;
        }
        if in_fence {
            kinds[i] = Line::Fenced;
        } else if t.trim_end().is_empty() {
            kinds[i] = Line::Blank;
        } else if t.starts_with('>') {
            kinds[i] = Line::Quoted;
        } else if machine_line(line) {
            kinds[i] = Line::Output;
        } else if starts_block && indent(line) >= 4 && !is_list_item(t) {
            // An indented code block as Markdown defines one: four columns
            // or more, not a nested list item, and not interrupting a
            // paragraph -- a hard wrap indented with a tab is still prose.
            kinds[i] = Line::Output;
            indented_block = true;
        }
    }

    // A paragraph most of whose lines are output is output.
    let mut i = 0;
    while i < lines.len() {
        if matches!(kinds[i], Line::Blank | Line::Fenced) {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !matches!(kinds[i], Line::Blank | Line::Fenced) {
            i += 1;
        }
        let para = &mut kinds[start..i];
        let output = para.iter().filter(|k| **k == Line::Output).count();
        if output * 2 > para.len() {
            for k in para.iter_mut().filter(|k| **k == Line::Prose) {
                *k = Line::Output;
            }
        }
    }

    // Lead-ins and the regions they open.
    let mut i = 0;
    while i < lines.len() {
        let region = (kinds[i] == Line::Prose)
            .then(|| lead_in(lines[i]))
            .flatten();
        i += 1;
        let Some(region) = region else { continue };
        let mark = if region == Region::Quotation {
            Line::Quoted
        } else {
            Line::Output
        };
        let mut started = false;
        let mut after_blank = false;
        while i < lines.len() {
            match kinds[i] {
                Line::Blank => {
                    after_blank = started;
                    i += 1;
                    continue;
                }
                Line::Fenced => {
                    started = true;
                    after_blank = false;
                    i += 1;
                    continue;
                }
                _ => {}
            }
            if after_blank && region == Region::Output {
                let looks = kinds[i] != Line::Prose || indent(lines[i]) >= 2;
                if !looks {
                    break;
                }
            }
            if kinds[i] == Line::Prose {
                kinds[i] = mark;
            }
            started = true;
            after_blank = false;
            i += 1;
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Line> {
        classify(text)
    }

    #[test]
    fn machine_shapes_are_output_and_prose_is_not() {
        for line in [
            "E       AssertionError: amount must be positive",
            "FAILED tests/test_retry.py::test_x - AssertionError: key must not change",
            "tests/test_retry.py:40: AssertionError",
            "src/retry.c:41:9: warning: value must be checked",
            "src\\retry.cpp(12): error C2065: must be declared",
            "  File \"src/retry.py\", line 12, in charge",
            "ValueError: amount must not be negative",
            "java.lang.IllegalStateException: must not be null",
            "2026-09-12T10:04:05Z WARN retry: the client should not retry",
            "[INFO] do not stop the watcher",
            "error[E0382]: borrow of moved value",
            "hint: do not use --force here",
            "  --> src/retry.rs:41:9",
            "41 |     send(key);",
            "$ ./deploy.sh --dry-run",
            "PS C:\\repo> cargo test",
            "    at Object.charge (src/retry.js:12:9)",
            "{\"error\": \"the key must be unique\"}",
            "  \"error\": \"amount must be positive\",",
            "// must not be called from the UI thread",
            "# the value must not be None",
            "if value is None:",
            "raise ValueError(\"must never be empty\")",
            "fn charge(key: &Key) -> Result<()> {",
            "============================= test session starts ==",
            "tests/test_retry.py ..F.....                   [ 16%]",
        ] {
            assert!(machine_line(line), "{line}");
        }
        for line in [
            "Do not modify the tests.",
            "Never change the public API of the loader.",
            "- never change the public API",
            "    - keep the old reader",
            "Note: never push to main.",
            "Warning: do not run this against production.",
            "Error handling must stay in the gateway.",
            "return early if the list is empty and never throw",
            "if you must change it, keep the old name",
            "# Rules",
            "at least keep the old flag working",
            "Fix src/retry.py so the key survives a retry.",
            "The mount takes a 5\" pipe.",
            "ok, do not touch the parser",
            "[WIP] do not merge the branch",
            "let's not change the API",
        ] {
            assert!(!machine_line(line), "{line}");
        }
    }

    #[test]
    fn lead_ins_open_the_right_region() {
        assert_eq!(lead_in("Here is the output:"), Some(Region::Output));
        assert_eq!(lead_in("test output:"), Some(Region::Output));
        assert_eq!(lead_in("logs:"), Some(Region::Output));
        assert_eq!(lead_in("Here's the error I get"), Some(Region::Output));
        assert_eq!(lead_in("The runbook says:"), Some(Region::Quotation));
        assert_eq!(lead_in("From the Stripe docs:"), Some(Region::Quotation));
        assert_eq!(lead_in("Rules for the log output:"), None);
        assert_eq!(lead_in("Do not change these files:"), None);
        assert_eq!(lead_in("Constraints:"), None);
        assert_eq!(lead_in("The output is wrong in two places."), None);
    }

    #[test]
    fn an_output_region_ends_where_the_output_does() {
        let text = "Here is the output:\n\nwhatever the tool printed\nexit status 1\n\n\
                    E   more output\n\nPlease fix it. Do not modify the tests.";
        let k = kinds(text);
        assert_eq!(k[0], Line::Prose);
        assert_eq!(k[2], Line::Output);
        assert_eq!(k[3], Line::Output);
        assert_eq!(k[5], Line::Output);
        assert_eq!(k[7], Line::Prose);
    }

    #[test]
    fn a_quotation_region_runs_to_the_end() {
        let text = "The runbook says:\n\n1. You must drain the node.\n\nNever restart it.";
        let k = kinds(text);
        assert_eq!(k[0], Line::Prose);
        assert_eq!(k[2], Line::Quoted);
        assert_eq!(k[4], Line::Quoted);
    }

    #[test]
    fn a_mostly_output_paragraph_is_output_and_a_mixed_one_is_not() {
        let k = kinds("E   one\nE   two\nand a line in between");
        assert!(k.iter().all(|k| *k == Line::Output), "{k:?}");
        let k = kinds("Do not modify the tests.\nFAILED tests/x.py::t - boom");
        assert_eq!(k, vec![Line::Prose, Line::Output]);
    }

    #[test]
    fn an_indented_block_is_code_but_an_indented_wrap_is_not() {
        let k = kinds("Why?\n\n    value = must_not_be_none()\n    other line\n\nok");
        assert_eq!(k[2], Line::Output);
        assert_eq!(k[3], Line::Output);
        assert_eq!(k[5], Line::Prose);
        let k = kinds("Always keep\n\tthe loop working.");
        assert_eq!(k, vec![Line::Prose, Line::Prose]);
        let k = kinds("Rules:\n- never change X\n    - keep the old reader");
        assert!(k.iter().all(|k| *k == Line::Prose), "{k:?}");
    }

    #[test]
    fn fences_and_blockquotes() {
        let k = kinds("Why?\n```\n# never\n```\n> Do not retry.\nFix it.");
        assert_eq!(
            k,
            vec![
                Line::Prose,
                Line::Fenced,
                Line::Fenced,
                Line::Fenced,
                Line::Quoted,
                Line::Prose
            ]
        );
    }

    proptest::proptest! {
        /// Fail-open: classification never panics and gives one kind per line.
        #[test]
        fn classifying_never_panics(
            raw in "(\u{e9}|\u{1f4b3}|```|~~~|> |E   |    |\t|here is the output:|logs:|says:|\\$ |# |// |\\{|\\}|\"k\": |[a-z]{1,5}|[0-9]{1,4}|:|\\.|\\(|\\)|\\||-->|\n|\n\n|\r\n){0,40}"
        ) {
            let k = classify(&raw);
            proptest::prop_assert_eq!(k.len(), raw.lines().count());
        }
    }

    #[test]
    fn classifying_is_linear_on_a_large_paste() {
        let big = "E   AssertionError: must be positive\n".repeat(200_000);
        let started = std::time::Instant::now();
        let k = classify(&big);
        assert!(k.iter().all(|k| *k == Line::Output));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
