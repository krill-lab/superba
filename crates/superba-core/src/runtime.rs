//! What the running Codex consumers actually use (GH#4), and stopping them for
//! the user (GH#2).
//!
//! Codex reads `config.toml`, `auth.json` and its environment once, when a
//! process starts; nothing re-reads them later. So "the configuration is
//! written" and "the running Codex uses it" are two different facts:
//!
//! - the desktop app's Codex core (its app-server child) uses the new
//!   configuration once Superba restarted it (hot restart over CDP, or a full
//!   restart of the app);
//! - a codex CLI session in a terminal, `codex exec`, the CLI daemon and the
//!   app-server an IDE extension (VS Code, Cursor, ...) runs keep the
//!   configuration they started with until they are started again.
//!
//! [`RuntimeView`] puts, for every running consumer of the Codex home, the
//! configuration it started with next to the one written last, whether a turn
//! is running in it (restarting it now interrupts that turn) and how it picks
//! the new configuration up ([`RestartAdvice`]). `superba-switch` builds it from
//! observations: process start times against the generation history, the
//! command line, the effective `CODEX_HOME`, and the rollouts of running turns.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::{ProcessInfo, ThreadActivity};
use crate::config::ActiveState;
use crate::i18n::LocalizedText;
use crate::ids::ProfileId;
use crate::site::SiteKind;
use crate::time::Timestamp;

/// An editor whose extension runs a Codex app-server (the OpenAI Codex
/// extension of VS Code and its forks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IdeKind {
    /// Visual Studio Code.
    VsCode,
    /// Visual Studio Code - Insiders.
    VsCodeInsiders,
    /// Cursor.
    Cursor,
    /// Windsurf.
    Windsurf,
    /// VSCodium.
    VsCodium,
    /// Another editor (an extension directory or host Superba does not know).
    Other,
}

impl IdeKind {
    /// Display name.
    pub fn text(self) -> LocalizedText {
        match self {
            IdeKind::VsCode => LocalizedText::new("ide.vscode", "VS Code"),
            IdeKind::VsCodeInsiders => {
                LocalizedText::new("ide.vscode_insiders", "VS Code Insiders")
            }
            IdeKind::Cursor => LocalizedText::new("ide.cursor", "Cursor"),
            IdeKind::Windsurf => LocalizedText::new("ide.windsurf", "Windsurf"),
            IdeKind::VsCodium => LocalizedText::new("ide.vscodium", "VSCodium"),
            IdeKind::Other => LocalizedText::new("ide.other", "an editor"),
        }
    }

    /// The editor whose per-user extension directory holds `exe`
    /// (`~/.vscode/extensions/openai.chatgpt-…/bin/…/codex`, `~/.cursor/…`,
    /// `~/.windsurf/…`, `~/.vscode-insiders/…`, `~/.vscode-oss/…`), matched on
    /// path components in any case and with either separator.
    pub fn from_extension_path(exe: &Path) -> Option<IdeKind> {
        let s = exe
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let parts: Vec<&str> = s.split('/').collect();
        let ext = parts.iter().position(|p| *p == "extensions")?;
        let dir = parts.get(ext.checked_sub(1)?)?;
        Some(match *dir {
            ".vscode" => IdeKind::VsCode,
            ".vscode-insiders" => IdeKind::VsCodeInsiders,
            ".cursor" => IdeKind::Cursor,
            ".windsurf" => IdeKind::Windsurf,
            ".vscode-oss" => IdeKind::VsCodium,
            d if d.starts_with(".vscode") => IdeKind::VsCode,
            _ => return None,
        })
    }

    /// The editor a host process belongs to, from its executable name
    /// (`Code.exe`, `code`, `Code Helper (Plugin)`, `Cursor.exe`,
    /// `cursor`, `Windsurf`, `codium`, ...).
    pub fn from_host_name(name: &str) -> Option<IdeKind> {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let lower = base.to_ascii_lowercase();
        let lower = lower.strip_suffix(".exe").unwrap_or(&lower);
        if lower.starts_with("code - insiders") || lower.starts_with("code-insiders") {
            return Some(IdeKind::VsCodeInsiders);
        }
        if lower == "code" || lower.starts_with("code helper") || lower == "code-oss" {
            return Some(IdeKind::VsCode);
        }
        if lower == "cursor" || lower.starts_with("cursor helper") {
            return Some(IdeKind::Cursor);
        }
        if lower == "windsurf" || lower.starts_with("windsurf helper") {
            return Some(IdeKind::Windsurf);
        }
        if lower == "codium" || lower == "vscodium" || lower.starts_with("codium helper") {
            return Some(IdeKind::VsCodium);
        }
        None
    }

