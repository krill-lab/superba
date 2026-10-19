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

    // An elevated process can create a directory owned by Administrators.
    // Grant the token's user explicitly so the same user's ordinary desktop
    // process can still read it after a move. Do not inherit Everyone on D:.
    let user = windows_user_sid().map_err(|e| Error::io(path, &e))?;
    let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user})")
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

#[cfg(windows)]
fn windows_user_sid() -> std::io::Result<String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut raw = std::ptr::null_mut();
    // SAFETY: `raw` receives an owned token handle, closed by OwnedHandle below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken succeeded and returned a uniquely owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut needed = 0;
    // SAFETY: a size query; no output buffer is passed.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: the buffer is pointer-aligned and contains at least needed bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the successful query returned a TOKEN_USER and its SID in buffer.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = std::ptr::null_mut();
    // SAFETY: sid remains valid and text receives an allocated UTF-16 string.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: Windows returned a NUL-terminated string; free it once after copying.
    let value = unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        value
    };
    Ok(value)
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

#[cfg(all(windows, test))]
mod windows_tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    fn dacl(path: &Path) -> String {
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        let mut text = std::ptr::null_mut();
        // SAFETY: input is NUL-terminated, output pointers receive Windows-owned
        // allocations. Both allocations are freed after the descriptor is copied.
        unsafe {
            assert_eq!(
                GetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut descriptor,
                ),
                0,
            );
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            );
            LocalFree(descriptor);
            assert_ne!(ok, 0);
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            value
        }
    }

    #[test]
    fn private_directory_and_files_grant_the_user_instead_of_the_owner_group() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private");
        private_dir(&path).unwrap();
        let file = path.join("secret.txt");
        fs::write(&file, "fixture").unwrap();
        let user = windows_user_sid().unwrap();
        for path in [&path, &file] {
            let dacl = dacl(path);
            assert!(dacl.contains(&format!(";;;{user})")), "{dacl}");
            assert!(dacl.contains(";;;SY)"), "{dacl}");
            assert!(
                !dacl.contains(";;;OW)"),
                "owner may be Administrators: {dacl}"
            );
            assert!(!dacl.contains(";;;WD)"), "{dacl}");
            assert!(!dacl.contains(";;;BU)"), "{dacl}");
        }
    }
}
