//! Bring back project folders the desktop app dropped (user bugs 135/136
//! follow-up).
//!
//! The user picks the folders in Superba ([`queue`]); they wait in
//! `<data>/app-state/restore-projects.json` until the next launch through
//! Superba, which writes them into the app's `local-projects` (and
//! `project-order`, when the app keeps one) in the same read, backup and
//! atomic replacement as [`crate::account_state`]. A folder some project
//! already has is skipped, nothing the app has is overwritten. The app's own
//! id for the project is reused when the app-server still lists it, so the
//! app's `project/import` (keyed by that id) finds the existing entry.
//!
//! The folders already offered are remembered per Codex home in
//! `<data>/app-state/lost-projects-answered.json`, so the offer comes once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use superba_core::{Error, LostProject, Result, Timestamp};

use crate::account_state::ACCOUNT_STATE_KEY;

/// Folders waiting for the next launch.
pub const PENDING_FILE: &str = "restore-projects.json";
/// Folders offered before.
pub const ANSWERED_FILE: &str = "lost-projects-answered.json";

const LOCAL_PROJECTS: &str = "local-projects";
const PROJECT_ORDER: &str = "project-order";
/// Keys whose folders count as projects the app knows.
const ROOT_LIST_KEYS: [&str; 2] = ["electron-saved-workspace-roots", "active-workspace-roots"];

/// One folder to bring back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreItem {
    /// The project's first folder.
    pub path: PathBuf,
    /// Further folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_roots: Vec<PathBuf>,
    /// Name.
    pub name: String,
    /// The app's own id for the project, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

impl From<&LostProject> for RestoreItem {
    fn from(p: &LostProject) -> Self {
        Self {
            path: p.path.clone(),
            extra_roots: p.extra_roots.clone(),
            name: p.name.clone(),
            app_id: p.app_id.clone(),
        }
    }
}

/// The waiting folders of one Codex home.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// Codex home whose app state they go into.
    pub codex_home: PathBuf,
    /// Folders.
    pub items: Vec<RestoreItem>,
    /// When the user asked.
    pub requested_at: Timestamp,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingFile {
    #[serde(default)]
    homes: Vec<Pending>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AnsweredFile {
    /// Codex home (comparison key) → folders.
    #[serde(default)]
    homes: BTreeMap<String, Vec<PathBuf>>,
}

/// Comparison key of a folder: separators unified, trailing ones dropped,
/// case folded on Windows.
pub fn path_key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let trimmed = s.trim_end_matches('/');
    let s = if trimmed.is_empty() { "/" } else { trimmed };
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// Read a queue or answers file. A missing file is empty; so is one of an
/// unknown shape (logged): it holds nothing usable, and treating it as empty
/// lets the next write replace it instead of failing for good.
fn read_json<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "ignored a project-restore file of an unknown shape"
            );
            T::default()
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::io(path, &e)),
    }
}

/// Owner-only (Unix) atomic replacement: the files list the user's folders.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid("state file without a directory"))?;
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::other(format!("serialize {}: {e}", path.display())))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(dir, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::io(temp.path(), &e))?;
    }
    temp.write_all(&bytes)
        .map_err(|e| Error::io(temp.path(), &e))?;
    temp.as_file()
        .sync_all()
        .map_err(|e| Error::io(temp.path(), &e))?;
    temp.persist(path).map_err(|e| Error::io(path, &e.error))?;
    Ok(())
}

/// Lock file serialising every read-modify-write of the queue and the
/// answers (the GUI's answer and a launch may run at the same time, in one
/// process or two).
pub const LOCK_FILE: &str = "project-restore.lock";

/// Run `f` holding the exclusive lock of `app_state_dir` (released when the
/// file closes, also when `f` fails).
fn locked<R>(app_state_dir: &Path, f: impl FnOnce() -> Result<R>) -> Result<R> {
    fs::create_dir_all(app_state_dir).map_err(|e| Error::io(app_state_dir, &e))?;
    let path = app_state_dir.join(LOCK_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::io(&path, &e))?;
    file.lock().map_err(|e| Error::io(&path, &e))?;
    let result = f();
    drop(file);
    result
}

