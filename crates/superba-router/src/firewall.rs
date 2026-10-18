//! The Windows Firewall rule that lets WSL 2 distributions behind NAT reach the
//! router's WSL listener (user bug 45).
//!
//! Windows Defender Firewall drops inbound connections from the WSL VM to a
//! program that has no allow rule. Creating a rule needs an administrator, so
//! Superba asks once, when the user turns on router access for WSL: `netsh
//! advfirewall firewall add rule` (or `set rule … new` when Superba's rule
//! exists) runs through `ShellExecuteExW` with the `runas` verb (the UAC
//! prompt). The rule is as narrow as netsh allows: inbound TCP, the router's
//! port, the WSL adapter address only (`localip`), peers on that adapter's
//! subnet only (`remoteip=LocalSubnet`) and the router program only.

use std::net::Ipv4Addr;
use std::path::Path;

use superba_core::{Error, Result};

/// Name of Superba's rule.
pub const WSL_RULE_NAME: &str = "Superba router (WSL)";

/// Quote a netsh argument value (`name="…"`); netsh has no escape for `"`.
fn quoted(value: &str) -> Result<String> {
    if value.contains('"') {
        return Err(Error::invalid(format!(
            "{value:?} cannot be passed to netsh (contains a quote)"
        )));
    }
    Ok(format!("\"{value}\""))
}

/// The `netsh` arguments that create (`exists = false`) or update Superba's
/// rule for `address:port` and the router program `exe` (pure).
pub fn rule_args(exists: bool, address: Ipv4Addr, port: u16, exe: &Path) -> Result<String> {
    let name = quoted(WSL_RULE_NAME)?;
    let program = quoted(&exe.to_string_lossy())?;
    let fields = format!(
        "dir=in action=allow enable=yes profile=any protocol=TCP localport={port} \
         localip={address} remoteip=LocalSubnet program={program}"
    );
    Ok(if exists {
        format!("advfirewall firewall set rule name={name} new {fields}")
    } else {
        format!("advfirewall firewall add rule name={name} {fields}")
    })
}

/// What [`allow_wsl`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirewallChange {
    /// Superba's rule was created.
    Added,
    /// Superba's rule existed and was updated.
    Updated,
}

/// Create or update Superba's rule (Windows; asks for administrator approval).
/// [`Error::Cancelled`] when the user declines the prompt.
#[cfg(windows)]
pub async fn allow_wsl(address: Ipv4Addr, port: u16, exe: &Path) -> Result<FirewallChange> {
    let exists = rule_exists().await;
    let args = rule_args(exists, address, port, exe)?;
    let code = tokio::task::spawn_blocking(move || elevated::run_netsh(&args))
        .await
        .map_err(|e| Error::other(format!("firewall task failed: {e}")))??;
    if code != 0 {
        return Err(Error::Process {
            message: format!("netsh could not change the firewall rule (exit code {code})"),
        });
    }
    Ok(if exists {
        FirewallChange::Updated
    } else {
        FirewallChange::Added
    })
}

/// Not available outside Windows.
#[cfg(not(windows))]
pub async fn allow_wsl(address: Ipv4Addr, port: u16, exe: &Path) -> Result<FirewallChange> {
    let _ = (address, port, exe);
    Err(Error::Unsupported {
        what: "Windows Firewall rules on this platform".into(),
    })
}

/// `netsh advfirewall firewall show rule name=<Superba's rule>` succeeds (no
/// elevation needed to read the policy).
#[cfg(windows)]
async fn rule_exists() -> bool {
    let mut cmd = tokio::process::Command::from(superba_core::process::command(netsh()));
    cmd.args(["advfirewall", "firewall", "show", "rule"])
        .arg(format!("name={WSL_RULE_NAME}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(15), cmd.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

#[cfg(windows)]
fn netsh() -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(|r| {
            std::path::PathBuf::from(r)
                .join("System32")
                .join("netsh.exe")
        })
        .filter(|p| p.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("netsh.exe"))
}

#[cfg(windows)]
mod elevated {
    use std::os::windows::ffi::OsStrExt;

    use superba_core::{Error, Result};
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Run `netsh <args>` elevated (UAC prompt) and wait up to two minutes for
    /// its exit code.
    pub(super) fn run_netsh(args: &str) -> Result<u32> {
        let verb = wide("runas");
        let file = wide(super::netsh());
        let params = wide(args);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: verb.as_ptr(),
            lpFile: file.as_ptr(),
            lpParameters: params.as_ptr(),
            nShow: SW_HIDE,
            ..Default::default()
        };
        // SAFETY: every string outlives the call; `info` is a valid, initialized
        // SHELLEXECUTEINFOW.
        let ok = unsafe { ShellExecuteExW(&raw mut info) };
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                return Err(Error::Cancelled);
            }
            return Err(Error::Process {
                message: format!("cannot ask for administrator approval: {e}"),
            });
        }
        let process = info.hProcess;
        if process.is_null() {
            return Err(Error::Process {
                message: "netsh started without a process handle".into(),
            });
        }
        // SAFETY: `process` is the handle ShellExecuteExW returned
        // (SEE_MASK_NOCLOSEPROCESS); it is closed exactly once below.
        unsafe {
            let waited = WaitForSingleObject(process, 120_000);
            let mut code = 1_u32;
            let got = GetExitCodeProcess(process, &raw mut code);
            CloseHandle(process);
            if waited != WAIT_OBJECT_0 || got == 0 {
                return Err(Error::Timeout {
                    what: "netsh (firewall rule)".into(),
                });
            }
            Ok(code)
        }
    }
}
