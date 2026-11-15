//! The Codex program Superba may run for a desktop app install (user bug 143,
//! GH#32).
//!
//! The ChatGPT app for Windows installed as an MSIX package keeps its Codex
//! inside the package (`C:\Program Files\WindowsApps\OpenAI.Codex_…\app\
//! resources\codex.exe`). Windows refuses to start a package file from a
//! process outside the package (`Access is denied. (os error 5)`); reading it
//! is allowed. The app runs it in one of two ways:
//!
//! - **Relocated** (26.1013 to 26.1111, when the "app-contained core" is off):
//!   before it starts its app-server the app copies `codex.exe` and the helpers
//!   it starts from its own folder ([`RELOCATED_SIBLINGS`]) out of the package to
//!   `%LOCALAPPDATA%\OpenAI\Codex\bin\<16 hex digits>\`, the folder name being
//!   [`copy_folder_name`] of the copied files (staged in `.staging-<name>-*`,
//!   renamed into place, other `<16 hex>` folders pruned), and checks each
//!   copy's size and SHA-256 before it reuses it.
//! - **App-contained core** (26.1106+ behind a gate; 26.1111 also by default on
//!   Windows builds above 26200, and on 26100 / 26200 from update revision
//!   9278): the app, which has the package identity, runs `codex.exe` straight
//!   from the package and makes **no** copy at all.
//!
//! [`runnable_codex`] is the one place that turns an app's Codex path into the
//! path Superba executes (`codex debug models`, `--version`, `features list`,
//! `app-server`): a path outside `WindowsApps` is returned unchanged (Linux,
//! macOS and every non-packaged install behave as before). For a packaged one
//! it uses, in this order: the app's extracted copy whose SHA-256 is identical;
//! Superba's own copy under `<data dir>/app-codex/<16 hex digits>/`
//! ([`superba_core::DataDir::app_codex_dir`]); else it makes that copy itself,
//! the way the app does (same files, same folder naming, staged and renamed,
//! every file's size and SHA-256 verified after copying, older copies pruned).
//! Only when that fails too does the caller get [`Error::PackagedCodex`], with
//! the reason. Hashes are remembered per file stamp (path, size, mtime) like
//! [`crate::codexbin::scanned_version`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use superba_core::{EnvSource, Error, LocalContext, Result};

use crate::codexbin::{FileStamp, lock, sha256_file};

/// The helpers the app copies together with `codex.exe` (the programs Codex
/// starts from its own folder), the same list from 26.1013 to 26.1111. A
/// helper missing from the package is skipped.
pub const RELOCATED_SIBLINGS: [&str; 3] = [
    "codex-code-mode-host.exe",
    "codex-windows-sandbox-setup.exe",
    "codex-command-runner.exe",
];

/// Staging folders older than this are left over from an interrupted copy.
const STALE_STAGING: Duration = Duration::from_secs(60 * 60);

/// Whether `binary` is a file inside an MSIX package that Superba must not
/// start itself: on Windows, a path under a `WindowsApps` package store. Never
/// on Linux or macOS.
pub fn is_packaged(binary: &Path) -> bool {
    cfg!(windows) && crate::appx::in_windows_apps(binary)
}

/// Where the app extracts its Codex: `%LOCALAPPDATA%`, else
/// `%USERPROFILE%\AppData\Local` (what the app itself falls back to).
pub fn local_app_data(env: &dyn EnvSource) -> Option<PathBuf> {
    let var = |key: &str| {
        env.var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    var("LOCALAPPDATA")
        .or_else(|| var("USERPROFILE").map(|home| home.join("AppData").join("Local")))
}

/// Where copies of a packaged Codex are looked for, and made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackagedCopies {
    /// `%LOCALAPPDATA%`: the app's copies are under `OpenAI\Codex\bin`.
    pub local_app_data: Option<PathBuf>,
    /// Superba's own copies (`<data dir>/app-codex`); `None`: none are looked
    /// for or made.
    pub own_dir: Option<PathBuf>,
}

impl PackagedCopies {
    /// The locations of this machine: the app's under its local app data,
    /// Superba's in its data directory.
    pub fn of(ctx: &LocalContext) -> Self {
        Self {
            local_app_data: local_app_data(ctx.env.as_ref()),
            own_dir: Some(ctx.data_dir.app_codex_dir()),
        }
    }
}

