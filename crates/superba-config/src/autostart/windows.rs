//! Only the GUI's named HKCU value is touched; the router's value is independent.

use super::{EntryStore, RUN_KEY, RUN_VALUE};
use std::path::PathBuf;
use superba_core::{Error, Result};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE},
};

#[derive(Debug)]
pub(super) struct RunEntry;
fn error(e: std::io::Error) -> Error {
    Error::other(format!("HKCU\\{RUN_KEY}\\{RUN_VALUE}: {e}"))
}
impl EntryStore for RunEntry {
    fn location(&self) -> String {
        format!("HKCU\\{RUN_KEY}\\{RUN_VALUE}")
    }
    fn read(&self) -> Result<Option<String>> {
        let result = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .and_then(|key| key.get_value(RUN_VALUE));
        match result {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(error(e)),
        }
    }
    fn write(&self, entry: Option<&str>) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let result = match entry {
            Some(s) => hkcu
                .create_subkey(RUN_KEY)
                .and_then(|(key, _)| key.set_value(RUN_VALUE, &s)),
            None => hkcu
                .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
                .and_then(|key| key.delete_value(RUN_VALUE)),
        };
        match result {
            Ok(()) => Ok(()),
            Err(e) if entry.is_none() && e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(error(e)),
        }
    }
}

pub(super) fn installed_gui() -> Result<Option<PathBuf>> {
    let value: std::io::Result<String> = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Krill\Superba")
        .and_then(|k| k.get_value("InstallDir"));
    match value {
        Ok(s) => Ok(Some(PathBuf::from(s).join("superba.exe"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(e)),
    }
}
