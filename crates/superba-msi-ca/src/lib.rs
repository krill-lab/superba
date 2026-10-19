//! The Windows installer's custom action (user bug 64). The MSI's folder page
//! (`packaging/windows/superba-ui.wxs`) runs [`CheckInstallFolder`] when the user
//! clicks Install: Superba installs per user without elevation, so a folder the
//! user cannot write to (one that needs administrator rights) is refused there,
//! before anything is written, instead of failing halfway through the install.
//!
//! Built as `superba_msi_ca.dll` for the MSI's Binary table; it is never
//! installed and none of the programs link it. The check itself
//! ([`can_install_into`]) is plain Rust and tested on every platform.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
mod msi;

#[cfg(windows)]
pub use msi::{CheckInstallFolder, RefreshAutostart, RemoveAutostart};

/// Whether files can be installed into the folder `dir` with this process's
/// rights. Windows Installer writes into the folder when it exists, and
/// otherwise creates it (with any missing parents) inside the nearest folder
/// above it that exists; so this creates a file in `dir`, or a subfolder with a
/// file in that nearest existing folder, and removes what it made. False for an
/// empty or relative path, for a path where a file sits in place of a folder,
/// and when no folder above it exists (a missing drive).
pub fn can_install_into(dir: &Path) -> bool {
    if superba_storage::validation::validate_directory(dir, &superba_core::ProcessEnv).is_err() {
        return false;
    }
    if dir.as_os_str().is_empty() || !dir.is_absolute() {
        return false;
    }
    let mut base = dir;
    loop {
        match fs::metadata(base) {
            Ok(meta) if meta.is_dir() => break,
            Ok(_) => return false,
            Err(_) => match base.parent() {
                Some(parent) => base = parent,
                None => return false,
            },
        }
    }
    let probe = base.join(probe_name());
    if base == dir {
        let ok = create_file(&probe);
        let _ = fs::remove_file(&probe);
        ok
    } else {
        if fs::create_dir(&probe).is_err() {
            return false;
        }
        let ok = create_file(&probe.join("probe"));
        let _ = fs::remove_dir_all(&probe);
        ok
    }
}

fn create_file(path: &Path) -> bool {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .is_ok()
}

/// A name no install has: this process and the time.
fn probe_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(".superba-install-check-{}-{nanos}", std::process::id())
}
