//! Claude Code version compatibility.
//!
//! Every version-dependent hook capability is encoded here as a constant,
//! verified against the official hooks reference
//! (<https://code.claude.com/docs/en/hooks>) and the changelog at build time.
//! Sources are noted per constant; see DECISIONS.md for the floors chosen
//! where the changelog does not name an introducing version.

use std::process::{Command, Stdio};
use std::time::Duration;

/// A Claude Code semantic version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// Parses a leading `x.y.z` from text such as `2.1.268 (Claude Code)`,
    /// `v1.0.62` or `2.1.268-win32-x64`.
    pub fn parse(text: &str) -> Option<Version> {
        let token = text
            .split_whitespace()
            .map(|t| t.trim_start_matches('v'))
            .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.'))?;
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let patch = parts
            .next()
            .unwrap_or("0")
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Newest Claude Code version known when this binary was built.
pub const LATEST_KNOWN: Version = Version::new(2, 1, 269);

/// `SessionStart` hook (changelog 1.0.62).
pub const MIN_SESSION_START: Version = Version::new(1, 0, 62);
/// `SessionEnd` hook (changelog 1.0.85).
pub const MIN_SESSION_END: Version = Version::new(1, 0, 85);
/// `PreCompact` hook (changelog 1.0.48).
pub const MIN_PRE_COMPACT: Version = Version::new(1, 0, 48);
/// `PostCompact` hook (changelog 2.1.76).
pub const MIN_POST_COMPACT: Version = Version::new(2, 1, 76);
/// Async command hooks (`"async": true`) — first changelog mention 2.1.23.
pub const MIN_ASYNC: Version = Version::new(2, 1, 23);
/// Conditional `if` field using permission-rule syntax (changelog 2.1.85).
pub const MIN_IF_FIELD: Version = Version::new(2, 1, 85);
/// Exec form `args: string[]` (changelog 2.1.139).
pub const MIN_EXEC_FORM: Version = Version::new(2, 1, 139);
/// `PostToolUseFailure` hook — first changelog mention 2.1.119.
pub const MIN_POST_TOOL_USE_FAILURE: Version = Version::new(2, 1, 119);
/// `PostToolBatch` hook — documented, no introducing version in the
/// changelog; verified present in 2.1.268.
pub const MIN_POST_TOOL_BATCH: Version = Version::new(2, 1, 268);
/// Windows `PowerShell` tool (changelog 2.1.84).
pub const MIN_POWERSHELL_TOOL: Version = Version::new(2, 1, 84);
/// `prompt_id` in hook input (docs: requires 2.1.196).
pub const MIN_PROMPT_ID: Version = Version::new(2, 1, 196);

/// Hook output strings are capped at 10,000 characters; longer output is
/// spilled to a file (docs, "JSON output").
pub const HOOK_OUTPUT_MAX_CHARS: usize = 10_000;
/// Capsule ceiling, staying under the spill limit (§8.4).
pub const CAPSULE_MAX_CHARS: usize = 9_500;

/// Capabilities of a detected (or assumed) Claude Code version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    pub exec_form: bool,
    pub if_field: bool,
    pub async_hooks: bool,
    pub session_start: bool,
    pub session_end: bool,
    pub pre_compact: bool,
    pub post_compact: bool,
    pub post_tool_use_failure: bool,
    pub post_tool_batch: bool,
    pub powershell_tool: bool,
    /// Hook input carries `prompt_id` (otherwise delivery keys use timestamps).
    pub prompt_id: bool,
}

impl Features {
    /// Capabilities for `version`; `None` assumes the latest known version.
    pub fn for_version(version: Option<Version>) -> Features {
        let v = version.unwrap_or(LATEST_KNOWN);
        Features {
            exec_form: v >= MIN_EXEC_FORM,
            if_field: v >= MIN_IF_FIELD,
            async_hooks: v >= MIN_ASYNC,
            session_start: v >= MIN_SESSION_START,
            session_end: v >= MIN_SESSION_END,
            pre_compact: v >= MIN_PRE_COMPACT,
            post_compact: v >= MIN_POST_COMPACT,
            post_tool_use_failure: v >= MIN_POST_TOOL_USE_FAILURE,
            post_tool_batch: v >= MIN_POST_TOOL_BATCH,
            powershell_tool: v >= MIN_POWERSHELL_TOOL,
            prompt_id: v >= MIN_PROMPT_ID,
        }
    }
}

