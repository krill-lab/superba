//! The Codex program Superba may run for a desktop app install (user bug 143,
//! GH#32).
//!
//! The ChatGPT app for Windows installed as an MSIX package keeps its Codex
//! inside the package (`C:\Program Files\WindowsApps\OpenAI.Codex_…\app\
//! resources\codex.exe`). Windows refuses to start a package file from a
//! process outside the package (`Access is denied. (os error 5)`), so the app
//! itself copies the binary out before it runs it, to
//! `%LOCALAPPDATA%\OpenAI\Codex\bin\<16 hex digits>\codex.exe` (26.1013 to
//! 26.1106; the folder name is a hash of the copied files, and the app checks
//! each copy's size and SHA-256 before it reuses it).
//!
//! [`runnable_codex`] is the one place that turns an app's Codex path into the
//! path Superba executes (`codex debug models`, `--version`, `features list`,
//! `app-server`): a path outside `WindowsApps` is returned unchanged (Linux,
//! macOS and every non-packaged install behave as before); a packaged one is
//! replaced by the app's extracted copy whose SHA-256 is identical, and without
//! such a copy the caller gets [`Error::PackagedCodex`], which says what to do,
//! instead of starting the package file only to see it refused. Hashes are
//! remembered per file stamp (path, size, mtime) like
//! [`crate::codexbin::scanned_version`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use superba_core::{EnvSource, Error, Result};

use crate::codexbin::{FileStamp, lock, sha256_file};

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

/// The Codex to execute for the app Codex `binary` (see the module docs).
/// Blocking: it may hash files; call it from `spawn_blocking` in async code.
pub fn runnable_codex(binary: &Path, env: &dyn EnvSource) -> Result<PathBuf> {
    if !is_packaged(binary) {
        return Ok(binary.to_path_buf());
    }
    resolve_packaged(binary, local_app_data(env).as_deref())
}

/// [`runnable_codex`] for a binary already known to be packaged: the app's
/// extracted copy under `local_app_data` with the same size and SHA-256 (the
/// most recently written first), else [`Error::PackagedCodex`]. Public for
/// tests on every platform.
pub fn resolve_packaged(binary: &Path, local_app_data: Option<&Path>) -> Result<PathBuf> {
    let unrunnable = || Error::PackagedCodex {
        path: binary.to_path_buf(),
    };
    let Some(local) = local_app_data else {
        return Err(unrunnable());
    };
    let size = std::fs::metadata(binary).ok().map(|m| m.len());
    let mut packaged_sha: Option<Option<String>> = None;
    for copy in crate::appx::relocated_codex_candidates(local) {
        let same_size = std::fs::metadata(&copy).is_ok_and(|m| Some(m.len()) == size);
        if !same_size {
            continue;
        }
        let want = packaged_sha.get_or_insert_with(|| cached_sha256(binary));
        let Some(want) = want.as_deref() else {
            // The package file cannot be read: nothing can be verified.
            break;
        };
        if cached_sha256(&copy).as_deref() == Some(want) {
            tracing::debug!(
                packaged = %binary.display(),
                copy = %copy.display(),
                "running the app's extracted Codex (same SHA-256)"
            );
            return Ok(copy);
        }
    }
    tracing::warn!(
        packaged = %binary.display(),
        "no extracted copy of the packaged Codex with the same SHA-256"
    );
    Err(unrunnable())
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
