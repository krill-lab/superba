//! Scoped remote renders (v0.3): a switch changes a following host's *connection*
//! only ([`RemoteApplyScope::Connection`]) and a settings sync pushes Codex
//! *settings* only ([`RemoteApplyScope::Settings`]); [`RemoteApplyScope::Both`] is
//! the 0.2 behaviour.
//!
//! Key families: a path [`superba_core::reserved_owner`] names (routing keys,
//! `model`, `model_reasoning_effort`, the model catalog / window / compaction /
//! `service_tier` keys, provider tables) belongs to the connection; every other
//! path is a Codex setting.
//!
//! The renderer ([`ConfigEngine::render`]) is reused unchanged, only through its
//! documented contract:
//!
//! - **Connection**: the profile's settings are not passed in and owned setting
//!   keys are not presented as owned, so they are "not ours, not managed" and
//!   stay exactly as they are (and stay owned in the local per-host record).
//! - **Settings**: exactly the paths listed in `desired.settings` change (a host
//!   sync passes the rows the user kept ticked): `Set` writes the value, `Unset`
//!   removes the key. An owned setting key the caller resolves in
//!   `desired.key_resolutions` without listing it is released instead (a sync's
//!   "remove" rows): it goes back to the host's own value from before Superba
//!   took it over, or is removed if the host had none. Owned setting keys that
//!   are neither listed nor resolved stay as they are (unticked rows); a listed
//!   row is the user's explicit choice, so a host value edited by hand since the
//!   last sync is overwritten ([`KeyResolution::Overwrite`] unless the caller
//!   resolved it). The profile's
//!   model keys are not passed in and owned connection keys are not presented as
//!   owned; the connection keys the token mode would still set
//!   (`model_provider`, `openai_base_url`, ...) are presented as drifted owned
//!   keys resolved [`KeyResolution::KeepCurrent`], which the renderer never
//!   changes. `auth.json` and the PAT environment are never written in this
//!   scope.
//! - Either way a second check refuses a render that would still change a key of
//!   the other family (the renderer may grow new owned keys).
//!
//! Ownership: a render decides only about the keys in its view ([`in_view`]);
//! every other key keeps its record in the host's state.
//!
//! The model catalog file path (`ResolvedProfile::model_catalog`) is a local path
//! and is never written to a remote host (no model catalog on hosts in 0.3).

use std::collections::BTreeSet;

use superba_core::{
    CodexSettings, ConfigEngine, Error, FileChange, KeyResolution, KeySnapshot, ModelDefaults,
    RemoteApplyScope, RenderInput, RenderedFiles, ResolvedMode, ResolvedProfile, Result,
    reserved_owner,
};

/// "Last written" value that can never equal a TOML value (a raw NUL is not
/// valid TOML), so a protected key always reads as drifted.
const KEEP_SENTINEL: &str = "\u{0}superba:keep";

/// Text key of the renderer's note for a drifted key kept as it is.
const DRIFT_KEPT_NOTE: &str = "config.note.drift_kept";

/// The family a dotted config path belongs to: [`RemoteApplyScope::Connection`]
/// for reserved paths, [`RemoteApplyScope::Settings`] for Codex settings.
pub fn family_of(path: &str) -> RemoteApplyScope {
    if reserved_owner(path).is_some() {
        RemoteApplyScope::Connection
    } else {
        RemoteApplyScope::Settings
    }
}

/// True if a scope writes keys of `path`'s family.
pub fn writes(scope: RemoteApplyScope, path: &str) -> bool {
    match scope {
        RemoteApplyScope::Connection | RemoteApplyScope::Settings => family_of(path) == scope,
        _ => true,
    }
}

/// Why `desired` cannot be applied in `scope`, if it cannot.
pub(crate) fn refusal(desired: &ResolvedProfile, scope: RemoteApplyScope) -> Option<String> {
    match (scope, &desired.mode) {
        (RemoteApplyScope::Settings, ResolvedMode::Token { .. }) => None,
        (RemoteApplyScope::Settings, _) => Some(
            "only a provider profile (Krill or a compatible site) has Codex settings to sync"
                .to_owned(),
        ),
        _ => None,
    }
}

