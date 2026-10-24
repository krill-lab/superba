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
    run_with_input_streaming, run_with_stdin,
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
    ready: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, ReadyState>>>,
    warmup_timeout: Duration,
    /// Limit of the `login` stage of a first attempt (a second attempt may use
    /// what is left of the operation's budget).
    login_timeout: Duration,
}

/// The first attempt's limit for reading the login environment.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);
/// Least time a write script gets, whatever earlier stages used of the budget.
const WRITE_MIN: Duration = Duration::from_secs(45);
/// Least time any other script gets.
const SCRIPT_MIN: Duration = Duration::from_secs(30);

impl Default for WslShell {
    fn default() -> Self {
        Self {
            program: default_wsl_program(),
            ready: Default::default(),
            warmup_timeout: WARMUP_TIMEOUT,
            login_timeout: LOGIN_TIMEOUT,
        }
    }
}

impl WslShell {
    /// Use a specific `wsl.exe`.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            ready: Default::default(),
            warmup_timeout: WARMUP_TIMEOUT,
            login_timeout: LOGIN_TIMEOUT,
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

// A cold WSL1 under load can spend most of the old 45 s before executing any
// script. Readiness has its own allowance; every stage and retry shares a hard
// deadline, and only a read-only probe is safe to replay.
const WARMUP_TIMEOUT: Duration = Duration::from_secs(90);
const READY_TTL: Duration = Duration::from_secs(300);
type ReadyState = std::sync::Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>;

/// What a timed-out WSL stage was doing, in words, with what to do about it
/// (alpha.10, W90: "login (attempt 2/2, 60005 ms; total limit 180 s)" named
/// neither the files involved nor a way out). Starts with `WSL <distro>: <stage>`.
fn stage_timeout(
    distro: &str,
    stage: &str,
    attempt: u8,
    elapsed: Duration,
    allowed: Duration,
) -> String {
    let secs = format!("{:.1}", elapsed.as_secs_f32());
    let allowed = format!("{:.1}", allowed.as_secs_f32());
    let (doing, hint) = match stage {
        "warmup" => (
            "starting the distribution",
            "WSL may still be starting or updating; open it once in a terminal, then Retry.",
        ),
        "login" => (
            "reading the login environment (bash -l with ~/.profile, then an interactive bash for the PATH from ~/.bashrc)",
            "A command in ~/.profile or ~/.bashrc may wait for input, the network or a Windows program; check them, then Retry, or apply it later with `spb wsl update`.",
        ),
        "version" => (
            "running codex --version",
            "Check that `codex --version` answers in that distribution, then Retry.",
        ),
        "files" => (
            "reading the Codex files",
            "Retry; the distribution may be busy.",
        ),
        "system" => (
            "checking the system",
            "Retry; the distribution may be busy.",
        ),
        "write" => (
            "writing the configuration",
            "Nothing was replayed; check ~/.codex there, then Retry.",
        ),
        _ => ("running a command", "Retry; the distribution may be busy."),
    };
    format!(
        "WSL {distro}: {stage} (attempt {attempt}/2) did not finish: {secs} s of the {allowed} s it had, {doing}. {hint}"
    )
}

impl WslShell {
    async fn ready(
        &self,
        host: &RemoteHost,
        distro: &str,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        let events = crate::operation::events();
        // Serialize initial contact (the GUI may probe a host in the background
        // at the same time as a switch). Waiting for the lock is cancellable too.
        let state = self
            .ready
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(distro.to_owned())
            .or_default()
            .clone();
        crate::operation::report(events.as_ref(), host, Some("warmup"), 1, true);
        let mut ready = self
            .watch(
                host,
                "warmup",
                1,
                true,
                deadline,
                self.warmup_timeout,
                state.lock(),
                None,
            )
            .await?;
        if ready.as_ref().is_some_and(|at| at.elapsed() < READY_TTL) {
            return Ok(());
        }
        for attempt in 1..=2 {
            crate::operation::report(events.as_ref(), host, Some("warmup"), attempt, true);
            let cmd = self.command(&wsl_args(distro, &["/bin/true"]));
            let run = run_with_stdin(
                cmd,
                b"",
                self.warmup_timeout + Duration::from_secs(1),
                distro,
            );
            let result = self
                .watch(
                    host,
                    "warmup",
                    attempt,
                    true,
                    deadline,
                    self.warmup_timeout,
                    run,
                    None,
                )
                .await
                .and_then(|r| r);
            match result {
                Ok(out) => {
                    if let Some(e) = wsl_failure(distro, &out) {
                        return Err(e);
                    }
                    *ready = Some(tokio::time::Instant::now());
                    return Ok(());
                }
                Err(Error::Timeout { .. })
                    if attempt == 1 && tokio::time::Instant::now() < deadline => {}
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }

    #[allow(clippy::too_many_arguments)]
    async fn watch<T>(
        &self,
        host: &RemoteHost,
        initial: &str,
        attempt: u8,
        cancellable: bool,
        deadline: tokio::time::Instant,
        limit: Duration,
        future: impl std::future::Future<Output = T>,
        mut phases: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    ) -> Result<T> {
        let events = crate::operation::events();
        let mut stage = initial.to_owned();
        let mut since = tokio::time::Instant::now();
        let mut due = deadline.min(since + limit);
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                _ = poll.tick() => {
                    if cancellable && events.as_ref().is_some_and(|e| e.stop_requested()) {
                        return Err(Error::Cancelled);
                    }
                }
                _ = tokio::time::sleep_until(due) => {
                    tracing::warn!(host = %host.target_id, %stage, attempt, elapsed_ms = since.elapsed().as_millis() as u64, "WSL stage timed out");
                    return Err(Error::Timeout { what: stage_timeout(&host.wsl_distro().unwrap_or_default(), &stage, attempt, since.elapsed(), due.saturating_duration_since(since)) });
                }
                next = async { match phases.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending().await } } => {
                    if let Some(next) = next {
                        tracing::info!(host = %host.target_id, %stage, attempt, elapsed_ms = since.elapsed().as_millis() as u64, "WSL stage finished");
                        stage = next;
                        since = tokio::time::Instant::now();
                        // A second attempt of a slow login (a cold distribution, an
                        // rc file that runs Windows programs) gets what is left of
                        // the budget instead of failing again at the same mark.
                        let stage_limit = match (stage.as_str(), attempt) {
                            ("login", 1) => self.login_timeout,
                            ("login", _) => limit.min(self.login_timeout * 3 / 2),
                            _ => limit,
                        };
                        due = deadline.min(since + stage_limit);
                        crate::operation::report(events.as_ref(), host, Some(&stage), attempt, cancellable);
                    } else { phases = None; }
                }
                result = &mut future => {
                    tracing::info!(host = %host.target_id, %stage, attempt, elapsed_ms = since.elapsed().as_millis() as u64, "WSL stage finished");
                    return Ok(result);
                }
            }
        }
    }