    /// The command that makes this editor's Codex extension start its Codex
    /// again without closing the editor (Command Palette entry).
    pub fn restart_command(self) -> &'static str {
        "Developer: Restart Extension Host"
    }
}

/// What a running consumer is, for people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsumerRole {
    /// The desktop app (ChatGPT / Codex app): its main process and Codex core.
    DesktopApp,
    /// An interactive codex CLI session in a terminal.
    Terminal,
    /// `codex exec` / `codex review`.
    Exec,
    /// The codex CLI's background server (`app-server --listen`).
    Daemon,
    /// The app-server of an editor's Codex extension.
    IdeExtension {
        /// The editor, when known.
        #[serde(default)]
        ide: Option<IdeKind>,
    },
    /// Another program that runs Codex (an app-server started by some tool,
    /// codex tooling subcommands).
    Other,
}

impl ConsumerRole {
    /// Display name ("ChatGPT desktop app", "codex in a terminal", "VS Code ·
    /// Codex extension", ...).
    pub fn text(&self) -> LocalizedText {
        match self {
            ConsumerRole::DesktopApp => {
                LocalizedText::new("runtime.role.app", "ChatGPT desktop app")
            }
            ConsumerRole::Terminal => {
                LocalizedText::new("runtime.role.terminal", "codex in a terminal")
            }
            ConsumerRole::Exec => LocalizedText::new("runtime.role.exec", "codex exec"),
            ConsumerRole::Daemon => {
                LocalizedText::new("runtime.role.daemon", "codex background server")
            }
            ConsumerRole::IdeExtension { ide: Some(ide) } => {
                LocalizedText::new("runtime.role.ide", "{ide} · Codex extension")
                    .arg_text("ide", ide.text())
            }
            ConsumerRole::IdeExtension { ide: None } => {
                LocalizedText::new("runtime.role.ide_unknown", "Codex extension of an editor")
            }
            ConsumerRole::Other => {
                LocalizedText::new("runtime.role.other", "Other Codex app-server")
            }
        }
    }
}

/// Which configuration generation a process started with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningGeneration {
    /// Generation number (`None`: the configuration before Superba first
    /// changed it, the "original configuration").
    #[serde(default)]
    pub generation: Option<u64>,
    /// The profile Superba applied in that generation.
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// Its site kind (official login, Krill, compatible site), when recorded.
    #[serde(default)]
    pub site_kind: Option<SiteKind>,
    /// When that generation was written.
    #[serde(default)]
    pub applied_at: Option<Timestamp>,
}

/// Why it is not known which configuration a process runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnknownReason {
    /// Superba has not written this Codex home: every consumer runs the
    /// configuration as it found it.
    NothingWritten,
    /// The OS does not report the process's start time.
    NoStartTime,
    /// The desktop app runs but no Codex core (app-server) of it is visible
    /// (starting, restarting, or on a remote / WSL host only).
    NoCore,
}

/// Whether a process runs the configuration written last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RuntimeEffect {
    /// It started after the configuration was written: it uses it.
    Current,
    /// It started before: it still runs an older configuration.
    Previous {
        /// The generation it started with.
        running: RunningGeneration,
    },
    /// Not known.
    Unknown {
        /// Why.
        reason: UnknownReason,
    },
}

impl RuntimeEffect {
    /// Still on an older configuration.
    pub fn is_previous(&self) -> bool {
        matches!(self, RuntimeEffect::Previous { .. })
    }
}

/// Whether a turn runs in the process (restarting it now interrupts it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BusyState {
    /// Not looked at (the cheap overview poll).
    #[default]
    NotChecked,
    /// No turn running.
    Idle,
    /// A turn runs in it (or, when several processes of this kind share a
    /// working directory, in one of them).
    Running {
        /// The threads.
        threads: Vec<ThreadActivity>,
    },
    /// Could not be determined.
    Unknown,
}

