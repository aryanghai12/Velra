//! Claude Code version compatibility.
//!
//! Every version-dependent hook capability is encoded here as a constant,
//! verified against the official hooks reference
//! (<https://code.claude.com/docs/en/hooks>) and the changelog at build time.
//! Sources are noted per constant; see DECISIONS.md for the floors chosen
//! where the changelog does not name an introducing version.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// Runs `cmd` and returns its stdout when it exits successfully within
/// `timeout`. A child still running at the deadline is killed, not left
/// behind: `detect` runs from `enable`, `status` and `doctor`, and a hung
/// `claude --version` used to outlive them.
///
/// Killing reaches the child only. When the child is `cmd.exe` running a
/// `.cmd` shim, a process the shim started keeps running until it ends by
/// itself; it is no longer waited for.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(64 * 1024).read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    // A grandchild can hold the pipe open after the child exits.
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(100));
    String::from_utf8(rx.recv_timeout(left).ok()?).ok()
}

/// The `claude` a shell would run, looked up in the absolute `PATH` entries
/// only.
///
/// Launching `claude` by name let the lookup reach the current directory:
/// `cmd /C claude` searches it before `PATH`, and a relative or empty `PATH`
/// entry means it everywhere. `velra enable` and `status` are run from inside
/// repositories, so a `claude.cmd` checked into one ran as the user
/// (reproduced), and the version it printed chose the hooks `enable` wrote.
fn find_claude(path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude.bat"]
    } else {
        &["claude"]
    };
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// `<claude> --version`, started in its own directory and, on Windows, with
/// `cmd.exe`'s search of the current directory turned off for anything a
/// `.cmd` shim runs in turn (`node`).
fn claude_version(exe: &Path, timeout: Duration) -> Option<Version> {
    let mut cmd = Command::new(exe);
    cmd.arg("--version");
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    cmd.env("NoDefaultCurrentDirectoryInExePath", "1");
    Version::parse(&run_with_timeout(cmd, timeout)?)
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
    // Windows npm installs expose `claude.cmd`; `Command` runs a batch file
    // through `cmd.exe` itself, with its arguments escaped.
    if let Some(exe) = find_claude(std::env::var_os("PATH").as_deref()) {
        if let Some(v) = claude_version(&exe, timeout.min(Duration::from_secs(3))) {
            return Detection::Cli(v);
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

    /// A script that sleeps, then records that it finished and prints a
    /// version.
    fn slow_script(dir: &Path, seconds: u32) -> PathBuf {
        if cfg!(windows) {
            let p = dir.join("slow.cmd");
            let body = format!(
                "@echo off\r\nping -n {} 127.0.0.1 >nul\r\necho done> \"%~dp0finished\"\r\necho 2.1.300\r\n",
                seconds + 1
            );
            std::fs::write(&p, body).unwrap();
            p
        } else {
            let p = dir.join("slow");
            let body = format!(
                "#!/bin/sh\nsleep {seconds}\necho done > \"$(dirname \"$0\")/finished\"\necho 2.1.300\n"
            );
            std::fs::write(&p, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        }
    }

    /// A child still running at the deadline is killed, not left running: it
    /// never gets as far as recording that it finished. Before, the timeout
    /// only stopped waiting, and the script ran to its end.
    #[test]
    fn a_timed_out_child_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let script = slow_script(dir.path(), 2);
        let started = Instant::now();
        assert_eq!(
            run_with_timeout(Command::new(&script), Duration::from_millis(300)),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(2), "bounded");
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !dir.path().join("finished").exists(),
            "the timed-out child ran to completion"
        );

        // Within the deadline, its output is the answer.
        let fast = slow_script(dir.path(), 0);
        let out = run_with_timeout(Command::new(&fast), Duration::from_secs(20));
        assert_eq!(out.as_deref().map(str::trim), Some("2.1.300"));
    }

    /// Only an absolute `PATH` entry is searched: a relative or empty one
    /// would mean the current directory, which is the repository `velra` is
    /// run from.
    #[test]
    fn claude_is_looked_up_in_absolute_path_entries_only() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let name = if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        };
        let script = slow_script(&bin, 0);
        std::fs::rename(&script, bin.join(name)).unwrap();

        let rel = std::ffi::OsString::from("bin");
        assert_eq!(find_claude(Some(&rel)), None);
        let joined =
            std::env::join_paths([PathBuf::from(""), PathBuf::from("."), bin.clone()]).unwrap();
        assert_eq!(find_claude(Some(&joined)), Some(bin.join(name)));
        assert_eq!(find_claude(None), None);
        assert_eq!(
            claude_version(&bin.join(name), Duration::from_secs(20)),
            Some(Version::new(2, 1, 300))
        );
    }
}
