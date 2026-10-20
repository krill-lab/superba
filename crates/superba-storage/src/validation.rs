//! Directory rules shared with the per-user MSI: absolute local paths, outside
//! system folders, with a real write probe in the nearest existing directory.

use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};

use superba_core::{EnvSource, Error, Result};

/// Symlinks and Windows junctions / other reparse points are never traversed by
/// copying or cleanup. Ordinary metadata follows junctions on Windows.
pub fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// Pure Windows policy, including device and UNC syntax, drive roots, reserved
/// names and default system directories. OS environment locations are also
/// checked by [`validate_directory`].
pub fn windows_path_allowed(text: &str) -> bool {
    let text = text.replace('/', "\\");
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let b = text.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return false;
    }
    let components: Vec<_> = text[3..].split('\\').filter(|s| !s.is_empty()).collect();
    if components.is_empty() {
        return false;
    }
    let first = components[0].to_ascii_lowercase();
    if matches!(
        first.as_str(),
        "windows"
            | "program files"
            | "program files (x86)"
            | "programdata"
            | "system volume information"
            | "$recycle.bin"
            | "recovery"
            | "config.msi"
            | "$windows.~bt"
            | "$windows.~ws"
    ) {
        return false;
    }
    for part in components {
        if part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"|?*".contains(c))
        {
            return false;
        }
        let name = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            || (name.len() == 4
                && (name.starts_with("COM") || name.starts_with("LPT"))
                && name.as_bytes()[3].is_ascii_digit())
        {
            return false;
        }
    }
    true
}

/// True when `path` names a system directory or anything below it. Path
/// comparisons use components, so `/usr-data` and `C:\Windows-data` stay valid.
pub fn system_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        !windows_path_allowed(&superba_core::paths::display_path(path))
    }
    #[cfg(not(windows))]
    {
        if path.parent().is_none() {
            return true;
        }
        let blocked: &[&str] = if cfg!(target_os = "macos") {
            &[
                "/System",
                "/Library",
                "/Applications",
                "/bin",
                "/sbin",
                "/usr",
                "/dev",
                "/etc",
                "/private/etc",
                "/private/var/db",
                "/private/var/root",
                "/cores",
            ]
        } else {
            &[
                "/bin", "/sbin", "/usr", "/etc", "/boot", "/dev", "/proc", "/sys", "/run", "/lib",
                "/lib64", "/var", "/snap", "/root",
            ]
        };
        blocked.iter().any(|p| path.starts_with(p))
    }
}

/// Canonicalize the existing ancestor and append the missing normal components.
/// This detects aliases into the source, network mounts and system folders.
pub fn planned_path(path: &Path) -> Result<(PathBuf, PathBuf)> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::invalid("data.path.absolute"));
    }
    #[cfg(windows)]
    if !windows_path_allowed(&superba_core::paths::display_path(path)) {
        return Err(Error::invalid("data.path.system"));
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                if ancestor == path && is_link(&meta) {
                    return Err(Error::invalid("data.path.link"));
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| Error::invalid("data.path.absolute"))?;
                missing.push(name.to_owned());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| Error::invalid("data.path.absolute"))?;
            }
            Err(e) => return Err(Error::io(ancestor, &e)),
        }
    }
    let real = superba_core::paths::canonical(ancestor).map_err(|e| Error::io(ancestor, &e))?;
    if !real.is_dir() {
        return Err(Error::invalid("data.path.directory"));
    }
    let mut full = real.clone();
    for name in missing.into_iter().rev() {
        full.push(name);
    }
    Ok((full, real))
}

/// Validate a local writable destination; existing contents are allowed here
/// (the MSI installs into an existing directory). Relocation additionally requires
/// an empty directory, no source overlap, and enough available bytes.
pub fn validate_directory(path: &Path, env: &dyn EnvSource) -> Result<PathBuf> {
    let (full, ancestor) = planned_path(path)?;
    if system_path(&full) {
        return Err(Error::invalid("data.path.system"));
    }
    #[cfg(windows)]
    for key in [
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(value) = env.var_os(key) {
            let forbidden = PathBuf::from(value);
            let forbidden = superba_core::paths::canonical(&forbidden).unwrap_or(forbidden);
            let a = superba_core::paths::display_path(&full).to_lowercase();
            let b = superba_core::paths::display_path(&forbidden).to_lowercase();
            if a == b || a.starts_with(&format!("{}\\", b.trim_end_matches('\\'))) {
                return Err(Error::invalid("data.path.system"));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = env;
    local_volume(&ancestor)?;
    // Like the MSI: create a private temporary directory and a file in it,
    // then remove both. This checks directory creation and actual file writes.
    let probe = tempfile::Builder::new()
        .prefix(".superba-write-check-")
        .tempdir_in(&ancestor)
        .map_err(|e| Error::io(&ancestor, &e))?;
    let path = probe.path().join("probe");
    fs::write(&path, b"Superba").map_err(|e| Error::io(&path, &e))?;
    probe.close().map_err(|e| Error::io(&ancestor, &e))?;
    Ok(full)
}

#[cfg(windows)]
fn local_volume(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; 32768];
    // SAFETY: input is NUL-terminated and output has the declared capacity.
    let ok = unsafe { GetVolumePathNameW(name.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == 0 {
        return Err(Error::io(path, &std::io::Error::last_os_error()));
    }
    // SAFETY: GetVolumePathNameW wrote a NUL-terminated volume root.
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    // DRIVE_FIXED and DRIVE_REMOVABLE. Reject network, optical, RAM and unknown.
    if kind != 2 && kind != 3 {
        return Err(Error::invalid("data.path.network"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn local_volume(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::invalid("data.path.absolute"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: path is NUL-terminated and stat points to a correctly sized buffer.
    if unsafe { libc::statfs(path_c.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::io(path, &std::io::Error::last_os_error()));
    }
    // SAFETY: statfs succeeded.
    let stat = unsafe { stat.assume_init() };
    // Network filesystems and FUSE (which can be sshfs/rclone). Reject unknown
    // FUSE transports rather than placing SQLite and keys on a remote filesystem.
    let kind = stat.f_type as u64;
    if matches!(
        kind,
        0x6969
            | 0xff534d42
            | 0xfe534d42
            | 0x517b
            | 0x5346414f
            | 0x6b414653
            | 0x00c36400
            | 0x73757245
            | 0x01021997
            | 0x65735546
            | 0x564c
    ) {
        return Err(Error::invalid("data.path.network"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn local_volume(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::invalid("data.path.absolute"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: name is NUL-terminated; stat is a writable statfs buffer.
    if unsafe { libc::statfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::io(path, &std::io::Error::last_os_error()));
    }
    // SAFETY: statfs succeeded.
    if unsafe { stat.assume_init() }.f_flags & libc::MNT_LOCAL as u32 == 0 {
        return Err(Error::invalid("data.path.network"));
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn local_volume(_path: &Path) -> Result<()> {
    Err(Error::invalid("data.path.network"))
}
