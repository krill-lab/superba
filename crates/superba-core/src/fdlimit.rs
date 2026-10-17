//! The process's open-file limit (`RLIMIT_NOFILE`).
//!
//! macOS starts GUI apps and launchd agents with a soft limit of 256 open files
//! while the hard limit is far higher. Superba's own code keeps the number of
//! files it holds open at once small and bounded; on top of that the binaries
//! call [`raise_open_files_limit`] at startup (defence in depth), and
//! [`crate::Error::io`] records the limit that applied when the OS answers
//! "too many open files", so the message can name it.
//!
//! Windows has no per-process soft limit for kernel handles: everything here is a
//! no-op there.

use std::io;

/// Soft limit the binaries ask for at startup. Enough for any session count
/// Superba handles (it never holds one handle per session) and far below what
/// every supported system allows.
pub const OPEN_FILES_TARGET: u64 = 8192;

/// Kind string of [`crate::Error::Io`] for "too many open files" (`EMFILE`,
/// `ENFILE`, Windows `ERROR_TOO_MANY_OPEN_FILES`). Set explicitly because the
/// matching `std::io::ErrorKind` variant is not stable.
pub const IO_KIND_TOO_MANY_OPEN_FILES: &str = "TooManyOpenFiles";

/// `OPEN_MAX` of macOS `<sys/syslimits.h>`: macOS `setrlimit` refuses a soft
/// `RLIMIT_NOFILE` above it (`EINVAL`).
#[cfg(target_os = "macos")]
const MACOS_OPEN_MAX: u64 = 10_240;

/// Which open-file limit an I/O error ran into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileLimitHit {
    /// This process has as many files open as its limit allows (`EMFILE`; on
    /// Windows `ERROR_TOO_MANY_OPEN_FILES`).
    Process,
    /// The whole system's file table is full (`ENFILE`).
    System,
}

/// Classify `err` as a "too many open files" error by its OS error code.
pub fn file_limit_hit(err: &io::Error) -> Option<FileLimitHit> {
    let code = err.raw_os_error()?;
    #[cfg(unix)]
    {
        use rustix::io::Errno;
        if code == Errno::MFILE.raw_os_error() {
            return Some(FileLimitHit::Process);
        }
        if code == Errno::NFILE.raw_os_error() {
            return Some(FileLimitHit::System);
        }
    }
    #[cfg(windows)]
    {
        // `ERROR_TOO_MANY_OPEN_FILES` (winerror.h).
        const ERROR_TOO_MANY_OPEN_FILES: i32 = 4;
        if code == ERROR_TOO_MANY_OPEN_FILES {
            return Some(FileLimitHit::Process);
        }
    }
    let _ = code;
    None
}

/// The current soft open-file limit of this process; `None` when unlimited,
/// unknown or not applicable (Windows).
pub fn open_files_limit() -> Option<u64> {
    #[cfg(unix)]
    {
        rustix::process::getrlimit(rustix::process::Resource::Nofile).current
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Soft open-file limit before and after [`raise_open_files_limit`] (`None`:
/// unlimited).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFilesLimit {
    /// Soft limit found at startup.
    pub before: Option<u64>,
    /// Soft limit now (equal to `before` when it was high enough or could not be
    /// raised).
    pub after: Option<u64>,
    /// Hard limit (the ceiling for the soft limit).
    pub hard: Option<u64>,
}

/// Raise the soft open-file limit towards `cap`, never above the hard limit and
/// never lowering it. On macOS the soft limit also stays at or below `OPEN_MAX`
/// and `kern.maxfilesperproc`: macOS refuses a larger value with `EINVAL`
/// instead of clamping it, so when `cap` is refused the largest value the
/// kernel accepts is searched for (a few `setrlimit` calls; a refused call
/// changes nothing). Linux only refuses values above the hard limit, which are
/// never asked for.
///
/// Returns `None` where the limit does not exist (Windows). Never fails: a
/// limit that cannot be raised is reported unchanged.
pub fn raise_open_files_limit(cap: u64) -> Option<OpenFilesLimit> {
    #[cfg(unix)]
    {
        Some(raise_unix(cap))
    }
    #[cfg(not(unix))]
    {
        let _ = cap;
        None
    }
}

#[cfg(unix)]
fn raise_unix(cap: u64) -> OpenFilesLimit {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    let start = getrlimit(Resource::Nofile);
    let unchanged = OpenFilesLimit {
        before: start.current,
        after: start.current,
        hard: start.maximum,
    };
    // `None` is RLIM_INFINITY: nothing to raise.
    let Some(before) = start.current else {
        return unchanged;
    };
    let mut target = cap;
    if let Some(hard) = start.maximum {
        target = target.min(hard);
    }
    #[cfg(target_os = "macos")]
    {
        target = target.min(MACOS_OPEN_MAX);
    }
    if target <= before {
        return unchanged;
    }
    // Keep the hard limit as it is: changing it is privileged and macOS checks
    // it against kern.maxfilesperproc as well.
    let set = |soft: u64| {
        setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(soft),
                maximum: start.maximum,
            },
        )
        .is_ok()
    };
    if !set(target) {
        // Largest accepted value in (before, target): `low` is known to be
        // accepted (it is the current limit or was just set), `high` refused.
        // Every successful call raises `low`, so the last one set is `low`.
        let (mut low, mut high) = (before, target);
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if set(mid) {
                low = mid;
            } else {
                high = mid;
            }
        }
    }
    OpenFilesLimit {
        before: Some(before),
        after: getrlimit(Resource::Nofile).current,
        hard: start.maximum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_open_files_is_classified_by_os_code() {
        #[cfg(unix)]
        {
            let emfile = io::Error::from_raw_os_error(rustix::io::Errno::MFILE.raw_os_error());
            let enfile = io::Error::from_raw_os_error(rustix::io::Errno::NFILE.raw_os_error());
            assert_eq!(file_limit_hit(&emfile), Some(FileLimitHit::Process));
            assert_eq!(file_limit_hit(&enfile), Some(FileLimitHit::System));
            let again = io::Error::from_raw_os_error(rustix::io::Errno::AGAIN.raw_os_error());
            assert_eq!(file_limit_hit(&again), None);
        }
        #[cfg(windows)]
        assert_eq!(
            file_limit_hit(&io::Error::from_raw_os_error(4)),
            Some(FileLimitHit::Process)
        );
        assert_eq!(file_limit_hit(&io::Error::other("no os code")), None);
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(file_limit_hit(&denied), None);
    }
}
