//! WSL distributions as managed hosts (user bug 45).
//!
//! A distribution is reached with `wsl.exe -d <distro> --cd ~ --exec /bin/sh -s`
//! (or `sh -c <payload bootstrap>` for streamed packages): the same POSIX
//! scripts as an SSH host run inside it as the distribution's default user, with
//! the script on stdin and nothing secret on a command line. `wsl.exe` is
//! started through `superba_core::process` (no console window), with
//! `WSL_UTF8=1`; its own messages are decoded with
//! [`superba_core::wsl::decode_output`] (UTF-16LE on older WSL).
//!
//! [`SystemWsl`] lists distributions (`wsl.exe --list --verbose`) and reads the
//! networking mode from `%USERPROFILE%\.wslconfig`; outside Windows it lists
//! nothing. Tests inject a [`WslLister`] and run the scripts with
//! [`crate::LocalShell`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use superba_core::wsl::{ENV_WSL_UTF8, WSL_EXE, WslDistro, WslNetworking, decode_output};
use superba_core::{Error, HostKind, RemoteHost, Result};

use crate::ssh::{
    Input, PAYLOAD_BOOTSTRAP, RemoteShell, ScriptOutput, payload_prefix, run_with_input,
    run_with_stdin,
};

/// Longest wait for `wsl.exe --list` (a stopped WSL starts its service first).
pub const LIST_TIMEOUT: Duration = Duration::from_secs(20);

/// What [`WslLister::list`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WslInventory {
    /// Distributions (the default first).
    pub distros: Vec<WslDistro>,
    /// `networkingMode` of `%USERPROFILE%\.wslconfig` (applies to WSL 2).
    pub networking: WslNetworking,
    /// Why nothing could be listed (WSL missing, `wsl.exe` failed); `None` when
    /// the list is complete (possibly empty).
    pub error: Option<String>,
}

/// Lists the WSL distributions of this computer.
#[async_trait]
pub trait WslLister: Send + Sync + fmt::Debug {
    /// The current inventory. Never fails: problems go into
    /// [`WslInventory::error`]. Starts no process when Windows' registry can
    /// be read (the GUI lists hosts on a timer).
    async fn list(&self) -> WslInventory;

    /// The distributions running now (`wsl.exe --list --running --quiet`,
    /// which never starts one; user bug 54); `None` when WSL cannot tell.
    async fn running(&self) -> Option<Vec<String>> {
        None
    }

    /// Forget a cached inventory (an explicit refresh).
    fn forget(&self) {}
}

/// A lister that always answers the same (tests, other platforms): the
/// distributions whose state is [`superba_core::wsl::STATE_RUNNING`] run;
/// unknown when no distribution has a state.
#[derive(Clone, Debug, Default)]
pub struct FixedWsl(pub WslInventory);

#[async_trait]
impl WslLister for FixedWsl {
    async fn list(&self) -> WslInventory {
        self.0.clone()
    }

    async fn running(&self) -> Option<Vec<String>> {
        let distros = &self.0.distros;
        distros.iter().any(|d| d.running().is_some()).then(|| {
            distros
                .iter()
                .filter(|d| d.running() == Some(true))
                .map(|d| d.name.clone())
                .collect()
        })
    }
}

/// How long [`SystemWsl::list`] answers from its last reading (user bug 54:
/// the overview and the host list ask on every poll).
pub const INVENTORY_TTL: Duration = Duration::from_secs(30);

/// The `wsl.exe` to run: `%SystemRoot%\System32\wsl.exe` when present (GUI
/// processes may have an unusual `PATH`), else `wsl.exe` from `PATH`.
pub fn default_wsl_program() -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let p = PathBuf::from(root).join("System32").join(WSL_EXE);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(WSL_EXE)
}

/// [`WslLister`] over the system `wsl.exe` (Windows only; elsewhere empty).
/// The inventory is read again after [`INVENTORY_TTL`] or [`WslLister::forget`].
#[derive(Debug)]
pub struct SystemWsl {
    program: PathBuf,
    /// `%USERPROFILE%` (for `.wslconfig`); `None` = from the environment.
    profile_dir: Option<PathBuf>,
    /// The last inventory and when it was read.
    cached: std::sync::Mutex<Option<(std::time::Instant, WslInventory)>>,
}

impl Default for SystemWsl {
    fn default() -> Self {
        Self {
            program: default_wsl_program(),
            profile_dir: None,
            cached: std::sync::Mutex::new(None),
        }
    }
}

