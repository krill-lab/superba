//! The switch transaction journal (`journal/switch/`) and the state lock
//! (`state.lock`), both owned by `superba-switch` (ARCHITECTURE §2).
//!
//! One JSON entry per execution records every component receipt as soon as it is
//! returned (config apply report, env edits, session migration receipt, remote
//! apply reports). Start-up recovery rolls back entries that crashed while
//! applying or undoing, and completes entries whose writes were all committed. Entries are
//! never deserialized into executable plans: they only hold receipts.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    ApplyReport, DataDir, EnvApplied, Error, MigrationReceipt, RemoteApplyReport, Result, TargetId,
    Timestamp,
};

/// Number of finished entries kept for history (remote "roll back this host").
pub const KEEP_FINISHED: usize = 32;

/// Holds `state.lock` (exclusive `flock` / `LockFileEx`) while alive.
#[derive(Debug)]
pub struct StateLock {
    _file: File,
    path: PathBuf,
}

impl StateLock {
    /// Try to take the lock without waiting; another Superba process (GUI or `spb`)
    /// holding it is reported as [`Error::Locked`].
    pub fn acquire(data_dir: &DataDir) -> Result<Self> {
        fs::create_dir_all(data_dir.root()).map_err(|e| Error::io(data_dir.root(), &e))?;
        let path = data_dir.state_lock();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| Error::io(&path, &e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(fs::TryLockError::WouldBlock) => Err(Error::Locked {
                what: path.display().to_string(),
            }),
            Err(fs::TryLockError::Error(e)) => Err(Error::io(&path, &e)),
        }
    }

    /// Lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Progress of a journaled execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalStatus {
    /// Writes in progress; a crash here means "roll everything back".
    Applying,
    /// Every write succeeded; restart / verification may still be running. A crash
    /// here keeps the new state.
    Committed,
    /// Undo started; recovery must finish it even if the writes had committed.
    RollingBack,
    /// Finished with the new state in place.
    Done,
    /// Local work finished, but at least one remote failed. Receipts of
    /// successful hosts remain available for rollback.
    Partial,
    /// Finished after undoing the writes.
    RolledBack,
}

/// One remote receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteReceipt {
    /// Target.
    pub target_id: TargetId,
    /// What was written.
    pub report: RemoteApplyReport,
    /// Already rolled back.
    #[serde(default)]
    pub rolled_back: bool,
}

/// Completed rollback steps, persisted so retries do not undo a component twice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RollbackProgress {
    /// Session migration was rolled back.
    pub migration: bool,
    /// Environment edits were reverted.
    pub env: bool,
    /// Configuration was restored.
    pub config: bool,
}

/// A journal entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Entry id (= plan id).
    pub id: String,
    /// When execution started.
    pub started_at: Timestamp,
    /// Last update.
    pub updated_at: Timestamp,
    /// Status.
    pub status: JournalStatus,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// Short English description (logs only).
    #[serde(default)]
    pub summary: String,
    /// Config receipt.
    #[serde(default)]
    pub config: Option<ApplyReport>,
    /// Env receipt.
    #[serde(default)]
    pub env: Option<EnvApplied>,
    /// Session migration receipt.
    #[serde(default)]
    pub migration: Option<MigrationReceipt>,
    /// Remote receipts.
    #[serde(default)]
    pub remotes: Vec<RemoteReceipt>,
    /// Failed hosts, including failures before a write returned a receipt.
    #[serde(default)]
    pub remote_failures: Vec<superba_core::RemoteOutcome>,
    /// Progress of an interrupted undo; absent in older journal files.
    #[serde(default)]
    pub rollback: RollbackProgress,
}

/// Reader/writer of `journal/switch/`.
#[derive(Clone, Debug)]
pub struct SwitchJournal {
    dir: PathBuf,
}

impl SwitchJournal {
    /// Journal of a data dir.
    pub fn new(data_dir: &DataDir) -> Self {
        Self {
            dir: data_dir.switch_journal_dir(),
        }
    }

    /// Directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Write (create or replace) an entry atomically.
    pub fn write(&self, entry: &JournalEntry) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, &e))?;
        let path = self.path(&entry.id);
        let tmp = self.dir.join(format!(".{}.tmp", entry.id));
        let bytes =
            serde_json::to_vec_pretty(entry).map_err(|e| Error::other(format!("journal: {e}")))?;
        {
            let mut f = File::create(&tmp).map_err(|e| Error::io(&tmp, &e))?;
            f.write_all(&bytes).map_err(|e| Error::io(&tmp, &e))?;
            f.sync_all().map_err(|e| Error::io(&tmp, &e))?;
        }
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, &e))
    }

    /// All entries, oldest first. Unreadable files are skipped (reported in the log).
    pub fn entries(&self) -> Result<Vec<JournalEntry>> {
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&self.dir, &e)),
        };
        let mut out = Vec::new();
        for item in rd.flatten() {
            let p = item.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice::<JournalEntry>(&b).ok())
            {
                Some(e) => out.push(e),
                None => tracing::warn!(path = %p.display(), "unreadable switch journal entry"),
            }
        }
        out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Entries whose apply, verification or rollback did not finish.
    pub fn unfinished(&self) -> Result<Vec<JournalEntry>> {
        Ok(self
            .entries()?
            .into_iter()
            .filter(|e| {
                matches!(
                    e.status,
                    JournalStatus::Applying | JournalStatus::Committed | JournalStatus::RollingBack
                )
            })
            .collect())
    }

    /// The newest finished entry with a live (not rolled back) receipt for `target`.
    pub fn last_remote(&self, target: &TargetId) -> Result<Option<(JournalEntry, RemoteReceipt)>> {
        Ok(self.entries()?.into_iter().rev().find_map(|e| {
            let r = e
                .remotes
                .iter()
                .find(|r| &r.target_id == target && !r.rolled_back)
                .cloned()?;
            matches!(e.status, JournalStatus::Done | JournalStatus::Partial).then_some((e, r))
        }))
    }

    /// Drop the oldest finished entries beyond [`KEEP_FINISHED`].
    pub fn prune(&self) -> Result<()> {
        let finished: Vec<JournalEntry> = self
            .entries()?
            .into_iter()
            .filter(|e| {
                matches!(
                    e.status,
                    JournalStatus::Done | JournalStatus::Partial | JournalStatus::RolledBack
                )
            })
            .collect();
        if finished.len() <= KEEP_FINISHED {
            return Ok(());
        }
        for e in &finished[..finished.len() - KEEP_FINISHED] {
            let p = self.path(&e.id);
            if let Err(err) = fs::remove_file(&p)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(Error::io(&p, &err));
            }
        }
        Ok(())
    }
}
