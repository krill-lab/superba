use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;
use sha2::{Digest, Sha256};
use superba_core::data::{DataMovePhase, DataMoveProgress, DataMoveProgressSink};
use superba_core::{Error, LocalContext, Result};

use crate::atomic::{private_dir, read_json, sync_dir, write_atomic, write_json};
use crate::location::{LOCATION_FILE, MoveJournal, Pointer, fixed_file};
use crate::validation::{is_link, planned_path, validate_directory};

pub(crate) const OWNER_FILE: &str = ".superba-data-owner";
const RESERVE: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    relative: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
    directory: bool,
}

fn inventory(root: &Path) -> Result<Vec<Entry>> {
    let mut todo = vec![PathBuf::new()];
    let mut entries = Vec::new();
    while let Some(relative) = todo.pop() {
        let path = root.join(&relative);
        let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, &e))?;
        if is_link(&meta) || (!meta.is_dir() && !meta.is_file()) {
            return Err(Error::invalid("data.path.link"));
        }
        if meta.is_dir() {
            for entry in fs::read_dir(&path).map_err(|e| Error::io(&path, &e))? {
                let entry = entry.map_err(|e| Error::io(&path, &e))?;
                let next = relative.join(entry.file_name());
                if !fixed_file(&next) {
                    todo.push(next);
                }
            }
        }
        if !relative.as_os_str().is_empty() {
            entries.push(Entry {
                relative,
                bytes: if meta.is_file() { meta.len() } else { 0 },
                modified: if meta.is_file() {
                    meta.modified().ok()
                } else {
                    None
                },
                directory: meta.is_dir(),
            });
        }
    }
    entries.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(entries)
}

/// Space is checked on the destination filesystem, with headroom for filesystem
/// metadata, JSON path growth and the next SQLite checkpoint.
pub fn check_space(bytes: u64, available: u64) -> Result<()> {
    if bytes
        .checked_add(RESERVE.max(bytes / 100))
        .is_none_or(|needed| needed > available)
    {
        return Err(Error::invalid("data.path.space"));
    }
    Ok(())
}

/// Prepared under the caller's exclusive data lock. Only private staging is
/// created; the source, final destination and pointer remain unchanged.
#[derive(Debug)]
pub struct PreparedMove {
    anchor: PathBuf,
    entries: Vec<Entry>,
    pub journal: MoveJournal,
}

