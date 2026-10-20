//! Coding-agent tools Superba installs, updates and removes (`superba-tools`):
//! Claude Code, dsh, opencode, pi, ZCode, WorkBuddy and WorkBuddy AI.
//!
//! Each tool is installed with the vendor's own artifacts the way its research
//! report (`multi-harness.md` §5/§7) chose for this OS: never with
//! administrator rights, every download checked against a published sha256 /
//! sha512 or (Windows installers) a signature, and without touching the
//! tool's configuration or data. The view carries what is installed (where,
//! how, which version), the newest version on the channel Superba follows,
//! the tool's own updater and what the user must confirm first.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::i18n::{LocalizedText, Notice};
use crate::time::Timestamp;

/// A tool Superba knows how to install.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ToolId {
    /// Anthropic's Claude Code (CLI).
    #[serde(rename = "claude")]
    ClaudeCode,
    /// DeepSeek Harness (`dsh`).
    #[serde(rename = "dsh")]
    Dsh,
    /// opencode (OpenCode 2 by default; an OpenCode 1 install keeps its major).
    #[serde(rename = "opencode")]
    Opencode,
    /// pi (`@earendil-works/pi-coding-agent`).
    #[serde(rename = "pi")]
    Pi,
    /// ZCode (Z.ai desktop app).
    #[serde(rename = "zcode")]
    Zcode,
    /// WorkBuddy (Tencent, China build).
    #[serde(rename = "workbuddy")]
    Workbuddy,
    /// WorkBuddy AI (Tencent, international build).
    #[serde(rename = "workbuddy-ai")]
    WorkbuddyAi,
}

impl ToolId {
    /// Every tool, in display order.
    pub const ALL: [ToolId; 7] = [
        ToolId::ClaudeCode,
        ToolId::Dsh,
        ToolId::Opencode,
        ToolId::Pi,
        ToolId::Zcode,
        ToolId::Workbuddy,
        ToolId::WorkbuddyAi,
    ];

    /// The id used on the command line and in files (`claude`, `workbuddy-ai`).
    pub fn as_str(self) -> &'static str {
        match self {
            ToolId::ClaudeCode => "claude",
            ToolId::Dsh => "dsh",
            ToolId::Opencode => "opencode",
            ToolId::Pi => "pi",
            ToolId::Zcode => "zcode",
            ToolId::Workbuddy => "workbuddy",
            ToolId::WorkbuddyAi => "workbuddy-ai",
        }
    }

    /// The tool for a command-line name (also `claude-code`, `workbuddy-intl`;
    /// case does not matter).
    pub fn parse(name: &str) -> Option<Self> {
        let n = name.trim().to_ascii_lowercase().replace('_', "-");
        Some(match n.as_str() {
            "claude" | "claude-code" | "claudecode" => ToolId::ClaudeCode,
            "dsh" | "deepseek-harness" => ToolId::Dsh,
            "opencode" => ToolId::Opencode,
            "pi" | "pi-coding-agent" => ToolId::Pi,
            "zcode" => ToolId::Zcode,
            "workbuddy" | "workbuddy-cn" => ToolId::Workbuddy,
            "workbuddy-ai" | "workbuddyai" | "workbuddy-intl" => ToolId::WorkbuddyAi,
            _ => return None,
        })
    }

    /// The product name (not translated).
    pub fn display_name(self) -> &'static str {
        match self {
            ToolId::ClaudeCode => "Claude Code",
            ToolId::Dsh => "dsh",
            ToolId::Opencode => "opencode",
            ToolId::Pi => "pi",
            ToolId::Zcode => "ZCode",
            ToolId::Workbuddy => "WorkBuddy",
            ToolId::WorkbuddyAi => "WorkBuddy AI",
        }
    }

    /// A desktop app (ZCode, WorkBuddy) rather than a command-line tool.
    pub fn is_app(self) -> bool {
        matches!(
            self,
            ToolId::Zcode | ToolId::Workbuddy | ToolId::WorkbuddyAi
        )
    }
}

impl std::fmt::Display for ToolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What Superba can do for a tool on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolSupport {
    /// Install, update and remove.
    Full,
    /// Install and remove; the tool updates itself and offers no way to stop
    /// it (WorkBuddy), so Superba shows the version only.
    InstallOnly,
    /// Only a link to the vendor's download page (no per-user install method
    /// Superba could verify on this OS).
    Link {
        /// The page or package to open.
        url: String,
    },
    /// No build of the tool for this OS or processor.
    Unavailable,
}

