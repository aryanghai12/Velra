//! Claude Code hook input → normalized, redacted payloads (§9).
//!
//! Parsing is tolerant by construction: every scalar field accepts any JSON
//! shape and degrades to `None` rather than failing the event, and
//! `tool_input` / `tool_response` stay as raw JSON until a field is needed.

use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use velra_core::event::{limits, Payload};
use velra_core::{paths, redact, text};

/// Accepts any JSON value for a string field; numbers and booleans are kept
/// as their literal text, `null` and objects/arrays become `None`.
fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let raw: Option<&RawValue> = Option::deserialize(d)?;
    Ok(raw.and_then(|r| {
        let t = r.get().trim();
        match t.as_bytes().first() {
            None => None,
            Some(b'"') => serde_json::from_str::<String>(t).ok(),
            Some(b'{') | Some(b'[') => None,
            _ if t == "null" => None,
            _ => Some(t.to_string()),
        }
    }))
}

fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    let raw: Option<&RawValue> = Option::deserialize(d)?;
    Ok(raw.and_then(|r| match r.get().trim() {
        "true" => Some(true),
        "false" => Some(false),
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }))
}

fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let raw: Option<&RawValue> = Option::deserialize(d)?;
    Ok(raw.and_then(|r| {
        let t = r.get().trim().trim_matches('"');
        t.parse::<i64>()
            .ok()
            .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
    }))
}

/// Common + event-specific hook input (§8.2, §8.3).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookInput<'a> {
    #[serde(deserialize_with = "lenient_string")]
    pub session_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub hook_event_name: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub cwd: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub transcript_path: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub prompt_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub agent_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub agent_type: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub prompt: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub source: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub model: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub reason: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub trigger: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub compact_summary: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub summary: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub tool_name: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub tool_use_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub error: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub error_message: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub error_type: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub stop_hook_active: Option<bool>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub is_interrupt: Option<bool>,
    #[serde(borrow, default)]
    pub custom_instructions: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub tool_input: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub tool_response: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub tool_output: Option<&'a RawValue>,
}

impl HookInput<'_> {
    pub fn error_text(&self) -> Option<&str> {
        self.error.as_deref().or(self.error_message.as_deref())
    }

    pub fn summary_text(&self) -> Option<&str> {
        self.compact_summary.as_deref().or(self.summary.as_deref())
    }

    pub fn has_custom_instructions(&self) -> bool {
        self.custom_instructions.is_some_and(|r| {
            let t = r.get().trim();
            t != "null" && t != "\"\"" && !t.is_empty()
        })
    }
}

/// Parses hook input; `None` when the JSON itself is unusable.
pub fn parse(raw: &[u8]) -> Option<HookInput<'_>> {
    serde_json::from_slice(raw).ok()
}

/// Last-resort scan for `"session_id": "..."` in unparseable input, so a
/// malformed event can still be recorded (§18).
pub fn salvage_session_id(raw: &[u8]) -> Option<String> {
    let text = valid_prefix(raw)?;
    let at = text.find("\"session_id\"")?;
    let rest = &text[at + "\"session_id\"".len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let mut chars = after.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    for (_, c) in chars {
        match c {
            '"' => return (!out.is_empty()).then_some(out),
            '\\' => return (!out.is_empty()).then_some(out),
            _ => out.push(c),
        }
    }
    None
}

/// The valid UTF-8 prefix of `raw`. Input cut at a byte cap can end inside a
/// multi-byte character; requiring the whole buffer to be valid lost the
/// session id -- and with it the event -- whenever the cut fell inside one.
fn valid_prefix(raw: &[u8]) -> Option<&str> {
    match std::str::from_utf8(raw) {
        Ok(t) => Some(t),
        Err(e) => std::str::from_utf8(&raw[..e.valid_up_to()]).ok(),
    }
}

/// The first `"key": "value"` string in `raw`, decoded as JSON, read without
/// parsing the rest: for input the hook declines to parse at all. Escapes are
/// decoded (a Windows `cwd` is nothing but `\\`); a value that does not close
/// is `None`.
pub fn salvage_json_string(raw: &[u8], key: &str) -> Option<String> {
    let text = valid_prefix(raw)?;
    let quoted = format!("\"{key}\"");
    let at = text.find(&quoted)?;
    let rest = &text[at + quoted.len()..];
    let after = rest.trim_start().strip_prefix(':')?.trim_start();
    if !after.starts_with('"') {
        return None;
    }
    let b = after.as_bytes();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return serde_json::from_str(&after[..=i]).ok(),
            _ => i += 1,
        }
    }
    None
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EditItem {
    #[serde(deserialize_with = "lenient_string")]
    pub old_string: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub new_string: Option<String>,
}

