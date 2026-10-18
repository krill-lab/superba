//! File helpers: atomic writes, replacing a program that may be running,
//! Superba's launcher scripts, removals that never follow links.

use std::fs;
use std::path::{Path, PathBuf};

use superba_core::{Error, Result};

/// Write `bytes` to `path` through a temporary file next to it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    }
    let tmp = sibling(path, &format!(".superba-{}.tmp", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, &e))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(path, &e));
    }
    Ok(())
}

/// `path` with `suffix` appended to its file name.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Move `new` over `target` (a program that may be running). Windows cannot
/// replace a running executable but can rename it: the old file becomes
/// `<target>.old` (removed now when possible, else at the next operation).
pub fn replace_program(new: &Path, target: &Path) -> Result<()> {
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    }
    if cfg!(windows) && target.exists() {
        let old = sibling(target, ".old");
        let _ = fs::remove_file(&old);
        fs::rename(target, &old).map_err(|e| Error::io(target, &e))?;
        if let Err(e) = fs::rename(new, target) {
            let _ = fs::rename(&old, target);
            return Err(Error::io(target, &e));
        }
        let _ = fs::remove_file(&old);
        return Ok(());
    }
    fs::rename(new, target).map_err(|e| Error::io(target, &e))
}

/// Remove `path` if it is a file or a link (never what a link points to).
pub fn remove_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Err(Error::Invalid {
            message: format!("{} is a folder, not a file", path.display()),
        }),
        Ok(_) => fs::remove_file(path)
            .map(|()| true)
            .map_err(|e| Error::io(path, &e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(path, &e)),
    }
}

/// Remove the folder `path` and everything in it (links inside are removed,
/// not followed); a link at `path` itself is only unlinked.
pub fn remove_dir(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => remove_file(path),
        Ok(_) => fs::remove_dir_all(path)
            .map(|()| true)
            .map_err(|e| Error::io(path, &e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(path, &e)),
    }
}

/// Remove `dir` and then its parents up to (not including) `stop` while they
/// are empty.
pub fn remove_empty_dirs(dir: &Path, stop: &Path) {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        if d == stop || !d.starts_with(stop) {
            break;
        }
        if fs::remove_dir(&d).is_err() {
            break;
        }
        cur = d.parent().map(Path::to_path_buf);
    }
}

/// The marker line of a launcher Superba wrote.
pub const LAUNCHER_MARKER: &str = "# Installed by Superba (superba-tools); removed with the tool.";

/// A Unix launcher script that runs `target` with the caller's arguments.
pub fn launcher_script(target: &Path) -> String {
    let t = target.to_string_lossy().replace('\'', "'\\''");
    format!("#!/bin/sh\n{LAUNCHER_MARKER}\nexec '{t}' \"$@\"\n")
}

/// A Windows `.cmd` launcher that runs `target` with the caller's arguments.
pub fn launcher_cmd(target: &Path) -> String {
    format!(
        "@echo off\r\nrem {}\r\n\"{}\" %*\r\n",
        LAUNCHER_MARKER.trim_start_matches("# "),
        target.display()
    )
}

/// `path` is a launcher Superba wrote (or absent).
pub fn is_own_launcher(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Err(_) => true,
        Ok(m) if m.is_file() => fs::read(path).is_ok_and(|b| {
            b.len() < 4096
                && String::from_utf8_lossy(&b).contains(LAUNCHER_MARKER.trim_start_matches("# "))
        }),
        Ok(_) => false,
    }
}

/// Write a launcher for `target` at `path` (Unix script, executable). Refused
/// when another program is there.
pub fn write_launcher(path: &Path, target: &Path) -> Result<()> {
    if !is_own_launcher(path) {
        return Err(Error::Conflict {
            message: format!(
                "{} already exists and was not written by Superba; it was left alone",
                path.display()
            ),
        });
    }
    let text = if cfg!(windows) {
        launcher_cmd(target)
    } else {
        launcher_script(target)
    };
    write_atomic(path, text.as_bytes())?;
    crate::archive::make_executable(path)
}

/// Size of a file or folder tree in bytes (links not followed).
pub fn tree_size(path: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !m.is_dir() {
        return m.len();
    }
    fs::read_dir(path)
        .map(|d| d.flatten().map(|e| tree_size(&e.path())).sum())
        .unwrap_or(0)
}

/// A fresh, empty staging folder `<parent>/.superba-staging-<name>`.
pub fn staging_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    let dir = parent.join(format!(".superba-staging-{name}"));
    let _ = remove_dir(&dir);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, &e))?;
    Ok(dir)
}
