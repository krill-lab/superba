//! Coexistence with other tools that manage Codex's configuration (GH#5).
//!
//! The config engine knows what Superba owns and reports what changed since it
//! wrote it ([`ConfigState::drift`], [`ConfigState::auth_drift`]); the env
//! persister knows the value it set ([`EnvReport::superba_value`]). This module
//! puts them together into one [`ExternalChange`] for the overview (start-up
//! and the GUI's polling), attributes it with `superba_import::detect` (the
//! other tools' stores and markers, read-only; cached per state of the files),
//! and decides the defaults the issue asks for:
//!
//! - a **switch** to a profile never writes over a changed key or credential
//!   without an answer (the planner raises [`PlanChoice::OwnedKeyConflict`] with
//!   the credential and the source; "take over" answers `Overwrite` for all);
//! - a **restore**, an **uninstall** and a **hand-off** put back only what
//!   Superba still owns: every changed key and the credential are kept
//!   (`KeepCurrent`) unless the request answers otherwise, and the plan says
//!   which were kept and by whom;
//! - the router's automatic fallback never writes when Codex was moved off
//!   Superba's credential or provider (see `router::router_fall_back_impl`).

use std::path::Path;
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use superba_core::codex::ENV_AUTHAPI_BASE_URL;
use superba_core::{
    ALL_CHANGED, AUTH_JSON_PATH, ChangeSource, ConfigState, EnvDrift, EnvLocation, EnvReport,
    ExternalChange, KeyResolution, LocalizedText, ResolvedMode, ResolvedProfile, ShellKind,
};
use superba_import::detect::{Attribution, Changed, LiveFiles};
use superba_import::{Os, ScanEnv};

use crate::Switcher;
use crate::messages::text;

/// Scratch directory (under Superba's cache) for private copies of the other
/// tools' databases while attributing a change.
const SCRATCH_DIR: &str = "import";

/// The last attribution and environment check, per state of the files.
#[derive(Debug, Default)]
pub(crate) struct OwnershipCache {
    /// Fingerprint of the Codex files the entries below belong to.
    files: String,
    env: Option<Vec<EnvDrift>>,
    attribution: Option<Attribution>,
}

/// Shared cache type held by the [`Switcher`].
pub(crate) type SharedOwnershipCache = std::sync::Arc<Mutex<OwnershipCache>>;

/// The fingerprint of a state of the Codex files (their hashes).
pub(crate) fn files_fingerprint(state: &ConfigState) -> String {
    format!(
        "{}|{}",
        state.config_sha256.as_deref().unwrap_or("-"),
        state.auth_sha256.as_deref().unwrap_or("-")
    )
}

fn fingerprint(files: &str, env: &[EnvDrift]) -> String {
    let mut h = Sha256::new();
    h.update(files.as_bytes());
    for e in env {
        h.update(format!("\0{:?}={:?}", e.location, e.live).as_bytes());
    }
    hex::encode(&h.finalize()[..12])
}

/// Locations whose persisted `CODEX_AUTHAPI_BASE_URL` changed since Superba set
/// it: Superba's own blocks and stores holding another value, and the user's
/// own lines (after Superba's block, so they win) setting another value.
pub(crate) fn env_drift(report: &EnvReport) -> Vec<EnvDrift> {
    let Some(written) = report.superba_value.as_deref() else {
        return Vec::new();
    };
    let mut out: Vec<EnvDrift> = report
        .persisted
        .iter()
        .filter(|p| p.managed && p.value.as_deref() != Some(written))
        .map(|p| EnvDrift {
            var: report.var.clone(),
            location: p.location.clone(),
            written: Some(written.to_owned()),
            live: p.value.clone(),
        })
        .collect();
    for line in &report.foreign_lines {
        if line.value != written {
            out.push(EnvDrift {
                var: report.var.clone(),
                location: EnvLocation::ShellRc {
                    path: line.path.clone(),
                    shell: shell_of(&line.path),
                },
                written: Some(written.to_owned()),
                live: Some(line.value.clone()),
            });
        }
    }
    out
}

fn shell_of(path: &Path) -> ShellKind {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with(".zsh") || name.starts_with(".zprofile") || name.starts_with(".zlogin") {
        ShellKind::Zsh
    } else if name.ends_with(".fish") {
        ShellKind::Fish
    } else if name == ".profile" {
        ShellKind::Sh
    } else {
        ShellKind::Bash
    }
}

/// The destinations that hand the files back rather than take them over.
pub(crate) fn restores(resolved: &ResolvedProfile) -> bool {
    matches!(
        resolved.mode,
        ResolvedMode::Original { .. }
            | ResolvedMode::Official
            | ResolvedMode::Generation { .. }
            | ResolvedMode::Release { .. }
    )
}

/// GH#5 default of restores, uninstall and hand-offs: keep every key and the
/// credential another tool changed, unless the request answered for it. Returns
/// the paths kept this way.
pub(crate) fn keep_external(resolved: &mut ResolvedProfile, state: &ConfigState) -> Vec<String> {
    if !restores(resolved) {
        return Vec::new();
    }
    let mut kept = Vec::new();
    // What changed since Superba wrote it, also after it released everything
    // (a restore or a hand-off) and someone else took over.
    let auth = state.auth_drift.is_some() || state.since_release_auth.is_some();
    let paths = state
        .drift
        .iter()
        .chain(&state.since_release)
        .map(|k| k.path.clone())
        .chain(auth.then(|| AUTH_JSON_PATH.to_owned()));
    for path in paths {
        if kept.contains(&path) {
            continue;
        }
        match resolved.key_resolutions.get(&path) {
            Some(KeyResolution::Overwrite) => {}
            Some(KeyResolution::KeepCurrent) => kept.push(path),
            None => {
                resolved
                    .key_resolutions
                    .insert(path.clone(), KeyResolution::KeepCurrent);
                kept.push(path);
            }
        }
    }
    kept
}