impl BusyState {
    /// A turn runs.
    pub fn is_running(&self) -> bool {
        matches!(self, BusyState::Running { .. })
    }
}

/// How a consumer picks up the configuration written last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RestartAdvice {
    /// Nothing to do: it already uses it (or it is not known to differ).
    None,
    /// Superba restarts the app's Codex core (about a second; the window and
    /// threads stay, a running turn loses its output).
    HotRestart,
    /// Reopen ChatGPT through Superba (it was not opened by Superba, so its
    /// Codex core cannot be restarted on its own).
    FullRestart,
    /// Leave the session (`/quit` or Ctrl+C twice) and continue it with
    /// `codex resume` in that terminal.
    Reopen,
    /// Let it finish; the next run uses the new configuration.
    Rerun,
    /// Run "Developer: Restart Extension Host" (or "Developer: Reload Window")
    /// in the editor; the editor itself stays open.
    ReloadIde {
        /// The editor, when known.
        #[serde(default)]
        ide: Option<IdeKind>,
    },
    /// Restart the CLI's background server (Superba can do it).
    RestartDaemon,
    /// Restart the program that started it.
    RestartOwner,
}

impl RestartAdvice {
    /// What the user does (or what Superba does when asked).
    pub fn text(&self) -> LocalizedText {
        match self {
            RestartAdvice::None => LocalizedText::new("runtime.advice.none", "Nothing to do."),
            RestartAdvice::HotRestart => LocalizedText::new(
                "runtime.advice.hot",
                "Superba can restart ChatGPT's Codex core (about a second; the window and threads stay).",
            ),
            RestartAdvice::FullRestart => LocalizedText::new(
                "runtime.advice.full",
                "Reopen ChatGPT through Superba: it was not opened by Superba, so its Codex core cannot be restarted on its own.",
            ),
            RestartAdvice::Reopen => LocalizedText::new(
                "runtime.advice.reopen",
                "In that terminal leave codex (/quit or Ctrl+C twice) and run `codex resume --last` to continue the session with the new configuration.",
            ),
            RestartAdvice::Rerun => LocalizedText::new(
                "runtime.advice.rerun",
                "Let it finish; the next run of codex exec uses the new configuration.",
            ),
            RestartAdvice::ReloadIde { ide } => LocalizedText::new(
                "runtime.advice.ide",
                "In {ide} run “Developer: Restart Extension Host” (Ctrl+Shift+P, on a Mac Cmd+Shift+P) or “Developer: Reload Window”; the editor stays open and the Codex extension starts its Codex again.",
            )
            .arg_text("ide", ide.unwrap_or(IdeKind::Other).text()),
            RestartAdvice::RestartDaemon => LocalizedText::new(
                "runtime.advice.daemon",
                "Restart the codex background server (the next switch does it, or `codex app-server-control restart`).",
            ),
            RestartAdvice::RestartOwner => LocalizedText::new(
                "runtime.advice.owner",
                "Restart the program that started this Codex process.",
            ),
        }
    }
}

/// One running consumer of the Codex home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerRuntime {
    /// What it is.
    pub role: ConsumerRole,
    /// The process (the app: its main process; its Codex core is `core_pid`).
    pub pid: u32,
    /// The desktop app's Codex core (app-server) process.
    #[serde(default)]
    pub core_pid: Option<u32>,
    /// When the process that read the configuration started (the app: its
    /// Codex core).
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// Effective Codex home.
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    /// Working directory (tells two terminals apart).
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Executable.
    #[serde(default)]
    pub exe: Option<PathBuf>,
    /// Command line (redacted).
    #[serde(default)]
    pub cmdline: Vec<String>,
    /// Which configuration it runs.
    pub effect: RuntimeEffect,
    /// A turn runs in it.
    #[serde(default)]
    pub busy: BusyState,
    /// How it picks the written configuration up.
    pub advice: RestartAdvice,
    /// Superba can stop this process for the user (graceful where the
    /// platform allows, else only by force after confirmation).
    #[serde(default)]
    pub can_stop: bool,
}

impl ConsumerRuntime {
    /// Still on an older configuration.
    pub fn is_previous(&self) -> bool {
        self.effect.is_previous()
    }
}

