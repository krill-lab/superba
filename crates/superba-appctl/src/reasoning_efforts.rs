//! Offer the Max reasoning effort in the desktop app's model controls with a
//! token sign-in (an API key or a personal access token).
//!
//! App 26.1106 shows only the reasoning efforts listed in its persisted atom
//! [`EFFORTS`] ("Available reasoning efforts" under Settings → General). Its
//! default leaves Max out, so Krill's gpt-6.1-sol, whose catalog entry lists
//! low … max … ultra, shows no Max. The settings row is only shown when the
//! app has the list of efforts its models support, which it may not have with
//! a token sign-in, so the user may have no way to turn it on.
//!
//! The app keeps persisted atoms per owner, under
//! `electron-owner-atoms:<owner>`; a token sign-in is the owner
//! [`SIGNED_OUT_OWNER`]. Before a launch with a token sign-in (the app is not
//! running), Superba writes the app's default plus Max there, only when the
//! atom is missing: any value the app or the user stored stays as it is. A
//! ChatGPT sign-in reads its account's own setting and is left alone.

use serde_json::Value;
use superba_core::{CodexHome, Result};

use crate::account_state::{LoginKind, SIGNED_OUT_OWNER, login_kind};
use crate::onboarding::edit_global_state;

/// The persisted atom holding the efforts the model controls offer.
pub(crate) const EFFORTS: &str = "enabled-reasoning-efforts";
/// Prefix of the per-owner persisted atoms (app 26.1106).
const OWNER_ATOMS: &str = "electron-owner-atoms:";
/// Persisted atoms before they are moved to an owner.
const ATOMS: &str = "electron-persisted-atom-state";
/// App 26.1106's default (`["low","medium","high","xhigh","ultra","persistent"]`)
/// with Max in its place.
pub(crate) const WITH_MAX: [&str; 7] = [
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "ultra",
    "persistent",
];

/// [`ensure_max`] for a launch, logging instead of failing: a state-file
/// problem must never keep the app from starting.
pub(crate) fn seed(home: &CodexHome) {
    match std::fs::read(home.auth_json()) {
        Ok(bytes) if login_kind(&bytes) == LoginKind::Token => {}
        _ => return,
    }
    match ensure_max(home) {
        Ok(true) => tracing::info!("offered the Max reasoning effort in the app"),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            path = %home.global_state_json().display(),
            %error,
            "left the app's reasoning efforts alone"
        ),
    }
}

/// Write [`WITH_MAX`] as the signed-out owner's [`EFFORTS`] when neither that
/// owner nor the not-yet-owned atoms hold a value. Returns whether it wrote.
pub(crate) fn ensure_max(home: &CodexHome) -> Result<bool> {
    let mut wrote = false;
    edit_global_state(home, |object| {
        let owner_key = format!("{OWNER_ATOMS}{SIGNED_OUT_OWNER}");
        let present = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_object)
                .is_some_and(|atoms| atoms.contains_key(EFFORTS))
        };
        if present(&owner_key) || present(ATOMS) {
            return Ok(false);
        }
        let atoms = object
            .entry(owner_key)
            .or_insert_with(|| Value::Object(Default::default()));
        let Some(atoms) = atoms.as_object_mut() else {
            // Not the shape this code knows: leave it to the app.
            return Ok(false);
        };
        atoms.insert(
            EFFORTS.into(),
            Value::Array(WITH_MAX.iter().map(|e| Value::from(*e)).collect()),
        );
        wrote = true;
        Ok(true)
    })?;
    Ok(wrote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn home_with(state: Option<&str>, auth: &str) -> (tempfile::TempDir, CodexHome) {
        let tmp = tempfile::tempdir().unwrap();
        let home = CodexHome::new(tmp.path());
        if let Some(state) = state {
            fs::write(home.global_state_json(), state).unwrap();
        }
        fs::write(home.auth_json(), auth).unwrap();
        (tmp, home)
    }

    fn state(home: &CodexHome) -> Value {
        serde_json::from_slice(&fs::read(home.global_state_json()).unwrap()).unwrap()
    }

    const API_KEY: &str = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"x"}"#;

    #[test]
    fn adds_max_for_the_signed_out_owner_keeping_other_atoms() {
        let (_t, home) = home_with(
            Some(
                r#"{"electron-renderer-content-owner":"signed-out","electron-owner-atoms:signed-out":{"prompt-history":[1]},"z":1}"#,
            ),
            API_KEY,
        );
        seed(&home);
        let s = state(&home);
        assert_eq!(
            s["electron-owner-atoms:signed-out"],
            serde_json::json!({"prompt-history":[1],"enabled-reasoning-efforts":WITH_MAX})
        );
        assert_eq!(s["z"], 1);
        assert_eq!(s["electron-renderer-content-owner"], "signed-out");
    }

    #[test]
    fn creates_the_owner_atoms_in_a_new_or_missing_file() {
        let (_t, home) = home_with(None, API_KEY);
        assert!(ensure_max(&home).unwrap());
        assert_eq!(
            state(&home),
            serde_json::json!({"electron-owner-atoms:signed-out":{"enabled-reasoning-efforts":WITH_MAX}})
        );
    }

    #[test]
    fn a_stored_choice_is_byte_for_byte_unchanged() {
        for before in [
            r#"{"electron-owner-atoms:signed-out":{"enabled-reasoning-efforts":["low"]}}"#,
            r#"{"electron-owner-atoms:signed-out":{"enabled-reasoning-efforts":[]}}"#,
            r#"{"electron-persisted-atom-state":{"enabled-reasoning-efforts":["high"]}}"#,
            r#"{"electron-owner-atoms:signed-out":"odd"}"#,
        ] {
            let (_t, home) = home_with(Some(before), API_KEY);
            assert!(!ensure_max(&home).unwrap(), "{before}");
            assert_eq!(
                fs::read_to_string(home.global_state_json()).unwrap(),
                before
            );
        }
    }

    #[test]
    fn only_token_sign_ins() {
        for auth in [
            r#"{"auth_mode":"chatgpt","tokens":{}}"#,
            r#"{"tokens":{"id_token":"x"}}"#,
            "not json",
        ] {
            let (_t, home) = home_with(Some("{}"), auth);
            seed(&home);
            assert_eq!(fs::read_to_string(home.global_state_json()).unwrap(), "{}");
        }
        let (_t, home) = home_with(Some("{}"), r#"{"personal_access_token":"x"}"#);
        seed(&home);
        assert_eq!(
            state(&home)["electron-owner-atoms:signed-out"]["enabled-reasoning-efforts"],
            serde_json::json!(WITH_MAX)
        );
    }

    #[test]
    fn an_unreadable_file_is_left_alone() {
        let (_t, home) = home_with(Some("{broken"), API_KEY);
        assert!(ensure_max(&home).is_err());
        seed(&home);
        assert_eq!(
            fs::read_to_string(home.global_state_json()).unwrap(),
            "{broken"
        );
    }
}
