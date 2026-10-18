//! Desktop app and Codex process types ([`crate::AppController`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::codex::{CodexVersion, LOCAL_HOST_ID};
use crate::env::EnvObservation;
use crate::i18n::LocalizedText;
use crate::ids::TargetId;
use crate::time::Timestamp;

/// Which Electron app variant.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppVariant {
    /// ChatGPT desktop (Electron) with Codex.
    ChatGpt,
    /// Codex app.
    Codex,
    /// Codex beta.
    CodexBeta,
    /// Unrecognised but Electron-based.
    Other(String),
}

/// An installed desktop app (native Swift "ChatGPT Classic" is excluded).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstall {
    /// Stable id (bundle id, MSIX family, or install dir hash).
    pub id: String,
    /// Variant.
    pub variant: AppVariant,
    /// Install location (.app bundle, MSIX dir, /usr/lib/chatgpt ...).
    pub path: PathBuf,
    /// Main executable.
    pub executable: PathBuf,
    /// App version (`26.1013...`).
    #[serde(default)]
    pub app_version: Option<String>,
    /// Bundled Codex CLI binary.
    #[serde(default)]
    pub bundled_codex: Option<PathBuf>,
    /// Its version.
    #[serde(default)]
    pub bundled_codex_version: Option<CodexVersion>,
}

impl AppInstall {
    /// The MSIX package version of a Windows install (`26.1013.4894.0`), read from
    /// its folder, which is named by the package full name
    /// (`OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0`). It is not the app's
    /// release version ([`Self::app_version`], `26.1013.40934`). `None` for any
    /// other install.
    pub fn package_version(&self) -> Option<&str> {
        let name = self.path.file_name()?.to_str()?;
        let parts: Vec<&str> = name.split('_').collect();
        let [family, version, arch, _resource, publisher] = parts.as_slice() else {
            return None;
        };
        let numeric = version.split('.').count() == 4
            && version
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        (numeric && !family.is_empty() && !arch.is_empty() && !publisher.is_empty())
            .then_some(*version)
    }

    /// The app version for display: [`app_version_text`] of the release version
    /// and the package version.
    pub fn version_text(&self) -> Option<LocalizedText> {
        app_version_text(self.app_version.as_deref(), self.package_version())
    }
}

/// A desktop app version for display, the same everywhere: the release version
/// (`26.1013.40934`) and, for an MSIX package, its package version labeled
/// (`26.1013.40934 (package 26.1013.4894.0)`). A release version equal to the
/// package version is one read from the package (the app's own was unreadable)
/// and is shown as the package version.
pub fn app_version_text(release: Option<&str>, package: Option<&str>) -> Option<LocalizedText> {
    let release = release.filter(|r| Some(*r) != package);
    match (release, package) {
        (Some(r), Some(p)) => Some(
            LocalizedText::new("app_version.with_package", "{version} (package {package})")
                .arg("version", r)
                .arg("package", p),
        ),
        (Some(r), None) => {
            Some(LocalizedText::new("app_version.release", "{version}").arg("version", r))
        }
        (None, Some(p)) => {
            Some(LocalizedText::new("app_version.package", "package {package}").arg("package", p))
        }
        (None, None) => None,
    }
}

/// A CDP endpoint of a running app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdpEndpoint {
    /// Loopback port.
    pub port: u16,
    /// The listener was verified to be the app main process (plan 12.1).
    pub owner_verified: bool,
}

