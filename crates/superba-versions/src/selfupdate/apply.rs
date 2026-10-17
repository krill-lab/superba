//! Replacing the installed files: where Superba is installed, whether it can be
//! written, and the transactional swaps (several files, or the macOS bundle).
//!
//! Nothing here deletes the old version before the new one is in place: files
//! are copied next to their targets first, swapped by rename (the previous file
//! is kept under a `.superba-old` name until every swap worked and put back if
//! one fails), and the macOS bundle is renamed aside as a backup that
//! [`SelfUpdater`](super::SelfUpdater) removes once the new version has started.

use std::fs;
use std::path::{Path, PathBuf};

use superba_core::{Error, Result};

use super::release::Os;

/// The binaries a release ships, without the platform suffix.
pub const BINARIES: [&str; 3] = ["superba", "spb", "superba-router"];

/// Marker of the copy made next to its target.
const NEW_MARK: &str = "superba-new";

/// Marker of the file kept while a swap runs.
const OLD_MARK: &str = "superba-old";

/// File name of binary `base` on `os`.
pub fn bin_name(base: &str, os: Os) -> String {
    if os == Os::Windows {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

/// How Superba is installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallLayout {
    /// macOS: `Superba.app` (with `superba-router` inside), and `spb` beside the
    /// bundle when there is one.
    Bundle {
        /// The bundle.
        app: PathBuf,
        /// The `spb` next to it.
        spb: Option<PathBuf>,
    },
    /// Binaries side by side in one directory (Linux, Windows, a bare `spb`).
    Files {
        /// The directory.
        dir: PathBuf,
        /// File names present in it, among [`BINARIES`].
        names: Vec<String>,
    },
}

/// Locate the installation `exe` belongs to.
pub fn detect_layout(exe: &Path, os: Os) -> Option<InstallLayout> {
    if os == Os::Macos {
        if let Some(app) = exe
            .ancestors()
            .skip(1)
            .find(|a| a.extension().is_some_and(|e| e == "app"))
        {
            let spb = app.parent().map(|p| p.join("spb")).filter(|p| p.is_file());
            return Some(InstallLayout::Bundle {
                app: app.to_owned(),
                spb,
            });
        }
        let beside = exe.parent()?.join("Superba.app");
        if beside.is_dir() {
            return Some(InstallLayout::Bundle {
                app: beside,
                spb: Some(exe.to_owned()).filter(|p| p.file_name().is_some_and(|n| n == "spb")),
            });
        }
    }
    let dir = exe.parent()?.to_owned();
    let names: Vec<String> = BINARIES
        .iter()
        .map(|b| bin_name(b, os))
        .filter(|n| dir.join(n).is_file())
        .collect();
    (!names.is_empty()).then_some(InstallLayout::Files { dir, names })
}

/// A file can be created in `dir` (so siblings can be written and renamed).
pub fn writable_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".superba-write-test-{}", crate::store::nonce()));
    match fs::File::create(&probe) {
        Ok(f) => {
            drop(f);
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn sibling(path: &Path, mark: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{mark}"))
}

fn io(path: &Path, e: &std::io::Error) -> Error {
    Error::io(path, e)
}

/// Replace each `(new file, target)` pair; all or nothing. Returns the names
/// replaced. A running executable can be replaced on Unix (the swap is a
/// rename over it) and on Windows (it is renamed aside first).
pub fn replace_files(pairs: &[(PathBuf, PathBuf)]) -> Result<Vec<String>> {
    // 1. copy every new file next to its target.
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let discard = |staged: &[(PathBuf, PathBuf)]| {
        for (new, _) in staged {
            let _ = fs::remove_file(new);
        }
    };
    for (src, target) in pairs {
        let real = fs::canonicalize(target).unwrap_or_else(|_| target.clone());
        let new = sibling(&real, NEW_MARK);
        let _ = fs::remove_file(&new);
        if let Err(e) = fs::copy(src, &new) {
            discard(&staged);
            let _ = fs::remove_file(&new);
            return Err(io(&new, &e));
        }
        staged.push((new, real));
    }
    // 2. swap, keeping the old file until the end.
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let rollback = |done: &[(PathBuf, PathBuf)]| {
        for (real, old) in done.iter().rev() {
            let _ = fs::rename(old, real);
        }
    };
    for (i, (new, real)) in staged.iter().enumerate() {
        let old = sibling(real, OLD_MARK);
        let _ = fs::remove_file(&old);
        let result = keep_old(real, &old).and_then(|()| {
            fs::rename(new, real).inspect_err(|_| {
                let _ = fs::rename(&old, real);
            })
        });
        match result {
            Ok(()) => done.push((real.clone(), old)),
            Err(e) => {
                rollback(&done);
                discard(&staged[i..]);
                return Err(io(real, &e));
            }
        }
    }
    // 3. the swap worked: drop the old files (a running .exe cannot be deleted
    // on Windows; the next start removes it, see `remove_leftovers`).
    for (_, old) in &done {
        let _ = fs::remove_file(old);
    }
    Ok(done
        .iter()
        .filter_map(|(real, _)| real.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect())
}

/// Keep `target` under the name `old`: a hard link on Unix (the target stays in
/// place until the rename over it), a rename elsewhere.
#[cfg(unix)]
fn keep_old(target: &Path, old: &Path) -> std::io::Result<()> {
    if target.exists() {
        fs::hard_link(target, old)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn keep_old(target: &Path, old: &Path) -> std::io::Result<()> {
    if target.exists() {
        fs::rename(target, old)?;
    }
    Ok(())
}

/// Remove `.name.superba-old` / `.name.superba-new` files left in `dir` by a
/// swap that could not delete them (Windows) or that was interrupted.
pub fn remove_leftovers(dir: &Path) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.')
            && (name.ends_with(&format!(".{OLD_MARK}")) || name.ends_with(&format!(".{NEW_MARK}")))
            && entry.path().is_file()
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Swap the macOS bundle `app` for `new_app` (a directory on the same
/// filesystem, normally a sibling). The old bundle is kept as the returned
/// backup; a failed swap puts it back.
pub fn swap_bundle(app: &Path, new_app: &Path) -> Result<PathBuf> {
    let name = app
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Superba.app".into());
    let backup = app.with_file_name(format!(".{name}.previous"));
    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(|e| io(&backup, &e))?;
    }
    fs::rename(app, &backup).map_err(|e| io(app, &e))?;
    if let Err(e) = fs::rename(new_app, app) {
        let _ = fs::rename(&backup, app);
        return Err(io(app, &e));
    }
    Ok(backup)
}
