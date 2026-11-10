//! Seed the desktop's projectless onboarding flag before a fresh launch (user
//! bug 129).
//!
//! App 26.1103 shows its "what do you work on" onboarding on every start until
//! `electron-persisted-atom-state` → `electron:onboarding-projectless-completed`
//! in `<CODEX_HOME>/.codex-global-state.json` is set, and it only sets it on the
//! last onboarding step. The key is global (not account-scoped). The desktop
//! reads this file from its own home even when its app-server runs in WSL, which
//! is the launch home the controller already resolves.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;
use superba_core::{CodexHome, Error, Result};

const ATOMS: &str = "electron-persisted-atom-state";
const COMPLETED: &str = "electron:onboarding-projectless-completed";

/// [`ensure_completed`], logging instead of failing: a state-file problem must
/// never keep the app from starting.
pub(crate) fn seed(home: &CodexHome) {
    if let Err(error) = ensure_completed(home) {
        tracing::warn!(
            path = %home.global_state_json().display(),
            %error,
            "left the desktop onboarding state alone"
        );
    }
}

/// Only a missing flag is ours to seed; even `false` belongs to the app.
pub(crate) fn ensure_completed(home: &CodexHome) -> Result<()> {
    let path = state_file(&home.global_state_json())?;
    let (mut state, permissions) = match fs::read(&path) {
        Ok(bytes) => {
            // App 26.1103's global-state loader uses readFileSync("utf8") +
            // JSON.parse, without stripping a BOM. Leave invalid files intact
            // so its .bak fallback still works.
            let state = serde_json::from_slice::<Value>(&bytes).map_err(|e| Error::Parse {
                what: "desktop global state".into(),
                message: e.to_string(),
            })?;
            let permissions = fs::metadata(&path)
                .map_err(|e| Error::io(&path, &e))?
                .permissions();
            (state, Some(permissions))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The app accepts a partial object: known keys are optional and
            // its global-state getter supplies defaults for absent entries.
            // Without a primary file it starts empty and never reads `.bak`.
            (Value::Object(Default::default()), None)
        }
        Err(e) => return Err(Error::io(&path, &e)),
    };
    let object = state
        .as_object_mut()
        .ok_or_else(|| Error::invalid("desktop global state is not an object"))?;
    let atoms = object
        .entry(ATOMS)
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| Error::invalid("desktop persisted atom state is not an object"))?;
    if atoms.contains_key(COMPLETED) {
        return Ok(());
    }
    atoms.insert(COMPLETED.into(), Value::Bool(true));

    // preserve_order keeps all existing keys in place, including unknown ones.
    let bytes = serde_json::to_vec(&state)
        .map_err(|e| Error::other(format!("serialize desktop global state: {e}")))?;
    let dir = path.parent().unwrap_or(home.root());
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(dir, &e))?;
    temp.write_all(&bytes)
        .map_err(|e| Error::io(temp.path(), &e))?;
    if let Some(permissions) = permissions {
        temp.as_file()
            .set_permissions(permissions)
            .map_err(|e| Error::io(temp.path(), &e))?;
    }
    temp.as_file()
        .sync_all()
        .map_err(|e| Error::io(temp.path(), &e))?;
    // Same-directory atomic replacement (the link target's directory for a
    // symlink, which stays a link); NamedTempFile cleans up on failure.
    // Deliberately never read or write the app's .bak file.
    temp.persist(&path)
        .map_err(|e| Error::io(&path, &e.error))?;
    Ok(())
}

/// The file to read and replace: a symlinked state file is written through to
/// its target so the link survives. A dangling link is an error (left alone).
fn state_file(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|e| Error::io(path, &e))
        }
        _ => Ok(path.to_path_buf()),
    }
}

#[cfg(test)]
#[path = "../tests/unit/onboarding.rs"]
mod tests;