impl SystemWsl {
    /// Read `.wslconfig` from `dir` instead of `%USERPROFILE%`.
    pub fn with_profile_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.profile_dir = Some(dir.into());
        self
    }

    fn wslconfig(&self) -> Option<PathBuf> {
        let dir = self
            .profile_dir
            .clone()
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
        Some(dir.join(".wslconfig"))
    }
}

/// The networking mode configured in the `.wslconfig` at `path` (NAT when the
/// file is missing or says nothing).
pub fn read_networking(path: Option<&Path>) -> WslNetworking {
    path.and_then(|p| std::fs::read(p).ok())
        .map(|b| superba_core::wsl::parse_wslconfig_networking(&String::from_utf8_lossy(&b)))
        .unwrap_or_default()
}

/// Registry key where WSL keeps the user's distributions (one `{GUID}` subkey
/// each with `DistributionName`, `State` and `Flags`; `DefaultDistribution`
/// names the default's GUID). The WSL version is bit 0x8 of `Flags`
/// (`LXSS_DISTRO_FLAGS_VM_MODE`): `Version` is the registration format (2 for
/// every modern distribution, WSL 1 ones too), observed in the test VM.
pub const LXSS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// One distribution as the registry records it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LxssEntry {
    /// Subkey name (`{GUID}`).
    pub guid: String,
    /// `DistributionName`.
    pub name: String,
    /// `Flags` (0x8 = runs in the WSL 2 VM).
    pub flags: Option<u32>,
    /// `State` (1 = installed; 2 and 3 are being installed / removed).
    pub state: Option<u32>,
}

/// `Flags` bit of a distribution that runs in the WSL 2 VM.
pub const FLAG_VM_MODE: u32 = 0x8;

/// Distributions from registry entries (pure): installed ones only, utility
/// distributions left out, the default (by `default_guid`) first.
pub fn distros_from_lxss(entries: &[LxssEntry], default_guid: Option<&str>) -> Vec<WslDistro> {
    let mut out: Vec<WslDistro> = Vec::new();
    for e in entries {
        let name = e.name.trim();
        if name.is_empty()
            || superba_core::wsl::is_utility_distro(name)
            || e.state.is_some_and(|s| s != 1)
            || out.iter().any(|d| d.name == name)
        {
            continue;
        }
        out.push(WslDistro {
            name: name.to_owned(),
            version: e.flags.map(|f| if f & FLAG_VM_MODE != 0 { 2 } else { 1 }),
            default: default_guid.is_some_and(|g| g.eq_ignore_ascii_case(&e.guid)),
            state: String::new(),
            networking: WslNetworking::Nat,
        });
    }
    out.sort_by_key(|d| !d.default);
    out
}

/// Read the user's WSL registry entries (Windows only).
#[cfg(windows)]
fn read_lxss() -> std::io::Result<(Vec<LxssEntry>, Option<String>)> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    let lxss = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(LXSS_KEY, KEY_READ)?;
    let default: Option<String> = lxss.get_value("DefaultDistribution").ok();
    let mut entries = Vec::new();
    for guid in lxss.enum_keys().flatten() {
        let Ok(key) = lxss.open_subkey_with_flags(&guid, KEY_READ) else {
            continue;
        };
        let Ok(name) = key.get_value::<String, _>("DistributionName") else {
            continue;
        };
        entries.push(LxssEntry {
            guid,
            name,
            flags: key.get_value::<u32, _>("Flags").ok(),
            state: key.get_value::<u32, _>("State").ok(),
        });
    }
    Ok((entries, default))
}

#[async_trait]
impl WslLister for SystemWsl {
    async fn list(&self) -> WslInventory {
        if !cfg!(windows) {
            return WslInventory::default();
        }
        if let Some((at, inventory)) = self
            .cached
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && at.elapsed() < INVENTORY_TTL
        {
            return inventory.clone();
        }
        let inventory = self.read().await;
        if inventory.error.is_none() {
            *self.cached.lock().unwrap_or_else(|p| p.into_inner()) =
                Some((std::time::Instant::now(), inventory.clone()));
        }
        inventory
    }

    async fn running(&self) -> Option<Vec<String>> {
        if !cfg!(windows) {
            return None;
        }
        let mut cmd = tokio::process::Command::from(superba_core::process::command(&self.program));
        cmd.args(["--list", "--running", "--quiet"])
            .env(ENV_WSL_UTF8, "1");
        let out = run_with_stdin(cmd, b"", LIST_TIMEOUT, "wsl.exe")
            .await
            .ok()?;
        let names = superba_core::wsl::parse_list_quiet(&decode_output(&out.stdout));
        // WSL 3 prints nothing (exit 0) when none runs; older versions print a
        // localized "no running distributions" line and exit non-zero.
        (out.status == Some(0) || !out.stdout.is_empty()).then_some(names)
    }

