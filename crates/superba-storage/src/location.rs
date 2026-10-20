use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{DataDir, Error, Result};

use crate::atomic::{private_dir, read_json, write_json};

pub const LOCATION_FILE: &str = "data-location.json";
pub const LOCK_FILE: &str = "data-location.lock";
pub const MOVE_FILE: &str = "data-move.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pointer {
    pub schema: u32,
    pub directory: PathBuf,
}

/// Resolve only the fixed pointer; never follow a chain of pointers in destinations.
/// An invalid pointer or an unavailable drive is an error, never a new empty profile.
pub fn resolve(anchor: &Path) -> Result<DataDir> {
    let canonical = superba_core::paths::canonical(anchor).unwrap_or_else(|_| anchor.to_path_buf());
    let anchor = canonical.as_path();
    match read_json::<Pointer>(&anchor.join(LOCATION_FILE))? {
        None => Ok(DataDir::new(anchor)),
        Some(p) => {
            if p.schema != 1 || !p.directory.is_absolute() {
                return Err(Error::invalid("data.pointer.invalid"));
            }
            let metadata =
                fs::symlink_metadata(&p.directory).map_err(|e| Error::io(&p.directory, &e))?;
            if !metadata.is_dir() || crate::validation::is_link(&metadata) {
                return Err(Error::invalid("data.pointer.invalid"));
            }
            Ok(DataDir::at_location(p.directory, anchor))
        }
    }
}

/// The lock stays in the default directory, including after every move.
#[derive(Debug)]
pub struct DataLock(File);

impl DataLock {
    pub fn shared(anchor: &Path) -> Result<Self> {
        Self::acquire(anchor, false)
    }
    pub fn exclusive(anchor: &Path) -> Result<Self> {
        Self::acquire(anchor, true)
    }

    fn acquire(anchor: &Path, exclusive: bool) -> Result<Self> {
        // Do not rewrite the ACL on every read. The first writer initializes it.
        if !anchor.exists() {
            private_dir(anchor)?;
        }
        let path = anchor.join(LOCK_FILE);
        if fs::symlink_metadata(&path).is_ok_and(|m| crate::validation::is_link(&m)) {
            return Err(Error::invalid("data.path.link"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|e| Error::io(&path, &e))?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match result {
            Ok(()) => Ok(Self(file)),
            Err(fs::TryLockError::WouldBlock) => Err(Error::Locked {
                what: "data.busy".into(),
            }),
            Err(fs::TryLockError::Error(e)) => Err(Error::io(&path, &e)),
        }
    }
}

impl Drop for DataLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Durable intent; the pointer is the commit record. Recovery keeps `target` only
/// when the pointer names it; otherwise it restores the source router and data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveJournal {
    pub schema: u32,
    pub id: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub staging: PathBuf,
    pub restart_router: bool,
    pub rollback: bool,
    pub copied_bytes: u64,
    pub previous_pointer: Option<Pointer>,
}

impl MoveJournal {
    pub fn read(anchor: &Path) -> Result<Option<Self>> {
        let canonical =
            superba_core::paths::canonical(anchor).unwrap_or_else(|_| anchor.to_path_buf());
        let anchor = canonical.as_path();
        let journal: Option<Self> = read_json(&anchor.join(MOVE_FILE))?;
        if let Some(j) = &journal {
            let stage_name = j
                .staging
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or_default();
            if j.schema != 1
                || !j.source.is_absolute()
                || !j.target.is_absolute()
                || j.target == j.source
                || j.target.starts_with(&j.source)
                || j.source.starts_with(&j.target)
                || j.target.starts_with(anchor)
                || anchor.starts_with(&j.target)
                || j.staging.parent() != j.target.parent()
                || !stage_name.starts_with(".superba-data-")
                || stage_name != format!(".superba-data-{}", j.id)
                || uuid::Uuid::parse_str(&j.id).is_err()
                || j.source
                    != j.previous_pointer
                        .as_ref()
                        .map(|p| p.directory.as_path())
                        .unwrap_or(anchor)
            {
                return Err(Error::invalid("data.journal.invalid"));
            }
        }
        Ok(journal)
    }

    pub fn write(&self, anchor: &Path) -> Result<()> {
        write_json(&anchor.join(MOVE_FILE), self)
    }

    pub fn clear(anchor: &Path) -> Result<()> {
        let path = anchor.join(MOVE_FILE);
        match fs::remove_file(&path) {
            Ok(()) => crate::atomic::sync_dir(anchor),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, &e)),
        }
    }

    pub fn restore_pointer(&self, anchor: &Path) -> Result<()> {
        let path = anchor.join(LOCATION_FILE);
        match &self.previous_pointer {
            Some(p) => write_json(&path, p),
            None => match fs::remove_file(&path) {
                Ok(()) => crate::atomic::sync_dir(anchor),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::io(&path, &e)),
            },
        }
    }

    /// Remove only directories bearing this transaction's ownership marker.
    /// An unrelated directory that appeared at the destination is never deleted.
    pub fn discard_copies(&self) -> Result<()> {
        for dir in [&self.staging, &self.target] {
            match fs::symlink_metadata(dir) {
                Ok(meta) if crate::validation::is_link(&meta) => {
                    return Err(Error::invalid("data.path.link"));
                }
                Ok(_)
                    if fs::read_to_string(dir.join(crate::relocation::OWNER_FILE))
                        .ok()
                        .as_deref()
                        == Some(&self.id) =>
                {
                    fs::remove_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(dir, &e)),
            }
        }
        Ok(())
    }

    /// Verify the destination belongs to this interrupted move before recovery.
    pub fn owns_target(&self) -> bool {
        fs::symlink_metadata(&self.target)
            .is_ok_and(|m| m.is_dir() && !crate::validation::is_link(&m))
            && fs::read_to_string(self.target.join(crate::relocation::OWNER_FILE))
                .ok()
                .as_deref()
                == Some(&self.id)
    }

    /// Remove the marker after source cleanup and journal removal have completed.
    pub fn forget_owner(&self) {
        if self.owns_target() {
            let _ = fs::remove_file(self.target.join(crate::relocation::OWNER_FILE));
        }
    }
}

pub(crate) fn fixed_file(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some(
            LOCATION_FILE
                | LOCK_FILE
                | MOVE_FILE
                | "gui-instance.json"
                | crate::relocation::OWNER_FILE
                | "state.lock"
        )
    )
}
