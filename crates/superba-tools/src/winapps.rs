//! Windows desktop apps installed per user: their entries under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall` (version,
//! folder, quiet uninstall command), launch-at-login values under
//! `HKCU\...\Run`, and command lines as Windows splits them. Off Windows the
//! registry functions find nothing.

use std::path::PathBuf;

/// One `Uninstall` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UninstallEntry {
    /// Key name.
    pub key: String,
    /// Under `HKEY_CURRENT_USER` (else `HKEY_LOCAL_MACHINE`: needs elevation
    /// to remove).
    pub per_user: bool,
    /// `DisplayName`.
    pub name: String,
    /// `DisplayVersion`.
    pub version: Option<String>,
    /// `InstallLocation`.
    pub location: Option<PathBuf>,
    /// `DisplayIcon` (usually the program).
    pub icon: Option<String>,
    /// `UninstallString`.
    pub uninstall: Option<String>,
    /// `QuietUninstallString`.
    pub quiet_uninstall: Option<String>,
}

/// Split a Windows command line into the program and its arguments
/// (`"C:\a b\x.exe" /S` → `C:\a b\x.exe`, `/S`).
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The `DisplayIcon` value as a path (`"C:\x\app.exe",0` → `C:\x\app.exe`).
pub fn icon_path(icon: &str) -> PathBuf {
    let s = icon.trim();
    let s = match s.rsplit_once(',') {
        Some((p, n)) if n.trim().parse::<i32>().is_ok() => p,
        _ => s,
    };
    PathBuf::from(s.trim().trim_matches('"'))
}

/// Uninstall entries matching `keep` (per-user first).
#[cfg(windows)]
pub fn uninstall_entries(keep: impl Fn(&UninstallEntry) -> bool) -> Vec<UninstallEntry> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    const PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    let mut out = Vec::new();
    for (hive, per_user) in [(HKEY_CURRENT_USER, true), (HKEY_LOCAL_MACHINE, false)] {
        let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(PATH, KEY_READ) else {
            continue;
        };
        for name in root.enum_keys().flatten() {
            let Ok(k) = root.open_subkey_with_flags(&name, KEY_READ) else {
                continue;
            };
            let get = |v: &str| {
                k.get_value::<String, _>(v)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            };
            let e = UninstallEntry {
                key: name.clone(),
                per_user,
                name: get("DisplayName").unwrap_or_default(),
                version: get("DisplayVersion"),
                location: get("InstallLocation").map(PathBuf::from),
                icon: get("DisplayIcon"),
                uninstall: get("UninstallString"),
                quiet_uninstall: get("QuietUninstallString"),
            };
            if keep(&e) {
                out.push(e);
            }
        }
    }
    out
}

/// Uninstall entries matching `keep` (none off Windows).
#[cfg(not(windows))]
pub fn uninstall_entries(keep: impl Fn(&UninstallEntry) -> bool) -> Vec<UninstallEntry> {
    let _ = keep;
    Vec::new()
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The launch-at-login command `name` of this user.
#[cfg(windows)]
pub fn run_value(name: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN, KEY_READ)
        .ok()?
        .get_value::<String, _>(name)
        .ok()
}

/// The launch-at-login command `name` (none off Windows).
#[cfg(not(windows))]
pub fn run_value(name: &str) -> Option<String> {
    let _ = (name, RUN);
    None
}

/// Remove the launch-at-login value `name`; `true` when it was there.
#[cfg(windows)]
pub fn remove_run_value(name: &str) -> superba_core::Result<bool> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| superba_core::Error::io(format!(r"HKCU\{RUN}"), &e))?;
    match key.delete_value(name) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(superba_core::Error::io(format!(r"HKCU\{RUN}\{name}"), &e)),
    }
}

/// Remove the launch-at-login value `name` (nothing off Windows).
#[cfg(not(windows))]
pub fn remove_run_value(name: &str) -> superba_core::Result<bool> {
    let _ = name;
    Ok(false)
}
