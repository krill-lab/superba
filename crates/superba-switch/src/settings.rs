//! The settings page and `spb settings` through the service: catalog rows for a
//! profile ([`superba_core::SwitchService::settings_view`]) and edits of the
//! settings a profile manages ([`superba_core::SwitchService::edit_settings`]).
//! Filtering and validation rules are `superba-config`'s settings engine; this
//! module only gathers the inputs (local Codex versions, `config.toml`, catalog).

use std::collections::BTreeMap;

use superba_config::settings::{
    HomeInput, apply_migrations, find_migrations, key_support, read_config_values, validate_value,
};
use superba_core::{
    CatalogKey, CodexSettings, ConfigState, Error, Notice, Profile, ProfileId, ProfileSet, Result,
    SettingKind, SettingRow, SettingsConsumer, SettingsEdit, SettingsEditReport, SettingsView,
    SettingsViewRequest, TargetId,
};

use crate::Switcher;
use crate::messages::text;
use crate::planner::{local_consumers, owned_setting_paths};

/// `path` equals one of `filters` or lies below one (on a `.` boundary); no
/// filters match everything.
fn matches(path: &str, filters: &[String]) -> bool {
    filters.is_empty()
        || filters.iter().any(|f| {
            let f = f.trim_end_matches('.');
            path == f
                || path
                    .strip_prefix(f)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
}

/// The profile to show or edit: the requested one, else the applied key-based
/// profile, else the last selected one, else the first key-based profile.
pub(crate) fn pick_profile<'a>(
    set: &'a ProfileSet,
    requested: Option<&ProfileId>,
    state: Option<&ConfigState>,
) -> Result<&'a Profile> {
    if let Some(id) = requested {
        let p = set.profile(id).ok_or_else(|| Error::NotFound {
            what: format!("profile {id}"),
        })?;
        if p.is_builtin() {
            return Err(Error::invalid(format!(
                "built-in profile {id} has no Codex settings of its own"
            )));
        }
        return Ok(p);
    }
    let usable = |id: Option<&ProfileId>| {
        id.and_then(|id| set.profile(id))
            .filter(|p| !p.is_builtin())
    };
    usable(
        state
            .and_then(|s| s.active.as_ref())
            .and_then(|a| a.profile_id.as_ref()),
    )
    .or_else(|| usable(set.last_selected.as_ref()))
    .or_else(|| set.profiles.iter().find(|p| !p.is_builtin()))
    .ok_or_else(|| Error::NotFound {
        what: "a profile (add one first)".into(),
    })
}

impl Switcher {
    async fn local_settings_inputs(&self) -> (Vec<SettingsConsumer>, Option<Vec<u8>>) {
        let d = &self.deps;
        let facts = self.monitor.facts().await;
        let installs = facts.installs.clone();
        // The app's pinned Codex reads the home when one is set.
        let installs = match d.profiles.load().await {
            Ok(set) => self.effective_installs(installs, &set).await,
            Err(_) => installs,
        };
        let binaries = facts.binaries.clone();
        let consumers = local_consumers(None, &installs, &binaries);
        let bytes = tokio::fs::read(d.ctx.codex_home().config_toml()).await.ok();
        (consumers, bytes)
    }

