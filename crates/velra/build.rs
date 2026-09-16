//! Captures the git sha and target triple for `velra --version`.

use std::path::{Path, PathBuf};

fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=VELRA_GIT_SHA={sha}");
    println!(
        "cargo:rustc-env=VELRA_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    for path in rerun_paths() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// Everything whose change means `git rev-parse HEAD` would answer differently.
///
/// Watching `.git/HEAD` alone is not enough, and getting this wrong is not
/// cosmetic. On a branch, `HEAD` holds `ref: refs/heads/main` and does not
/// change when you commit — only the ref file does — so cargo never re-runs
/// this script and the binary keeps reporting the sha it was first built at.
/// The v0.1 benchmark attributed a dataset to `e9f40151c` while the tree it
/// was built from was several commits further on, which is exactly this bug
/// turning a provenance claim into a guess.
///
/// So: `HEAD` (to catch a checkout), the ref `HEAD` points at (to catch a
/// commit), and `packed-refs` (a ref that has been packed away has no loose
/// file of its own).
fn rerun_paths() -> Vec<PathBuf> {
    let Some(git_dir) = git_dir(Path::new("../../.git")) else {
        // A published crate has no `.git`, and naming a path that does not
        // exist makes cargo rebuild on every run.
        return Vec::new();
    };

    let mut paths = Vec::new();
    let head = git_dir.join("HEAD");
    if !head.is_file() {
        return paths;
    }
    paths.push(head.clone());

    if let Ok(text) = std::fs::read_to_string(&head) {
        if let Some(reference) = text.trim().strip_prefix("ref:") {
            let reference = reference.trim();
            // Branch names may contain slashes, so the ref file lives one or
            // more directories deep. Only emit it when it exists: a packed ref
            // has no loose file, and naming a missing path rebuilds every run.
            let loose = git_dir.join(reference.replace('/', std::path::MAIN_SEPARATOR_STR));
            if loose.is_file() {
                paths.push(loose);
            }
        }
    }
    let packed = git_dir.join("packed-refs");
    if packed.is_file() {
        paths.push(packed);
    }
    paths
}

/// Resolves `.git`, which is a directory in a normal checkout and a file
/// holding `gitdir: <path>` in a worktree or a submodule.
fn git_dir(candidate: &Path) -> Option<PathBuf> {
    if candidate.is_dir() {
        return Some(candidate.to_path_buf());
    }
    if candidate.is_file() {
        let text = std::fs::read_to_string(candidate).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        let path = Path::new(target);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            candidate.parent()?.join(path)
        };
        return resolved.is_dir().then_some(resolved);
    }
    None
}
