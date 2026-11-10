//! Project folders the desktop app dropped (user bugs 135/136 follow-up).
//!
//! Before Superba folded the app's account-scoped state back (see
//! `superba_appctl::account_state`), app 26.1103 deleted the projects added
//! during a personal-access-token session at its next start. The folders are
//! still listed elsewhere: the app-server keeps every project it imported in
//! its state database (`projects` / `project_roots` /
//! `project_idempotency_keys` in `state_*.sqlite`), and the threads table
//! records where each chat ran. Superba lists those folders, the user ticks
//! the ones to bring back, and the next launch through Superba writes them
//! into the app's state.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// Where a candidate folder was found.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LostProjectSource {
    /// The app-server's own project list (the project the app had).
    ProjectList,
    /// Chats that ran in this folder.
    Chats {
        /// Number of chats.
        count: u32,
    },
}

/// One folder Superba offers to bring back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostProject {
    /// The folder (the project's first root).
    pub path: PathBuf,
    /// Further roots of a multi-folder project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_roots: Vec<PathBuf>,
    /// The project's name (the folder name when it had none).
    pub name: String,
    /// Last use (project update or newest chat), for the ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Timestamp>,
    /// Where it was found, strongest first.
    pub sources: Vec<LostProjectSource>,
    /// The app's own id for the project (its import idempotency key), reused
    /// so the app finds its project-server entry again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// The app-server's id of the project (`projects.id`); `None` for a
    /// folder only seen in chats. An answer about the project is kept by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

impl LostProject {
    /// From the app's project list (ticked by default); folders only seen
    /// in chats start unticked.
    pub fn recommended(&self) -> bool {
        self.sources.contains(&LostProjectSource::ProjectList)
    }

    /// Chats that ran in the folder.
    pub fn chats(&self) -> u32 {
        self.sources
            .iter()
            .map(|s| match s {
                LostProjectSource::Chats { count } => *count,
                LostProjectSource::ProjectList => 0,
            })
            .sum()
    }
}

/// The folders on offer, most recently used first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostProjects {
    /// Codex home whose app state the folders go into.
    pub codex_home: PathBuf,
    /// Candidates.
    pub candidates: Vec<LostProject>,
    /// Stable id of this list (its paths): an answer is recorded for it.
    pub fingerprint: String,
}

/// What was answered before for one Codex home: projects of the app-server's
/// list by their id (whatever their folders), and folders only seen in
/// chats that the user turned down.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostProjectsAnswered {
    /// App-server project ids.
    #[serde(default)]
    pub project_ids: Vec<String>,
    /// Folders only seen in chats.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
}

/// The user's answer to [`LostProjects`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LostProjectsAnswer {
    /// Bring these folders back at the next launch through Superba; the
    /// projects of the app-server's list left unticked are not offered
    /// again, folders only seen in chats left unticked may be.
    Restore {
        /// Ticked folders.
        paths: Vec<PathBuf>,
    },
    /// Do not offer this list again (its projects and chat folders).
    Dismiss,
}
