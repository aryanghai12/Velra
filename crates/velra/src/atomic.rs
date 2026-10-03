//! Atomic file replacement: temp file in the same directory, fsync, rename,
//! then fsync the directory on POSIX (§6.2 step 8).

use std::io::Write;
use std::path::{Path, PathBuf};

fn temp_prefix(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "velra".into());
    format!(".{name}.velra-tmp-")
}

fn temp_path(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{}{}", temp_prefix(path), std::process::id()))
}

/// A temp file older than this was left by a run that was killed between
/// writing it and renaming it; no live write takes this long.
const STALE_TEMP: std::time::Duration = std::time::Duration::from_secs(60);

/// Removes the temp files that killed runs left beside `path`. Each run
/// names its own by process id, so they were never overwritten or removed
/// (D144); a backup's temp, named after a path used once, never would be.
/// Any Velra temp file in the directory counts. A fresh one may be another
/// run's, mid-write, and is left alone.
fn remove_stale_temps(path: &Path) {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = name.starts_with('.')
            && name.contains(".velra-tmp-")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= STALE_TEMP);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `VELRA_TEST_*` stalls inside a replacement, for tests that kill the
/// process there (fault-injection builds only). `VELRA_TEST_STALL_TARGET`,
/// when set, limits them to files whose name ends with it.
#[cfg(feature = "fault-injection")]
fn stall(var: &str, path: &Path) {
    if let Ok(target) = std::env::var("VELRA_TEST_STALL_TARGET") {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if !name.is_some_and(|n| n.ends_with(&target)) {
            return;
        }
    }
    if let Some(ms) = std::env::var(var).ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Writes `bytes` to `path` atomically, preserving the existing file's
/// permissions when there is one. On failure `path` is as it was and the
/// temp file is gone.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_if(path, bytes, || Ok(true)).map(|_| ())
}

/// [`write`], replacing `path` only if `unchanged` still says so once the
/// new content is on disk: it runs after the temp file is written and
/// synced, immediately before the rename. `Ok(false)` when it did not, with
/// `path` untouched and the temp file gone.
///
/// Checking before writing the temp file left the write and its fsync
/// between the check and the rename, and a program that saved the file in
/// that time lost what it saved (reproduced for `settings.json`, D143).
/// What remains is the time between one read and the rename; no file
/// system offers a compare-and-swap to close it.
pub fn write_if(
    path: &Path,
    bytes: &[u8],
    unchanged: impl FnOnce() -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && !dir.is_dir() {
            crate::home::ensure_dir(dir)?;
        }
    }
    remove_stale_temps(path);
    let tmp = temp_path(path);
    let written = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(feature = "fault-injection")]
        {
            let half = bytes.len() / 2;
            f.write_all(&bytes[..half])?;
            f.flush()?;
            stall("VELRA_TEST_STALL_MID_TEMP_WRITE_MS", path);
            f.write_all(&bytes[half..])?;
        }
        #[cfg(not(feature = "fault-injection"))]
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()
    })();
    #[cfg(feature = "fault-injection")]
    stall("VELRA_TEST_STALL_BEFORE_REPLACE_MS", path);
    #[cfg(unix)]
    if written.is_ok() {
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }
    #[cfg(test)]
    BEFORE_CHECK.with(|h| {
        let f = h.borrow_mut().take();
        if let Some(mut f) = f {
            f()
        }
    });
    match written.and_then(|()| unchanged()) {
        Ok(true) => {}
        Ok(false) => {
            let _ = std::fs::remove_file(&tmp);
            return Ok(false);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    if let Err(e) = replace(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(true)
}

#[cfg(test)]
thread_local! {
    /// Runs once the new content is written and synced, before `unchanged`:
    /// where another program's save lands, opened by the tests instead of
    /// by timing.
    pub static BEFORE_CHECK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// How long a replacement another program is blocking is retried.
#[cfg(windows)]
const REPLACE_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// `rename(tmp, path)`, retried on Windows while another program holds
/// `path` open without delete sharing -- an editor saving it, a virus
/// scanner or the search indexer reading it. Windows refuses the rename then
/// (access denied, a sharing or lock violation) where POSIX would not, and
/// the holder usually lets go within milliseconds. A read-only file is
/// refused the same way and is not retried: it will not change.
fn replace(tmp: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let deadline = std::time::Instant::now() + REPLACE_RETRY;
        loop {
            match std::fs::rename(tmp, path) {
                Err(e) if is_held_open(&e, path) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                r => return r,
            }
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(tmp, path)
}

/// `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` or `ERROR_LOCK_VIOLATION`
/// on a file that is not read-only.
#[cfg(windows)]
fn is_held_open(e: &std::io::Error, path: &Path) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32 | 33)) && !is_read_only(path)
}

/// Whether `path` carries the read-only attribute (Windows) or has no write
/// permission bit (POSIX).
pub fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write(&p, b"{}\n").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{}\n");
        write(&p, b"{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":1}");
        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("velra-tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
