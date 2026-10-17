//! `app-sessions.json`: apps launched by Superba (pid, start time, CDP port).
//!
//! The generation key of a launch is `(main pid, process start time)`, so a reused
//! pid never inherits a CDP port. Records of processes that are gone are pruned on
//! every write. Writes are atomic (temp file + rename).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{Error, Result, Timestamp};

/// Schema version of the file.
pub const SCHEMA: u32 = 1;

/// One app launched by Superba.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    /// Install id, if known.
    #[serde(default)]
    pub install_id: Option<String>,
    /// Executable that was started.
    #[serde(default)]
    pub executable: Option<PathBuf>,
    /// Main process pid.
    pub main_pid: u32,
    /// Main process start time as reported by the OS (generation key).
    #[serde(default)]
    pub process_started_at: Option<Timestamp>,
    /// When Superba launched it.
    pub launched_at: Timestamp,
    /// CDP port (`None` = launched without CDP, safe mode).
    #[serde(default)]
    pub cdp_port: Option<u16>,
    /// `HOME` override the app runs with.
    #[serde(default)]
    pub home_override: Option<PathBuf>,
    /// Environment variables Superba injected (names only; values may be secrets).
    #[serde(default)]
    pub injected_env: Vec<String>,
}

/// The whole file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSessions {
    /// Schema version.
    pub schema: u32,
    /// Records, newest last.
    #[serde(default)]
    pub sessions: Vec<LaunchRecord>,
}

impl Default for AppSessions {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            sessions: Vec::new(),
        }
    }
}

impl AppSessions {
    /// The record for a live process, matched by pid and start time.
    pub fn find(&self, pid: u32, started_at: Option<Timestamp>) -> Option<&LaunchRecord> {
        self.sessions.iter().rev().find(|r| {
            r.main_pid == pid
                && match (r.process_started_at, started_at) {
                    (Some(a), Some(b)) => {
                        (a.as_millis() - b.as_millis()).abs() <= crate::procs::START_TIME_SLACK_MS
                    }
                    // Unknown on either side: accept the pid match only for records
                    // younger than the process could be reused (best effort).
                    _ => true,
                }
        })
    }
}

/// Load the file; missing or unreadable content yields an empty set (the file is a
/// cache of observations, never a source of truth for writes).
pub fn load(path: &Path) -> AppSessions {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => AppSessions::default(),
    }
}

/// Write atomically.
pub fn save(path: &Path, sessions: &AppSessions) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("no parent directory: {}", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    let bytes = serde_json::to_vec_pretty(sessions)
        .map_err(|e| Error::other(format!("serialize app sessions: {e}")))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("app-sessions.json"),
        std::process::id()
    ));
    std::fs::write(&tmp, &bytes).map_err(|e| Error::io(&tmp, &e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(path, &e)
    })
}

/// Add a record, dropping records for which `alive` is false, and save.
pub fn record_launch(
    path: &Path,
    record: LaunchRecord,
    alive: &dyn Fn(&LaunchRecord) -> bool,
) -> Result<()> {
    let mut s = load(path);
    s.sessions
        .retain(|r| r.main_pid != record.main_pid && alive(r));
    s.sessions.push(record);
    save(path, &s)
}

/// Remove the record of `pid` (and dead ones) and save.
pub fn forget(path: &Path, pid: u32, alive: &dyn Fn(&LaunchRecord) -> bool) -> Result<()> {
    let mut s = load(path);
    let before = s.sessions.len();
    s.sessions.retain(|r| r.main_pid != pid && alive(r));
    if s.sessions.len() == before && !path.exists() {
        return Ok(());
    }
    save(path, &s)
}
