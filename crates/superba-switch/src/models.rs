//! Models page, model catalog files and speed tiers (v0.3 feedback 4, 5, 11;
//! plan section 11).
//!
//! - **Model list** of a profile: the provider's own list (`LineProber::
//!   model_list`), else Superba's feed, else the consumer Codex's bundled list.
//! - **Catalog file** (`model_catalog_json`): written only when the profile needs
//!   one (default rule, decision 4): a per-model override, the provider picker
//!   (custom sites), Ultrafast, or Fast/Ultrafast through tier-suffix model names.
//!   Never for the official login, never in PAT mode (the app's own speed menu
//!   and live list work there; only the all-models keys are written), never for
//!   remote hosts in 0.3.
//! - The file is generated for the **app's** Codex (pinned or bundled) from that
//!   binary's own bundled list, preflighted with it (and with the CLI first on the
//!   PATH when that is another version), written content-addressed to
//!   `<CODEX_HOME>/superba/models-<sha8>.json` before `config.toml` names it,
//!   never rewritten; unreferenced files are pruned after a successful switch
//!   (the referenced and the previous file stay).
//! - The all-models context window / compaction keys are written only without a
//!   catalog file; with one they are folded into every entry without its own
//!   value, so global and per-model values never meet.
//! - **Speed**: one tier per profile; key-based profiles always write
//!   `service_tier` (`default` for Standard). With tier-suffix support the model
//!   becomes `<slug>-<tier>` and `service_tier = "default"`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    AppliedModel, AppliedModels, AuthChoice, AuthMode, BuiltinProfile, CatalogFileStatus,
    CodexVersion, ConfigState, Error, LocalizedText, MIN_CONTEXT_WINDOW, ModelDefaults, ModelList,
    ModelListRequest, ModelListSource, ModelOverride, ModelPicker, ModelSpec, ModelVisibility,
    ModelsEdit, ModelsEditReport, ModelsView, ModelsViewRequest, Notice, Profile, ProfileId,
    ProfileSet, ReasoningLevel, Result, ServiceTierSpec, Site, SiteKind, SpeedEffect, SpeedSupport,
    SpeedTier, TierEffect, Timestamp, effective_auto_compact,
};
use superba_models::{BundledCatalog, CatalogFile, CatalogInput, CatalogSource, OverrideCheck};

use crate::Switcher;
use crate::messages::text;
use crate::planner::all_sites;

/// The data-dir file recording the catalog file of each Codex home.
pub const CATALOG_STATE_FILE: &str = "catalog-files.json";

// ---------------------------------------------------------------------------
// The generator seam
// ---------------------------------------------------------------------------

/// Model catalog generation as the switch service uses it (production:
/// `superba-models`; tests: a fake).
pub trait CatalogBuilder: Send + Sync + std::fmt::Debug {
    /// Parse `codex debug models --bundled` output.
    fn parse_bundled(&self, output: &[u8]) -> Result<BundledCatalog>;
    /// Build the catalog file for one consumer.
    fn build(&self, input: &CatalogInput<'_>) -> Result<CatalogFile>;
    /// Check an override against a model.
    fn validate_override(
        &self,
        spec: &ModelSpec,
        model_override: &ModelOverride,
    ) -> Result<OverrideCheck>;
    /// Union of router members' lists.
    fn union(&self, lists: &[(String, ModelList)]) -> Result<ModelList>;
}

/// [`CatalogBuilder`] over `superba-models`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelsCrate;

impl CatalogBuilder for ModelsCrate {
    fn parse_bundled(&self, output: &[u8]) -> Result<BundledCatalog> {
        superba_models::parse_bundled(output)
    }

    fn build(&self, input: &CatalogInput<'_>) -> Result<CatalogFile> {
        superba_models::build_catalog(input)
    }

    fn validate_override(
        &self,
        spec: &ModelSpec,
        model_override: &ModelOverride,
    ) -> Result<OverrideCheck> {
        superba_models::validate_override(spec, model_override)
    }

    fn union(&self, lists: &[(String, ModelList)]) -> Result<ModelList> {
        superba_models::union(lists)
    }
}

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

/// Why a profile needs a catalog file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogReason {
    /// A per-model override (context window, compaction, name, hidden).
    Overrides,
    /// The app's picker shows the provider's models (custom sites).
    ProviderPicker,
    /// Ultrafast needs the tier in the catalog entries.
    Ultrafast,
    /// Fast / Ultrafast through tier-suffix model names.
    TierSuffix,
}

/// An override that needs a catalog entry (an upstream name alone edits the
/// site's model map instead).
pub fn override_needs_catalog(o: &ModelOverride) -> bool {
    o.context_window.is_some() || o.auto_compact.is_some() || o.display_name.is_some() || o.hidden
}

/// The speed tier a profile runs with (Standard unless chosen).
pub fn profile_speed(profile: &Profile) -> SpeedTier {
    profile.model.speed.unwrap_or_default()
}

/// Whether the tier-suffix model names apply: the site maps them, the tier is
/// not Standard and the desktop app runs in API-key mode (with PAT the app's own
/// speed menu works).
pub fn suffix_applies(site: &Site, auth: AuthMode, speed: SpeedTier) -> bool {
    site.capabilities.tier_model_suffix
        && speed != SpeedTier::Standard
        && !auth.needs_authapi_env()
        && auth.uses_key()
}

/// Reasons a key-based profile needs a catalog file in `auth` mode (empty =
/// none). Official profiles and PAT never get one (decision 4).
pub fn catalog_reasons(profile: &Profile, site: &Site, auth: AuthMode) -> Vec<CatalogReason> {
    if profile.is_builtin()
        || site.kind == SiteKind::Official
        || !auth.uses_key()
        || auth.needs_authapi_env()
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    if profile.model_overrides.values().any(override_needs_catalog) {
        out.push(CatalogReason::Overrides);
    }
    if site.kind == SiteKind::Compatible && profile.model_picker == ModelPicker::Provider {
        out.push(CatalogReason::ProviderPicker);
    }
    let speed = profile_speed(profile);
    if speed == SpeedTier::Ultrafast {
        out.push(CatalogReason::Ultrafast);
    }
    if suffix_applies(site, auth, speed) {
        out.push(CatalogReason::TierSuffix);
    }
    out
}

/// Overrides with the all-models window and compaction folded into every
/// listed model (and the configured one) that has no value of its own. The
/// all-models window is capped at each model's maximum (`max(slug)`), as Codex
/// caps `model_context_window` without a catalog file, so a catalog never
/// raises a model's window above what it was without one
/// ([`superba_core::model_limits`] shows the same values).
pub fn fold_globals(
    overrides: &BTreeMap<String, ModelOverride>,
    defaults: &ModelDefaults,
    slugs: impl IntoIterator<Item = String>,
    max: impl Fn(&str) -> Option<u64>,
) -> BTreeMap<String, ModelOverride> {
    let mut out = overrides.clone();
    if defaults.context_window.is_none() && defaults.auto_compact.is_none() {
        return out;
    }
    for slug in slugs {
        let cap = max(&slug);
        let o = out.entry(slug).or_default();
        if o.context_window.is_none() {
            o.context_window = defaults.context_window.map(|w| cap.map_or(w, |m| w.min(m)));
        }
        if o.auto_compact.is_none() {
            o.auto_compact = defaults.auto_compact;
        }
    }
    out.retain(|_, o| !o.is_empty());
    out
}

/// Tiers a model offers, Standard only when the list carries no tier data.
fn offers(list: &ModelList, model: Option<&str>, tier: SpeedTier) -> bool {
    if tier == SpeedTier::Standard {
        return true;
    }
    match model.and_then(|m| list.model(m)) {
        Some(spec) => spec.speed_tiers().contains(&tier),
        None => list.offered_tiers().contains(&tier),
    }
}

