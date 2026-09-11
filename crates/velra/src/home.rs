//! `$VELRA_HOME` layout (§10.1), configuration and persisted state.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// User home directory without pulling in a dependency.
pub fn user_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(p));
        }
        let drive = std::env::var_os("HOMEDRIVE")?;
        let path = std::env::var_os("HOMEPATH")?;
        let mut s = drive;
        s.push(path);
        Some(PathBuf::from(s))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

/// `$VELRA_HOME`, default `~/.velra`.
pub fn velra_home() -> Option<PathBuf> {
    match std::env::var_os("VELRA_HOME").filter(|v| !v.is_empty()) {
        Some(v) => Some(PathBuf::from(v)),
        None => Some(user_home()?.join(".velra")),
    }
}

pub fn db_path(home: &Path) -> PathBuf {
    home.join("velra.db")
}
pub fn spool_dir(home: &Path) -> PathBuf {
    home.join("spool")
}
pub fn logs_dir(home: &Path) -> PathBuf {
    home.join("logs")
}
pub fn backups_dir(home: &Path) -> PathBuf {
    home.join("backups")
}
pub fn state_path(home: &Path) -> PathBuf {
    home.join("state.json")
}
pub fn config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}
pub fn disabled_path(home: &Path) -> PathBuf {
    home.join("disabled")
}

/// Kill switch (§8.1 rule 5).
pub fn is_disabled(home: Option<&Path>) -> bool {
    if std::env::var_os("VELRA_DISABLE").is_some_and(|v| v == "1") {
        return true;
    }
    home.is_some_and(|h| disabled_path(h).exists())
}

fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
        {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Creates `$VELRA_HOME` (0700 on POSIX) if missing.
pub fn ensure_home(home: &Path) -> std::io::Result<()> {
    if home.is_dir() {
        return Ok(());
    }
    create_dir_private(home)
}

pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    create_dir_private(dir)
}

/// Optional `~/.velra/config.toml`, parsed only when the file exists.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Capsule target size in estimated tokens (§16.3).
    pub budget_tokens: Option<u32>,
}

impl Config {
    pub fn load(home: &Path) -> Config {
        let path = config_path(home);
        if !path.is_file() {
            return Config::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn render(&self) -> velra_core::render::RenderConfig {
        velra_core::render::RenderConfig {
            budget_tokens: self
                .budget_tokens
                .unwrap_or(velra_core::render::DEFAULT_BUDGET_TOKENS),
        }
    }
}

/// `~/.velra/state.json`: what `enable` did, for `disable` and `doctor`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Stable binary path registered in settings (§5.3).
    pub bin_path: Option<String>,
    pub settings_path: Option<String>,
    /// Whether a `hooks` key existed before `enable` (§6.4).
    pub hooks_existed_before: Option<bool>,
    pub enabled_ms: Option<i64>,
    pub claude_version: Option<String>,
    pub velra_version: Option<String>,
    pub last_backup: Option<String>,
}

impl State {
    pub fn load(home: &Path) -> State {
        std::fs::read_to_string(state_path(home))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, home: &Path) -> std::io::Result<()> {
        ensure_home(home)?;
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::atomic::write(&state_path(home), text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = State {
            bin_path: Some("/x/velra".into()),
            hooks_existed_before: Some(true),
            ..Default::default()
        };
        s.save(dir.path()).unwrap();
        let loaded = State::load(dir.path());
        assert_eq!(loaded.bin_path.as_deref(), Some("/x/velra"));
        assert_eq!(loaded.hooks_existed_before, Some(true));
    }

    #[test]
    fn config_defaults_without_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()).render().budget_tokens, 800);
        std::fs::write(config_path(dir.path()), "budget_tokens = 500\n").unwrap();
        assert_eq!(Config::load(dir.path()).render().budget_tokens, 500);
    }
}
