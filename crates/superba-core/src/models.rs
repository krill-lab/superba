//! Model lists, per-model overrides, speed tiers and the model catalog file
//! (v0.3 Models page, `spb models`, `spb speed`).
//!
//! Facts (Codex 0.140-0.163, verified in the v0.3 spikes):
//! - In API-key mode with `openai_base_url` set Codex never fetches `/models`; the
//!   app's picker shows the Codex binary's bundled list. Only a
//!   `model_catalog_json` file ([`crate::codex::config_keys::MODEL_CATALOG_JSON`])
//!   changes it, and the same file carries per-model context windows, compaction
//!   thresholds and service tiers.
//! - The file's schema is the consumer Codex version's own `ModelInfo`: a catalog
//!   is generated per consumer version (`superba-models`) and preflighted with that
//!   binary before `config.toml` names it.
//! - One global `service_tier` applies to every model, filtered by the model's
//!   catalog tiers; the desktop app in API-key mode always sends Standard, so a
//!   tier reaches it only through tier-suffix model names on sites that map them
//!   ([`crate::SiteCapabilities::tier_model_suffix`]).
//!
//! Additive owner in v0.3 Phase 1: the catalog/models builder (types);
//! `superba-switch` consumes them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::codex::CodexVersion;
use crate::i18n::{LocalizedText, Notice};
use crate::ids::{ProfileId, SiteId};
use crate::profile::{ModelDefaults, Profile};
use crate::time::Timestamp;

/// Smallest context window an override may set (16K tokens).
pub const MIN_CONTEXT_WINDOW: u64 = 16_000;
/// Largest context window Superba accepts where no model maximum applies (the
/// all-models value, a model without a known maximum): a sanity bound against
/// typos (10M tokens).
pub const MAX_CONTEXT_WINDOW: u64 = 10_000_000;
/// Smallest compaction threshold Superba accepts (8K tokens; below that Codex
/// would compact after nearly every turn).
pub const MIN_AUTO_COMPACT: u64 = 8_000;
/// Codex clamps a compaction threshold to this percentage of the window.
pub const AUTO_COMPACT_MAX_PERCENT: u8 = 90;
/// Codex's default `effective_context_window_percent` (usable share of the window).
pub const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u8 = 95;

// ---------------------------------------------------------------------------
// Speed tiers
// ---------------------------------------------------------------------------

/// The profile's speed tier (one per profile: Codex has a single global
/// `service_tier`).
///
/// Serialized as `standard` / `fast` / `ultrafast`. [`SpeedTier::config_value`]
/// is what goes into `config.toml` and [`SpeedTier::from_config_value`] reads it.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SpeedTier {
    /// Standard routing (written as `service_tier = "default"` so the terminal
    /// Codex never applies a model's catalog default tier on its own).
    #[default]
    Standard,
    /// Fast (`priority` on the wire).
    Fast,
    /// Ultrafast (only for models whose catalog entry lists it).
    Ultrafast,
}

impl SpeedTier {
    /// Every tier, slowest first.
    pub const ALL: [SpeedTier; 3] = [SpeedTier::Standard, SpeedTier::Fast, SpeedTier::Ultrafast];

    /// Value written to `config.toml` `service_tier` (what the Codex TUI writes
    /// back too): `default`, `fast`, `ultrafast`.
    pub fn config_value(self) -> &'static str {
        match self {
            SpeedTier::Standard => "default",
            SpeedTier::Fast => "fast",
            SpeedTier::Ultrafast => "ultrafast",
        }
    }

    /// Parse a `config.toml` `service_tier` value (`default`, `fast` or
    /// `priority`, `ultrafast`; `standard` is accepted too). `flex` and unknown
    /// values have no tier here.
    pub fn from_config_value(value: &str) -> Option<SpeedTier> {
        match value.trim() {
            "default" | "standard" => Some(SpeedTier::Standard),
            "fast" | "priority" => Some(SpeedTier::Fast),
            "ultrafast" => Some(SpeedTier::Ultrafast),
            _ => None,
        }
    }

    /// The request body's `service_tier` for this tier (`None` for Standard: Codex
    /// sends no tier).
    pub fn request_value(self) -> Option<&'static str> {
        match self {
            SpeedTier::Standard => None,
            SpeedTier::Fast => Some("priority"),
            SpeedTier::Ultrafast => Some("ultrafast"),
        }
    }

    /// The tier for a catalog service tier id (`priority` / `fast`, `ultrafast`).
    pub fn from_catalog_id(id: &str) -> Option<SpeedTier> {
        match id {
            "priority" | "fast" => Some(SpeedTier::Fast),
            "ultrafast" => Some(SpeedTier::Ultrafast),
            _ => None,
        }
    }

    /// Suffix of the tier-suffix model name (`gpt-6-astra-ultrafast`); `None` for
    /// Standard.
    pub fn model_suffix(self) -> Option<&'static str> {
        match self {
            SpeedTier::Standard => None,
            SpeedTier::Fast => Some("-fast"),
            SpeedTier::Ultrafast => Some("-ultrafast"),
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            SpeedTier::Standard => LocalizedText::new("speed_tier.standard", "Standard"),
            SpeedTier::Fast => LocalizedText::new("speed_tier.fast", "Fast"),
            SpeedTier::Ultrafast => LocalizedText::new("speed_tier.ultrafast", "Ultrafast"),
        }
    }
}

