//! The DLL entry point Windows Installer calls (`DllEntry="CheckInstallFolder"`).

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
    MSIHANDLE, MsiGetPropertyW, MsiGetTargetPathW, MsiSetPropertyW,
};

const ERROR_SUCCESS: u32 = 0;
const ERROR_MORE_DATA: u32 = 234;

/// The install folder's directory (and property) in `superba.wxs`.
const FOLDER: &str = "INSTALLFOLDER";
/// Set to `1` when [`crate::can_install_into`] the folder, else `0`. The page
/// refuses the folder only on `0`: when this action cannot run, the property
/// stays empty and the install goes on (a folder it cannot write to then fails
/// the install, which rolls back).
const RESULT: &str = "SUPERBA_DIR_WRITABLE";

/// Immediate custom action, run from the folder page's Install button after
/// `SetTargetPath`, with the install handle Windows Installer passes. Always
/// returns success: the answer is in [`RESULT`].
#[unsafe(no_mangle)]
pub extern "system" fn CheckInstallFolder(install: MSIHANDLE) -> u32 {
    let _ = std::panic::catch_unwind(|| {
        let folder = read(install, FOLDER, true)
            .or_else(|| read(install, FOLDER, false))
            .filter(|f| !f.is_empty());
        let ok = folder.is_some_and(|f| crate::can_install_into(&PathBuf::from(f)));
        set(install, RESULT, if ok { "1" } else { "0" });
    });
    ERROR_SUCCESS
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The resolved target path of the directory `name` (`target`), or the
/// property `name`.
fn read(install: MSIHANDLE, name: &str, target: bool) -> Option<OsString> {
    let name = wide(name);
    let call = |buf: *mut u16, len: &mut u32| {
        // SAFETY: `name` is NUL-terminated and outlives the call; `buf` holds
        // `*len` characters (the API writes at most that many, NUL included).
        unsafe {
            if target {
                MsiGetTargetPathW(install, name.as_ptr(), buf, len)
            } else {
                MsiGetPropertyW(install, name.as_ptr(), buf, len)
            }
        }
    };
    let mut probe = [0u16; 1];
    let mut len = 0u32;
    match call(probe.as_mut_ptr(), &mut len) {
        ERROR_SUCCESS => return Some(OsString::new()),
        ERROR_MORE_DATA => {}
        _ => return None,
    }
    let mut buf = vec![0u16; len as usize + 1];
    let mut len = len + 1;
    if call(buf.as_mut_ptr(), &mut len) != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(len as usize);
    Some(OsString::from_wide(&buf))
}

fn set(install: MSIHANDLE, name: &str, value: &str) {
    let (name, value) = (wide(name), wide(value));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MsiSetPropertyW(install, name.as_ptr(), value.as_ptr());
    }
}

/// Remove only the GUI startup entry. Embedded in the MSI so removal works
/// even when installed executables are missing. Runs for REMOVE=Main and ALL,
/// but not the old product's removal during an upgrade.
#[unsafe(no_mangle)]
pub extern "system" fn RemoveAutostart(_install: MSIHANDLE) -> u32 {
    startup_action(|| {
        use superba_core::autostart::{RUN_KEY, RUN_VALUE};
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, KEY_SET_VALUE},
        };
        match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
            .and_then(|k| k.delete_value(RUN_VALUE))
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    })
}

/// An install/repair/upgrade refreshes an existing opt-in with INSTALLFOLDER.
/// A fresh install without a Run value leaves login startup off.
#[unsafe(no_mangle)]
pub extern "system" fn RefreshAutostart(install: MSIHANDLE) -> u32 {
    startup_action(|| {
        use superba_core::autostart::{RUN_KEY, RUN_VALUE, windows_command};
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE},
        };
        let key = match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE)
        {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match key.get_value::<String, _>(RUN_VALUE) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        let folder = read(install, FOLDER, true)
            .or_else(|| read(install, FOLDER, false))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| std::io::Error::other("missing INSTALLFOLDER"))?;
        let command = windows_command(&PathBuf::from(folder).join("superba.exe"))
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        key.set_value(RUN_VALUE, &command)
    })
}

fn startup_action(action: impl FnOnce() -> std::io::Result<()> + std::panic::UnwindSafe) -> u32 {
    match std::panic::catch_unwind(action) {
        Ok(Ok(())) => ERROR_SUCCESS,
        _ => 1603,
    }
}
