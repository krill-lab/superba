//! The file editor's backups and save records (`<data>/backups/editor/`).
//!
//! Before every save the editor seals the file it replaces (ChaCha20-Poly1305,
//! the same data key as the switch backups and login stashes) into
//! `backups/editor/<home key>/<id>.enc` and lists it in `index.json` with the
//! time, the home and why it was taken. The newest [`KEEP_PER_FILE`] backups of
//! each file of each home are kept. The index also records the last save of
//! each file per home ([`EditRecord`]): the background check names the
//! editor as the source of a change it finds in a file the editor saved
//! (GH#5), and "in effect" knows which running Codex started before the save
//! (GH#4).
//!
//! Listing a local home adds the backups Superba already keeps: the pre-image
//! of each recent switch (`backups/switch/`, ids `switch-g<generation>-<file>`)
//! and the original before Superba's first write (`original-<file>`).
//! Contents never leave this module unencrypted except through
//! [`EditorStore::read`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use superba_core::{
    BackupSource, CodexFileBackup, CodexFileBackupContents, CodexFileBackupQuery, CodexFileKind,
    Error, LocalContext, Result, TargetId, Timestamp,
};

use crate::fsx::{self, Mode};
use crate::state;
use crate::vault::Vault;

/// Editor backups kept per file and home.
pub const KEEP_PER_FILE: usize = 50;
/// Size of the sealed format's overhead (magic, nonce, tag).
const SEALED_OVERHEAD: u64 = 6 + 12 + 16;
const INDEX_SCHEMA: u32 = 1;

static INDEX_MUTEX: Mutex<()> = Mutex::new(());

/// The last save of one file of one home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRecord {
    /// The home.
    pub target: TargetId,
    /// The file.
    pub kind: CodexFileKind,
    /// Its path (local path or remote POSIX path).
    pub path: String,
    /// When.
    pub at: Timestamp,
    /// sha256 before the save (`None` = the file did not exist).
    #[serde(default)]
    pub before_sha256: Option<String>,
    /// sha256 of what was saved.
    pub after_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    backup: CodexFileBackup,
    /// The sealed copy, relative to the editor directory.
    file: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    backups: Vec<Entry>,
    #[serde(default)]
    edits: Vec<EditRecord>,
    /// Copies of other harnesses' files (dsh, opencode, pi, ZCode, WorkBuddy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    harness: Vec<HarnessEntry>,
}

/// One harness file copy in the index.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HarnessEntry {
    #[serde(flatten)]
    backup: superba_core::harness::HarnessBackup,
    /// The sealed copy, relative to the editor directory.
    sealed: PathBuf,
}

/// Replace the local Codex file at `path` with `contents`, only when it still
/// has `expected` (sha256; `None` = it must not exist): staged next to it,
/// checked again right before the rename (a symlinked file is written
/// through). `auth.json` gets mode 0600; `config.toml` keeps its mode.
pub fn write_file(
    path: &Path,
    kind: CodexFileKind,
    contents: &[u8],
    expected: Option<&str>,
) -> Result<()> {
    let target = fsx::resolve_write_target(path);
    let changed = || Error::Conflict {
        message: format!(
            "{} changed since it was opened; nothing was written",
            path.display()
        ),
    };
    if fsx::file_sha256(&target)?.as_deref() != expected {
        return Err(changed());
    }
    let mode = match kind {
        CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => Mode::Private,
        CodexFileKind::ConfigToml => Mode::PreserveOrPrivate,
    };
    let temp = fsx::stage(&target, contents, mode)?;
    if fsx::file_sha256(&target)?.as_deref() != expected {
        let _ = std::fs::remove_file(&temp);
        return Err(changed());
    }
    fsx::publish(&temp, &target)
}

/// The editor's backups and save records (see the module docs).
#[derive(Clone, Debug)]
pub struct EditorStore {
    ctx: LocalContext,
    vault: Vault,
}

fn kind_short(kind: CodexFileKind) -> &'static str {
    kind.short_name()
}

fn kind_of_short(s: &str) -> Option<CodexFileKind> {
    match s {
        "config" => Some(CodexFileKind::ConfigToml),
        "auth" => Some(CodexFileKind::AuthJson),
        _ => None,
    }
}

/// `20261018-153012` (UTC) of a time.
fn stamp(t: Timestamp) -> String {
    let r = t.to_rfc3339();
    let digits: String = r.chars().filter(char::is_ascii_digit).collect();
    format!("{}-{}", &digits[..8], &digits[8..14.min(digits.len())])
}