/// What one consumer (terminal Codex or the desktop app) actually gets for the
/// profile's speed tier.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SpeedEffect {
    /// The tier applies as chosen.
    #[default]
    Applies,
    /// The tier applies through a tier-suffix model name the site maps
    /// (`gpt-6-astra-ultrafast`, shown as "6 Astra Ultrafast").
    ViaModelName {
        /// The model name written.
        model: String,
    },
    /// The desktop app in API-key mode sends no tier: it runs Standard.
    AppForcesStandard,
    /// The selected model does not offer the tier: it runs Standard.
    NotOffered,
    /// The app's own speed menu decides (official login and PAT).
    AppMenu,
}

impl SpeedEffect {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            SpeedEffect::Applies => LocalizedText::new("speed_effect.applies", "Applies"),
            SpeedEffect::ViaModelName { model } => LocalizedText::new(
                "speed_effect.via_model_name",
                "Applies through the model name {model}",
            )
            .arg("model", model),
            SpeedEffect::AppForcesStandard => LocalizedText::new(
                "speed_effect.app_forces_standard",
                "The desktop app runs Standard in API-key mode",
            ),
            SpeedEffect::NotOffered => LocalizedText::new(
                "speed_effect.not_offered",
                "The model does not offer this tier; it runs Standard",
            ),
            SpeedEffect::AppMenu => {
                LocalizedText::new("speed_effect.app_menu", "Set in the app's own speed menu")
            }
        }
    }
}

/// Where a profile's speed tier applies (the per-consumer effect line).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedSupport {
    /// Tiers some listed model offers, Standard first (the control hides the
    /// others).
    #[serde(default = "standard_only")]
    pub offered: Vec<SpeedTier>,
    /// Terminal Codex (TUI, `codex exec`).
    #[serde(default)]
    pub cli: SpeedEffect,
    /// The desktop app.
    #[serde(default)]
    pub app: SpeedEffect,
    /// What each tier would do (the same two effects for every tier), so a tier
    /// picked but not saved yet shows its own effect, not the saved tier's.
    /// Empty = only [`SpeedSupport::cli`] / [`SpeedSupport::app`] are known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<TierEffect>,
}

/// The per-consumer effect of one speed tier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEffect {
    /// The tier.
    pub tier: SpeedTier,
    /// Terminal Codex.
    pub cli: SpeedEffect,
    /// The desktop app.
    pub app: SpeedEffect,
}

impl SpeedSupport {
    /// The terminal and desktop-app effects of `tier`: its entry in
    /// [`SpeedSupport::tiers`], else the saved tier's [`SpeedSupport::cli`] and
    /// [`SpeedSupport::app`].
    pub fn effect(&self, tier: SpeedTier) -> (&SpeedEffect, &SpeedEffect) {
        self.tiers
            .iter()
            .find(|t| t.tier == tier)
            .map_or((&self.cli, &self.app), |t| (&t.cli, &t.app))
    }
}

fn standard_only() -> Vec<SpeedTier> {
    vec![SpeedTier::Standard]
}

