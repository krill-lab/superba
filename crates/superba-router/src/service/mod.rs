//! Per-user service lifecycle of the router (install, uninstall, status, restart).
//!
//! The router runs as a supervised per-user background service so it survives
//! crashes and logins without Superba being open:
//!
//! | Platform | Backend | Supervision |
//! |---|---|---|
//! | Linux with a user systemd | [`ServiceKind::SystemdUser`]: `~/.config/systemd/user/superba-router.service` | `Restart=always` |
//! | Linux without one (WSL, containers, no session bus) | [`ServiceKind::XdgAutostart`]: `~/.config/autostart/superba-router.desktop` + a detached `superba-router supervise` | the supervisor restarts `run` |
//! | macOS | [`ServiceKind::LaunchAgent`]: `~/Library/LaunchAgents/com.krill.superba.router.plist` | `KeepAlive` |
//! | Windows | [`ServiceKind::WindowsRunKey`]: `HKCU\…\Run` value `SuperbaRouter` + a detached `superba-router supervise` | the supervisor restarts `run` |
//!
//! Every backend is plain logic over three injected seams — the home directory
//! ([`ServiceHost`]), a [`CommandRunner`] (systemctl, launchctl, detached spawns,
//! signals) and a [`RunKeyStore`] (the Windows Run key) — so all of them are unit
//! tested on Linux with fakes and never touch the real user's service manager,
//! registry or processes. Only [`SystemRunner`] and the registry store talk to the
//! OS.
//!
//! Contract shared by every backend:
//! - `install` is idempotent: it (re)writes the definition and (re)starts the
//!   service so a new binary or data dir takes effect.
//! - `uninstall` is idempotent: it stops the service, removes the definition and
//!   succeeds when nothing was installed. Superba writes the direct-mode Codex
//!   configuration *before* calling it (see [`crate::watchdog`]).
//! - No secret ever appears in a service definition, command line or environment:
//!   the router reads its keys from its own 0600 file at start.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use superba_core::Result;

mod autostart;
mod launchd;
mod runkey;
pub mod supervise;
mod system;
mod systemd;

pub use launchd::{LAUNCHCTL, launchctl_print_fields};
pub use system::SystemRunner;
#[cfg(windows)]
pub use system::WindowsRunKey;

/// launchd label (also the plist file stem).
pub const LAUNCHD_LABEL: &str = "com.krill.superba.router";
/// systemd user unit name.
pub const SYSTEMD_UNIT: &str = "superba-router.service";
/// XDG autostart desktop entry file name.
pub const AUTOSTART_FILE: &str = "superba-router.desktop";
/// Value name under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub const RUN_VALUE_NAME: &str = "SuperbaRouter";

/// Soft open-file limit the LaunchAgent and the systemd unit start the router
/// with (launchd's default for agents is 256; each proxied request holds two
/// sockets). The router also raises its own soft limit at start; the
/// XDG-autostart and Windows backends rely on that alone.
pub const NOFILE_SOFT: u64 = superba_core::fdlimit::OPEN_FILES_TARGET;
/// Hard open-file limit in the same definitions: systemd's default hard limit.
/// It never asks for more than the system allows: macOS lowers a larger hard
/// limit to `kern.maxfilesperproc`, and systemd sets the closest value a user
/// manager with a lower hard limit permits.
pub const NOFILE_HARD: u64 = 524_288;

/// What to run as the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Absolute path of the `superba-router` executable.
    pub exe: PathBuf,
    /// Superba data dir, passed as `--data-dir <dir>`.
    pub data_dir: PathBuf,
    /// File receiving stdout/stderr where the backend needs one (launchd, the
    /// detached supervisor). systemd uses the journal instead.
    pub log_file: PathBuf,
    /// File holding the pid of the detached supervisor (XDG autostart and Windows
    /// backends only).
    pub pid_file: PathBuf,
    /// Extra environment for the service, e.g. `RUST_LOG`. Never secrets.
    pub env: Vec<(String, String)>,
}

impl ServiceSpec {
    /// Arguments of the router process itself: `run --data-dir <dir>`.
    pub fn run_args(&self) -> Vec<OsString> {
        vec![
            "run".into(),
            "--data-dir".into(),
            self.data_dir.clone().into_os_string(),
        ]
    }

    /// Arguments of the self-supervisor: `supervise --data-dir <dir>`.
    pub fn supervise_args(&self) -> Vec<OsString> {
        vec![
            "supervise".into(),
            "--data-dir".into(),
            self.data_dir.clone().into_os_string(),
        ]
    }
}

/// Which service backend is in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// Linux user systemd unit.
    SystemdUser,
    /// Linux fallback: XDG autostart entry plus a detached self-supervisor.
    XdgAutostart,
    /// macOS LaunchAgent.
    LaunchAgent,
    /// Windows per-user Run key plus a detached self-supervisor.
    WindowsRunKey,
}