impl EditorStore {
    /// A store in `ctx`'s data directory.
    pub fn new(ctx: LocalContext) -> Self {
        Self {
            vault: Vault::new(ctx.clone()),
            ctx,
        }
    }

    fn dir(&self) -> PathBuf {
        self.ctx.data_dir.editor_backups_dir()
    }

    fn index_path(&self) -> PathBuf {
        self.dir().join("index.json")
    }

    fn load(&self) -> Result<Index> {
        Ok(
            fsx::read_json::<Index>(&self.index_path(), "backups/editor/index.json")?
                .unwrap_or_default(),
        )
    }

    fn save(&self, index: &Index) -> Result<()> {
        fsx::write_json(&self.index_path(), index)
    }

    fn with_index<T>(&self, f: impl FnOnce(&mut Index) -> Result<T>) -> Result<T> {
        let _guard = INDEX_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let mut index = self.load()?;
        let out = f(&mut index)?;
        index.schema = INDEX_SCHEMA;
        self.save(&index)?;
        Ok(out)
    }

    /// Seal `bytes` as a backup of `kind` in `target` and list it.
    pub fn backup(
        &self,
        target: &TargetId,
        target_label: &str,
        kind: CodexFileKind,
        bytes: &[u8],
        source: BackupSource,
        at: Timestamp,
    ) -> Result<CodexFileBackup> {
        if kind == CodexFileKind::ClaudeSettings {
            let path = superba_core::ClaudeHome::resolve(self.ctx.env.as_ref(), &self.ctx.home_dir)
                .settings_json();
            return self
                .harness_backup(
                    superba_core::harness::HarnessId::ClaudeCode,
                    "settings.json",
                    &path,
                    bytes,
                    source,
                    at,
                )
                .map(claude_backup);
        }
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("{}-{}-{}", stamp(at), kind_short(kind), &rand[..4]);
        let home_key = fsx::short_hash(target.as_str().as_bytes());
        let rel = PathBuf::from(&home_key).join(format!("{id}.enc"));
        let path = self.dir().join(&rel);
        self.vault.write_sealed(&path, bytes)?;
        let backup = CodexFileBackup {
            id,
            target: target.clone(),
            target_label: target_label.to_owned(),
            kind,
            at,
            source,
            size: bytes.len() as u64,
            sha256: Some(fsx::sha256_hex(bytes)),
        };
        let dir = self.dir();
        let listed = backup.clone();
        let result = self.with_index(move |index| {
            index.backups.push(Entry {
                backup: listed,
                file: rel,
            });
            // Keep the newest KEEP_PER_FILE of this file of this home.
            // The original (before Superba first wrote the file) is never
            // pruned.
            let mut same: Vec<(Timestamp, String)> = index
                .backups
                .iter()
                .filter(|e| {
                    e.backup.target == *target
                        && e.backup.kind == kind
                        && e.backup.source != BackupSource::Original
                })
                .map(|e| (e.backup.at, e.backup.id.clone()))
                .collect();
            same.sort();
            let excess = same.len().saturating_sub(KEEP_PER_FILE);
            let drop: Vec<String> = same.into_iter().take(excess).map(|(_, id)| id).collect();
            index.backups.retain(|e| {
                if drop.contains(&e.backup.id) {
                    let _ = std::fs::remove_file(dir.join(&e.file));
                    false
                } else {
                    true
                }
            });
            Ok(())
        });
        if let Err(e) = result {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        Ok(backup)
    }

    /// Record the last save of a file of a home.
    pub fn record_edit(&self, record: EditRecord) -> Result<()> {
        self.with_index(move |index| {
            index
                .edits
                .retain(|e| !(e.target == record.target && e.kind == record.kind));
            index.edits.push(record);
            Ok(())
        })
    }

    /// The last save of `kind` in `target`, if the editor saved it.
    pub fn last_edit(&self, target: &TargetId, kind: CodexFileKind) -> Result<Option<EditRecord>> {
        Ok(self
            .load()?
            .edits
            .into_iter()
            .find(|e| &e.target == target && e.kind == kind))
    }

    /// Every recorded save of `target` (one per file at most).
    pub fn edits(&self, target: &TargetId) -> Result<Vec<EditRecord>> {
        Ok(self
            .load()?
            .edits
            .into_iter()
            .filter(|e| &e.target == target)
            .collect())
    }

    /// Backups matching `query`, newest first. `local_home` is this
    /// computer's Codex home: its switch pre-images and originals are listed
    /// too (labelled `local_label`).
    pub fn list(
        &self,
        query: &CodexFileBackupQuery,
        local_home: &Path,
        local_label: &str,
    ) -> Result<Vec<CodexFileBackup>> {
        let wants = |t: &TargetId, k: CodexFileKind| {
            query.target.as_ref().is_none_or(|q| q == t) && query.kind.is_none_or(|q| q == k)
        };
        let mut out: Vec<CodexFileBackup> = self
            .load()?
            .backups
            .into_iter()
            .map(|e| e.backup)
            .filter(|b| wants(&b.target, b.kind))
            .collect();
        let local = TargetId::local();
        if wants(&local, CodexFileKind::ClaudeSettings) {
            out.extend(
                self.harness_backups(superba_core::harness::HarnessId::ClaudeCode)?
                    .into_iter()
                    .map(claude_backup),
            );
        }
        if query.target.as_ref().is_none_or(TargetId::is_local) {
            for (b, _) in self.engine_backups(local_home, local_label)? {
                if wants(&local, b.kind) {
                    out.push(b);
                }
            }
        }
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
        Ok(out)
    }

    /// The pre-images of Superba's recent switches and the originals of the
    /// local home, with their sealed files.
    fn engine_backups(&self, home: &Path, label: &str) -> Result<Vec<(CodexFileBackup, PathBuf)>> {
        let hs = state::read_home(&self.ctx.data_dir, home)?;
        let mut out = Vec::new();
        let key = fsx::short_hash(home.to_string_lossy().as_bytes());
        let make = |id: String, kind, at, source, path: &Path, sha: Option<String>| {
            let size = std::fs::metadata(path)
                .map(|m| m.len().saturating_sub(SEALED_OVERHEAD))
                .unwrap_or(0);
            CodexFileBackup {
                id,
                target: TargetId::local(),
                target_label: label.to_owned(),
                kind,
                at,
                source,
                size,
                sha256: sha,
            }
        };
        let switch_dir = self.ctx.data_dir.switch_backups_dir().join(&key);
        if let Ok(rd) = std::fs::read_dir(&switch_dir) {
            for item in rd.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                let Some(generation) = name
                    .strip_prefix('g')
                    .and_then(|s| s.split('-').next())
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    continue;
                };
                let record = hs
                    .history
                    .iter()
                    .find(|e| e.record.generation == generation);
                for kind in [CodexFileKind::ConfigToml, CodexFileKind::AuthJson] {
                    let path = item.path().join(format!("{}.pre.enc", kind.file_name()));
                    if !path.is_file() {
                        continue;
                    }
                    let at = record.map(|e| e.record.applied_at).unwrap_or_else(|| {
                        std::fs::metadata(&path)
                            .and_then(|m| m.modified())
                            .map(Timestamp::from_system_time)
                            .unwrap_or_default()
                    });
                    let source = BackupSource::BeforeSwitch {
                        generation,
                        profile: record.and_then(|e| e.record.profile_id.clone()),
                    };
                    let id = format!("switch-g{generation}-{}", kind_short(kind));
                    out.push((make(id, kind, at, source, &path, None), path));
                }
            }
        }
        let captured = hs.baseline.as_ref().map(|b| b.captured_at);
        for (kind, fw) in &hs.first_write {
            let (Some(path), true) = (&fw.backup, fw.existed) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let at = captured.unwrap_or_default();
            let id = format!("original-{}", kind_short(*kind));
            out.push((
                make(
                    id,
                    *kind,
                    at,
                    BackupSource::Original,
                    path,
                    fw.sha256.clone(),
                ),
                path.clone(),
            ));
        }
        Ok(out)
    }

    /// Decrypt one backup (`local_home` / `local_label` as in
    /// [`EditorStore::list`]).
    pub fn read(
        &self,
        id: &str,
        local_home: &Path,
        local_label: &str,
    ) -> Result<CodexFileBackupContents> {
        let not_found = || Error::NotFound {
            what: format!("backup {id}"),
        };
        let found = if id.starts_with("switch-g") || id.starts_with("original-") {
            let suffix = id.rsplit('-').next().unwrap_or_default();
            kind_of_short(suffix).ok_or_else(not_found)?;
            self.engine_backups(local_home, local_label)?
                .into_iter()
                .find(|(b, _)| b.id == id)
        } else {
            self.load()?
                .backups
                .into_iter()
                .find(|e| e.backup.id == id)
                .map(|e| (e.backup, self.dir().join(e.file)))
        };
        if found.is_none() {
            let (backup, contents) = self.harness_backup_read(id)?;
            if backup.harness != superba_core::harness::HarnessId::ClaudeCode {
                return Err(not_found());
            }
            return Ok(CodexFileBackupContents {
                backup: claude_backup(backup),
                contents,
            });
        }
        let (backup, path) = found.ok_or_else(not_found)?;
        let contents = self
            .vault
            .read_sealed(&path)?
            .ok_or_else(|| Error::NotFound {
                what: format!("the encrypted copy of backup {id}"),
            })?;
        if let Some(sha) = &backup.sha256
            && &fsx::sha256_hex(contents.expose()) != sha
        {
            return Err(Error::Conflict {
                message: format!("backup {id} does not match its recorded hash"),
            });
        }
        Ok(CodexFileBackupContents { backup, contents })
    }
}