/// The `tool_input` fields Velra consumes.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ToolInput {
    #[serde(deserialize_with = "lenient_string")]
    pub file_path: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub path: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub notebook_path: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub command: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub old_string: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub new_string: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub content: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub new_source: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub pattern: Option<String>,
    #[serde(deserialize_with = "lenient_i64")]
    pub offset: Option<i64>,
    #[serde(deserialize_with = "lenient_i64")]
    pub limit: Option<i64>,
    pub edits: Option<Vec<EditItem>>,
}

impl ToolInput {
    pub fn parse(raw: Option<&RawValue>) -> ToolInput {
        raw.and_then(|r| serde_json::from_str(r.get()).ok())
            .unwrap_or_default()
    }

    /// Target path for file tools (§8.3).
    pub fn target_path(&self) -> Option<&str> {
        self.file_path
            .as_deref()
            .or(self.path.as_deref())
            .or(self.notebook_path.as_deref())
    }

    /// Lines added / removed for an edit (§9.1).
    pub fn line_counts(&self, tool: &str) -> (Option<u32>, Option<u32>) {
        match tool {
            "Write" => (self.content.as_deref().map(text::line_count), None),
            "NotebookEdit" => (self.new_source.as_deref().map(text::line_count), None),
            "MultiEdit" => {
                let edits = self.edits.as_deref().unwrap_or(&[]);
                let sum = |f: fn(&EditItem) -> Option<&String>| -> u32 {
                    edits
                        .iter()
                        .filter_map(f)
                        .map(|s| text::line_count(s))
                        .sum()
                };
                (
                    Some(sum(|e| e.new_string.as_ref())),
                    Some(sum(|e| e.old_string.as_ref())),
                )
            }
            _ => (
                self.new_string.as_deref().map(text::line_count),
                self.old_string.as_deref().map(text::line_count),
            ),
        }
    }

    /// First changed `-`/`+` line pair (§9.1); `None` for whole-file writes.
    pub fn excerpt(&self, tool: &str) -> Option<String> {
        let (old, new) = match tool {
            "Write" | "NotebookEdit" => return None,
            "MultiEdit" => {
                let first = self.edits.as_deref().and_then(<[EditItem]>::first)?;
                (
                    first.old_string.as_deref()?,
                    first.new_string.as_deref().unwrap_or(""),
                )
            }
            _ => (
                self.old_string.as_deref()?,
                self.new_string.as_deref().unwrap_or(""),
            ),
        };
        diff_excerpt(old, new)
    }
}

/// `- {first changed old line}\n+ {first changed new line}`, each ≤ 160 chars.
pub fn diff_excerpt(old: &str, new: &str) -> Option<String> {
    let mut o = old.lines();
    let mut n = new.lines();
    loop {
        let (a, b) = (o.next(), n.next());
        match (a, b) {
            (Some(x), Some(y)) if x == y => continue,
            (None, None) => return None,
            (a, b) => {
                let mut out = String::new();
                if let Some(minus) = a.map(str::trim).filter(|s| !s.is_empty()) {
                    out.push_str("- ");
                    out.push_str(&text::truncate_chars(minus, limits::EXCERPT_LINE_CHARS));
                }
                if let Some(plus) = b.map(str::trim).filter(|s| !s.is_empty()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("+ ");
                    out.push_str(&text::truncate_chars(plus, limits::EXCERPT_LINE_CHARS));
                }
                return (!out.is_empty()).then_some(out);
            }
        }
    }
}

