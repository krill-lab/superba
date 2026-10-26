//! The migration ledger: `backups/migrate-<id>/ledger.json`.
//!
//! Written before the first change and updated before every SQLite commit, it
//! records each rollout edit (with before/after hashes) and each changed SQLite
//! cell (key, before, after). Rollback and crash recovery restore exactly the items
//! whose current state still equals the recorded "after" state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use superba_core::{
    DataDir, Error, MigrationReceipt, MigrationRequest, Result, RollbackReport, Timestamp,
};

use crate::rollout::Edit;
use crate::sqlite::CellChange;
use crate::util::{normalize_dir, write_json_atomic};

/// Ledger format version.
pub(crate) const LEDGER_VERSION: u32 = 1;
/// File name inside the backup directory.
pub(crate) const LEDGER_FILE: &str = "ledger.json";
/// Prefix of migration backup directories (`DataDir::migrate_backup_dir`).
const DIR_PREFIX: &str = "migrate-";

/// Lifecycle of a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LedgerState {
    /// Writes may be in progress (or were interrupted by a crash).
    Pending,
    /// Migration finished and verified.
    Completed,
    /// A rollback started (user request or failure) and has not finished.
    RollingBack,
    /// Rolled back.
    RolledBack,
}

impl LedgerState {
    /// Needs [`crate::CodexSessionMigrator`]'s `recover`.
    pub(crate) fn needs_recovery(self) -> bool {
        matches!(self, LedgerState::Pending | LedgerState::RollingBack)
    }
}

/// One rewritten rollout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RolloutEntry {
    pub path: PathBuf,
    pub thread_id: String,
    pub archived: bool,
    pub compressed: bool,
    /// Provider ids replaced in this file.
    pub sources: Vec<String>,
    pub to: String,
    /// Edits in the uncompressed content.
    pub edits: Vec<Edit>,
    /// SHA-256 of the bytes on disk before / after.
    pub raw_before_sha256: String,
    pub raw_after_sha256: String,
    /// SHA-256 of the uncompressed content before / after.
    pub plain_before_sha256: String,
    pub plain_after_sha256: String,
    pub len_before: u64,
    pub len_after: u64,
    /// Original modification time (restored after every write).
    pub mtime: Option<SystemTime>,
    /// Pre-image inside the backup directory (relative), until pruned.
    pub backup: Option<String>,
}

/// One database touched by the migration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DbEntry {
    pub path: PathBuf,
    /// Online backup inside the backup directory (relative), until pruned.
    pub backup: Option<String>,
    /// Changed cells (recorded before the commit).
    #[serde(default)]
    pub changes: Vec<CellChange>,
}

/// The ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Ledger {
    pub version: u32,
    pub id: String,
    pub codex_home: PathBuf,
    pub state: LedgerState,
    pub created_at: Timestamp,
    #[serde(default)]
    pub completed_at: Option<Timestamp>,
    /// A per-home order assigned under the migration lock, independent of wall
    /// clock resolution. Zero identifies older ledgers without this field.
    #[serde(default)]
    pub completion_order: u64,
    #[serde(default)]
    pub rolled_back_at: Option<Timestamp>,
    pub request: MigrationRequest,
    pub to_provider: String,
    pub by_source_provider: BTreeMap<String, u32>,
    pub rollouts: Vec<RolloutEntry>,
    pub databases: Vec<DbEntry>,
    /// Last rollback result.
    #[serde(default)]
    pub rollback: Option<RollbackReport>,
    /// Pre-images and database backups were removed by retention.
    #[serde(default)]
    pub pruned: bool,
}

impl Ledger {
    /// Provider rows changed.
    pub(crate) fn thread_rows(&self) -> u32 {
        self.databases
            .iter()
            .flat_map(|d| d.changes.iter())
            .filter(|c| c.is_provider())
            .count() as u32
    }

    /// Receipt for `history` / `migrate`.
    pub(crate) fn receipt(&self, backup_dir: &Path) -> MigrationReceipt {
        MigrationReceipt {
            id: self.id.clone(),
            codex_home: self.codex_home.clone(),
            migrated_at: self.completed_at.unwrap_or(self.created_at),
            to_provider: self.to_provider.clone(),
            by_source_provider: self.by_source_provider.clone(),
            threads: self.thread_rows(),
            rollouts: self.rollouts.len() as u32,
            backup_dir: backup_dir.to_path_buf(),
            ledger_path: backup_dir.join(LEDGER_FILE),
            rolled_back: self.state == LedgerState::RolledBack,
        }
    }

    /// Belongs to `home`.
    pub(crate) fn is_for(&self, home_root: &Path) -> bool {
        normalize_dir(&self.codex_home) == normalize_dir(home_root)
    }
}

/// A ledger together with its directory.
#[derive(Clone, Debug)]
pub(crate) struct Stored {
    pub dir: PathBuf,
    pub ledger: Ledger,
}

