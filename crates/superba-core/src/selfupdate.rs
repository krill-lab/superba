//! Superba's own update: what the check found, what the user chose, and the
//! result of an install (`Updates` page, Overview to-do, `spb self-update`).
//!
//! The release source is the GitHub releases of `krill-lab/superba`. The repo may
//! be private, in which case the check ends in [`SelfUpdateStatus::PageUnavailable`],
//! a calm state and not an error.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
use crate::time::Timestamp;

/// Outcome of the latest check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SelfUpdateStatus {
    /// Nothing was checked yet (or the check is off).
    #[default]
    NotChecked,
    /// The newest release is not newer than this build.
    UpToDate,
    /// A newer release exists.
    Available,
    /// The release page answered 404 / 401 / 403 (private repository or no
    /// release): "cannot check right now", not an error.
    PageUnavailable,
    /// The release page could not be reached (network, timeout, server error).
    CheckFailed,
    /// Online checks are off in the network preferences.
    OnlineChecksOff,
}

/// One release of Superba.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfRelease {
    /// Git tag (`v0.2.0`).
    pub tag: String,
    /// Semantic version without the `v`.
    pub version: String,
    /// Marked as a pre-release (or has a pre-release part).
    pub prerelease: bool,
    /// Release notes (markdown source).
    #[serde(default)]
    pub notes: String,
    /// Publication time, as GitHub reports it (RFC 3339).
    #[serde(default)]
    pub published_at: Option<String>,
    /// The release page.
    #[serde(default)]
    pub url: String,
    /// Name of the package for this OS and architecture, when the release has one.
    #[serde(default)]
    pub asset: Option<String>,
    /// Its size in bytes, when GitHub reports it.
    #[serde(default)]
    pub asset_size: Option<u64>,
}

/// What the Updates page, the Overview and `spb self-update check` show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfUpdateView {
    /// The running version (`CARGO_PKG_VERSION`).
    pub current: String,
    /// Latest check result.
    pub status: SelfUpdateStatus,
    /// The release to offer ([`SelfUpdateStatus::Available`]) or the newest known.
    #[serde(default)]
    pub latest: Option<SelfRelease>,
    /// When the check last got an answer (also a `304`).
    #[serde(default)]
    pub checked_at: Option<Timestamp>,
    /// A short reason for [`SelfUpdateStatus::CheckFailed`] (no secrets).
    #[serde(default)]
    pub detail: Option<String>,
    /// The version the user chose to skip.
    #[serde(default)]
    pub skipped: Option<String>,
    /// Pre-releases are included.
    #[serde(default)]
    pub include_prereleases: bool,
    /// Checking automatically is on.
    #[serde(default)]
    pub auto_check: bool,
    /// This OS / architecture has no package in the offered release.
    #[serde(default)]
    pub no_package: bool,
    /// The last install attempt failed or ended with a problem (shown once at
    /// the next start, and on the Updates page until the next attempt).
    #[serde(default)]
    pub problem: Option<SelfUpdateProblem>,
}

/// What went wrong in the last install attempt: the reason and the log with
/// every step (`<data dir>/logs/selfupdate.log`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfUpdateProblem {
    /// The version that was being installed.
    pub version: String,
    /// When the attempt started.
    pub at: Timestamp,
    /// `true`: the new version was installed but a later step failed (router
    /// restart, a pending Windows restart); `false`: the update failed and the
    /// old version is still installed.
    pub installed: bool,
    /// Why.
    pub reason: LocalizedText,
    /// The log of that attempt.
    pub log: PathBuf,
    /// Already shown to the user ([`crate::SwitchService::self_update_acknowledge`]).
    #[serde(default)]
    pub seen: bool,
}

impl SelfUpdateView {
    /// A newer release exists and has not been skipped.
    pub fn offered(&self) -> bool {
        self.status == SelfUpdateStatus::Available && !self.is_skipped()
    }

    /// The newer release is the skipped one.
    pub fn is_skipped(&self) -> bool {
        match (&self.skipped, &self.latest) {
            (Some(s), Some(l)) => s == &l.version,
            _ => false,
        }
    }
}

/// [`crate::SwitchService::self_update_check`] request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfUpdateCheckRequest {
    /// The user asked (button, command, `spb self-update check`): ignore the
    /// "automatic checks" preference and the 24 h interval. Automatic checks
    /// leave the cached answer alone while it is fresh.
    pub manual: bool,
}

/// [`crate::SwitchService::self_update_install`] request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfUpdateInstallRequest {
    /// Start the new version when the swap is done (the caller then quits).
    pub relaunch: bool,
}

/// How an install ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SelfInstallOutcome {
    /// The files were replaced in place.
    Installed,
    /// Windows: the verified package is staged and the new `superba.exe`
    /// finishes the swap once this process has exited.
    Handoff,
    /// The install location is not writable: the verified package is kept and
    /// shown with manual steps.
    Manual,
}

/// Result of [`crate::SwitchService::self_update_install`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfUpdateReport {
    /// Installed (or staged) version.
    pub version: String,
    /// How it ended.
    pub outcome: SelfInstallOutcome,
    /// The verified package (always for [`SelfInstallOutcome::Manual`]).
    #[serde(default)]
    pub package: Option<PathBuf>,
    /// Manual steps ([`SelfInstallOutcome::Manual`]).
    #[serde(default)]
    pub steps: Vec<LocalizedText>,
    /// The local router service was running and has been restarted.
    #[serde(default)]
    pub router_restarted: bool,
    /// A new Superba start is scheduled: the caller should quit now.
    #[serde(default)]
    pub relaunching: bool,
    /// Files replaced (names).
    #[serde(default)]
    pub replaced: Vec<String>,
}