/// A running desktop app main process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSession {
    /// Which install, when known.
    #[serde(default)]
    pub install_id: Option<String>,
    /// Main process pid (no `--type=` argument).
    pub main_pid: u32,
    /// Process start time.
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// Executable.
    #[serde(default)]
    pub executable: Option<PathBuf>,
    /// CDP endpoint (`None` = launched without `--remote-debugging-port`).
    #[serde(default)]
    pub cdp: Option<CdpEndpoint>,
    /// Launched by Superba.
    pub launched_by_us: bool,
    /// `CODEX_AUTHAPI_BASE_URL` in the main process env; inherited by every
    /// hot-restarted app-server (plan 7.1 rules 6/7).
    #[serde(default)]
    pub authapi_env: EnvObservation,
    /// `HOME` override the app runs with (isolated e2e runs).
    #[serde(default)]
    pub home_override: Option<PathBuf>,
    /// Effective Codex home of the app (`CODEX_HOME` in its env, else `<HOME>/.codex`);
    /// `None` when it cannot be observed. Diagnostics flag a mismatch with ours.
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    /// Process image of the app's local app-server (`/proc/<pid>/exe`,
    /// `proc_pidpath`, `QueryFullProcessImageName`): the only proof of a
    /// `CODEX_CLI_PATH` pin. `None` = not observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_server_exe: Option<PathBuf>,
}

/// How to expose CDP when launching.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "port", rename_all = "snake_case")]
pub enum CdpMode {
    /// No debugging port (safe mode).
    Disabled,
    /// `--remote-debugging-port=0` + `DevToolsActivePort`, falling back to a random
    /// free port verified after start.
    #[default]
    Auto,
    /// A fixed loopback port (tests).
    Port(u16),
}

/// Launch parameters. Always loopback-only CDP and
/// `--remote-allow-origins=http://127.0.0.1:<port>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOptions {
    /// Install to launch (`None` = the default install).
    #[serde(default)]
    pub install_id: Option<String>,
    /// CDP mode.
    #[serde(default)]
    pub cdp: CdpMode,
    /// Extra environment (normally `CODEX_AUTHAPI_BASE_URL`, injected on every launch).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables to remove from the inherited environment.
    #[serde(default)]
    pub env_remove: Vec<String>,
    /// Extra arguments (e2e: `--no-sandbox --disable-gpu`).
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Run with this `HOME` (isolated e2e runs only).
    #[serde(default)]
    pub home_override: Option<PathBuf>,
    /// Wait this long for the CDP bridge to become ready (0 = default 15 s).
    #[serde(default)]
    pub ready_timeout_ms: u64,
}

/// Result of probing the restart bridge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BridgeStatus {
    /// `window.electronBridge.sendMessageFromView` is callable in the main renderer.
    Ready {
        /// The renderer target URL.
        target_url: String,
    },
    /// The app has no CDP port.
    NoCdp,
    /// The port did not answer.
    CdpUnreachable {
        /// Reason.
        detail: String,
    },
    /// The port's listener is not the app main process.
    OwnerMismatch,
    /// No `app://-/index.html` page target.
    NoRendererTarget,
    /// The page exists but the bridge function does not.
    BridgeMissing,
}

impl BridgeStatus {
    /// True if a hot restart can be sent.
    pub fn is_ready(&self) -> bool {
        matches!(self, BridgeStatus::Ready { .. })
    }
}

/// Which app-server to restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotRestartRequest {
    /// `local` or a remote `hostId`.
    pub host_id: String,
    /// Pass `killCodexProcess: true` (remote hosts).
    pub kill_remote: bool,
}

impl HotRestartRequest {
    /// The local app-server.
    pub fn local() -> Self {
        Self {
            host_id: LOCAL_HOST_ID.into(),
            kill_remote: false,
        }
    }

    /// A remote host's app-server.
    pub fn remote(host_id: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
            kill_remote: true,
        }
    }
}

/// Result of sending the restart IPC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotRestartOutcome {
    /// When the IPC was sent (T0 for verification).
    pub sent_at: Timestamp,
}

/// Quit behaviour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuitOptions {
    /// Graceful wait (default 15 s).
    pub graceful_timeout_ms: u64,
    /// Force-kill after the timeout (only after explicit user confirmation).
    pub force: bool,
}

impl Default for QuitOptions {
    fn default() -> Self {
        Self {
            graceful_timeout_ms: 15_000,
            force: false,
        }
    }
}