impl Default for SpeedSupport {
    fn default() -> Self {
        Self {
            offered: standard_only(),
            cli: SpeedEffect::Applies,
            app: SpeedEffect::Applies,
            tiers: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Model specs and lists
// ---------------------------------------------------------------------------

/// Codex's `visibility` of a catalog entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelVisibility {
    /// Shown in the picker.
    #[default]
    List,
    /// Hidden from the picker (still usable by name).
    Hide,
    /// Not offered at all.
    None,
    /// A value this Superba does not know (the raw entry keeps the original).
    #[serde(other)]
    Other,
}

/// One reasoning effort a model supports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningLevel {
    /// Effort (`low`, `medium`, `high`, `xhigh`, ...).
    pub effort: String,
    /// Upstream description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One service tier a model offers (Codex `ModelServiceTier`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceTierSpec {
    /// Wire id (`priority`, `ultrafast`, `flex`).
    pub id: String,
    /// Display name (`Fast`).
    #[serde(default)]
    pub name: String,
    /// Upstream description ("2x speed, increased usage").
    #[serde(default)]
    pub description: String,
}

/// A model as Superba sees it, from any source (Codex-native `/models` entry,
/// OpenAI-style list item with metadata, feed entry, bundled dump).
///
/// Token counts are `u64`; `None` = unknown. Codex-native entries keep their full
/// JSON in [`ModelSpec::raw`] so a catalog file can reproduce them verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Model slug (the `model` value).
    pub slug: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Upstream description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Picker visibility.
    #[serde(default)]
    pub visibility: ModelVisibility,
    /// Usable with an API key (`false` entries are dropped in API-key mode).
    #[serde(default = "yes")]
    pub supported_in_api: bool,
    /// Sort priority (lower first), when the source has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Largest window an override may set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    /// Usable share of the window in percent (Codex default 95).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_context_window_percent: Option<u8>,
    /// Compaction threshold (Codex default: 90 % of the window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<u64>,
    /// Supported reasoning efforts (empty for non-reasoning models).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// Default reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_level: Option<String>,
    /// Service tiers the model offers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_tiers: Vec<ServiceTierSpec>,
    /// Catalog default tier id (applied by the TUI when config has no
    /// `service_tier`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
    /// Input modalities (`text`, `image`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<String>,
    /// The Codex-native catalog entry this spec was read from, unchanged
    /// (unknown fields preserved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

fn yes() -> bool {
    true
}

impl Default for ModelSpec {
    /// An empty slug, listed and usable with an API key.
    fn default() -> Self {
        Self {
            slug: String::new(),
            display_name: None,
            description: None,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: None,
            context_window: None,
            max_context_window: None,
            effective_context_window_percent: None,
            auto_compact_token_limit: None,
            reasoning_levels: Vec::new(),
            default_reasoning_level: None,
            service_tiers: Vec::new(),
            default_service_tier: None,
            input_modalities: Vec::new(),
            raw: None,
        }
    }
}

impl ModelSpec {
    /// A spec with only a slug (API usable, listed).
    pub fn new(slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            ..Self::default()
        }
    }