/// Observed state of the service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Backend.
    pub kind: ServiceKind,
    /// The definition (unit, plist, desktop entry, Run value) exists.
    pub installed: bool,
    /// The service manager reports it running; `None` when it cannot tell.
    pub running: Option<bool>,
    /// Main pid (or the supervisor's pid) when known.
    pub pid: Option<u32>,
    /// Short diagnostic (e.g. `systemctl is-active` output). Never secrets.
    pub detail: Option<String>,
}

/// Captured output of a finished command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code; `None` when killed by a signal.
    pub code: Option<i32>,
    /// Standard output (lossy UTF-8).
    pub stdout: String,
    /// Standard error (lossy UTF-8).
    pub stderr: String,
}

impl CommandOutput {
    /// True when the exit code is 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// `exit 5` / `killed by a signal`, for messages.
    pub fn exit_text(&self) -> String {
        match self.code {
            Some(code) => format!("exit {code}"),
            None => "killed by a signal".into(),
        }
    }

    /// Standard error, else standard output, trimmed and cut to `max` characters.
    pub fn message(&self, max: usize) -> String {
        let text = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        cut(text, max)
    }
}

/// `text` cut to `max` characters, with `…` when cut.
pub(crate) fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// A command that was run for diagnostics and what it answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecord {
    /// The command line as a user would type it (arguments with spaces quoted).
    pub command: String,
    /// Its output, or why it could not run.
    pub result: std::result::Result<CommandRecordOutput, String>,
}

/// [`CommandOutput`] in a serializable form.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecordOutput {
    /// Exit code; `None` when killed by a signal (or stopped after its timeout).
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

impl CommandRecord {
    /// Run `program args` through `runner` (at most `timeout`) and record it.
    pub fn run(
        runner: &dyn CommandRunner,
        program: &str,
        args: &[OsString],
        timeout: Duration,
    ) -> Self {
        let command = command_line(program, args);
        let result = runner
            .run_timeout(program, args, timeout)
            .map(|o| CommandRecordOutput {
                code: o.code,
                stdout: o.stdout,
                stderr: o.stderr,
            })
            .map_err(|e| e.to_string());
        Self { command, result }
    }

    /// True when the command ran and exited with 0.
    pub fn success(&self) -> bool {
        matches!(&self.result, Ok(o) if o.code == Some(0))
    }
}

