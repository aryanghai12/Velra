//! Discovery of historical Claude Code sessions from the local project
//! transcripts under `~/.claude/projects/<project>/<session>.jsonl`.
//!
//! # The contract with this file format
//!
//! There isn't one. The transcript is Claude Code's own on-disk journal, not a
//! published interface: record types appear and disappear between releases,
//! fields are added, and a session that is being written right now ends in a
//! half-line. Everything here is therefore written to the weakest possible
//! assumption — that a line *might* be JSON and *might* carry a field we
//! recognise — and to one hard rule: **nothing in this module may panic, and
//! nothing may read a whole transcript.**
//!
//! Concretely:
//!
//! * every line is parsed into [`serde_json::Value`], so unknown fields cost
//!   nothing and unknown record types are simply skipped;
//! * a line that does not parse is skipped, not fatal — a transcript that is
//!   half corrupt still yields whatever sits before the corruption;
//! * only the first [`PROBE_BYTES`] of a file are ever read. Transcripts on
//!   this machine run to 4 MB, and the picker needs a title, not a
//!   conversation;
//! * the trailing partial line of a bounded read is always discarded, so a
//!   session Claude Code is writing to *right now* can never contribute a
//!   truncated title;
//! * every string that reaches the caller has been redacted, stripped of
//!   control characters and capped.
//!
//! What this module deliberately does **not** do is interpret the transcript.
//! It extracts an identifier, a timestamp and a label. The operational state
//! comes from Velra's own ledger, never from here.

use crate::redact;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How much of a transcript's head is read to find a title.
///
/// Claude Code writes the session's first user record and its `ai-title`
/// records within the first few hundred lines; 256 KiB covers that with room
/// to spare on every transcript measured here, and bounds the work at roughly
/// one disk read per session however large the file is.
pub const PROBE_BYTES: usize = 256 * 1024;

/// Longest title shown in the picker, in characters.
pub const TITLE_MAX_CHARS: usize = 72;

/// A historical session the user can choose to restore from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    /// Claude Code's session id (the transcript's file stem).
    pub session_id: String,
    /// Absolute path of the transcript, when one was found on disk.
    pub transcript_path: Option<PathBuf>,
    /// Transcript mtime, or the ledger's last event, in epoch ms.
    pub last_activity_ms: i64,
    /// Transcript size in bytes, when known. Free — it comes from the same
    /// `stat` as the mtime, so it costs no extra I/O.
    pub size_bytes: Option<u64>,
    /// Human-readable label, already redacted and capped. `None` when the
    /// transcript yielded nothing usable, in which case the session is still
    /// selectable and the caller falls back to the id and the timestamp.
    pub title: Option<String>,
    /// Working directory the transcript reports, used to confirm the session
    /// belongs to this workspace.
    pub cwd: Option<String>,
    /// Whether Velra's ledger holds state for this session.
    pub has_state: bool,
}

impl SessionRef {
    /// Short form of the id used when there is no title, e.g. `8f32…c91a`.
    pub fn short_id(&self) -> String {
        let chars: Vec<char> = self.session_id.chars().collect();
        if chars.len() <= 12 {
            return self.session_id.clone();
        }
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}\u{2026}{tail}")
    }

    /// What the picker prints on the first line of an entry.
    pub fn label(&self) -> String {
        match &self.title {
            Some(t) => t.clone(),
            None => self.short_id(),
        }
    }
}

/// `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
pub fn projects_dir(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("projects"));
    }
    Some(user_home?.join(".claude").join("projects"))
}

/// Claude Code's directory-name encoding for a working directory: every
/// character outside `[A-Za-z0-9]` becomes `-`.
///
/// Derived from the names on disk rather than from documentation, so it is
/// treated as a hint and never as the only way to find a session — see
/// [`project_dirs_for_root`].
pub fn encode_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Candidate transcript directories for a workspace root.
///
/// The encoding above is applied to whatever string the client passed as its
/// cwd, and clients disagree about the case of a Windows drive letter: this
/// machine has both `c--Users-…` and `C--Users-…` directories. Rather than
/// guess, every entry of `projects_dir` whose name matches a candidate
/// case-insensitively is returned. That is a directory listing, not a file
/// read, so the cost does not scale with transcript size.
pub fn project_dirs_for_root(projects: &Path, root: &str) -> Vec<PathBuf> {
    let wanted = encode_project_dir(&crate::paths::normalize_abs(root).replace('/', "\\"));
    let alt = encode_project_dir(&crate::paths::normalize_abs(root));
    let candidates = [wanted.to_lowercase(), alt.to_lowercase()];
    let Ok(rd) = std::fs::read_dir(projects) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            candidates.contains(&name)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// What a bounded head read of a transcript yielded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub title: Option<String>,
    pub cwd: Option<String>,
}

