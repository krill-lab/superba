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
    let path = home.global_state_json();
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
    fs::create_dir_all(home.root()).map_err(|e| Error::io(home.root(), &e))?;
    let mut temp =
        tempfile::NamedTempFile::new_in(home.root()).map_err(|e| Error::io(home.root(), &e))?;
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
    // Same-directory atomic replacement; NamedTempFile cleans up on failure.
    // Deliberately never read or write the app's .bak file.
    temp.persist(&path)
        .map_err(|e| Error::io(&path, &e.error))?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/onboarding.rs"]
mod tests;