    /// Display name, else the slug.
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.slug)
    }

    /// Read a Codex-native catalog entry (a `ModelInfo` of `/backend-api/codex/models`,
    /// `codex debug models` or a release's `models.json`). The entry is kept
    /// unchanged in [`ModelSpec::raw`]; `None` when it has no non-empty `slug`.
    ///
    /// Lenient on purpose: a field of an unexpected type is treated as absent
    /// (Codex versions differ), negative token counts are dropped.
    pub fn from_codex_entry(entry: &serde_json::Value) -> Option<ModelSpec> {
        let o = entry.as_object()?;
        let text = |k: &str| {
            o.get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let count = |k: &str| o.get(k).and_then(serde_json::Value::as_u64);
        let slug = text("slug")?;
        let visibility = match o.get("visibility").and_then(serde_json::Value::as_str) {
            Some("list") | None => ModelVisibility::List,
            Some("hide") => ModelVisibility::Hide,
            Some("none") => ModelVisibility::None,
            Some(_) => ModelVisibility::Other,
        };
        let reasoning_levels = o
            .get("supported_reasoning_levels")
            .and_then(serde_json::Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|l| {
                        let effort = l
                            .get("effort")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| l.as_str())?;
                        Some(ReasoningLevel {
                            effort: effort.to_owned(),
                            description: l
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let service_tiers = o
            .get("service_tiers")
            .and_then(serde_json::Value::as_array)
            .map(|tiers| {
                tiers
                    .iter()
                    .filter_map(|t| {
                        let id = t.get("id").and_then(serde_json::Value::as_str)?;
                        let field = |k: &str| {
                            t.get(k)
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned()
                        };
                        Some(ServiceTierSpec {
                            id: id.to_owned(),
                            name: field("name"),
                            description: field("description"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let input_modalities = o
            .get("input_modalities")
            .and_then(serde_json::Value::as_array)
            .map(|m| {
                m.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(ModelSpec {
            display_name: text("display_name"),
            description: text("description"),
            visibility,
            supported_in_api: o
                .get("supported_in_api")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            priority: o.get("priority").and_then(serde_json::Value::as_i64),
            context_window: count("context_window"),
            max_context_window: count("max_context_window"),
            effective_context_window_percent: count("effective_context_window_percent")
                .and_then(|p| u8::try_from(p).ok()),
            auto_compact_token_limit: count("auto_compact_token_limit"),
            reasoning_levels,
            default_reasoning_level: text("default_reasoning_level"),
            service_tiers,
            default_service_tier: text("default_service_tier"),
            input_modalities,
            raw: Some(entry.clone()),
            slug,
        })
    }

    /// Context window Codex resolves for this model (`context_window`, else
    /// `max_context_window`).
    pub fn resolved_context_window(&self) -> Option<u64> {
        self.context_window.or(self.max_context_window)
    }

    /// Largest window an override may set: `max_context_window`, else the
    /// listed window (the larger of the two when an upstream lists both the
    /// wrong way round).
    pub fn max_window(&self) -> Option<u64> {
        match (self.max_context_window, self.context_window) {
            (Some(max), Some(window)) => Some(max.max(window)),
            (max, window) => max.or(window),
        }
    }

    /// Speed tiers this model offers through its catalog tiers, Standard first.
    pub fn speed_tiers(&self) -> Vec<SpeedTier> {
        let mut out = vec![SpeedTier::Standard];
        for t in &self.service_tiers {
            if let Some(tier) = SpeedTier::from_catalog_id(&t.id)
                && !out.contains(&tier)
            {
                out.push(tier);
            }
        }
        out.sort();
        out
    }
}

/// Where a model list came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelListSource {
    /// The provider's own list (`/backend-api/codex/models?client_version=..` or
    /// `<base>/models`).
    Upstream {
        /// Path (and query) that answered, without origin or key.
        path: String,
        /// When fetched.
        fetched_at: Timestamp,
        /// Its ETag, when sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        etag: Option<String>,
    },
    /// Superba's signed model feed (fetched from a mirror or cached).
    Feed {
        /// Codex version the entries are for.
        codex_version: CodexVersion,
        /// Mirror that served the feed (`builtin` / `cache` when not fetched now).
        mirror: String,
        /// Feed serial.
        serial: u64,
        /// When fetched.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fetched_at: Option<Timestamp>,
    },
    /// The list compiled into Superba.
    Snapshot {
        /// Codex version of the snapshot.
        codex_version: CodexVersion,
    },
    /// `codex debug models --bundled` of a local Codex binary.
    Bundled {
        /// That binary's version.
        codex_version: CodexVersion,
    },
    /// The official ChatGPT login: Codex's own list, shown read-only.
    OfficialReadOnly,
    /// The union of the router members' lists (each model from the first member
    /// that lists it).
    Union {
        /// Member labels in priority order.
        members: Vec<String>,
    },
}

/// A model list with its source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelList {
    /// Source.
    pub source: ModelListSource,
    /// Models in source order.
    #[serde(default)]
    pub models: Vec<ModelSpec>,
    /// Why a better source failed (upstream not listing models, network), when
    /// this list is a fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<crate::Error>,
}

impl ModelList {
    /// The spec of `slug`.
    pub fn model(&self, slug: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|m| m.slug == slug)
    }

    /// Tiers some model offers, Standard first.
    pub fn offered_tiers(&self) -> Vec<SpeedTier> {
        let mut out = vec![SpeedTier::Standard];
        for tier in self.models.iter().flat_map(ModelSpec::speed_tiers) {
            if !out.contains(&tier) {
                out.push(tier);
            }
        }
        out.sort();
        out
    }
}

// ---------------------------------------------------------------------------
// Overrides, picker mode, the catalog file
// ---------------------------------------------------------------------------

/// The user's changes to one model of a profile (no per-model speed: Codex has
/// one global tier).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOverride {
    /// Context window (tokens); must be >= [`MIN_CONTEXT_WINDOW`] and <= the
    /// model's `max_context_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Compaction threshold (tokens; Codex clamps it to 90 % of the window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<u64>,
    /// Display name in the app's picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Hide from the picker (`visibility = "hide"`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// The site's name for this model (custom sites; edits
    /// [`crate::Site::model_map`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_name: Option<String>,
}

impl ModelOverride {
    /// True if the override changes nothing.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Which list the app's model picker shows for a custom site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelPicker {
    /// Codex's own list (no catalog file needed).
    #[default]
    CodexDefault,
    /// The provider's models (written as a catalog file).
    Provider,
}

impl ModelPicker {
    /// True for [`ModelPicker::CodexDefault`] (serde skip helper).
    pub fn is_default(&self) -> bool {
        matches!(self, ModelPicker::CodexDefault)
    }
}

/// State of the profile's `model_catalog_json` file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CatalogFileStatus {
    /// No file is needed (no override, no provider picker, no tier that needs one).
    #[default]
    NotNeeded,
    /// Written and preflighted: `<CODEX_HOME>/superba/models-<sha8>.json`.
    Written {
        /// Absolute path.
        path: PathBuf,
        /// First 8 hex digits of the content's sha256.
        sha8: String,
        /// Codex version the file was generated for.
        consumer: CodexVersion,
    },
    /// Generation or preflight failed; nothing was written.
    Failed {
        /// Why.
        reason: LocalizedText,
    },
}

impl CatalogFileStatus {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            CatalogFileStatus::NotNeeded => {
                LocalizedText::new("catalog_file.not_needed", "No model catalog file")
            }
            CatalogFileStatus::Written { consumer, .. } => LocalizedText::new(
                "catalog_file.written",
                "Model catalog file written for Codex {version}",
            )
            .arg("version", consumer),
            CatalogFileStatus::Failed { reason } => LocalizedText::new(
                "catalog_file.failed",
                "Model catalog file not written: {reason}",
            )
            .arg_text("reason", reason.clone()),
        }
    }
}

/// What [`crate::SwitchService::models_view`] shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsViewRequest {
    /// Profile (`None` = the applied profile, else the last selected one).
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// Bypass the list cache (Refresh).
    #[serde(default)]
    pub refresh: bool,
}

/// The Models page of one profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsView {
    /// Profile.
    pub profile_id: ProfileId,
    /// Its site.
    pub site_id: SiteId,
    /// Official login: shown read-only.
    #[serde(default)]
    pub read_only: bool,
    /// The model list and its source.
    pub list: ModelList,
    /// The profile's defaults (model, effort, speed, all-models window and
    /// compaction).
    #[serde(default)]
    pub defaults: ModelDefaults,
    /// Per-model overrides by slug.
    #[serde(default)]
    pub overrides: BTreeMap<String, ModelOverride>,
    /// Picker mode.
    #[serde(default)]
    pub picker: ModelPicker,
    /// The picker mode can be changed (custom sites).
    #[serde(default)]
    pub picker_available: bool,
    /// Where the speed tier applies.
    #[serde(default)]
    pub speed: SpeedSupport,
    /// The catalog file.
    #[serde(default)]
    pub catalog_file: CatalogFileStatus,
    /// Notes (frozen list, fingerprint warning, stale consumer).
    #[serde(default)]
    pub notes: Vec<Notice>,
    /// Per-model context windows and compaction thresholds reach Codex for this
    /// profile (through a catalog file). `false` with a personal access token
    /// (decision 4: Codex reads the provider's live list, only the all-models
    /// values apply) and for the official login.
    pub per_model: bool,
    /// What Codex reads now, when this profile is the applied one (to tell
    /// saved values from applied ones); `None` for other profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<AppliedModels>,
}

/// What Codex reads now for the applied profile's models: the all-models keys
/// of `config.toml` and the entries of the catalog file it names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedModels {
    /// `model_context_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// `model_auto_compact_token_limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<u64>,
    /// `model_catalog_json` (`None` = Codex uses its own or the provider's list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<PathBuf>,
    /// The catalog file's entries by slug (empty without a file).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, AppliedModel>,
}

/// One entry of the catalog file Codex reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedModel {
    /// `context_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// `max_context_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    /// `auto_compact_token_limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<u64>,
}

impl AppliedModel {
    /// The window, maximum and compaction of a catalog entry (`models[]` of a
    /// `model_catalog_json` file); `None` without a slug.
    pub fn from_entry(entry: &serde_json::Value) -> Option<(String, AppliedModel)> {
        let o = entry.as_object()?;
        let slug = o.get("slug")?.as_str()?.trim();
        if slug.is_empty() {
            return None;
        }
        let n = |k: &str| o.get(k).and_then(serde_json::Value::as_u64);
        Some((
            slug.to_owned(),
            AppliedModel {
                context_window: n("context_window"),
                max_context_window: n("max_context_window"),
                auto_compact: n("auto_compact_token_limit"),
            },
        ))
    }
}

impl AppliedModels {
    /// The window and compaction Codex uses now for `spec` (its catalog entry
    /// when a file is in use, else the listed values; the all-models keys win,
    /// the window capped at the model's maximum, as Codex does).
    pub fn limits(&self, spec: &ModelSpec) -> ModelLimits {
        let base = match (&self.catalog, self.models.get(&spec.slug)) {
            (Some(_), Some(e)) => ModelSpec {
                context_window: e.context_window,
                max_context_window: e.max_context_window,
                auto_compact_token_limit: e.auto_compact,
                effective_context_window_percent: spec.effective_context_window_percent,
                ..ModelSpec::new(spec.slug.clone())
            },
            _ => ModelSpec {
                raw: None,
                ..spec.clone()
            },
        };
        let globals = ModelDefaults {
            context_window: self.context_window,
            auto_compact: self.auto_compact,
            ..ModelDefaults::default()
        };
        model_limits(&base, None, &globals)
    }
}

/// An edit of a profile's models ([`crate::SwitchService::save_models`]). Takes
/// effect at the next switch to the profile (the caller plans one when it is
/// applied).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsEdit {
    /// Profile (`None` = the applied profile, else the last selected one).
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// New model defaults, replacing the whole [`ModelDefaults`] (`None` =
    /// unchanged).
    #[serde(default)]
    pub defaults: Option<ModelDefaults>,
    /// Overrides to store, replacing the slug's previous override (an empty
    /// override removes it).
    #[serde(default)]
    pub set: BTreeMap<String, ModelOverride>,
    /// Slugs whose override is removed.
    #[serde(default)]
    pub reset: Vec<String>,
    /// Remove every override first.
    #[serde(default)]
    pub reset_all: bool,
    /// Picker mode (`None` = unchanged).
    #[serde(default)]
    pub picker: Option<ModelPicker>,
}