    pub(crate) async fn settings_view_impl(
        &self,
        req: SettingsViewRequest,
    ) -> Result<SettingsView> {
        let d = &self.deps;
        let set = d.profiles.load().await?;
        let state = d.config.read_state(&d.ctx.codex_home()).await.ok();
        let profile = pick_profile(&set, req.profile_id.as_ref(), state.as_ref())?;
        let managed = profile.codex_settings.clone();
        let (consumers, bytes) = self.local_settings_inputs().await;
        let version = consumers.iter().find_map(|c| c.version.clone());
        let snapshot = d.catalog.snapshot(version.as_ref());
        let current = read_config_values(bytes.as_deref()).unwrap_or_default();
        let mut notes = Vec::new();
        let mut rows: BTreeMap<String, SettingRow> = BTreeMap::new();
        // Remote hosts the app knows, with the Codex version of their last probe
        // (what a settings sync would skip there).
        let cache = self.cached_hosts();
        let hosts: Vec<(
            SettingsConsumer,
            std::sync::Arc<dyn superba_core::CatalogSource>,
        )> = self
            .app_hosts(None, None)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|h| {
                let state = cache.get(&h.target_id)?.state.clone()?;
                Some((
                    SettingsConsumer::new(
                        superba_core::ConsumerKind::Remote {
                            target_id: h.target_id.clone(),
                        },
                        h.label.clone(),
                        state.codex_version.clone(),
                    ),
                    (d.remote_catalog)(&state.os),
                ))
            })
            .collect();
        let host_support = |path: &str| -> Vec<superba_core::ConsumerSupport> {
            hosts
                .iter()
                .flat_map(|(c, cat)| key_support(path, std::slice::from_ref(c), cat.as_ref()))
                .collect()
        };
        let common = d.common_settings.clone();
        let row = |path: &str, key: Option<CatalogKey>, advanced: bool| SettingRow {
            common: common.iter().any(|c| c == path),
            host_support: host_support(path),
            path: path.to_owned(),
            key,
            feature: snapshot
                .as_ref()
                .and_then(|s| s.feature_for_path(path).cloned()),
            zh: (d.explain_zh)(path),
            current: current.get(path).map(|v| v.to_toml_repr()),
            managed: managed.get(path).cloned(),
            support: key_support(path, &consumers, d.catalog.as_ref()),
            advanced,
        };
        match &snapshot {
            Some(snap) => {
                if snap.matched.is_inexact() {
                    notes.push(Notice::info(match &version {
                        Some(installed) => text("switch.settings.catalog_inexact")
                            .arg("version", &snap.codex_version)
                            .arg("installed", installed),
                        None => text("switch.settings.catalog_no_codex")
                            .arg("version", &snap.codex_version),
                    }));
                }
                let shown = |path: &str, advanced: bool| {
                    matches(path, &req.paths)
                        && (req.advanced
                            || !advanced
                            || managed.contains(path)
                            || current.contains_key(path))
                };
                for (path, key) in &snap.keys {
                    // Map templates (`model_providers.<name>.*`) are not settings rows.
                    if key.under_map || path.contains('<') {
                        continue;
                    }
                    let feature = snap.feature_for_path(path);
                    let advanced =
                        key.deprecated_hint || feature.is_some_and(|f| !f.stage.in_default_view());
                    if shown(path, advanced) {
                        rows.insert(path.clone(), row(path, Some(key.clone()), advanced));
                    }
                }
                for feature in snap.features.values() {
                    let path = &feature.config_path;
                    if rows.contains_key(path) {
                        continue;
                    }
                    let advanced = !feature.stage.in_default_view();
                    if shown(path, advanced) {
                        let mut key = CatalogKey::new(path.clone(), SettingKind::Boolean);
                        key.default = Some(feature.default_enabled.into());
                        key.description = feature.menu_description.clone();
                        rows.insert(path.clone(), row(path, Some(key), advanced));
                    }
                }
            }
            None => notes.push(Notice::info(text("switch.settings.no_catalog"))),
        }
        // Managed keys the catalog does not know are listed too.
        for (path, _) in managed.iter() {
            if !rows.contains_key(path) && matches(path, &req.paths) {
                rows.insert(path.to_owned(), row(path, None, false));
            }
        }
        Ok(SettingsView {
            profile_id: profile.id.clone(),
            catalog_version: snapshot.as_ref().map(|s| s.codex_version.clone()),
            catalog_match: snapshot.as_ref().map(|s| s.matched.clone()),
            consumers,
            rows: rows.into_values().collect(),
            notes,
        })
    }

    pub(crate) async fn edit_settings_impl(
        &self,
        edit: SettingsEdit,
    ) -> Result<SettingsEditReport> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let state = d.config.read_state(&d.ctx.codex_home()).await.ok();
        let id = pick_profile(&set, edit.profile_id.as_ref(), state.as_ref())?
            .id
            .clone();
        let (consumers, bytes) = self.local_settings_inputs().await;
        let version = consumers.iter().find_map(|c| c.version.clone());
        let snapshot = d.catalog.snapshot(version.as_ref());
        let mut notes = Vec::new();
        let owned = state
            .as_ref()
            .map(|s| owned_setting_paths(s, &set))
            .unwrap_or_default();

        // The profile's own settings are the starting point.
        let profile = set
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::NotFound {
                what: format!("profile {id}"),
            })?;
        let mut settings: CodexSettings = profile.codex_settings.clone();

        for (path, value) in &edit.set {
            let key = snapshot.as_ref().and_then(|s| {
                s.key(path).cloned().or_else(|| {
                    s.feature_for_path(path)
                        .filter(|f| &f.config_path == path)
                        .map(|_| CatalogKey::new(path.clone(), SettingKind::Boolean))
                })
            });
            match (&key, snapshot.is_some()) {
                (Some(k), _) => {
                    if let Err(expected) = validate_value(k, value) {
                        if !edit.force {
                            return Err(Error::invalid(format!(
                                "{path}: expected {expected}, got {value}"
                            )));
                        }
                        notes.push(Notice::warning(
                            text("switch.settings.forced_value")
                                .arg("path", path)
                                .arg("expected", &expected),
                        ));
                    }
                }
                (None, true) if !edit.force => {
                    return Err(Error::invalid(format!(
                        "{path} is not a Codex setting of this Codex version (use --force to store it anyway)"
                    )));
                }
                (None, _) => notes.push(Notice::warning(
                    text("switch.settings.unknown_key").arg("path", path),
                )),
            }
            // Every other Codex reading this home must accept the value too:
            // the settings plan skips it for that home otherwise.
            for c in consumers
                .iter()
                .filter(|c| c.version.is_some() && c.version != version)
            {
                if let Some(other) = d.catalog.snapshot(c.version.as_ref())
                    && let Some(k) = other.key(path)
                    && let Err(expected) = validate_value(k, value)
                {
                    notes.push(Notice::warning(
                        text("switch.settings.value_not_for_consumer")
                            .arg("path", path)
                            .arg("consumer", consumer_text(c))
                            .arg("expected", &expected),
                    ));
                }
            }
            settings.set(path, value.clone())?;
            for s in key_support(path, &consumers, d.catalog.as_ref()) {
                if !s.support.may_apply() {
                    notes.push(Notice::info(
                        text("switch.settings.not_supported")
                            .arg("path", path)
                            .arg("consumer", &s.label)
                            .arg_text("support", s.support.text()),
                    ));
                }
            }
        }
        for path in &edit.reset {
            settings.unset(path)?;
        }
        for path in &edit.forget {
            if settings.forget(path).is_none() {
                notes.push(Notice::info(
                    text("switch.settings.not_managed").arg("path", path),
                ));
            }
        }
        if !edit.accept_migrations.is_empty() {
            let local = TargetId::local();
            let input = HomeInput::new(&local, &consumers, d.catalog.as_ref())
                .config_toml(bytes.as_deref())
                .owned_paths(&owned);
            let offered = find_migrations(&settings, &input);
            let accepted: Vec<_> = offered
                .into_iter()
                .filter(|m| edit.accept_migrations.contains(&m.legacy_path))
                .collect();
            for legacy in &edit.accept_migrations {
                if !accepted.iter().any(|m| &m.legacy_path == legacy) {
                    return Err(Error::NotFound {
                        what: format!("a migration offered for {legacy}"),
                    });
                }
            }
            apply_migrations(&mut settings, &accepted)?;
        }
        profile.codex_settings = settings;
        let saved = profile.clone();
        d.profiles.save(&set).await?;
        Ok(SettingsEditReport {
            profile: saved,
            notes,
        })
    }
}

/// "ChatGPT app 0.162.0" / "CLI": a consumer for messages.
fn consumer_text(c: &superba_core::SettingsConsumer) -> String {
    match &c.version {
        Some(v) => format!("{} {v}", c.label),
        None => c.label.clone(),
    }
}
