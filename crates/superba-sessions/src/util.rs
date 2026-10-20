//! Small helpers: hashing, atomic writes with preserved metadata, time parsing and
//! path classification.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use sha2::{Digest, Sha256};
use superba_core::{Error, Result};

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `Error::Io` for `path`.
pub(crate) fn io_err(path: &Path, err: &std::io::Error) -> Error {
    Error::io(path, err)
}

/// Size and modification time of a file, used to detect concurrent writers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    /// Length in bytes.
    pub len: u64,
    /// Modification time, when the platform reports one.
    pub mtime: Option<SystemTime>,
}

impl Fingerprint {
    /// Read the fingerprint of `path` (following no symlinks).
    pub(crate) fn of(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path).map_err(|e| io_err(path, &e))?;
        Ok(Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }
}

/// Temporary sibling used by [`atomic_replace`].
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tag = uuid::Uuid::new_v4().simple().to_string();
    path.with_file_name(format!(".{name}.superba-{}.tmp", &tag[..8]))
}

/// Replace `path` with `bytes` atomically (temp file in the same directory, fsync,
/// rename). The permissions of `like` (the file being replaced) are copied and the
/// modification time is set to `mtime` when given.
pub(crate) fn atomic_replace(
    path: &Path,
    bytes: &[u8],
    like: Option<&fs::Metadata>,
    mtime: Option<SystemTime>,
) -> Result<()> {
    let tmp = temp_sibling(path);
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(meta) = like {
            file.set_permissions(meta.permissions())?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(mtime) = mtime {
            file.set_times(fs::FileTimes::new().set_modified(mtime))?;
        }
        drop(file);
        fs::rename(&tmp, path)?;
        sync_parent(path);
        Ok(())
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(path, &err));
    }
    Ok(())
}

/// Best-effort fsync of the directory holding `path` (Unix only; a no-op elsewhere).
pub(crate) fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Ok(handle) = fs::File::open(dir)
    {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Write a new file (must not exist) and fsync it.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    result.map_err(|e| io_err(path, &e))
}

/// Serialize `value` as pretty JSON and replace `path` atomically.
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::other(format!("cannot serialize {}: {e}", path.display())))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, &e))?;
    }
    atomic_replace(path, &bytes, None, None)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an RFC 3339 timestamp (`2026-10-16T07:35:26.266Z`, `...+08:00`) into