/// Where the profile's speed tier applies (pure): `pat` = the app uses a
/// personal access token, `official` = the official login.
pub fn speed_support(
    speed: SpeedTier,
    list: &ModelList,
    model: Option<&str>,
    official: bool,
    pat: bool,
    tier_suffix: bool,
) -> SpeedSupport {
    speed_support_with(speed, list, None, model, official, pat, tier_suffix)
}

/// [`speed_support`] when Codex filters the tier with another list than the
/// provider's: `cli_list` is the consumer's bundled list, which an API-key
/// Codex without a catalog file uses (it never fetches the provider's list, so
/// a compatible site whose OpenAI list carries no tier data still gets Fast
/// for a model whose bundled entry offers it). Fast is offered when either list
/// offers it; Ultrafast always needs a catalog file built from the provider's
/// list, so only that list counts for it.
pub fn speed_support_with(
    speed: SpeedTier,
    list: &ModelList,
    cli_list: Option<&ModelList>,
    model: Option<&str>,
    official: bool,
    pat: bool,
    tier_suffix: bool,
) -> SpeedSupport {
    let mut offered = list.offered_tiers();
    if let Some(b) = cli_list
        && b.offered_tiers().contains(&SpeedTier::Fast)
        && !offered.contains(&SpeedTier::Fast)
    {
        offered.push(SpeedTier::Fast);
        offered.sort();
    }
    if official {
        return SpeedSupport {
            offered,
            cli: SpeedEffect::AppMenu,
            app: SpeedEffect::AppMenu,
            tiers: Vec::new(),
        };
    }
    let on_list = offers(list, model, speed);
    let offered_here = match cli_list {
        Some(b) if speed == SpeedTier::Fast => offers(b, model, speed),
        _ => on_list,
    };
    let cli = if offered_here {
        SpeedEffect::Applies
    } else {
        SpeedEffect::NotOffered
    };
    let app = if pat {
        SpeedEffect::AppMenu
    } else if speed == SpeedTier::Standard {
        SpeedEffect::Applies
    } else if !offered_here {
        SpeedEffect::NotOffered
    } else if tier_suffix && on_list {
        match (model, speed.model_suffix()) {
            (Some(m), Some(suffix)) => SpeedEffect::ViaModelName {
                model: format!("{m}{suffix}"),
            },
            _ => SpeedEffect::AppForcesStandard,
        }
    } else {
        SpeedEffect::AppForcesStandard
    };
    SpeedSupport {
        offered,
        cli,
        app,
        tiers: Vec::new(),
    }
}

/// [`speed_support_with`]'s two effects for every tier, so the Models page shows
/// what a tier picked but not saved yet will do. `cli_list(tier)` is the list
/// Codex filters that tier with when it is not the provider's (the bundled list
/// of an API-key Codex without a catalog file; whether a catalog file is written
/// depends on the tier).
pub fn tier_effects<'a>(
    list: &ModelList,
    cli_list: impl Fn(SpeedTier) -> Option<&'a ModelList>,
    model: Option<&str>,
    official: bool,
    pat: bool,
    tier_suffix: bool,
) -> Vec<TierEffect> {
    SpeedTier::ALL
        .into_iter()
        .map(|tier| {
            let s = speed_support_with(
                tier,
                list,
                cli_list(tier),
                model,
                official,
                pat,
                tier_suffix,
            );
            TierEffect {
                tier,
                cli: s.cli,
                app: s.app,
            }
        })
        .collect()
}

/// A [`ModelSpec`] from a Codex-native catalog entry (the entry is kept raw).
pub fn spec_from_entry(entry: &serde_json::Value) -> Option<ModelSpec> {
    let o = entry.as_object()?;
    let s = |k: &str| o.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let n = |k: &str| o.get(k).and_then(serde_json::Value::as_u64);
    let slug = s("slug")?;
    Some(ModelSpec {
        display_name: s("display_name"),
        description: s("description"),
        visibility: o
            .get("visibility")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(ModelVisibility::List),
        supported_in_api: o
            .get("supported_in_api")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        priority: o.get("priority").and_then(serde_json::Value::as_i64),
        context_window: n("context_window"),
        max_context_window: n("max_context_window"),
        effective_context_window_percent: n("effective_context_window_percent")
            .and_then(|v| u8::try_from(v).ok()),
        auto_compact_token_limit: n("auto_compact_token_limit"),
        reasoning_levels: o
            .get("supported_reasoning_levels")
            .and_then(|v| serde_json::from_value::<Vec<ReasoningLevel>>(v.clone()).ok())
            .unwrap_or_default(),
        default_reasoning_level: s("default_reasoning_level"),
        service_tiers: o
            .get("service_tiers")
            .and_then(|v| serde_json::from_value::<Vec<ServiceTierSpec>>(v.clone()).ok())
            .unwrap_or_default(),
        default_service_tier: s("default_service_tier"),
        input_modalities: o
            .get("input_modalities")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            .unwrap_or_default(),
        raw: Some(entry.clone()),
        lists: Vec::new(),
        slug,
    })
}

/// Basic override checks that need no generator: window >= 16K and <= the
/// model's maximum, the 90 % compaction clamp reported.
pub fn check_override(spec: Option<&ModelSpec>, o: &ModelOverride) -> Result<OverrideCheck> {
    let mut check = OverrideCheck::default();
    if let Some(w) = o.context_window {
        if w < MIN_CONTEXT_WINDOW {
            return Err(Error::invalid(format!(
                "context window {w} is below the minimum {MIN_CONTEXT_WINDOW}"
            )));
        }
        if let Some(max) = spec.and_then(|s| s.max_context_window.or(s.context_window))
            && w > max
        {
            return Err(Error::invalid(format!(
                "context window {w} is above the model's maximum {max}"
            )));
        }
    }
    let window = o
        .context_window
        .or_else(|| spec.and_then(|s| s.context_window));
    check.effective_window = window;
    if let (Some(w), Some(limit)) = (window, o.auto_compact) {
        let effective = effective_auto_compact(w, Some(limit));
        check.effective_auto_compact = Some(effective);
        if effective < limit {
            check.clamped = true;
            check.notes.push(Notice::info(
                text("switch.models.compact_clamped")
                    .arg("limit", limit)
                    .arg("effective", effective),
            ));
        }
    } else if let Some(w) = window {
        check.effective_auto_compact = Some(effective_auto_compact(w, None));
    }
    Ok(check)
}

// ---------------------------------------------------------------------------
// Catalog files on disk
// ---------------------------------------------------------------------------

/// A catalog file to write before `config.toml` names it.
#[derive(Clone, Debug)]
pub struct CatalogWrite {
    /// `<CODEX_HOME>/superba/models-<sha8>.json`.
    pub path: PathBuf,
    /// Content.
    pub bytes: Vec<u8>,
    /// Content address.
    pub sha8: String,
}

/// What Superba remembers about a home's catalog file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRecord {
    /// `models-<sha8>.json`.
    pub file: String,
    /// Content address.
    pub sha8: String,
    /// The app's Codex the file was generated for.
    pub consumer: CodexVersion,
    /// The CLI version it was also preflighted with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CodexVersion>,
    /// The profile it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<ProfileId>,
    /// IDE extensions' Codex binaries (running app-servers) that accepted it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ide: Vec<PathBuf>,
    /// When generated (the model list is frozen at this time).
    pub generated_at: Timestamp,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct HomeCatalog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<CatalogRecord>,
    /// The file the configuration before the current one named (kept, so a
    /// restore or a rollback finds it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<CatalogRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CatalogState {
    #[serde(default)]
    homes: BTreeMap<String, HomeCatalog>,
}