impl Stored {
    /// `ledger.json`.
    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(LEDGER_FILE)
    }

    /// Persist atomically.
    pub(crate) fn save(&self) -> Result<()> {
        write_json_atomic(&self.path(), &self.ledger)
    }
}

/// Migration ids name directories: ASCII letters, digits, `-` and `_` only.
pub(crate) fn validate_id(migration_id: &str) -> Result<()> {
    if migration_id.is_empty()
        || migration_id.len() > 128
        || migration_id
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(Error::invalid(format!(
            "invalid migration id {migration_id:?}"
        )));
    }
    Ok(())
}

/// Load the ledger of `migration_id`.
pub(crate) fn load(data_dir: &DataDir, migration_id: &str) -> Result<Stored> {
    validate_id(migration_id)?;
    let dir = data_dir.migrate_backup_dir(migration_id);
    let path = dir.join(LEDGER_FILE);
    let bytes = fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound {
                what: format!("session migration {migration_id}"),
            }
        } else {
            Error::io(&path, &e)
        }
    })?;
    let ledger: Ledger =
        serde_json::from_slice(&bytes).map_err(|e| Error::parse(path.display().to_string(), e))?;
    if ledger.version > LEDGER_VERSION {
        return Err(Error::Unsupported {
            what: format!(
                "migration ledger version {} (this Superba understands {LEDGER_VERSION})",
                ledger.version
            ),
        });
    }
    Ok(Stored { dir, ledger })
}

/// A ledger this Superba cannot use (newer version, damaged file).
#[derive(Debug)]
pub(crate) struct Unusable {
    /// Its `codex_home`, when the file is at least JSON.
    pub codex_home: Option<PathBuf>,
    /// Its state says (or may say) that recovery is needed.
    pub needs_recovery: bool,
    pub error: Error,
}

/// Every readable ledger in the data dir (any home).
pub(crate) fn list(data_dir: &DataDir) -> Vec<Stored> {
    scan(data_dir).0
}

/// Ledgers of `home_root` that need recovery but cannot be read here: recovery
/// and new writes must stop instead of ignoring them.
pub(crate) fn unusable_for(data_dir: &DataDir, home_root: &Path) -> Option<Error> {
    scan(data_dir).1.into_iter().find_map(|u| {
        let ours = u
            .codex_home
            .as_ref()
            .is_some_and(|h| normalize_dir(h) == normalize_dir(home_root));
        (ours && u.needs_recovery).then_some(u.error)
    })
}

/// Readable ledgers (newest first) and unusable ones.
pub(crate) fn scan(data_dir: &DataDir) -> (Vec<Stored>, Vec<Unusable>) {
    let Ok(entries) = fs::read_dir(data_dir.backups_dir()) else {
        return (Vec::new(), Vec::new());
    };
    let mut out = Vec::new();
    let mut unusable = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix(DIR_PREFIX) else {
            continue;
        };
        match load(data_dir, id) {
            Ok(stored) => out.push(stored),
            Err(Error::NotFound { .. }) => {}
            Err(error) => {
                let loose: Option<serde_json::Value> = fs::read(entry.path().join(LEDGER_FILE))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok());
                let codex_home = loose
                    .as_ref()
                    .and_then(|v| v.get("codex_home")?.as_str().map(PathBuf::from));
                let needs_recovery = loose
                    .as_ref()
                    .and_then(|v| v.get("state")?.as_str().map(str::to_owned))
                    .is_none_or(|s| s != "completed" && s != "rolled_back");
                unusable.push(Unusable {
                    codex_home,
                    needs_recovery,
                    error,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        b.ledger
            .created_at
            .cmp(&a.ledger.created_at)
            .then_with(|| b.ledger.id.cmp(&a.ledger.id))
    });
    (out, unusable)
}

/// Keep the pre-images of the newest `keep` finished migrations of `home_root`;
/// older ones lose their `rollouts/` and `sqlite/` directories (their ledgers stay,
/// so rollback still works through the recorded edits).
pub(crate) fn prune(data_dir: &DataDir, home_root: &Path, keep: usize) {
    let mut finished: Vec<Stored> = list(data_dir)
        .into_iter()
        .filter(|s| s.ledger.is_for(home_root) && !s.ledger.state.needs_recovery())
        .collect();
    finished.sort_by_key(|s| std::cmp::Reverse(s.ledger.created_at));
    for mut stored in finished.into_iter().skip(keep) {
        if stored.ledger.pruned {
            continue;
        }
        for sub in ["rollouts", "sqlite"] {
            let _ = fs::remove_dir_all(stored.dir.join(sub));
        }
        stored.ledger.pruned = true;
        for r in &mut stored.ledger.rollouts {
            r.backup = None;
        }
        for d in &mut stored.ledger.databases {
            d.backup = None;
        }
        let _ = stored.save();
    }
}