/// Reads at most `PROBE_BYTES` from the head of `path`.
///
/// Returns an empty buffer rather than an error for anything unreadable: a
/// transcript locked by another process, deleted between the listing and the
/// read, or replaced by a directory is a session without a title, not a
/// failure of the command.
fn read_head(path: &Path, max: usize) -> Vec<u8> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    // `take` bounds the read even if the file grows while it is open.
    if file.take(max as u64).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    buf
}

/// Drops the trailing partial line of a bounded read.
///
/// Without this, the probe of a session Claude Code is writing to right now
/// ends mid-record, and — far worse than losing a title — a cut inside a
/// string could hand back half of whatever that string held.
fn whole_lines(buf: &[u8]) -> &[u8] {
    match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => &buf[..=i],
        None => &[],
    }
}

/// Cleans a candidate title: redact, drop control characters, collapse
/// whitespace, cap length.
///
/// Every string that leaves this module goes through here. The transcript is
/// the one input Velra reads that its own ingest-time redaction never saw, so
/// this is the boundary where that is corrected.
pub fn clean_title(raw: &str) -> Option<String> {
    let redacted = redact::redact(raw);
    let no_control: String = redacted
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = crate::text::collapse_whitespace(&no_control);
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(crate::text::truncate_chars(trimmed, TITLE_MAX_CHARS).into_owned())
}

/// True for a user record that represents something the person actually
/// typed, as opposed to the machinery around it.
///
/// Excluded: sidechain (subagent) turns, records with no `message` at all
/// (environment attachments carry `attachment`/`rendered` instead), tool
/// results, and slash-command invocations — `/clear` is not a session title.
fn user_text(record: &serde_json::Value) -> Option<String> {
    if record.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    if record.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let content = record.get("message")?.get("content")?;
    // Older transcripts store a bare string; current ones store a list of
    // typed blocks. Both shapes, and anything else, are handled without
    // assuming either is present.
    //
    // Context the client injected is not what the user typed: the VS Code
    // extension sends `<ide_opened_file>` as its own text block ahead of the
    // prompt, and a title that opens with it names the editor tab, not the task.
    let text = match content {
        serde_json::Value::String(s) => crate::prompt::authored(s).to_string(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .map(crate::prompt::authored)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with("<command-name>") {
        return None;
    }
    Some(trimmed.to_string())
}

/// Extracts a title and a cwd from a bounded head read.
///
/// Title preference, in order: the `ai-title` record Claude Code already wrote
/// to the transcript, then the first thing the user typed. Velra invokes no
/// model of its own for this — both are strings already on disk.
pub fn probe(path: &Path) -> Probe {
    probe_bytes(&read_head(path, PROBE_BYTES))
}

/// [`probe`] against an in-memory buffer, so the parsing rules can be tested
/// without touching the filesystem.
pub fn probe_bytes(buf: &[u8]) -> Probe {
    let mut out = Probe::default();
    let mut first_prompt: Option<String> = None;
    for line in whole_lines(buf).split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        if !record.is_object() {
            continue;
        }
        if out.cwd.is_none() {
            if let Some(c) = record.get("cwd").and_then(|v| v.as_str()) {
                out.cwd = Some(c.to_string());
            }
        }
        match record.get("type").and_then(|v| v.as_str()) {
            Some("ai-title") => {
                if out.title.is_none() {
                    out.title = record
                        .get("aiTitle")
                        .and_then(|v| v.as_str())
                        .and_then(clean_title);
                }
            }
            Some("user") if first_prompt.is_none() => {
                first_prompt = user_text(&record).as_deref().and_then(clean_title);
            }
            _ => {}
        }
        if out.title.is_some() && out.cwd.is_some() {
            break;
        }
    }
    if out.title.is_none() {
        out.title = first_prompt;
    }
    out
}

/// Lists the transcripts in one project directory, newest first.
///
/// Only `stat` is called here. Titles are probed by the caller, so a caller
/// that only needs ids and timestamps pays nothing for them.
pub fn list_transcripts(dir: &Path) -> Vec<SessionRef> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<SessionRef> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let path = e.path();
            let session_id = path.file_stem()?.to_string_lossy().into_owned();
            let meta = e.metadata().ok();
            let last_activity_ms = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or(0);
            Some(SessionRef {
                session_id,
                transcript_path: Some(path),
                last_activity_ms,
                size_bytes: meta.as_ref().map(|m| m.len()),
                title: None,
                cwd: None,
                has_state: false,
            })
        })
        .collect();
    sort_newest_first(&mut out);
    out
}