/// Result of [`crate::SwitchService::save_models`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsEditReport {
    /// The saved profile.
    pub profile: Profile,
    /// Notes (clamped compaction, catalog file needed from now on).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

// ---------------------------------------------------------------------------
// Token count input (`400k`, `1m`, `80%`)
// ---------------------------------------------------------------------------

/// Parse a token count typed by the user: `400000`, `400k`, `1m`, `1.5m`,
/// `272K` (decimal: k = 1,000, m = 1,000,000; `_` and `,` separators allowed).
pub fn parse_token_count(text: &str) -> Option<u64> {
    let t: String = text
        .trim()
        .chars()
        .filter(|c| !matches!(c, '_' | ',' | ' '))
        .collect();
    let lower = t.to_ascii_lowercase();
    let (num, scale) = if let Some(n) = lower.strip_suffix('k') {
        (n, 1_000u64)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 1_000_000u64)
    } else {
        (lower.as_str(), 1u64)
    };
    if num.is_empty() {
        return None;
    }
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || !digits(frac) || (int.is_empty() && frac.is_empty()) {
        return None;
    }
    if scale == 1 && !frac.is_empty() {
        return None;
    }
    let whole: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut value = whole.checked_mul(scale)?;
    if !frac.is_empty() {
        let places = u32::try_from(frac.len()).ok()?;
        let denom = 10u64.checked_pow(places)?;
        let f: u64 = frac.parse().ok()?;
        let part = f.checked_mul(scale)?;
        if part % denom != 0 {
            return None;
        }
        value = value.checked_add(part / denom)?;
    }
    Some(value)
}

