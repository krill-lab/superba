//! Request statistics: the request log, the optional reply stats line and cost
//! estimates (spike "router-stats", plan v0.3 B1).
//!
//! Everything here runs only while [`RouterConfig::stats`](crate::RouterConfig)
//! is present. Without it the router behaves exactly like 0.2: request bodies
//! and responses pass byte for byte and WebSockets are joined byte for byte.
//!
//! With it:
//!
//! - **Request side** ([`meta`], [`strip`]): one borrowed peek of each Responses
//!   request (originator, turn metadata, model, `service_tier`,
//!   `prompt_cache_key`, structured output), and the router's own stats line is
//!   removed from assistant `output_text` in the outgoing input — always, also
//!   with the line turned off, so old lines never reach an upstream.
//! - **Response side** ([`sse`], [`tap`], [`inject`]): an SSE scanner that
//!   forwards upstream chunks untouched (zero copy) while timing events and
//!   reading `usage`; when the line is on and the request qualifies, the end of
//!   the final answer is held from its `output_text.done` until
//!   `response.completed` and the line is appended (one synthetic delta plus the
//!   patched done / part / item / completed events).
//! - **Recording** (`hub`, `store`): one row per request in
//!   `<data dir>/router/usage.sqlite3`, written by a dedicated thread through a
//!   bounded channel that never blocks the proxy. Numbers only: request and
//!   response headers and bodies are never stored.
//! - **Cost** ([`price`], [`usage`]): micro-dollar estimates from the price table
//!   Superba passes; unknown price = no cost.
//!
//! Nothing here logs keys, tokens, thread ids, prompts or bodies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use superba_core::{
    Catalog, Error, Locale, Multiplier, PriceEntry, PriceTable, RequestHarness, Result,
    RouterStatsSettings, StatsLineStyle,
};

use crate::members::router_id;

pub(crate) mod body;
pub(crate) mod hub;
pub mod inject;
pub mod meta;
pub mod price;
pub mod sse;
pub(crate) mod store;
pub mod strip;
pub mod tap;
pub mod usage;

/// Request recording, the stats line and prices (`config.json` → `stats`).
///
/// Written by Superba through
/// [`RouterManager::configure`](crate::manager::RouterManager::configure);
/// applied with a reload, never a Codex restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsConfig {
    /// Record one row per request (numbers only).
    pub record: bool,
    /// Keep rows this many days.
    pub usage_days: u32,
    /// Keep at most this many rows.
    pub max_rows: u32,
    /// The reply stats line; `None` = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<StatsLineConfig>,
    /// Prices of the cost estimates.
    pub prices: PriceConfig,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            record: true,
            usage_days: RouterStatsSettings::DEFAULT_USAGE_DAYS,
            max_rows: RouterStatsSettings::DEFAULT_MAX_ROWS,
            line: None,
            prices: PriceConfig::default(),
        }
    }
}

/// The stats line as the router renders it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsLineConfig {
    /// Template with `{tps}`, `{ttft}`, `{input}`, `{output}`, `{cached}`,
    /// `{cost}`, `{requests}` (already in the user's language; a leading `⚡` is
    /// replaced by the router's marker).
    pub template: String,
    /// Quote style.
    #[serde(default)]
    pub style: StatsLineStyle,
    /// `originator` header values that get the line (`codex_exec` never does,
    /// even when listed).
    pub originators: Vec<String>,
    /// Harnesses that get the line (Codex also by `originators`; Claude Code
    /// on the final answer of its main conversation).
    #[serde(default = "default_line_harnesses")]
    pub harnesses: Vec<RequestHarness>,
}

/// Codex and Claude Code: the harnesses that get the line by default.
pub fn default_line_harnesses() -> Vec<RequestHarness> {
    vec![RequestHarness::Codex, RequestHarness::ClaudeCode]
}

/// Prices of the cost estimates: the table plus per-member adjustments (member
/// ids are router ids).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PriceConfig {
    /// List prices by model slug.
    pub table: PriceTable,
    /// Ratio of the list price a member charges; missing = 1.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ratio_by_member: BTreeMap<String, Multiplier>,
    /// Absolute prices by member, then model slug.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, BTreeMap<String, PriceEntry>>,
}

/// Longest accepted stats line template.
pub const MAX_TEMPLATE_LEN: usize = 400;

impl StatsConfig {
    /// The router configuration for Superba's persisted settings, the price
    /// table (from the feeds) and the UI language of the default line template.
    /// Profile ids become router member ids ([`router_id`]).
    pub fn from_settings(
        settings: &RouterStatsSettings,
        table: PriceTable,
        locale: Locale,
    ) -> StatsConfig {
        let line = settings.line.enabled.then(|| StatsLineConfig {
            template: settings.line.template_text(&Catalog::new(locale)),
            style: settings.line.style,
            originators: settings
                .line
                .clients
                .iter()
                .map(|c| c.originator().to_owned())
                .collect(),
            harnesses: settings.line.harnesses.clone(),
        });
        StatsConfig {
            record: settings.record,
            usage_days: settings.usage_days,
            max_rows: settings.max_rows,
            line,
            prices: PriceConfig {
                table,
                ratio_by_member: settings
                    .prices
                    .ratio_by_profile
                    .iter()
                    .map(|(p, r)| (router_id(p.as_str()), *r))
                    .collect(),
                overrides: settings
                    .prices
                    .overrides
                    .iter()
                    .map(|(p, m)| (router_id(p.as_str()), m.clone()))
                    .collect(),
            },
        }
    }

    /// Check the limits; the router refuses a configuration otherwise.
    pub fn validate(&self) -> Result<()> {
        if self.usage_days == 0 || self.max_rows == 0 {
            return Err(Error::invalid(
                "request log retention must be at least one day and one row",
            ));
        }
        if let Some(line) = &self.line {
            if line.template.trim().is_empty() {
                return Err(Error::invalid("the stats line template is empty"));
            }
            if line.template.chars().count() > MAX_TEMPLATE_LEN {
                return Err(Error::invalid(format!(
                    "the stats line template is longer than {MAX_TEMPLATE_LEN} characters"
                )));
            }
        }
        Ok(())
    }
}