/// The folders waiting for `codex_home`, if any.
pub fn pending(app_state_dir: &Path, codex_home: &Path) -> Result<Option<Pending>> {
    let file: PendingFile = read_json(&app_state_dir.join(PENDING_FILE))?;
    let home = path_key(codex_home);
    Ok(file
        .homes
        .into_iter()
        .find(|p| path_key(&p.codex_home) == home && !p.items.is_empty()))
}

/// Add `items` to the folders waiting for `codex_home` (a folder already
/// waiting keeps its first entry).
pub fn queue(
    app_state_dir: &Path,
    codex_home: &Path,
    items: Vec<RestoreItem>,
    now: Timestamp,
) -> Result<()> {
    locked(app_state_dir, || {
        let path = app_state_dir.join(PENDING_FILE);
        let mut file: PendingFile = read_json(&path)?;
        let home = path_key(codex_home);
        let idx = match file
            .homes
            .iter()
            .position(|p| path_key(&p.codex_home) == home)
        {
            Some(i) => i,
            None => {
                file.homes.push(Pending {
                    codex_home: codex_home.to_path_buf(),
                    items: Vec::new(),
                    requested_at: now,
                });
                file.homes.len() - 1
            }
        };
        let entry = &mut file.homes[idx];
        entry.requested_at = now;
        for item in items {
            if !entry
                .items
                .iter()
                .any(|i| path_key(&i.path) == path_key(&item.path))
            {
                entry.items.push(item);
            }
        }
        write_json(&path, &file)
    })
}

/// Forget the folders of `done` waiting for `codex_home` (they were written,
/// or were there already). Folders queued since `done` was read stay.
pub fn clear_applied(app_state_dir: &Path, codex_home: &Path, done: &[RestoreItem]) -> Result<()> {
    locked(app_state_dir, || {
        let path = app_state_dir.join(PENDING_FILE);
        let mut file: PendingFile = read_json(&path)?;
        let home = path_key(codex_home);
        let done: BTreeSet<String> = done.iter().map(|i| path_key(&i.path)).collect();
        for entry in file
            .homes
            .iter_mut()
            .filter(|p| path_key(&p.codex_home) == home)
        {
            entry.items.retain(|i| !done.contains(&path_key(&i.path)));
        }
        file.homes.retain(|p| !p.items.is_empty());
        if file.homes.is_empty() {
            return match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::io(&path, &e)),
            };
        }
        write_json(&path, &file)
    })
}

/// The folders offered before for `codex_home`.
pub fn answered(app_state_dir: &Path, codex_home: &Path) -> Result<Vec<PathBuf>> {
    let file: AnsweredFile = read_json(&app_state_dir.join(ANSWERED_FILE))?;
    Ok(file
        .homes
        .get(&path_key(codex_home))
        .cloned()
        .unwrap_or_default())
}

/// Remember `paths` as offered for `codex_home`.
pub fn record_answered(app_state_dir: &Path, codex_home: &Path, paths: &[PathBuf]) -> Result<()> {
    locked(app_state_dir, || {
        let path = app_state_dir.join(ANSWERED_FILE);
        let mut file: AnsweredFile = read_json(&path)?;
        let list = file.homes.entry(path_key(codex_home)).or_default();
        for p in paths {
            if !list.iter().any(|q| path_key(q) == path_key(p)) {
                list.push(p.clone());
            }
        }
        write_json(&path, &file)
    })
}

