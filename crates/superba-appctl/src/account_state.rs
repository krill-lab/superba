//! Keep the desktop's projects when it starts without a ChatGPT sign-in (user
//! bugs 135 and 136).
//!
//! App 26.1103 keeps part of `<CODEX_HOME>/.codex-global-state.json` per
//! signed-in account. Once a ChatGPT login has been seen, the file carries
//! [`ACCOUNT_STATE_KEY`] = `{owner, inactive}`: `owner` is a hash of the
//! signed-in user and account, `inactive` holds the account-scoped state of the
//! other owners (`atoms`: persisted atoms other than a few global ones such as
//! the onboarding flag; `globals`: top-level keys such as
//! `electron-saved-workspace-roots`, `local-projects`, `project-order`,
//! pinned threads and thread/project assignments).
//!
//! On every start the app first selects owner `null`: it moves the current
//! owner's state into `inactive` and drops every account-scoped key; when the
//! sign-in resolves it restores that owner's entry. The owner comes from the
//! claims of a ChatGPT JWT only. With a personal access token (Superba's PAT
//! mode) or an API key there is never one, so after a single ChatGPT start
//! (official or mixed login) every later token start shows no projects, and
//! projects added in such a session are dropped at the next start: nothing
//! stashes them because the owner is `null`. Without [`ACCOUNT_STATE_KEY`]
//! the app keeps the top-level state as it is (token logins never create the
//! key; a later ChatGPT start adopts the top-level state for that account).
//!
//! So before a launch with a token login (the app is not running), Superba
//! folds the stashed entries back into the top-level state (current values
//! win, nothing is overwritten), removes the key, and keeps a copy of the
//! original file in `backups/app-state/` of its data directory.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use superba_core::{CodexHome, Error, Result, Timestamp};

/// Top-level key of the app's account-scoped renderer state.
pub const ACCOUNT_STATE_KEY: &str = "electron-account-renderer-state-v1";
/// Top-level key of the app's persisted atoms.
const ATOMS: &str = "electron-persisted-atom-state";
/// Copies of the original state file kept in the backup directory.
const BACKUPS_KEPT: usize = 10;
const BACKUP_PREFIX: &str = "global-state-";

/// How the app will sign in, read from `auth.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginKind {
    /// ChatGPT tokens: the app has an account owner.
    ChatGpt,
    /// Personal access token or API key without ChatGPT tokens: no owner.
    Token,
    /// Missing, unreadable or unrecognised (credentials in a keyring, ...).
    Unknown,
}

/// Classify `auth.json` bytes.
pub fn login_kind(auth_json: &[u8]) -> LoginKind {
    let Ok(Value::Object(auth)) = serde_json::from_slice::<Value>(auth_json) else {
        return LoginKind::Unknown;
    };
    if auth.get("tokens").is_some_and(|t| !t.is_null()) {
        return LoginKind::ChatGpt;
    }
    let present = |key: &str| {
        auth.get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    if present("personal_access_token") || present("OPENAI_API_KEY") {
        LoginKind::Token
    } else {
        LoginKind::Unknown
    }
}

/// Fold the account-scoped stashes of `state` into its top-level state and
/// remove [`ACCOUNT_STATE_KEY`]. Returns the number of stashed owners folded
/// in, or `None` (state untouched) without the key or with a shape this code
/// does not know.
///
/// Values already at the top level win; a stashed value only fills what is
/// missing: objects are merged key by key (recursively), arrays get the
/// stashed items they lack appended, and anything else stays as it is.
pub fn flatten(state: &mut Value) -> Option<usize> {
    let object = state.as_object_mut()?;
    let inactive = object
        .get(ACCOUNT_STATE_KEY)?
        .as_object()?
        .get("inactive")
        .map_or(Some(Map::new()), |v| v.as_object().cloned())?;
    let mut stashes = Vec::with_capacity(inactive.len());
    for stash in inactive.into_values() {
        let Value::Object(mut stash) = stash else {
            return None;
        };
        let atoms = take_object(&mut stash, "atoms")?;
        let globals = take_object(&mut stash, "globals")?;
        stashes.push((atoms, globals));
    }
    let count = stashes.len();
    for (atoms, globals) in stashes {
        for (key, value) in globals {
            if !value.is_null() {
                merge_missing(object.entry(key).or_insert(Value::Null), value);
            }
        }
        if !atoms.is_empty() {
            merge_missing(
                object.entry(ATOMS).or_insert(Value::Null),
                Value::Object(atoms),
            );
        }
    }
    object.shift_remove(ACCOUNT_STATE_KEY);
    Some(count)
}

/// `stash[key]` as an object (absent = empty); `None` for another type.
fn take_object(stash: &mut Map<String, Value>, key: &str) -> Option<Map<String, Value>> {
    match stash.remove(key) {
        None | Some(Value::Null) => Some(Map::new()),
        Some(Value::Object(map)) => Some(map),
        Some(_) => None,
    }
}

fn merge_missing(current: &mut Value, stashed: Value) {
    match (current, stashed) {
        (current @ Value::Null, stashed) => *current = stashed,
        (Value::Object(current), Value::Object(stashed)) => {
            for (key, value) in stashed {
                if !value.is_null() {
                    merge_missing(current.entry(key).or_insert(Value::Null), value);
                }
            }
        }
        (Value::Array(current), Value::Array(stashed)) => {
            for item in stashed {
                if !current.contains(&item) {
                    current.push(item);
                }
            }
        }
        _ => {}
    }
}

/// What [`prepare`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do: a ChatGPT or unknown login, no state file, or no
    /// account-scoped state in it.
    Unchanged,
    /// Folded `stashes` owners in; the original is at `backup`.
    Flattened {
        /// Stashed owners folded into the top-level state.
        stashes: usize,
        /// Copy of the original file.
        backup: PathBuf,
    },
}

