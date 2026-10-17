//! Safety checks before any write: the migrate lock, locks of similar tools,
//! Codex's own thread writer locks, and live Codex processes using the home.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use superba_core::{CodexHome, ConsumersStopped, Error, Notice, Result, Timestamp};

use crate::text;
use crate::util::normalize_dir;

/// How long a [`ConsumersStopped`] token stays valid.
pub(crate) const TOKEN_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// A lock of another tool older than this is treated as stale.
const FOREIGN_LOCK_STALE_AFTER: Duration = Duration::from_secs(30 * 60);
/// Codex holds an exclusive `flock` on `thread-writer-locks/<thread>.lock` while a
/// process has the thread open for writing.
const WRITER_LOCK_DIR: &str = "thread-writer-locks";

/// Extra attempts (25 ms apart) before a held migration lock is reported.
const LOCK_RETRIES: u32 = 20;

/// Exclusive lock on `CodexHome::migrate_lock` for the duration of a write.
#[derive(Debug)]
pub(crate) struct MigrateLock {
    _file: File,
}

impl MigrateLock {
    /// Take the lock or fail with `Error::Locked` (after a short grace period:
    /// a process forked at the moment the previous holder released it keeps the
    /// lock until it execs, so a lock released just now may look held for a few
    /// milliseconds).
    pub(crate) fn acquire(home: &CodexHome) -> Result<Self> {
        let path = home.migrate_lock();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| Error::io(&path, &e))?;
        let mut tries = 0;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs::TryLockError::WouldBlock) if tries < LOCK_RETRIES => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(Error::Locked {
                        what: format!("another session migration is running ({})", path.display()),
                    });
                }
                Err(fs::TryLockError::Error(e)) => return Err(Error::io(&path, &e)),
            }
        }
    }
}

/// Refuse while a similar tool migrates the same home (Codex++ provider sync).
/// Locks older than 30 minutes are reported as stale and ignored.
pub(crate) fn check_foreign_locks(home: &CodexHome, notices: &mut Vec<Notice>) -> Result<()> {
    let candidates = [home.tmp_dir().join("provider-sync.lock")];
    for path in candidates {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or_default();
        if age > FOREIGN_LOCK_STALE_AFTER {
            notices.push(text::stale_foreign_lock(&path));
            continue;
        }
        return Err(Error::Locked {
            what: format!("another tool is migrating sessions ({})", path.display()),
        });
    }
    Ok(())
}

/// Refuse while a Codex process holds a thread writer lock of this home.
pub(crate) fn check_writer_locks(home: &CodexHome) -> Result<()> {
    let dir = home.root().join(WRITER_LOCK_DIR);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(err) => {
                // Windows: a holder that opened the file without sharing makes the
                // open itself fail (sharing violation); treat that as held.
                // Elsewhere an unreadable lock file proves nothing.
                if cfg!(windows) && err.kind() != std::io::ErrorKind::NotFound {
                    return Err(Error::Locked {
                        what: format!("a Codex process holds {}", path.display()),
                    });
                }
                continue;
            }
        };
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
            }
            Err(fs::TryLockError::WouldBlock) => {
                let thread = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                return Err(Error::Locked {
                    what: format!("a Codex process is writing thread {thread}"),
                });
            }
            Err(fs::TryLockError::Error(_)) => {}
        }
    }
    Ok(())
}

/// Codex writer locks held by Superba while it rewrites threads, so a Codex
/// process that starts meanwhile cannot write them. Released on drop.
#[derive(Debug)]
pub(crate) struct WriterLocks {
    _files: Vec<File>,
}