/// How an install of a tool was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolInstallKind {
    /// Claude Code's native layout (`~/.local/bin/claude` and
    /// `~/.local/share/claude/versions`).
    Native,
    /// A vendor release archive (opencode in `~/.opencode/bin`, pi in
    /// Superba's tools directory).
    Standalone,
    /// npm into a prefix Superba owns (dsh).
    NpmPrefix,
    /// dsh's Node-free runtime (the PyPI runtime build, unpacked).
    Runtime,
    /// The user's own global npm install.
    NpmGlobal,
    /// Homebrew.
    Homebrew,
    /// WinGet.
    Winget,
    /// pi's own installer (`~/.pi/agent/install`).
    Managed,
    /// A desktop app installed for this user.
    App,
    /// A system package (apt, dnf, ...; needs root to change).
    SystemPackage,
    /// Something else.
    Other,
}

/// One install of a tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInstall {
    /// The version, when it could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The program (or the app's folder / bundle).
    pub path: PathBuf,
    /// How it was installed.
    pub kind: ToolInstallKind,
    /// Installed (and owned) by Superba.
    #[serde(default)]
    pub by_superba: bool,
    /// The command runs this install (first on `PATH`). Apps: `true`.
    #[serde(default)]
    pub on_path: bool,
}

/// The tool's own updater.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolUpdater {
    /// The tool has none (dsh, a standalone pi).
    None,
    /// The tool checks and tells the user; it installs nothing by itself.
    Notifies,
    /// The tool installs updates by itself.
    Automatic,
    /// Turned off by a setting or variable Superba did not write.
    Off {
        /// The setting or variable (`DISABLE_AUTOUPDATER`).
        by: String,
    },
    /// Turned off by Superba: Superba manages the updates.
    Superba,
    /// The tool updates itself and cannot be stopped (WorkBuddy).
    Forced,
    /// Its package manager updates it (Homebrew, WinGet, npm).
    PackageManager,
    /// Updates are blocked for this user (Claude Code's `DISABLE_UPDATES`):
    /// Superba's install and update would do nothing either.
    Blocked {
        /// The setting or variable.
        by: String,
    },
}

/// A `PATH` directory the tool needs that the user's shell does not have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathHint {
    /// The directory.
    pub dir: PathBuf,
    /// The line to add to the shell's start-up file.
    pub line: String,
    /// That file (`~/.zshrc`).
    pub file: String,
}

/// What Superba can start for a tool now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolActions {
    /// Install (none yet, or Superba's method beside an install of another kind).
    pub install: bool,
    /// Update the install in use.
    pub update: bool,
    /// Remove the install in use.
    pub uninstall: bool,
    /// The "Superba manages updates" switch is offered.
    pub manage_updates: bool,
}

/// One tool on the Updates page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRow {
    /// The tool.
    pub id: ToolId,
    /// Product name.
    pub name: String,
    /// What Superba can do on this OS.
    pub support: ToolSupport,
    /// Installs found; the first is the one in use.
    #[serde(default)]
    pub installs: Vec<ToolInstall>,
    /// The channel Superba follows (`latest`, `stable`, `next`, `v1`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Channels offered.
    #[serde(default)]
    pub channels: Vec<String>,
    /// Newest version on [`Self::channel`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// When the newest version was looked up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Timestamp>,
    /// Why the newest version is not known (the lookup failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_error: Option<String>,
    /// The install in use is older than [`Self::latest`].
    #[serde(default)]
    pub update_available: bool,
    /// The tool's own updater.
    pub updater: ToolUpdater,
    /// What can be started.
    #[serde(default)]
    pub actions: ToolActions,
    /// Things the user must accept before installing (`ToolRequest::confirmed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub install_cautions: Vec<LocalizedText>,
    /// Things the user must accept before updating.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update_cautions: Vec<LocalizedText>,
    /// Things the user must accept before removing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uninstall_cautions: Vec<LocalizedText>,
    /// Folders with the tool's settings and data, kept by an uninstall unless
    /// the user asks to remove them too (opencode).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_dirs: Vec<PathBuf>,
    /// The program's folder is not on the shell's `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hint: Option<PathHint>,
    /// Remarks (several installs, prerequisites, a delegated method).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Notice>,
    /// The program the user chose for the command (「指定路径…」).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_path: Option<PathBuf>,
    /// Where Superba looked for the command (`PATH: …`, `Homebrew: …`),
    /// set when nothing was found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub searched: Vec<String>,
    /// How a new install is made when it differs from the usual method
    /// (`homebrew`), or why the usual method is used instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_method: Option<LocalizedText>,
}

