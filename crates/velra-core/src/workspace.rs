//! Workspace identity: the single definition of "which project is this".
//!
//! # Why this is one function and not two
//!
//! `workspace_id` is the key the staged capsule is filed under. `velra
//! restore` computes it to decide where to *write*, and the `SessionStart`
//! hook computes it to decide where to *read*. If those two computations ever
//! disagree, restore does not fail loudly — it silently never delivers, which
//! is the worst possible failure for this feature.
//!
//! Before this module the two lived apart: `hook.rs::project_root` and
//! `cli.rs::workspace_for_cwd`. Their tails were identical, character for
//! character, but their heads were not — the hook honoured
//! `CLAUDE_PROJECT_DIR` and the CLI did not. Anywhere that variable is set to
//! something other than the repository root, the hook and the CLI produced
//! different ids for the same directory.
//!
//! Both now call [`resolve`], so agreement is a property of there being one
//! implementation rather than of two implementations being kept in step.
//! `hook_and_cli_agree_on_workspace_identity` asserts it anyway.
//!
//! # The mapping, unchanged
//!
//! ```text
//! root  := $CLAUDE_PROJECT_DIR, else first ancestor of cwd holding .git, else cwd   (§8.2)
//! root' := normalize_abs(canonical(root))
//! id    := blake3(identity(root'))[0..16]
//! ```
//!
//! This is what `hook.rs` has always done. The only behaviour that moved is
//! that the CLI now honours `CLAUDE_PROJECT_DIR` too, which is the change that
//! makes the two agree.

use crate::{git, hash, paths};
use std::path::{Path, PathBuf};

/// Length of a workspace id in hex characters.
pub const ID_HEX_LEN: usize = 16;

/// §8.2: `CLAUDE_PROJECT_DIR`, else the first ancestor with `.git`, else cwd.
///
/// `cwd` is the directory the caller was invoked in — the hook passes what
/// Claude Code reported, the CLI passes `None` and lets the process's own
/// working directory stand in.
pub fn root(cwd: Option<&str>) -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_PROJECT_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let start = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    git::find_repo_root(&start).unwrap_or(start)
}

/// The canonical, `/`-separated string form a workspace is recorded under.
pub fn root_string(root: &Path) -> String {
    let canonical = paths::canonical(root).unwrap_or_else(|| root.to_path_buf());
    paths::normalize_abs(&canonical.to_string_lossy())
}

/// `blake3(identity(root))[0..16]`, where `root` is already in the form
/// [`root_string`] returns.
pub fn id(root_string: &str) -> String {
    hash::hex_prefix(paths::identity(root_string).as_bytes(), ID_HEX_LEN)
}

/// The workspace of a directory: `(workspace_id, canonical root)`.
///
/// The one entry point. Everything that needs to agree about workspace
/// identity calls this and nothing else.
pub fn resolve(cwd: Option<&str>) -> (String, String) {
    let root_str = root_string(&root(cwd));
    (id(&root_str), root_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two directories that are the same directory reached by different
    /// spellings must hash the same, because the staged capsule of one has to
    /// be findable from the other.
    #[test]
    fn the_same_directory_always_gets_the_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();

        let a = id(&root_string(&root));
        let b = id(&root_string(&root.join(".")));
        assert_eq!(a, b);
        assert_eq!(a.len(), ID_HEX_LEN);

        // A different directory gets a different id.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_ne!(a, id(&root_string(&other)));
    }

    /// A subdirectory of a repository resolves to the repository, so running
    /// `velra restore` from `src/` stages where `SessionStart` will look.
    #[test]
    fn a_subdirectory_resolves_to_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();

        let from_root = root(Some(&repo.to_string_lossy()));
        let from_deep = root(Some(&repo.join("src/deep").to_string_lossy()));
        assert_eq!(
            id(&root_string(&from_root)),
            id(&root_string(&from_deep)),
            "a subdirectory must resolve to its repository"
        );
    }

    /// Without a `.git` anywhere above it, the directory itself is the
    /// workspace — Velra is useful outside a repository too.
    #[test]
    fn a_directory_without_a_repository_is_its_own_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("no-git");
        std::fs::create_dir_all(&plain).unwrap();
        let resolved = root(Some(&plain.to_string_lossy()));
        assert_eq!(
            paths::identity(&root_string(&resolved)),
            paths::identity(&root_string(&plain))
        );
    }
}