    fn forget(&self) {
        *self.cached.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

impl SystemWsl {
    /// The registry, else `wsl.exe --list --verbose`.
    async fn read(&self) -> WslInventory {
        let networking = read_networking(self.wslconfig().as_deref());
        // The registry first: listing hosts must not start a process (the GUI
        // lists hosts on a timer).
        #[cfg(windows)]
        match tokio::task::spawn_blocking(read_lxss).await {
            Ok(Ok((entries, default))) => {
                return WslInventory {
                    distros: with_networking(
                        distros_from_lxss(&entries, default.as_deref()),
                        networking,
                    ),
                    networking,
                    error: None,
                };
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return WslInventory {
                    networking,
                    ..WslInventory::default()
                };
            }
            _ => {}
        }
        self.list_with_wsl_exe(networking).await
    }
}

impl SystemWsl {
    /// `wsl.exe --list --verbose` (the fallback when the registry cannot be read).
    pub async fn list_with_wsl_exe(&self, networking: WslNetworking) -> WslInventory {
        let mut cmd = tokio::process::Command::from(superba_core::process::command(&self.program));
        cmd.args(["--list", "--verbose"]).env(ENV_WSL_UTF8, "1");
        let out = match run_with_stdin(cmd, b"", LIST_TIMEOUT, "wsl.exe").await {
            Ok(out) => out,
            Err(e) => {
                return WslInventory {
                    networking,
                    error: Some(e.to_string()),
                    ..WslInventory::default()
                };
            }
        };
        let text = decode_output(&out.stdout);
        if out.status == Some(0) {
            return WslInventory {
                distros: with_networking(superba_core::wsl::parse_list_verbose(&text), networking),
                networking,
                error: None,
            };
        }
        // No distribution installed, WSL not installed or the service failed:
        // wsl.exe says so (localized) and exits non-zero.
        let message = first_line(&text)
            .or_else(|| first_line(&decode_output(out.stderr.as_bytes())))
            .unwrap_or_else(|| format!("wsl.exe --list failed ({:?})", out.status));
        WslInventory {
            distros: Vec::new(),
            networking,
            error: Some(message),
        }
    }
}

/// Give every distribution the configured networking mode.
pub fn with_networking(mut distros: Vec<WslDistro>, networking: WslNetworking) -> Vec<WslDistro> {
    for d in &mut distros {
        d.networking = networking;
    }
    distros
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(300).collect())
}

/// The distribution of a WSL host, refusing anything else.
pub fn require_distro(host: &RemoteHost) -> Result<String> {
    if host.kind != HostKind::Wsl {
        return Err(Error::invalid(format!(
            "{} is not a WSL distribution",
            host.title()
        )));
    }
    let distro = host
        .wsl_distro()
        .ok_or_else(|| Error::invalid("WSL host without a distribution name"))?;
    if distro.is_empty()
        || distro.starts_with('-')
        || !distro
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(Error::invalid(format!(
            "unsafe WSL distribution {distro:?}"
        )));
    }
    Ok(distro)
}

/// `wsl.exe` arguments running `remote` inside `distro` in the user's home.
pub fn wsl_args(distro: &str, remote: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-d".into(),
        distro.into(),
        "--cd".into(),
        "~".into(),
        "--exec".into(),
    ];
    args.extend(remote.iter().map(|w| (*w).to_owned()));
    args
}

/// [`RemoteShell`] over `wsl.exe` for [`HostKind::Wsl`] hosts.
#[derive(Clone, Debug)]
pub struct WslShell {
    program: PathBuf,
}

impl Default for WslShell {
    fn default() -> Self {
        Self {
            program: default_wsl_program(),
        }
    }
}

impl WslShell {
    /// Use a specific `wsl.exe`.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn command(&self, args: &[String]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::from(superba_core::process::command(&self.program));
        cmd.args(args).env(ENV_WSL_UTF8, "1");
        cmd
    }
}

/// The error for a run whose script produced no records: `wsl.exe` itself
/// failed (no such distribution, WSL not installed, the VM did not start).
fn wsl_failure(distro: &str, out: &ScriptOutput) -> Option<Error> {
    if out.status == Some(0) || out.stdout.windows(5).any(|w| w == b"@@SB:") {
        return None;
    }
    let message = first_line(&decode_output(out.stderr.as_bytes()))
        .or_else(|| first_line(&decode_output(&out.stdout)))
        .unwrap_or_else(|| format!("wsl.exe exited with {:?}", out.status));
    Some(Error::Wsl {
        distro: distro.to_owned(),
        message,
    })
}

