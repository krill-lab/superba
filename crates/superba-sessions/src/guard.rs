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

/// Codex's thread writer locks and how Superba keeps writers out while it
/// rewrites sessions.
///
/// Codex (since July 2026, `codex-rs/rollout/src/writer_lock.rs`) holds an
/// exclusive lock on `thread-writer-locks/<thread>.lock` for as long as a process
/// has that thread open for writing. Every Codex writer first takes the exclusive
/// `thread-writer-locks/.coordination.lock` (blocking) and only then creates,
/// locks, unlocks or deletes a thread lock; Codex's own rollout compression relies
/// on exactly that rule to replace the files of idle threads ("holds coordination
/// through publication after probing that the thread is idle").
///
/// Superba does the same for a whole write operation (migrate, rollback,
/// recovery): [`WriterGate::acquire`] takes the coordination lock and keeps it
/// until the operation ends, then [`WriterGate::check_no_writers`] and
/// [`WriterGate::check_idle`] probe the thread locks one at a time (open, try to
/// lock, unlock, close). A thread lock found free stays free while the gate is
/// held, because nobody can lock it without the coordination lock: a Codex that
/// starts meanwhile waits in its coordination lock until Superba is done, and then
/// reads the migrated files. A thread lock found held is a genuine conflict.
///
/// This replaces holding every migrated thread's lock for the whole operation,
/// which kept one open file per session (EMFILE at ~235 sessions under macOS's
/// soft limit of 256) and made the automatic rollback after a failed migration
/// collide with Superba's own locks. The handles open at once are now bounded:
/// the migrate lock, the coordination lock and one probe.
#[derive(Debug)]
pub(crate) struct WriterGate {
    root: PathBuf,
    /// `thread-writer-locks/`; `None` when this Codex does not use writer locks.
    dir: Option<PathBuf>,
    /// The held coordination lock (released on drop).
    _coordination: Option<File>,
}

/// Attempts (25 ms apart) to take Codex's coordination lock: Codex holds it only
/// for a moment (opening, closing or publishing a thread).
const COORDINATION_ATTEMPTS: u32 = 120;
/// Codex's coordination lock inside [`WRITER_LOCK_DIR`].
const COORDINATION_LOCK: &str = ".coordination.lock";

/// Result of probing one lock file.
enum Probe {
    /// Missing or not locked.
    Free,
    /// Locked by someone else (a genuine conflict).
    Held,
    /// The probe itself failed.
    Failed(std::io::Error),
}