// ---------------------------------------------------------------------------
// Other harnesses' files: the same sealed copies and index, in their own
// list (`harness/<id>/<copy>.enc`), the newest KEEP_PER_FILE per file kept and
// the original (before Superba first wrote the file) never pruned.

impl EditorStore {
    /// Seal `bytes` (the file `file` of `harness` at `path`) and list it.
    pub fn harness_backup(
        &self,
        harness: superba_core::harness::HarnessId,
        file: &str,
        path: &Path,
        bytes: &[u8],
        source: BackupSource,
        at: Timestamp,
    ) -> Result<superba_core::harness::HarnessBackup> {
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("{}-{}-{}", stamp(at), harness.as_str(), &rand[..6]);
        let rel = PathBuf::from("harness")
            .join(harness.as_str())
            .join(format!("{id}.enc"));
        let sealed = self.dir().join(&rel);
        self.vault.write_sealed(&sealed, bytes)?;
        let backup = superba_core::harness::HarnessBackup {
            id,
            harness,
            file: file.to_owned(),
            path: path.to_path_buf(),
            at,
            source,
            size: bytes.len() as u64,
            sha256: Some(fsx::sha256_hex(bytes)),
        };
        let dir = self.dir();
        let listed = backup.clone();
        let result = self.with_index(move |index| {
            index.harness.push(HarnessEntry {
                backup: listed.clone(),
                sealed: rel,
            });
            let mut same: Vec<(Timestamp, String)> = index
                .harness
                .iter()
                .filter(|e| {
                    e.backup.harness == listed.harness
                        && e.backup.file == listed.file
                        && e.backup.source != BackupSource::Original
                })
                .map(|e| (e.backup.at, e.backup.id.clone()))
                .collect();
            same.sort();
            let excess = same.len().saturating_sub(KEEP_PER_FILE);
            let drop: Vec<String> = same.into_iter().take(excess).map(|(_, id)| id).collect();
            index.harness.retain(|e| {
                if drop.contains(&e.backup.id) {
                    let _ = std::fs::remove_file(dir.join(&e.sealed));
                    false
                } else {
                    true
                }
            });
            Ok(())
        });
        if let Err(e) = result {
            let _ = std::fs::remove_file(&sealed);
            return Err(e);
        }
        Ok(backup)
    }

