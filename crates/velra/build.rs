//! Captures the git sha and target triple for `velra --version`.

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
    // Only when building from a checkout. A published crate has no .git, and
    // naming a path that does not exist makes cargo rebuild on every run.
    if std::path::Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
