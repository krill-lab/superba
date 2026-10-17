//! Safe extraction of a verified Codex package (`.tar.gz`).
//!
//! Every entry path must be relative and stay inside the destination (no `..`,
//! no root or drive prefix); only regular files and directories are accepted:
//! symbolic and hard links are refused outright (real Codex packages have none,
//! so nothing can be written or resolved through a link); total size and entry
//! count are bounded. npm tarballs keep the package under
//! `package/vendor/<target>/`, which becomes the root (other entries are
//! skipped).

use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use superba_core::{Error, Result};

use crate::sources::Layout;

/// Most bytes extracted from one package (an extracted 0.162 package is ~430 MB).
pub const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Most entries in one package.
pub const MAX_ENTRIES: usize = 20_000;

fn reject(archive: &Path, what: impl std::fmt::Display) -> Error {
    Error::Invalid {
        message: format!("refused to extract {}: {what}", archive.display()),
    }
}

/// A relative path made only of normal components (`.` dropped), or `None`.
fn clean_relative(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Extract `archive` (gzip-compressed tar) into the new directory `dest`.
pub fn extract(archive: &Path, layout: Layout, target: &str, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive).map_err(|e| Error::io(archive, &e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    fs::create_dir_all(dest).map_err(|e| Error::io(dest, &e))?;
    let prefix: PathBuf = match layout {
        Layout::Package => PathBuf::new(),
        Layout::Npm => ["package", "vendor", target].iter().collect(),
    };
    let mut total: u64 = 0;
    let entries = tar.entries().map_err(|e| reject(archive, e))?;
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES {
            return Err(reject(archive, "too many entries"));
        }
        let mut entry = entry.map_err(|e| reject(archive, e))?;
        let raw = entry.path().map_err(|e| reject(archive, e))?.into_owned();
        let Some(path) = clean_relative(&raw) else {
            if raw.components().all(|c| matches!(c, Component::CurDir)) {
                continue;
            }
            return Err(reject(archive, format!("unsafe path {}", raw.display())));
        };
        let rel = match path.strip_prefix(&prefix) {
            Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
            _ => continue,
        };
        let out = dest.join(&rel);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&out).map_err(|e| Error::io(&out, &e))?;
            continue;
        }
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(reject(
                archive,
                format!("{} is a link (packages hold no links)", rel.display()),
            ));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, &e))?;
        }
        if !kind.is_file() {
            return Err(reject(
                archive,
                format!("unsupported entry type for {}", rel.display()),
            ));
        }
        let size = entry.header().size().map_err(|e| reject(archive, e))?;
        total = total.saturating_add(size);
        if total > MAX_EXTRACTED_BYTES {
            return Err(reject(archive, "package larger than the extraction limit"));
        }
        let mode = entry.header().mode().unwrap_or(0o644);
        let mut f = fs::File::create(&out).map_err(|e| Error::io(&out, &e))?;
        let copied =
            std::io::copy(&mut (&mut entry).take(size), &mut f).map_err(|e| Error::io(&out, &e))?;
        if copied != size {
            return Err(reject(archive, format!("{} is truncated", rel.display())));
        }
        drop(f);
        set_mode(&out, mode).map_err(|e| Error::io(&out, &e))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Owner always reads and writes; group/other never write; no setuid bits.
    fs::set_permissions(path, fs::Permissions::from_mode((mode & 0o755) | 0o600))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}