/// A compaction threshold as typed: absolute tokens or a share of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CompactLimit {
    /// Tokens.
    Tokens(u64),
    /// Percent of the context window (1..=100).
    Percent(u8),
}

impl CompactLimit {
    /// Parse `80%`, `300k`, `250000`.
    pub fn parse(text: &str) -> Option<CompactLimit> {
        let t = text.trim();
        if let Some(p) = t.strip_suffix('%') {
            let p: u8 = p.trim().parse().ok()?;
            return (1..=100).contains(&p).then_some(CompactLimit::Percent(p));
        }
        parse_token_count(t).map(CompactLimit::Tokens)
    }

    /// Tokens for a model with `window` tokens (percent rounds down).
    pub fn tokens(self, window: u64) -> u64 {
        match self {
            CompactLimit::Tokens(n) => n,
            CompactLimit::Percent(p) => window.saturating_mul(u64::from(p)) / 100,
        }
    }
}

/// The threshold Codex actually uses: `min(limit, 90 % of window)`, or 90 % of the
/// window when no limit is set.
pub fn effective_auto_compact(window: u64, limit: Option<u64>) -> u64 {
    let cap = window.saturating_mul(u64::from(AUTO_COMPACT_MAX_PERCENT)) / 100;
    limit.map_or(cap, |l| l.min(cap))
}

