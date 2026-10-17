//! Local per-host data under `DataDir::remote_dir()/<target>/`:
//!
//! - `host-state.json`: remote `CODEX_HOME` / `$HOME` seen last, generation counter,
//!   the owned key values Superba wrote last (drift detection) and the host's own
//!   values of keys Superba took over after the baseline was captured.
//! - `baseline.json`: the host's pre-takeover baseline (from the pure
//!   `ConfigEngine::capture_baseline`), written once before the first remote write.
//!   Renders see it extended with the recorded own values (`HostState::user_values`).
//! - `journal/<apply-id>.json`: one record per apply (pending -> applied /
//!   conflict / failed / rolled back) with what rollback needs to restore local state.
//!
//! Nothing here holds secrets: remote file pre-images stay on the host (0600 under
//! `umask 077`); ChatGPT logins go to the config engine's encrypted stash.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    Baseline, DataDir, Error, KeySnapshot, RemoteApplyReport, Result, TargetId, Timestamp,
};

use crate::util::target_dir_name;

/// Persistent per-host state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    /// Schema version.
    #[serde(default)]
    pub schema: u32,
    /// Target.
    #[serde(default)]
    pub target_id: Option<TargetId>,
    /// Remote `CODEX_HOME` seen by the last probe.
    #[serde(default)]
    pub codex_home: Option<String>,
    /// Remote `$HOME` seen by the last probe.
    #[serde(default)]
    pub home: Option<String>,
    /// Incremented by every successful apply.
    #[serde(default)]
    pub generation: u64,
    /// Owned key values Superba wrote last.
    #[serde(default)]
    pub last_written: Vec<KeySnapshot>,
    /// The host's own value (`None` = absent) of each key outside the captured
    /// baseline (model keys, Codex settings) from just before Superba last took
    /// it over: what the key returns to when Superba releases it.
    #[serde(default)]
    pub user_values: Vec<KeySnapshot>,
    /// Last update.
    #[serde(default)]
    pub updated_at: Timestamp,
}

/// Status of a journal record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalStatus {
    /// Script about to run (a crash leaves this; the remote may be half written).
    Pending,
    /// Written.
    Applied,
    /// Precondition failed; nothing written.
    Conflict,
    /// The script failed; it restored what it had replaced.
    Failed,
    /// Rolled back.
    RolledBack,
}

/// One apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Apply id (also the remote backup directory name).
    pub id: String,
    /// Status.
    pub status: JournalStatus,
    /// Created.
    pub created_at: Timestamp,
    /// Remote paths the apply writes.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Remote backup directory.
    #[serde(default)]
    pub backup_dir: Option<String>,
    /// The report (once applied).
    #[serde(default)]
    pub report: Option<RemoteApplyReport>,
    /// `last_written` before the apply (restored by rollback).
    #[serde(default)]
    pub prev_last_written: Vec<KeySnapshot>,
    /// Generation before the apply.
    #[serde(default)]
    pub prev_generation: u64,
    /// Failure detail.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Storage of one host.
#[derive(Clone, Debug)]
pub struct HostStore {
    dir: PathBuf,
}

impl HostStore {
    /// Store for `target` under `data_dir`.
    pub fn new(data_dir: &DataDir, target: &TargetId) -> Self {
        Self {
            dir: data_dir.remote_dir().join(target_dir_name(target)),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("host-state.json")
    }

    fn baseline_path(&self) -> PathBuf {
        self.dir.join("baseline.json")
    }

    fn journal_dir(&self) -> PathBuf {
        self.dir.join("journal")
    }

    /// Load the host state (missing = default).
    pub async fn load_state(&self) -> Result<HostState> {
        Ok(read_json(&self.state_path()).await?.unwrap_or_default())
    }

    /// Save the host state.
    pub async fn save_state(&self, state: &HostState) -> Result<()> {
        write_json(&self.state_path(), state).await
    }

    /// The stored baseline, if any.
    pub async fn load_baseline(&self) -> Result<Option<Baseline>> {
        read_json(&self.baseline_path()).await
    }

    /// Store the baseline unless one exists (the first capture wins).
    pub async fn save_baseline_once(&self, baseline: &Baseline) -> Result<()> {
        if tokio::fs::try_exists(self.baseline_path())
            .await
            .map_err(|e| Error::io(self.baseline_path(), &e))?
        {
            return Ok(());
        }
        write_json(&self.baseline_path(), baseline).await
    }

    /// Write (create or replace) a journal record.
    pub async fn write_journal(&self, entry: &JournalEntry) -> Result<()> {
        let name = sanitize_file(&entry.id);
        write_json(&self.journal_dir().join(format!("{name}.json")), entry).await
    }

    /// All journal records, oldest first.
    pub async fn journal(&self) -> Result<Vec<JournalEntry>> {
        let dir = self.journal_dir();
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&dir, &e)),
        };
        let mut out = Vec::new();
        while let Some(ent) = rd.next_entry().await.map_err(|e| Error::io(&dir, &e))? {
            let p = ent.path();
            if p.extension().is_some_and(|x| x == "json")
                && let Some(entry) = read_json::<JournalEntry>(&p).await?
            {
                out.push(entry);
            }
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// The applied journal record holding exactly `report`, if any.
    pub async fn find_by_report(&self, report: &RemoteApplyReport) -> Result<Option<JournalEntry>> {
        Ok(self
            .journal()
            .await?
            .into_iter()
            .rev()
            .find(|e| e.report.as_ref() == Some(report)))
    }
}

fn sanitize_file(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

async fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::parse(path.display().to_string(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, &e)),
    }
}

async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| Error::other(e.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("no parent for {}", path.display())))?;
    create_private_dir(parent).await?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| Error::io(&tmp, &e))?;
    set_private_file(&tmp).await?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| Error::io(path, &e))
}

async fn create_private_dir(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| Error::io(dir, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| Error::io(dir, &e))?;
    }
    Ok(())
}

async fn set_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| Error::io(path, &e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