/// Before a launch into `home` (the app is not running): with a token login,
/// [`flatten`] its global state, keeping a copy of the original in
/// `backup_dir`. A file the app cannot parse is left for its `.bak` fallback
/// (an error); the app's `.bak` file is never touched.
pub fn prepare(home: &CodexHome, backup_dir: &Path, now: Timestamp) -> Result<Outcome> {
    match fs::read(home.auth_json()) {
        Ok(bytes) if login_kind(&bytes) == LoginKind::Token => {}
        Ok(_) => return Ok(Outcome::Unchanged),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Outcome::Unchanged),
        Err(e) => return Err(Error::io(home.auth_json(), &e)),
    }
    let path = state_file(&home.global_state_json())?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Outcome::Unchanged),
        Err(e) => return Err(Error::io(&path, &e)),
    };
    // The app parses the file with JSON.parse (no BOM stripping) and falls
    // back to `.bak` when that fails; such a file stays as it is.
    let mut state = serde_json::from_slice::<Value>(&bytes).map_err(|e| Error::Parse {
        what: "desktop global state".into(),
        message: e.to_string(),
    })?;
    if state.get(ACCOUNT_STATE_KEY).is_none() {
        return Ok(Outcome::Unchanged);
    }
    let Some(stashes) = flatten(&mut state) else {
        return Err(Error::invalid(
            "desktop account-scoped state has an unknown shape",
        ));
    };
    let backup = write_backup(backup_dir, &bytes, now)?;
    let permissions = fs::metadata(&path)
        .map_err(|e| Error::io(&path, &e))?
        .permissions();
    // preserve_order keeps every other key, unknown ones included, in place.
    let out = serde_json::to_vec(&state)
        .map_err(|e| Error::other(format!("serialize desktop global state: {e}")))?;
    let dir = path.parent().unwrap_or(home.root());
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(dir, &e))?;
    temp.write_all(&out)
        .map_err(|e| Error::io(temp.path(), &e))?;
    temp.as_file()
        .set_permissions(permissions)
        .map_err(|e| Error::io(temp.path(), &e))?;
    temp.as_file()
        .sync_all()
        .map_err(|e| Error::io(temp.path(), &e))?;
    temp.persist(&path)
        .map_err(|e| Error::io(&path, &e.error))?;
    Ok(Outcome::Flattened { stashes, backup })
}

/// [`prepare`], logging instead of failing: a state-file problem must never
/// keep the app from starting.
pub(crate) fn seed(home: &CodexHome, backup_dir: &Path) {
    match prepare(home, backup_dir, Timestamp::now()) {
        Ok(Outcome::Unchanged) => {}
        Ok(Outcome::Flattened { stashes, backup }) => tracing::info!(
            path = %home.global_state_json().display(),
            stashes,
            backup = %backup.display(),
            "kept the desktop's account-scoped state for a launch without a ChatGPT sign-in"
        ),
        Err(error) => tracing::warn!(
            path = %home.global_state_json().display(),
            %error,
            "left the desktop's account-scoped state alone"
        ),
    }
}

/// Write `bytes` to a new `global-state-<ms>.json` in `dir` and keep only the
/// newest [`BACKUPS_KEPT`] copies.
fn write_backup(dir: &Path, bytes: &[u8], now: Timestamp) -> Result<PathBuf> {
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    let mut ms = now.as_millis();
    let path = loop {
        let path = dir.join(format!("{BACKUP_PREFIX}{ms}.json"));
        if !path.exists() {
            break path;
        }
        ms += 1;
    };
    fs::write(&path, bytes).map_err(|e| Error::io(&path, &e))?;
    let mut kept: Vec<(i64, PathBuf)> = fs::read_dir(dir)
        .map_err(|e| Error::io(dir, &e))?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let ms = path
                .file_name()?
                .to_str()?
                .strip_prefix(BACKUP_PREFIX)?
                .strip_suffix(".json")?
                .parse()
                .ok()?;
            Some((ms, path))
        })
        .collect();
    kept.sort();
    let excess = kept.len().saturating_sub(BACKUPS_KEPT);
    for (_, old) in kept.into_iter().take(excess) {
        let _ = fs::remove_file(old);
    }
    Ok(path)
}

/// The file to read and replace: a symlinked state file is written through to
/// its target so the link survives.
fn state_file(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|e| Error::io(path, &e))
        }
        _ => Ok(path.to_path_buf()),
    }
}