    async fn script(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
        sink: Option<crate::ssh::LineSink>,
    ) -> Result<ScriptOutput> {
        let distro = require_distro(host)?;
        let budget = crate::operation::budget(host, timeout.max(crate::operation::TOTAL_TIMEOUT));
        // Every script gets a guaranteed minimum on top of what earlier slow
        // stages left of the shared budget (alpha.10 re-test, W81: a write
        // started with 0 s left and failed before it ran). A write gets more:
        // it is never replayed, so it must not be cut short.
        let floor = if script.starts_with("# superba-write\n") {
            WRITE_MIN
        } else {
            SCRIPT_MIN
        };
        let deadline = budget
            .deadline
            .max(tokio::time::Instant::now() + floor.min(timeout.max(Duration::from_secs(1))));
        let events = crate::operation::events();
        let _finish = ProgressEnd {
            events: events.clone(),
            host: host.clone(),
        };
        self.ready(host, &distro, deadline).await?;
        let probe = script.starts_with("# superba-wsl-probe\n");
        let prefix = script
            .lines()
            .find_map(|l| l.strip_prefix("SB_N='").and_then(|s| s.strip_suffix('\'')))
            .map(|nonce| format!("@@SBPH:{nonce}:"));
        for attempt in 1..=2 {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let prefix = prefix.clone();
            let downstream = sink.clone();
            let lines: crate::ssh::LineSink = std::sync::Arc::new(move |line| {
                if let Some(stage) = prefix.as_ref().and_then(|p| line.strip_prefix(p))
                    && matches!(stage, "login" | "version" | "files" | "system")
                {
                    let _ = tx.send(stage.to_owned());
                }
                if let Some(sink) = &downstream {
                    sink(line);
                }
            });
            let stage = if script.starts_with("# superba-write\n") {
                "write"
            } else {
                "command"
            };
            crate::operation::report(events.as_ref(), host, Some(stage), attempt, probe);
            let cmd = self.command(&wsl_args(&distro, &["/bin/sh", "-s"]));
            let run = run_with_input_streaming(
                cmd,
                Input::new(script.as_bytes().to_vec()),
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                &distro,
                Some(lines),
            );
            // The transport's old whole-script timeout becomes a stage timeout.
            let result = self
                .watch(
                    host,
                    stage,
                    attempt,
                    probe,
                    deadline,
                    timeout,
                    run,
                    Some(rx),
                )
                .await
                .and_then(|r| r);
            match result {
                Ok(out) => {
                    return match wsl_failure(&distro, &out) {
                        Some(e) => Err(e),
                        None => Ok(out),
                    };
                }
                Err(Error::Timeout { .. })
                    if probe && attempt == 1 && tokio::time::Instant::now() < deadline =>
                {
                    self.ready
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&distro);
                    self.ready(host, &distro, deadline).await?;
                    tracing::info!(host = %host.target_id, "retrying read-only WSL probe after timeout");
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
}

struct ProgressEnd {
    events: Option<std::sync::Arc<dyn superba_core::SwitchEvents>>,
    host: RemoteHost,
}

impl Drop for ProgressEnd {
    fn drop(&mut self) {
        crate::operation::report(self.events.as_ref(), &self.host, None, 1, false);
    }
}

#[async_trait]
impl RemoteShell for WslShell {
    async fn run(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        self.script(host, script, timeout, None).await
    }

    async fn run_streaming(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
        sink: crate::ssh::LineSink,
    ) -> Result<ScriptOutput> {
        // Session migration is already bounded and cancellable by its helper;
        // do not replay a mutation or impose the switching budget on it.
        let distro = require_distro(host)?;
        let cmd = self.command(&wsl_args(&distro, &["/bin/sh", "-s"]));
        let out = run_with_input_streaming(
            cmd,
            Input::new(script.as_bytes().to_vec()),
            timeout,
            &distro,
            Some(sink),
        )
        .await?;
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

#[cfg(all(test, unix))]
mod timeout_tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use superba_core::{SwitchEvent, SwitchEvents};

    fn host() -> RemoteHost {
        RemoteHost {
            target_id: superba_core::wsl::target_id("Ubuntu"),
            label: "Ubuntu".into(),
            host_id: None,
            kind: HostKind::Wsl,
            ssh: Default::default(),
            sources: vec![],
            connected: None,
            wsl: Some(WslDistro::named("Ubuntu")),
        }
    }

    fn fake(body: &str) -> (tempfile::TempDir, WslShell) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wsl.exe");
        let staged = dir.path().join("staged");
        std::fs::write(
            &staged,
            format!(
                "#!/bin/sh\ncd {}\n{body}\n\
                 # Resolve Linux executable paths on the Unix test host.\n\
                 case \"$1\" in /bin/true|/bin/sh)\n\
                   program=$(command -v \"${{1##*/}}\") || exit 127\n\
                   shift; set -- \"$program\" \"$@\";;\n\
                 esac\n\
                 exec \"$@\"\n",
                crate::script::sh_quote(&dir.path().display().to_string())
            ),
        )
        .unwrap();
        assert!(
            superba_core::process::command("cp")
                .arg(staged)
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut shell = WslShell::with_program(path);
        // Allow slow CI hosts to launch the helper; stalled attempts sleep much longer.
        shell.warmup_timeout = Duration::from_secs(5);
        (dir, shell)
    }

    #[derive(Default)]
    struct Events {
        messages: Mutex<Vec<SwitchEvent>>,
        stop: AtomicBool,
        stop_at_files: bool,
    }
    impl SwitchEvents for Events {
        fn emit(&self, event: SwitchEvent) {
            if self.stop_at_files
                && matches!(&event, SwitchEvent::RemoteProgress { message: Some(m), .. } if m.key == "wsl.progress.files")
            {
                self.stop.store(true, Ordering::SeqCst);
            }
            self.messages.lock().unwrap().push(event);
        }
        fn stop_requested(&self) -> bool {
            self.stop.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn warmup_timeout_retries_once_and_success_is_shared_by_clones() {
        let (dir, shell) = fake(
            "shift 5\nif [ \"$1\" = /bin/true ]; then\n echo warm >> calls\n if [ ! -f warmed ]; then touch warmed; exec sleep 30; fi\nfi",
        );
        for shell in [shell.clone(), shell] {
            let out = shell
                .run(&host(), "echo ready\n", Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(out.stdout, b"ready\n");
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn probe_timeout_retries_once_and_reports_the_stage() {
        let (dir, shell) = fake("shift 5");
        let events = Arc::new(Events::default());
        let script = "# superba-wsl-probe\nSB_N='test'\necho attempt >> calls\necho '@@SBPH:test:files'\nif [ ! -f tried ]; then touch tried; exec sleep 30; fi\necho done\n";
        let out = crate::operation::with_events(
            events.clone(),
            shell.run(&host(), script, Duration::from_secs(5)),
        )
        .await
        .unwrap();
        assert!(out.stdout.ends_with(b"done\n"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(events.messages.lock().unwrap().iter().any(|e| matches!(e, SwitchEvent::RemoteProgress { message: Some(m), .. } if m.key == "wsl.progress.files" && m.render_fallback().contains("2/2"))));
    }

    /// alpha.10 (W90): a slow login stage gets more time on its second
    /// attempt (half again) instead of timing out at the same mark again.
    #[tokio::test]
    async fn a_slow_login_gets_more_time_on_the_second_attempt() {
        let (dir, mut shell) = fake("shift 5");
        shell.login_timeout = Duration::from_millis(800);
        let script = "# superba-wsl-probe\nSB_N='test'\necho attempt >> calls\necho '@@SBPH:test:login'\nif [ ! -f tried ]; then touch tried; sleep 3; else sleep 1.0; fi\necho done\n";
        let out = shell
            .run(&host(), script, Duration::from_secs(6))
            .await
            .unwrap();
        assert!(out.stdout.ends_with(b"done\n"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    /// alpha.10 re-test (W81): slow login and system stages used up the
    /// shared budget and the write that followed failed "in 0 s". A write
    /// still gets its minimum, and a probe its own.
    #[tokio::test]
    async fn slow_earlier_stages_still_leave_the_write_its_minimum() {
        let (dir, shell) = fake("shift 5");
        let events = Arc::new(Events::default());
        let h = host();
        let out = crate::operation::with_events(events, async {
            // What slow login and system stages left: nothing.
            crate::operation::set_remaining(&h, Duration::ZERO);
            let probe = shell
                .run(
                    &h,
                    "# superba-wsl-probe\nSB_N='test'\necho '@@SBPH:test:system'\nsleep 0.5\necho probed\n",
                    Duration::from_secs(10),
                )
                .await
                .unwrap();
            assert!(probe.stdout.ends_with(b"probed\n"));
            crate::operation::set_remaining(&h, Duration::ZERO);
            shell
                .run(
                    &h,
                    "# superba-write\necho written >> calls\nsleep 1\necho ok\n",
                    Duration::from_secs(10),
                )
                .await
        })
        .await
        .unwrap();
        assert!(out.stdout.ends_with(b"ok\n"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            "written\n"
        );
    }

    /// The timeout says what the login stage reads and what to do.
    #[tokio::test]
    async fn a_login_timeout_names_the_files_and_the_way_out() {
        let (_dir, mut shell) = fake("shift 5");
        shell.login_timeout = Duration::from_millis(300);
        let script = "# superba-wsl-probe\nSB_N='test'\necho '@@SBPH:test:login'\nexec sleep 30\n";
        let error = shell
            .run(&host(), script, Duration::from_millis(900))
            .await
            .unwrap_err();
        let Error::Timeout { what } = &error else {
            panic!("{error}")
        };
        assert!(
            what.starts_with("WSL Ubuntu: login (attempt 2/2)"),
            "{what}"
        );
        assert!(
            what.contains("~/.profile") && what.contains("~/.bashrc"),
            "{what}"
        );
        assert!(what.contains("spb wsl update"), "{what}");
    }

    #[tokio::test]
    async fn mutation_timeout_is_never_replayed_and_names_wsl() {
        let (dir, shell) = fake("shift 5");
        let error = shell
            .run(
                &host(),
                "# superba-write\necho written >> calls\nexec sleep 30\n",
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, Error::Timeout { what } if what.contains("WSL Ubuntu: write") && !what.contains("ssh")),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancellation_stops_the_probe_child_and_clears_progress() {
        let (dir, shell) = fake("shift 5");
        let events = Arc::new(Events {
            stop_at_files: true,
            ..Default::default()
        });
        let script = "# superba-wsl-probe\nSB_N='test'\necho $$ > pid\necho '@@SBPH:test:files'\nexec sleep 30\n";
        let error = crate::operation::with_events(
            events.clone(),
            shell.run(&host(), script, Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert_eq!(error, Error::Cancelled);
        let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = superba_core::process::command("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "the cancelled child still runs");
        assert!(matches!(
            events.messages.lock().unwrap().last(),
            Some(SwitchEvent::RemoteProgress { message: None, .. })
        ));
    }

    #[tokio::test]
    async fn new_phases_cannot_extend_the_total_deadline() {
        let shell = WslShell::default();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let work = async move {
            loop {
                let _ = tx.send("files".into());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let start = tokio::time::Instant::now();
        let result: Result<()> = shell
            .watch(
                &host(),
                "command",
                1,
                true,
                start + Duration::from_millis(100),
                Duration::from_secs(30),
                work,
                Some(rx),
            )
            .await;
        assert!(matches!(result, Err(Error::Timeout { .. })));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
