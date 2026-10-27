//! # superba-sessions
//!
//! Session provider scan and migration (plan section 10). [`CodexSessionMigrator`]
//! implements [`SessionMigrator`]: the desktop app lists only the sessions whose
//! provider id equals the current one, so sessions recorded under a legacy id
//! (`krill`, `codex`, ...) are moved to `openai` on takeover and can be moved back
//! on restore.
//!
//! Where providers live (Codex 0.162):
//! - every SQLite database under `CODEX_HOME`, `CODEX_SQLITE_HOME` and the
//!   `sqlite_home` config key (and their `sqlite/` subdirectories) with a table
//!   that has `model_provider` and a thread id column (`state_5.sqlite`
//!   `threads`, the app's `local_thread_catalog`, ...);
//! - the `session_meta` record of each rollout (`sessions/**/rollout-*.jsonl`,
//!   `archived_sessions/`, also `.jsonl.zst`).
//!
//! `thread_history_*.sqlite` stores byte offsets into the rollouts; when a
//! provider id changes length, those offsets are shifted with the rewrite.
//!
//! Safety:
//! - writes refuse while a Codex process uses the home (process scan), unless the
//!   caller passes a [`ConsumersStopped`] token (`*_stopped` methods); Codex's
//!   thread writer locks, the migrate lock (`CodexHome::migrate_lock`) and locks of
//!   similar tools are always checked, and Codex's writer coordination lock is
//!   held for the whole write so no Codex can start writing a session meanwhile
//!   (a handful of files is open at once, whatever the session count);
//! - backups: rollout pre-images and SQLite online backups (through the WAL) with
//!   `integrity_check`, in `DataDir::migrate_backup_dir(id)`;
//! - rollouts: only the provider token of `session_meta` lines changes; size and
//!   mtime are checked before replacing; the original mtime is restored; `.zst`
//!   files are re-compressed and verified;
//! - SQLite: one `BEGIN IMMEDIATE` transaction per database with a busy timeout;
//!   every changed cell is recorded in the ledger before the commit;
//! - rollback and crash recovery restore exactly the items still in their migrated
//!   state; a session changed since is skipped as a whole;
//! - databases on WSL/UNC shares are skipped with a warning (no locking there).

#![forbid(unsafe_code)]

pub mod browse;
pub mod claude_usage;
mod engine;
pub mod export;
mod guard;
mod inventory;
mod jsonscan;
mod ledger;
mod progress;
mod rollout;
mod sqlite;
mod text;
pub mod titles;
pub mod usage;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use superba_core::SessionProgressSink;
use superba_core::codex::{LEGACY_KRILL_PROVIDER, PROVIDER_OPENAI};
use superba_core::{
    Clock, CodexHome, ConsumersStopped, DataDir, Error, Locale, MigrationPlan, MigrationReceipt,
    MigrationRequest, MigrationSelector, Result, RollbackReport, SessionMigrator, SessionReport,
    SystemClock,
};

use crate::engine::Engine;

/// Provider ids that Superba migrates to `openai` on takeover: the legacy Krill
/// tutorial id and the generic `codex` id some guides used.
pub const LEGACY_PROVIDER_IDS: &[&str] = &[LEGACY_KRILL_PROVIDER, "codex"];

/// Raw TOML source of this crate's message catalog (`sessions.*` keys), for
/// `superba_core::Catalog::add_toml`.
pub fn catalog_source(locale: Locale) -> &'static str {
    text::source(locale)
}

/// The takeover request: every session under a [`LEGACY_PROVIDER_IDS`] id found in
/// `report` moves to `openai` (archived ones included). `None` when there are none.
pub fn takeover_request(report: &SessionReport) -> Option<MigrationRequest> {
    let selectors: Vec<MigrationSelector> = LEGACY_PROVIDER_IDS
        .iter()
        .filter(|id| report.by_provider.get(**id).is_some_and(|p| p.distinct > 0))
        .map(|id| MigrationSelector::Provider {
            provider: (*id).to_owned(),
        })
        .collect();
    (!selectors.is_empty()).then(|| MigrationRequest {
        to_provider: PROVIDER_OPENAI.to_owned(),
        selectors,
        include_archived: true,
        exclude: Vec::new(),
    })
}

/// Test hook: where an injected failure happens during `migrate`.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    /// After backups and the pending ledger were written.
    AfterLedger,
    /// After the n-th rollout was replaced.
    AfterRollout(usize),
    /// After the n-th database was committed.
    AfterDatabase(usize),
    /// After verification, before the ledger is completed.
    BeforeComplete,
}

/// Test hook: inject a failure; with `crash`, return without the automatic
/// rollback (as if the process died), leaving a pending ledger for `recover`.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultPlan {
    /// Where to fail.
    pub fail_at: Option<FaultPoint>,
    /// Skip the automatic rollback.
    pub crash: bool,
}

/// [`SessionMigrator`] for local Codex homes.
#[derive(Clone)]
pub struct CodexSessionMigrator {
    data_dir: DataDir,
    home_dir: Option<PathBuf>,
    clock: Arc<dyn Clock>,
    faults: FaultPlan,
}

impl std::fmt::Debug for CodexSessionMigrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexSessionMigrator")
            .field("data_dir", &self.data_dir)
            .field("home_dir", &self.home_dir)
            .finish_non_exhaustive()
    }
}

impl CodexSessionMigrator {
    /// A migrator that keeps backups and ledgers under `data_dir`.
    pub fn new(data_dir: DataDir) -> Self {
        Self {
            data_dir,
            home_dir: None,
            clock: Arc::new(SystemClock::new()),
            faults: FaultPlan::default(),
        }
    }