/// Shell tool response fields (§9.1).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ShellResponse {
    #[serde(deserialize_with = "lenient_string")]
    pub stdout: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub stderr: Option<String>,
    #[serde(deserialize_with = "lenient_bool")]
    pub interrupted: Option<bool>,
    #[serde(
        alias = "exitCode",
        alias = "returnCode",
        deserialize_with = "lenient_i64"
    )]
    pub exit_code: Option<i64>,
}

impl ShellResponse {
    pub fn parse(raw: Option<&RawValue>) -> ShellResponse {
        raw.and_then(|r| serde_json::from_str(r.get()).ok())
            .unwrap_or_default()
    }
}

/// File tool response fields.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileResponse {
    #[serde(rename = "originalFile", deserialize_with = "lenient_string")]
    pub original_file: Option<String>,
    #[serde(rename = "filePath", deserialize_with = "lenient_string")]
    pub file_path: Option<String>,
}

impl FileResponse {
    pub fn parse(raw: Option<&RawValue>) -> FileResponse {
        raw.and_then(|r| serde_json::from_str(r.get()).ok())
            .unwrap_or_default()
    }
}

/// Redacts, then truncates to `max_bytes` on a char boundary.
pub fn redact_capped(s: &str, max_bytes: usize) -> String {
    let redacted = redact::redact(s);
    text::prefix_bytes(&redacted, max_bytes).to_string()
}

/// Keeps the tail of long output: cut with a margin, redact, then cut again
/// so a secret spanning the first cut is still removed.
pub fn redact_tail(s: &str, max_bytes: usize) -> String {
    let window = text::suffix_bytes(s, max_bytes.saturating_add(1024));
    let cleaned = text::clean_terminal_output(window);
    let redacted = redact::redact(&cleaned);
    text::suffix_bytes(&redacted, max_bytes).to_string()
}

/// Project-relative display path, with sensitive paths flagged (§9.2).
pub fn display_path(abs: &str, root: &str) -> (String, bool) {
    let rel = paths::relative_to_root_resolved(abs, root);
    let sensitive = paths::is_sensitive(&rel);
    (rel, sensitive)
}

fn longest_field(p: &mut Payload) -> Option<&mut String> {
    let mut best: Option<&mut String> = None;
    for field in [
        p.stdout_tail.as_mut(),
        p.stderr_tail.as_mut(),
        p.error.as_mut(),
        p.summary.as_mut(),
        p.prompt.as_mut(),
        p.command.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        if best.as_ref().is_none_or(|b| field.len() > b.len()) {
            best = Some(field);
        }
    }
    best
}