fn state_path(data_dir: &superba_core::DataDir) -> PathBuf {
    data_dir.root().join(CATALOG_STATE_FILE)
}

fn load_state(data_dir: &superba_core::DataDir) -> CatalogState {
    std::fs::read(state_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_state(data_dir: &superba_core::DataDir, state: &CatalogState) -> Result<()> {
    let path = state_path(data_dir);
    write_atomic(
        &path,
        &serde_json::to_vec_pretty(state).map_err(|e| Error::other(e.to_string()))?,
    )
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir.display().to_string(), &e))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(path.display().to_string(), &e));
    }
    Ok(())
}

/// The catalog record of `home`, if any.
pub(crate) fn current_record(
    data_dir: &superba_core::DataDir,
    home: &Path,
) -> Option<CatalogRecord> {
    load_state(data_dir)
        .homes
        .get(&home.display().to_string())
        .and_then(|h| h.current.clone())
}

/// Write a catalog file unless it is already there with the same bytes (content
/// addressed: never rewritten). Returns whether it was created.
pub(crate) fn write_catalog_file(w: &CatalogWrite) -> Result<bool> {
    if let Ok(existing) = std::fs::read(&w.path)
        && existing == w.bytes
    {
        return Ok(false);
    }
    write_atomic(&w.path, &w.bytes)?;
    Ok(true)
}

/// Record a successful switch's catalog for `home` and prune unreferenced files
/// (the current and the previous file stay). `record = None` = no catalog was
/// planned: a restore or the official login. The file the written `config.toml`
/// names is never pruned, and when it is the current or the previous file (a
/// restored generation), its record stays current.
pub(crate) fn commit_record(
    data_dir: &superba_core::DataDir,
    home: &superba_core::CodexHome,
    record: Option<CatalogRecord>,
) -> Result<()> {
    let named = named_catalog_file(home);
    let mut state = load_state(data_dir);
    let key = home.root().display().to_string();
    let entry = state.homes.entry(key.clone()).or_default();
    let old_current = entry.current.take();
    let old_previous = entry.previous.take();
    let record = record.or_else(|| {
        let named = named.as_ref()?;
        [old_current.as_ref(), old_previous.as_ref()]
            .into_iter()
            .flatten()
            .find(|r| &r.file == named)
            .cloned()
    });
    // `previous` = the file the configuration before this switch referenced:
    // none when that configuration had no catalog (a released file is pruned
    // by the next switch that writes one).
    entry.previous = match old_current {
        Some(old) if record.as_ref().is_none_or(|r| r.file != old.file) => Some(old),
        Some(_) => old_previous.filter(|p| record.as_ref().is_none_or(|r| r.file != p.file)),
        None => None,
    };
    entry.current = record;
    let mut keep: Vec<String> = entry
        .current
        .iter()
        .chain(entry.previous.iter())
        .map(|r| r.file.clone())
        .collect();
    keep.extend(named);
    if entry.current.is_none() && entry.previous.is_none() {
        state.homes.remove(&key);
    }
    save_state(data_dir, &state)?;
    prune(home, &keep);
    Ok(())
}

/// File name of the catalog file in `<home>/superba/` that `config.toml` names
/// now (`model_catalog_json`, relative paths resolved against the home).
pub(crate) fn named_catalog_file(home: &superba_core::CodexHome) -> Option<String> {
    let path = named_catalog_path(home)?;
    if path.parent()? != home.superba_dir().as_path() {
        return None;
    }
    Some(path.file_name()?.to_string_lossy().into_owned())
}

/// The file `config.toml`'s `model_catalog_json` names (relative paths resolved
/// against the home), whether or not it exists.
pub(crate) fn named_catalog_path(home: &superba_core::CodexHome) -> Option<PathBuf> {
    let bytes = std::fs::read(home.config_toml()).ok()?;
    let doc = superba_config::toml_keys::ConfigDoc::parse(&bytes).ok()?;
    let value = doc.get_str(superba_core::codex::config_keys::MODEL_CATALOG_JSON)?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(home.root().join(value))
}

/// Remove `models-*.json` files under `<home>/superba/` except `keep`.
pub(crate) fn prune(home: &superba_core::CodexHome, keep: &[String]) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(home.superba_dir()) else {
        return removed;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("models-") && name.ends_with(".json") && !keep.contains(&name) {
            let path = e.path();
            match std::fs::remove_file(&path) {
                Ok(()) => removed.push(path),
                Err(err) => {
                    tracing::debug!(path = %path.display(), error = %err, "cannot prune a catalog file")
                }
            }
        }
    }
    removed
}

/// Forget every catalog of `home` and remove `<home>/superba/` (uninstall).
pub(crate) fn remove_all(data_dir: &superba_core::DataDir, home: &superba_core::CodexHome) {
    let mut state = load_state(data_dir);
    if state
        .homes
        .remove(&home.root().display().to_string())
        .is_some()
        && let Err(e) = save_state(data_dir, &state)
    {
        tracing::warn!(error = %e, "cannot update the catalog record");
    }
    let dir = home.superba_dir();
    if let Err(e) = std::fs::remove_dir_all(&dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(error = %e, "cannot remove {}", dir.display());
    }
}

// ---------------------------------------------------------------------------
// Local Codex consumers of a catalog
// ---------------------------------------------------------------------------

/// A local Codex binary that reads the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalCodex {
    pub version: Option<CodexVersion>,
    pub binary: Option<PathBuf>,
}

/// The app's Codex (pinned or bundled; `installs` are pin-adjusted) and the CLI
/// first on the PATH.
pub(crate) fn local_codexes(
    install_id: Option<&str>,
    installs: &[superba_core::AppInstall],
    binaries: &[superba_core::CodexBinary],
) -> (Option<LocalCodex>, Option<LocalCodex>) {
    let app = install_id
        .and_then(|id| installs.iter().find(|i| i.id == id))
        .or_else(|| installs.first())
        .map(|i| LocalCodex {
            version: i.bundled_codex_version.clone(),
            binary: i.bundled_codex.clone(),
        });
    let cli = binaries
        .iter()
        .find(|b| b.path_rank == Some(0))
        .map(|b| LocalCodex {
            version: b.version.clone(),
            binary: Some(b.path.clone()),
        });
    (app, cli)
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// What Codex reads now in `home` for the models (Models page "applied"
/// state): the all-models keys of `config.toml` and the entries of the catalog
/// file it names (a relative path resolves against the Codex home, as Codex
/// does). Missing or unreadable files give empty values.
pub async fn applied_models(home: &superba_core::CodexHome) -> AppliedModels {
    let mut out = AppliedModels::default();
    let Ok(bytes) = tokio::fs::read(home.config_toml()).await else {
        return out;
    };
    let Ok(doc) = superba_config::toml_keys::ConfigDoc::parse(&bytes) else {
        return out;
    };
    let count = |key: &str| {
        doc.get_repr(key)
            .and_then(|r| r.trim().replace('_', "").parse::<u64>().ok())
    };
    out.context_window = count(superba_core::codex::config_keys::MODEL_CONTEXT_WINDOW);
    out.auto_compact = count(superba_core::codex::config_keys::MODEL_AUTO_COMPACT_TOKEN_LIMIT);
    let Some(path) = doc
        .get_str(superba_core::codex::config_keys::MODEL_CATALOG_JSON)
        .filter(|p| !p.trim().is_empty())
    else {
        return out;
    };
    let path = home.root().join(path);
    if let Ok(bytes) = tokio::fs::read(&path).await
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        out.models = json
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(|m| m.iter().filter_map(AppliedModel::from_entry).collect())
            .unwrap_or_default();
    }
    out.catalog = Some(path);
    out
}

