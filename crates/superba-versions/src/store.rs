//! The Superba-managed Codex store `<data dir>/codex/`:
//!
//! ```text
//! <ver>-<target>/                 extracted, verified package (bin/codex, ...)
//! <ver>-<target>/.superba-verified  written last: the directory is complete
//! packages/<archive>              verified archives (remote uploads), + .sha256
//! packages/<archive>.part         a download in progress (resumed)
//! .tmp-*/                         extraction in progress (removed on failure)
//! .trash-*/                       an entry being removed (renamed away first)
//! .lock                           exclusive while downloading / extracting / pruning
//! releases.json                   release list cache
//! ```
//!
//! Windows (user bug 40, checked on a Windows runner): a running `codex.exe`
//! cannot be deleted ("access denied", os error 5) although its directory can
//! be renamed, and a file another program holds open (an antivirus scan of a
//! new binary, the indexer) blocks renaming its directory for a moment. The
//! old removal deleted the marker first and then failed on the running
//! program, leaving a half-deleted entry that every later install of that
//! version tried, and failed, to remove again. Now new versions are always
//! extracted beside the others; an entry is never removed or replaced while it
//! is in use ([`entry_in_use`]); renames are retried for a while when Windows
//! reports a file in use; and an entry is renamed away before it is deleted,
//! so a failed deletion never leaves a half-deleted entry in its place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use superba_core::{CodexVersion, Error, Result, StoredCodex, Timestamp};

use crate::sources::{Layout, TARGETS};

/// Name of the marker written last into a complete store entry.
pub const VERIFIED_MARKER: &str = ".superba-verified";

/// Versions kept besides the pinned / in-use ones.
pub const KEEP_NEWEST: usize = 3;

/// `codex` or `codex.exe` for a target.
pub fn exe_name(target: &str) -> &'static str {
    if target.contains("windows") {
        "codex.exe"
    } else {
        "codex"
    }
}

/// Paths of the store.
#[derive(Clone, Debug)]
pub struct StoreDirs {
    /// `<data dir>/codex`.
    pub root: PathBuf,
}

impl StoreDirs {
    /// `<root>/<ver>-<target>`.
    pub fn entry(&self, version: &CodexVersion, target: &str) -> PathBuf {
        self.root.join(format!("{version}-{target}"))
    }

    /// `<root>/packages`.
    pub fn packages(&self) -> PathBuf {
        self.root.join("packages")
    }

    /// A verified archive of `version` for `target` (`layout` decides the name).
    pub fn archive(&self, version: &CodexVersion, target: &str, layout: Layout) -> PathBuf {
        self.packages().join(archive_name(version, target, layout))
    }

    /// The lock file.
    pub fn lock_file(&self) -> PathBuf {
        self.root.join(".lock")
    }

    /// The release list cache.
    pub fn releases_cache(&self) -> PathBuf {
        self.root.join("releases.json")
    }
}

/// File name of a verified archive.
pub fn archive_name(version: &CodexVersion, target: &str, layout: Layout) -> String {
    match layout {
        Layout::Package => format!("codex-package-{version}-{target}.tar.gz"),
        Layout::Npm => format!("codex-npm-{version}-{target}.tgz"),
    }
}

/// `(version, target)` of a store entry or archive name.
pub fn parse_entry_name(name: &str) -> Option<(CodexVersion, &'static str)> {
    let base = name
        .strip_prefix("codex-package-")
        .or_else(|| name.strip_prefix("codex-npm-"))
        .map(|n| n.trim_end_matches(".tar.gz").trim_end_matches(".tgz"))
        .unwrap_or(name);
    TARGETS.iter().find_map(|t| {
        let v = base.strip_suffix(&format!("-{t}"))?;
        Some((CodexVersion::parse(v).ok()?, *t))
    })
}

/// Exclusive lock over the store (blocks until free; also between processes).
/// Released when dropped (the file is closed).
pub struct StoreLock {
    _file: fs::File,
}

/// Take the store lock (blocking; call from a blocking context).
pub fn lock(dirs: &StoreDirs) -> Result<StoreLock> {
    fs::create_dir_all(&dirs.root).map_err(|e| Error::io(&dirs.root, &e))?;
    let path = dirs.lock_file();
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| Error::io(&path, &e))?;
    file.lock().map_err(|e| Error::io(&path, &e))?;
    Ok(StoreLock { _file: file })
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.path().symlink_metadata() else {
                continue;
            };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                total += m.len();
            }
        }
    }
    total
}

