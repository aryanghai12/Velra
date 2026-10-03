//! Local observability (§20). Hooks never write to stderr; errors go to
//! `~/.velra/logs/errors.log`, rotated at 1 MiB keeping 3 files.

use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 1024 * 1024;
const KEEP: usize = 3;

fn rotate(path: &Path) {
    let base = path.to_path_buf();
    let nth = |n: usize| -> PathBuf {
        let mut p = base.clone().into_os_string();
        p.push(format!(".{n}"));
        PathBuf::from(p)
    };
    let _ = std::fs::remove_file(nth(KEEP));
    for n in (1..KEEP).rev() {
        let _ = std::fs::rename(nth(n), nth(n + 1));
    }
    let _ = std::fs::rename(&base, nth(1));
}

fn append(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        if !dir.is_dir() && crate::home::ensure_dir(dir).is_err() {
            return;
        }
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        rotate(path);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(path) {
        // One append per line: concurrent hook processes share this file,
        // and two separate writes would let their output interleave.
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        let _ = f.write_all(buf.as_bytes());
    }
}

/// One log line, redacted before it is written like everything else Velra
/// persists. The messages are Velra's own and do not quote hook input
/// today, but the panic hook logs whatever a panic says and an error's
/// `Display` is whatever its source chose; neither is checked here, so the
/// line is redacted rather than trusted.
fn line(level: &str, subcommand: &str, session: Option<&str>, msg: &str) -> String {
    let msg = msg.replace('\n', " ");
    format!(
        "{} {level} {subcommand} {} {}",
        velra_core::time::rfc3339_utc(velra_core::time::now_ms()),
        session.unwrap_or("-"),
        velra_core::redact::redact(&msg)
    )
}

/// Records an error. Never fails and never writes to stderr.
pub fn error(
    home: Option<&Path>,
    subcommand: &str,
    session: Option<&str>,
    msg: impl std::fmt::Display,
) {
    let Some(home) = home else { return };
    append(
        &crate::home::logs_dir(home).join("errors.log"),
        &line("ERROR", subcommand, session, &msg.to_string()),
    );
}

pub fn warn(
    home: Option<&Path>,
    subcommand: &str,
    session: Option<&str>,
    msg: impl std::fmt::Display,
) {
    let Some(home) = home else { return };
    append(
        &crate::home::logs_dir(home).join("errors.log"),
        &line("WARN", subcommand, session, &msg.to_string()),
    );
}

/// True when `VELRA_LOG=debug`.
pub fn debug_enabled() -> bool {
    std::env::var_os("VELRA_LOG").is_some_and(|v| v == "debug")
}

/// Per-invocation timing, only when debug logging is on (§20).
pub fn debug(
    home: Option<&Path>,
    subcommand: &str,
    session: Option<&str>,
    msg: impl std::fmt::Display,
) {
    if !debug_enabled() {
        return;
    }
    let Some(home) = home else { return };
    append(
        &crate::home::logs_dir(home).join("debug.log"),
        &line("DEBUG", subcommand, session, &msg.to_string()),
    );
}

/// Reads the last `n` lines of `errors.log` for `velra doctor`.
pub fn tail_errors(home: &Path, n: usize) -> Vec<String> {
    let path = crate::home::logs_dir(home).join("errors.log");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 9: whatever a message quotes -- the panic hook logs `panic:
    /// {info}` as it is -- is redacted before it reaches the file.
    #[test]
    fn a_logged_message_is_redacted_before_it_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let secret = "ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        let message = format!("injected panic in `git push https://{secret}@github.com/o/r`");
        error(
            Some(home),
            "hook post-tool-use",
            Some("s1"),
            format!("panic: {message}"),
        );
        warn(
            Some(home),
            "hook stop",
            None,
            format!("reconcile: token={secret}"),
        );
        let written =
            std::fs::read_to_string(crate::home::logs_dir(home).join("errors.log")).unwrap();
        assert!(!written.contains(secret), "{written}");
        assert_eq!(written.matches("[REDACTED:").count(), 2, "{written}");
        // `debug` writes the same `line`; it is gated on an env var this
        // test does not set, since tests share the process.
        let debug_line = line("DEBUG", "reduce", None, &format!("token={secret}"));
        assert!(!debug_line.contains(secret), "{debug_line}");
    }

    #[test]
    fn writes_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        error(Some(home), "hook post-tool-use", Some("s1"), "boom");
        let tail = tail_errors(home, 5);
        assert_eq!(tail.len(), 1);
        assert!(
            tail[0].contains("ERROR hook post-tool-use s1 boom"),
            "{}",
            tail[0]
        );

        let log = crate::home::logs_dir(home).join("errors.log");
        std::fs::write(&log, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        error(Some(home), "x", None, "after rotate");
        assert!(log.with_extension("log.1").exists());
        assert_eq!(tail_errors(home, 5).len(), 1);
    }
}