/// The Codex to execute for the app Codex `binary` (see the module docs).
/// Blocking: it may hash and copy files; call it from `spawn_blocking` in
/// async code.
pub fn runnable_codex(binary: &Path, ctx: &LocalContext) -> Result<PathBuf> {
    runnable_codex_with(binary, &PackagedCopies::of(ctx), &is_packaged)
}

/// [`runnable_codex`] with the copy locations and the "packaged" check given
/// (tests force it on every platform).
pub fn runnable_codex_with(
    binary: &Path,
    copies: &PackagedCopies,
    packaged: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf> {
    if !packaged(binary) {
        return Ok(binary.to_path_buf());
    }
    resolve_packaged_with(binary, copies)
}

/// The app's extracted copy of the packaged `binary` under `local_app_data`
/// with the same size and SHA-256 (the most recently written first), else
/// [`Error::PackagedCodex`]. Looks only; copies nothing (the bundled Codex's
/// version, [`crate::install::bundled_codex_version`]). Public for tests on
/// every platform.
pub fn resolve_packaged(binary: &Path, local_app_data: Option<&Path>) -> Result<PathBuf> {
    let copies = PackagedCopies {
        local_app_data: local_app_data.map(Path::to_path_buf),
        own_dir: None,
    };
    resolve_packaged_with(binary, &copies)
}

/// [`runnable_codex`] for a binary already known to be packaged: the app's
/// identical copy, else Superba's own (found or made now), else
/// [`Error::PackagedCodex`] with the reason.
pub fn resolve_packaged_with(binary: &Path, copies: &PackagedCopies) -> Result<PathBuf> {
    if let Some(local) = &copies.local_app_data
        && let Some(copy) = app_copy(binary, local)
    {
        tracing::debug!(
            packaged = %binary.display(),
            copy = %copy.display(),
            "running the app's extracted Codex (same SHA-256)"
        );
        return Ok(copy);
    }
    let Some(own) = &copies.own_dir else {
        return Err(Error::PackagedCodex {
            path: binary.to_path_buf(),
            reason: "no identical copy, and none is made here".into(),
        });
    };
    match own_copy(binary, own) {
        Ok(copy) => Ok(copy),
        Err(reason) => {
            tracing::warn!(
                packaged = %binary.display(),
                own = %own.display(),
                %reason,
                "no runnable copy of the packaged Codex"
            );
            Err(Error::PackagedCodex {
                path: binary.to_path_buf(),
                reason,
            })
        }
    }
}

/// The app's copy (`%LOCALAPPDATA%\OpenAI\Codex\bin\…\codex.exe`) with the
/// size and SHA-256 of `binary`.
fn app_copy(binary: &Path, local: &Path) -> Option<PathBuf> {
    let size = std::fs::metadata(binary).ok().map(|m| m.len())?;
    let mut packaged_sha: Option<Option<String>> = None;
    for copy in crate::appx::relocated_codex_candidates(local) {
        let same_size = std::fs::metadata(&copy).is_ok_and(|m| m.len() == size);
        if !same_size {
            continue;
        }
        let want = packaged_sha.get_or_insert_with(|| cached_sha256(binary));
        // An unreadable package file cannot be verified.
        let want = want.as_deref()?;
        if cached_sha256(&copy).as_deref() == Some(want) {
            return Some(copy);
        }
    }
    None
}

/// One file of a copy set: its name, the package file, its size and SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopiedFile {
    /// File name (`codex.exe`).
    pub name: String,
    /// The file inside the package.
    pub source: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256, lower-case hex.
    pub sha256: String,
}

/// The name of the folder a set of copied files goes to, as the app names its
/// own: the first 16 hex digits of the SHA-256 over `name NUL sha256-hex NUL`
/// of each file, in order (`codex.exe` first, then [`RELOCATED_SIBLINGS`]).
pub fn copy_folder_name(files: &[CopiedFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(file.name.as_bytes());
        hasher.update([0]);
        hasher.update(file.sha256.as_bytes());
        hasher.update([0]);
    }
    let mut name = hex::encode(hasher.finalize());
    name.truncate(16);
    name
}