/// Written configuration versus what the running consumers use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeView {
    /// When observed.
    pub checked_at: Timestamp,
    /// The configuration Superba wrote last (`None`: Superba has not written
    /// this Codex home, or it restored the original configuration).
    #[serde(default)]
    pub written: Option<ActiveState>,
    /// Running consumers of the Codex home.
    #[serde(default)]
    pub consumers: Vec<ConsumerRuntime>,
    /// Running turns were looked at ([`ConsumerRuntime::busy`] is meaningful).
    #[serde(default)]
    pub busy_checked: bool,
}

impl RuntimeView {
    /// Consumers still on an older configuration.
    pub fn previous(&self) -> impl Iterator<Item = &ConsumerRuntime> {
        self.consumers.iter().filter(|c| c.is_previous())
    }

    /// How many consumers still run an older configuration.
    pub fn previous_count(&self) -> usize {
        self.previous().count()
    }
}

/// Result of stopping one process for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopOutcome {
    /// It exited after being asked.
    Exited,
    /// It was ended by force.
    Killed,
    /// It still runs (it did not react in time).
    StillRunning,
    /// It was already gone (or its pid now belongs to another program).
    NotRunning,
    /// This platform cannot ask it to exit (Windows: a console program or an
    /// app-server talking over pipes has no window to close); only ending it by
    /// force works.
    ForceOnly,
}

impl StopOutcome {
    /// The process no longer runs.
    pub fn is_gone(self) -> bool {
        matches!(
            self,
            StopOutcome::Exited | StopOutcome::Killed | StopOutcome::NotRunning
        )
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            StopOutcome::Exited => LocalizedText::new("runtime.stop.exited", "exited"),
            StopOutcome::Killed => LocalizedText::new("runtime.stop.killed", "ended by force"),
            StopOutcome::StillRunning => {
                LocalizedText::new("runtime.stop.still_running", "still running")
            }
            StopOutcome::NotRunning => {
                LocalizedText::new("runtime.stop.not_running", "already closed")
            }
            StopOutcome::ForceOnly => LocalizedText::new(
                "runtime.stop.force_only",
                "cannot be asked to exit on this system",
            ),
        }
    }
}

/// One process stopped for the user, and how it went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReport {
    /// The process as it was listed.
    pub process: ProcessInfo,
    /// Result.
    pub outcome: StopOutcome,
    /// Error text when stopping failed.
    #[serde(default)]
    pub error: Option<String>,
}

/// A Codex process's display line: role, working directory and pid.
pub fn process_role(p: &ProcessInfo) -> ConsumerRole {
    use crate::app::CodexProcessKind as K;
    match &p.kind {
        K::AppMain | K::AppServer => ConsumerRole::DesktopApp,
        K::CliTui => ConsumerRole::Terminal,
        K::CliExec => ConsumerRole::Exec,
        K::CliDaemon => ConsumerRole::Daemon,
        K::IdeAppServer => ConsumerRole::IdeExtension { ide: p.ide },
        _ => ConsumerRole::Other,
    }
}

/// What stopping `p` does to it, for the confirmation list ("the session ends;
/// continue it later with `codex resume`", "the extension loses its Codex until
/// its window is reloaded", ...).
pub fn stop_effect(p: &ProcessInfo) -> LocalizedText {
    match process_role(p) {
        ConsumerRole::DesktopApp => LocalizedText::new(
            "runtime.stop_effect.app",
            "ChatGPT quits; Superba opens it again after the switch.",
        ),
        ConsumerRole::Terminal => LocalizedText::new(
            "runtime.stop_effect.terminal",
            "The codex session in that terminal ends (the conversation is kept: `codex resume --last` continues it).",
        ),
        ConsumerRole::Exec => LocalizedText::new(
            "runtime.stop_effect.exec",
            "The codex exec run ends before it finishes.",
        ),
        ConsumerRole::Daemon => LocalizedText::new(
            "runtime.stop_effect.daemon",
            "The background server stops; codex starts it again when needed.",
        ),
        ConsumerRole::IdeExtension { ide } => LocalizedText::new(
            "runtime.stop_effect.ide",
            "The Codex extension in {ide} loses its Codex until you run “Developer: Restart Extension Host” or reload the window; {ide} itself stays open.",
        )
        .arg_text("ide", ide.unwrap_or(IdeKind::Other).text()),
        _ => LocalizedText::new(
            "runtime.stop_effect.other",
            "The program that started it loses its Codex.",
        ),
    }
}
