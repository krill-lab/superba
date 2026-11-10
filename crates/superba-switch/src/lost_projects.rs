//! Project folders the desktop app dropped (user bugs 135/136 follow-up): the
//! overview's offer ([`discover_for`]) and the user's answer.
//!
//! Discovery reads the app's `.codex-global-state.json` (which folders it
//! knows, any account), Superba's queue and answers
//! (`superba_appctl::project_restore`) and the Codex databases
//! (`superba_sessions::lost_projects`). The overview polls every few seconds,
//! so the result is cached per Codex home until one of those files changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde_json::Value;
use sha2::{Digest, Sha256};
use superba_appctl::project_restore::{self, RestoreItem, path_key};
use superba_core::{CodexHome, Error, LostProjects, LostProjectsAnswer, Result};

use crate::Switcher;

type Stamp = Vec<(PathBuf, Option<SystemTime>, u64)>;
type Cache = Mutex<BTreeMap<PathBuf, (Stamp, Option<LostProjects>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Modification times and sizes of everything the result depends on.
fn stamp(home: &CodexHome, app_state_dir: &Path) -> Stamp {
    let mut files = vec![
        home.global_state_json(),
        app_state_dir.join(project_restore::PENDING_FILE),
        app_state_dir.join(project_restore::ANSWERED_FILE),
    ];
    for dir in [home.root(), home.sqlite_home()] {
        for sub in [dir.to_path_buf(), dir.join("sqlite")] {
            if let Ok(entries) = std::fs::read_dir(&sub) {
                let mut found: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                            n.starts_with("state_")
                                && (n.ends_with(".sqlite") || n.ends_with(".sqlite-wal"))
                        })
                    })
                    .collect();
                found.sort();
                files.extend(found);
            }
        }
    }
    files
        .into_iter()
        .map(|p| {
            let meta = std::fs::metadata(&p).ok();
            let modified = meta.as_ref().and_then(|m| m.modified().ok());
            let len = meta.map(|m| m.len()).unwrap_or(0);
            (p, modified, len)
        })
        .collect()
}

/// The folders to offer for `home`, or `None`.
pub fn discover_for(
    home: &CodexHome,
    home_dir: &Path,
    app_state_dir: &Path,
) -> Option<LostProjects> {
    // Without the app's state there is nothing it lost (and no way to tell
    // which folders it knows).
    let bytes = std::fs::read(home.global_state_json()).ok()?;
    let state: Value = serde_json::from_slice(&bytes).ok()?;
    let mut known = project_restore::known_roots(&state);
    if let Ok(Some(p)) = project_restore::pending(app_state_dir, home.root()) {
        known.extend(p.items.iter().map(|i| i.path.clone()));
    }
    let answered = project_restore::answered(app_state_dir, home.root()).unwrap_or_default();
    let candidates = superba_sessions::lost_projects::discover(home, home_dir, &known, &answered);
    if candidates.is_empty() {
        return None;
    }
    let mut keys: Vec<String> = candidates.iter().map(|c| path_key(&c.path)).collect();
    keys.sort();
    let fingerprint = hex::encode(Sha256::digest(keys.join("\n").as_bytes()));
    Some(LostProjects {
        codex_home: home.root().to_path_buf(),
        candidates,
        fingerprint,
    })
}

/// [`discover_for`], cached until one of its files changes.
pub fn discover_cached(
    home: &CodexHome,
    home_dir: &Path,
    app_state_dir: &Path,
) -> Option<LostProjects> {
    let now = stamp(home, app_state_dir);
    let key = home.root().to_path_buf();
    if let Ok(c) = cache().lock()
        && let Some((s, v)) = c.get(&key)
        && *s == now
    {
        return v.clone();
    }
    let value = discover_for(home, home_dir, app_state_dir);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, (now, value.clone()));
    }
    value
}

/// Record `answer` for the offer with `fingerprint`: queue the ticked
/// folders and stop offering every folder of the list.
pub fn answer(
    home: &CodexHome,
    home_dir: &Path,
    app_state_dir: &Path,
    fingerprint: &str,
    answer: &LostProjectsAnswer,
    now: superba_core::Timestamp,
) -> Result<usize> {
    let current = discover_for(home, home_dir, app_state_dir)
        .filter(|l| l.fingerprint == fingerprint)
        .ok_or_else(|| Error::Conflict {
            message: "the list of project folders changed; review it again".into(),
        })?;
    let mut queued = 0;
    if let LostProjectsAnswer::Restore { paths } = answer {
        let wanted: Vec<String> = paths.iter().map(|p| path_key(p)).collect();
        let items: Vec<RestoreItem> = current
            .candidates
            .iter()
            .filter(|c| wanted.contains(&path_key(&c.path)))
            .map(RestoreItem::from)
            .collect();
        queued = items.len();
        if !items.is_empty() {
            project_restore::queue(app_state_dir, home.root(), items, now)?;
        }
    }
    let all: Vec<PathBuf> = current.candidates.iter().map(|c| c.path.clone()).collect();
    project_restore::record_answered(app_state_dir, home.root(), &all)?;
    Ok(queued)
}

impl Switcher {
    pub(crate) async fn lost_projects_overview(&self, home: &CodexHome) -> Option<LostProjects> {
        let home = home.clone();
        let home_dir = self.deps.ctx.home_dir.clone();
        let dir = self.deps.ctx.data_dir.app_state_dir();
        tokio::task::spawn_blocking(move || discover_cached(&home, &home_dir, &dir))
            .await
            .ok()
            .flatten()
    }

    pub(crate) async fn answer_lost_projects_impl(
        &self,
        fingerprint: String,
        reply: LostProjectsAnswer,
    ) -> Result<()> {
        let home = self.deps.ctx.codex_home();
        let home_dir = self.deps.ctx.home_dir.clone();
        let dir = self.deps.ctx.data_dir.app_state_dir();
        let now = self.deps.clock.now();
        let queued = tokio::task::spawn_blocking(move || {
            answer(&home, &home_dir, &dir, &fingerprint, &reply, now)
        })
        .await
        .map_err(|e| Error::other(format!("lost projects: {e}")))??;
        tracing::info!(queued, "answered the offer of dropped project folders");
        Ok(())
    }
}