/// milliseconds since the Unix epoch.
pub(crate) fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let b = text.trim().as_bytes();
    let num = |s: &[u8]| -> Option<i64> {
        if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(s).ok()?.parse().ok()
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (year, month, day) = (num(&b[0..4])?, num(&b[5..7])?, num(&b[8..10])?);
    let (hour, minute, second) = (num(&b[11..13])?, num(&b[14..16])?, num(&b[17..19])?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let mut i = 19;
    let mut millis = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let frac = &b[start..i];
        if frac.is_empty() {
            return None;
        }
        let mut digits = [b'0'; 3];
        for (slot, d) in digits.iter_mut().zip(frac.iter()) {
            *slot = *d;
        }
        millis = num(&digits)?;
    }
    let offset_secs = match b.get(i..)? {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let h = num(&[*h1, *h2])?;
            let m = num(&[*m1, *m2])?;
            let s = h * 3600 + m * 60;
            if *sign == b'+' { s } else { -s }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    Some(secs * 1000 + millis)
}

/// Milliseconds embedded in a UUIDv7 (Codex thread ids are v7), if `id` is one.
pub(crate) fn uuid_v7_ms(id: &str) -> Option<i64> {
    let uuid = uuid::Uuid::parse_str(id).ok()?;
    if uuid.get_version_num() != 7 {
        return None;
    }
    let bytes = uuid.as_bytes();
    let mut ms: i64 = 0;
    for b in &bytes[..6] {
        ms = (ms << 8) | i64::from(*b);
    }
    Some(ms)
}

/// True for paths on a share where SQLite cannot take locks: Windows UNC paths to
/// WSL (`\\wsl$\...`, `\\wsl.localhost\...`) and, on Windows, any UNC path. Such
/// databases are skipped (with a warning) instead of risking corruption.
pub(crate) fn is_unlockable_share(path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let unc = text
        .strip_prefix("\\\\?\\unc\\")
        .map(|rest| format!("\\\\{rest}"))
        .unwrap_or(text);
    if unc.starts_with("\\\\wsl$\\") || unc.starts_with("\\\\wsl.localhost\\") {
        return true;
    }
    cfg!(windows)
        && unc.starts_with("\\\\")
        && !unc.starts_with("\\\\?\\")
        && !unc.starts_with("\\\\.\\")
}

/// Best-effort normalization for comparing directories: canonical path when it
/// exists, otherwise the path with `.`/`..` components folded.
pub(crate) fn normalize_dir(path: &Path) -> PathBuf {
    if let Ok(canon) = superba_core::paths::canonical(path) {
        return canon;
    }
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Total size of a SQLite database including its `-wal` file.
pub(crate) fn db_size(path: &Path) -> u64 {
    let main = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wal = fs::metadata(sidecar(path, "-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    main + wal
}

/// `<db>-wal` / `<db>-shm` / `<db>-journal`.
pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_parses_codex_timestamps() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-10-16T07:35:26.266Z"),
            Some(1_792_136_126_266)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-16T15:35:26.266+08:00"),
            Some(1_792_136_126_266)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-16T07:35:26.2Z"),
            Some(1_792_136_126_200)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-16T07:35:26.123456789Z"),
            Some(1_792_136_126_123)
        );
        assert_eq!(
            parse_rfc3339_ms("2000-02-29T12:00:00Z"),
            Some(951_825_600_000)
        );
        for bad in [
            "",
            "2026-10-16",
            "2026-10-16T07:35:26",
            "2026-13-16T07:35:26Z",
            "2026-10-16T07:35:26.Z",
            "2026-10-16T07:35:26Zjunk",
            "２０２６-10-16T07:35:26Z",
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn uuid_v7_timestamp_is_extracted() {
        assert_eq!(
            uuid_v7_ms("01a143a3-1f0f-7031-a8df-e99d93328ef5"),
            Some(0x01a1_43a3_1f0f)
        );
        assert_eq!(uuid_v7_ms("6f1c2c4e-0b1a-4c3d-8e9f-0123456789ab"), None);
        assert_eq!(uuid_v7_ms("not-a-uuid"), None);
    }

    #[test]
    fn wsl_shares_are_unlockable() {
        assert!(is_unlockable_share(Path::new(
            r"\\wsl$\Ubuntu\home\u\.codex\state_5.sqlite"
        )));
        assert!(is_unlockable_share(Path::new(
            r"\\wsl.localhost\Ubuntu\home\u\.codex\state_5.sqlite"
        )));
        assert!(is_unlockable_share(Path::new(
            "//wsl.localhost/Ubuntu/home/u/.codex/state_5.sqlite"
        )));
        assert!(is_unlockable_share(Path::new(
            r"\\?\UNC\wsl$\Ubuntu\home\u\.codex\state_5.sqlite"
        )));
        assert!(!is_unlockable_share(Path::new(
            "/home/u/.codex/state_5.sqlite"
        )));
        assert!(!is_unlockable_share(Path::new(
            r"C:\Users\u\.codex\state_5.sqlite"
        )));
        assert_eq!(
            is_unlockable_share(Path::new(r"\\fileserver\share\state_5.sqlite")),
            cfg!(windows)
        );
    }

    #[test]
    fn atomic_replace_keeps_permissions_and_sets_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.jsonl");
        fs::write(&path, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let meta = fs::metadata(&path).unwrap();
        let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        atomic_replace(&path, b"new", Some(&meta), Some(mtime)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.modified().unwrap(), mtime);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(after.permissions().mode() & 0o777, 0o640);
        }
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temp file left behind");
    }
}
