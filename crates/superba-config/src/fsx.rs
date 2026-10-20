//! File system helpers: hashing, optional reads, private files and the atomic
//! replace primitive used by every writer in this crate (plan 9.2).
//!
//! Atomic replace = write a temp file in the target's directory, fsync it, apply
//! the permissions, re-check the target's hash, rename over the target, fsync the
//! directory. Symlinked targets are resolved first so a dotfile manager's link
//! keeps pointing at the (updated) real file.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use superba_core::{Error, Result};

/// sha256 of `bytes` as lowercase hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Short (16 hex chars) sha256, for file names derived from identities and paths.
pub(crate) fn short_hash(bytes: &[u8]) -> String {
    let mut h = sha256_hex(bytes);
    h.truncate(16);
    h
}

/// Read a file; `None` if it does not exist.
pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, &e)),
    }
}

/// sha256 of a file; `None` if it does not exist.
pub(crate) fn file_sha256(path: &Path) -> Result<Option<String>> {
    Ok(read_optional(path)?.map(|b| sha256_hex(&b)))
}

/// Remove a file; missing files are fine.
pub(crate) fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, &e)),
    }
}

/// Create `dir` (and parents). Directories inside Superba's data dir are 0700.
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    set_mode(dir, 0o700)
}

/// Create `dir` (and parents) with default permissions (Codex home, rc dirs).
pub(crate) fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))
}

/// Follow a symlink chain so writes replace the real file, not the link.
/// Non-links and missing paths are returned unchanged.
pub(crate) fn resolve_write_target(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            superba_core::paths::canonical(path).unwrap_or_else(|_| {
                // Dangling link: write through the link target if it is readable.
                fs::read_link(path)
                    .map(|t| match path.parent() {
                        Some(p) if t.is_relative() => p.join(t),
                        _ => t,
                    })
                    .unwrap_or_else(|_| path.to_path_buf())
            })
        }
        _ => path.to_path_buf(),
    }
}

/// Permission bits for a replaced file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Always 0600 (auth.json, Superba's own data, backups).
    Private,
    /// Keep the existing file's mode; 0600 for new files (config.toml may hold MCP tokens).
    PreserveOrPrivate,
    /// Keep the existing file's mode; 0644 for new files (shell rc files).
    PreserveOrPublic,
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| Error::io(path, &e))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    // Windows: files inherit the ACL of the profile directory (owner-only by default).
    Ok(())
}

#[cfg(unix)]
fn existing_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> Option<u32> {
    None
}

fn target_mode(target: &Path, mode: Mode) -> u32 {
    match mode {
        Mode::Private => 0o600,
        Mode::PreserveOrPrivate => existing_mode(target).unwrap_or(0o600),
        Mode::PreserveOrPublic => existing_mode(target).unwrap_or(0o644),
    }
}

/// Write `contents` to a new temp file next to `target`, fsync it and apply the
/// permissions. Returns the temp path (to be published with [`publish`]).
pub(crate) fn stage(target: &Path, contents: &[u8], mode: Mode) -> Result<PathBuf> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_dir(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let temp = dir.join(format!(".{name}.superba-{}.tmp", &id[..12]));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&temp).map_err(|e| Error::io(&temp, &e))?;
    let res = (|| {
        f.write_all(contents)?;
        f.sync_all()
    })();
    if let Err(e) = res {
        drop(f);
        let _ = fs::remove_file(&temp);
        return Err(Error::io(&temp, &e));
    }
    drop(f);
    if let Err(e) = set_mode(&temp, target_mode(target, mode)) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    Ok(temp)
}

/// Rename a staged temp file over `target` and fsync the directory. On Windows a
/// sharing violation (another process has the file open) is retried with a
/// 50 / 200 / 800 ms backoff.
pub(crate) fn publish(temp: &Path, target: &Path) -> Result<()> {
    rename_with_retry(temp, target)?;
    sync_parent(target);
    Ok(())
}

#[cfg(windows)]
fn rename_with_retry(from: &Path, to: &Path) -> Result<()> {
    // std::fs::rename uses MoveFileExW(MOVEFILE_REPLACE_EXISTING) on Windows, which
    // replaces the target atomically on NTFS. ERROR_SHARING_VIOLATION (32),
    // ERROR_LOCK_VIOLATION (33) and ERROR_ACCESS_DENIED (5) are transient while an
    // editor, antivirus or Codex itself holds the file open.
    let mut delays = [50u64, 200, 800].into_iter();
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33)) => match delays.next() {
                Some(ms) => std::thread::sleep(std::time::Duration::from_millis(ms)),
                None => return Err(Error::io(to, &e)),
            },
            Err(e) => return Err(Error::io(to, &e)),
        }
    }
}

#[cfg(not(windows))]
fn rename_with_retry(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|e| Error::io(to, &e))
}

#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent()
        && let Ok(d) = fs::File::open(dir)
    {
        // Best effort: the rename is already visible; this only hardens it
        // against power loss.
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

/// Atomically replace `path` with `contents` (no hash re-check). Used for
/// Superba's own files and for backups, never for Codex files.
pub(crate) fn write_atomic(path: &Path, contents: &[u8], mode: Mode) -> Result<()> {
    let target = resolve_write_target(path);
    let temp = stage(&target, contents, mode)?;
    if let Err(e) = publish(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// Read and parse a JSON file; `None` if it does not exist.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path, what: &str) -> Result<Option<T>> {
    match read_optional(path)? {
        None => Ok(None),
        Some(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| Error::parse(what, format!("{}: {e}", path.display()))),
    }
}

/// Serialize `value` as pretty JSON and write it atomically with mode 0600.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::other(format!("serialize {}: {e}", path.display())))?;
    bytes.push(b'\n');
    write_atomic(path, &bytes, Mode::Private)
}

/// Turn an arbitrary string into a safe single path component.
pub(crate) fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "_".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_and_short_hash() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(short_hash(b"").len(), 16);
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_component("profile:a/b"), "profile_a_b");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("ok-1.2_x"), "ok-1.2_x");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_modes_and_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.txt");
        write_atomic(&p, b"one", Mode::Private).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&p, b"two", Mode::PreserveOrPrivate).unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o640
        );
        // Symlinked target: the link survives and the real file is updated.
        let real = tmp.path().join("real.toml");
        fs::write(&real, b"x").unwrap();
        let link = tmp.path().join("link.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"y", Mode::PreserveOrPrivate).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"y");
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
