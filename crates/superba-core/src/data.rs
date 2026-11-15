//! Superba data location, disk usage and relocation progress.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Resolved location and the fixed pointer used by subsequent app and CLI starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataLocation {
    /// Active Superba data directory.
    pub directory: PathBuf,
    /// Fixed pointer file, outside the relocated data.
    pub pointer: PathBuf,
    /// Codex's own home; relocating Superba does not move it.
    pub codex_home: PathBuf,
}

/// One disjoint group of files inside Superba's data directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataCategoryUsage {
    /// Stable identifier, translated by the UI.
    pub category: String,
    /// Logical bytes, including SQLite WAL files; no symbolic links followed.
    pub bytes: u64,
    /// Regular files counted.
    pub files: u64,
}

/// Disk usage of Superba and, separately, Codex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataUsage {
    /// Current paths.
    pub location: DataLocation,
    /// Disjoint Superba categories.
    pub categories: Vec<DataCategoryUsage>,
    /// Sum of the Superba categories.
    pub total_bytes: u64,
    /// Codex sessions plus archived sessions (not part of `total_bytes`).
    pub codex_sessions_bytes: u64,
    /// Whole Codex home, including sessions (not part of `total_bytes`).
    pub codex_home_bytes: u64,
    /// SQLite home when Codex keeps it separately.
    pub codex_sqlite_bytes: u64,
    /// Files or links that could not safely be counted; the UI marks the result partial.
    pub skipped: u64,
}

/// Stage of a data relocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataMovePhase {
    /// Waiting for current Superba operations to finish.
    #[default]
    Waiting,
    /// Checking the directory and free space.
    Checking,
    /// Stopping the router before touching its files.
    Stopping,
    /// Copying and checking file checksums.
    Copying,
    /// Publishing the new directory and pointer.
    Switching,
    /// Starting the router with the same token and port.
    Restarting,
    /// Removing the old copies after success.
    Cleaning,
    /// Restoring the old location after a failure.
    RollingBack,
    /// Complete.
    Done,
}

/// Progress report; file names and contents are deliberately omitted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataMoveProgress {
    /// Current stage.
    pub phase: DataMovePhase,
    /// Bytes copied and verified.
    pub copied_bytes: u64,
    /// Planned number of bytes.
    pub total_bytes: u64,
}

/// Receives progress on a background worker. Returning false cancels before publication.
pub type DataMoveProgressSink = Arc<dyn Fn(DataMoveProgress) -> bool + Send + Sync>;

/// Shared localization key for a progress stage.
pub fn phase_key(phase: DataMovePhase) -> &'static str {
    match phase {
        DataMovePhase::Waiting => "data.phase.waiting",
        DataMovePhase::Checking => "data.phase.checking",
        DataMovePhase::Stopping => "data.phase.stopping",
        DataMovePhase::Copying => "data.phase.copying",
        DataMovePhase::Switching => "data.phase.switching",
        DataMovePhase::Restarting => "data.phase.restarting",
        DataMovePhase::Cleaning => "data.phase.cleaning",
        DataMovePhase::RollingBack => "data.phase.rolling_back",
        DataMovePhase::Done => "data.phase.done",
    }
}

/// Stable data-operation errors shared by the GUI and CLI.
pub fn error_text(key: &str) -> Option<crate::LocalizedText> {
    let fallback = match key {
        "data.path.absolute" => "Choose an absolute directory path without '..'.",
        "data.path.system" => "Choose a directory outside system and application folders.",
        "data.path.network" => {
            "Choose a local disk. Network and unsupported filesystems cannot hold Superba data."
        }
        "data.path.directory" => "The destination must be a directory.",
        "data.path.link" => {
            "Symbolic links, junctions and special files cannot be migrated safely."
        }
        "data.path.overlap" => {
            "The destination must not overlap the current data directory, the fixed locator, or Codex's home."
        }
        "data.path.not_empty" => "Choose a new or empty destination directory.",
        "data.path.space" => {
            "The destination does not have enough free space for the data and a safety reserve."
        }
        "data.pointer.invalid" => {
            "The data location pointer is invalid. Restore it before opening Superba; no empty replacement will be created."
        }
        "data.journal.invalid" => {
            "The data relocation journal is invalid. Keep both directories and repair the journal before continuing."
        }
        "data.busy" => {
            "Superba data is in use. Wait for ongoing operations or close other Superba windows and CLI commands, then retry."
        }
        "data.changed" => {
            "Data changed while it was being copied. The original location is kept; retry after stopping other writers."
        }
        "data.verify" => "A copied file did not pass verification. The original location is kept.",
        "data.pin" => {
            "Set the app's Codex version to Bundled before moving data, then select the desired version again afterwards."
        }
        "data.pending_switch" => {
            "An interrupted switch needs recovery. Restart Superba, let recovery finish, then move the data."
        }
        "data.router.stop" => "The router did not stop. The original data location is kept.",
        "data.codex.running" => {
            "Codex is running. Quit the ChatGPT app and close Codex in terminals and editors, then move the session data."
        }
        "data.codex.pending" => {
            "An interrupted move of Codex's session data needs recovery. Restart Superba (or run spb recover), then try again."
        }
        "data.codex.overlap" => {
            "Choose a folder outside the Codex home and outside Superba's data folder."
        }
        "data.codex.name" => {
            "The folder path must not contain a folder named sessions or archived_sessions."
        }
        "data.codex.unmanaged" => {
            "Codex's session folder is a link Superba did not create. Superba leaves it alone; change it by hand."
        }
        "data.codex.other" => {
            "Codex's session folder is not a folder. Superba leaves it alone; check it by hand."
        }
        "data.codex.not_moved" => "Codex's session data is already in the Codex home.",
        "data.codex.same" => "Codex's session data is already in this folder.",
        "data.codex.unreachable" => {
            "The folder holding Codex's session data cannot be reached. Connect the drive again, then retry."
        }
        "data.codex.in_use" => {
            "A program still has Codex's session files open. Close Codex in the app, terminals and editors, then retry. Nothing was changed."
        }
        "data.codex.links" => {
            "This system does not allow Superba to create the link Codex needs. Nothing was changed."
        }
        "data.codex.journal" => {
            "The record of an interrupted move of Codex's session data is invalid. Both copies are kept; check them before retrying."
        }
        _ => return None,
    };
    Some(crate::LocalizedText::new(key, fallback))
}

