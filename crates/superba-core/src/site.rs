//! Sites (API providers) and their lines (https entry origins).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::auth::AuthMode;
use crate::i18n::LocalizedText;
use crate::ids::{LineId, SiteId};
use crate::origin::{CodexPath, Origin};

/// The kind of a site, which selects the config shape written for it (plan 2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SiteKind {
    /// Krill relay (primary support): PAT and API-key auth, built-in lines.
    Krill,
    /// The official ChatGPT login (owned keys removed, stashed login restored).
    Official,
    /// Any OpenAI-compatible site serving the Responses API (API-key auth).
    Compatible,
}

/// An API provider described by data, not code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    /// Stable id.
    pub id: SiteId,
    /// Kind.
    pub kind: SiteKind,
    /// Display name.
    pub name: LocalizedText,
    /// Main web site (account, billing). Krill: <https://www.krill-code.com>.
    #[serde(default)]
    pub homepage: Option<Url>,
    /// Entry lines. Empty for the official site.
    #[serde(default)]
    pub lines: Vec<Line>,
    /// What the site supports.
    pub capabilities: SiteCapabilities,
    /// Relative endpoint paths used by probes and key checks.
    #[serde(default)]
    pub endpoints: SiteEndpoints,
    /// Line used when nothing better is known (no probe yet).
    #[serde(default)]
    pub default_line: Option<LineId>,
    /// Model names this site serves under another name: Codex's model name ->
    /// the site's model name. Applied to the `model` key in direct mode and to
    /// request bodies by the local router (custom sites; empty for built-ins).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_map: BTreeMap<String, String>,
}

impl Site {
    /// Look up a line by id.
    pub fn line(&self, id: &LineId) -> Option<&Line> {
        self.lines.iter().find(|l| &l.id == id)
    }

    /// The default line, else the first line.
    pub fn default_line(&self) -> Option<&Line> {
        self.default_line
            .as_ref()
            .and_then(|id| self.line(id))
            .or_else(|| self.lines.first())
    }

    /// True if `mode` is offered by this site.
    pub fn supports(&self, mode: AuthMode) -> bool {
        self.capabilities.auth_modes.contains(&mode)
    }

    /// The site's name for Codex's model `model` ([`Site::model_map`]; unmapped
    /// names are used as they are).
    pub fn site_model<'a>(&'a self, model: &'a str) -> &'a str {
        self.model_map.get(model).map_or(model, String::as_str)
    }
}

/// One https entry origin of a site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Stable id, unique within the site.
    pub id: LineId,
    /// Display name (i18n key + English fallback).
    pub name: LocalizedText,
    /// `https://host[:port]`.
    pub origin: Origin,
    /// Where the line definition came from.
    #[serde(default)]
    pub source: LineSource,
}

/// Provenance of a [`Line`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineSource {
    /// Compiled into Superba.
    #[default]
    Builtin,
    /// Added by the user (custom compatible site).
    User,
    /// Injected by tests / e2e (loopback forwarders).
    Test,
}

/// Feature flags of a site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteCapabilities {
    /// Auth modes offered, in preference order (first = default).
    pub auth_modes: Vec<AuthMode>,
    /// Codex paths to try, in preference order.
    pub codex_paths: Vec<CodexPath>,
    /// Unauthenticated `GET <models_unauth>` answers quickly (401 + CORS) and can be
    /// used for latency probes.
    #[serde(default)]
    pub latency_probe: bool,
    /// whoami + authenticated models can validate a key per line.
    #[serde(default)]
    pub key_check: bool,
    /// Query the standard billing endpoints for the account card (limit, used,
    /// remaining, plan, expiry). They are optional and feature-detected, so this is
    /// on for every key-based site.
    #[serde(default)]
    pub account_summary: bool,
    /// Query the OpenAI organization usage and costs APIs for the usage view
    /// (Krill; on demand only because the site rate-limits them).
    #[serde(default)]
    pub usage_api: bool,
    /// The site serves a model catalog (`/models`).
    #[serde(default)]
    pub model_catalog: bool,
    /// The site maps tier-suffix model names (`<slug>-fast`, `<slug>-ultrafast`)
    /// to service tiers, so the desktop app in API-key mode can use Fast /
    /// Ultrafast through the model name. Off unless confirmed for the site.
    #[serde(default)]
    pub tier_model_suffix: bool,
}

/// Relative endpoint paths (joined onto a line origin).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteEndpoints {
    /// Unauthenticated latency probe, expected 401: `/v1/models`.
    pub models_unauth: String,
    /// PAT whoami called by Codex at start-up: `/v1/user-auth-credential/whoami`.
    pub whoami: String,
    /// PAT account check called by Codex: `/backend-api/wham/accounts/check`.
    pub accounts_check: String,
    /// Authenticated model list on the backend-api path: `/backend-api/codex/models`.
    pub backend_codex_models: String,
    /// Authenticated model list on the fallback path: `/codex/v1/models`.
    pub codex_v1_models: String,
    /// Legacy OpenAI billing subscription (limit, plan, expiry):
    /// `/v1/dashboard/billing/subscription`; the path without a leading `/v1` is
    /// tried when this one is missing.
    #[serde(default = "default_billing_subscription")]
    pub billing_subscription: String,
    /// Legacy OpenAI billing usage (`total_usage` in cents for a date range):
    /// `/v1/dashboard/billing/usage`; same `/v1` fallback.
    #[serde(default = "default_billing_usage")]
    pub billing_usage: String,
    /// OpenAI Usage API, completions: `/v1/organization/usage/completions`.
    #[serde(default = "default_usage_completions")]
    pub usage_completions: String,
    /// OpenAI Costs API: `/v1/organization/costs`.
    #[serde(default = "default_costs")]
    pub costs: String,
}

fn default_billing_subscription() -> String {
    "/v1/dashboard/billing/subscription".into()
}

fn default_billing_usage() -> String {
    "/v1/dashboard/billing/usage".into()
}

fn default_usage_completions() -> String {
    "/v1/organization/usage/completions".into()
}

fn default_costs() -> String {
    "/v1/organization/costs".into()
}

impl Default for SiteEndpoints {
    fn default() -> Self {
        Self {
            models_unauth: "/v1/models".into(),
            whoami: "/v1/user-auth-credential/whoami".into(),
            accounts_check: "/backend-api/wham/accounts/check".into(),
            backend_codex_models: "/backend-api/codex/models".into(),
            codex_v1_models: "/codex/v1/models".into(),
            billing_subscription: default_billing_subscription(),
            billing_usage: default_billing_usage(),
            usage_completions: default_usage_completions(),
            costs: default_costs(),
        }
    }
}
