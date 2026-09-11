//! The normalized event row (§10.4 `events`) and its retained payload (§9.1).
//!
//! The same types are written by hooks (directly or via the spool) and read
//! by the reducer.

use serde::{Deserialize, Serialize};

/// Retained payload limits (§9.1), in bytes.
pub mod limits {
    pub const PROMPT: usize = 4 * 1024;
    pub const COMMAND: usize = 2 * 1024;
    pub const OUTPUT_TAIL: usize = 8 * 1024;
    pub const ERROR: usize = 4 * 1024;
    pub const PATTERN_CHARS: usize = 200;
    pub const EXCERPT_LINE_CHARS: usize = 160;
    pub const PAYLOAD: usize = 16 * 1024;
    pub const POST_COMPACT_PAYLOAD: usize = 48 * 1024;
    pub const SUMMARY: usize = 32 * 1024;
}

/// A file hash observed by a hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileObservation {
    pub path: String,
    pub hash: String,
    #[serde(default)]
    pub size: u64,
}

/// Git-aware observation attached to shell tool events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitObservation {
    /// Restore-family subcommand text, if the command contains one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<String>,
    /// Whether the command contains `git commit`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub commit: bool,
    /// Hashes of files edited in the current epoch.
    #[serde(default)]
    pub files: Vec<FileObservation>,
}

/// Normalized, redacted payload. One flat struct keeps (de)serialization
/// cheap; unused fields are omitted from the JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Payload {
    // UserPromptSubmit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,

    // SessionStart / SessionEnd / Stop / compaction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_hook_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    // File tools
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    // Shell tools
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitObservation>,

    // PostToolUseFailure
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_interrupt: Option<bool>,

    // checkpoint_request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
}

impl Payload {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Tolerant parse: malformed payloads become empty.
    pub fn from_json(s: &str) -> Payload {
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// Project identity recorded alongside events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project_id: String,
    pub root_path: String,
    pub is_git: bool,
}

/// A normalized event as appended to `events` or written to the spool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvent {
    pub dedupe_key: String,
    pub session_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub hook_event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    pub ts_ms: i64,
    /// Normalized, redacted payload JSON.
    pub payload: String,
    /// Project row to upsert with the event (not an `events` column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectInfo>,
}

/// `blake3(hook_event|session_id|tool_use_id|prompt_id|ts_ms|agent_id)[0..32]`.
pub fn dedupe_key(
    hook_event: &str,
    session_id: &str,
    tool_use_id: Option<&str>,
    prompt_id: Option<&str>,
    ts_ms: i64,
    agent_id: Option<&str>,
) -> String {
    let material = format!(
        "{hook_event}|{session_id}|{}|{}|{ts_ms}|{}",
        tool_use_id.unwrap_or(""),
        prompt_id.unwrap_or(""),
        agent_id.unwrap_or("")
    );
    crate::hash::hex_prefix(material.as_bytes(), 32)
}

/// A stored event row as read by the reducer.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub session_id: String,
    pub project_id: String,
    pub agent_id: Option<String>,
    pub hook_event: String,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub ts_ms: i64,
    pub payload: Payload,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_omits_empty_fields() {
        let p = Payload {
            prompt: Some("hi".into()),
            ..Default::default()
        };
        assert_eq!(p.to_json(), r#"{"prompt":"hi"}"#);
        assert_eq!(Payload::from_json("not json"), Payload::default());
        assert_eq!(
            Payload::from_json(r#"{"prompt":"hi","unknown":1}"#)
                .prompt
                .as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn dedupe_is_stable() {
        let a = dedupe_key("PostToolUse", "s", Some("t"), None, 5, None);
        assert_eq!(a.len(), 32);
        assert_eq!(a, dedupe_key("PostToolUse", "s", Some("t"), None, 5, None));
        assert_ne!(a, dedupe_key("PostToolUse", "s", Some("t"), None, 6, None));
    }
}
