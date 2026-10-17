//! Files of the router inside Superba's data dir (`<data dir>/router/`).

use std::path::{Path, PathBuf};

use superba_core::DataDir;

/// Name of the router directory inside the data dir.
pub const ROUTER_DIR: &str = "router";

/// Layout of `<data dir>/router/`.
///
/// | File | Writer | Contents |
/// |---|---|---|
/// | `config.json` | Superba | members, lines, model mapping, port (no secret) |
/// | `secrets.json` (0600) | Superba | router token, member keys |
/// | `runtime.json` (0600) | the running router | pid, port, instance id, admin token |
/// | `router.log` | launchd / the detached supervisor | stdout + stderr |
/// | `supervisor.pid` | the detached supervisor | its pid |
/// | `usage.sqlite3` (0600, WAL) | the running router | request log (numbers only) |
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouterPaths {
    dir: PathBuf,
}

impl RouterPaths {
    /// `<data dir>/router/`.
    pub fn new(data_dir: &DataDir) -> Self {
        Self::from_dir(data_dir.root().join(ROUTER_DIR))
    }

    /// Use `dir` as the router directory.
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The router directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `config.json`.
    pub fn config_json(&self) -> PathBuf {
        self.dir.join("config.json")
    }

    /// `secrets.json`.
    pub fn secrets_json(&self) -> PathBuf {
        self.dir.join("secrets.json")
    }

    /// `runtime.json`.
    pub fn runtime_json(&self) -> PathBuf {
        self.dir.join("runtime.json")
    }

    /// `router.log`.
    pub fn log_file(&self) -> PathBuf {
        self.dir.join("router.log")
    }

    /// `supervisor.pid`.
    pub fn supervisor_pid(&self) -> PathBuf {
        self.dir.join("supervisor.pid")
    }

    /// `usage.sqlite3`: the request log.
    pub fn usage_db(&self) -> PathBuf {
        self.dir.join("usage.sqlite3")
    }
}