impl ToolRow {
    /// The install in use.
    pub fn primary(&self) -> Option<&ToolInstall> {
        self.installs.first()
    }

    /// The version of the install in use.
    pub fn installed_version(&self) -> Option<&str> {
        self.primary().and_then(|i| i.version.as_deref())
    }
}

/// All tools.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsView {
    /// One row per tool, in [`ToolId::ALL`] order.
    pub rows: Vec<ToolRow>,
    /// Remarks for the whole list (WSL, online checks off).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Notice>,
}

impl ToolsView {
    /// The row of `id`.
    pub fn row(&self, id: ToolId) -> Option<&ToolRow> {
        self.rows.iter().find(|r| r.id == id)
    }
}

/// What to do with a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolOp {
    /// Install.
    Install,
    /// Update the install in use.
    Update,
    /// Remove the install in use.
    Uninstall,
}

/// One install, update or removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequest {
    /// The tool.
    pub tool: ToolId,
    /// What to do.
    pub op: ToolOp,
    /// An exact version (else the newest on the channel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The channel to take the newest version from (also remembered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The user accepted the row's cautions for this operation.
    #[serde(default)]
    pub confirmed: bool,
    /// Uninstall: also remove the tool's settings and data
    /// ([`ToolRow::data_dirs`]). Never the default.
    #[serde(default)]
    pub remove_data: bool,
}

impl ToolRequest {
    /// A request with no version, channel or confirmation.
    pub fn new(tool: ToolId, op: ToolOp) -> Self {
        Self {
            tool,
            op,
            version: None,
            channel: None,
            confirmed: false,
            remove_data: false,
        }
    }
}

/// What an operation did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReport {
    /// The tool.
    pub tool: ToolId,
    /// What was done.
    pub op: ToolOp,
    /// The version installed now (`None` after a removal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The program (or app) now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Remarks (`PATH`, the app was closed, a command to run).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Notice>,
    /// The step-by-step log.
    pub log: PathBuf,
}

/// A change to how Superba treats a tool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSettingsChange {
    /// Follow this channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Turn "Superba manages updates" on (the tool's own updater off) or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_updates: Option<bool>,
    /// Use this program for the command (checked with `--version` first);
    /// an empty path goes back to searching for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_path: Option<PathBuf>,
}

/// Stage of a tool operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolStage {
    /// Looking up the version and the file.
    Resolving,
    /// Transferring bytes.
    Downloading,
    /// Checking the checksum or signature.
    Verifying,
    /// Unpacking.
    Extracting,
    /// Running the tool's installer or package manager.
    Installing,
    /// Removing files or running the uninstaller.
    Removing,
    /// Checking the result.
    Checking,
}

/// One progress report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProgress {
    /// The tool.
    pub tool: ToolId,
    /// Stage.
    pub stage: ToolStage,
    /// Bytes done (downloads).
    #[serde(default)]
    pub done: u64,
    /// Total bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// The file, or the last line a command printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ToolProgress {
    /// A report for `stage` with nothing transferred.
    pub fn stage(tool: ToolId, stage: ToolStage) -> Self {
        Self {
            tool,
            stage,
            done: 0,
            total: None,
            detail: None,
        }
    }

    /// Fraction done (0..=1) when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done.min(total) as f64 / total as f64) as f32)
    }
}

/// Receives [`ToolProgress`] reports. Must not block. An operation asked to
/// stop ends with [`crate::Error::Cancelled`] after undoing what it started
/// (a running installer is ended; a partial download is kept for the next
/// attempt).
pub trait ToolProgressSink: Send + Sync {
    /// A new report.
    fn report(&self, progress: ToolProgress);

    /// The user asked to stop. Default: never.
    fn cancel_requested(&self) -> bool {
        false
    }
}

/// A sink that drops every report and never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoToolProgress;

impl ToolProgressSink for NoToolProgress {
    fn report(&self, _progress: ToolProgress) {}
}

/// A sink keeping the latest report and a cancel flag the UI reads and sets.
#[derive(Debug, Default)]
pub struct SharedToolProgress {
    latest: Mutex<Option<ToolProgress>>,
    cancel: AtomicBool,
}

impl SharedToolProgress {
    /// The latest report.
    pub fn latest(&self) -> Option<ToolProgress> {
        self.latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Ask the operation to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Whether a stop was asked.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

impl ToolProgressSink for SharedToolProgress {
    fn report(&self, progress: ToolProgress) {
        *self.latest.lock().unwrap_or_else(|p| p.into_inner()) = Some(progress);
    }

    fn cancel_requested(&self) -> bool {
        self.cancelled()
    }
}