/// Enforces the retained-payload budget (§9.1) by shrinking the largest
/// text field until the serialized payload fits.
pub fn enforce_budget(p: &mut Payload, max_bytes: usize) {
    for _ in 0..24 {
        let len = p.to_json().len();
        if len <= max_bytes {
            return;
        }
        let over = len - max_bytes;
        let Some(field) = longest_field(p) else {
            return;
        };
        let target = field
            .len()
            .saturating_sub(over.max(field.len() / 4).max(64));
        *field = text::suffix_bytes(field, target).to_string();
        if field.is_empty() {
            // Drop empties so the next pass picks another field.
            if p.stdout_tail.as_deref() == Some("") {
                p.stdout_tail = None;
            } else if p.stderr_tail.as_deref() == Some("") {
                p.stderr_tail = None;
            } else if p.error.as_deref() == Some("") {
                p.error = None;
            } else if p.summary.as_deref() == Some("") {
                p.summary = None;
            } else if p.prompt.as_deref() == Some("") {
                p.prompt = None;
            } else if p.command.as_deref() == Some("") {
                p.command = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerates_wrong_types_and_unknown_fields() {
        let raw = br#"{"session_id": 12345, "prompt": true, "cwd": {"a":1}, "unknown": [1,2], "tool_use_id": "t1"}"#;
        let input = parse(raw).unwrap();
        assert_eq!(input.session_id.as_deref(), Some("12345"));
        assert_eq!(input.prompt.as_deref(), Some("true"));
        assert_eq!(input.cwd, None);
        assert_eq!(input.tool_use_id.as_deref(), Some("t1"));
    }

    #[test]
    fn salvages_session_id() {
        assert_eq!(
            salvage_session_id(br#"{"session_id": "abc", "#).as_deref(),
            Some("abc")
        );
        assert_eq!(salvage_session_id(b"{broken"), None);
    }

    #[test]
    fn salvage_survives_input_cut_inside_a_character() {
        // A byte cap that lands in the middle of `é` used to fail the UTF-8
        // check for the whole buffer, and lose the session id with it.
        let mut raw =
            br#"{"session_id": "abc", "cwd": "C:\\Users\\dev\\repo", "prompt": "r"#.to_vec();
        raw.extend_from_slice(&"\u{e9}".as_bytes()[..1]);
        assert_eq!(salvage_session_id(&raw).as_deref(), Some("abc"));
        // Escapes are decoded where the value is read whole: a Windows cwd.
        assert_eq!(
            salvage_json_string(&raw, "cwd").as_deref(),
            Some("C:\\Users\\dev\\repo")
        );
        // A value that never closes is not guessed at.
        assert_eq!(salvage_json_string(&raw, "prompt"), None);
        assert_eq!(salvage_json_string(&raw, "prompt_id"), None);
    }

    #[test]
    fn edit_line_counts_and_excerpt() {
        let ti = ToolInput {
            old_string: Some("a\nb\nc".into()),
            new_string: Some("a\nB\nc".into()),
            ..Default::default()
        };
        assert_eq!(ti.line_counts("Edit"), (Some(3), Some(3)));
        assert_eq!(ti.excerpt("Edit").as_deref(), Some("- b\n+ B"));
        let write = ToolInput {
            content: Some("x\ny\n".into()),
            ..Default::default()
        };
        assert_eq!(write.line_counts("Write"), (Some(2), None));
        assert_eq!(write.excerpt("Write"), None);
        let added = ToolInput {
            old_string: Some("a".into()),
            new_string: Some("a\nnew line".into()),
            ..Default::default()
        };
        assert_eq!(added.excerpt("Edit").as_deref(), Some("+ new line"));
    }

    #[test]
    fn shell_response_aliases() {
        let raw = serde_json::value::RawValue::from_string(
            r#"{"stdout":"ok","exitCode":3,"interrupted":false}"#.into(),
        )
        .unwrap();
        let r = ShellResponse::parse(Some(&raw));
        assert_eq!(r.stdout.as_deref(), Some("ok"));
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(r.interrupted, Some(false));
    }

    #[test]
    fn redaction_and_budget() {
        let secret = format!("token=abcdefgh12345678 {}", "x".repeat(20_000));
        let tail = redact_tail(&secret, 8192);
        assert!(tail.len() <= 8192);
        let mut p = Payload {
            stdout_tail: Some("y".repeat(20_000)),
            stderr_tail: Some("z".repeat(20_000)),
            command: Some("cargo test".into()),
            ..Default::default()
        };
        enforce_budget(&mut p, limits::PAYLOAD);
        assert!(
            p.to_json().len() <= limits::PAYLOAD,
            "{}",
            p.to_json().len()
        );
        assert_eq!(p.command.as_deref(), Some("cargo test"));
    }
}