/// The desired state the renderer sees for `scope` (no local model catalog
/// path).
fn scoped_desired(desired: &ResolvedProfile, scope: RemoteApplyScope) -> ResolvedProfile {
    let mut d = desired.clone();
    d.model_catalog = None;
    match scope {
        RemoteApplyScope::Connection => d.settings = CodexSettings::new(),
        RemoteApplyScope::Settings => d.model = ModelDefaults::default(),
        _ => {}
    }
    d
}

/// True if a render of `desired` in `scope` decides whether Superba owns `path`
/// afterwards: the scope's key family, and in the settings scope only the paths
/// `desired` lists (written or removed) or resolves without listing (released).
/// Keys outside the view keep their ownership record.
pub(crate) fn in_view(scope: RemoteApplyScope, desired: &ResolvedProfile, path: &str) -> bool {
    writes(scope, path)
        && (scope != RemoteApplyScope::Settings
            || desired.settings.contains(path)
            || desired.key_resolutions.contains_key(path))
}

/// Owned keys the renderer may treat as owned in `scope` (Settings: only the
/// listed and released paths, so other owned settings are left alone).
fn scoped_last_written(
    last_written: &[KeySnapshot],
    scope: RemoteApplyScope,
    desired: &ResolvedProfile,
) -> Vec<KeySnapshot> {
    last_written
        .iter()
        .filter(|k| in_view(scope, desired, &k.path))
        .cloned()
        .collect()
}

/// Render `input` limited to `scope` (see the module docs).
pub(crate) fn render(
    engine: &dyn ConfigEngine,
    input: &RenderInput<'_>,
    scope: RemoteApplyScope,
) -> Result<RenderedFiles> {
    let mut desired = scoped_desired(input.desired, scope);
    if !matches!(
        scope,
        RemoteApplyScope::Connection | RemoteApplyScope::Settings
    ) {
        return engine.render(&RenderInput {
            desired: &desired,
            ..*input
        });
    }
    let mut last_written = scoped_last_written(input.last_written.unwrap_or(&[]), scope, &desired);
    if scope == RemoteApplyScope::Settings {
        let listed: Vec<String> = desired.settings.paths().map(str::to_owned).collect();
        for path in listed {
            desired
                .key_resolutions
                .entry(path)
                .or_insert(KeyResolution::Overwrite);
        }
    }
    let first = engine.render(&RenderInput {
        desired: &desired,
        last_written: Some(&last_written),
        ..*input
    })?;
    let protect: BTreeSet<String> = first
        .key_changes
        .iter()
        .filter(|k| !writes(scope, &k.path))
        .map(|k| k.path.clone())
        .collect();
    let mut rendered = if protect.is_empty() {
        first
    } else {
        for path in &protect {
            desired
                .key_resolutions
                .insert(path.clone(), KeyResolution::KeepCurrent);
            last_written.retain(|k| &k.path != path);
            last_written.push(KeySnapshot {
                path: path.clone(),
                value: Some(KEEP_SENTINEL.to_owned()),
            });
        }
        let mut second = engine.render(&RenderInput {
            desired: &desired,
            last_written: Some(&last_written),
            ..*input
        })?;
        second.notes.retain(|n| {
            !(n.text.key == DRIFT_KEPT_NOTE
                && n.text
                    .args
                    .iter()
                    .any(|(name, value)| name == "key" && protect.contains(value)))
        });
        second
    };
    if let Some(stray) = rendered
        .key_changes
        .iter()
        .find(|k| !writes(scope, &k.path))
    {
        return Err(Error::Conflict {
            message: format!(
                "{} would change outside the scope of this remote change; nothing was planned",
                stray.path
            ),
        });
    }
    if scope == RemoteApplyScope::Settings {
        rendered.auth_json = FileChange::Unchanged;
        rendered.auth_change = None;
    }
    Ok(rendered)
}