#[async_trait]
impl RemoteShell for WslShell {
    async fn run(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let distro = require_distro(host)?;
        let cmd = self.command(&wsl_args(&distro, &["/bin/sh", "-s"]));
        let out = run_with_stdin(cmd, script.as_bytes(), timeout, &distro).await?;
        match wsl_failure(&distro, &out) {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    async fn run_streaming(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
        sink: crate::ssh::LineSink,
    ) -> Result<ScriptOutput> {
        let distro = require_distro(host)?;
        let cmd = self.command(&wsl_args(&distro, &["/bin/sh", "-s"]));
        let input = Input::new(script.as_bytes().to_vec());
        let out =
            crate::ssh::run_with_input_streaming(cmd, input, timeout, &distro, Some(sink)).await?;
        match wsl_failure(&distro, &out) {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    async fn run_with_payload(
        &self,
        host: &RemoteHost,
        script: &str,
        payload: &Path,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let distro = require_distro(host)?;
        let cmd = self.command(&wsl_args(&distro, &["/bin/sh", "-c", PAYLOAD_BOOTSTRAP]));
        let input = Input::with_file(payload_prefix(script), payload.to_path_buf());
        let out = run_with_input(cmd, input, timeout, &distro).await?;
        match wsl_failure(&distro, &out) {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }
}

/// Sends each host's scripts through its kind's transport: SSH hosts through
/// `ssh`, WSL distributions through `wsl.exe`.
#[derive(Clone, Debug)]
pub struct HostShells {
    /// SSH hosts.
    pub ssh: std::sync::Arc<dyn RemoteShell>,
    /// WSL distributions.
    pub wsl: std::sync::Arc<dyn RemoteShell>,
}

impl HostShells {
    /// The transport for `host`.
    pub fn for_host(&self, host: &RemoteHost) -> &std::sync::Arc<dyn RemoteShell> {
        if host.kind == HostKind::Wsl {
            &self.wsl
        } else {
            &self.ssh
        }
    }
}

/// Bytes of a response [`http_get_script`] keeps at most.
pub const HTTP_GET_MAX: usize = 4096;

/// A script that sends `GET <path>` to `<address>:<port>` from inside the host
/// with bash's `/dev/tcp` (no curl needed; bash is what the app requires) and
/// reports the first [`HTTP_GET_MAX`] bytes of the answer (`F answer`), or
/// `K connect 0` when nothing accepted the connection within 5 s (`timeout`
/// when available, else a watchdog). `K nobash 1` when there is no bash.
/// `address` must be an IPv4 literal and `path` an absolute path of safe
/// characters ([`crate::script::sh_quote`]d either way).
pub fn http_get_script(
    nonce: &str,
    address: std::net::Ipv4Addr,
    port: u16,
    path: &str,
) -> Result<String> {
    if !path.starts_with('/')
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.'))
    {
        return Err(Error::invalid(format!("unsafe request path {path:?}")));
    }
    let q = crate::script::sh_quote;
    let body = format!(
        r#"sb_a={a}; sb_p={p}; sb_path={path}
sb_bash=
for sb_c in /usr/bin/bash /bin/bash; do if [ -x "$sb_c" ]; then sb_bash=$sb_c; break; fi; done
if [ -z "$sb_bash" ]; then sb_put nobash 1; sb_put done 1; return 0; fi
SB_GET='exec 3<>"/dev/tcp/$1/$2" || exit 9; printf "GET %s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n" "$3" "$1" >&3; head -c {max} <&3'
sb_tmp=$(mktemp 2>/dev/null || echo "/tmp/superba-get.$$")
if command -v timeout >/dev/null 2>&1; then
  timeout 5 "$sb_bash" -c "$SB_GET" sb "$sb_a" "$sb_p" "$sb_path" > "$sb_tmp" 2>/dev/null
  sb_rc=$?
else
  "$sb_bash" -c "$SB_GET" sb "$sb_a" "$sb_p" "$sb_path" > "$sb_tmp" 2>/dev/null &
  sb_pid=$!
  ( sleep 5; kill "$sb_pid" 2>/dev/null ) &
  sb_w=$!
  wait "$sb_pid"; sb_rc=$?
  kill "$sb_w" 2>/dev/null
fi
if [ -s "$sb_tmp" ]; then sb_put connect 1; sb_file answer "$sb_tmp"; else sb_put connect 0; sb_put rc "$sb_rc"; fi
rm -f "$sb_tmp"
sb_put done 1"#,
        a = q(&address.to_string()),
        p = q(&port.to_string()),
        path = q(path),
        max = HTTP_GET_MAX,
    );
    Ok(crate::script::wrap_public(nonce, &body))
}
