//! Config journal (`<data>/journal/config/<id>.json`, plan 9.5): one entry per
//! apply or rollback of a Codex home, Pending -> Publishing -> (deleted after the
//! state commit). Recovery decides per file by hash: before = not done, after =
//! done, anything else = changed by someone else (their change wins).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{CodexFileKind, DataDir, Error, Result, Timestamp};

use crate::fsx;
use crate::state::GenerationEntry;

/// Journal phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    /// Temp files and backups are being prepared; nothing renamed yet.
    Pending,
    /// Renames in progress.
    Publishing,
}

/// What the entry does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Commit {
    /// Apply: on completion push this generation.
    Apply {
        /// New history entry.
        entry: Box<GenerationEntry>,
    },
    /// Rollback of `generation`: on completion drop it from the history.
    Rollback {
        /// Generation being rolled back.
        generation: u64,
    },
}

/// One file of an entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JournalFile {
    pub kind: CodexFileKind,
    /// Real path written (symlinks resolved).
    pub path: PathBuf,
    /// Hash before (`None` = absent).
    pub before_sha256: Option<String>,
    /// Hash after (`None` = removed).
    pub after_sha256: Option<String>,
    /// Staged temp file holding the new contents (`None` for removals).
    #[serde(default)]
    pub temp: Option<PathBuf>,
    /// Encrypted pre-image (`None` when the file did not exist).
    #[serde(default)]
    pub backup: Option<PathBuf>,
    /// Encrypted copy of the new contents, when they exist outside the temp file
    /// (rollback entries restore an earlier pre-image).
    #[serde(default)]
    pub source: Option<PathBuf>,
}

/// One journal entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: String,
    pub codex_home: PathBuf,
    pub created_at: Timestamp,
    pub phase: Phase,
    pub files: Vec<JournalFile>,
    pub commit: Commit,
}

fn path_for(data: &DataDir, id: &str) -> PathBuf {
    data.config_journal_dir()
        .join(format!("{}.json", fsx::sanitize_component(id)))
}

/// Persist (create or update) an entry.
pub(crate) fn write(data: &DataDir, entry: &Entry) -> Result<()> {
    fsx::ensure_private_dir(&data.config_journal_dir())?;
    fsx::write_json(&path_for(data, &entry.id), entry)
}

/// Delete an entry.
pub(crate) fn remove(data: &DataDir, id: &str) -> Result<()> {
    fsx::remove_if_exists(&path_for(data, id))
}

/// Entries for `root` (all homes when `None`), oldest first.
pub(crate) fn list(data: &DataDir, root: Option<&Path>) -> Result<Vec<Entry>> {
    let dir = data.config_journal_dir();
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&dir, &e)),
    };
    let mut out = Vec::new();
    for item in rd {
        let item = item.map_err(|e| Error::io(&dir, &e))?;
        let p = item.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let entry: Entry = match fsx::read_json(&p, "config journal")? {
            Some(e) => e,
            None => continue,
        };
        if root.is_none_or(|r| entry.codex_home == r) {
            out.push(entry);
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    Ok(out)
}

/// Remove staged temp files of an entry.
pub(crate) fn cleanup_temps(entry: &Entry) {
    for f in &entry.files {
        if let Some(t) = &f.temp {
            let _ = fsx::remove_if_exists(t);
        }
    }
}
