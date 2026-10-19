//! Superba's own records about the tools, under `<data dir>/tools/`:
//!
//! - `state.json`: per tool the channel Superba follows, whether Superba
//!   manages its updates (and what the setting held before), the installs
//!   Superba made, the launchers it wrote and the `PATH` entries it added
//!   (so a removal takes back exactly those);
//! - `latest.json`: the newest version per tool and channel, with the time
//!   it was looked up;
//! - `<tool>.lock`: held while an operation on the tool runs.
//!
//! Files are replaced atomically; an unreadable file counts as empty.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{DataDir, Error, Result, Timestamp, ToolId, ToolInstallKind};

/// `<data dir>/tools`.
pub fn tools_dir(data: &DataDir) -> PathBuf {
    data.root().join("tools")
}

/// What Superba installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRecord {
    /// How.
    pub kind: ToolInstallKind,
    /// Version.
    pub version: String,
    /// The program (or app).
    pub path: PathBuf,
    /// When.
    pub at: Timestamp,
}

/// A setting Superba changed to turn a tool's updater off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedUpdates {
    /// The file.
    pub file: PathBuf,
    /// The key (`env.DISABLE_AUTOUPDATER`, `update`).
    pub key: String,
    /// Its value before (JSON text; `None` = absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// Superba created the file (turning the switch off removes it again
    /// when nothing else is in it).
    #[serde(default)]
    pub created: bool,
}

/// Superba's record of one tool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolState {
    /// The channel the user chose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// "Superba manages updates" is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_updates: Option<ManagedUpdates>,
    /// Superba's install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<InstalledRecord>,
    /// Launchers Superba wrote (`~/.local/bin/pi`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub launchers: Vec<PathBuf>,
    /// Windows user `PATH` entries Superba added.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path_entries: Vec<String>,
}

/// `state.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Per tool.
    pub tools: BTreeMap<ToolId, ToolState>,
}

impl State {
    /// Load (empty when absent or unreadable).
    pub fn load(data: &DataDir) -> Self {
        read_json(&tools_dir(data).join("state.json")).unwrap_or_default()
    }

    /// Save atomically.
    pub fn save(&self, data: &DataDir) -> Result<()> {
        write_json(&tools_dir(data).join("state.json"), self)
    }

    /// The record of `tool`.
    pub fn tool(&self, tool: ToolId) -> ToolState {
        self.tools.get(&tool).cloned().unwrap_or_default()
    }

    /// Change the record of `tool` and save.
    pub fn update(data: &DataDir, tool: ToolId, f: impl FnOnce(&mut ToolState)) -> Result<()> {
        let mut s = Self::load(data);
        let entry = s.tools.entry(tool).or_default();
        f(entry);
        if *entry == ToolState::default() {
            s.tools.remove(&tool);
        }
        s.save(data)
    }
}

/// The newest version on one channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestEntry {
    /// Version.
    pub version: String,
    /// Looked up at.
    pub at: Timestamp,
}

/// `latest.json`: tool → channel → newest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LatestCache {
    /// Entries.
    pub tools: BTreeMap<ToolId, BTreeMap<String, LatestEntry>>,
}

impl LatestCache {
    /// Load (empty when absent or unreadable).
    pub fn load(data: &DataDir) -> Self {
        read_json(&tools_dir(data).join("latest.json")).unwrap_or_default()
    }

    /// Save atomically.
    pub fn save(&self, data: &DataDir) -> Result<()> {
        write_json(&tools_dir(data).join("latest.json"), self)
    }

    /// The entry of `tool` / `channel`.
    pub fn get(&self, tool: ToolId, channel: &str) -> Option<&LatestEntry> {
        self.tools.get(&tool)?.get(channel)
    }

    /// Record a lookup.
    pub fn set(&mut self, tool: ToolId, channel: &str, version: String, at: Timestamp) {
        self.tools
            .entry(tool)
            .or_default()
            .insert(channel.to_owned(), LatestEntry { version, at });
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| Error::other(e.to_string()))?;
    crate::fsx::write_atomic(path, &bytes)
}