impl PreparedMove {
    pub fn prepare(ctx: &LocalContext, destination: &Path) -> Result<Self> {
        let target = validate_directory(destination, ctx.env.as_ref())?;
        let source = superba_core::paths::canonical(ctx.data_dir.root())
            .map_err(|e| Error::io(ctx.data_dir.root(), &e))?;
        let anchor = superba_core::paths::canonical(ctx.data_dir.anchor())
            .map_err(|e| Error::io(ctx.data_dir.anchor(), &e))?;
        let home =
            superba_core::paths::canonical(&ctx.home_dir).unwrap_or_else(|_| ctx.home_dir.clone());
        let codex = planned_path(ctx.codex_home().root())
            .map(|p| p.0)
            .unwrap_or_else(|_| ctx.codex_home().root().to_path_buf());
        if target == home
            || target == codex
            || target.starts_with(&codex)
            || codex.starts_with(&target)
            || target.starts_with(&source)
            || source.starts_with(&target)
            || target.starts_with(&anchor)
            || anchor.starts_with(&target)
        {
            return Err(Error::invalid("data.path.overlap"));
        }
        if target.exists()
            && fs::read_dir(&target)
                .map_err(|e| Error::io(&target, &e))?
                .next()
                .is_some()
        {
            return Err(Error::invalid("data.path.not_empty"));
        }
        let entries = inventory(&source)?;
        let bytes = entries
            .iter()
            .try_fold(0u64, |n, e| n.checked_add(e.bytes))
            .ok_or_else(|| Error::invalid("data.path.space"))?;
        let (_, existing) = planned_path(&target)?;
        check_space(
            bytes,
            fs4::available_space(&existing).map_err(|e| Error::io(&existing, &e))?,
        )?;
        let parent = target
            .parent()
            .ok_or_else(|| Error::invalid("data.path.absolute"))?;
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, &e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let staging = parent.join(format!(".superba-data-{id}"));
        // Atomic exclusive directory reservation, then protected before any data arrives.
        fs::create_dir(&staging).map_err(|e| Error::io(&staging, &e))?;
        if let Err(error) = private_dir(&staging)
            .and_then(|()| write_atomic(&staging.join(OWNER_FILE), id.as_bytes()))
        {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        let previous_pointer = match read_json::<Pointer>(&anchor.join(LOCATION_FILE)) {
            Ok(pointer) => pointer,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        // Source uses canonical paths throughout, including the first pointer.
        if previous_pointer
            .as_ref()
            .is_some_and(|p| p.directory != source)
        {
            let _ = fs::remove_dir_all(&staging);
            return Err(Error::invalid("data.pointer.invalid"));
        }
        Ok(Self {
            anchor,
            entries,
            journal: MoveJournal {
                schema: 1,
                id,
                source,
                target,
                staging,
                restart_router: false,
                rollback: false,
                copied_bytes: bytes,
                previous_pointer,
            },
        })
    }

    pub fn total_bytes(&self) -> u64 {
        self.journal.copied_bytes
    }

    /// Record intent before stopping the router. A stop failure is recoverable too.
    pub fn begin(&mut self, restart_router: bool) -> Result<()> {
        self.journal.restart_router = restart_router;
        self.journal.write(&self.anchor)
    }

    /// Re-scan after stopping the router, copy regular files in bounded chunks,
    /// sync and verify every copy, and rewrite only owned path metadata.
    pub fn copy(&mut self, progress: &DataMoveProgressSink) -> Result<()> {
        self.entries = inventory(&self.journal.source)?;
        self.journal.copied_bytes = self.entries.iter().map(|e| e.bytes).sum();
        check_space(
            self.total_bytes(),
            fs4::available_space(&self.journal.staging)
                .map_err(|e| Error::io(&self.journal.staging, &e))?,
        )?;
        self.journal.write(&self.anchor)?;
        let mut copied = 0u64;
        for entry in &self.entries {
            let from = self.journal.source.join(&entry.relative);
            let to = self.journal.staging.join(&entry.relative);
            if entry.directory {
                private_dir(&to)?;
                continue;
            }
            if let Some(parent) = to.parent() {
                private_dir(parent)?;
            }
            let mut input = File::open(&from).map_err(|e| Error::io(&from, &e))?;
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = options.open(&to).map_err(|e| Error::io(&to, &e))?;
            let mut hash = Sha256::new();
            let mut buffer = vec![0u8; 1024 * 1024];
            let mut file_bytes = 0u64;
            loop {
                let n = input.read(&mut buffer).map_err(|e| Error::io(&from, &e))?;
                if n == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..n])
                    .map_err(|e| Error::io(&to, &e))?;
                hash.update(&buffer[..n]);
                file_bytes += n as u64;
                copied += n as u64;
                if !progress(DataMoveProgress {
                    phase: DataMovePhase::Copying,
                    copied_bytes: copied,
                    total_bytes: self.total_bytes(),
                }) {
                    return Err(Error::Cancelled);
                }
            }
            output.sync_all().map_err(|e| Error::io(&to, &e))?;
            let meta = input.metadata().map_err(|e| Error::io(&from, &e))?;
            if file_bytes != entry.bytes || meta.modified().ok() != entry.modified {
                return Err(Error::Conflict {
                    message: "data.changed".into(),
                });
            }
            let expected = hash.finalize();
            let mut verified = Sha256::new();
            let mut check = File::open(&to).map_err(|e| Error::io(&to, &e))?;
            loop {
                let n = check.read(&mut buffer).map_err(|e| Error::io(&to, &e))?;
                if n == 0 {
                    break;
                }
                verified.update(&buffer[..n]);
            }
            if expected != verified.finalize() {
                return Err(Error::other("data.verify"));
            }
            // Windows cannot atomically replace a destination still held by the
            // verification reader. Metadata rewriting below replaces the file.
            drop(check);
            // Keep executable bits for downloaded programs. Never make private
            // data readable by a wider set of users than its original mode.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = meta.permissions().mode() & 0o700;
                fs::set_permissions(&to, fs::Permissions::from_mode(mode | 0o600))
                    .map_err(|e| Error::io(&to, &e))?;
            }
            if let Some(modified) = entry.modified {
                output
                    .set_modified(modified)
                    .map_err(|e| Error::io(&to, &e))?;
            }
            drop(output);
            if metadata_file(&entry.relative) {
                let bytes = fs::read(&to).map_err(|e| Error::io(&to, &e))?;
                let mut value: Value = serde_json::from_slice(&bytes).map_err(|e| {
                    Error::parse(
                        "data metadata",
                        format!("{}: {e}", entry.relative.display()),
                    )
                })?;
                if rewrite_paths(&mut value, &self.journal.source, &self.journal.target) {
                    write_json(&to, &value)?;
                }
            }
        }
        if inventory(&self.journal.source)? != self.entries {
            return Err(Error::Conflict {
                message: "data.changed".into(),
            });
        }
        // Directory fsync from leaves upwards makes the rename durable.
        for entry in self.entries.iter().rev().filter(|e| e.directory) {
            sync_dir(&self.journal.staging.join(&entry.relative))?;
        }
        sync_dir(&self.journal.staging)
    }

    /// Atomic directory publication followed by the atomic pointer commit.
    pub fn publish(&self) -> Result<()> {
        let (target, _) = planned_path(&self.journal.target)?;
        if target != self.journal.target {
            return Err(Error::invalid("data.path.link"));
        }
        if self.journal.target.exists() {
            // remove_dir refuses a populated directory, including files which
            // appeared since preflight. Never overwrite or merge user data.
            fs::remove_dir(&self.journal.target)
                .map_err(|e| Error::io(&self.journal.target, &e))?;
        }
        fs::rename(&self.journal.staging, &self.journal.target)
            .map_err(|e| Error::io(&self.journal.target, &e))?;
        sync_dir(self.journal.target.parent().expect("validated parent"))?;
        write_json(
            &self.anchor.join(LOCATION_FILE),
            &Pointer {
                schema: 1,
                directory: self.journal.target.clone(),
            },
        )
    }
}