// ---------------------------------------------------------------------------
// Effective window and compaction, validation (Models page, `spb models`, save)
// ---------------------------------------------------------------------------

/// Where an effective window or compaction threshold comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    /// The model list's own value (Codex's default for the model).
    #[default]
    Model,
    /// The profile's all-models value (`model_context_window` /
    /// `model_auto_compact_token_limit`).
    AllModels,
    /// The model's own override.
    Custom,
}

/// The context window and compaction threshold Codex uses for one model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLimits {
    /// Context window (`None` = unknown).
    pub window: Option<u64>,
    /// Where the window comes from.
    pub window_source: LimitSource,
    /// The model's maximum window, when known.
    pub max: Option<u64>,
    /// The configured compaction threshold before Codex's 90 % clamp (`None` =
    /// none set: 90 % of the window).
    pub compact_limit: Option<u64>,
    /// The threshold Codex uses (after the clamp); `None` when the window is
    /// unknown and no threshold is set.
    pub compact: Option<u64>,
    /// Where the compaction threshold comes from.
    pub compact_source: LimitSource,
    /// The configured threshold is above 90 % of the window and was clamped.
    pub clamped: bool,
    /// Share of the window Codex lets a turn use (`effective_context_window_percent`).
    pub usable_percent: u8,
}

impl ModelLimits {
    /// Tokens of the window a turn can use (`window` x `usable_percent`).
    pub fn usable_window(&self) -> Option<u64> {
        self.window
            .map(|w| w.saturating_mul(u64::from(self.usable_percent)) / 100)
    }

    /// The compaction threshold as a share of the window (whole percent).
    pub fn compact_percent(&self) -> Option<u64> {
        match (self.compact, self.window) {
            (Some(c), Some(w)) if w > 0 => Some((c.saturating_mul(100) + w / 2) / w),
            _ => None,
        }
    }

    /// The window and the threshold Codex uses (what "applied" compares).
    pub fn pair(&self) -> (Option<u64>, Option<u64>) {
        (self.window, self.compact)
    }
}

/// The context window and compaction threshold Codex uses for `spec` with the
/// model's override `o` and the profile's all-models values in `defaults`
/// (UX §3.4, spike A2/A5): the override wins, else the all-models value (the
/// window capped at the model's maximum, as Codex caps `model_context_window`
/// and as Superba folds it into a catalog file), else the listed value; the
/// threshold is clamped to 90 % of the window.
pub fn model_limits(
    spec: &ModelSpec,
    o: Option<&ModelOverride>,
    defaults: &ModelDefaults,
) -> ModelLimits {
    let max = spec.max_window();
    let (window, window_source) = match (o.and_then(|o| o.context_window), defaults.context_window)
    {
        (Some(w), _) => (Some(w), LimitSource::Custom),
        (None, Some(all)) => (
            Some(max.map_or(all, |m| all.min(m))),
            LimitSource::AllModels,
        ),
        (None, None) => (spec.resolved_context_window(), LimitSource::Model),
    };
    let (compact_limit, compact_source) =
        match (o.and_then(|o| o.auto_compact), defaults.auto_compact) {
            (Some(c), _) => (Some(c), LimitSource::Custom),
            (None, Some(all)) => (Some(all), LimitSource::AllModels),
            (None, None) => (spec.auto_compact_token_limit, LimitSource::Model),
        };
    let compact = match window {
        Some(w) => Some(effective_auto_compact(w, compact_limit)),
        None => compact_limit,
    };
    ModelLimits {
        window,
        window_source,
        max,
        compact_limit,
        compact,
        compact_source,
        clamped: matches!((compact_limit, compact), (Some(l), Some(c)) if c < l),
        usable_percent: spec
            .effective_context_window_percent
            .filter(|p| (1..=100).contains(p))
            .unwrap_or(DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT),
    }
}