/// Complete store entries for `target` (all targets when `None`), newest
/// version first.
pub fn list(dirs: &StoreDirs, target: Option<&str>) -> Vec<StoredCodex> {
    let Ok(rd) = fs::read_dir(&dirs.root) else {
        return Vec::new();
    };
    let mut out: Vec<StoredCodex> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (version, t) = parse_entry_name(&name)?;
            if target.is_some_and(|want| want != t) {
                return None;
            }
            let dir = e.path();
            let marker = fs::metadata(dir.join(VERIFIED_MARKER)).ok()?;
            let executable = dir.join("bin").join(exe_name(t));
            executable.is_file().then_some(())?;
            let installed_at = marker
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| Timestamp::from_millis(i64::try_from(d.as_millis()).unwrap_or(0)));
            Some(StoredCodex {
                version,
                target: t.to_owned(),
                executable,
                size_bytes: Some(dir_size(&dir)),
                installed_at,
                in_use: false,
                pinned: false,
            })
        })
        .collect();
    out.sort_by(|a, b| b.version.cmp(&a.version).then(a.target.cmp(&b.target)));
    out
}

/// Pauses between attempts of a rename Windows refuses because a file is in
/// use (an antivirus scan of a freshly written `codex.exe` holds it briefly).
const IN_USE_RETRIES: [Duration; 6] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(3),
];

/// A file-system error that a file in use explains: on Windows
/// `ERROR_ACCESS_DENIED` (5, also what renaming a directory with an open or
/// running file inside gives), `ERROR_SHARING_VIOLATION` (32) and
/// `ERROR_LOCK_VIOLATION` (33); a busy executable or resource elsewhere.
pub fn is_in_use(e: &io::Error) -> bool {
    (cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)))
        || matches!(
            e.kind(),
            io::ErrorKind::ExecutableFileBusy | io::ErrorKind::ResourceBusy
        )
}

/// Run `op` again after each of `pauses` while it fails with [`is_in_use`].
pub fn retry_in_use<T>(
    mut op: impl FnMut() -> io::Result<T>,
    pauses: &[Duration],
    sleep: &dyn Fn(Duration),
) -> io::Result<T> {
    let mut pauses = pauses.iter();
    loop {
        match op() {
            Err(e) if is_in_use(&e) => match pauses.next() {
                Some(pause) => sleep(*pause),
                None => return Err(e),
            },
            other => return other,
        }
    }
}

/// `fs::rename`, retried for a few seconds while a file is in use.
pub fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    retry_in_use(
        || fs::rename(from, to),
        &IN_USE_RETRIES,
        &std::thread::sleep,
    )
}

/// The error for an entry that cannot be removed or replaced because it is in
/// use, saying what to do.
pub fn in_use_error(dir: &Path, why: &str) -> Error {
    Error::Locked {
        what: format!(
            "the Codex in {} ({why}); Superba never removes or replaces a Codex that is in \
             use. Quit what runs it (ChatGPT, a terminal) and try again",
            dir.display()
        ),
    }
}

