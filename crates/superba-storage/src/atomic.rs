use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use superba_core::{Error, Result};

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| Error::io(path, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::io(path, &e))?;
    }
    #[cfg(windows)]
    protect_windows_directory(path)?;
    Ok(())
}

#[cfg(windows)]
fn protect_windows_directory(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    };

    // OWNER RIGHTS gives the owner full control; both ACEs inherit into files
    // and subdirectories. A custom directory on D: must not inherit Everyone.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;OW)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let path_w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: NUL-terminated strings and all output buffers outlive these calls.
    // The descriptor allocated by Windows is freed once after extracting its DACL.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(Error::io(path, &std::io::Error::last_os_error()));
        }
        let result =
            if GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) == 0 {
                Err(Error::io(path, &std::io::Error::last_os_error()))
            } else {
                let rc = SetNamedSecurityInfoW(
                    path_w.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    acl,
                    std::ptr::null(),
                );
                if rc == 0 {
                    Ok(())
                } else {
                    Err(Error::io(
                        path,
                        &std::io::Error::from_raw_os_error(rc as i32),
                    ))
                }
            };
        LocalFree(descriptor);
        result
    }
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::other(format!("serialize data location: {e}")))?;
    write_atomic(path, &bytes)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid("data.path.absolute"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| Error::io(parent, &e))?;
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|e| Error::io(path, &e))?;
    temp.persist(path).map_err(|e| Error::io(path, &e.error))?;
    sync_dir(parent)?;
    Ok(())
}

pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|d| d.sync_all())
        .map_err(|e| Error::io(path, &e))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::parse("data location", format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, &e)),
    }
}