/// `program args` as a user would type it in a POSIX shell.
pub fn command_line(program: &str, args: &[OsString]) -> String {
    let quote = |s: &str| {
        if !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-:=@,+%".contains(c))
        {
            s.to_owned()
        } else {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
    };
    std::iter::once(quote(program))
        .chain(args.iter().map(|a| quote(&a.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the service manager reports about the service, for diagnostics
/// (`spb router doctor`, the detail of a failed start). Never secrets: service
/// definitions hold paths and `RUST_LOG` only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDiagnosis {
    /// Where the definition lives (plist, unit, desktop entry); `None` for the
    /// Windows Run key.
    pub definition_path: Option<PathBuf>,
    /// The definition's text, when it exists.
    pub definition: Option<String>,
    /// The program the definition starts, when it could be read.
    pub program: Option<PathBuf>,
    /// The service manager's own view (`launchctl print ...`).
    pub query: Option<CommandRecord>,
    /// Facts from it (`state`, `pid`, `runs`, `last exit code`, ...).
    pub fields: Vec<(String, String)>,
    /// The service manager has it loaded (`None` when it cannot tell).
    pub loaded: Option<bool>,
    /// It keeps exiting and being restarted.
    pub crash_looping: bool,
    /// Exit code of the last run, when the service manager reports one.
    pub last_exit_code: Option<i32>,
    /// Signal that ended the last run (`Killed: 9`), when reported.
    pub last_signal: Option<String>,
}

impl ServiceDiagnosis {
    /// The value of `key` in [`ServiceDiagnosis::fields`].
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Runs external commands and manages detached processes. The real one is
/// [`SystemRunner`]; tests record calls.
pub trait CommandRunner: Send + Sync + fmt::Debug {
    /// Run `program args` to completion and capture its output (no shell).
    fn run(&self, program: &str, args: &[OsString]) -> std::io::Result<CommandOutput>;

    /// Start `exe args` detached from the caller (own session / process group, no
    /// console window, stdin null, stdout+stderr appended to `log_file`), with
    /// `env` added to the inherited environment. Returns its pid.
    fn spawn_detached(
        &self,
        exe: &Path,
        args: &[OsString],
        env: &[(String, String)],
        log_file: &Path,
    ) -> std::io::Result<u32>;

    /// True if a process with this pid exists and its executable file name is
    /// `superba-router` (`superba-router.exe`): pids are reused, and only our own
    /// process may ever be signalled.
    fn is_router_process(&self, pid: u32) -> bool;

    /// Ask the process to stop (SIGTERM on Unix; terminate the process tree on
    /// Windows). Callers check [`CommandRunner::is_router_process`] first.
    fn terminate(&self, pid: u32) -> std::io::Result<()>;

    /// [`CommandRunner::run`], giving up after `timeout` (diagnostic commands
    /// such as `superba-router --version`, which macOS may hold while Gatekeeper
    /// asks the user). The default ignores the timeout.
    fn run_timeout(
        &self,
        program: &str,
        args: &[OsString],
        timeout: Duration,
    ) -> std::io::Result<CommandOutput> {
        let _ = timeout;
        self.run(program, args)
    }

    /// Wait between polls of the service manager. Fakes return at once.
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Access to the per-user Run key (Windows). Faked in tests.
pub trait RunKeyStore: Send + Sync + fmt::Debug {
    /// Read a value.
    fn get(&self, name: &str) -> std::io::Result<Option<String>>;
    /// Create or replace a value (REG_SZ).
    fn set(&self, name: &str, value: &str) -> std::io::Result<()>;
    /// Delete a value; succeeds when it does not exist.
    fn delete(&self, name: &str) -> std::io::Result<()>;
}

/// Where and how service definitions are written.
#[derive(Clone)]
pub struct ServiceHost {
    /// The user's home directory.
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME` when set (Linux); defaults to `<home>/.config`.
    pub xdg_config_home: Option<PathBuf>,
    /// Numeric user id for `launchctl` domains (`gui/<uid>`); `None` off macOS.
    pub uid: Option<u32>,
    /// Command runner.
    pub runner: Arc<dyn CommandRunner>,
    /// Run key access (Windows backend); `None` makes that backend fail with
    /// `Error::Unsupported`.
    pub run_key: Option<Arc<dyn RunKeyStore>>,
}

impl fmt::Debug for ServiceHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceHost")
            .field("home", &self.home)
            .field("xdg_config_home", &self.xdg_config_home)
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

impl ServiceHost {
    /// The real host: process home / `XDG_CONFIG_HOME`, current uid, [`SystemRunner`]
    /// and (on Windows) the real Run key.
    pub fn system(home: PathBuf) -> Self {
        system::system_host(home)
    }

    /// `XDG_CONFIG_HOME` or `<home>/.config`.
    pub fn config_home(&self) -> PathBuf {
        self.xdg_config_home
            .clone()
            .unwrap_or_else(|| self.home.join(".config"))
    }
}

/// A service backend.
pub trait ServiceManager: Send + Sync + fmt::Debug {
    /// Backend kind.
    fn kind(&self) -> ServiceKind;
    /// Write the definition and (re)start the service. Idempotent.
    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus>;
    /// Stop the service and remove its definition. Idempotent. `spec` gives the
    /// pid file and paths of a detached supervisor.
    fn uninstall(&self, spec: &ServiceSpec) -> Result<()>;
    /// Observe the service.
    fn status(&self, spec: &ServiceSpec) -> Result<ServiceStatus>;
    /// Restart a running service (e.g. after a binary update).
    fn restart(&self, spec: &ServiceSpec) -> Result<()>;
    /// Everything the service manager reports, for diagnostics. The default
    /// has the [`ServiceManager::status`] detail only.
    fn diagnose(&self, spec: &ServiceSpec) -> ServiceDiagnosis {
        let status = self.status(spec);
        let mut d = ServiceDiagnosis::default();
        match status {
            Ok(s) => {
                if let Some(detail) = s.detail {
                    d.fields.push(("state".into(), detail));
                }
                if let Some(pid) = s.pid {
                    d.fields.push(("pid".into(), pid.to_string()));
                }
            }
            Err(e) => d.fields.push(("error".into(), e.to_string())),
        }
        d
    }
}

/// Backend `kind` on `host`.
pub fn manager(host: ServiceHost, kind: ServiceKind) -> Box<dyn ServiceManager> {
    match kind {
        ServiceKind::SystemdUser => Box::new(systemd::SystemdUser::new(host)),
        ServiceKind::XdgAutostart => Box::new(autostart::XdgAutostart::new(host)),
        ServiceKind::LaunchAgent => Box::new(launchd::LaunchAgent::new(host)),
        ServiceKind::WindowsRunKey => Box::new(runkey::WindowsRunKeyService::new(host)),
    }
}

/// The backend this platform uses: macOS → LaunchAgent, Windows → Run key, Linux
/// and other Unix → systemd user unit when `systemctl --user show-environment`
/// succeeds through `host.runner`, else XDG autostart.
pub fn detect(host: &ServiceHost) -> ServiceKind {
    if cfg!(target_os = "macos") {
        ServiceKind::LaunchAgent
    } else if cfg!(windows) {
        ServiceKind::WindowsRunKey
    } else if systemd::user_manager_available(host) {
        ServiceKind::SystemdUser
    } else {
        ServiceKind::XdgAutostart
    }
}

/// [`detect`] + [`manager`].
pub fn for_platform(host: ServiceHost) -> Box<dyn ServiceManager> {
    let kind = detect(&host);
    manager(host, kind)
}
