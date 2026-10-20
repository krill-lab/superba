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
//! Historically turning the router on with API-key authentication
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
//!
//! WSL distributions that follow this computer (user bug 63) get the same rule
//! for their own Codex ([`Switcher::plan_host_model_keep`]: their `config.toml`,
//! auth, line and Codex version; [`kept_host_model`]), named per distribution in
//! the plan. With the app's "Run Codex in WSL" the app's Codex inside the
//! default distribution reads this computer's `config.toml`, so the local
//! decision is its decision; the plan names that distribution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    AuthMode, CodexHome, CodexVersion, ConfigState, DataDir, Error, LocalizedText,
    ModelListRequest, ModelListSource, ModelSpec, ModelVisibility, Notice, RemoteState,
    ResolvedMode, Result, Secret, Site, TargetId,
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
    /// A router can select a different member, with a different default.
    Router,
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
    /// Current configuration matches Superba's recorded router endpoint.
    pub current_routed: bool,
    /// The model [`kept_model`] reports for it.
    pub kept: Option<&'a str>,
    /// The destination's site.
    pub dest_site: &'a superba_core::SiteId,
    /// The destination's auth mode for the local home.
    pub dest_auth: AuthMode,
    /// The destination uses the local router, whose member may change.
    pub dest_routed: bool,
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
    let active = i.state.active.as_ref()?;
    let from = if i.current_routed {
        ModelForm::Router
    } else {
        ModelForm::of(active.auth_mode?)?
    };
    let to = if i.dest_routed {
        ModelForm::Router
    } else {
        ModelForm::of(i.dest_auth)?
    };
    scope_of_forms(
        ScopeFacts {
            same_site: i.state.handed_off.is_none() && active.site_id.as_ref() == Some(i.dest_site),
            auth: active.auth_mode,
            dest_auth: i.dest_auth,
            model: i.state.model.as_deref(),
            kept: i.kept,
            dest_sets_model: i.dest_sets_model,
            left: i.left,
        },
        from,
        to,
    )
}

/// What [`scope_of`] decides on: how one Codex home (this computer's or a WSL
/// distribution's) chooses its model now and after the switch.
#[derive(Clone, Copy, Debug)]
pub struct ScopeFacts<'a> {
    /// The home is on the destination's site now (never after a hand-off).
    pub same_site: bool,
    /// Its auth mode now (`None` = not known).
    pub auth: Option<AuthMode>,
    /// Its auth mode after the switch.
    pub dest_auth: AuthMode,
    /// The `model` its `config.toml` names now.
    pub model: Option<&'a str>,
    /// The model Superba kept there ([`kept_model`], [`kept_host_model`]).
    pub kept: Option<&'a str>,
    /// The destination sets a model itself (profile, mapped or replacement name).
    pub dest_sets_model: bool,
    /// The `model` value `config.toml` keeps when the destination sets none.
    pub left: Option<&'a str>,
}

/// The rule of [`keep_scope`] on plain facts (pure).
pub fn scope_of(f: ScopeFacts<'_>) -> Option<KeepScope> {
    scope_of_forms(f, ModelForm::of(f.auth?)?, ModelForm::of(f.dest_auth)?)
}