/// Take Codex's writer lock of every thread in `thread_ids` (creating missing
/// lock files) when this Codex version uses writer locks at all. Fails with
/// `Error::Locked` when one is held.
pub(crate) fn hold_writer_locks<'a>(
    home: &CodexHome,
    thread_ids: impl IntoIterator<Item = &'a str>,
) -> Result<WriterLocks> {
    let dir = home.root().join(WRITER_LOCK_DIR);
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(WriterLocks { _files: files });
    }
    for id in thread_ids {
        // Thread ids name files: only plain ids (Codex uses UUIDs).
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        let path = dir.join(format!("{id}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|_| Error::Locked {
                what: format!("a Codex process holds {}", path.display()),
            })?;
        match file.try_lock() {
            Ok(()) => files.push(file),
            Err(fs::TryLockError::WouldBlock) => {
                return Err(Error::Locked {
                    what: format!("a Codex process is writing thread {id}"),
                });
            }
            Err(fs::TryLockError::Error(e)) => return Err(Error::io(&path, &e)),
        }
    }
    Ok(WriterLocks { _files: files })
}

/// Validate a caller's [`ConsumersStopped`] token for `home` at `now`.
pub(crate) fn validate_token(
    token: &ConsumersStopped,
    home: &CodexHome,
    now: Timestamp,
) -> Result<()> {
    if normalize_dir(&token.codex_home) != normalize_dir(home.root()) {
        return Err(Error::invalid(format!(
            "the processes-stopped token covers {}, not {}",
            token.codex_home.display(),
            home.root().display()
        )));
    }
    let age = i128::from(now.0) - i128::from(token.checked_at.0);
    let fresh = (-60_000..=TOKEN_MAX_AGE.as_millis() as i128).contains(&age);
    if !fresh {
        return Err(Error::invalid(
            "the processes-stopped token is too old; check the processes again",
        ));
    }
    Ok(())
}

/// A live process that may use the Codex home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveProcess {
    pub pid: u32,
    pub name: String,
}

/// True for executable names of Codex processes (`codex`, `codex.exe`,
/// `codex-x86_64-unknown-linux-musl`, the app's `Codex` on macOS, ...).
pub(crate) fn is_codex_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    base.starts_with("codex")
}

/// Codex home a process uses, from its environment (`CODEX_HOME`, else
/// `HOME`/`USERPROFILE` + `.codex`). `None` when the environment is unknown.
pub(crate) fn process_codex_home(environ: &[OsString], cwd: Option<&Path>) -> Option<PathBuf> {
    let path = process_codex_home_raw(environ)?;
    if path.is_absolute() {
        return Some(path);
    }
    // Relative values are relative to that process's working directory.
    cwd.map(|dir| dir.join(path))
}