/// Result of a quit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuitOutcome {
    /// Exited gracefully.
    Exited,
    /// Was force-killed.
    Killed,
    /// Still running after the graceful timeout (force was not allowed).
    TimedOut,
    /// Was not running.
    NotRunning,
}

/// Kind of a Codex-related process.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodexProcessKind {
    /// Desktop app main process.
    AppMain,
    /// app-server spawned by the desktop app.
    AppServer,
    /// Interactive CLI (TUI).
    CliTui,
    /// `codex exec`.
    CliExec,
    /// CLI daemon (`app-server-control`).
    CliDaemon,
    /// app-server spawned by an IDE extension.
    IdeAppServer,
    /// Local `ssh ... codex app-server --listen` child of the app (remote host link).
    RemoteLink {
        /// Host as seen on the ssh command line.
        host: String,
    },
    /// Something else that runs `codex`.
    Other,
}

/// A running process relevant to switching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// pid.
    pub pid: u32,
    /// Parent pid.
    #[serde(default)]
    pub ppid: Option<u32>,
    /// Kind.
    pub kind: CodexProcessKind,
    /// Executable.
    #[serde(default)]
    pub exe: Option<PathBuf>,
    /// Command line, sanitized with [`redact_argv`] (other programs may put secrets
    /// on argv; Superba never does).
    #[serde(default)]
    pub cmdline: Vec<String>,
    /// Start time.
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// `CODEX_AUTHAPI_BASE_URL` in its environment.
    #[serde(default)]
    pub authapi_env: EnvObservation,
    /// Effective Codex home of the process, when observable.
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    /// Started before the current generation was applied (still on the old config).
    /// Computed by `superba-switch` from `started_at` and the generation time;
    /// `AppController` implementations leave it `false`.
    #[serde(default)]
    pub stale: bool,
    /// The editor whose Codex extension runs this app-server (from the
    /// extension directory of its executable or its host process).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ide: Option<crate::runtime::IdeKind>,
    /// Working directory, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

/// Sanitize a foreign command line for display and logs: values of flags whose
/// name mentions key/token/secret/password/auth, and anything that looks like an
/// API key (`sk-...`, Krill's `nb_...`), become `***`.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    fn sensitive_name(flag: &str) -> bool {
        let f = flag.trim_start_matches('-').to_ascii_lowercase();
        [
            "key",
            "token",
            "secret",
            "password",
            "passwd",
            "auth",
            "credential",
        ]
        .iter()
        .any(|w| f.contains(w))
    }
    fn looks_like_key(v: &str) -> bool {
        v.starts_with("sk-")
            || v.starts_with("nb_")
            || v.starts_with("sess-")
            || v.starts_with("eyJ")
    }
    let mut out = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for arg in argv {
        if redact_next {
            out.push("***".to_owned());
            redact_next = false;
            continue;
        }
        if arg.starts_with('-') {
            if let Some((flag, _)) = arg.split_once('=') {
                if sensitive_name(flag) {
                    out.push(format!("{flag}=***"));
                    continue;
                }
            } else if sensitive_name(arg) {
                redact_next = true;
            }
        }
        if looks_like_key(arg) {
            out.push("***".to_owned());
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// Where a Codex binary came from (for upgrade instructions).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstallSource {
    /// `npm i -g @openai/codex`.
    Npm,
    /// Homebrew.
    Brew,
    /// Bundled inside the desktop app.
    AppBundled,
    /// `cargo install`.
    Cargo,
    /// Unknown.
    Unknown,
}

/// A Codex CLI binary found on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexBinary {
    /// Path.
    pub path: PathBuf,
    /// `codex --version`.
    #[serde(default)]
    pub version: Option<CodexVersion>,
    /// Origin.
    pub source: InstallSource,
    /// Position on PATH (0 = first); `None` if not on PATH.
    #[serde(default)]
    pub path_rank: Option<usize>,
}

impl CodexBinary {
    /// A desktop app's own Codex that no terminal runs: bundled with (or
    /// copied by) the app and not on PATH.
    pub fn is_app_copy(&self) -> bool {
        self.source == InstallSource::AppBundled && self.path_rank.is_none()
    }
}