/// How the Claude Code version was determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// `claude --version` succeeded.
    Cli(Version),
    /// Version read from an installed VS Code extension directory.
    Extension(Version),
    /// Not found; the latest known version is assumed.
    Assumed,
}

impl Detection {
    pub fn version(&self) -> Option<Version> {
        match self {
            Detection::Cli(v) | Detection::Extension(v) => Some(*v),
            Detection::Assumed => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Detection::Cli(v) => format!("{v}"),
            Detection::Extension(v) => format!("{v} (from VS Code extension)"),
            Detection::Assumed => format!("not detected, assuming {LATEST_KNOWN}"),
        }
    }
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<String> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) if out.status.success() => String::from_utf8(out.stdout).ok(),
        _ => None,
    }
}

/// Highest Claude Code version among installed VS Code extensions.
fn version_from_vscode_extension() -> Option<Version> {
    let home = crate::home::user_home()?;
    let mut best: Option<Version> = None;
    for dir in [
        home.join(".vscode/extensions"),
        home.join(".vscode-server/extensions"),
        home.join(".cursor/extensions"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("anthropic.claude-code-") else {
                continue;
            };
            if let Some(v) = Version::parse(rest) {
                best = Some(best.map_or(v, |b: Version| b.max(v)));
            }
        }
    }
    best
}

/// Detects the installed Claude Code version (§6.2 step 4), 3 s timeout.
pub fn detect(timeout: Duration) -> Detection {
    // Escape hatch: pin the assumed version (used by tests, and by anyone
    // whose Claude Code is not on PATH).
    if let Some(v) = std::env::var("VELRA_CLAUDE_VERSION")
        .ok()
        .and_then(|s| Version::parse(&s))
    {
        return Detection::Cli(v);
    }
    let mut claude = Command::new("claude");
    claude.arg("--version");
    if let Some(out) = run_with_timeout(claude, timeout.min(Duration::from_secs(3))) {
        if let Some(v) = Version::parse(&out) {
            return Detection::Cli(v);
        }
    }
    // Windows npm installs expose `claude.cmd`, which needs a shell.
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "claude", "--version"]);
        if let Some(out) = run_with_timeout(cmd, timeout.min(Duration::from_secs(3))) {
            if let Some(v) = Version::parse(&out) {
                return Detection::Cli(v);
            }
        }
    }
    match version_from_vscode_extension() {
        Some(v) => Detection::Extension(v),
        None => Detection::Assumed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(
            Version::parse("2.1.268 (Claude Code)"),
            Some(Version::new(2, 1, 268))
        );
        assert_eq!(Version::parse("v1.0.62"), Some(Version::new(1, 0, 62)));
        assert_eq!(
            Version::parse("2.1.268-win32-x64"),
            Some(Version::new(2, 1, 268))
        );
        assert_eq!(
            Version::parse("Claude Code 2.0.1"),
            Some(Version::new(2, 0, 1))
        );
        assert_eq!(Version::parse("no numbers here"), None);
        assert!(Version::new(2, 1, 139) > Version::new(2, 1, 85));
    }

    #[test]
    fn feature_gates() {
        let latest = Features::for_version(None);
        assert!(
            latest.exec_form && latest.if_field && latest.post_tool_batch && latest.async_hooks
        );
        let old = Features::for_version(Some(Version::new(2, 1, 100)));
        assert!(!old.exec_form, "exec form needs 2.1.139");
        assert!(old.if_field, "if field exists since 2.1.85");
        assert!(!old.post_tool_batch);
        assert!(old.post_compact);
        let ancient = Features::for_version(Some(Version::new(1, 0, 50)));
        assert!(!ancient.session_end && !ancient.async_hooks && ancient.pre_compact);
    }
}