fn process_codex_home_raw(environ: &[OsString]) -> Option<PathBuf> {
    let mut codex_home = None;
    let mut home = None;
    let mut profile = None;
    for entry in environ {
        let Some(text) = entry.to_str() else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        match key {
            "CODEX_HOME" if !value.is_empty() => codex_home = Some(PathBuf::from(value)),
            "HOME" if !value.is_empty() => home = Some(PathBuf::from(value)),
            "USERPROFILE" if !value.is_empty() => profile = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    codex_home.or_else(|| home.or(profile).map(|h| h.join(".codex")))
}

/// Codex processes of the current user that use `home`, plus (Linux) any process
/// holding a lock on the home's SQLite databases or thread writer locks.
///
/// A Codex process is recognized by its executable name (`codex*`; the process
/// name only when the executable is unknown, so scripts such as `codex_loop.sh`
/// run by `bash` do not count). Its home comes from its environment. When the
/// environment is unreadable or names no home, Linux decides by lock evidence from
/// `/proc/locks`; other platforms count the process (conservative; callers that
/// checked processes themselves pass a [`ConsumersStopped`] token). Processes of
/// other users are ignored.
pub(crate) fn live_codex_processes(home: &CodexHome, lock_files: &[PathBuf]) -> Vec<LiveProcess> {
    use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_environ(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet),
    );
    let me = sysinfo::get_current_pid().ok();
    let my_user = me
        .and_then(|pid| sys.process(pid))
        .and_then(|p| p.user_id().cloned());
    let target = normalize_dir(home.root());
    let lockers = lock_holders(lock_files);
    let mut out: Vec<LiveProcess> = Vec::new();
    for (pid, process) in sys.processes() {
        if Some(*pid) == me || process.thread_kind().is_some() {
            continue;
        }
        if matches!(
            process.status(),
            ProcessStatus::Zombie | ProcessStatus::Dead
        ) {
            continue;
        }
        let name = process.name().to_string_lossy().into_owned();
        let exe_name = process
            .exe()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned());
        let shown = exe_name.clone().unwrap_or_else(|| name.clone());
        let holds_lock = lockers.as_ref().is_some_and(|l| l.contains(&pid.as_u32()));
        if holds_lock {
            out.push(LiveProcess {
                pid: pid.as_u32(),
                name: shown,
            });
            continue;
        }
        let is_codex = match &exe_name {
            Some(exe) => is_codex_name(exe),
            None => is_codex_name(&name),
        };
        if !is_codex {
            continue;
        }
        if let (Some(mine), Some(theirs)) = (&my_user, process.user_id())
            && mine != theirs
        {
            continue;
        }
        let uses_home = match process_codex_home(process.environ(), process.cwd()) {
            Some(path) => normalize_dir(&path) == target,
            // Unknown home: lock evidence when available, else assume it does.
            None => lockers.is_none(),
        };
        if uses_home {
            out.push(LiveProcess {
                pid: pid.as_u32(),
                name: shown,
            });
        }
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// PIDs holding a lock on any of `paths` (Linux `/proc/locks`); `None` when the
/// platform offers no such table.
#[cfg(target_os = "linux")]
fn lock_holders(paths: &[PathBuf]) -> Option<std::collections::BTreeSet<u32>> {
    use std::os::unix::fs::MetadataExt;
    let mut inodes = std::collections::BTreeSet::new();
    for path in paths {
        if let Ok(meta) = fs::metadata(path) {
            let dev = meta.dev();
            let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
            let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
            inodes.insert((major, minor, meta.ino()));
        }
    }
    let table = fs::read_to_string("/proc/locks").ok()?;
    Some(parse_proc_locks(&table, &inodes))
}

/// Non-Linux platforms have no lock table to consult.
#[cfg(not(target_os = "linux"))]
fn lock_holders(paths: &[PathBuf]) -> Option<std::collections::BTreeSet<u32>> {
    let _ = paths;
    None
}

/// PIDs of `/proc/locks` entries on `inodes` (`(major, minor, inode)`). Lines look
/// like `12: POSIX  ADVISORY  READ 4242 08:02:7921437 128 128` (waiters carry
/// `->`; OFD locks have pid -1 and are skipped).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_locks(
    table: &str,
    inodes: &std::collections::BTreeSet<(u64, u64, u64)>,
) -> std::collections::BTreeSet<u32> {
    let mut out = std::collections::BTreeSet::new();
    for line in table.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(idx) = tokens.iter().position(|t| t.matches(':').count() == 2) else {
            continue;
        };
        let Some(pid) = idx
            .checked_sub(1)
            .and_then(|i| tokens[i].parse::<i64>().ok())
            .filter(|p| *p > 0)
        else {
            continue;
        };
        let mut parts = tokens[idx].split(':');
        let (Some(maj), Some(min), Some(ino)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(maj), Ok(min), Ok(ino)) = (
            u64::from_str_radix(maj, 16),
            u64::from_str_radix(min, 16),
            ino.parse::<u64>(),
        ) else {
            continue;
        };
        if inodes.contains(&(maj, min, ino)) {
            out.insert(pid as u32);
        }
    }
    out
}

/// Files whose locks reveal a live Codex: every SQLite file (with `-wal`/`-shm`)
/// in `dbs` and the thread writer locks of `home`.
pub(crate) fn lock_evidence_files(home: &CodexHome, dbs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for db in dbs {
        out.push(db.clone());
        out.push(crate::util::sidecar(db, "-wal"));
        out.push(crate::util::sidecar(db, "-shm"));
    }
    if let Ok(entries) = fs::read_dir(home.root().join(WRITER_LOCK_DIR)) {
        out.extend(entries.flatten().map(|e| e.path()));
    }
    out
}