/// Held while an operation on one tool runs (the lock is released when
/// dropped, also when the process dies).
#[derive(Debug)]
pub struct ToolLock {
    _file: fs::File,
    path: PathBuf,
}

impl Drop for ToolLock {
    fn drop(&mut self) {
        // Unlink while we still hold the lock. An opener of the old file
        // must check its identity after locking (below), or it could run
        // alongside a new opener of this path. Windows keeps a deleted
        // file pending until the last open handle is closed.
        let _ = fs::remove_file(&self.path);
        // Closing only our descriptor can leave flock held by a concurrent
        // spawn's inherited descriptor until exec. End the operation's lock
        // explicitly so the next operation can start immediately.
        let _ = self._file.unlock();
    }
}

impl ToolLock {
    /// Take the lock of `tool`, or fail with [`Error::Locked`].
    pub fn acquire(data: &DataDir, tool: ToolId) -> Result<Self> {
        let dir = tools_dir(data);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, &e))?;
        let path = dir.join(format!("{}.lock", tool.as_str()));
        loop {
            match Self::try_acquire(&path, tool)? {
                Some(lock) => return Ok(lock),
                None => continue,
            }
        }
    }

    fn try_acquire(path: &Path, tool: ToolId) -> Result<Option<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::io(path, &e))?;
        match file.try_lock() {
            Ok(()) => {
                let ours = same_file::Handle::from_file(
                    file.try_clone().map_err(|e| Error::io(path, &e))?,
                )
                .map_err(|e| Error::io(path, &e))?;
                let current = match same_file::Handle::from_path(path) {
                    Ok(current) => current,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(Error::io(path, &e)),
                };
                if ours != current {
                    // The preceding operation unlinked the file after we
                    // opened it. Retry on the current path, never on this inode.
                    return Ok(None);
                }
                Ok(Some(Self {
                    _file: file,
                    path: path.to_owned(),
                }))
            }
            Err(fs::TryLockError::WouldBlock) => Err(Error::Locked {
                what: format!(
                    "another install, update or removal of {}",
                    tool.display_name()
                ),
            }),
            Err(fs::TryLockError::Error(e)) => Err(Error::io(path, &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releasing_an_operation_unlocks_even_with_a_duplicate_handle() {
        let root = tempfile::tempdir().unwrap();
        let data = DataDir::new(root.path());
        let lock = ToolLock::acquire(&data, ToolId::Pi).unwrap();
        // A concurrent process spawn can inherit the descriptor until exec.
        // Model that live duplicate without forking the test process.
        let duplicate = lock._file.try_clone().unwrap();
        assert!(ToolLock::acquire(&data, ToolId::Pi).is_err());
        drop(lock);
        // Windows keeps the name delete-pending while the duplicate is open.
        #[cfg(unix)]
        let next = ToolLock::acquire(&data, ToolId::Pi).unwrap();
        drop(duplicate);
        #[cfg(not(unix))]
        let next = ToolLock::acquire(&data, ToolId::Pi).unwrap();
        assert!(ToolLock::acquire(&data, ToolId::Pi).is_err());
        drop(next);
        assert!(!tools_dir(&data).join("pi.lock").exists());
        assert!(ToolLock::acquire(&data, ToolId::Pi).is_ok());
    }

    #[test]
    fn competing_operations_remain_exclusive_while_lock_files_are_removed() {
        use std::sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        };
        let root = tempfile::tempdir().unwrap();
        let data = DataDir::new(root.path());
        let inside = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(4));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let (inside, start, data) = (inside.clone(), start.clone(), data.clone());
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..100 {
                        let lock = loop {
                            if let Ok(lock) = ToolLock::acquire(&data, ToolId::Pi) {
                                break lock;
                            }
                            std::thread::yield_now();
                        };
                        assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
                        std::thread::yield_now();
                        assert_eq!(inside.fetch_sub(1, Ordering::SeqCst), 1);
                        drop(lock);
                    }
                });
            }
        });
        assert!(!tools_dir(&data).join("pi.lock").exists());
    }
}