/// Whether `name` is a copy folder's name (16 lower-case hex digits).
fn is_copy_folder(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The files to copy for the packaged `binary`: itself and the helpers next
/// to it that the package has.
pub fn copy_set(binary: &Path) -> std::result::Result<Vec<CopiedFile>, String> {
    let dir = binary
        .parent()
        .ok_or_else(|| format!("{} has no folder", binary.display()))?;
    let name = binary
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no file name", binary.display()))?;
    let mut files = Vec::new();
    for (name, required) in std::iter::once((name, true))
        .chain(RELOCATED_SIBLINGS.iter().map(|sibling| (*sibling, false)))
    {
        let source = dir.join(name);
        let meta = match std::fs::metadata(&source) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) | Err(_) if !required => continue,
            Ok(_) => return Err(format!("{} is not a file", source.display())),
            Err(e) => return Err(format!("cannot read {}: {e}", source.display())),
        };
        let sha256 =
            cached_sha256(&source).ok_or_else(|| format!("cannot read {}", source.display()))?;
        files.push(CopiedFile {
            name: name.to_owned(),
            source,
            size: meta.len(),
            sha256,
        });
    }
    Ok(files)
}

/// Whether `dir` holds every file of `files` with its size and SHA-256.
fn holds(dir: &Path, files: &[CopiedFile], fresh: bool) -> bool {
    files.iter().all(|file| {
        let path = dir.join(&file.name);
        let same_size = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == file.size);
        let sha = if fresh {
            sha256_file(&path).ok()
        } else {
            cached_sha256(&path)
        };
        same_size && sha.as_deref() == Some(file.sha256.as_str())
    })
}

/// Superba's own copy of the packaged `binary` under `own`: an existing,
/// verified one, else made now (staged, verified, renamed into place), older
/// copies pruned. The error is the reason, for [`Error::PackagedCodex`].
pub fn own_copy(binary: &Path, own: &Path) -> std::result::Result<PathBuf, String> {
    let files = copy_set(binary)?;
    let name = copy_folder_name(&files);
    let target = own.join(&name);
    let program = target.join(&files[0].name);
    if holds(&target, &files, false) {
        prune(own, &name);
        return Ok(program);
    }
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| format!("cannot replace the damaged copy {}: {e}", target.display()))?;
    }
    std::fs::create_dir_all(own).map_err(|e| format!("cannot create {}: {e}", own.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(&format!(".staging-{name}-"))
        .tempdir_in(own)
        .map_err(|e| format!("cannot create a folder in {}: {e}", own.display()))?;
    for file in &files {
        let to = staging.path().join(&file.name);
        std::fs::copy(&file.source, &to).map_err(|e| {
            format!(
                "cannot copy {} to {}: {e}",
                file.source.display(),
                to.display()
            )
        })?;
    }
    if !holds(staging.path(), &files, true) {
        return Err(format!(
            "the copy in {} does not match the package files (size or SHA-256)",
            staging.path().display()
        ));
    }
    let staged = staging.keep();
    if let Err(e) = std::fs::rename(&staged, &target) {
        let _ = std::fs::remove_dir_all(&staged);
        // Another Superba process may have made the same copy meanwhile.
        if !holds(&target, &files, true) {
            return Err(format!("cannot move the copy to {}: {e}", target.display()));
        }
    }
    tracing::info!(
        packaged = %binary.display(),
        copy = %program.display(),
        files = files.len(),
        "copied the packaged Codex out of the app package"
    );
    prune(own, &name);
    Ok(program)
}

/// Remove the copy folders of `own` other than `keep`, and staging folders
/// left over from interrupted copies. Errors are ignored (a copy whose program
/// is running cannot be removed on Windows; the next prune tries again).
fn prune(own: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(own) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let stale_staging = name.starts_with(".staging-")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|age| age > STALE_STAGING);
        if ((is_copy_folder(name) && name != keep) || stale_staging)
            && let Err(e) = std::fs::remove_dir_all(entry.path())
        {
            tracing::debug!(dir = %entry.path().display(), error = %e, "old Codex copy kept");
        }
    }
}

/// SHA-256 (hex) of a file, remembered per [`FileStamp`]; `None` when it
/// cannot be read.
pub fn cached_sha256(path: &Path) -> Option<String> {
    type Cache = Mutex<HashMap<FileStamp, String>>;
    static CACHE: LazyLock<Cache> = LazyLock::new(Default::default);

    let stamp = FileStamp::of(path)?;
    if let Some(sha) = lock(&CACHE).get(&stamp) {
        return Some(sha.clone());
    }
    let sha = sha256_file(&stamp.path).ok()?;
    let mut cache = lock(&CACHE);
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(stamp, sha.clone());
    Some(sha)
}