/// The Codex CLI a new terminal runs among `binaries`: the first `codex` on
/// PATH, else the first one found that is not a desktop app's own copy
/// ([`CodexBinary::is_app_copy`]: its bundled Codex, and on Windows the
/// runtime copy in `%LOCALAPPDATA%\OpenAI\Codex\bin\<hash>` it runs its
/// app-server from, which no terminal finds; user bug 55).
pub fn terminal_cli(binaries: &[CodexBinary]) -> Option<&CodexBinary> {
    binaries
        .iter()
        .filter(|b| b.path_rank.is_some())
        .min_by_key(|b| b.path_rank)
        .or_else(|| binaries.iter().find(|b| !b.is_app_copy()))
}

/// How an active turn was detected (plan 7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySignal {
    /// Rollout tail shows a started, unfinished turn.
    RolloutTail,
    /// Rollout written within the last ~20 s (fallback, may false-positive).
    RolloutMtime,
    /// Stop button visible in the renderer (auxiliary).
    RendererDom,
    /// Not a conversation: the desktop app is finishing its own first-start
    /// setup (on Windows `codex-windows-sandbox-setup` creating the sandbox,
    /// "Finishing Windows setup"), after which it restarts its Codex core
    /// itself. Restarting the core meanwhile left the app stuck on that screen
    /// (user bug 59), so a switch waits for it like for a running turn.
    AppSetup,
}

/// A thread with a turn in flight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadActivity {
    /// Target the thread runs on.
    pub target_id: TargetId,
    /// Thread / session id.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Thread title.
    #[serde(default)]
    pub title: Option<String>,
    /// Rollout file (path string; remote paths are POSIX).
    #[serde(default)]
    pub rollout_path: Option<String>,
    /// Last event time.
    #[serde(default)]
    pub last_event_at: Option<Timestamp>,
    /// Signal.
    pub signal: ActivitySignal,
    /// `originator` of the session (`Codex Desktop`, `codex_cli_rs`,
    /// `codex_exec`, `codex_vscode`, ...), from the rollout's session header:
    /// which kind of client runs the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    /// Working directory of the session (session header).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Turn activity of one target at one moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnActivity {
    /// Target.
    pub target_id: TargetId,
    /// When checked.
    pub checked_at: Timestamp,
    /// Busy threads.
    pub busy: Vec<ThreadActivity>,
    /// False if some signal source failed (treat as "unknown", ask the user).
    pub complete: bool,
}

impl TurnActivity {
    /// True if any thread is busy.
    pub fn is_busy(&self) -> bool {
        !self.busy.is_empty()
    }
}

/// A thread reference for "return to the original conversation".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRef {
    /// Thread id.
    pub thread_id: String,
    /// Title.
    #[serde(default)]
    pub title: Option<String>,
}

/// Read-only renderer state (verification check 4, plan 7.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererState {
    /// Current page URL.
    pub url: String,
    /// Showing the login page (auth was lost).
    pub on_login_page: bool,
    /// Showing home (e.g. kicked out of a thread by a restart mid-turn).
    pub on_home: bool,
    /// Open thread, if any.
    #[serde(default)]
    pub current_thread: Option<ThreadRef>,
}

/// Something the environment monitor of the switch service should know about
/// ([`crate::SwitchService::environment_event`]). The monitor owns the slow
/// facts (desktop app installs, Codex binaries and their versions); it looks at
/// the disk again only on these events, on a cheap change check of the watched
/// paths, and on a long interval, never because a view was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EnvironmentEvent {
    /// The window is shown (start-up, restored from the taskbar/dock): watch.
    Shown,
    /// The window is hidden or minimised: stop watching until it is shown.
    Hidden,
    /// The window got the focus back: check the watched paths now (the user
    /// may have installed or updated something meanwhile).
    FocusGained,
    /// The user asked to refresh: detect everything again and ask every Codex
    /// binary for its version again.
    Refresh,
}