/// Every folder of a project the app knows in `state` (any account): the
/// roots of `local-projects` and the workspace-root lists, at the top level
/// and in the account-scoped stashes.
pub fn known_roots(state: &Value) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut scan = |globals: &Map<String, Value>| {
        if let Some(Value::Object(projects)) = globals.get(LOCAL_PROJECTS) {
            for p in projects.values() {
                if let Some(Value::Array(roots)) = p.get("rootPaths") {
                    out.extend(roots.iter().filter_map(Value::as_str).map(PathBuf::from));
                }
            }
        }
        for key in ROOT_LIST_KEYS {
            if let Some(Value::Array(roots)) = globals.get(key) {
                out.extend(roots.iter().filter_map(Value::as_str).map(PathBuf::from));
            }
        }
    };
    let Some(top) = state.as_object() else {
        return out;
    };
    scan(top);
    if let Some(Value::Object(inactive)) =
        top.get(ACCOUNT_STATE_KEY).and_then(|a| a.get("inactive"))
    {
        for stash in inactive.values() {
            if let Some(Value::Object(globals)) = stash.get("globals") {
                scan(globals);
            }
        }
    }
    out
}

/// Write `items` into the top-level `local-projects` of `state` (and append
/// their ids to `project-order` when it is a list). Folders a top-level
/// project already has are skipped; existing entries are never changed.
/// Returns how many projects were added; `None` for a state of an unknown
/// shape (left as it is).
pub fn apply(state: &mut Value, items: &[RestoreItem], now: Timestamp) -> Option<usize> {
    let top = state.as_object_mut()?;
    match top.get(LOCAL_PROJECTS) {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        Some(_) => return None,
    }
    if !matches!(
        top.get(PROJECT_ORDER),
        None | Some(Value::Null) | Some(Value::Array(_))
    ) {
        return None;
    }
    let mut have: BTreeSet<String> = BTreeSet::new();
    let mut ids: BTreeSet<String> = BTreeSet::new();
    if let Some(Value::Object(projects)) = top.get(LOCAL_PROJECTS) {
        for (id, p) in projects {
            ids.insert(id.clone());
            if let Some(Value::Array(roots)) = p.get("rootPaths") {
                have.extend(
                    roots
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|r| path_key(Path::new(r))),
                );
            }
        }
    }
    let mut added = Vec::new();
    for item in items {
        let roots: Vec<&PathBuf> = std::iter::once(&item.path)
            .chain(&item.extra_roots)
            .collect();
        if roots.iter().any(|r| have.contains(&path_key(r))) {
            continue;
        }
        let id = project_id(item, &ids);
        ids.insert(id.clone());
        have.extend(roots.iter().map(|r| path_key(r)));
        let ms = now.as_millis();
        added.push((
            id.clone(),
            json!({
                "id": id,
                "name": item.name,
                "rootPaths": roots.iter().map(|r| r.to_string_lossy()).collect::<Vec<_>>(),
                "createdAt": ms,
                "updatedAt": ms,
            }),
        ));
    }
    if added.is_empty() {
        return Some(0);
    }
    let count = added.len();
    if !matches!(top.get(LOCAL_PROJECTS), Some(Value::Object(_))) {
        top.insert(LOCAL_PROJECTS.into(), Value::Object(Map::new()));
    }
    let order: Vec<String> = added.iter().map(|(id, _)| id.clone()).collect();
    if let Some(Value::Object(projects)) = top.get_mut(LOCAL_PROJECTS) {
        for (id, entry) in added {
            projects.insert(id, entry);
        }
    }
    if let Some(Value::Array(list)) = top.get_mut(PROJECT_ORDER) {
        for id in order {
            let id = Value::String(id);
            if !list.contains(&id) {
                list.push(id);
            }
        }
    }
    Some(count)
}

/// The app's own id when it is free, else `local-<sha256(path)[..32]>`
/// (made unique).
fn project_id(item: &RestoreItem, taken: &BTreeSet<String>) -> String {
    if let Some(id) = item.app_id.as_ref().filter(|id| !id.is_empty())
        && !taken.contains(id)
    {
        return id.clone();
    }
    let digest = Sha256::digest(item.path.to_string_lossy().as_bytes());
    let base = format!("local-{}", &hex::encode(digest)[..32]);
    let mut id = base.clone();
    let mut n = 2;
    while taken.contains(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}
