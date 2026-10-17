//! Non-file environment stores: macOS `launchctl` (current login session) and the
//! Windows user environment (`HKCU\Environment` + `WM_SETTINGCHANGE`).
//!
//! The [`NativeEnv`] trait keeps these side effects injectable: tests use
//! [`MemoryNativeEnv`]; production uses [`SystemNativeEnv`], whose platform code is
//! compiled only on its platform and kept deliberately small because it cannot be
//! exercised on Linux CI.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use superba_core::{EnvLocation, Error, Result};

/// A platform environment store.
pub trait NativeEnv: Send + Sync + fmt::Debug {
    /// Current value of `var` at `location` (`None` = unset).
    fn get(&self, location: &EnvLocation, var: &str) -> Result<Option<String>>;
    /// Set `var`.
    fn set(&self, location: &EnvLocation, var: &str, value: &str) -> Result<()>;
    /// Unset `var` (no error when unset).
    fn delete(&self, location: &EnvLocation, var: &str) -> Result<()>;
}

/// Stable string key of a location (env-state bookkeeping, memory store).
pub(crate) fn location_key(location: &EnvLocation) -> String {
    match location {
        EnvLocation::ShellRc { path, .. }
        | EnvLocation::EnvironmentD { path }
        | EnvLocation::LaunchAgent { path } => path.to_string_lossy().into_owned(),
        EnvLocation::Launchctl => "launchctl".into(),
        EnvLocation::WindowsUserEnv => "hkcu_environment".into(),
        other => format!("{other:?}"),
    }
}

/// In-memory [`NativeEnv`] for tests (and dry runs).
#[derive(Debug, Default)]
pub struct MemoryNativeEnv {
    values: Mutex<BTreeMap<(String, String), String>>,
}

impl MemoryNativeEnv {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-set a value (e.g. a legacy `setx` value).
    pub fn with(self, location: &EnvLocation, var: &str, value: &str) -> Self {
        self.values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((location_key(location), var.to_owned()), value.to_owned());
        self
    }
}

impl NativeEnv for MemoryNativeEnv {
    fn get(&self, location: &EnvLocation, var: &str) -> Result<Option<String>> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(location_key(location), var.to_owned()))
            .cloned())
    }

    fn set(&self, location: &EnvLocation, var: &str, value: &str) -> Result<()> {
        self.values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((location_key(location), var.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, location: &EnvLocation, var: &str) -> Result<()> {
        self.values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(location_key(location), var.to_owned()));
        Ok(())
    }
}

/// The real platform stores.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemNativeEnv;

fn unsupported(location: &EnvLocation) -> Error {
    Error::Unsupported {
        what: format!("environment location {location:?} on this platform"),
    }
}

impl NativeEnv for SystemNativeEnv {
    fn get(&self, location: &EnvLocation, var: &str) -> Result<Option<String>> {
        #[cfg(target_os = "macos")]
        if matches!(location, EnvLocation::Launchctl) {
            return macos::getenv(var);
        }
        #[cfg(windows)]
        if matches!(location, EnvLocation::WindowsUserEnv) {
            return windows::get(var);
        }
        let _ = var;
        Err(unsupported(location))
    }

    fn set(&self, location: &EnvLocation, var: &str, value: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        if matches!(location, EnvLocation::Launchctl) {
            return macos::setenv(var, value);
        }
        #[cfg(windows)]
        if matches!(location, EnvLocation::WindowsUserEnv) {
            return windows::set(var, value);
        }
        let _ = (var, value);
        Err(unsupported(location))
    }

    fn delete(&self, location: &EnvLocation, var: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        if matches!(location, EnvLocation::Launchctl) {
            return macos::unsetenv(var);
        }
        #[cfg(windows)]
        if matches!(location, EnvLocation::WindowsUserEnv) {
            return windows::delete(var);
        }
        let _ = var;
        Err(unsupported(location))
    }
}

/// macOS: `launchctl getenv/setenv/unsetenv` affect apps launched by launchd from
/// now on (Dock, Finder, `open`) for the current login session. The LaunchAgent
/// written next to it re-applies the value at every login.
#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    fn launchctl(args: &[&str]) -> Result<String> {
        let out = superba_core::process::command("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|e| Error::Process {
                message: format!("launchctl {}: {e}", args.first().unwrap_or(&"")),
            })?;
        if !out.status.success() {
            return Err(Error::Process {
                message: format!(
                    "launchctl {} failed: {}",
                    args.first().unwrap_or(&""),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    pub(super) fn getenv(var: &str) -> Result<Option<String>> {
        let v = launchctl(&["getenv", var])?;
        Ok((!v.is_empty()).then_some(v))
    }

    pub(super) fn setenv(var: &str, value: &str) -> Result<()> {
        launchctl(&["setenv", var, value]).map(|_| ())
    }

    pub(super) fn unsetenv(var: &str) -> Result<()> {
        launchctl(&["unsetenv", var]).map(|_| ())
    }
}

/// Windows: the per-user environment lives in `HKCU\Environment` (this is also
/// what `setx` from the legacy tutorial wrote). After a change Explorer must be
/// told with a `WM_SETTINGCHANGE("Environment")` broadcast; Windows Terminal and
/// VS Code ignore it and need a restart (the UI says so). The ChatGPT app (an
/// MSIX package activated by Superba or the Start menu) reads the values from
/// the registry at each activation, broadcast or not (checked on a Windows
/// runner); a running app keeps the environment it started with.
#[cfg(windows)]
mod windows {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

    use super::*;

    const SUBKEY: &str = "Environment";

    fn io(e: std::io::Error) -> Error {
        Error::io(r"HKCU\Environment", &e)
    }

    pub(super) fn get(var: &str) -> Result<Option<String>> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(SUBKEY, KEY_READ)
            .map_err(io)?;
        match key.get_value::<String, _>(var) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    pub(super) fn set(var: &str, value: &str) -> Result<()> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(SUBKEY, KEY_READ | KEY_SET_VALUE)
            .map_err(io)?;
        key.set_value(var, &value.to_owned()).map_err(io)?;
        broadcast();
        Ok(())
    }

    pub(super) fn delete(var: &str) -> Result<()> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(SUBKEY, KEY_READ | KEY_SET_VALUE)
            .map_err(io)?;
        match key.delete_value(var) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        broadcast();
        Ok(())
    }

    /// Tell top-level windows (Explorer) that the environment changed. Best effort
    /// with a 5 s timeout per window so a hung app cannot block Superba.
    fn broadcast() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        let name: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result: usize = 0;
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call;
        // `result` is a valid out pointer; HWND_BROADCAST is a documented sentinel.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                name.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }
}