impl WriterGate {
    /// Take Codex's coordination lock of `home` (creating it like Codex does) when
    /// this Codex version uses writer locks at all. Fails with `Error::Locked` only
    /// when a Codex process keeps holding it; other failures keep their OS error.
    pub(crate) fn acquire(home: &CodexHome) -> Result<Self> {
        let dir = home.root().join(WRITER_LOCK_DIR);
        if !dir.is_dir() {
            return Ok(Self {
                root: home.root().to_path_buf(),
                dir: None,
                _coordination: None,
            });
        }
        let path = dir.join(COORDINATION_LOCK);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| Error::io(&path, &e))?;
        let mut attempts = 0;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) if attempts + 1 < COORDINATION_ATTEMPTS => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(Error::Locked {
                        what: format!(
                            "a Codex process{} keeps holding its session lock {}; try again in a moment",
                            holder_suffix(&path),
                            path.display()
                        ),
                    });
                }
                Err(fs::TryLockError::Error(e)) => return Err(Error::io(&path, &e)),
            }
        }
        Ok(Self {
            root: home.root().to_path_buf(),
            dir: Some(dir),
            _coordination: Some(file),
        })
    }

    /// Refuse while any thread of the home has a Codex writer. Lock files Superba
    /// may not open prove nothing and are skipped (stale files of another user);
    /// every other failure is returned with its OS error.
    pub(crate) fn check_no_writers(&self) -> Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        // Collect the names first: no directory handle stays open while probing.
        let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()) == Some("lock")
                        && p.file_name().and_then(|n| n.to_str()) != Some(COORDINATION_LOCK)
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::io(dir, &e)),
        };
        paths.sort();
        for path in paths {
            let thread = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            match probe(&path) {
                Probe::Free => {}
                Probe::Held => return Err(self.conflict(&thread, &path)),
                Probe::Failed(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                Probe::Failed(e) => return Err(Error::io(&path, &e)),
            }
        }
        Ok(())
    }

    /// Refuse while one of `threads` (the sessions about to be rewritten) has a
    /// Codex writer. Stricter than [`Self::check_no_writers`]: a lock file of these
    /// threads that cannot be probed fails the operation with its OS error.
    pub(crate) fn check_idle<'a>(&self, threads: impl IntoIterator<Item = &'a str>) -> Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        for id in threads {
            // Thread ids name files: only plain ids (Codex uses UUIDs).
            if id.is_empty()
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                continue;
            }
            let path = dir.join(format!("{id}.lock"));
            match probe(&path) {
                Probe::Free => {}
                Probe::Held => return Err(self.conflict(id, &path)),
                Probe::Failed(e) => return Err(Error::io(&path, &e)),
            }
        }
        Ok(())
    }

    /// `Error::Locked` for a thread whose writer lock is held, naming the
    /// session file and (where the platform tells) the process holding it.
    fn conflict(&self, thread: &str, lock: &Path) -> Error {
        let session = session_file_of(&self.root, thread)
            .map(|p| format!(", {}", p.display()))
            .unwrap_or_default();
        Error::Locked {
            what: format!(
                "a Codex process{} is writing session {thread}{session} (it holds {})",
                holder_suffix(lock),
                lock.display()
            ),
        }
    }
}

/// Open `path` read-only (no create), try to lock it and release it at once.
/// Read access is enough for `flock`/`LockFileEx`, so lock files of another
/// owner can still be probed.
fn probe(path: &Path) -> Probe {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Probe::Free,
        Err(e) if is_sharing_violation(&e) => return Probe::Held,
        Err(e) => return Probe::Failed(e),
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            Probe::Free
        }
        Err(fs::TryLockError::WouldBlock) => Probe::Held,
        Err(fs::TryLockError::Error(e)) => Probe::Failed(e),
    }
}

/// Windows: a holder that opened the file without sharing makes the open itself
/// fail (`ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION`): the file is held.
fn is_sharing_violation(err: &std::io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33))
}

/// The rollout file of `thread` (live or archived), for messages. Walks the
/// session folders; used only when reporting a conflict.
fn session_file_of(root: &Path, thread: &str) -> Option<PathBuf> {
    if thread.is_empty() {
        return None;
    }
    ["sessions", "archived_sessions"].iter().find_map(|sub| {
        walkdir::WalkDir::new(root.join(sub))
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| crate::rollout::is_rollout_name(n) && n.contains(thread))
            })
            .map(|e| e.into_path())
    })
}

/// ` (pid 4242 codex)` for the processes holding a lock on `path` when the
/// platform tells (Linux `/proc/locks`); empty otherwise.
fn holder_suffix(path: &Path) -> String {
    let Some(pids) = lock_holders(&[path.to_path_buf()]) else {
        return String::new();
    };
    let me = std::process::id();
    let list: Vec<String> = pids
        .into_iter()
        .filter(|pid| *pid != me)
        .map(|pid| match process_name(pid) {
            Some(name) => format!("pid {pid} {name}"),
            None => format!("pid {pid}"),
        })
        .collect();
    if list.is_empty() {
        String::new()
    } else {
        format!(" ({})", list.join(", "))
    }
}

#[cfg(target_os = "linux")]
fn process_name(pid: u32) -> Option<String> {
    let name = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(not(target_os = "linux"))]
fn process_name(pid: u32) -> Option<String> {
    let _ = pid;
    None
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