/// "Kept 3 changes made by cc-switch (provider “Kimi”): ..." for a restore plan.
pub(crate) fn kept_notice(kept: &[String], source: &ChangeSource) -> LocalizedText {
    text("switch.ownership.kept")
        .arg("count", kept.len())
        .arg("items", kept.join(", "))
        .arg_text("source", source.text())
}

/// Every path changed outside Superba in `state`: owned keys, keys changed
/// since Superba released them, and [`AUTH_JSON_PATH`] for the credential.
pub fn changed_paths(state: &ConfigState) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in state.drift.iter().chain(&state.since_release) {
        if !out.contains(&k.path) {
            out.push(k.path.clone());
        }
    }
    if state.auth_drift.is_some() || state.since_release_auth.is_some() {
        out.push(AUTH_JSON_PATH.to_owned());
    }
    out
}

/// Replace the [`ALL_CHANGED`] wildcard of `request` by an answer for each
/// changed path that has none.
pub(crate) fn expand_all_changed(request: &mut superba_core::SwitchRequest, state: &ConfigState) {
    let Some(resolution) = request.key_resolutions.remove(ALL_CHANGED) else {
        return;
    };
    for path in changed_paths(state) {
        request.key_resolutions.entry(path).or_insert(resolution);
    }
}

/// Answer every part of a [`superba_core::PlanChoice::OwnedKeyConflict`] (and
/// whatever else changed by then) with "take over" (`Overwrite`).
pub fn take_over(request: &mut superba_core::SwitchRequest) {
    request
        .key_resolutions
        .insert(ALL_CHANGED.to_owned(), KeyResolution::Overwrite);
}

impl Switcher {
    /// What changed outside Superba in what it owns in the local home, with who
    /// most likely made the change; `None` when nothing did (or Superba owns
    /// nothing). The persisted environment is inspected on the first call and
    /// whenever the Codex files change (never on a timer: macOS asks
    /// `launchctl`), the other tools' stores only when something changed.
    pub(crate) async fn external_change(&self, state: &ConfigState) -> Option<ExternalChange> {
        if state.handed_off.is_some() || state.journal_pending {
            return None;
        }
        let files = files_fingerprint(state);
        let cached_env = {
            let c = self.ownership.lock().ok()?;
            (c.files == files).then(|| c.env.clone()).flatten()
        };
        let env = match cached_env {
            Some(env) => env,
            None => {
                let env = match self.deps.env.inspect(ENV_AUTHAPI_BASE_URL).await {
                    Ok(report) => env_drift(&report),
                    Err(_) => Vec::new(),
                };
                if let Ok(mut c) = self.ownership.lock() {
                    if c.files != files {
                        *c = OwnershipCache {
                            files: files.clone(),
                            ..OwnershipCache::default()
                        };
                    }
                    c.env = Some(env.clone());
                }
                env
            }
        };
        let mut change = ExternalChange {
            codex_home: state.codex_home.clone(),
            keys: state.drift.clone(),
            auth: state.auth_drift.clone(),
            env,
            fingerprint: String::new(),
            ..ExternalChange::default()
        };
        if change.is_empty() {
            return None;
        }
        change.fingerprint = fingerprint(&files, &change.env);
        let a = self.change_attribution(state).await;
        change.source = a.source;
        change.evidence = a.evidence;
        Some(change)
    }

    /// Who changed the local files (cached per state of the files).
    pub(crate) async fn change_attribution(&self, state: &ConfigState) -> Attribution {
        let files = files_fingerprint(state);
        if let Ok(c) = self.ownership.lock()
            && c.files == files
            && let Some(a) = &c.attribution
        {
            return a.clone();
        }
        let root = state.codex_home.clone();
        let keys: Vec<String> = state
            .drift
            .iter()
            .chain(&state.since_release)
            .map(|k| k.path.clone())
            .collect();
        let auth = state.auth_drift.is_some() || state.since_release_auth.is_some();
        let home = self.deps.ctx.home_dir.clone();
        let env = self.deps.ctx.env.clone();
        let scratch = self.deps.ctx.data_dir.cache_dir().join(SCRATCH_DIR);
        let attribution = tokio::task::spawn_blocking(move || {
            let config = std::fs::read(root.join("config.toml")).ok();
            let auth_json = std::fs::read(root.join("auth.json")).ok();
            let live = LiveFiles {
                codex_home: &root,
                config_toml: config.as_deref(),
                auth_json: auth_json.as_deref(),
            };
            let scan = ScanEnv {
                home: &home,
                env: env.as_ref(),
                os: Os::current(),
                scratch: &scratch,
            };
            superba_import::detect::attribute(&live, &Changed { keys: &keys, auth }, &scan)
        })
        .await
        .unwrap_or_default();
        if let Ok(mut c) = self.ownership.lock() {
            if c.files != files {
                *c = OwnershipCache {
                    files,
                    ..OwnershipCache::default()
                };
            }
            c.attribution = Some(attribution.clone());
        }
        attribution
    }
}
