//! Git metadata without spawning `git` (§13.7): HEAD, branch, worktrees.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitInfo {
    /// Current branch, `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Full HEAD commit sha, `None` on an unborn branch.
    pub head: Option<String>,
}

impl GitInfo {
    pub fn short_sha(&self) -> Option<&str> {
        self.head.as_deref().map(|h| &h[..h.len().min(7)])
    }
}

/// Resolves the git directory for a work tree root: `.git` directory, or the
/// target of a `.git` file (`gitdir: …`, used by worktrees and submodules).
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    let meta = std::fs::metadata(&dot).ok()?;
    if meta.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let p = Path::new(target);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    resolved.is_dir().then_some(resolved)
}

/// The common dir holding refs (differs from `git_dir` for linked worktrees).
fn common_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(s) => {
            let p = Path::new(s.trim());
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                git_dir.join(p)
            }
        }
        Err(_) => git_dir.to_path_buf(),
    }
}

fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn resolve_ref(git_dir: &Path, common: &Path, name: &str, depth: u8) -> Option<String> {
    if depth > 5 {
        return None;
    }
    for base in [git_dir, common] {
        if let Ok(s) = std::fs::read_to_string(base.join(name)) {
            let s = s.trim();
            if let Some(target) = s.strip_prefix("ref:") {
                return resolve_ref(git_dir, common, target.trim(), depth + 1);
            }
            if is_sha(s) {
                return Some(s.to_string());
            }
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        if line.starts_with('#') || line.starts_with('^') {
            return None;
        }
        let (sha, refname) = line.split_once(' ')?;
        (refname.trim() == name && is_sha(sha)).then(|| sha.to_string())
    })
}

/// Reads branch and HEAD for the work tree at `root`; `None` if not a repo.
pub fn head_info(root: &Path) -> Option<GitInfo> {
    let gd = git_dir(root)?;
    let common = common_dir(&gd);
    let head = std::fs::read_to_string(gd.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(target) = head.strip_prefix("ref:") {
        let target = target.trim();
        let branch = target
            .strip_prefix("refs/heads/")
            .unwrap_or(target)
            .to_string();
        let sha = resolve_ref(&gd, &common, target, 0);
        Some(GitInfo {
            branch: Some(branch),
            head: sha,
        })
    } else if is_sha(head) {
        Some(GitInfo {
            branch: None,
            head: Some(head.to_string()),
        })
    } else {
        Some(GitInfo::default())
    }
}

/// Walks up from `start` to the first directory containing `.git`.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn loose_packed_detached_and_worktree() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let g = root.join(".git");
        std::fs::create_dir_all(g.join("refs/heads")).unwrap();
        std::fs::write(g.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        // unborn
        assert_eq!(
            head_info(root),
            Some(GitInfo {
                branch: Some("main".into()),
                head: None
            })
        );
        // packed
        std::fs::write(
            g.join("packed-refs"),
            format!("# pack-refs\n{SHA} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(head_info(root).unwrap().head.as_deref(), Some(SHA));
        // loose wins
        let other = "fedcba9876543210fedcba9876543210fedcba98";
        std::fs::write(g.join("refs/heads/main"), format!("{other}\n")).unwrap();
        let info = head_info(root).unwrap();
        assert_eq!(info.head.as_deref(), Some(other));
        assert_eq!(info.short_sha(), Some("fedcba9"));
        // detached
        std::fs::write(g.join("HEAD"), format!("{SHA}\n")).unwrap();
        assert_eq!(
            head_info(root),
            Some(GitInfo {
                branch: None,
                head: Some(SHA.into())
            })
        );

        // linked worktree: .git file → gitdir with commondir
        let wt_git = g.join("worktrees/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = root.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(head_info(&wt).unwrap().head.as_deref(), Some(other));
        std::fs::create_dir_all(wt.join("x/y")).unwrap();
        assert_eq!(find_repo_root(&wt.join("x/y")).unwrap(), wt);
    }
}
