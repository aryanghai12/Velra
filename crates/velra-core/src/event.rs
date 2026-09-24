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

/// An injected block of a prompt that was not stored (see
/// [`Payload::prompt_omitted`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedBlock {
    pub tag: String,
    /// Size as delivered, in bytes.
    pub bytes: u64,
}

/// One constraint sentence extracted from the whole prompt (see
/// [`PromptFacts`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptConstraint {
    /// The sentence as the user wrote it, whitespace-collapsed and redacted.
    pub text: String,
    /// The literal cue that selected it (`crate::constraint`).
    pub cue: String,
    /// `labelled` / `prohibition` / `requirement`.
    pub kind: String,
    /// Byte offset, in the user's redacted text, of the paragraph or list
    /// item it was quoted from. Past the scan bound the unexamined middle is
    /// counted at its delivered size.
    pub at: u64,
    /// Why it was selected (`crate::constraint::Basis`): `cue`,
    /// `rejection`, or `rejection+antecedent`.
    pub basis: String,
}

/// What the hook read from the whole of the user's text before the prompt
/// was bounded for storage (`crate::prompt::for_storage`).
///
/// The reducer takes a prompt's constraints from here rather than from
/// [`Payload::prompt`], so a rule stated past the storage bound is not lost to
/// it. Absent on events written before v0.1.2's Phase 1B hardening, and on
/// prompts delivered without text; the reducer then extracts from `prompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptFacts {
    /// Version of the extraction rules that produced this record.
    pub v: u32,
    /// The user's own text as delivered, in bytes.
    pub authored_bytes: u64,
    /// How much of it was examined: less than `authored_bytes` only past the
    /// scan bound (`crate::prompt::SCAN_LIMIT`).
    pub scanned_bytes: u64,
    /// Bytes of the user's text that `prompt` does not hold.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub elided_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<PromptConstraint>,
    /// Constraint sentences that qualified before the per-prompt caps.
    /// Recorded only when a cap dropped some, so that the record says it
    /// holds fewer than the prompt stated.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub constraints_found: u64,
    /// Identifiers named only in text `prompt` does not hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<String>,
    /// Injected blocks left out of `prompt`, all of them counted;
    /// [`Payload::prompt_omitted`] lists at most 32.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_blocks: u64,
    /// Bytes past `crate::prompt::MAX_EDGE_BLOCKS` injected blocks that were
    /// not classified and are not stored.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unparsed_bytes: u64,
    /// The prompt was not processed: the hook's watchdog fired first, or the
    /// input exceeded the size the hook parses a prompt from. Nothing of the
    /// prompt is stored; `authored_bytes` is then the size of the whole prompt
    /// as delivered (of the whole hook input, when it was not parsed), and the
    /// record exists so that the log says a prompt arrived rather than
    /// holding nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unprocessed: bool,
}

/// The version of [`PromptFacts`] this build writes.
pub const PROMPT_FACTS_VERSION: u32 = 1;

impl PromptFacts {
    /// Whether this build may take state from the record: its version is one
    /// this build knows (1 through [`PROMPT_FACTS_VERSION`]).
    ///
    /// A record from a newer build can mean something different in fields
    /// this build reads -- a constraint list filtered differently, an
    /// `unprocessed` flag with new conditions -- and reading it as version 1
    /// would assert state the newer build did not. The reducer then does what
    /// it does for events written before the record existed: extracts from
    /// the stored prompt. A record with no version (0) is malformed.
    pub fn usable(&self) -> bool {
        (1..=PROMPT_FACTS_VERSION).contains(&self.v)
    }
}

/// `prompt_facts` that parses, or `None`.
fn lenient_facts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<PromptFacts>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

fn is_zero(n: &u64) -> bool {
    *n == 0
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
    /// Injected blocks the hook left out whole to keep `prompt` within its
    /// budget (`crate::prompt::for_storage`). Recorded so the log says what it
    /// does not hold rather than silently holding less.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_omitted: Option<Vec<OmittedBlock>>,
    /// Some of the user's own text is not in `prompt`: it was kept as a
    /// digest to fit `limits::PROMPT` (`crate::prompt::for_storage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_truncated: Option<bool>,
    /// What was extracted from the whole prompt before it was bounded.
    ///
    /// Read leniently: a record this build cannot parse is `None`, never a
    /// reason to discard the rest of the payload (which `from_json` would
    /// otherwise do, prompt and all). Whether a parsed record is used is the
    /// reducer's decision (`PromptFacts::usable`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_facts"
    )]
    pub prompt_facts: Option<PromptFacts>,

    /// The hook could not append this event and wrote it to the spool
    /// (`crate::spool::write`); it was ingested later, after rows that
    /// happened after it. The reducer places it by its timestamp
    /// (`crate::order`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spooled: Option<bool>,

    /// Stop: the files edited in the session, hashed by the Stop hook when the
    /// turn ended. `None` when no observation was made (the database was
    /// unreachable, the event predates the field): the reducer then records
    /// no turn-end state rather than hashing the disk as it is when it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_scan: Option<Vec<FileObservation>>,

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