    /// The user's home directory, used to expand `~` in `sqlite_home`
    /// (`LocalContext::home_dir`). Without it such values are reported and skipped.
    pub fn with_home_dir(mut self, home_dir: impl Into<PathBuf>) -> Self {
        self.home_dir = Some(home_dir.into());
        self
    }

    /// Use `clock` for timestamps and token freshness (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Test hook: inject a failure into `migrate`.
    #[doc(hidden)]
    pub fn with_faults(mut self, faults: FaultPlan) -> Self {
        self.faults = faults;
        self
    }

    /// The data dir backups go to.
    pub fn data_dir(&self) -> &DataDir {
        &self.data_dir
    }

    fn engine(&self) -> Engine<'_> {
        Engine {
            data_dir: &self.data_dir,
            home_dir: self.home_dir.as_deref(),
            clock: self.clock.as_ref(),
            faults: &self.faults,
            progress: &progress::NONE,
        }
    }

    /// Run `f` on the blocking pool (file and SQLite work).
    async fn blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&CodexSessionMigrator) -> Result<T> + Send + 'static,
    {
        let me = self.clone();
        tokio::task::spawn_blocking(move || f(&me))
            .await
            .map_err(|e| Error::other(format!("session migration task failed: {e}")))?
    }
}

#[async_trait]
impl SessionMigrator for CodexSessionMigrator {
    async fn scan(&self, home: &CodexHome) -> Result<SessionReport> {
        let home = home.clone();
        self.blocking(move |m| Ok(m.engine().scan(&home))).await
    }

    async fn plan(&self, home: &CodexHome, request: &MigrationRequest) -> Result<MigrationPlan> {
        let (home, request) = (home.clone(), request.clone());
        self.blocking(move |m| m.engine().plan(&home, &request))
            .await
    }

    async fn migrate(&self, home: &CodexHome, plan: &MigrationPlan) -> Result<MigrationReceipt> {
        let (home, plan) = (home.clone(), plan.clone());
        self.blocking(move |m| m.engine().migrate(&home, &plan, None))
            .await
    }

    async fn rollback(&self, home: &CodexHome, migration_id: &str) -> Result<RollbackReport> {
        let (home, id) = (home.clone(), migration_id.to_owned());
        self.blocking(move |m| m.engine().rollback(&home, &id, None))
            .await
    }

    async fn history(&self, home: &CodexHome) -> Result<Vec<MigrationReceipt>> {
        let home = home.clone();
        self.blocking(move |m| Ok(m.engine().history(&home))).await
    }

    async fn recover(&self, home: &CodexHome) -> Result<RollbackReport> {
        let home = home.clone();
        self.blocking(move |m| m.engine().recover(&home, None))
            .await
    }

    async fn migrate_stopped(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        stopped: &ConsumersStopped,
    ) -> Result<MigrationReceipt> {
        let (home, plan, token) = (home.clone(), plan.clone(), stopped.clone());
        self.blocking(move |m| m.engine().migrate(&home, &plan, Some(&token)))
            .await
    }

    async fn rollback_stopped(
        &self,
        home: &CodexHome,
        migration_id: &str,
        stopped: &ConsumersStopped,
    ) -> Result<RollbackReport> {
        let (home, id, token) = (home.clone(), migration_id.to_owned(), stopped.clone());
        self.blocking(move |m| m.engine().rollback(&home, &id, Some(&token)))
            .await
    }

    async fn recover_stopped(
        &self,
        home: &CodexHome,
        stopped: &ConsumersStopped,
    ) -> Result<RollbackReport> {
        let (home, token) = (home.clone(), stopped.clone());
        self.blocking(move |m| m.engine().recover(&home, Some(&token)))
            .await
    }

    async fn migrate_stopped_with_progress(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        stopped: &ConsumersStopped,
        sink: Arc<dyn SessionProgressSink>,
    ) -> Result<MigrationReceipt> {
        let (home, plan, token) = (home.clone(), plan.clone(), stopped.clone());
        self.blocking(move |m| {
            let progress = progress::Progress::new(sink, Default::default());
            m.engine_with(&progress).migrate(&home, &plan, Some(&token))
        })
        .await
    }

    async fn rollback_stopped_with_progress(
        &self,
        home: &CodexHome,
        migration_id: &str,
        stopped: &ConsumersStopped,
        sink: Arc<dyn SessionProgressSink>,
    ) -> Result<RollbackReport> {
        let (home, id, token) = (home.clone(), migration_id.to_owned(), stopped.clone());
        self.blocking(move |m| {
            let titles = progress::ledger_titles(&m.data_dir, m.home_dir.as_deref(), &home, &id);
            let progress = progress::Progress::new(sink, titles);
            m.engine_with(&progress).rollback(&home, &id, Some(&token))
        })
        .await
    }

    async fn rollback_many_stopped_with_progress(
        &self,
        home: &CodexHome,
        migration_ids: &[String],
        stopped: &ConsumersStopped,
        sink: Arc<dyn SessionProgressSink>,
    ) -> Result<RollbackReport> {
        let (home, ids, token) = (home.clone(), migration_ids.to_vec(), stopped.clone());
        self.blocking(move |m| {
            let mut titles = std::collections::BTreeMap::new();
            for id in &ids {
                titles.extend(progress::ledger_titles(
                    &m.data_dir,
                    m.home_dir.as_deref(),
                    &home,
                    id,
                ));
            }
            let progress = progress::Progress::new(sink, titles);
            m.engine_with(&progress)
                .rollback_many(&home, &ids, Some(&token))
        })
        .await
    }
}
