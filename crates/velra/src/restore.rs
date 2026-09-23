//! `velra restore`: choose a previous session of this workspace and stage its
//! operational state for the next one.
//!
//! The workflow this exists for:
//!
//! ```text
//! long Claude Code session -> the developer decides to leave it
//!   -> velra restore -> pick a previous session
//!   -> start a brand-new Claude Code session
//!   -> SessionStart(startup) delivers the staged state, once
//! ```
//!
//! This module stops at staging. Nothing here consumes the capsule; the
//! `SessionStart` hook does (`hook.rs`, `deliver_staged`).
//!
//! # The picker
//!
//! A numbered stdin selector, and deliberately no more than that. The
//! alternative was a TUI crate, and the dependency audit did not justify one:
//! this is a list of at most a screenful of lines read once per restore, the
//! binary ships through `cargo binstall` and MinGW, and the terminal
//! behaviours a raw-mode picker has to get right — Windows console modes,
//! redirected stdin, a non-tty in CI — are exactly the ones a numbered prompt
//! gets right for free. See the `restore_picker` tests.

use crate::home;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use velra_core::restore::LedgerSession;
use velra_core::transcript::{self, SessionRef};

/// Most sessions offered at once.
///
/// A workspace accumulates transcripts indefinitely; a picker that prints two
/// hundred of them is not a picker. The list is newest-first, so the cut only
/// ever removes sessions the user is least likely to want.
pub const PICKER_MAX: usize = 20;

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub session: SessionRef,
    /// Present when the ledger holds this session for this workspace.
    pub ledger: Option<LedgerSession>,
}

impl Candidate {
    /// Whether this session can actually be restored from.
    pub fn restorable(&self) -> bool {
        self.session.has_state
    }
}

/// Collects the sessions of one workspace from both sources that know about
/// them, and reconciles the two.
///
/// * **Velra's ledger** is authoritative for *which sessions have state*, and
///   is workspace-scoped by `project_id`.
/// * **The transcript directory** is authoritative for *what a session was
///   about* and *when it was last active*, including sessions from before
///   Velra was enabled.
///
/// Neither alone is enough: a session Velra recorded may have had its
/// transcript deleted, and a session with a transcript may predate the hooks.
/// The union is offered, with the ledger deciding what is restorable.
pub fn discover(
    conn: &rusqlite::Connection,
    project_id: &str,
    workspace_root: &str,
    user_home: Option<&Path>,
) -> Vec<Candidate> {
    let ledger: BTreeMap<String, LedgerSession> =
        velra_core::restore::sessions_for_workspace(conn, project_id)
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();

    let mut by_id: BTreeMap<String, SessionRef> = BTreeMap::new();

    // Transcripts under the workspace's project directory.
    if let Some(projects) = transcript::projects_dir(user_home) {
        for dir in transcript::project_dirs_for_root(&projects, workspace_root) {
            for entry in transcript::list_transcripts(&dir) {
                by_id.entry(entry.session_id.clone()).or_insert(entry);
            }
        }
    }

    // Sessions the ledger knows about, including any whose transcript is gone
    // or lives somewhere the directory-name encoding did not predict.
    for (id, row) in &ledger {
        let existing = by_id.entry(id.clone()).or_insert_with(|| SessionRef {
            session_id: id.clone(),
            transcript_path: row.transcript_path.as_deref().map(PathBuf::from),
            last_activity_ms: row.last_event_ms,
            size_bytes: None,
            title: None,
            cwd: None,
            has_state: false,
        });
        if existing.last_activity_ms == 0 {
            existing.last_activity_ms = row.last_event_ms;
        }
        if existing.transcript_path.is_none() {
            existing.transcript_path = row.transcript_path.as_deref().map(PathBuf::from);
        }
    }

    let mut refs: Vec<SessionRef> = by_id.into_values().collect();
    transcript::sort_newest_first(&mut refs);
    refs.truncate(PICKER_MAX);

    // Probing is the only part that reads file contents, so it happens last
    // and only for the rows that will actually be shown.
    refs.into_iter()
        .map(|mut s| {
            if let Some(path) = s.transcript_path.clone() {
                let probe = transcript::probe(&path);
                s.title = probe.title;
                s.cwd = probe.cwd;
            }
            let ledger_row = ledger.get(&s.session_id).cloned();
            s.has_state = ledger_row
                .as_ref()
                .map(|_| {
                    velra_core::restore::has_restorable_state(conn, &s.session_id).unwrap_or(false)
                })
                .unwrap_or(false);
            Candidate {
                session: s,
                ledger: ledger_row,
            }
        })
        .collect()
}

/// Human-readable size, or an empty string when it is not known.
fn size_hint(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) if b >= 1024 * 1024 => format!(" · ~{} MB", b / (1024 * 1024)),
        Some(b) if b >= 1024 => format!(" · ~{} KB", b / 1024),
        Some(_) => String::new(),
        None => String::new(),
    }
}

/// The lines the picker prints for one candidate.
///
/// Split out from the printing so the formatting can be asserted without a
/// terminal, which is also what keeps the "no secrets in picker output" claim
/// testable.
pub fn render_entry(index: usize, c: &Candidate, tz: i32) -> String {
    let when = velra_core::time::rfc3339_utc(c.session.last_activity_ms);
    let _ = tz;
    let state = if c.restorable() {
        "state: yes"
    } else {
        "state: none"
    };
    format!(
        "{index}. {}\n   {} · Last activity: {when} · {state}{}",
        c.session.label(),
        c.session.short_id(),
        size_hint(c.session.size_bytes)
    )
}