/// Inputs the planner hands to [`Switcher::plan_catalog`].
pub(crate) struct CatalogPlanInput<'a> {
    pub profile: &'a Profile,
    pub site: &'a Site,
    pub auth: AuthMode,
    pub routed: bool,
    pub endpoints: Option<&'a superba_core::ResolvedEndpoints>,
    pub key: Option<&'a superba_core::Secret>,
    pub install_id: Option<&'a str>,
    pub installs: &'a [superba_core::AppInstall],
    pub binaries: &'a [superba_core::CodexBinary],
    pub configured_model: Option<String>,
}

/// What [`Switcher::plan_catalog`] decided.
#[derive(Debug, Default)]
pub(crate) struct CatalogPlan {
    pub write: Option<CatalogWrite>,
    pub record: Option<CatalogRecord>,
    /// The model to write instead (tier-suffix name).
    pub model: Option<String>,
    /// The global window / compaction keys are folded into the file.
    pub folded_globals: bool,
    /// The speed is carried by the model name (`service_tier = "default"`).
    pub speed_via_model: bool,
    pub notes: Vec<Notice>,
    /// Why no file could be made (the switch is blocked).
    pub failed: Option<LocalizedText>,
}

impl Switcher {
    /// Model list of a profile for display and generation: the provider's list,
    /// else the feed, else the app Codex's bundled list.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn model_list_for(
        &self,
        site: &Site,
        base_url: Option<&url::Url>,
        key: Option<&superba_core::Secret>,
        client_version: Option<&CodexVersion>,
        app_binary: Option<&Path>,
        refresh: bool,
        source: Option<superba_core::ModelSource>,
    ) -> ModelList {
        let d = &self.deps;
        let mut reason: Option<Error> = None;
        if let (Some(base_url), Some(key)) = (base_url, key) {
            match d
                .prober
                .model_list(ModelListRequest {
                    site,
                    base_url,
                    key,
                    client_version,
                    refresh,
                    source,
                    standard_base: None,
                })
                .await
            {
                Ok(list) if !list.models.is_empty() => return list,
                Ok(list) => reason = list.sources.into_iter().find_map(|s| s.error),
                Err(e) => reason = Some(e),
            }
        }
        let set = d.profiles.load().await.unwrap_or_default();
        let feeds = (d.feeds)(&set.settings.network);
        if let Ok(snap) = feeds.snapshot(false).await
            && !snap.models.is_empty()
        {
            return ModelList {
                source: ModelListSource::Feed {
                    codex_version: snap
                        .codex_versions
                        .first()
                        .map(|r| r.version.clone())
                        .or_else(|| client_version.cloned())
                        .unwrap_or(CodexVersion::new(0, 0, 0)),
                    mirror: match &snap.source {
                        superba_core::FeedOrigin::Mirror { name } => name.clone(),
                        superba_core::FeedOrigin::Cache => "cache".into(),
                        _ => "builtin".into(),
                    },
                    serial: snap.serial,
                    fetched_at: snap.fetched_at,
                },
                models: snap.models,
                fallback_reason: reason,
                sources: Vec::new(),
            };
        }
        if let Some(mut list) = self.bundled_list(app_binary).await {
            if let ModelListSource::Bundled { codex_version } = &mut list.source
                && *codex_version == CodexVersion::new(0, 0, 0)
                && let Some(v) = client_version
            {
                *codex_version = v.clone();
            }
            list.fallback_reason = reason;
            return list;
        }
        ModelList {
            source: ModelListSource::Snapshot {
                codex_version: client_version
                    .cloned()
                    .unwrap_or(CodexVersion::new(0, 0, 0)),
            },
            models: Vec::new(),
            fallback_reason: reason,
            sources: Vec::new(),
        }
    }

    /// The app Codex's own list (`codex debug models --bundled`).
    async fn bundled_list(&self, app_binary: Option<&Path>) -> Option<ModelList> {
        let d = &self.deps;
        let out = d.app.codex_debug_models(app_binary?, None).await.ok()?;
        let bundled = d.models.parse_bundled(out.as_bytes()).ok()?;
        Some(ModelList {
            source: ModelListSource::Bundled {
                codex_version: bundled
                    .codex_version
                    .clone()
                    .unwrap_or(CodexVersion::new(0, 0, 0)),
            },
            models: bundled.entries.iter().filter_map(spec_from_entry).collect(),
            fallback_reason: None,
            sources: Vec::new(),
        })
    }

    /// Decide and (for writes) build and preflight a profile's catalog file.
    /// Writes only a scratch copy for the preflight, which is removed again.
    pub(crate) async fn plan_catalog(&self, input: CatalogPlanInput<'_>) -> CatalogPlan {
        let d = &self.deps;
        let mut out = CatalogPlan::default();
        let profile = input.profile;
        let speed = profile_speed(profile);
        let mut reasons = catalog_reasons(profile, input.site, input.auth);
        if self.catalog_off.load(std::sync::atomic::Ordering::SeqCst) && !reasons.is_empty() {
            reasons.clear();
            out.notes
                .push(Notice::warning(text("switch.models.catalog_dropped")));
        }
        if input.auth.needs_authapi_env()
            && profile.model_overrides.values().any(override_needs_catalog)
        {
            out.notes
                .push(Notice::info(text("switch.models.pat_no_catalog")));
        }
        if reasons.is_empty() {
            return out;
        }
        let (app, cli) = local_codexes(input.install_id, input.installs, input.binaries);
        let Some(app) = app.filter(|a| a.binary.is_some() && a.version.is_some()) else {
            out.failed = Some(text("switch.models.no_consumer"));
            return out;
        };
        let (Some(app_binary), Some(app_version)) = (app.binary.clone(), app.version.clone())
        else {
            out.failed = Some(text("switch.models.no_consumer"));
            return out;
        };
        // The consumer's own catalog is the schema template.
        let template = match d.app.codex_debug_models(&app_binary, None).await {
            Ok(dump) => match d.models.parse_bundled(dump.as_bytes()) {
                Ok(t) => t,
                Err(e) => {
                    out.failed = Some(e.to_text());
                    return out;
                }
            },
            Err(e) => {
                out.failed = Some(e.to_text());
                return out;
            }
        };
        let list = if input.routed {
            self.router_member_list(&app_version, &app_binary).await
        } else {
            self.model_list_for(
                input.site,
                input.endpoints.map(|e| &e.openai_base_url),
                input.key,
                Some(&app_version),
                Some(&app_binary),
                false,
                input.profile.model_source,
            )
            .await
        };
        let source = match &list.source {
            ModelListSource::Upstream { .. }
                if !list.models.is_empty() && list.models.iter().all(|m| m.raw.is_some()) =>
            {
                CatalogSource::Native(list.models.clone())
            }
            ModelListSource::Upstream { .. } => CatalogSource::OpenAiList(list.models.clone()),
            ModelListSource::Feed { .. } => CatalogSource::Feed(list.models.clone()),
            _ => CatalogSource::Bundled,
        };
        let set = d.profiles.load().await.unwrap_or_default();
        let feeds = (d.feeds)(&set.settings.network);
        let feed_models = feeds
            .snapshot(false)
            .await
            .map(|s| s.models)
            .unwrap_or_default();
        let prompt = feeds.prompt(&app_version).await.ok().flatten();
        let mut slugs: Vec<String> = list.models.iter().map(|m| m.slug.clone()).collect();
        if matches!(source, CatalogSource::Bundled) {
            slugs.extend(
                template
                    .entries
                    .iter()
                    .filter_map(|e| e.get("slug").and_then(|s| s.as_str()).map(str::to_owned)),
            );
        }
        if let Some(m) = &input.configured_model {
            slugs.push(m.clone());
        }
        let max_of = |slug: &str| {
            list.model(slug)
                .and_then(ModelSpec::max_window)
                .or_else(|| {
                    template
                        .entries
                        .iter()
                        .find(|e| e.get("slug").and_then(|s| s.as_str()) == Some(slug))
                        .and_then(|e| {
                            let n = |k: &str| e.get(k).and_then(serde_json::Value::as_u64);
                            n("max_context_window").or_else(|| n("context_window"))
                        })
                })
        };
        let overrides = fold_globals(&profile.model_overrides, &profile.model, slugs, max_of);
        out.folded_globals =
            profile.model.context_window.is_some() || profile.model.auto_compact.is_some();
        let suffix = suffix_applies(input.site, input.auth, speed);
        let catalog_input = CatalogInput {
            consumer: app_version.clone(),
            template: &template,
            source,
            feed: &feed_models,
            prompt: prompt.as_deref(),
            overrides: &overrides,
            speed,
            tier_model_suffix: suffix,
            model: input.configured_model.as_deref(),
        };
        let file = match d.models.build(&catalog_input) {
            Ok(f) => f,
            Err(e) => {
                out.failed = Some(e.to_text());
                return out;
            }
        };
        // Preflight with the app's Codex, and with the CLI when it is another
        // version (both read the same config.toml).
        let mut consumers = vec![app_binary.clone()];
        let cli_version = cli.as_ref().and_then(|c| c.version.clone());
        if let Some(c) = &cli
            && c.version != app.version
            && let Some(b) = &c.binary
        {
            consumers.push(b.clone());
        }
        if let Err(reason) = self.preflight(&file, &consumers).await {
            out.failed = Some(reason);
            return out;
        }
        // IDE extensions run their own Codex on the same config.toml (often
        // another version): a refusal is reported, it does not block the switch.
        let mut ide_ok = Vec::new();
        for b in self.ide_codexes().await {
            if consumers.contains(&b) {
                continue;
            }
            match self.preflight(&file, std::slice::from_ref(&b)).await {
                Ok(()) => ide_ok.push(b),
                Err(reason) => out.notes.push(ide_refusal(&b, reason)),
            }
        }
        for n in &file.notes {
            out.notes.push(n.clone());
        }
        if suffix {
            let model = input.configured_model.clone();
            match (model, speed.model_suffix()) {
                (Some(m), Some(sfx)) if offers(&list, Some(&m), speed) => {
                    out.model = Some(format!("{m}{sfx}"));
                    out.speed_via_model = true;
                }
                _ => out
                    .notes
                    .push(Notice::info(text("switch.models.tier_not_offered"))),
            }
        }
        let home = d.ctx.codex_home();
        let path = home.model_catalog_file(&file.sha8);
        out.record = Some(CatalogRecord {
            file: file.file_name(),
            sha8: file.sha8.clone(),
            consumer: app_version,
            cli: cli_version,
            profile_id: Some(profile.id.clone()),
            ide: ide_ok,
            generated_at: d.clock.now(),
        });
        out.write = Some(CatalogWrite {
            path,
            bytes: file.bytes,
            sha8: file.sha8,
        });
        out
    }

    /// Run `codex debug models -c model_catalog_json=<scratch copy>` with every
    /// consumer binary; the scratch copy lives in the data dir and is removed.
    async fn preflight(
        &self,
        file: &CatalogFile,
        consumers: &[PathBuf],
    ) -> std::result::Result<(), LocalizedText> {
        let d = &self.deps;
        let dir = d.ctx.data_dir.root().join("tmp");
        let scratch = dir.join(format!("preflight-{}.json", file.sha8));
        write_atomic(&scratch, &file.bytes).map_err(|e| e.to_text())?;
        let mut result = Ok(());
        for binary in consumers {
            if let Err(e) = d.app.codex_debug_models(binary, Some(&scratch)).await {
                result = Err(text("switch.models.preflight_failed")
                    .arg("binary", superba_core::paths::display_path(binary))
                    .arg_text("reason", e.to_text()));
                break;
            }
        }
        let _ = std::fs::remove_file(&scratch);
        result
    }

    /// Union of the router members' lists (each from its own provider).
    async fn router_member_list(&self, version: &CodexVersion, binary: &Path) -> ModelList {
        let d = &self.deps;
        let set = d.profiles.load().await.unwrap_or_default();
        let mut lists = Vec::new();
        for id in &set.settings.router.members {
            let Some(p) = set.profile(id) else { continue };
            let Ok((site, resolved)) = self.direct_resolution(&set, p).await else {
                continue;
            };
            let superba_core::ResolvedMode::Token { endpoints, key, .. } = &resolved.mode else {
                continue;
            };
            let list = self
                .model_list_for(
                    &site,
                    Some(&endpoints.openai_base_url),
                    Some(key),
                    Some(version),
                    Some(binary),
                    false,
                    p.model_source,
                )
                .await;
            lists.push((p.name.clone(), list));
        }
        match d.models.union(&lists) {
            Ok(l) => l,
            Err(_) => lists
                .into_iter()
                .next()
                .map(|(_, l)| l)
                .unwrap_or(ModelList {
                    source: ModelListSource::Snapshot {
                        codex_version: version.clone(),
                    },
                    models: Vec::new(),
                    fallback_reason: None,
                    sources: Vec::new(),
                }),
        }
    }

    /// The profile a Models request is about: the requested one, else the
    /// applied profile, else the last selected, else the first key-based one.
    fn models_profile<'a>(
        set: &'a ProfileSet,
        requested: Option<&ProfileId>,
        state: Option<&ConfigState>,
    ) -> Result<&'a Profile> {
        if let Some(id) = requested {
            return set.profile(id).ok_or_else(|| Error::NotFound {
                what: format!("profile {id}"),
            });
        }
        let pick = |id: Option<&ProfileId>| {
            id.and_then(|id| set.profile(id))
                .filter(|p| p.builtin != Some(BuiltinProfile::Original))
        };
        pick(
            state
                .and_then(|s| s.active.as_ref())
                .and_then(|a| a.profile_id.as_ref()),
        )
        .or_else(|| pick(set.last_selected.as_ref()))
        .or_else(|| set.profiles.iter().find(|p| !p.is_builtin()))
        .ok_or_else(|| Error::NotFound {
            what: "a profile (add one first)".into(),
        })
    }

    /// The auth mode the app would most likely use for `profile` (display only:
    /// the applied mode when it is applied, else the profile's choice; router
    /// mode is API key).
    fn likely_auth(
        profile: &Profile,
        site: &Site,
        state: Option<&ConfigState>,
        routed: bool,
    ) -> AuthMode {
        if routed {
            return AuthMode::ApiKey;
        }
        if let Some(a) = state.and_then(|s| s.active.as_ref())
            && a.profile_id.as_ref() == Some(&profile.id)
            && let Some(m) = a.auth_mode
        {
            return m;
        }
        match profile.auth_choice() {
            AuthChoice::ApiKey => AuthMode::ApiKey,
            AuthChoice::Pat => AuthMode::Pat,
            _ if site.supports(AuthMode::Pat) => AuthMode::Pat,
            _ => AuthMode::ApiKey,
        }
    }

    pub(crate) async fn models_view_impl(&self, req: ModelsViewRequest) -> Result<ModelsView> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let set = d.profiles.load().await?;
        let state = d.config.read_state(&home).await.ok();
        let profile = Self::models_profile(&set, req.profile_id.as_ref(), state.as_ref())?;
        if profile.builtin == Some(BuiltinProfile::Original) {
            return Err(Error::invalid(format!(
                "built-in profile {} has no models of its own",
                profile.id
            )));
        }
        let sites = all_sites(&set, d.ctx.env.as_ref());
        let site = sites
            .iter()
            .find(|s| s.id == profile.site_id)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", profile.site_id),
            })?;
        let facts = self.monitor.facts().await;
        let installs = self.effective_installs(facts.installs.clone(), &set).await;
        let binaries = facts.binaries.clone();
        let install_id = set.settings.preferred_app_id.clone();
        let (app, _) = local_codexes(install_id.as_deref(), &installs, &binaries);
        let app_version = app.as_ref().and_then(|a| a.version.clone());
        let app_binary = app.as_ref().and_then(|a| a.binary.clone());
        let official =
            profile.builtin == Some(BuiltinProfile::Official) || site.kind == SiteKind::Official;
        let mut notes = Vec::new();
        if official {
            let mut list = self
                .model_list_for(
                    &site,
                    None,
                    None,
                    app_version.as_ref(),
                    app_binary.as_deref(),
                    req.refresh,
                    None,
                )
                .await;
            list.source = ModelListSource::OfficialReadOnly;
            let speed = speed_support(
                profile_speed(profile),
                &list,
                profile.model.model.as_deref(),
                true,
                false,
                false,
            );
            return Ok(ModelsView {
                profile_id: profile.id.clone(),
                site_id: site.id.clone(),
                read_only: true,
                list,
                defaults: profile.model.clone(),
                overrides: BTreeMap::new(),
                picker: ModelPicker::CodexDefault,
                picker_available: false,
                speed,
                catalog_file: CatalogFileStatus::NotNeeded,
                notes,
                per_model: false,
                applied: None,
            });
        }
        let (base_url, key) = match self.direct_resolution(&set, profile).await {
            Ok((_, r)) => match r.mode {
                superba_core::ResolvedMode::Token { endpoints, key, .. } => {
                    (Some(endpoints.openai_base_url), Some(key))
                }
                _ => (None, None),
            },
            Err(e) => {
                notes.push(Notice::info(e.to_text()));
                (None, None)
            }
        };
        let list = self
            .model_list_for(
                &site,
                base_url.as_ref(),
                key.as_ref(),
                app_version.as_ref(),
                app_binary.as_deref(),
                req.refresh,
                profile.model_source,
            )
            .await;
        self.model_lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(profile.id.clone(), list.clone());
        let routed = set.settings.router.enabled
            && state
                .as_ref()
                .and_then(|s| s.active.as_ref())
                .is_some_and(|a| a.profile_id.as_ref() == Some(&profile.id));
        let auth = Self::likely_auth(profile, &site, state.as_ref(), routed);
        let pat = auth.needs_authapi_env();
        let model = profile
            .model
            .model
            .clone()
            .or_else(|| state.as_ref().and_then(|s| s.model.clone()));
        let reasons = catalog_reasons(profile, &site, auth);
        // Without a catalog file an API-key Codex filters the tier with its own
        // bundled list (it never fetches the provider's). Whether there is one
        // depends on the tier too (Ultrafast and tier-suffix names need it).
        let needs_catalog = |tier: SpeedTier| {
            let mut p = profile.clone();
            p.model.speed = Some(tier);
            !catalog_reasons(&p, &site, auth).is_empty()
        };
        let bundled = if !pat
            && !matches!(list.source, ModelListSource::Bundled { .. })
            && (reasons.is_empty() || SpeedTier::ALL.into_iter().any(|t| !needs_catalog(t)))
        {
            self.bundled_list(app_binary.as_deref()).await
        } else {
            None
        };
        let mut speed = speed_support_with(
            profile_speed(profile),
            &list,
            bundled.as_ref().filter(|_| reasons.is_empty()),
            model.as_deref(),
            false,
            pat,
            site.capabilities.tier_model_suffix,
        );
        speed.tiers = tier_effects(
            &list,
            |tier| bundled.as_ref().filter(|_| !needs_catalog(tier)),
            model.as_deref(),
            false,
            pat,
            site.capabilities.tier_model_suffix,
        );
        let record = current_record(&d.ctx.data_dir, home.root())
            .filter(|r| r.profile_id.as_ref() == Some(&profile.id));
        let catalog_file = match (&record, reasons.is_empty()) {
            (Some(r), false) => CatalogFileStatus::Written {
                path: home.model_catalog_file(&r.sha8),
                sha8: r.sha8.clone(),
                consumer: r.consumer.clone(),
            },
            _ => CatalogFileStatus::NotNeeded,
        };
        if !reasons.is_empty() {
            notes.push(Notice::info(match &record {
                Some(r) => text("switch.models.frozen")
                    .arg("time", crate::localtime::local(r.generated_at).to_string()),
                None => text("switch.models.catalog_next_switch"),
            }));
            if site.kind == SiteKind::Krill {
                notes.push(Notice::warning(text("switch.models.fingerprint")));
            }
            if let (Some(r), Some(v)) = (&record, &app_version)
                && &r.consumer != v
            {
                notes.push(Notice::warning(
                    text("switch.models.consumer_changed")
                        .arg("built", &r.consumer)
                        .arg("now", v),
                ));
            }
        }
        if pat && profile.model_overrides.values().any(override_needs_catalog) {
            notes.push(Notice::info(text("switch.models.pat_no_catalog")));
        }
        let active = state
            .as_ref()
            .and_then(|s| s.active.as_ref())
            .is_some_and(|a| a.profile_id.as_ref() == Some(&profile.id));
        let applied = if active {
            Some(applied_models(&home).await)
        } else {
            None
        };
        Ok(ModelsView {
            profile_id: profile.id.clone(),
            site_id: site.id.clone(),
            read_only: false,
            list,
            defaults: profile.model.clone(),
            overrides: profile.model_overrides.clone(),
            picker: profile.model_picker,
            picker_available: site.kind == SiteKind::Compatible,
            speed,
            catalog_file,
            notes,
            per_model: !pat,
            applied,
        })
    }

    pub(crate) async fn save_models_impl(&self, edit: ModelsEdit) -> Result<ModelsEditReport> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let state = d.config.read_state(&d.ctx.codex_home()).await.ok();
        let id = Self::models_profile(&set, edit.profile_id.as_ref(), state.as_ref())?
            .id
            .clone();
        let sites = all_sites(&set, d.ctx.env.as_ref());
        let profile = set.profile(&id).cloned().ok_or_else(|| Error::NotFound {
            what: format!("profile {id}"),
        })?;
        let site = sites
            .iter()
            .find(|s| s.id == profile.site_id)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", profile.site_id),
            })?;
        if profile.is_builtin() || site.kind == SiteKind::Official {
            return Err(Error::invalid(
                "the official login uses Codex's own models; they are shown read-only",
            ));
        }
        let list = self
            .model_lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned();
        let spec = |slug: &str| list.as_ref().and_then(|l| l.model(slug)).cloned();
        let mut notes = Vec::new();
        let mut p = profile.clone();
        if let Some(defaults) = &edit.defaults {
            superba_core::check_all_models(defaults)
                .map_err(|e| Error::invalid(format!("all models: {}", e.message())))?;
            p.model = defaults.clone();
        }
        if edit.reset_all {
            p.model_overrides.clear();
        }
        for slug in &edit.reset {
            p.model_overrides.remove(slug);
        }
        let mut site_edit: Option<Site> = None;
        for (slug, o) in &edit.set {
            if let Some(upstream) = &o.upstream_name {
                if !set.custom_sites.iter().any(|c| c.id == site.id) {
                    return Err(Error::invalid(format!(
                        "{}: upstream model names can be set for custom sites only",
                        site.id
                    )));
                }
                let s = site_edit.get_or_insert_with(|| site.clone());
                if upstream.trim().is_empty() || upstream == slug {
                    s.model_map.remove(slug);
                } else {
                    s.model_map.insert(slug.clone(), upstream.clone());
                }
            }
            let mut stored = o.clone();
            stored.upstream_name = None;
            if stored.is_empty() {
                p.model_overrides.remove(slug);
                continue;
            }
            let model = spec(slug);
            let check = match &model {
                Some(m) => match d.models.validate_override(m, &stored) {
                    Ok(c) => c,
                    Err(e) if e.is_not_implemented() => check_override(Some(m), &stored)?,
                    Err(e) => return Err(e),
                },
                None => check_override(None, &stored)?,
            };
            notes.extend(check.notes);
            p.model_overrides.insert(slug.clone(), stored);
        }
        if let Some(picker) = edit.picker {
            if picker == ModelPicker::Provider && site.kind != SiteKind::Compatible {
                return Err(Error::invalid(
                    "showing the provider's models is offered for custom sites only",
                ));
            }
            p.model_picker = picker;
        }
        let auth = Self::likely_auth(&p, &site, state.as_ref(), set.settings.router.enabled);
        // The bounds the Models page and `spb models` check (user bug 7): every
        // override this edit stores, and with new all-models values every
        // override that reaches Codex, against the window the model ends up with.
        for (slug, o) in &p.model_overrides {
            let touched = edit.set.contains_key(slug);
            if !touched && (edit.defaults.is_none() || auth.needs_authapi_env()) {
                continue;
            }
            superba_core::check_model_override(spec(slug).as_ref(), o, &p.model)
                .map_err(|e| Error::invalid(format!("{slug}: {}", e.message())))?;
        }
        let before = catalog_reasons(&profile, &site, auth);
        let after = catalog_reasons(&p, &site, auth);
        if before.is_empty() && !after.is_empty() {
            notes.push(Notice::info(text("switch.models.catalog_from_next_switch")));
            if site.kind == SiteKind::Krill {
                notes.push(Notice::warning(text("switch.models.fingerprint")));
            }
        }
        if !before.is_empty() && after.is_empty() {
            notes.push(Notice::info(text(
                "switch.models.catalog_removed_next_switch",
            )));
        }
        if auth.needs_authapi_env() && p.model_overrides.values().any(override_needs_catalog) {
            notes.push(Notice::info(text("switch.models.pat_no_catalog")));
        }
        if let Some(slot) = set.profiles.iter_mut().find(|x| x.id == id) {
            *slot = p.clone();
        }
        d.profiles.save(&set).await?;
        if let Some(s) = site_edit {
            self.save_site_impl(s).await?;
        }
        Ok(ModelsEditReport { profile: p, notes })
    }
}

