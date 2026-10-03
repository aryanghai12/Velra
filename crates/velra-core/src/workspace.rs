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
//! One case the shared mapping cannot settle alone: Claude Code started in a
//! repository's *subdirectory* sets `CLAUDE_PROJECT_DIR` to it, and a terminal
//! in that directory has no such variable, so the mapping gives it the
//! repository root. The CLI therefore resolves through [`resolve_recorded`],
//! which prefers the nearest directory the ledger has recorded as a workspace
//! (`tests/workspace_identity.rs` drives both binaries, each in its own
//! environment).
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
    root_in(project_dir(), cwd)
}

/// `CLAUDE_PROJECT_DIR`, when set and not empty.
fn project_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("CLAUDE_PROJECT_DIR").filter(|v| !v.is_empty())
}

fn start_dir(cwd: Option<&str>) -> PathBuf {
    cwd.map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// [`root`] with the variable passed in, so a test decides it rather than the
/// environment the test runs in -- inside Claude Code the variable is set, and
/// every answer would be it.
fn root_in(project_dir: Option<std::ffi::OsString>, cwd: Option<&str>) -> PathBuf {
    if let Some(dir) = project_dir {
        return PathBuf::from(dir);
    }
    let start = start_dir(cwd);
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

/// The workspace a command typed in a terminal addresses: [`resolve`], except
/// that inside a repository the nearest directory from `cwd` up to the
/// repository root that `known` recognises as a recorded workspace wins.
///
/// The hook's workspace is the directory Claude Code was started in
/// (`CLAUDE_PROJECT_DIR`). A terminal has no such variable, and [`resolve`]
/// falls back to the repository root, so a session started in a repository's
/// subdirectory -- a package of a monorepo -- was recorded under one id and
/// `velra restore` run in that same directory looked under another: it listed
/// the root's sessions, or none, and a capsule it staged was never delivered.
/// The ledger knows which directories sessions were started in, and the
/// nearest one is the one the user is in.
///
/// With `CLAUDE_PROJECT_DIR` set the answer is [`resolve`]'s, as in the hook.
/// Outside a repository there is nothing to bound a search upwards, so only
/// `cwd` itself is tried before falling back.
pub fn resolve_recorded(cwd: Option<&str>, known: impl Fn(&str) -> bool) -> (String, String) {
    resolve_recorded_in(project_dir(), cwd, known)
}

fn resolve_recorded_in(
    project_dir: Option<std::ffi::OsString>,
    cwd: Option<&str>,
    known: impl Fn(&str) -> bool,
) -> (String, String) {
    if project_dir.is_none() {
        let start = start_dir(cwd);
        let repo = git::find_repo_root(&start);
        for dir in start.ancestors() {
            let root_str = root_string(dir);
            let id = id(&root_str);
            if known(&id) {
                return (id, root_str);
            }
            if repo.as_deref().is_none_or(|r| dir == r) {
                break;
            }
        }
    }
    let root_str = root_string(&root_in(project_dir, cwd));
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

        let from_root = root_in(None, Some(&repo.to_string_lossy()));
        let from_deep = root_in(None, Some(&repo.join("src/deep").to_string_lossy()));
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
        let resolved = root_in(None, Some(&plain.to_string_lossy()));
        assert_eq!(
            paths::identity(&root_string(&resolved)),
            paths::identity(&root_string(&plain))
        );
    }

    /// A terminal in a repository's subdirectory addresses the workspace a
    /// session was started in there, when there is one; otherwise the
    /// repository root, as before.
    #[test]
    fn a_recorded_workspace_nearer_than_the_repository_root_wins() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let pkg = repo.join("packages/foo");
        let deep = pkg.join("src/deep");
        std::fs::create_dir_all(&deep).unwrap();
        let repo_id = id(&root_string(&repo));
        let pkg_id = id(&root_string(&pkg));
        let at = |d: &Path, known: &[&str]| {
            resolve_recorded_in(None, Some(&d.to_string_lossy()), |i| known.contains(&i)).0
        };

        // Nothing recorded: the repository root, exactly as `resolve`.
        assert_eq!(at(&deep, &[]), repo_id);
        // A session started in the package: from the package or below it.
        assert_eq!(at(&pkg, &[&pkg_id, &repo_id]), pkg_id);
        assert_eq!(at(&deep, &[&pkg_id, &repo_id]), pkg_id);
        // Only the root recorded: the root.
        assert_eq!(at(&deep, &[&repo_id]), repo_id);
        // The search stops at the repository root: a recorded directory above
        // it is another workspace.
        let above = id(&root_string(dir.path()));
        assert_eq!(at(&deep, &[&above]), repo_id);
        // `CLAUDE_PROJECT_DIR` decides, as it does in the hook.
        let (by_var, _) = resolve_recorded_in(
            Some(pkg.clone().into_os_string()),
            Some(&repo.to_string_lossy()),
            |i| i == repo_id,
        );
        assert_eq!(by_var, pkg_id);
    }

    /// Outside a repository nothing bounds a search upwards, so a directory
    /// recorded above `cwd` is not taken.
    #[test]
    fn outside_a_repository_only_the_directory_itself_is_tried() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        let sub = plain.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let plain_id = id(&root_string(&plain));
        let sub_id = id(&root_string(&sub));
        let at = |d: &Path, known: &[&str]| {
            resolve_recorded_in(None, Some(&d.to_string_lossy()), |i| known.contains(&i)).0
        };
        assert_eq!(at(&plain, &[&plain_id]), plain_id);
        assert_eq!(at(&sub, &[&plain_id]), sub_id);
    }
}
