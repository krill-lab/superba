//! The model Codex runs with stays the same across a change of authentication
//! (user bug 58).
//!
//! Codex chooses a model itself when `config.toml` names none, and how depends
//! on the authentication (Codex 0.162-0.163, desktop app 26.1013):
//!
//! - With a personal access token (ChatGPT mode) Codex fetches the provider's
//!   own list (`<openai_base_url>/models?client_version=..`) and uses its
//!   default: the entries sorted by `priority`, the first one shown in the
//!   picker (`visibility = "list"`; `models-manager`,
//!   `ModelPreset::mark_default_by_picker_visibility`). [`provider_default`]
//!   is that rule.
//! - With an API key and `openai_base_url` set Codex never fetches the list:
//!   the app-server offers the Codex binary's bundled list (or the
//!   `model_catalog_json` file), and the desktop app keeps a default only when
//!   it passes the app's own filters, else it falls back to `gpt-5.5` (renderer
//!   `listModelsData.defaultModel ?? "gpt-5.5"`). Superba cannot predict that
//!   choice.
//!
//! So turning the router on (Codex in API-key mode against the local router)
//! moved the app from the provider's default (`gpt-6.1-sol`) to `gpt-5.5`
//! without a word, and a relay without capacity for that model left the turn
//! retrying for minutes.
//!
//! The rule ([`Switcher::plan_model_keep`]): a local switch that stays on the
//! applied site, whose destination sets no model and leaves none in
//! `config.toml`, and that changes how Codex chooses (personal access token <->
//! API key, direct <-> router) or would drop a model kept earlier, writes the
//! model Codex runs with now ([`decide`]), names it in the plan (the preview row
//! says "Keeps the model in use") and says how to choose another. The written
//! value is Superba's (`ConfigState::model_owned`); [`KEPT_MODEL_FILE`]
//! remembers that it was kept (not a profile's model the user has removed since),
//! so a later switch keeps it again while it is still needed and drops it, back
//! to the user's own value, once the destination runs the same model by itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    AuthMode, CodexHome, CodexVersion, ConfigState, DataDir, Error, LocalizedText,
    ModelListRequest, ModelListSource, ModelSpec, ModelVisibility, Notice, ResolvedMode, Result,
    Secret, Site,
};

use crate::Switcher;
use crate::messages::text;

/// The data-dir file recording, per Codex home, the model Superba kept.
pub const KEPT_MODEL_FILE: &str = "kept-model.json";

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

/// How Codex chooses a model when `config.toml` names none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelForm {
    /// A personal access token: the default of the provider's own list.
    ProviderList,
    /// An API key (direct or through the router): Codex's bundled list or the
    /// catalog file, with the desktop app's own choice of a default.
    ApiKey,
}

impl ModelForm {
    /// The form of a key-based auth mode (`None` for the official login).
    pub fn of(auth: AuthMode) -> Option<ModelForm> {
        if !auth.uses_key() {
            None
        } else if auth.needs_authapi_env() {
            Some(ModelForm::ProviderList)
        } else {
            Some(ModelForm::ApiKey)
        }
    }
}

/// The model Codex uses in ChatGPT mode (personal access token) when
/// `config.toml` names none: the provider's entries sorted by priority (stable;
/// entries without one last), the first one shown in the picker. `None` when no
/// entry is shown (Codex then mixes in its bundled list).
pub fn provider_default(models: &[ModelSpec]) -> Option<String> {
    let mut sorted: Vec<&ModelSpec> = models.iter().collect();
    sorted.sort_by_key(|m| m.priority.unwrap_or(i64::MAX));
    sorted
        .into_iter()
        .find(|m| m.visibility == ModelVisibility::List && !m.slug.is_empty())
        .map(|m| m.slug.clone())
}

/// A switch that may change the model Codex chooses by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeepScope {
    /// How Codex chooses now.
    pub from: ModelForm,
    /// How Codex chooses after the switch.
    pub to: ModelForm,
    /// The model Superba kept earlier, which `config.toml` holds now.
    pub kept: Option<String>,
}

/// Inputs of [`keep_scope`].
#[derive(Clone, Copy, Debug)]
pub struct ScopeInput<'a> {
    /// The local home now.
    pub state: &'a ConfigState,
    /// The model [`kept_model`] reports for it.
    pub kept: Option<&'a str>,
    /// The destination's site.
    pub dest_site: &'a superba_core::SiteId,
    /// The destination's auth mode for the local home (API key when routed).
    pub dest_auth: AuthMode,
    /// The destination sets a model itself (profile, mapped or replacement name).
    pub dest_sets_model: bool,
    /// The `model` value `config.toml` keeps when the destination sets none
    /// ([`crate::planner::model_left_to_config`]).
    pub left: Option<&'a str>,
}

