//! Project folders the desktop app dropped (user bugs 135/136 follow-up): the
//! overview's offer ([`discover_for`]) and the user's answer.
//!
//! Discovery reads the app's `.codex-global-state.json` (which folders it
//! knows, any account), Superba's queue and answers
//! (`superba_appctl::project_restore`) and the Codex databases
//! (`superba_sessions::lost_projects`). The overview polls every few seconds,
//! so the result is cached per Codex home until one of those files changes
//! (the state databases of every directory discovery reads, `config.toml`
//! included) and for at most [`MAX_AGE`]: a change a file's size and time do
//! not show, or a candidate folder removed meanwhile, is seen within it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use sha2::{Digest, Sha256};
use superba_appctl::project_restore::{self, RestoreItem, path_key};
use superba_core::{
    CodexHome, Error, LostProjects, LostProjectsAnswer, LostProjectsAnswered, Result,
};

use crate::Switcher;

type Stamp = Vec<(PathBuf, Option<SystemTime>, u64)>;
type Cache = Mutex<BTreeMap<PathBuf, (Stamp, Instant, Option<LostProjects>)>>;

/// The longest a cached result is used.
pub const MAX_AGE: Duration = Duration::from_secs(60);

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Modification times and sizes of everything the result depends on.
fn stamp(home: &CodexHome, home_dir: &Path, app_state_dir: &Path) -> Stamp {
    let mut files = vec![
        home.global_state_json(),
        home.config_toml(),
        app_state_dir.join(project_restore::PENDING_FILE),
        app_state_dir.join(project_restore::ANSWERED_FILE),
    ];
    for dir in superba_sessions::lost_projects::database_dirs(home, home_dir) {
        for sub in [dir.clone(), dir.join("sqlite")] {
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

/// [`discover_for`], cached until one of its files changes, at most for
/// [`MAX_AGE`].
pub fn discover_cached(
    home: &CodexHome,
    home_dir: &Path,
    app_state_dir: &Path,
) -> Option<LostProjects> {
    discover_cached_for(home, home_dir, app_state_dir, MAX_AGE)
}

/// [`discover_cached`] with another longest age.
pub fn discover_cached_for(
    home: &CodexHome,
    home_dir: &Path,
    app_state_dir: &Path,
    max_age: Duration,
) -> Option<LostProjects> {
    let now = stamp(home, home_dir, app_state_dir);
    let key = home.root().to_path_buf();
    if let Ok(c) = cache().lock()
        && let Some((s, at, v)) = c.get(&key)
        && *s == now
        && at.elapsed() < max_age
    {
        return v.clone();
    }
    let value = discover_for(home, home_dir, app_state_dir);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, (now, Instant::now(), value.clone()));
    }
    value
}

/// Record `answer` for the offer with `fingerprint`: queue the ticked
/// folders; stop offering the list's app-server projects (by id), and with
/// "don't bring back" also its chat folders.
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
    // Projects of the app-server's list are settled by their id, ticked or
    // not (the dialog says so). Folders only seen in chats are settled only
    // when the whole offer is turned down: left unticked while others come
    // back, they may be offered again.
    let answered = LostProjectsAnswered {
        project_ids: current
            .candidates
            .iter()
            .filter_map(|c| c.project_id.clone())
            .collect(),
        folders: match answer {
            LostProjectsAnswer::Dismiss => current
                .candidates
                .iter()
                .filter(|c| c.project_id.is_none())
                .map(|c| c.path.clone())
                .collect(),
            LostProjectsAnswer::Restore { .. } => Vec::new(),
        },
    };
    project_restore::record_answered(app_state_dir, home.root(), &answered)?;
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
