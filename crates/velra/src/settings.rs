//! Zero-touch configuration of Claude Code settings (§6).
//!
//! Edits are minimal text splices computed from `jsonc-parser` byte ranges,
//! so comments, trailing commas, indentation and every non-Velra byte survive
//! untouched, and `enable` → `disable` restores the original file byte for
//! byte (see DECISIONS.md for why the CST mutation API is not used).

use crate::compat::Features;
use jsonc_parser::ast::{Object, ObjectProp, Value};
use jsonc_parser::common::Ranged;
use jsonc_parser::{parse_to_ast, CollectOptions, ParseOptions};
use serde_json::{json, Map, Value as JsonValue};
use std::path::{Path, PathBuf};

/// One hook registration from the table in §6.3.
pub struct Registration {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub if_rule: Option<&'static str>,
    /// Argument vector after the binary, e.g. `["hook", "session-start"]`.
    pub args: &'static [&'static str],
    pub is_async: bool,
    pub timeout: u64,
    /// Whether the detected Claude Code version supports this registration.
    pub supported: fn(&Features) -> bool,
}

pub const REGISTRATIONS: &[Registration] = &[
    Registration {
        event: "SessionStart",
        matcher: None,
        if_rule: None,
        args: &["hook", "session-start"],
        is_async: false,
        timeout: 10,
        supported: |f| f.session_start,
    },
    Registration {
        event: "UserPromptSubmit",
        matcher: None,
        if_rule: None,
        args: &["hook", "user-prompt-submit"],
        is_async: false,
        timeout: 10,
        supported: |_| true,
    },
    Registration {
        event: "PreToolUse",
        matcher: Some("Write|Edit|MultiEdit|NotebookEdit"),
        if_rule: None,
        args: &["hook", "pre-tool-use"],
        is_async: false,
        timeout: 10,
        supported: |_| true,
    },
    // §6.3 pairs these with `if` rules of `Bash(git *)` / `PowerShell(git *)`.
    // Those rules are prefix matches, so they see `git restore src/a.py` but
    // not `cd "/path" && git restore src/a.py`, which is how an agent phrases
    // the same command whenever it needs a working directory first. A revert
    // missed here has no `git_pre` snapshot, and the capsule then reports the
    // discard as "changed outside the agent" with no command text — observed
    // in the v0.1 benchmark. There is no rule syntax for "git anywhere in the
    // command line", so the filter moves into the binary: `pre-tool-use`
    // parses the command itself and returns before opening the database when
    // there is no git subcommand in it (D58).
    Registration {
        event: "PreToolUse",
        matcher: Some("Bash"),
        if_rule: None,
        args: &["hook", "pre-tool-use"],
        is_async: false,
        timeout: 10,
        supported: |_| true,
    },
    Registration {
        event: "PreToolUse",
        matcher: Some("PowerShell"),
        if_rule: None,
        args: &["hook", "pre-tool-use"],
        is_async: false,
        timeout: 10,
        supported: |f| f.powershell_tool,
    },
    Registration {
        event: "PostToolUse",
        matcher: Some("*"),
        if_rule: None,
        args: &["hook", "post-tool-use"],
        is_async: false,
        timeout: 10,
        supported: |_| true,
    },
    Registration {
        event: "PostToolUseFailure",
        matcher: Some("*"),
        if_rule: None,
        args: &["hook", "post-tool-use-failure"],
        is_async: false,
        timeout: 10,
        supported: |f| f.post_tool_use_failure,
    },
    Registration {
        event: "PostToolBatch",
        matcher: None,
        if_rule: None,
        args: &["reduce"],
        is_async: true,
        timeout: 30,
        supported: |f| f.post_tool_batch && f.async_hooks,
    },
    Registration {
        event: "Stop",
        matcher: None,
        if_rule: None,
        args: &["hook", "stop"],
        is_async: false,
        timeout: 10,
        supported: |_| true,
    },
    Registration {
        event: "Stop",
        matcher: None,
        if_rule: None,
        args: &["reduce"],
        is_async: true,
        timeout: 30,
        supported: |f| f.async_hooks,
    },
    Registration {
        event: "PreCompact",
        matcher: None,
        if_rule: None,
        args: &["hook", "pre-compact"],
        is_async: false,
        timeout: 10,
        supported: |f| f.pre_compact,
    },
    Registration {
        event: "PostCompact",
        matcher: None,
        if_rule: None,
        args: &["hook", "post-compact"],
        is_async: true,
        timeout: 30,
        supported: |f| f.post_compact && f.async_hooks,
    },
    Registration {
        event: "SessionEnd",
        matcher: None,
        if_rule: None,
        args: &["hook", "session-end"],
        is_async: false,
        timeout: 1,
        supported: |f| f.session_end,
    },
];

