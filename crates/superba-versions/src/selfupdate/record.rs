//! The record of the last install attempt (`last_attempt` in
//! `<data dir>/selfupdate.json`): which version, how far it got and what went
//! wrong. The process that downloads writes it as it goes, the second stage
//! (`--finish-update`) finishes it before it starts the new version, and the
//! next start turns it into a [`SelfUpdateProblem`] when the attempt failed,
//! ended with a problem, never finished, or claimed success while the old
//! version still runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};
use superba_core::{LocalizedText, SelfUpdateProblem, Timestamp};

use super::runlog;
use super::state::State;
use super::version;

/// An attempt that is still running or handed off after this long without
/// news did not finish (the downloading process or the second stage died).
pub const STALE: Duration = Duration::from_secs(15 * 60);

/// How far an attempt got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Downloading, verifying, swapping (in the process the user started it from).
    Running,
    /// macOS / Linux: the files were replaced; the old process still runs
    /// until it quits (a second stage may start the new one).
    Installed,
    /// Windows: the second stage waits for the old process and runs the MSI.
    Handoff,
    /// The second stage finished; the new version should be the one running.
    Done,
    /// The verified package is kept for a manual install.
    Manual,
    /// The update failed; the old version is still installed.
    Failed,
}

/// The last attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// The version that started it.
    pub from: String,
    /// The version being installed.
    pub to: String,
    /// When it started.
    pub started_at: Timestamp,
    /// The last change of [`Attempt::stage`] or [`Attempt::problem`].
    pub updated_at: Timestamp,
    /// How far it got.
    pub stage: Stage,
    /// Why it failed ([`Stage::Failed`]) or what went wrong although the new
    /// version was installed (router restart, a pending Windows restart).
    #[serde(default)]
    pub problem: Option<LocalizedText>,
    /// The attempt's log.
    pub log: PathBuf,
    /// The problem was shown already (an error in the window that started
    /// it, or the notice at the next start).
    #[serde(default)]
    pub seen: bool,
}

impl Attempt {
    /// A new attempt from `from` to `to`, logged in `log`.
    pub fn new(from: &str, to: &str, log: &Path, now: Timestamp) -> Self {
        Self {
            from: from.to_owned(),
            to: to.to_owned(),
            started_at: now,
            updated_at: now,
            stage: Stage::Running,
            problem: None,
            log: log.to_owned(),
            seen: false,
        }
    }

    /// What to tell the user about this attempt when `current` runs at `now`;
    /// `None` when it went well or is still going.
    pub fn problem(&self, current: &Version, now: Timestamp) -> Option<SelfUpdateProblem> {
        let stale = now
            .duration_since(self.updated_at)
            .is_some_and(|age| age >= STALE);
        let not_applied =
            version::parse_version(&self.to).is_some_and(|to| version::is_newer(&to, current));
        let (installed, reason) = match self.stage {
            Stage::Failed => (
                false,
                self.problem.clone().unwrap_or_else(|| {
                    LocalizedText::new(
                        "selfupdate.problem.failed",
                        "The update to {version} failed.",
                    )
                    .arg("version", &self.to)
                }),
            ),
            Stage::Done if not_applied => (
                false,
                LocalizedText::new(
                    "selfupdate.problem.not_applied",
                    "The installer reported success, but this is still Superba {current}.",
                )
                .arg("current", current),
            ),
            Stage::Running | Stage::Handoff if stale => (
                false,
                LocalizedText::new(
                    "selfupdate.problem.unfinished",
                    "The update to {version} did not finish.",
                )
                .arg("version", &self.to),
            ),
            Stage::Done | Stage::Installed => (true, self.problem.clone()?),
            _ => return None,
        };
        Some(SelfUpdateProblem {
            version: self.to.clone(),
            at: self.started_at,
            installed,
            reason,
            log: self.log.clone(),
            seen: self.seen,
        })
    }
}

/// Start a new attempt in the state file `path` (replacing the last one).
pub fn start(path: &Path, attempt: Attempt) {
    let mut state = State::load(path);
    state.last_attempt = Some(attempt);
    save(path, &state);
}

/// Change the last attempt in the state file `path`, if there is one; its
/// [`Attempt::updated_at`] becomes `now`.
pub fn update(path: &Path, now: Timestamp, change: impl FnOnce(&mut Attempt)) {
    let mut state = State::load(path);
    let Some(attempt) = state.last_attempt.as_mut() else {
        return;
    };
    change(attempt);
    attempt.updated_at = now;
    save(path, &state);
}

fn save(path: &Path, state: &State) {
    if let Err(e) = state.save(path) {
        tracing::warn!(error = %e, "could not record the update attempt");
    }
}

/// The reason for an error of the process that downloads (shown there already).
pub fn error_reason(detail: &str) -> LocalizedText {
    LocalizedText::new("selfupdate.problem.error", "{detail}").arg("detail", detail)
}

/// The old process did not exit in time: nothing was installed.
pub fn wait_timeout_reason(version: &str, waited: Duration) -> LocalizedText {
    LocalizedText::new(
        "selfupdate.problem.wait_timeout",
        "Superba did not quit within {secs} s, so the update to {version} was not installed.",
    )
    .arg("secs", waited.as_secs())
    .arg("version", version)
}

/// `msiexec` could not be started.
pub fn msiexec_start_reason(error: &str) -> LocalizedText {
    LocalizedText::new(
        "selfupdate.problem.msiexec_start",
        "The installer (msiexec) could not be started: {error}",
    )
    .arg("error", error)
}

/// `msiexec` ended with the failure `code`.
pub fn msiexec_reason(code: i32) -> LocalizedText {
    LocalizedText::new(
        "selfupdate.problem.msiexec",
        "The installer (msiexec) ended with code {code}: {meaning}",
    )
    .arg("code", code)
    .arg_text("meaning", msiexec_meaning_text(code))
}

/// The translated meaning of a Windows Installer exit code.
pub fn msiexec_meaning_text(code: i32) -> LocalizedText {
    let (english, _) = runlog::msiexec_meaning(code);
    if runlog::MSIEXEC_KNOWN_CODES.contains(&code) {
        LocalizedText::new(format!("selfupdate.msiexec.c{code}"), english)
    } else {
        LocalizedText::new("selfupdate.msiexec.unknown", english)
    }
}

/// Installed, but Windows must restart to finish (`msiexec` 3010 / 1641).
pub fn restart_pending_reason() -> LocalizedText {
    LocalizedText::new(
        "selfupdate.problem.restart_pending",
        "The update was installed; Windows must restart to finish it.",
    )
}

/// Installed, but the local router could not be started again.
pub fn router_reason(detail: &str) -> LocalizedText {
    LocalizedText::new(
        "selfupdate.problem.router",
        "The update was installed, but the local router could not be restarted ({detail}). Start it again on the Router page.",
    )
    .arg("detail", detail)
}