/// Whether a switch can change the model Codex chooses by itself (pure): the
/// destination stays on the applied site, sets no model and leaves none in
/// `config.toml`, and either the way Codex chooses changes or a kept model would
/// be dropped.
pub fn keep_scope(i: ScopeInput<'_>) -> Option<KeepScope> {
    if i.dest_sets_model || i.left.is_some() || i.state.handed_off.is_some() {
        return None;
    }
    let active = i.state.active.as_ref()?;
    if active.site_id.as_ref() != Some(i.dest_site) {
        return None;
    }
    let from = ModelForm::of(active.auth_mode?)?;
    let to = ModelForm::of(i.dest_auth)?;
    let kept = i.kept.map(str::to_owned);
    // A model in config.toml that Superba did not keep: the profile's own,
    // since removed from the profile (the user chose Codex's default).
    if i.state.model.is_some() && kept.is_none() {
        return None;
    }
    (from != to || kept.is_some()).then_some(KeepScope { from, to, kept })
}

/// What [`decide`] needs besides the scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeepFacts {
    /// The default of the provider's list with the personal access token, when
    /// it was read (needed when either side chooses from it).
    pub provider_default: Option<String>,
    /// Why the provider's list could not be read.
    pub list_error: Option<String>,
}

/// What the planner does about the model of one switch.
#[derive(Clone, Debug, PartialEq)]
pub enum KeepDecision {
    /// Nothing to write or say.
    Nothing,
    /// Write `model` = `model` and show `notice`.
    Keep {
        /// The model Codex runs with now.
        model: String,
        /// Why (info).
        notice: Notice,
    },
    /// Nothing to write; the plan names what changes.
    Note(Notice),
}

/// The decision for a switch in `scope` (pure). `site` names the provider.
pub fn decide(scope: &KeepScope, facts: &KeepFacts, site: &LocalizedText) -> KeepDecision {
    let before = match (&scope.kept, scope.from) {
        (Some(m), _) => Some(m.clone()),
        (None, ModelForm::ProviderList) => facts.provider_default.clone(),
        (None, ModelForm::ApiKey) => None,
    };
    let after = match scope.to {
        ModelForm::ProviderList => facts.provider_default.clone(),
        ModelForm::ApiKey => None,
    };
    match (before, after) {
        (Some(b), Some(a)) if a == b => KeepDecision::Nothing,
        (Some(b), Some(a)) => KeepDecision::Keep {
            notice: Notice::info(
                text("switch.models.keep_pat_default")
                    .arg("model", &b)
                    .arg("default", a)
                    .arg_text("site", site.clone()),
            ),
            model: b,
        },
        (Some(b), None) if scope.to == ModelForm::ApiKey => KeepDecision::Keep {
            notice: Notice::info(text("switch.models.keep_api_key").arg("model", &b)),
            model: b,
        },
        // Back to the personal access token, its list unread.
        (Some(b), None) => KeepDecision::Keep {
            notice: Notice::info(
                text("switch.models.keep_unread")
                    .arg("model", &b)
                    .arg_text("site", site.clone())
                    .arg("reason", facts.list_error.clone().unwrap_or_default()),
            ),
            model: b,
        },
        (None, Some(a)) => KeepDecision::Note(Notice::info(
            text("switch.models.pat_default")
                .arg("model", a)
                .arg_text("site", site.clone()),
        )),
        (None, None) if scope.from == ModelForm::ProviderList => {
            KeepDecision::Note(Notice::warning(
                text("switch.models.keep_unknown")
                    .arg_text("site", site.clone())
                    .arg("reason", facts.list_error.clone().unwrap_or_default()),
            ))
        }
        (None, None) => KeepDecision::Nothing,
    }
}

/// The base model of a kept value: a tier-suffix site carries the speed in the
/// written name (`<slug>-fast`), which the catalog plan adds again.
pub fn kept_base(site: &Site, model: &str) -> String {
    if site.capabilities.tier_model_suffix {
        superba_models::split_tier_suffix(model)
            .map(|(base, _)| base.to_owned())
            .unwrap_or_else(|| model.to_owned())
    } else {
        model.to_owned()
    }
}

// ---------------------------------------------------------------------------
// The record of kept models
// ---------------------------------------------------------------------------

/// The model Superba kept in one home, as written by that generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptRecord {
    /// The `model` value written.
    pub model: String,
    /// The generation that wrote it.
    pub generation: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct KeptState {
    #[serde(default)]
    homes: BTreeMap<String, KeptRecord>,
}

fn kept_path(data_dir: &DataDir) -> PathBuf {
    data_dir.root().join(KEPT_MODEL_FILE)
}

