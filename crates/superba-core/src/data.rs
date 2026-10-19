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