    /// The copies of `harness`'s files, newest first.
    pub fn harness_backups(
        &self,
        harness: superba_core::harness::HarnessId,
    ) -> Result<Vec<superba_core::harness::HarnessBackup>> {
        let mut out: Vec<_> = self
            .load()?
            .harness
            .into_iter()
            .map(|e| e.backup)
            .filter(|b| b.harness == harness)
            .collect();
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
        Ok(out)
    }

    /// Decrypt one harness file copy.
    pub fn harness_backup_read(
        &self,
        id: &str,
    ) -> Result<(
        superba_core::harness::HarnessBackup,
        superba_core::SecretBytes,
    )> {
        let entry = self
            .load()?
            .harness
            .into_iter()
            .find(|e| e.backup.id == id)
            .ok_or_else(|| Error::NotFound {
                what: format!("backup {id}"),
            })?;
        let contents = self
            .vault
            .read_sealed(&self.dir().join(&entry.sealed))?
            .ok_or_else(|| Error::NotFound {
                what: format!("the encrypted copy of backup {id}"),
            })?;
        if let Some(sha) = &entry.backup.sha256
            && &fsx::sha256_hex(contents.expose()) != sha
        {
            return Err(Error::Conflict {
                message: format!("backup {id} does not match its recorded hash"),
            });
        }
        Ok((entry.backup, contents))
    }
}

/// Projection for the Claude settings editor; the sealed copy and index entry
/// remain in the shared harness partition.
fn claude_backup(b: superba_core::harness::HarnessBackup) -> CodexFileBackup {
    CodexFileBackup {
        id: b.id,
        target: TargetId::local(),
        target_label: "This computer".into(),
        kind: CodexFileKind::ClaudeSettings,
        at: b.at,
        source: b.source,
        size: b.size,
        sha256: b.sha256,
    }
}