fn load(data_dir: &DataDir) -> KeptState {
    std::fs::read(kept_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn home_key(home: &Path) -> String {
    home.display().to_string()
}

/// The model Superba kept in `home`, while `config.toml` still holds the value
/// that generation wrote (a later apply, a restore or an edit ends it).
pub fn kept_model(data_dir: &DataDir, home: &CodexHome, state: &ConfigState) -> Option<String> {
    let record = load(data_dir).homes.remove(&home_key(home.root()))?;
    let generation = state.active.as_ref()?.generation;
    (state.model_owned
        && generation == record.generation
        && state.model.as_deref() == Some(record.model.as_str()))
    .then_some(record.model)
}

/// Record (or, with `None`, forget) the model kept by the apply of `generation`.
pub fn commit_kept(
    data_dir: &DataDir,
    home: &CodexHome,
    model: Option<&str>,
    generation: u64,
) -> Result<()> {
    let mut state = load(data_dir);
    let key = home_key(home.root());
    let changed = match model {
        Some(m) => {
            let record = KeptRecord {
                model: m.to_owned(),
                generation,
            };
            state.homes.insert(key, record.clone()) != Some(record)
        }
        None => state.homes.remove(&key).is_some(),
    };
    if !changed {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| Error::other(e.to_string()))?;
    crate::models::write_atomic(&kept_path(data_dir), &bytes)
}

/// The diff preview row of the kept model says why it is written.
pub fn mark_kept(changes: &mut [superba_core::SettingChange]) {
    let local = superba_core::TargetId::local();
    for c in changes.iter_mut().filter(|c| {
        c.target == local && c.path == superba_core::codex::config_keys::MODEL && c.to.is_some()
    }) {
        c.reason = superba_core::ChangeReason::KeptModel;
    }
}

// ---------------------------------------------------------------------------
// The planner step
// ---------------------------------------------------------------------------

/// What the planner hands to [`Switcher::plan_model_keep`].
pub(crate) struct KeepInput<'a> {
    pub state: &'a ConfigState,
    pub set: &'a superba_core::ProfileSet,
    pub site: &'a Site,
    /// The destination's auth mode for the local home (API key when routed).
    pub dest_auth: AuthMode,
    /// The destination's direct endpoints and key (`None` when routed).
    pub direct: Option<(&'a url::Url, &'a Secret)>,
    pub resolved: &'a superba_core::ResolvedProfile,
    pub left: Option<&'a str>,
    pub client_version: Option<&'a CodexVersion>,
}

impl Switcher {
    /// [`decide`] for one switch, reading the provider's list when one side
    /// chooses from it: with the applied profile's key at the line `config.toml`
    /// names now, or at the destination's line.
    pub(crate) async fn plan_model_keep(&self, i: KeepInput<'_>) -> KeepDecision {
        let d = &self.deps;
        if !matches!(i.resolved.mode, ResolvedMode::Token { .. }) {
            return KeepDecision::Nothing;
        }
        let home = d.ctx.codex_home();
        let kept = kept_model(&d.ctx.data_dir, &home, i.state);
        let Some(scope) = keep_scope(ScopeInput {
            state: i.state,
            kept: kept.as_deref(),
            dest_site: &i.site.id,
            dest_auth: i.dest_auth,
            dest_sets_model: i.resolved.model.model.is_some(),
            left: i.left,
        }) else {
            return KeepDecision::Nothing;
        };
        let mut facts = KeepFacts::default();
        let needs_list = scope.to == ModelForm::ProviderList
            || (scope.from == ModelForm::ProviderList && scope.kept.is_none());
        if needs_list {
            let source = match (scope.to, i.direct) {
                (ModelForm::ProviderList, Some((base, key))) => Some((base.clone(), key.clone())),
                _ => self.applied_list_source(i.state, i.set).await,
            };
            match source {
                Some((base, key)) => match self
                    .provider_list_default(i.site, &base, &key, i.client_version)
                    .await
                {
                    Ok(m) => facts.provider_default = m,
                    Err(e) => facts.list_error = Some(e),
                },
                None => facts.list_error = Some("no key or line".into()),
            }
        }
        let mut decision = decide(&scope, &facts, &i.site.name);
        if let KeepDecision::Keep { model, .. } = &mut decision {
            *model = kept_base(i.site, model);
        }
        decision
    }

    /// The applied profile's key and the base URL `config.toml` names now.
    async fn applied_list_source(
        &self,
        state: &ConfigState,
        set: &superba_core::ProfileSet,
    ) -> Option<(url::Url, Secret)> {
        let base = url::Url::parse(state.openai_base_url.as_deref()?).ok()?;
        let profile = set.profile(state.active.as_ref()?.profile_id.as_ref()?)?;
        let key = self
            .deps
            .secrets
            .get(profile.key_ref.as_ref()?)
            .await
            .ok()?;
        (!key.is_empty()).then_some((base, key))
    }

    /// [`provider_default`] of the provider's own list (`Err` = why it could not
    /// be read; a fallback list does not count).
    async fn provider_list_default(
        &self,
        site: &Site,
        base_url: &url::Url,
        key: &Secret,
        client_version: Option<&CodexVersion>,
    ) -> std::result::Result<Option<String>, String> {
        let list = self
            .deps
            .prober
            .model_list(ModelListRequest {
                site,
                base_url,
                key,
                client_version,
                refresh: false,
            })
            .await
            .map_err(|e| e.to_string())?;
        if !matches!(list.source, ModelListSource::Upstream { .. }) {
            return Err(list
                .fallback_reason
                .map(|e| e.to_string())
                .unwrap_or_else(|| "the site lists no models".into()));
        }
        Ok(provider_default(&list.models))
    }
}
