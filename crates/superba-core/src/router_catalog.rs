//! Shared settings and evidence for the router's merged Codex catalog.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// User choices for the router's model names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterCatalogSettings {
    /// Offer provider-prefixed names in Codex's picker.
    pub show_prefixed: bool,
    /// User-selected provider slugs, keyed by profile id.
    pub provider_slugs: BTreeMap<String, String>,
    /// Explicit picker visibility; true can also reveal an upstream-hidden model.
    pub visibility: BTreeMap<String, bool>,
    /// Overrides for the merged list, independent of which profile is selected.
    pub overrides: BTreeMap<String, crate::ModelOverride>,
}

impl Default for RouterCatalogSettings {
    fn default() -> Self {
        Self {
            show_prefixed: true,
            provider_slugs: BTreeMap::new(),
            visibility: BTreeMap::new(),
            overrides: BTreeMap::new(),
        }
    }
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
    /// A Codex entry constructed from a standard id.
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
    /// The member cannot serve native Responses.
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