#[derive(Debug)]
pub enum SettingsError {
    Parse {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
    NotAnObject(PathBuf),
    Verify(String),
    Concurrent,
    Io(std::io::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Parse {
                path,
                line,
                column,
                message,
            } => write!(
                f,
                "Could not parse {} at line {line}, column {column}: {message}. No changes made.",
                path.display()
            ),
            SettingsError::NotAnObject(p) => write!(
                f,
                "{} does not contain a JSON object. No changes made.",
                p.display()
            ),
            SettingsError::Verify(m) => write!(f, "refusing to write: {m}"),
            SettingsError::Concurrent => write!(
                f,
                "the settings file was modified concurrently; no changes made"
            ),
            SettingsError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for SettingsError {
    fn from(e: std::io::Error) -> Self {
        SettingsError::Io(e)
    }
}

type Result<T> = std::result::Result<T, SettingsError>;

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json` (§6.1).
pub fn settings_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("settings.json"));
    }
    crate::home::user_home().map(|h| h.join(".claude").join("settings.json"))
}

/// Follows a symlinked settings file so dotfile managers keep working (§6.1).
pub fn resolve_target(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

fn parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        ..Default::default()
    }
}

fn parse<'a>(text: &'a str, path: &Path) -> Result<Option<Value<'a>>> {
    match parse_to_ast(text, &CollectOptions::default(), &parse_options()) {
        Ok(result) => Ok(result.value),
        Err(e) => Err(SettingsError::Parse {
            path: path.to_path_buf(),
            line: e.line_display(),
            column: e.column_display(),
            message: e.kind().to_string(),
        }),
    }
}

// ---------------------------------------------------------------- formatting

/// Indentation unit detected from the file (2 spaces, 4 spaces or a tab).
fn detect_indent(text: &str) -> String {
    for line in text.lines() {
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if ws.is_empty() || ws.len() == line.len() {
            continue;
        }
        if ws.starts_with('\t') {
            return "\t".to_string();
        }
        return " ".repeat(ws.len().min(8));
    }
    "  ".to_string()
}

fn detect_newline(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn line_indent(text: &str, pos: usize) -> String {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    text[start..pos]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Pretty-prints JSON with the file's own indentation and newline style.
fn render_json(value: &JsonValue, indent: &str, base: &str, newline: &str) -> String {
    fn go(v: &JsonValue, indent: &str, base: &str, depth: usize, newline: &str, out: &mut String) {
        let pad = |n: usize| format!("{base}{}", indent.repeat(n));
        match v {
            JsonValue::Object(map) if !map.is_empty() => {
                out.push('{');
                for (i, (k, val)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(newline);
                    out.push_str(&pad(depth + 1));
                    out.push_str(&JsonValue::String(k.clone()).to_string());
                    out.push_str(": ");
                    go(val, indent, base, depth + 1, newline, out);
                }
                out.push_str(newline);
                out.push_str(&pad(depth));
                out.push('}');
            }
            JsonValue::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(newline);
                    out.push_str(&pad(depth + 1));
                    go(item, indent, base, depth + 1, newline, out);
                }
                out.push_str(newline);
                out.push_str(&pad(depth));
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    go(value, indent, base, 0, newline, &mut out);
    out
}

// ------------------------------------------------------------- text splicing

/// True when the text between `from` and `to` contains a comment.
fn gap_has_comment(text: &str, from: usize, to: usize) -> bool {
    let gap = &text[from..to];
    gap.contains("//") || gap.contains("/*")
}

/// Position of the separating comma in a gap that contains only whitespace,
/// commas and comments.
fn find_comma(text: &str, from: usize, to: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < to {
        match bytes[i] {
            b',' => return Some(i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < to && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < to && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn splice(text: &str, mut ranges: Vec<(usize, usize, String)>) -> String {
    ranges.sort_by_key(|(start, ..)| std::cmp::Reverse(*start));
    let mut out = text.to_string();
    for (start, end, replacement) in ranges {
        out.replace_range(start..end, &replacement);
    }
    out
}

/// Inserts `value_text` as the last entry of a container, matching the file's
/// existing separator style (including a trailing comma when present).
// Each parameter is a distinct piece of the splice; grouping them would only
// move the noise.
#[allow(clippy::too_many_arguments)]
fn insert_into_container(
    text: &str,
    open: usize,
    close: usize,
    last_child_end: Option<usize>,
    value_text: &str,
    child_indent: &str,
    base_indent: &str,
    newline: &str,
) -> (usize, usize, String) {
    match last_child_end {
        Some(end) => {
            let trailing_comma = find_comma(text, end, close);
            match trailing_comma {
                Some(comma) => (
                    comma + 1,
                    comma + 1,
                    format!("{newline}{child_indent}{value_text},"),
                ),
                None => (end, end, format!(",{newline}{child_indent}{value_text}")),
            }
        }
        None => (
            open + 1,
            close,
            format!("{newline}{child_indent}{value_text}{newline}{base_indent}"),
        ),
    }
}

/// Removal range(s) for one child of a container, preserving neighbouring
/// comments and restoring `[]` / `{}` when the last child goes.
fn removal_ranges(
    text: &str,
    open: usize,
    close: usize,
    children: &[(usize, usize)],
    index: usize,
) -> Vec<(usize, usize, String)> {
    let (start, end) = children[index];
    if children.len() == 1 {
        if gap_has_comment(text, open + 1, close) {
            let mut ranges = vec![(start, end, String::new())];
            if let Some(comma) = find_comma(text, end, close) {
                ranges.push((comma, comma + 1, String::new()));
            }
            return ranges;
        }
        return vec![(open + 1, close, String::new())];
    }
    if index > 0 {
        let prev_end = children[index - 1].1;
        if !gap_has_comment(text, prev_end, start) {
            return vec![(prev_end, end, String::new())];
        }
        let mut ranges = vec![(start, end, String::new())];
        if let Some(comma) = find_comma(text, prev_end, start) {
            ranges.push((comma, comma + 1, String::new()));
        }
        return ranges;
    }
    let next_start = children[index + 1].0;
    if !gap_has_comment(text, end, next_start) {
        return vec![(start, next_start, String::new())];
    }
    let mut ranges = vec![(start, end, String::new())];
    if let Some(comma) = find_comma(text, end, next_start) {
        ranges.push((comma, comma + 1, String::new()));
    }
    ranges
}

// ---------------------------------------------------------- handler identity

fn exe_is_velra(command: &str) -> bool {
    velra_core::shell::exe_basename(command) == "velra"
}

/// The Velra role of a handler object: its argument vector, e.g.
/// `["hook", "post-tool-use"]`.
fn velra_role(handler: &Object<'_>) -> Option<Vec<String>> {
    let kind = handler.get_string("type").map(|s| s.value.to_string());
    if kind.is_some_and(|k| k != "command") {
        return None;
    }
    let command = handler.get_string("command")?.value.to_string();
    match handler.get_array("args") {
        Some(args) => {
            if !exe_is_velra(&command) {
                return None;
            }
            let values: Vec<String> = args
                .elements
                .iter()
                .filter_map(|e| e.as_string_lit().map(|s| s.value.to_string()))
                .collect();
            matches!(
                values.first().map(String::as_str),
                Some("hook") | Some("reduce")
            )
            .then_some(values)
        }
        None => {
            let words = velra_core::shell::tokenize(&command);
            let first = words.first()?;
            if !exe_is_velra(first) {
                return None;
            }
            let rest: Vec<String> = words[1..].to_vec();
            matches!(
                rest.first().map(String::as_str),
                Some("hook") | Some("reduce")
            )
            .then_some(rest)
        }
    }
}

fn matcher_of(group: &Object<'_>) -> Option<String> {
    group.get_string("matcher").map(|s| s.value.to_string())
}

/// The desired handler JSON for a registration (§6.3).
fn handler_value(bin: &str, reg: &Registration, features: &Features) -> JsonValue {
    let mut map = Map::new();
    map.insert("type".into(), json!("command"));
    if features.exec_form {
        map.insert("command".into(), json!(bin));
        map.insert("args".into(), json!(reg.args));
    } else {
        // Shell form: quote the absolute path; forward slashes work in Git
        // Bash on Windows, which is what Claude Code uses there.
        let display = if cfg!(windows) {
            bin.replace('\\', "/")
        } else {
            bin.to_string()
        };
        map.insert(
            "command".into(),
            json!(format!("\"{display}\" {}", reg.args.join(" "))),
        );
    }
    if let Some(rule) = reg.if_rule.filter(|_| features.if_field) {
        map.insert("if".into(), json!(rule));
    }
    if reg.is_async {
        map.insert("async".into(), json!(true));
    }
    map.insert("timeout".into(), json!(reg.timeout));
    JsonValue::Object(map)
}

fn group_value(bin: &str, reg: &Registration, features: &Features) -> JsonValue {
    let mut map = Map::new();
    if let Some(m) = reg.matcher {
        map.insert("matcher".into(), json!(m));
    }
    map.insert("hooks".into(), json!([handler_value(bin, reg, features)]));
    JsonValue::Object(map)
}

/// What one `apply` pass changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub skipped: Vec<String>,
}

fn root_object<'a>(value: &'a Option<Value<'a>>, path: &Path) -> Result<&'a Object<'a>> {
    match value {
        Some(Value::Object(o)) => Ok(o),
        _ => Err(SettingsError::NotAnObject(path.to_path_buf())),
    }
}

fn prop<'a>(obj: &'a Object<'a>, name: &str) -> Option<&'a ObjectProp<'a>> {
    obj.get(name)
}

/// Ensures `hooks.<event>` exists, returning the updated text.
fn ensure_event_array(
    text: &str,
    event: &str,
    indent: &str,
    newline: &str,
    path: &Path,
) -> Result<String> {
    let parsed = parse(text, path)?;
    let root = root_object(&parsed, path)?;
    let hooks = prop(root, "hooks").and_then(|p| p.value.as_object());
    let Some(hooks) = hooks else {
        // Add a `hooks` object with the event array inside.
        let base = line_indent(text, root.range.start);
        let child = format!("{base}{indent}");
        let value = render_json(&json!({ event: [] }), indent, &child, newline);
        // `{ "hooks": { "<event>": [] } }` rendered at the right depth.
        let inner = value.trim_start_matches('{').trim_end_matches('}');
        let text_value = format!("\"hooks\": {{{inner}}}");
        let children: Vec<(usize, usize)> = root
            .properties
            .iter()
            .map(|p| (p.range.start, p.range.end))
            .collect();
        let (start, end, replacement) = insert_into_container(
            text,
            root.range.start,
            root.range.end - 1,
            children.last().map(|(_, e)| *e),
            &text_value,
            &child,
            &base,
            newline,
        );
        return Ok(splice(text, vec![(start, end, replacement)]));
    };
    if prop(hooks, event).is_some() {
        return Ok(text.to_string());
    }
    let base = line_indent(text, hooks.range.start);
    let child = format!("{base}{indent}");
    let value_text = format!("\"{event}\": []");
    let children: Vec<(usize, usize)> = hooks
        .properties
        .iter()
        .map(|p| (p.range.start, p.range.end))
        .collect();
    let (start, end, replacement) = insert_into_container(
        text,
        hooks.range.start,
        hooks.range.end - 1,
        children.last().map(|(_, e)| *e),
        &value_text,
        &child,
        &base,
        newline,
    );
    Ok(splice(text, vec![(start, end, replacement)]))
}

/// Adds or updates the handler for one registration.
#[allow(clippy::too_many_arguments)]
fn upsert(
    text: &str,
    bin: &str,
    reg: &Registration,
    features: &Features,
    indent: &str,
    newline: &str,
    path: &Path,
    changes: &mut Changes,
) -> Result<String> {
    let desired_handler = handler_value(bin, reg, features);
    // Look for an existing Velra handler with the same event + matcher + args.
    {
        let parsed = parse(text, path)?;
        let root = root_object(&parsed, path)?;
        if let Some(events) = prop(root, "hooks").and_then(|p| p.value.as_object()) {
            if let Some(array) = prop(events, reg.event).and_then(|p| p.value.as_array()) {
                for group in &array.elements {
                    let Some(group) = group.as_object() else {
                        continue;
                    };
                    if matcher_of(group).as_deref() != reg.matcher {
                        continue;
                    }
                    let Some(handlers) = prop(group, "hooks").and_then(|p| p.value.as_array())
                    else {
                        continue;
                    };
                    for handler in &handlers.elements {
                        let Some(handler_obj) = handler.as_object() else {
                            continue;
                        };
                        let Some(role) = velra_role(handler_obj) else {
                            continue;
                        };
                        if role != reg.args {
                            continue;
                        }
                        let current: JsonValue = serde_json::from_str(
                            &text[handler_obj.range.start..handler_obj.range.end],
                        )
                        .unwrap_or(JsonValue::Null);
                        if current == desired_handler {
                            return Ok(text.to_string());
                        }
                        let base = line_indent(text, handler_obj.range.start);
                        let rendered = render_json(&desired_handler, indent, &base, newline);
                        changes.updated += 1;
                        return Ok(splice(
                            text,
                            vec![(handler_obj.range.start, handler_obj.range.end, rendered)],
                        ));
                    }
                }
            }
        }
    }
    // Append a new matcher group at the end of hooks.<event>.
    let text = ensure_event_array(text, reg.event, indent, newline, path)?;
    let parsed = parse(&text, path)?;
    let root = root_object(&parsed, path)?;
    let events = prop(root, "hooks")
        .and_then(|p| p.value.as_object())
        .ok_or_else(|| SettingsError::Verify("hooks missing".into()))?;
    let array = prop(events, reg.event)
        .and_then(|p| p.value.as_array())
        .ok_or_else(|| SettingsError::Verify(format!("hooks.{} missing", reg.event)))?;
    let base = line_indent(&text, array.range.start);
    let child = format!("{base}{indent}");
    let value_text = render_json(&group_value(bin, reg, features), indent, &child, newline);
    let children: Vec<(usize, usize)> = array
        .elements
        .iter()
        .map(|e| (e.range().start, e.range().end))
        .collect();
    let (start, end, replacement) = insert_into_container(
        &text,
        array.range.start,
        array.range.end - 1,
        children.last().map(|(_, e)| *e),
        &value_text,
        &child,
        &base,
        newline,
    );
    changes.added += 1;
    Ok(splice(&text, vec![(start, end, replacement)]))
}

/// A registration to keep during removal: `(event, matcher, args)`.
type KeepSpec = (&'static str, Option<&'static str>, &'static [&'static str]);

/// One removal pass: drops the first Velra handler matching `keep`'s
/// complement, collapsing empty groups, arrays and the `hooks` object.
fn remove_one(
    text: &str,
    keep: Option<&[KeepSpec]>,
    remove_hooks_key: bool,
    path: &Path,
) -> Result<Option<String>> {
    let parsed = parse(text, path)?;
    let root = root_object(&parsed, path)?;
    let Some(hooks_prop) = prop(root, "hooks") else {
        return Ok(None);
    };
    let Some(events) = hooks_prop.value.as_object() else {
        return Ok(None);
    };
    for event_prop in &events.properties {
        let event_name = event_prop.name.as_str().to_string();
        let Some(array) = event_prop.value.as_array() else {
            continue;
        };
        for (group_index, group) in array.elements.iter().enumerate() {
            let Some(group_obj) = group.as_object() else {
                continue;
            };
            let matcher = matcher_of(group_obj);
            let Some(handlers_prop) = prop(group_obj, "hooks") else {
                continue;
            };
            let Some(handlers) = handlers_prop.value.as_array() else {
                continue;
            };
            for (handler_index, handler) in handlers.elements.iter().enumerate() {
                let Some(handler_obj) = handler.as_object() else {
                    continue;
                };
                let Some(role) = velra_role(handler_obj) else {
                    continue;
                };
                if let Some(keep) = keep {
                    let wanted = keep.iter().any(|(event, m, args)| {
                        *event == event_name && m.map(str::to_string) == matcher && role == *args
                    });
                    if wanted {
                        continue;
                    }
                }
                // Remove the handler; collapse containers that become empty.
                if handlers.elements.len() > 1 {
                    let children: Vec<(usize, usize)> = handlers
                        .elements
                        .iter()
                        .map(|e| (e.range().start, e.range().end))
                        .collect();
                    let ranges = removal_ranges(
                        text,
                        handlers.range.start,
                        handlers.range.end - 1,
                        &children,
                        handler_index,
                    );
                    return Ok(Some(splice(text, ranges)));
                }
                if array.elements.len() > 1 {
                    let children: Vec<(usize, usize)> = array
                        .elements
                        .iter()
                        .map(|e| (e.range().start, e.range().end))
                        .collect();
                    let ranges = removal_ranges(
                        text,
                        array.range.start,
                        array.range.end - 1,
                        &children,
                        group_index,
                    );
                    return Ok(Some(splice(text, ranges)));
                }
                if events.properties.len() > 1 || !remove_hooks_key {
                    let children: Vec<(usize, usize)> = events
                        .properties
                        .iter()
                        .map(|p| (p.range.start, p.range.end))
                        .collect();
                    let index = events
                        .properties
                        .iter()
                        .position(|p| p.name.as_str() == event_name)
                        .unwrap_or(0);
                    let ranges = removal_ranges(
                        text,
                        events.range.start,
                        events.range.end - 1,
                        &children,
                        index,
                    );
                    return Ok(Some(splice(text, ranges)));
                }
                let children: Vec<(usize, usize)> = root
                    .properties
                    .iter()
                    .map(|p| (p.range.start, p.range.end))
                    .collect();
                let index = root
                    .properties
                    .iter()
                    .position(|p| p.name.as_str() == "hooks")
                    .unwrap_or(0);
                let ranges =
                    removal_ranges(text, root.range.start, root.range.end - 1, &children, index);
                return Ok(Some(splice(text, ranges)));
            }
        }
    }
    Ok(None)
}

/// Applies every supported registration, then removes stale Velra handlers.
pub fn apply_enable(
    original: &str,
    bin: &str,
    features: &Features,
    path: &Path,
) -> Result<(String, Changes)> {
    let indent = detect_indent(original);
    let newline = detect_newline(original);
    let mut text = original.to_string();
    let mut changes = Changes::default();
    let mut keep: Vec<(&'static str, Option<&'static str>, &'static [&'static str])> = Vec::new();
    for reg in REGISTRATIONS {
        if !(reg.supported)(features) {
            changes
                .skipped
                .push(format!("{} ({})", reg.event, reg.args.join(" ")));
            continue;
        }
        text = upsert(
            &text,
            bin,
            reg,
            features,
            &indent,
            newline,
            path,
            &mut changes,
        )?;
        keep.push((reg.event, reg.matcher, reg.args));
    }
    // Stale Velra handlers (older matchers, unsupported events) are removed.
    while let Some(updated) = remove_one(&text, Some(&keep), false, path)? {
        text = updated;
        changes.removed += 1;
    }
    Ok((text, changes))
}

/// Removes every Velra handler (§6.4).
pub fn apply_disable(
    original: &str,
    remove_hooks_key: bool,
    path: &Path,
) -> Result<(String, Changes)> {
    let mut text = original.to_string();
    let mut changes = Changes::default();
    while let Some(updated) = remove_one(&text, None, remove_hooks_key, path)? {
        text = updated;
        changes.removed += 1;
    }
    Ok((text, changes))
}

// ------------------------------------------------------------- verification

/// Drops Velra handlers and any container that ends up empty, so two files
/// can be compared for "everything except Velra" equality (§6.2 step 7).
fn strip_velra(mut value: JsonValue) -> JsonValue {
    let Some(root) = value.as_object_mut() else {
        return value;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return value;
    };
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(array) = hooks.get_mut(&event).and_then(|a| a.as_array_mut()) else {
            continue;
        };
        array.retain_mut(|group| {
            let Some(group_obj) = group.as_object_mut() else {
                return true;
            };
            if let Some(handlers) = group_obj.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                handlers.retain(|h| !is_velra_json(h));
                if handlers.is_empty() {
                    return false;
                }
            }
            true
        });
        if array.is_empty() {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    value
}

fn is_velra_json(handler: &JsonValue) -> bool {
    let Some(obj) = handler.as_object() else {
        return false;
    };
    if obj
        .get("type")
        .and_then(JsonValue::as_str)
        .is_some_and(|t| t != "command")
    {
        return false;
    }
    let Some(command) = obj.get("command").and_then(JsonValue::as_str) else {
        return false;
    };
    match obj.get("args").and_then(JsonValue::as_array) {
        Some(args) => {
            exe_is_velra(command)
                && args
                    .first()
                    .and_then(JsonValue::as_str)
                    .is_some_and(|a| a == "hook" || a == "reduce")
        }
        None => {
            let words = velra_core::shell::tokenize(command);
            words.first().is_some_and(|w| exe_is_velra(w))
                && words.get(1).is_some_and(|w| w == "hook" || w == "reduce")
        }
    }
}

/// Parses both texts and verifies everything except Velra handlers matches.
pub fn verify_semantics(original: &str, updated: &str) -> Result<()> {
    let opts = parse_options();
    let a: JsonValue = jsonc_parser::parse_to_serde_value::<Option<JsonValue>>(original, &opts)
        .map_err(|e| SettingsError::Verify(format!("original unparseable: {e}")))?
        .unwrap_or(JsonValue::Null);
    let b: JsonValue = jsonc_parser::parse_to_serde_value::<Option<JsonValue>>(updated, &opts)
        .map_err(|e| SettingsError::Verify(format!("result unparseable: {e}")))?
        .unwrap_or(JsonValue::Null);
    if strip_velra(a) == strip_velra(b) {
        Ok(())
    } else {
        Err(SettingsError::Verify(
            "non-Velra settings would change".into(),
        ))
    }
}

/// True when the file globally disables hooks (§6.2 step 9).
pub fn disable_all_hooks(text: &str) -> bool {
    jsonc_parser::parse_to_serde_value::<Option<JsonValue>>(text, &parse_options())
        .ok()
        .flatten()
        .and_then(|v| v.get("disableAllHooks").and_then(JsonValue::as_bool))
        .unwrap_or(false)
}

/// Whether the file already has a `hooks` key (recorded for `disable`).
pub fn has_hooks_key(text: &str) -> bool {
    jsonc_parser::parse_to_serde_value::<Option<JsonValue>>(text, &parse_options())
        .ok()
        .flatten()
        .is_some_and(|v| v.get("hooks").is_some())
}

/// Unified diff for `--dry-run`.
pub fn diff(original: &str, updated: &str, path: &Path) -> String {
    let name = path.display().to_string();
    similar::TextDiff::from_lines(original, updated)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string()
}

// ------------------------------------------------------------------ file I/O

/// Result of enabling or disabling.
#[derive(Debug, Default)]
pub struct Outcome {
    pub changed: bool,
    pub changes: Changes,
    pub backup: Option<PathBuf>,
    pub diff: Option<String>,
    pub created_file: bool,
    pub hooks_existed_before: bool,
    pub disable_all_hooks: bool,
    pub target: PathBuf,
}

fn read_or_empty(path: &Path) -> std::io::Result<(String, bool)> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok((t, false)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), true)),
        Err(e) => Err(e),
    }
}

fn backup(home: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let dir = crate::home::backups_dir(home);
    crate::home::ensure_dir(&dir)?;
    let stamp = velra_core::time::compact_utc(velra_core::time::now_ms());
    let base = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".into());
    let mut path = dir.join(format!("{base}.{stamp}.bak"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{base}.{stamp}-{n}.bak"));
        n += 1;
    }
    crate::atomic::write_synced(&path, bytes)?;
    prune_backups(&dir, &base, 10);
    Ok(path)
}

fn prune_backups(dir: &Path, base: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(base))
        })
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for path in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// Applies an edit to the settings file with backup, verification, atomic
/// replacement and concurrent-modification retry (§6.2 steps 2–8).
fn write_change(
    target: &Path,
    home: &Path,
    original: &str,
    updated: &str,
    created_file: bool,
) -> Result<PathBuf> {
    verify_semantics(if created_file { "{}" } else { original }, updated)?;
    let backup_path = backup(home, target, original.as_bytes())?;
    if !created_file {
        let current = std::fs::read_to_string(target).unwrap_or_default();
        if current != original {
            return Err(SettingsError::Concurrent);
        }
    }
    crate::atomic::write(target, updated.as_bytes())?;
    Ok(backup_path)
}

/// `velra enable` (§6.2).
pub fn enable(
    settings: &Path,
    home: &Path,
    bin: &str,
    features: &Features,
    dry_run: bool,
) -> Result<Outcome> {
    let target = resolve_target(settings);
    for attempt in 0..3 {
        let (original, missing) = read_or_empty(&target)?;
        let base = if missing || original.trim().is_empty() {
            "{}\n".to_string()
        } else {
            original.clone()
        };
        let (updated, changes) = apply_enable(&base, bin, features, &target)?;
        let mut outcome = Outcome {
            changed: updated != base || missing,
            changes,
            created_file: missing,
            hooks_existed_before: has_hooks_key(&base),
            disable_all_hooks: disable_all_hooks(&updated),
            target: target.clone(),
            ..Default::default()
        };
        if dry_run {
            outcome.diff = Some(diff(&base, &updated, &target));
            return Ok(outcome);
        }
        if !outcome.changed {
            return Ok(outcome);
        }
        match write_change(&target, home, &base, &updated, missing) {
            Ok(path) => {
                outcome.backup = Some(path);
                return Ok(outcome);
            }
            Err(SettingsError::Concurrent) if attempt < 2 => continue,
            Err(e) => return Err(e),
        }
    }
    Err(SettingsError::Concurrent)
}

/// `velra disable` (§6.4).
pub fn disable(
    settings: &Path,
    home: &Path,
    remove_hooks_key: bool,
    dry_run: bool,
) -> Result<Outcome> {
    let target = resolve_target(settings);
    for attempt in 0..3 {
        let (original, missing) = read_or_empty(&target)?;
        if missing {
            return Ok(Outcome {
                target,
                ..Default::default()
            });
        }
        let (updated, changes) = apply_disable(&original, remove_hooks_key, &target)?;
        let mut outcome = Outcome {
            changed: updated != original,
            changes,
            hooks_existed_before: has_hooks_key(&original),
            target: target.clone(),
            ..Default::default()
        };
        if dry_run {
            outcome.diff = Some(diff(&original, &updated, &target));
            return Ok(outcome);
        }
        if !outcome.changed {
            return Ok(outcome);
        }
        match write_change(&target, home, &original, &updated, false) {
            Ok(path) => {
                outcome.backup = Some(path);
                return Ok(outcome);
            }
            Err(SettingsError::Concurrent) if attempt < 2 => continue,
            Err(e) => return Err(e),
        }
    }
    Err(SettingsError::Concurrent)
}

/// A registered Velra handler: `(event, argument vector, command string)`.
pub type InstalledHandler = (String, Vec<String>, Option<String>);

/// Velra handlers currently registered in a settings file.
pub fn installed_handlers(text: &str) -> Vec<InstalledHandler> {
    let Ok(result) = parse_to_ast(text, &CollectOptions::default(), &parse_options()) else {
        return Vec::new();
    };
    let Some(Value::Object(root)) = result.value else {
        return Vec::new();
    };
    let Some(events) = root.get("hooks").and_then(|p| p.value.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for event_prop in &events.properties {
        let Some(array) = event_prop.value.as_array() else {
            continue;
        };
        for group in &array.elements {
            let Some(group_obj) = group.as_object() else {
                continue;
            };
            let Some(handlers) = group_obj.get("hooks").and_then(|p| p.value.as_array()) else {
                continue;
            };
            for handler in &handlers.elements {
                let Some(handler_obj) = handler.as_object() else {
                    continue;
                };
                let Some(role) = velra_role(handler_obj) else {
                    continue;
                };
                let command = handler_obj
                    .get_string("command")
                    .map(|s| s.value.to_string());
                out.push((event_prop.name.as_str().to_string(), role, command));
            }
        }
    }
    out
}