impl Drop for PreparedMove {
    fn drop(&mut self) {
        // A published target is handled by the caller's rollback/recovery. Only
        // uncommitted staging can be removed automatically on early return.
        if fs::read_to_string(self.journal.staging.join(OWNER_FILE))
            .ok()
            .as_deref()
            == Some(&self.journal.id)
        {
            let _ = fs::remove_dir_all(&self.journal.staging);
        }
    }
}

/// Only metadata written by Superba is interpreted. Session backup JSON/JSONL,
/// downloaded manifests, keys, model catalogs and arbitrary caches remain bytes.
pub fn metadata_file(relative: &Path) -> bool {
    if relative.extension().is_none_or(|ext| ext != "json") {
        return false;
    }
    matches!(relative.to_str(), Some("profiles.json" | "harnesses.json" | "state.json" | "env-state.json" | "secrets/vault.json"))
        || relative.starts_with("journal")
        || relative.starts_with("remote")
        || (relative.starts_with("backups") && relative.file_name().is_some_and(|name|
            name == "ledger.json" || name == "index.json"))
        // Path::to_str uses backslashes on Windows.
        || relative == Path::new("secrets").join("vault.json")
}

/// Rewrite typed path fields in owned JSON metadata, only when the full path
/// starts with the old directory on a component boundary. Ordinary strings,
/// credentials, user settings and restored file contents are not rewritten.
pub fn rewrite_paths(value: &mut Value, old: &Path, new: &Path) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let path_field = matches!(
                    key.as_str(),
                    "path"
                        | "backup"
                        | "auth_snapshot"
                        | "config_snapshot"
                        | "backup_dir"
                        | "ledger_path"
                        | "temp"
                        | "source"
                        | "file"
                        | "directory"
                );
                if path_field
                    && let Value::String(text) = value
                    && let Some(rebased) = rebase_path(text, old, new)
                {
                    *text = rebased.to_string_lossy().into_owned();
                    changed = true;
                }
                changed |= rewrite_paths(value, old, new);
            }
        }
        Value::Array(values) => {
            for value in values {
                changed |= rewrite_paths(value, old, new);
            }
        }
        _ => {}
    }
    changed
}

fn rebase_path(text: &str, old: &Path, new: &Path) -> Option<PathBuf> {
    let path = Path::new(text);
    if let Ok(relative) = path.strip_prefix(old) {
        return Some(new.join(relative));
    }
    #[cfg(windows)]
    {
        let plain = superba_core::paths::display_path(path);
        let source = superba_core::paths::display_path(old);
        if plain
            .get(..source.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(&source))
            && plain
                .get(source.len()..)
                .is_some_and(|s| s.is_empty() || s.starts_with('\\'))
        {
            return Some(new.join(plain[source.len()..].trim_start_matches('\\')));
        }
    }
    // Earlier data may have used an OS alias (/var on macOS, a non-verbatim
    // drive path on Windows). Resolve existing path fields before prefix matching.
    if path.is_absolute()
        && let Ok(real) = superba_core::paths::canonical(path)
        && let Ok(relative) = real.strip_prefix(old)
    {
        return Some(new.join(relative));
    }
    None
}

/// Delete old copies after the router and pointer are committed. The fixed
/// locator, coordination files and GUI endpoint stay at the anchor. A failure is
/// reported to the user and leaves the journal for a later cleanup attempt.
pub fn cleanup_source(source: &Path, anchor: &Path) -> Result<()> {
    let canonical = superba_core::paths::canonical(anchor).unwrap_or_else(|_| anchor.to_path_buf());
    let anchor = canonical.as_path();
    let meta = fs::symlink_metadata(source).map_err(|e| Error::io(source, &e))?;
    if is_link(&meta) {
        return Err(Error::invalid("data.path.link"));
    }
    for entry in fs::read_dir(source).map_err(|e| Error::io(source, &e))? {
        let entry = entry.map_err(|e| Error::io(source, &e))?;
        let name = PathBuf::from(entry.file_name());
        if source == anchor && fixed_file(&name) {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, &e))?;
        // remove_dir_all does not traverse symbolic links, but refuse unexpected
        // reparse points as well instead of trusting an altered source.
        if is_link(&meta) {
            return Err(Error::invalid("data.path.link"));
        }
        let result = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.map_err(|e| Error::io(&path, &e))?;
    }
    if source != anchor {
        fs::remove_dir(source).map_err(|e| Error::io(source, &e))?;
    }
    Ok(())
}
