//! `runtime.json`: written by the running router (0600) so Superba can find it
//! and authenticate to its admin API with the per-instance admin token.

use std::fs;

use serde::{Deserialize, Serialize};
use superba_core::{Error, Result, Secret};
use zeroize::Zeroize;

use crate::config::{CONFIG_FORMAT, write_private};
use crate::paths::RouterPaths;

/// What a running router publishes about itself.
#[derive(Clone, Debug)]
pub struct RuntimeInfo {
    /// Process id.
    pub pid: u32,
    /// Bound port on `127.0.0.1`.
    pub port: u16,
    /// Random id of this process (also served unauthenticated by `/health`).
    pub instance_id: String,
    /// Per-instance admin token for `/_superba/v1/*`.
    pub admin_token: Secret,
    /// Start time, Unix milliseconds.
    pub started_at_ms: u64,
    /// Router version.
    pub version: String,
}

#[derive(Serialize, Deserialize)]
struct RuntimeWire {
    format: u32,
    pid: u32,
    port: u16,
    instance_id: String,
    admin_token: String,
    started_at_ms: u64,
    version: String,
}

impl Drop for RuntimeWire {
    fn drop(&mut self) {
        self.admin_token.zeroize();
    }
}

impl RuntimeInfo {
    /// Read `runtime.json`.
    pub fn load(paths: &RouterPaths) -> Result<Self> {
        let path = paths.runtime_json();
        let bytes = fs::read(&path).map_err(|e| Error::io(&path, &e))?;
        let w: RuntimeWire = serde_json::from_slice(&bytes)
            .map_err(|_| Error::parse("router runtime.json", "not valid"))?;
        if w.format != CONFIG_FORMAT {
            return Err(Error::parse(
                "router runtime.json",
                format!("format {}", w.format),
            ));
        }
        Ok(Self {
            pid: w.pid,
            port: w.port,
            instance_id: w.instance_id.clone(),
            admin_token: Secret::new(w.admin_token.as_str()),
            started_at_ms: w.started_at_ms,
            version: w.version.clone(),
        })
    }

    /// Write `runtime.json` (0600, atomic).
    pub fn save(&self, paths: &RouterPaths) -> Result<()> {
        let w = RuntimeWire {
            format: CONFIG_FORMAT,
            pid: self.pid,
            port: self.port,
            instance_id: self.instance_id.clone(),
            admin_token: self.admin_token.expose().to_owned(),
            started_at_ms: self.started_at_ms,
            version: self.version.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&w)
            .map_err(|e| Error::other(format!("serialize runtime.json: {e}")))?;
        bytes.push(b'\n');
        let r = write_private(&paths.runtime_json(), &bytes);
        bytes.zeroize();
        r
    }

    /// Remove `runtime.json` if it still describes `instance_id`.
    pub fn remove_if_owned(paths: &RouterPaths, instance_id: &str) {
        if let Ok(info) = Self::load(paths)
            && info.instance_id == instance_id
        {
            let _ = fs::remove_file(paths.runtime_json());
        }
    }
}