fn scope_of_forms(f: ScopeFacts<'_>, from: ModelForm, to: ModelForm) -> Option<KeepScope> {
    if f.dest_sets_model || f.left.is_some() || !f.same_site {
        return None;
    }
    let kept = f.kept.map(str::to_owned);
    // A model in config.toml that Superba did not keep: the user's own, or the
    // profile's, since removed from the profile (the user chose Codex's default).
    if f.model.is_some() && kept.is_none() {
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
        (None, ModelForm::ApiKey | ModelForm::Router) => None,
    };
    let after = match scope.to {
        ModelForm::ProviderList => facts.provider_default.clone(),
        ModelForm::ApiKey | ModelForm::Router => None,
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
        (Some(b), None) if scope.to == ModelForm::Router => KeepDecision::Keep {
            notice: Notice::info(text("switch.models.keep_router").arg("model", &b)),
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

/// The model Superba kept in a WSL distribution's Codex home (user bug 63).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptHostRecord {
    /// The `model` value written.
    pub model: String,
    /// The distribution's `CODEX_HOME` it was written to.
    pub codex_home: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct KeptState {
    #[serde(default)]
    homes: BTreeMap<String, KeptRecord>,
    /// By target id (`remote-wsl:<distro>`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    hosts: BTreeMap<String, KeptHostRecord>,
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
    save_if(data_dir, &state, changed)
}

fn save_if(data_dir: &DataDir, state: &KeptState, changed: bool) -> Result<()> {
    if !changed {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| Error::other(e.to_string()))?;
    crate::models::write_atomic(&kept_path(data_dir), &bytes)
}

/// The model Superba kept in a WSL distribution (user bug 63), while its
/// `config.toml` (`model`, as inspected) still holds that value as a key
/// Superba wrote, in the same Codex home. A later switch records again (or
/// forgets); a restore, a rollback or an edit ends it.
pub fn kept_host_model(
    data_dir: &DataDir,
    target: &TargetId,
    state: &RemoteState,
    model: Option<&str>,
) -> Option<String> {
    let record = load(data_dir).hosts.remove(target.as_str())?;
    let owned = state
        .owned_paths
        .iter()
        .any(|p| p == superba_core::codex::config_keys::MODEL);
    (owned && record.codex_home == state.codex_home && model == Some(record.model.as_str()))
        .then_some(record.model)
}

/// Record (or, with `None`, forget) the model a switch kept in a distribution.
pub fn commit_kept_host(
    data_dir: &DataDir,
    target: &TargetId,
    codex_home: &str,
    model: Option<&str>,
) -> Result<()> {
    let mut state = load(data_dir);
    let key = target.as_str().to_owned();
    let changed = match model {
        Some(m) => {
            let record = KeptHostRecord {
                model: m.to_owned(),
                codex_home: codex_home.to_owned(),
            };
            state.hosts.insert(key, record.clone()) != Some(record)
        }
        None => state.hosts.remove(&key).is_some(),
    };
    save_if(data_dir, &state, changed)
}

/// The diff preview row of the kept model says why it is written.
pub fn mark_kept(changes: &mut [superba_core::SettingChange]) {
    mark_kept_on(changes, &TargetId::local());
}

/// [`mark_kept`] for the rows of one target (a WSL distribution, user bug 63).
pub fn mark_kept_on(changes: &mut [superba_core::SettingChange], target: &TargetId) {
    for c in changes.iter_mut().filter(|c| {
        &c.target == target && c.path == superba_core::codex::config_keys::MODEL && c.to.is_some()
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
    /// The destination's auth mode for the local home.
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
            current_routed: superba_core::router::codex_uses_router(
                i.state,
                i.set.settings.router.enabled,
                d.router.endpoint().ok().as_ref().map(|e| &e.base_url),
            ),
            kept: kept.as_deref(),
            dest_site: &i.site.id,
            dest_auth: i.dest_auth,
            dest_routed: i.direct.is_none(),
            dest_sets_model: i.resolved.model.model.is_some(),
            left: i.left,
        }) else {
            return KeepDecision::Nothing;
        };
        let mut facts = KeepFacts::default();
        if needs_list(&scope) {
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
        Some((base, self.applied_key(state, set).await?))
    }

    /// The key of the profile applied to this computer.
    async fn applied_key(
        &self,
        state: &ConfigState,
        set: &superba_core::ProfileSet,
    ) -> Option<Secret> {
        let profile = set.profile(state.active.as_ref()?.profile_id.as_ref()?)?;
        let key = self
            .deps
            .secrets
            .get(profile.key_ref.as_ref()?)
            .await
            .ok()?;
        (!key.is_empty()).then_some(key)
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
                source: None,
                standard_base: None,
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

/// The provider's list is needed: one side chooses from it (the side before only
/// when no kept model says what runs now).
fn needs_list(scope: &KeepScope) -> bool {
    scope.to == ModelForm::ProviderList
        || (scope.from == ModelForm::ProviderList && scope.kept.is_none())
}

// ---------------------------------------------------------------------------
// WSL distributions following this computer (user bug 63)
// ---------------------------------------------------------------------------
//
// A distribution's Codex chooses its model by the same rules (its own
// `config.toml`, auth and Codex version), and router mode puts it in API-key
// mode too. Each distribution that follows a switch gets its own decision:
// what its Codex runs now (the model kept there, else the provider's default
// for the personal access token it holds) against what the destination would
// give it. The local home's kept model is never copied to a distribution.

/// Whether a distribution's Codex is on `site` now: its `openai_base_url` is at
/// a line of that site, or at the local router (loopback or the WSL listener)
/// while this computer's applied site is `site`.
pub fn host_on_site(
    base_url: Option<&str>,
    site: &Site,
    applied_site: Option<&superba_core::SiteId>,
    router_endpoints: &[url::Url],
) -> bool {
    let Some(url) = base_url.and_then(|u| url::Url::parse(u).ok()) else {
        return false;
    };
    if crate::rules::line_for_origin(site, &url.origin().ascii_serialization()).is_some() {
        return true;
    }
    applied_site == Some(&site.id)
        && router_endpoints
            .iter()
            .any(|endpoint| superba_core::router::matches_router_endpoint(url.as_str(), endpoint))
}

/// The line and key a distribution's personal access token reads the
/// provider's list with: the line its `config.toml` names, the one of `keys`
/// its `auth.json` holds (compared by the last four characters).
fn host_list_source(
    state: &RemoteState,
    keys: &[Secret],
) -> std::result::Result<(url::Url, Secret), String> {
    let base = state
        .openai_base_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .ok_or_else(|| "no line in the distribution's config.toml".to_owned())?;
    let superba_core::AuthShape::Pat { key_last4 } = &state.auth else {
        return Err("no personal access token in the distribution".into());
    };
    keys.iter()
        .find(|k| !k.is_empty() && &k.last4() == key_last4)
        .map(|k| (base, k.clone()))
        .ok_or_else(|| "the distribution holds a key of no profile".into())
}

/// What the planner hands to [`Switcher::plan_host_model_keep`].
pub(crate) struct HostKeepInput<'a> {
    pub host: &'a superba_core::RemoteHost,
    /// The distribution as probed now.
    pub state: &'a RemoteState,
    /// The destination's site.
    pub site: &'a Site,
    /// This computer's home now (applied site and profile).
    pub local: &'a ConfigState,
    pub set: &'a superba_core::ProfileSet,
    /// What the distribution gets (its `model` as the destination sets it).
    pub resolved: &'a superba_core::ResolvedProfile,
    /// The destination's key for a direct connection.
    pub direct_key: Option<&'a Secret>,
}

impl Switcher {
    /// [`decide`] for one WSL distribution following a switch, reading the
    /// provider's list (with the distribution's Codex version) when one side
    /// chooses from it.
    pub(crate) async fn plan_host_model_keep(&self, i: HostKeepInput<'_>) -> KeepDecision {
        let ResolvedMode::Token {
            auth,
            endpoints,
            key,
            ..
        } = &i.resolved.mode
        else {
            return KeepDecision::Nothing;
        };
        let files = superba_core::CodexFiles {
            config_toml: i.state.config_toml.clone(),
            auth_json: None,
        };
        let model = match self
            .deps
            .config
            .inspect_files(Path::new(&i.state.codex_home), &files)
        {
            Ok(cs) if cs.parse_error.is_none() => cs.model,
            _ => return KeepDecision::Nothing,
        };
        let kept = kept_host_model(
            &self.deps.ctx.data_dir,
            &i.host.target_id,
            i.state,
            model.as_deref(),
        );
        let applied_site = i.local.active.as_ref().and_then(|a| a.site_id.as_ref());
        let router_endpoints = if i.set.settings.router.enabled {
            self.deps
                .router
                .endpoint()
                .ok()
                .map(|e| {
                    let mut endpoints = vec![e.base_url.clone()];
                    for address in self.deps.router.wsl_listen() {
                        let mut endpoint = e.base_url.clone();
                        if endpoint.set_host(Some(&address.to_string())).is_ok() {
                            endpoints.push(endpoint);
                        }
                    }
                    endpoints
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let from = if i.state.openai_base_url.as_deref().is_some_and(|base| {
            router_endpoints.iter().any(|endpoint| {
                endpoint.as_str().trim_end_matches('/') == base.trim_end_matches('/')
            })
        }) {
            ModelForm::Router
        } else if let Some(form) = i.state.auth.mode().and_then(ModelForm::of) {
            form
        } else {
            return KeepDecision::Nothing;
        };
        let to = if endpoints.ca_certificate.is_some() {
            ModelForm::Router
        } else if let Some(form) = ModelForm::of(*auth) {
            form
        } else {
            return KeepDecision::Nothing;
        };
        let Some(scope) = scope_of_forms(
            ScopeFacts {
                same_site: host_on_site(
                    i.state.openai_base_url.as_deref(),
                    i.site,
                    applied_site,
                    &router_endpoints,
                ),
                auth: i.state.auth.mode(),
                dest_auth: *auth,
                model: model.as_deref(),
                kept: kept.as_deref(),
                dest_sets_model: i.resolved.model.model.is_some(),
                left: None,
            },
            from,
            to,
        ) else {
            return KeepDecision::Nothing;
        };
        let mut facts = KeepFacts::default();
        if needs_list(&scope) {
            let source = if scope.to == ModelForm::ProviderList {
                Ok((endpoints.openai_base_url.clone(), key.clone()))
            } else {
                // The token the distribution holds: the destination's key or
                // the one applied here (a distribution left out while stopped).
                let mut keys: Vec<Secret> = i.direct_key.cloned().into_iter().collect();
                keys.extend(self.applied_key(i.local, i.set).await);
                host_list_source(i.state, &keys)
            };
            let read = match source {
                Ok((base, key)) => {
                    self.provider_list_default(i.site, &base, &key, i.state.codex_version.as_ref())
                        .await
                }
                Err(e) => Err(e),
            };
            match read {
                Ok(m) => facts.provider_default = m,
                Err(e) => facts.list_error = Some(e),
            }
        }
        let mut decision = decide(&scope, &facts, &i.site.name);
        if let KeepDecision::Keep { model, .. } = &mut decision {
            *model = kept_base(i.site, model);
        }
        decision
    }
}

/// A distribution's notice: `note` prefixed with the distribution's name.
pub fn on_host(host: &superba_core::RemoteHost, note: Notice) -> Notice {
    let Notice { severity, text: t } = note;
    Notice {
        severity,
        text: text("switch.warn.on_host")
            .arg_text("host", host.title())
            .arg_text("text", t),
    }
}