/// Newest first, ties broken by session id so the picker's numbering is
/// reproducible between two runs that see the same directory.
pub fn sort_newest_first(refs: &mut [SessionRef]) {
    refs.sort_by(|a, b| {
        b.last_activity_ms
            .cmp(&a.last_activity_ms)
            .then(a.session_id.cmp(&b.session_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        format!("{v}\n")
    }

    #[test]
    fn encodes_the_directory_name_claude_code_uses() {
        // Both observed on this machine: the drive letter's case follows
        // whatever the client passed, so neither form may be assumed.
        assert_eq!(
            encode_project_dir(r"c:\Users\aryan\Videos\Velra"),
            "c--Users-aryan-Videos-Velra"
        );
        assert_eq!(
            encode_project_dir(r"C:\Users\aryan\Videos\Velra"),
            "C--Users-aryan-Videos-Velra"
        );
        assert_eq!(encode_project_dir("/home/u/proj"), "-home-u-proj");
    }

    #[test]
    fn prefers_the_ai_title_then_the_first_prompt() {
        let buf = [
            line(serde_json::json!({"type": "queue-operation", "operation": "enqueue"})),
            line(serde_json::json!({
                "type": "user",
                "cwd": "/home/u/proj",
                "message": {"role": "user", "content": [{"type": "text", "text": "fix the rounding bug"}]}
            })),
            line(serde_json::json!({"type": "ai-title", "aiTitle": "Invoice rounding"})),
        ]
        .concat();
        let p = probe_bytes(buf.as_bytes());
        assert_eq!(p.title.as_deref(), Some("Invoice rounding"));
        assert_eq!(p.cwd.as_deref(), Some("/home/u/proj"));

        // With no ai-title, the user's own first sentence is the label.
        let buf = [
            line(serde_json::json!({"type": "queue-operation"})),
            line(serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": "fix the rounding bug"}]}
            })),
        ]
        .concat();
        assert_eq!(
            probe_bytes(buf.as_bytes()).title.as_deref(),
            Some("fix the rounding bug")
        );
    }

    /// The picker must survive whatever the file happens to contain: garbage
    /// lines, record types that did not exist when this was written, records
    /// missing every field we look at, and a final line cut mid-write.
    #[test]
    fn tolerates_garbage_unknown_types_and_a_cut_final_line() {
        let buf = [
            "not json at all\n".to_string(),
            "\n".to_string(),
            line(serde_json::json!(["a bare array, not an object"])),
            line(serde_json::json!({"type": "some-future-record", "shape": {"we": "cannot know"}})),
            line(serde_json::json!({"no_type_field": true})),
            line(serde_json::json!({"type": "user"})), // no message
            line(serde_json::json!({"type": "user", "message": {"content": 42}})),
            line(serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "text", "text": "the real prompt"}]}
            })),
            r#"{"type":"ai-title","aiTitle":"cut off mid"#.to_string(),
        ]
        .concat();
        let p = probe_bytes(buf.as_bytes());
        assert_eq!(p.title.as_deref(), Some("the real prompt"));
        // The truncated last line contributed nothing.
        assert!(!p.title.as_deref().unwrap_or_default().contains("cut off"));
    }

    /// VS Code sends the editor's open file as its own text block ahead of the
    /// prompt, and a background task's notification arrives as a user record.
    /// Neither is what the user typed, so neither is the picker's title.
    #[test]
    fn injected_context_is_not_a_title() {
        let buf = [
            line(serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "text", "text": "<task-notification><status>completed</status></task-notification>"}]}
            })),
            line(serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {"type": "text", "text": "<ide_opened_file>The user opened the file c:\\repo\\readme.md in the IDE. This may or may not be related to the current task.</ide_opened_file>"},
                    {"type": "text", "text": "We have a failing payment retry test."}
                ]}
            })),
        ]
        .concat();
        assert_eq!(
            probe_bytes(buf.as_bytes()).title.as_deref(),
            Some("We have a failing payment retry test.")
        );
    }

    #[test]
    fn a_transcript_with_no_usable_title_yields_none() {
        let buf = [
            line(serde_json::json!({"type": "queue-operation"})),
            // A slash command is not a title, and neither is a subagent turn.
            line(serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "text", "text": "/clear"}]}
            })),
            line(serde_json::json!({
                "type": "user",
                "isSidechain": true,
                "message": {"content": [{"type": "text", "text": "subagent instructions"}]}
            })),
        ]
        .concat();
        assert_eq!(probe_bytes(buf.as_bytes()).title, None);
    }

    /// The transcript is the one thing Velra reads that its ingest-time
    /// redaction never touched, so the picker redacts on the way out.
    #[test]
    fn a_secret_in_a_title_is_redacted() {
        let buf = line(serde_json::json!({
            "type": "ai-title",
            "aiTitle": "deploy with sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFFGGGG"
        }));
        let title = probe_bytes(buf.as_bytes()).title.expect("title");
        assert!(!title.contains("sk-ant-api03"), "{title}");
        assert!(title.contains("deploy with"), "{title}");
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let buf = line(serde_json::json!({
            "type": "ai-title",
            "aiTitle": "red \u{1b}[31malert\u{1b}[0m\u{7}\u{0d}"
        }));
        let title = probe_bytes(buf.as_bytes()).title.expect("title");
        assert!(!title.chars().any(char::is_control), "{title:?}");
        assert!(!title.contains('\u{1b}'), "{title:?}");
    }

    #[test]
    fn titles_are_capped() {
        let long = "x".repeat(500);
        let buf = line(serde_json::json!({"type": "ai-title", "aiTitle": long}));
        let title = probe_bytes(buf.as_bytes()).title.expect("title");
        assert!(title.chars().count() <= TITLE_MAX_CHARS, "{}", title.len());
    }

    #[test]
    fn an_empty_or_missing_transcript_is_not_an_error() {
        assert_eq!(probe_bytes(b""), Probe::default());
        assert_eq!(probe_bytes(b"\n\n\n"), Probe::default());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(probe(&dir.path().join("nope.jsonl")), Probe::default());
        assert!(list_transcripts(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn short_ids_are_readable_and_never_panic_on_short_input() {
        let mk = |id: &str| SessionRef {
            session_id: id.to_string(),
            transcript_path: None,
            last_activity_ms: 0,
            size_bytes: None,
            title: None,
            cwd: None,
            has_state: false,
        };
        assert_eq!(
            mk("8f32aaaa-bbbb-cccc-dddd-eeeeeeeec91a").short_id(),
            "8f32\u{2026}c91a"
        );
        assert_eq!(mk("short").short_id(), "short");
        assert_eq!(mk("").short_id(), "");
        // With no title the entry still has a usable label.
        assert_eq!(mk("short").label(), "short");
    }

    #[test]
    fn listing_is_newest_first_and_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.jsonl", "b.jsonl", "not-a-transcript.txt"] {
            std::fs::write(dir.path().join(name), "{}\n").unwrap();
        }
        let listed = list_transcripts(dir.path());
        assert_eq!(listed.len(), 2, "only .jsonl files are sessions");

        let mut refs = vec![
            SessionRef {
                session_id: "b".into(),
                last_activity_ms: 100,
                transcript_path: None,
                size_bytes: None,
                title: None,
                cwd: None,
                has_state: false,
            },
            SessionRef {
                session_id: "a".into(),
                last_activity_ms: 100,
                transcript_path: None,
                size_bytes: None,
                title: None,
                cwd: None,
                has_state: false,
            },
            SessionRef {
                session_id: "c".into(),
                last_activity_ms: 200,
                transcript_path: None,
                size_bytes: None,
                title: None,
                cwd: None,
                has_state: false,
            },
        ];
        sort_newest_first(&mut refs);
        let order: Vec<&str> = refs.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(order, ["c", "a", "b"], "newest first, then id");
    }
}