/// Refuse while Codex processes use `home`, unless the caller vouched.
pub(crate) fn check_processes(
    home: &CodexHome,
    dbs: &[PathBuf],
    token: Option<&ConsumersStopped>,
    now: Timestamp,
) -> Result<()> {
    if let Some(token) = token {
        return validate_token(token, home, now);
    }
    let live = live_codex_processes(home, &lock_evidence_files(home, dbs));
    if live.is_empty() {
        return Ok(());
    }
    let list = live
        .iter()
        .map(|p| format!("{} (pid {})", p.name, p.pid))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::Locked {
        what: format!(
            "Codex is still running for {}: {list}; close it first",
            home.root().display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_names() {
        for yes in [
            "codex",
            "codex.exe",
            "Codex",
            "codex-x86_64-unknown-linux-musl",
            "/usr/bin/codex",
            r"C:\x\codex.exe",
        ] {
            assert!(is_codex_name(yes), "{yes}");
        }
        for no in ["node", "ChatGPT", "superba", "spb", "vscodex", ""] {
            assert!(!is_codex_name(no), "{no}");
        }
    }

    #[test]
    fn proc_locks_lines_are_parsed() {
        let table = "\
1: POSIX  ADVISORY  WRITE 2998464 00:2e:6042762 0 EOF
425: POSIX  ADVISORY  READ 4012727 08:02:7922324 128 128
426: -> POSIX  ADVISORY  READ 4012728 08:02:7922324 128 128
427: OFDLCK ADVISORY  READ -1 08:02:7922324 0 EOF
746: FLOCK  ADVISORY  WRITE 3445291 103:05:8017371 0 EOF
garbage
";
        let inodes = [(8, 2, 7_922_324), (0x103, 5, 8_017_371)]
            .into_iter()
            .collect();
        let pids: Vec<u32> = parse_proc_locks(table, &inodes).into_iter().collect();
        assert_eq!(pids, vec![3_445_291, 4_012_727, 4_012_728]);
    }

    #[test]
    fn codex_home_from_environment() {
        let env = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let cwd = Some(Path::new("/work"));
        assert_eq!(
            process_codex_home(&env(&["HOME=/h", "CODEX_HOME=/c"]), cwd),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(
            process_codex_home(&env(&["HOME=/h", "CODEX_HOME="]), cwd),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(
            process_codex_home(&env(&["CODEX_HOME=rel/.codex"]), cwd),
            Some(PathBuf::from("/work/rel/.codex"))
        );
        assert_eq!(process_codex_home(&env(&["CODEX_HOME=rel"]), None), None);
        if cfg!(windows) {
            assert_eq!(
                process_codex_home(&env(&["USERPROFILE=C:\\Users\\u"]), cwd),
                Some(PathBuf::from("C:\\Users\\u").join(".codex"))
            );
        }
        assert_eq!(process_codex_home(&env(&["PATH=/bin"]), cwd), None);
        assert_eq!(process_codex_home(&[], cwd), None);
    }

    #[test]
    fn token_must_match_home_and_be_fresh() {
        let home = CodexHome::new("/tmp/superba-test-home/.codex");
        let now = Timestamp(10_000_000);
        let ok = ConsumersStopped::new(&home, Timestamp(now.0 - 1000));
        assert!(validate_token(&ok, &home, now).is_ok());
        let other = ConsumersStopped::new(&CodexHome::new("/tmp/other/.codex"), now);
        assert!(validate_token(&other, &home, now).is_err());
        let old = ConsumersStopped::new(&home, Timestamp(now.0 - 11 * 60 * 1000));
        assert!(validate_token(&old, &home, now).is_err());
        let future = ConsumersStopped::new(&home, Timestamp(now.0 + 5 * 60 * 1000));
        assert!(validate_token(&future, &home, now).is_err());
    }
}
