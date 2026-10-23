//! Shared settings and evidence for the router's merged Codex catalog.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which models the router publishes to Codex (its "router model list").
///
/// The mode only decides what Codex's picker is offered. Routing does not
/// depend on it: a model or pinned alias that a session already uses keeps
/// working after the mode changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogMode {
    /// Exactly the installed Codex's own model identities (L0); provider
    /// catalogs (L1) only refresh their metadata. The default for everyone.
    #[default]
    Codex,
    /// Codex's models plus the providers' other models. A model that is not
    /// known to Codex or to a provider's Codex catalog is published only after
    /// its two-turn tool-call test passed.
    Extended,
    /// [`CatalogMode::Extended`] plus one alias per provider that pins the
    /// model to that provider (`krill/gpt-5.5`, shown as "GPT-5.5 · Krill").
    Prefixed,
}

impl CatalogMode {
    /// Every mode, in the order the pickers list them.
    pub const ALL: [Self; 3] = [Self::Codex, Self::Extended, Self::Prefixed];

    /// The stable spelling used by `spb models mode` and the settings file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Extended => "extended",
            Self::Prefixed => "prefixed",
        }
    }

    /// Parse [`CatalogMode::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == value.trim())
    }

    /// Models outside Codex's own list are published.
    pub fn extends(self) -> bool {
        self != Self::Codex
    }

    /// Pinned provider aliases are published.
    pub fn prefixes(self) -> bool {
        self == Self::Prefixed
    }
}

/// User choices for the router's model names.
///
/// Settings written by alpha.7 carry a `show_prefixed` flag; it is ignored on
/// purpose (no migration): every configuration starts in [`CatalogMode::Codex`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterCatalogSettings {
    /// What the router publishes to Codex.
    pub mode: CatalogMode,
    /// User-selected provider slugs (the alias namespace), keyed by member id.
    pub provider_slugs: BTreeMap<String, String>,
    /// Explicit picker visibility; true can also reveal an upstream-hidden model.
    pub visibility: BTreeMap<String, bool>,
    /// Overrides of the canonical models, keyed by model id; they apply to the
    /// bare id and to every provider alias of it.
    pub overrides: BTreeMap<String, crate::ModelOverride>,
}

/// Origin of a model's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogLayer {
    /// The bundled fallback.
    Builtin,
    /// A complete upstream Codex entry.
    Codex,
    /// An id in the key's standard model list.
    Standard,
    /// A standard-list id that exactly matches a known Codex model (the
    /// installed Codex or a provider's Codex catalog): its complete metadata is
    /// reused. Whether this provider can serve it is checked separately.
    StandardKnown,
    /// A Codex entry constructed from a standard id (family defaults).
    Synthesized,
}

/// Whether a model can be offered to Codex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogReadiness {
    /// An upstream Codex entry or a successful tool round trip.
    Ready,
    /// No successful two-turn tool probe yet.
    Pending,
    /// The latest tool probe failed.
    Failed,
    /// No native Responses endpoint or enabled conversion route.
    NeedsTranslation,
}

/// Evidence scoped to one member's credentials and endpoint; no secret values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogProvenance {
    /// Stable member/profile id.
    pub provider: String,
    /// Display name.
    pub provider_name: String,
    /// Unique provider namespace.
    pub provider_slug: String,
    /// Opaque credential fingerprint, never the key itself.
    pub key_scope: String,
    /// Unmodified upstream model id.
    pub upstream_id: String,
    /// Endpoint which supplied this evidence.
    pub endpoint: String,
    /// Protocol used for inference.
    pub protocol: String,
    /// Source layer.
    pub layer: CatalogLayer,
    /// What established the capability.
    pub evidence: String,
    /// Observation time, Unix milliseconds.
    pub observed_at: u64,
    /// This provider's own readiness for the model (the row's readiness is
    /// the best of its providers). `None` from routers before alpha.8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<CatalogReadiness>,
    /// The alias that pins the model to this provider (`krill/gpt-5.5`),
    /// whether or not the current mode publishes it; `None` when it would
    /// collide with a real upstream id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl CatalogProvenance {
    /// Inference reaches a non-Responses endpoint through the router codecs.
    pub fn translated(&self) -> bool {
        matches!(
            self.protocol.as_str(),
            "chat_completions" | "anthropic_messages" | "gemini"
        )
    }
}

/// One row in the Models page, including models withheld from Codex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterCatalogModel {
    /// Codex ModelInfo (unknown upstream fields retained).
    pub entry: serde_json::Value,
    /// All members that supplied this model.
    pub sources: Vec<CatalogProvenance>,
    /// Whether the entry is published.
    pub readiness: CatalogReadiness,
    /// Safe diagnostic text, if the probe failed.
    pub detail: Option<String>,
}

/// Independent status of an upstream model list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSourceStatus {
    /// Stable member/profile id.
    pub provider: String,
    /// Codex or standard list.
    pub layer: CatalogLayer,
    /// Endpoint requested.
    pub endpoint: String,
    /// Time of the last successful observation.
    pub observed_at: Option<u64>,
    /// A transient failure is using previously observed data.
    pub stale: bool,
    /// Failure, including authentication errors; never treated as an empty list.
    pub error: Option<String>,
}

/// Router-owned model list and its source diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterCatalogView {
    /// Current user choices.
    pub settings: RouterCatalogSettings,
    /// Models including pending or failed probes.
    pub models: Vec<RouterCatalogModel>,
    /// One status for each member and list type.
    pub sources: Vec<CatalogSourceStatus>,
    /// Published provider aliases and the canonical model each pins; never
    /// rows of their own.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    /// Provider models the current mode keeps out of Codex's list and out of
    /// [`Self::models`] (Codex mode: everything outside Codex's own list), so
    /// the Models page can say how to get them (user bug 106).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withheld: Vec<String>,
}

impl RouterCatalogView {
    /// The row of a canonical model id.
    pub fn model(&self, id: &str) -> Option<&RouterCatalogModel> {
        self.models.iter().find(|m| m.id() == id)
    }
}

impl RouterCatalogModel {
    /// The canonical model id (the entry's slug).
    pub fn id(&self) -> &str {
        self.entry["slug"].as_str().unwrap_or_default()
    }

    /// Codex is offered this model (the bare id).
    pub fn published(&self) -> bool {
        self.readiness == CatalogReadiness::Ready
    }

    /// The window is a family estimate: no provider supplied real metadata.
    pub fn estimated(&self) -> bool {
        !self.sources.is_empty()
            && self
                .sources
                .iter()
                .all(|s| s.layer == CatalogLayer::Synthesized)
    }
}

/// Result of testing a selected model through the local router.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterModelTest {
    /// Requested model, including its namespace when selected.
    pub model: String,
    /// Whether the test completed successfully.
    pub passed: bool,
    /// Safe result summary.
    pub detail: String,
    /// Unix milliseconds.
    pub tested_at: u64,
}
