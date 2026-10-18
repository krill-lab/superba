//! `<data dir>/selfupdate.json`: ETags and the answers they belong to, the time
//! of the last check and the version the user skipped. Contains no secrets.

use std::path::Path;

use serde::{Deserialize, Serialize};
use superba_core::{SelfRelease, Timestamp};

/// How the last round of requests ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Never checked.
    #[default]
    None,
    /// Answers were read (or confirmed unchanged by a 304).
    Ok,
    /// 404 / 401 / 403: private repository or no release page.
    Unavailable,
    /// Network error, server error or rate limit.
    Failed,
}

/// One request and what it returned last time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// `ETag` of the last 200 answer.
    #[serde(default)]
    pub etag: Option<String>,
    /// The release read from it.
    #[serde(default)]
    pub release: Option<SelfRelease>,
}

/// A backup to remove once the new version has started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cleanup {
    /// Backup of the replaced macOS bundle.
    pub path: std::path::PathBuf,
    /// Version that must be running before it is removed.
    pub version: String,
}

/// The file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// API base the ETags belong to (a changed base drops them).
    #[serde(default)]
    pub api: Option<String>,
    /// When the last round got answers.
    #[serde(default)]
    pub checked_at: Option<Timestamp>,
    /// How it ended.
    #[serde(default)]
    pub outcome: Outcome,
    /// Why it failed (short, no secrets).
    #[serde(default)]
    pub detail: Option<String>,
    /// The last round also asked for pre-releases.
    #[serde(default)]
    pub included_prereleases: bool,
    /// `releases/latest`.
    #[serde(default)]
    pub stable: Channel,
    /// `releases` (newest pre-release), only fetched when opted in.
    #[serde(default)]
    pub prerelease: Channel,
    /// The version the user skipped.
    #[serde(default)]
    pub skipped: Option<String>,
    /// A backup waiting for the new version to start.
    #[serde(default)]
    pub cleanup: Option<Cleanup>,
    /// The last install attempt ([`super::record`]).
    #[serde(default)]
    pub last_attempt: Option<super::record::Attempt>,
}

impl State {
    /// Read the file; a missing or unreadable one is the default.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Write the file atomically (best effort: a failed write only means the
    /// next check starts without ETag).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension(format!("tmp-{}", crate::store::nonce()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}