/// Prints the list and reads a choice from stdin.
///
/// Returns the chosen index, or `None` when the user declined — an empty line,
/// `q`, or EOF. An out-of-range or unparseable answer re-prompts rather than
/// guessing, and a non-interactive stdin gets one read and then gives up, so
/// this can never spin in a script.
pub fn prompt_choice(
    entries: &[Candidate],
    tz: i32,
    input: &mut impl BufRead,
    output: &mut impl Write,
    interactive: bool,
) -> std::io::Result<Option<usize>> {
    writeln!(output, "Velra — Restore previous session\n")?;
    for (i, c) in entries.iter().enumerate() {
        writeln!(output, "{}\n", render_entry(i + 1, c, tz))?;
    }
    let attempts = if interactive { 3 } else { 1 };
    for _ in 0..attempts {
        write!(output, "Select [1-{}] (q to cancel): ", entries.len())?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None); // EOF
        }
        let answer = line.trim();
        if answer.is_empty() || answer.eq_ignore_ascii_case("q") {
            return Ok(None);
        }
        match answer.parse::<usize>() {
            Ok(n) if n >= 1 && n <= entries.len() => return Ok(Some(n - 1)),
            _ => {
                writeln!(output, "\nNot a choice in range.")?;
            }
        }
    }
    Ok(None)
}

/// `$VELRA_HOME/staged/<workspace_id>/staged_capsule` for this workspace.
pub fn staged_path(velra_home: &Path, workspace_id: &str) -> PathBuf {
    velra_core::staging::staged_path(velra_home, workspace_id)
}

/// The user's home, for locating `~/.claude/projects`.
pub fn user_home() -> Option<PathBuf> {
    home::user_home()
}

#[cfg(test)]
mod tests {
    use super::*;
    use velra_core::transcript::SessionRef;

    fn candidate(id: &str, title: Option<&str>, has_state: bool) -> Candidate {
        Candidate {
            session: SessionRef {
                session_id: id.to_string(),
                transcript_path: None,
                last_activity_ms: 1_789_207_445_000,
                size_bytes: Some(2 * 1024 * 1024),
                title: title.map(str::to_string),
                cwd: None,
                has_state,
            },
            ledger: None,
        }
    }

    #[test]
    fn an_entry_without_a_title_is_still_selectable() {
        let c = candidate("8f32aaaa-bbbb-cccc-dddd-eeeeeeeec91a", None, false);
        let line = render_entry(2, &c, 0);
        assert!(line.starts_with("2. 8f32\u{2026}c91a"), "{line}");
        assert!(line.contains("Last activity: "), "{line}");
        assert!(line.contains("state: none"), "{line}");
    }

    #[test]
    fn an_entry_shows_whether_velra_has_state() {
        let with = render_entry(1, &candidate("a", Some("Fix invoice rounding"), true), 0);
        assert!(with.contains("1. Fix invoice rounding"), "{with}");
        assert!(with.contains("state: yes"), "{with}");
        assert!(with.contains("~2 MB"), "{with}");
    }

    #[test]
    fn a_choice_in_range_is_returned_and_anything_else_cancels() {
        let entries = vec![candidate("a", Some("A"), true), candidate("b", None, true)];
        let mut out = Vec::new();

        let mut input = std::io::Cursor::new(b"2\n".to_vec());
        assert_eq!(
            prompt_choice(&entries, 0, &mut input, &mut out, false).unwrap(),
            Some(1)
        );

        for declined in ["\n", "q\n", "Q\n", ""] {
            let mut input = std::io::Cursor::new(declined.as_bytes().to_vec());
            let mut out = Vec::new();
            assert_eq!(
                prompt_choice(&entries, 0, &mut input, &mut out, false).unwrap(),
                None,
                "{declined:?}"
            );
        }
    }

    /// A number outside the list, or something that is not a number, must not
    /// be rounded into a selection — restoring the wrong session silently is
    /// worse than restoring nothing.
    #[test]
    fn an_out_of_range_or_junk_answer_never_selects() {
        let entries = vec![candidate("a", Some("A"), true)];
        for answer in ["0\n", "2\n", "-1\n", "abc\n", "1.5\n", "\u{1b}[A\n"] {
            let mut input = std::io::Cursor::new(answer.as_bytes().to_vec());
            let mut out = Vec::new();
            assert_eq!(
                prompt_choice(&entries, 0, &mut input, &mut out, false).unwrap(),
                None,
                "{answer:?}"
            );
        }
    }

    /// Non-interactive input gets exactly one read: a piped stdin that never
    /// produces a valid answer must terminate, not loop.
    #[test]
    fn a_non_interactive_picker_does_not_spin() {
        let entries = vec![candidate("a", Some("A"), true)];
        let mut input = std::io::Cursor::new(b"nonsense\n".to_vec());
        let mut out = Vec::new();
        assert_eq!(
            prompt_choice(&entries, 0, &mut input, &mut out, false).unwrap(),
            None
        );
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("Select [1-1]").count(), 1, "{text}");
    }
}
