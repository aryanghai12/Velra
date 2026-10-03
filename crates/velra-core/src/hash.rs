//! Content identity (§13.1) and short ids.

use std::io::Read;
use std::path::Path;

/// Files larger than this are identified by size + mtime instead of content.
pub const MAX_HASH_BYTES: u64 = 4 * 1024 * 1024;

pub const ABSENT: &str = "absent";
pub const UNREADABLE: &str = "unreadable";

/// First `n` hex chars of the blake3 digest of `bytes`.
pub fn hex_prefix(bytes: &[u8], n: usize) -> String {
    let hex = blake3::hash(bytes).to_hex();
    hex[..n.min(hex.len())].to_string()
}

/// Content hash for identity comparisons: 32 hex chars.
pub fn content_hash(bytes: &[u8]) -> String {
    hex_prefix(bytes, 32)
}

/// Hash of a file on disk, with its size.
///
/// * missing → `absent`
/// * larger than 4 MiB → `large:{size}:{mtime_ns}`
/// * unreadable → `unreadable`
pub fn hash_file(path: &Path) -> (String, u64) {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (ABSENT.to_string(), 0),
        Err(_) => return (UNREADABLE.to_string(), 0),
    };
    if !meta.is_file() {
        return (
            if meta.is_dir() { ABSENT } else { UNREADABLE }.to_string(),
            0,
        );
    }
    let size = meta.len();
    if size > MAX_HASH_BYTES {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return (format!("large:{size}:{mtime_ns}"), size);
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (UNREADABLE.to_string(), size),
    };
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                total += n as u64;
                if total > MAX_HASH_BYTES {
                    // Grew while reading; treat as large.
                    return (format!("large:{total}:0"), total);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return (UNREADABLE.to_string(), size),
        }
    }
    let hex = hasher.finalize().to_hex();
    (hex[..32].to_string(), total)
}

/// True for hashes that identify real content (not absent/unreadable).
pub fn is_content(hash: &str) -> bool {
    hash != ABSENT && hash != UNREADABLE
}

/// True only for a digest of the file's bytes ([`content_hash`]'s 32
/// lower-case hex characters): the one form where equal values mean equal
/// content.
///
/// Not for the `absent` / `unreadable` sentinels, and not for a large file's
/// `large:{size}:{mtime_ns}` fingerprint -- two of those are equal whenever
/// size and mtime are, and `large:{n}:0` (a file that grew while it was read,
/// or whose mtime could not be read) is equal for any two files of one size.
pub fn is_digest(hash: &str) -> bool {
    hash.len() == 32 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        assert_eq!(hash_file(&p).0, ABSENT);
        std::fs::write(&p, b"hello").unwrap();
        let (h, size) = hash_file(&p);
        assert_eq!(size, 5);
        assert_eq!(h, content_hash(b"hello"));
        assert_eq!(h.len(), 32);
        assert_eq!(hash_file(dir.path()).0, ABSENT);
        assert!(is_digest(&h));
    }

    #[test]
    fn only_a_byte_digest_is_identity() {
        assert!(is_digest(&content_hash(b"x")));
        for h in [
            ABSENT,
            UNREADABLE,
            "large:5000000:0",
            "large:5000000:1789207445000000000",
            "",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde",
        ] {
            assert!(!is_digest(h), "{h}");
        }
    }
}
