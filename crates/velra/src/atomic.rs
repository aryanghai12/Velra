//! Atomic file replacement: temp file in the same directory, fsync, rename,
//! then fsync the directory on POSIX (§6.2 step 8).

use std::io::Write;
use std::path::{Path, PathBuf};

fn temp_path(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "velra".into());
    dir.join(format!(".{name}.velra-tmp-{}", std::process::id()))
}

/// Writes `bytes` to `path` atomically, preserving the existing file's
/// permissions when there is one. On failure `path` is as it was and the
/// temp file is gone.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && !dir.is_dir() {
            crate::home::ensure_dir(dir)?;
        }
    }
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
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()
    })();
    #[cfg(unix)]
    if written.is_ok() {
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }
    if let Err(e) = written.and_then(|()| replace(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
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

/// Writes a new file and fsyncs it (used for backups).
pub fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.is_dir() {
            crate::home::ensure_dir(dir)?;
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
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