impl Switcher {
    /// Start-up check of the local catalog file: when the app's (or the CLI's)
    /// Codex changed since the file was made, preflight the file with the new
    /// binaries; when one refuses it, rewrite the applied profile write-only
    /// (a rebuilt file, else the key removed), so a stale file never stops the
    /// app-server. A running IDE extension's Codex that has not seen the file
    /// yet is preflighted too (a refusal is reported). Returns what happened.
    pub(crate) async fn catalog_startup_check(&self) -> Vec<Notice> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let Some(record) = current_record(&d.ctx.data_dir, home.root()) else {
            return Vec::new();
        };
        let Ok(set) = d.profiles.load().await else {
            return Vec::new();
        };
        let installs = self
            .effective_installs(d.app.detect_installs().await.unwrap_or_default(), &set)
            .await;
        let binaries = d.app.codex_binaries().await.unwrap_or_default();
        let (app, cli) = local_codexes(
            set.settings.preferred_app_id.as_deref(),
            &installs,
            &binaries,
        );
        let path = home.model_catalog_file(&record.sha8);
        let mut notes = self.ide_startup_check(&record, &path).await;
        let app_v = app.as_ref().and_then(|a| a.version.clone());
        let cli_v = cli.as_ref().and_then(|c| c.version.clone());
        let cli_changed = cli_v.is_some() && cli_v != record.cli && cli_v != app_v;
        if app_v.is_none() || (app_v.as_ref() == Some(&record.consumer) && !cli_changed) {
            return notes;
        }
        let mut binaries_to_check = Vec::new();
        if let Some(b) = app.as_ref().and_then(|a| a.binary.clone()) {
            binaries_to_check.push(b);
        }
        if cli_changed && let Some(b) = cli.as_ref().and_then(|c| c.binary.clone()) {
            binaries_to_check.push(b);
        }
        let mut still_valid = path.exists();
        for b in &binaries_to_check {
            if still_valid {
                still_valid = d.app.codex_debug_models(b, Some(&path)).await.is_ok();
            }
        }
        let built = record.consumer.clone();
        if still_valid {
            let mut updated = current_record(&d.ctx.data_dir, home.root()).unwrap_or(record);
            if let Some(v) = app_v {
                updated.consumer = v;
            }
            updated.cli = cli_v;
            if let Err(e) = commit_record(&d.ctx.data_dir, &home, Some(updated)) {
                tracing::warn!(error = %e, "cannot update the model catalog record");
            }
            return notes;
        }
        let app_changed = app_v.as_ref() != Some(&built);
        let now = if app_changed {
            app_v.clone().unwrap_or(built.clone())
        } else {
            cli_v.clone().unwrap_or_else(|| built.clone())
        };
        notes.push(Notice::warning(if app_changed {
            text("switch.models.consumer_changed")
                .arg("built", &built)
                .arg("now", &now)
        } else {
            // Only the terminal Codex (first `codex` on PATH) changed.
            text("switch.models.cli_changed")
                .arg("built", record.cli.clone().unwrap_or_else(|| built.clone()))
                .arg("now", &now)
        }));
        match self.rewrite_applied_profile(false).await {
            Ok(()) => notes.push(Notice::info(text("switch.models.catalog_rebuilt"))),
            Err(first) => match self.rewrite_applied_profile(true).await {
                Ok(()) => notes.push(Notice::warning(text("switch.models.catalog_dropped"))),
                Err(_) => notes.push(Notice::danger(
                    text("switch.models.catalog_unrepaired")
                        .arg("now", &now)
                        .arg("file", superba_core::paths::display_path(&path))
                        .arg(
                            "config",
                            superba_core::paths::display_path(&home.config_toml()),
                        )
                        .arg_text("reason", first),
                )),
            },
        }
        for n in &notes {
            tracing::warn!(note = %n.text.render_fallback(), "model catalog start-up check");
        }
        notes
    }

    /// Preflight the catalog file with running IDE extensions' Codex binaries
    /// that have not accepted it yet; accepted ones are remembered, refusals
    /// are reported (every start, until fixed).
    async fn ide_startup_check(&self, record: &CatalogRecord, path: &Path) -> Vec<Notice> {
        let d = &self.deps;
        let mut notes = Vec::new();
        let mut accepted = Vec::new();
        if !path.exists() {
            return notes;
        }
        for b in self.ide_codexes().await {
            if record.ide.contains(&b) {
                continue;
            }
            match d.app.codex_debug_models(&b, Some(path)).await {
                Ok(_) => accepted.push(b),
                Err(e) => notes.push(ide_refusal(&b, e.to_text())),
            }
        }
        if !accepted.is_empty() {
            let mut updated = record.clone();
            updated.ide.extend(accepted);
            let home = d.ctx.codex_home();
            if let Err(e) = commit_record(&d.ctx.data_dir, &home, Some(updated)) {
                tracing::warn!(error = %e, "cannot update the model catalog record");
            }
        }
        notes
    }

    /// Executables of the IDE extensions' Codex app-servers running on this
    /// home (they read the same config.toml).
    pub(crate) async fn ide_codexes(&self) -> Vec<PathBuf> {
        let home = self.deps.ctx.codex_home();
        let processes = self.deps.app.processes(&home).await.unwrap_or_default();
        let mut out: Vec<PathBuf> = Vec::new();
        for p in processes {
            let same_home = p.codex_home.as_deref().is_none_or(|h| h == home.root());
            if p.kind == superba_core::CodexProcessKind::IdeAppServer
                && same_home
                && let Some(exe) = p.exe
                && !out.contains(&exe)
            {
                out.push(exe);
            }
        }
        out
    }

    /// Plan the applied profile again to repair its catalog file: the user's
    /// own edits of owned keys are kept and unknown versions accepted (the file
    /// is Superba's own and the switch only writes). `without_catalog` leaves
    /// the catalog out. The plan's context is taken out of the cache.
    async fn plan_catalog_repair(
        &self,
        without_catalog: bool,
    ) -> std::result::Result<(superba_core::SwitchPlan, crate::planner::PlanContext), LocalizedText>
    {
        use std::sync::atomic::Ordering;
        let d = &self.deps;
        let Some(id) = self.applied_profile().await else {
            return Err(text("switch.models.repair_no_profile"));
        };
        let state = d
            .config
            .read_state(&d.ctx.codex_home())
            .await
            .map_err(|e| e.to_text())?;
        let mut request = superba_core::SwitchRequest::profile(id);
        request.accept_unknown_versions = true;
        for k in &state.drift {
            request
                .key_resolutions
                .insert(k.path.clone(), superba_core::KeyResolution::KeepCurrent);
        }
        self.catalog_off.store(without_catalog, Ordering::SeqCst);
        let plan = self.plan_impl(request).await;
        self.catalog_off.store(false, Ordering::SeqCst);
        let plan = plan.map_err(|e| e.to_text())?;
        let pc = self.take_plan(&plan.id);
        if let superba_core::RestartKind::Blocked { reasons } = &plan.primary {
            return Err(reasons
                .first()
                .map(superba_core::BlockReason::text)
                .unwrap_or_else(|| text("switch.models.repair_question")));
        }
        if !plan.choices.is_empty() {
            return Err(text("switch.models.repair_question"));
        }
        let pc = pc.ok_or_else(|| text("switch.models.repair_question"))?;
        Ok((plan, pc))
    }

    /// Write the applied profile again without restarting anything (start-up
    /// repair). `without_catalog` leaves the model catalog out.
    async fn rewrite_applied_profile(
        &self,
        without_catalog: bool,
    ) -> std::result::Result<(), LocalizedText> {
        let (plan, pc) = self.plan_catalog_repair(without_catalog).await?;
        self.remember_plan(plan.id.clone(), pc);
        let outcome = self
            .execute_impl(
                plan,
                superba_core::ExecuteOptions {
                    write_only: true,
                    force_turns: true,
                    allow_full_restart: true,
                    ..Default::default()
                },
                std::sync::Arc::new(superba_core::SafeDefaults),
                std::sync::Arc::new(superba_core::NoEvents),
            )
            .await
            .map_err(|e| e.to_text())?;
        if matches!(
            outcome.status,
            superba_core::OutcomeStatus::WrittenOnly | superba_core::OutcomeStatus::Switched
        ) {
            Ok(())
        } else {
            Err(text("switch.models.repair_question"))
        }
    }

    /// Plan-time part of a pin change: true when `config.toml` names a model
    /// catalog file that may not fit the pin's Codex (made for another version,
    /// or for an unknown one); a note says it is checked before the restart.
    pub(crate) async fn pin_catalog_check(
        &self,
        pin: &superba_core::AppCodexPin,
        install: Option<&superba_core::AppInstall>,
        warnings: &mut Vec<Notice>,
    ) -> bool {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        if named_catalog_path(&home).is_none() {
            return false;
        }
        let version = match pin {
            superba_core::AppCodexPin::Version { version } => Some(version.clone()),
            superba_core::AppCodexPin::Path { path } => d
                .app
                .codex_binaries()
                .await
                .unwrap_or_default()
                .into_iter()
                .find(|b| crate::versions::same_file(&b.path, path))
                .and_then(|b| b.version),
            _ => install.and_then(|i| i.bundled_codex_version.clone()),
        };
        let record = current_record(&d.ctx.data_dir, home.root());
        if let (Some(r), Some(v)) = (&record, &version)
            && &r.consumer == v
        {
            return false;
        }
        warnings.push(Notice::info(text("switch.pin.catalog_check").arg(
            "version",
            version.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
        )));
        true
    }

    /// Execution part of a pin change: the pin's Codex preflights the file
    /// `config.toml` names; when it refuses it, the applied profile is planned
    /// again as if the pin were stored (a catalog file rebuilt for that Codex,
    /// else the key left out). `Ok(None)` = the file fits; `Err` = the
    /// configuration cannot be repaired (the pin is blocked).
    pub(crate) async fn pin_catalog_rewrite(
        &self,
        pin: &crate::versions::PinPlan,
        install_id: Option<&str>,
    ) -> std::result::Result<Option<PinCatalogRewrite>, LocalizedText> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let Some(file) = named_catalog_path(&home) else {
            return Ok(None);
        };
        let binary = match &pin.exe {
            Some(p) => Some(p.clone()),
            None => {
                let installs = d.app.detect_installs().await.unwrap_or_default();
                install_id
                    .and_then(|id| installs.iter().find(|i| i.id == id))
                    .or_else(|| installs.first())
                    .and_then(|i| i.bundled_codex.clone())
            }
        };
        if let Some(b) = &binary
            && file.is_file()
            && d.app.codex_debug_models(b, Some(&file)).await.is_ok()
        {
            return Ok(None);
        }
        *self.pin_override.lock().unwrap_or_else(|p| p.into_inner()) = Some(pin.pin.clone());
        let rebuilt = self.plan_catalog_repair(false).await;
        let result = match rebuilt {
            Ok(r) => Ok((r, Notice::info(text("switch.pin.catalog_rebuilt")))),
            Err(first) => match self.plan_catalog_repair(true).await {
                Ok(r) => Ok((r, Notice::warning(text("switch.pin.catalog_dropped")))),
                Err(_) => Err(first),
            },
        };
        *self.pin_override.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let ((plan, pc), note) = result?;
        let Some(config) = plan.config.filter(|c| !c.is_noop()) else {
            return Err(text("switch.models.repair_question"));
        };
        Ok(Some(PinCatalogRewrite {
            config,
            catalog: pc.catalog,
            record: pc.catalog_record,
            notes: vec![note],
        }))
    }
}

/// A pin change's rewrite of `config.toml` for the new Codex.
pub(crate) struct PinCatalogRewrite {
    /// The applied profile's configuration for the new Codex.
    pub config: superba_core::ApplyPlan,
    /// The rebuilt catalog file (`None` = left out).
    pub catalog: Option<CatalogWrite>,
    /// The catalog record after the pin.
    pub record: Option<Option<CatalogRecord>>,
    /// What happened.
    pub notes: Vec<Notice>,
}

/// An IDE extension's Codex refuses the catalog file.
fn ide_refusal(binary: &Path, reason: LocalizedText) -> Notice {
    Notice::warning(
        text("switch.models.ide_refuses")
            .arg("binary", superba_core::paths::display_path(binary))
            .arg_text("reason", reason),
    )
}
