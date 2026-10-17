//! The Superba-managed Codex store `<data dir>/codex/`:
//!
//! ```text
//! <ver>-<target>/                 extracted, verified package (bin/codex, ...)
//! <ver>-<target>/.superba-verified  written last: the directory is complete
//! packages/<archive>              verified archives (remote uploads), + .sha256
//! packages/<archive>.part         a download in progress (resumed)
//! .tmp-*/                         extraction in progress (removed on failure)
//! .lock                           exclusive while downloading / extracting / pruning
//! releases.json                   release list cache
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

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

/// Remove entries of `target` beyond the newest [`KEEP_NEWEST`] versions,
/// never one in `keep`; their archives go too, as do archives of versions no
/// longer kept and leftovers of interrupted extractions. Returns removed
/// versions. Call with the store lock held.
pub fn prune(dirs: &StoreDirs, target: &str, keep: &[CodexVersion]) -> Result<Vec<CodexVersion>> {
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
        remove_dir(&dirs.entry(&e.version, target))?;
        removed.push(e.version.clone());
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
            if f.file_name().to_string_lossy().starts_with(".tmp-") {
                let _ = remove_dir(&f.path());
            }
        }
    }
    Ok(removed)
}

/// An archive file name without `.sha256` / `.part`.
fn archive_base(name: &str) -> &str {
    name.trim_end_matches(".sha256").trim_end_matches(".part")
}

/// Remove a directory tree (renamed away first, so a half-removed entry is
/// never mistaken for a complete one).
pub fn remove_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let marker = dir.join(VERIFIED_MARKER);
    let _ = fs::remove_file(&marker);
    fs::remove_dir_all(dir).map_err(|e| Error::io(dir, &e))
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
