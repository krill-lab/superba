//! Session (thread) provider scan and migration types ([`crate::SessionMigrator`]).
//!
//! Facts: the app lists only threads whose `model_provider` equals the current
//! provider id. The provider is stored in `state_5.sqlite` `threads.model_provider`
//! and in each rollout's `session_meta.model_provider` (`sessions/**/rollout-*.jsonl`,
//! `archived_sessions/`, possibly `.jsonl.zst`). Migration requires every Codex
//! process of the home to be stopped (hence a full app restart).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::Notice;
use crate::ids::TargetId;
use crate::paths::CodexHome;
use crate::time::Timestamp;

/// Counts for one provider id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSessions {
    /// Rows in the sqlite `threads` table.
    pub threads: u32,
    /// Live rollout files (`sessions/`).
    pub rollouts: u32,
    /// Archived rollout files (`archived_sessions/`).
    pub archived: u32,
    /// Of the rollouts above, how many are `.zst` compressed.
    pub compressed: u32,
    /// Distinct sessions across all storage forms (union of thread ids from sqlite,
    /// rollouts and archived rollouts). This is the user-facing count.
    #[serde(default)]
    pub distinct: u32,
}

/// Provider distribution of a Codex home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReport {
    /// Codex home root.
    pub codex_home: PathBuf,
    /// When scanned.
    pub scanned_at: Timestamp,
    /// sqlite databases found.
    pub sqlite_paths: Vec<PathBuf>,
    /// Counts per provider id (`openai`, `krill`, ...).
    pub by_provider: BTreeMap<String, ProviderSessions>,
    /// Threads whose sqlite row and rollout disagree on the provider.
    pub inconsistent: u32,
    /// Problems (unreadable files, locked database ...).
    #[serde(default)]
    pub warnings: Vec<Notice>,
}

impl SessionReport {
    /// Thread count per provider (the baseline's `session_distribution`).
    pub fn distribution(&self) -> BTreeMap<String, u32> {
        self.by_provider
            .iter()
            .map(|(k, v)| (k.clone(), v.distinct))
            .collect()
    }

    /// Sessions not under `provider` (hidden in the app when `provider` is current).
    pub fn hidden_under(&self, provider: &str) -> u32 {
        self.distribution()
            .iter()
            .filter(|(k, _)| k.as_str() != provider)
            .map(|(_, v)| v)
            .sum()
    }
}

/// Which sessions to move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MigrationSelector {
    /// Every session whose provider is this id (takeover: `krill` -> `openai`).
    Provider {
        /// Source provider id.
        provider: String,
    },
    /// Undo a previous migration using its ledger (reverse migration, plan 10.4).
    Ledger {
        /// Migration id.
        migration_id: String,
    },
    /// Explicit thread ids (user-adjusted selection in the preview).
    ThreadIds {
        /// Thread / session ids.
        ids: Vec<String>,
    },
    /// Sessions of `provider` created in `[from, to)` (generation-based reverse
    /// migration of sessions created while Superba was active).
    CreatedBetween {
        /// Source provider id.
        provider: String,
        /// Start (inclusive).
        from: Timestamp,
        /// End (exclusive); `None` = now.
        to: Option<Timestamp>,
    },
}

/// A migration request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRequest {
    /// Destination provider id.
    pub to_provider: String,
    /// Sessions to include (union).
    pub selectors: Vec<MigrationSelector>,
    /// Include `archived_sessions/`.
    pub include_archived: bool,
    /// Thread ids to leave out (unticked in the preview).
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One session in a migration preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationItem {
    /// Thread / session id.
    pub thread_id: String,
    /// Title, if known.
    #[serde(default)]
    pub title: Option<String>,
    /// Current provider.
    pub source_provider: String,
    /// Creation time, if known.
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    /// Lives in `archived_sessions/`.
    #[serde(default)]
    pub archived: bool,
}

/// A computed migration (no writes yet).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// Migration id (also names the backup dir and ledger).
    pub id: String,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// Request.
    pub request: MigrationRequest,
    /// sqlite rows to update.
    pub thread_count: u32,
    /// Rollout files to rewrite.
    pub rollout_count: u32,
    /// Source provider -> count.
    pub by_source_provider: BTreeMap<String, u32>,
    /// Backup directory that will be created.
    pub backup_dir: PathBuf,
    /// Bytes to back up (for a disk-space check).
    pub backup_bytes: u64,
    /// Sessions included (for the adjustable preview).
    #[serde(default)]
    pub items: Vec<MigrationItem>,
}

/// Ledger summary of a completed migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReceipt {
    /// Migration id.
    pub id: String,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// When.
    pub migrated_at: Timestamp,
    /// Destination provider id.
    pub to_provider: String,
    /// Source provider -> count.
    pub by_source_provider: BTreeMap<String, u32>,
    /// sqlite rows updated.
    pub threads: u32,
    /// Rollout files rewritten.
    pub rollouts: u32,
    /// Backup directory.
    pub backup_dir: PathBuf,
    /// Ledger file (per-item before/after hashes).
    pub ledger_path: PathBuf,
    /// True once rolled back.
    #[serde(default)]
    pub rolled_back: bool,
}

/// Result of a rollback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReport {
    /// sqlite rows restored.
    pub restored_threads: u32,
    /// Rollout files restored.
    pub restored_rollouts: u32,
    /// Items skipped because they changed after the migration.
    pub skipped_changed: u32,
}

/// The caller's statement that it listed and stopped every Codex process using a
/// Codex home (desktop app, app-server daemon, TUI/exec, IDE app-servers), for
/// example `superba-switch` after `CloseConsumers` with `AppController::processes`.
///
/// Passed to [`crate::SessionMigrator::migrate_stopped`] and its siblings, it lets the
/// migrator skip its own process detection (which can be inconclusive, e.g. when
/// another process's environment is unreadable). It never disables the other
/// safety checks (Codex thread writer locks, the migrate lock, SQLite busy checks).
/// Implementations reject tokens for another home or older than a few minutes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumersStopped {
    /// Root of the Codex home the check covered.
    pub codex_home: PathBuf,
    /// When the processes were checked.
    pub checked_at: Timestamp,
}

impl ConsumersStopped {
    /// A token for `home`, checked at `checked_at`.
    pub fn new(home: &CodexHome, checked_at: Timestamp) -> Self {
        Self {
            codex_home: home.root().to_path_buf(),
            checked_at,
        }
    }
}

/// Where a session name came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TitleSource {
    /// `state_5.sqlite` `threads.title`.
    Title,
    /// The thread's first user message.
    FirstUserMessage,
    /// Not found: the short key.
    #[default]
    ShortKey,
}

/// A session name for a router log key ([`crate::SwitchService::session_titles`]).
/// Titles are looked up at display time and never stored in the router database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitle {
    /// Thread id the key resolved to (`<thread id>` or `<source>:<parent id>`
    /// keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Name to show.
    pub title: String,
    /// Where it came from.
    #[serde(default)]
    pub source: TitleSource,
}

/// Sessions of one host (Sessions page host switcher,
/// [`crate::SwitchService::host_sessions`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSessions {
    /// Host (`local` for this computer).
    pub target_id: TargetId,
    /// Provider distribution.
    pub report: SessionReport,
    /// Migration history (this computer only).
    #[serde(default)]
    pub history: Vec<MigrationReceipt>,
    /// Remote hosts are scanned read-only in 0.3.
    #[serde(default)]
    pub read_only: bool,
    /// Command the user can run on the host to migrate (`spb sessions migrate
    /// --from <provider>`), for remote hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrate_command: Option<String>,
}
