//! Path normalization (§9.1) and sensitive-path detection (§9.2).
//!
//! Stored paths are project-relative with `/` separators, or absolute (also
//! with `/`) when outside the project root.

use std::path::{Path, PathBuf};

/// Replaces `\` with `/`.
pub fn to_slash(p: &str) -> String {
    p.replace('\\', "/")
}

/// `/`-separated form with Windows verbatim prefixes removed and the drive
/// letter upper-cased.
pub fn normalize_abs(p: &str) -> String {
    let mut s = to_slash(p);
    for prefix in ["//?/UNC/", "//?/", "//./"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = if prefix.ends_with("UNC/") {
                format!("//{rest}")
            } else {
                rest.to_string()
            };
            break;
        }
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let mut owned = s.into_bytes();
        owned[0] = owned[0].to_ascii_uppercase();
        s = String::from_utf8(owned).unwrap_or_default();
    }
    s
}

/// Windows paths compare case-insensitively; others exactly.
pub fn paths_equal(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

fn starts_with_dir(path: &str, dir: &str) -> Option<usize> {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() || path.len() <= dir.len() {
        return None;
    }
    let (head, tail) = path.split_at(dir.len());
    if paths_equal(head, dir) && tail.starts_with('/') {
        Some(dir.len() + 1)
    } else {
        None
    }
}

/// Project-relative display path for `abs`, or the normalized absolute path
/// when outside `root`.
pub fn relative_to_root(abs: &str, root: &str) -> String {
    let a = normalize_abs(abs);
    let r = normalize_abs(root);
    match starts_with_dir(&a, &r) {
        Some(cut) => a[cut..].to_string(),
        None if paths_equal(&a, r.trim_end_matches('/')) => ".".to_string(),
        None => a,
    }
}

/// Resolves a stored (relative or absolute) path against `root`.
pub fn resolve(stored: &str, root: &Path) -> PathBuf {
    let p = Path::new(stored);
    if is_absolute_str(stored) {
        p.to_path_buf()
    } else {
        root.join(stored)
    }
}

/// Absolute on either platform's conventions (`/x`, `C:/x`, `C:\x`, `\\srv`).
pub fn is_absolute_str(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with('\\')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'/' || b[2] == b'\\'))
}

/// Canonical form of an existing path without Windows verbatim prefixes.
pub fn canonical(p: &Path) -> Option<PathBuf> {
    let c = std::fs::canonicalize(p).ok()?;
    Some(PathBuf::from(
        normalize_abs(&c.to_string_lossy()).replace('/', std::path::MAIN_SEPARATOR_STR),
    ))
}

/// Identity string for hashing (project ids): `/` separators, case-folded on
/// Windows.
pub fn identity(p: &str) -> String {
    let n = normalize_abs(p);
    let n = n.trim_end_matches('/').to_string();
    if cfg!(windows) {
        n.to_lowercase()
    } else {
        n
    }
}

/// Sensitive-path globs from §9.2, matched case-insensitively.
pub fn is_sensitive(path: &str) -> bool {
    let lower = to_slash(path).to_lowercase();
    let comps: Vec<&str> = lower.split('/').filter(|c| !c.is_empty()).collect();
    let Some(name) = comps.last().copied() else {
        return false;
    };
    let in_dir = |d: &str| comps[..comps.len() - 1].contains(&d);
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.ends_with(".kdbx")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name == ".npmrc"
        || name == ".pypirc"
        || name == ".netrc"
        || name.contains("credentials")
        || name.contains("secret")
        || in_dir(".ssh")
        || in_dir(".aws")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativizes() {
        assert_eq!(
            relative_to_root("/home/u/proj/src/a.rs", "/home/u/proj"),
            "src/a.rs"
        );
        assert_eq!(
            relative_to_root("/home/u/proj/src/a.rs", "/home/u/proj/"),
            "src/a.rs"
        );
        assert_eq!(
            relative_to_root("/home/u/other/a.rs", "/home/u/proj"),
            "/home/u/other/a.rs"
        );
        assert_eq!(
            relative_to_root("/home/u/project2/a.rs", "/home/u/proj"),
            "/home/u/project2/a.rs"
        );
        assert_eq!(
            relative_to_root(r"c:\proj\src\a.rs", r"C:\proj"),
            "src/a.rs"
        );
        assert_eq!(normalize_abs(r"\\?\C:\x\y"), "C:/x/y");
    }

    #[test]
    fn sensitive() {
        for p in [
            ".env",
            "app/.env.local",
            "certs/server.PEM",
            "k.key",
            "a.p12",
            "b.pfx",
            "id_rsa.pub",
            "id_ed25519",
            ".npmrc",
            ".pypirc",
            ".netrc",
            "aws_credentials.json",
            "my-secret.txt",
            "home/.ssh/config",
            ".aws/config",
            "vault.kdbx",
        ] {
            assert!(is_sensitive(p), "{p}");
        }
        for p in [
            "src/main.rs",
            "environment.ts",
            "keyboard.rs",
            "docs/env.md",
        ] {
            assert!(!is_sensitive(p), "{p}");
        }
    }
}