/// `path` without the verbatim `\\?\` prefix of a canonical Windows drive path.
fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|t| t.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Why the store entry `dir` must not be removed or replaced now: a running
/// process's program lives in it, or (Windows) one of its programs cannot be
/// opened for writing because it runs (an image section; Windows can still
/// rename its directory, but never delete it) or another program holds it
/// open. `None` when it is free.
pub fn entry_in_use(dir: &Path, target: &str) -> Option<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    // Long, plain and (Windows) lower-case: a process started through a short
    // `RUNNER~1` path reports that form.
    let key = |p: &Path| -> PathBuf {
        let p = plain(superba_core::paths::canonical(p).unwrap_or_else(|_| p.to_path_buf()));
        if cfg!(windows) {
            PathBuf::from(p.to_string_lossy().to_lowercase())
        } else {
            p
        }
    };
    let root = key(dir);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    if let Some(p) = sys
        .processes()
        .values()
        .find(|p| p.exe().is_some_and(|exe| key(exe).starts_with(&root)))
    {
        return Some(format!("process {} runs a program from it", p.pid()));
    }
    #[cfg(windows)]
    for program in fs::read_dir(dir.join("bin"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")))
    {
        // Opening for writing changes nothing; a running image or a file open
        // without write sharing gives ERROR_SHARING_VIOLATION.
        if let Err(e) = fs::OpenOptions::new().write(true).open(&program)
            && e.raw_os_error() == Some(32)
        {
            return Some(format!(
                "{} is running or open in another program",
                program.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    let _ = target;
    None
}

/// Remove entries of `target` beyond the newest [`KEEP_NEWEST`] versions,
/// never one in `keep` nor one in use ([`entry_in_use`]; it is kept and tried
/// again next time); their archives go too, as do archives of versions no
/// longer kept and leftovers of interrupted extractions and removals. Returns
/// removed versions. Call with the store lock held.
pub fn prune(dirs: &StoreDirs, target: &str, keep: &[CodexVersion]) -> Result<Vec<CodexVersion>> {
    prune_with(dirs, target, keep, &entry_in_use, &remove_dir)
}

/// [`prune`] with the in-use check and the removal supplied (tests).
pub fn prune_with(
    dirs: &StoreDirs,
    target: &str,
    keep: &[CodexVersion],
    in_use: &dyn Fn(&Path, &str) -> Option<String>,
    remove: &dyn Fn(&Path) -> Result<()>,
) -> Result<Vec<CodexVersion>> {
    let entries = list(dirs, Some(target));
    let mut kept: Vec<CodexVersion> = keep.to_vec();
    let mut newest = 0;
    let mut removed = Vec::new();
    for e in &entries {
        if keep.contains(&e.version) {
            continue;
        }
        if newest < KEEP_NEWEST {
            newest += 1;
            kept.push(e.version.clone());
            continue;
        }
        let dir = dirs.entry(&e.version, target);
        if let Some(why) = in_use(&dir, target) {
            tracing::info!(dir = %dir.display(), why, "keeping a Codex in use");
            kept.push(e.version.clone());
            continue;
        }
        match remove(&dir) {
            Ok(()) => removed.push(e.version.clone()),
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "cannot remove an old Codex; kept");
                kept.push(e.version.clone());
            }
        }
    }
    // Archives: those of kept versions and the newest few others (remote
    // uploads for other targets) stay.
    let mut archive_versions: Vec<CodexVersion> = fs::read_dir(dirs.packages())
        .map(|rd| {
            rd.flatten()
                .filter_map(|f| {
                    let name = f.file_name().to_string_lossy().into_owned();
                    parse_entry_name(archive_base(&name)).map(|(v, _)| v)
                })
                .collect()
        })
        .unwrap_or_default();
    archive_versions.sort();
    archive_versions.dedup();
    let newest_archives: Vec<CodexVersion> = archive_versions
        .iter()
        .rev()
        .filter(|v| !removed.contains(v))
        .take(KEEP_NEWEST)
        .cloned()
        .collect();
    if let Ok(rd) = fs::read_dir(dirs.packages()) {
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if let Some((v, _)) = parse_entry_name(archive_base(&name))
                && !kept.contains(&v)
                && !newest_archives.contains(&v)
            {
                let _ = fs::remove_file(f.path());
            }
        }
    }
    if let Ok(rd) = fs::read_dir(&dirs.root) {
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if name.starts_with(".tmp-") || name.starts_with(".trash-") {
                let _ = fs::remove_dir_all(f.path());
            }
        }
    }
    Ok(removed)
}

/// An archive file name without `.sha256` / `.part`.
fn archive_base(name: &str) -> &str {
    name.trim_end_matches(".sha256").trim_end_matches(".part")
}

/// Remove a directory tree. It is renamed away first (to `.trash-*` beside
/// it, retried while a file is in use) and only then deleted, so a directory
/// that cannot be moved stays whole, marker included, instead of becoming a
/// half-deleted entry; what cannot be deleted after the rename is left in the
/// trash for a later [`prune`]. Windows renames a directory whose program
/// runs (and then cannot delete the program), so a directory in use
/// ([`entry_in_use`]) is refused before anything is moved.
pub fn remove_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if let Some(why) = entry_in_use(dir, "") {
        return Err(in_use_error(dir, &why));
    }
    let parent = dir.parent().unwrap_or(Path::new("."));
    let trash = parent.join(format!(".trash-{}", nonce()));
    if let Err(e) = rename_retrying(dir, &trash) {
        return Err(if is_in_use(&e) {
            in_use_error(dir, &format!("a file in it is in use: {e}"))
        } else {
            Error::io(dir, &e)
        });
    }
    if let Err(e) = fs::remove_dir_all(&trash) {
        tracing::info!(trash = %trash.display(), error = %e, "removal left files behind; a later prune retries");
    }
    Ok(())
}

/// A fresh temporary directory name in the store.
pub fn temp_dir(dirs: &StoreDirs, label: &str) -> PathBuf {
    dirs.root.join(format!(".tmp-{label}-{}", nonce()))
}

/// 16 hex characters, unique per call.
pub fn nonce() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = Sha256::new();
    h.update(
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hex::encode(h.finalize())[..16].to_owned()
}