/// Which value a [`LimitError`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitField {
    /// Context window.
    Window,
    /// Compaction threshold.
    Compact,
}

/// Why a context window or compaction threshold cannot be saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LimitError {
    /// A window below [`MIN_CONTEXT_WINDOW`].
    WindowTooSmall {
        /// The minimum.
        min: u64,
    },
    /// A window above the model's maximum (or [`MAX_CONTEXT_WINDOW`]).
    WindowOverMax {
        /// The maximum.
        max: u64,
    },
    /// A threshold below [`MIN_AUTO_COMPACT`].
    CompactTooSmall {
        /// The minimum.
        min: u64,
    },
    /// A threshold not below the context window it applies to.
    CompactNotBelowWindow {
        /// The window.
        window: u64,
    },
}

impl LimitError {
    /// The value the error is about.
    pub fn field(&self) -> LimitField {
        match self {
            LimitError::WindowTooSmall { .. } | LimitError::WindowOverMax { .. } => {
                LimitField::Window
            }
            _ => LimitField::Compact,
        }
    }

    /// English text (service errors; the UIs translate the variants).
    pub fn message(&self) -> String {
        match self {
            LimitError::WindowTooSmall { min } => {
                format!("the context window must be at least {min} tokens")
            }
            LimitError::WindowOverMax { max } => {
                format!("the context window must be at most {max} tokens")
            }
            LimitError::CompactTooSmall { min } => {
                format!("the auto-compact threshold must be at least {min} tokens")
            }
            LimitError::CompactNotBelowWindow { window } => format!(
                "the auto-compact threshold must be below the context window ({window} tokens)"
            ),
        }
    }
}

/// Check a context window on its own: at least [`MIN_CONTEXT_WINDOW`], at most
/// `max` (the model's maximum; [`MAX_CONTEXT_WINDOW`] when unknown or for all
/// models, whose value Codex caps per model).
pub fn check_window(value: u64, max: Option<u64>) -> std::result::Result<(), LimitError> {
    if value < MIN_CONTEXT_WINDOW {
        return Err(LimitError::WindowTooSmall {
            min: MIN_CONTEXT_WINDOW,
        });
    }
    let max = max.unwrap_or(MAX_CONTEXT_WINDOW).min(MAX_CONTEXT_WINDOW);
    if value > max {
        return Err(LimitError::WindowOverMax { max });
    }
    Ok(())
}

/// Check a compaction threshold: at least [`MIN_AUTO_COMPACT`] and below the
/// window it applies to (when known). Thresholds between 90 % and 100 % of the
/// window are valid; Codex clamps them ([`ModelLimits::clamped`]).
pub fn check_compact(value: u64, window: Option<u64>) -> std::result::Result<(), LimitError> {
    if value < MIN_AUTO_COMPACT {
        return Err(LimitError::CompactTooSmall {
            min: MIN_AUTO_COMPACT,
        });
    }
    if let Some(window) = window
        && value >= window
    {
        return Err(LimitError::CompactNotBelowWindow { window });
    }
    Ok(())
}

/// Check the all-models values: each on its own, and the threshold below the
/// all-models window when both are set.
pub fn check_all_models(defaults: &ModelDefaults) -> std::result::Result<(), LimitError> {
    if let Some(w) = defaults.context_window {
        check_window(w, None)?;
    }
    if let Some(c) = defaults.auto_compact {
        check_compact(c, defaults.context_window)?;
    }
    Ok(())
}

/// Check one model's override with the profile's all-models values: its own
/// window within the model's bounds, its own threshold at least the minimum and
/// below the window the model ends up with ([`model_limits`]). `spec = None`
/// (a model the list does not have) checks the plain bounds only.
pub fn check_model_override(
    spec: Option<&ModelSpec>,
    o: &ModelOverride,
    defaults: &ModelDefaults,
) -> std::result::Result<(), LimitError> {
    if let Some(w) = o.context_window {
        check_window(w, spec.and_then(ModelSpec::max_window))?;
    }
    if let Some(c) = o.auto_compact {
        let window = match spec {
            Some(spec) => model_limits(spec, Some(o), defaults).window,
            None => o.context_window.or(defaults.context_window),
        };
        check_compact(c, window)?;
    }
    Ok(())
}