/// A completed move; old copies may remain when the OS refuses their deletion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataMoveReport {
    /// Active location after the move.
    pub location: DataLocation,
    /// Previous data directory.
    pub previous: PathBuf,
    /// Bytes copied and verified.
    pub copied_bytes: u64,
    /// Old files could not all be removed; the source remains safe to inspect.
    pub source_cleanup_pending: bool,
}

/// Result of enforcing retention and deleting disposable caches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataCleanupReport {
    /// Logical bytes reclaimed (SQLite and caches).
    pub reclaimed_bytes: u64,
    /// Fresh usage after cleanup.
    pub usage: DataUsage,
}

/// The directories of a Codex home that "Codex data location" can place on
/// another disk (GitHub issue 15): the conversation rollouts. Both move
/// together, because Codex archives a conversation with a plain rename from
/// `sessions/` to `archived_sessions/`, which fails across volumes.
pub const CODEX_DATA_DIRS: [&str; 2] = ["sessions", "archived_sessions"];

/// How one of [`CODEX_DATA_DIRS`] is placed in the Codex home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodexDirPlacement {
    /// Not created yet (Codex creates it on first use).
    Missing,
    /// An ordinary directory inside the Codex home.
    InHome,
    /// A directory junction (Windows) or symbolic link to `target`.
    Linked {
        /// Where the data is (the link target as stored).
        target: PathBuf,
        /// Superba created this link (its move record names it).
        managed: bool,
        /// The target exists and is a directory (false: the drive is
        /// unplugged or the folder was removed).
        reachable: bool,
    },
    /// Something else (a file, a link to a file): never touched.
    Other,
}

/// One Codex data directory and its size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexDataDir {
    /// `sessions` or `archived_sessions`.
    pub name: String,
    /// The path Codex uses (`<CODEX_HOME>/<name>`).
    pub path: PathBuf,
    /// Where the data actually lives.
    pub placement: CodexDirPlacement,
    /// Logical bytes of the files (through the link when it is reachable).
    pub bytes: u64,
    /// Regular files counted.
    pub files: u64,
}

/// Where Codex's session data is, as Superba sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexDataLocation {
    /// The local Codex home (`CODEX_HOME` or `~/.codex`); never changed.
    pub codex_home: PathBuf,
    /// The folder Superba moved the data to (`None`: in the Codex home).
    pub directory: Option<PathBuf>,
    /// Each of [`CODEX_DATA_DIRS`].
    pub dirs: Vec<CodexDataDir>,
    /// An interrupted move waits for recovery (finished at the next start).
    pub pending: bool,
    /// Old copies a finished move could not remove (safe to inspect).
    pub leftovers: Vec<PathBuf>,
}

impl CodexDataLocation {
    /// True when a link Superba created points to a folder that cannot be
    /// reached now (e.g. the drive is unplugged).
    pub fn unreachable(&self) -> bool {
        self.dirs.iter().any(|d| {
            matches!(
                d.placement,
                CodexDirPlacement::Linked {
                    reachable: false,
                    ..
                }
            )
        })
    }
}

/// One directory a Codex data move copies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexDataMoveItem {
    /// `sessions` or `archived_sessions`.
    pub name: String,
    /// Where the data is now.
    pub from: PathBuf,
    /// Where it will be.
    pub to: PathBuf,
    /// Bytes to copy.
    pub bytes: u64,
    /// Files to copy.
    pub files: u64,
}

/// Preview of a Codex data move (no side effects besides a write probe).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexDataPlan {
    /// The current placement.
    pub location: CodexDataLocation,
    /// The new folder; `None` moves the data back into the Codex home.
    pub destination: Option<PathBuf>,
    /// What is copied.
    pub moves: Vec<CodexDataMoveItem>,
    /// Sum of the items' bytes.
    pub total_bytes: u64,
    /// Free bytes on the destination volume, when known.
    pub available_bytes: Option<u64>,
    /// Reasons the move cannot run now (empty: it can).
    pub blockers: Vec<crate::LocalizedText>,
    /// Things to know before moving.
    pub notes: Vec<crate::LocalizedText>,
}

impl CodexDataPlan {
    /// The move can run now.
    pub fn ready(&self) -> bool {
        self.blockers.is_empty() && !self.moves.is_empty()
    }
}

/// A finished Codex data move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexDataMoveReport {
    /// The placement after the move.
    pub location: CodexDataLocation,
    /// Bytes copied and verified.
    pub copied_bytes: u64,
    /// Files copied and verified.
    pub files: u64,
    /// Old copies that could not be removed (the new place is active).
    pub leftovers: Vec<PathBuf>,
}
