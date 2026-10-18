//! Line probing and key validation results ([`crate::LineProber`]).

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
use crate::ids::{LineId, SiteId};
use crate::origin::CodexPath;
use crate::site::Site;
use crate::time::Timestamp;

/// Endpoints probed on a line (all joined onto the line origin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProbeEndpoint {
    /// Unauthenticated `GET /v1/models` (latency; expect 401 with CORS).
    ModelsUnauth,
    /// `GET /v1/user-auth-credential/whoami` (PAT).
    Whoami,
    /// `GET /backend-api/wham/accounts/check` (PAT).
    WhamAccountsCheck,
    /// `GET /backend-api/codex/models`.
    BackendCodexModels,
    /// `GET /codex/v1/models`.
    CodexV1Models,
    /// Account data (billing / usage endpoints).
    AccountSummary,
}

/// Result of one HTTP probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointCheck {
    /// Which endpoint.
    pub endpoint: ProbeEndpoint,
    /// HTTP status, `None` on transport failure.
    pub status: Option<u16>,
    /// Round-trip time in ms (after warm-up), if measured.
    pub latency_ms: Option<u32>,
    /// The answer looks like the relay itself (e.g. a JSON 401), not an edge.
    pub ok: bool,
    /// Krill edge interception detected (`l-block` header and/or empty 200).
    pub edge_blocked: bool,
    /// Short diagnostic (never contains secrets).
    #[serde(default)]
    pub detail: Option<String>,
}

/// Capability report of one line, optionally for one key (plan 3.2 `LineCaps`).
/// Cached per (line, key) with `checked_at`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineCaps {
    /// Site.
    pub site_id: SiteId,
    /// Line.
    pub line_id: LineId,
    /// When probed.
    pub checked_at: Timestamp,
    /// The line answered at all.
    pub reachable: bool,
    /// Median latency of the unauthenticated probe.
    pub latency_ms: Option<u32>,
    /// PAT endpoints (whoami + wham/accounts/check) reach the relay (not edge-blocked).
    pub pat_ok: bool,
    /// Some Codex path is usable with an API key.
    pub apikey_ok: bool,
    /// First usable Codex path; `None` = no Responses path works on this line.
    pub codex_path: Option<CodexPath>,
    /// Edge interception was detected on some endpoint.
    pub blocked_by_edge: bool,
    /// Key access result, when a key was checked on this line.
    #[serde(default)]
    pub key_access: Option<KeyAccess>,
    /// Individual probe results.
    #[serde(default)]
    pub checks: Vec<EndpointCheck>,
    /// One-line human summary ("PAT unavailable on this line: edge blocks wham").
    #[serde(default)]
    pub detail: Option<LocalizedText>,
}

impl LineCaps {
    /// Latency class of the line (unreachable lines are [`LatencyGrade::Unknown`]).
    pub fn latency_grade(&self) -> LatencyGrade {
        if self.reachable {
            LatencyGrade::from_ms(self.latency_ms)
        } else {
            LatencyGrade::Unknown
        }
    }

    /// True if `mode` can be used on this line according to the probe.
    pub fn allows(&self, mode: crate::auth::AuthMode) -> bool {
        if !mode.uses_key() || !self.reachable || self.codex_path.is_none() {
            return false;
        }
        if mode.needs_authapi_env() || mode.writes_chatgpt_base_url() {
            // PAT needs the backend-api path; `/codex/v1` is an API-key-only fallback.
            self.pat_ok && self.codex_path == Some(CodexPath::BackendApiCodex)
        } else {
            self.apikey_ok
        }
    }
}

/// Latency class of a line for display and sorting (plan 4 #4). Ordered best
/// first, so sorting lines by grade puts the fastest first and unknown last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyGrade {
    /// Below [`LatencyGrade::EXCELLENT_BELOW_MS`].
    Excellent,
    /// Below [`LatencyGrade::GOOD_BELOW_MS`].
    Good,
    /// Below [`LatencyGrade::FAIR_BELOW_MS`].
    Fair,
    /// [`LatencyGrade::FAIR_BELOW_MS`] or slower.
    Poor,
    /// Not measured or no answer.
    Unknown,
}

impl LatencyGrade {
    /// Upper bound (exclusive) of [`LatencyGrade::Excellent`] in ms.
    pub const EXCELLENT_BELOW_MS: u32 = 300;
    /// Upper bound (exclusive) of [`LatencyGrade::Good`] in ms.
    pub const GOOD_BELOW_MS: u32 = 500;
    /// Upper bound (exclusive) of [`LatencyGrade::Fair`] in ms.
    pub const FAIR_BELOW_MS: u32 = 800;

    /// Classify a measured latency (`None` = not measured).
    pub fn from_ms(ms: Option<u32>) -> Self {
        match ms {
            None => LatencyGrade::Unknown,
            Some(ms) if ms < Self::EXCELLENT_BELOW_MS => LatencyGrade::Excellent,
            Some(ms) if ms < Self::GOOD_BELOW_MS => LatencyGrade::Good,
            Some(ms) if ms < Self::FAIR_BELOW_MS => LatencyGrade::Fair,
            Some(_) => LatencyGrade::Poor,
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            LatencyGrade::Excellent => LocalizedText::new("latency.excellent", "Fast"),
            LatencyGrade::Good => LocalizedText::new("latency.good", "Good"),
            LatencyGrade::Fair => LocalizedText::new("latency.fair", "Fair"),
            LatencyGrade::Poor => LocalizedText::new("latency.poor", "Slow"),
            LatencyGrade::Unknown => LocalizedText::new("latency.unknown", "No answer"),
        }
    }
}

/// What the authenticated models request says about a key on a line (the relay
/// checks access point, quota/balance and channel authorization in that order).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyAccess {
    /// Usable on this line.
    Ok,
    /// Key unknown / revoked (401).
    InvalidKey,
    /// The key's plan does not include this line (access point rule).
    AccessPointDenied,
    /// Quota or balance exhausted.
    QuotaExhausted,
    /// Not authorized for the Codex channel.
    ChannelDenied,
    /// Edge intercepted the request.
    EdgeBlocked,
    /// Network failure.
    Unreachable,
    /// Anything else.
    Unknown {
        /// HTTP status, if any.
        status: Option<u16>,
        /// Short redacted message.
        message: String,
    },
}

impl KeyAccess {
    /// True for [`KeyAccess::Ok`].
    pub fn is_ok(&self) -> bool {
        matches!(self, KeyAccess::Ok)
    }

    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            KeyAccess::Ok => LocalizedText::new("key_access.ok", "Available"),
            KeyAccess::InvalidKey => LocalizedText::new("key_access.invalid_key", "Invalid key"),
            KeyAccess::AccessPointDenied => LocalizedText::new(
                "key_access.access_point_denied",
                "Your plan does not include this line",
            ),
            KeyAccess::QuotaExhausted => {
                LocalizedText::new("key_access.quota_exhausted", "Quota used up")
            }
            KeyAccess::ChannelDenied => LocalizedText::new(
                "key_access.channel_denied",
                "Codex channel not enabled for this key",
            ),
            KeyAccess::EdgeBlocked => {
                LocalizedText::new("key_access.edge_blocked", "Blocked by the line's edge")
            }
            KeyAccess::Unreachable => {
                LocalizedText::new("key_access.unreachable", "Line unreachable")
            }
            KeyAccess::Unknown { status, message } => LocalizedText::new(
                "key_access.unknown",
                "Unexpected answer ({status}): {message}",
            )
            .arg(
                "status",
                status.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            )
            .arg("message", message),
        }
    }
}

/// Health of the PAT auth API origin a running app-server would use (plan 7.1
/// rule 7: whoami with the current key against the app's `CODEX_AUTHAPI_BASE_URL`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthApiHealth {
    /// whoami returned 200 with all required fields.
    Healthy,
    /// Anything else (timeout, non-200, missing fields, edge interception).
    Unhealthy {
        /// Short redacted reason.
        reason: String,
    },
}

/// Identity returned by whoami. Note: Krill's whoami `plan` is always `pro` and must
/// not be displayed as the user's plan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    /// Email.
    #[serde(default)]
    pub email: Option<String>,
    /// User id.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Result of validating a key on one line (whoami + authenticated models).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCheck {
    /// Site.
    pub site_id: SiteId,
    /// Line.
    pub line_id: LineId,
    /// When.
    pub checked_at: Timestamp,
    /// Last four characters of the key that was checked.
    pub key_last4: String,
    /// whoami result (PAT path); `None` when not applicable.
    #[serde(default)]
    pub whoami: Option<AuthApiHealth>,
    /// Identity from whoami.
    #[serde(default)]
    pub identity: Option<AccountIdentity>,
    /// Authenticated models result (access point, quota, channel).
    pub access: KeyAccess,
    /// Codex path the check used.
    #[serde(default)]
    pub codex_path: Option<CodexPath>,
}

/// Usage window shown on the account card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Label ("5h", "weekly", "daily").
    pub label: String,
    /// Used percentage 0..=100.
    #[serde(default)]
    pub used_percent: Option<f32>,
    /// When the window resets.
    #[serde(default)]
    pub resets_at: Option<Timestamp>,
}

/// Account card data. Every field is optional. Identity comes from whoami (Krill);
/// plan, balance and expiry come from the standard billing endpoints
/// ([`crate::BillingSummary`]): `plan_name` = `plan.title`, `balance` = remaining
/// (`limit - used`, in USD), `expires_at` = `access_until`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountSummary {
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Email.
    #[serde(default)]
    pub email: Option<String>,
    /// Last four characters of the key.
    #[serde(default)]
    pub key_last4: Option<String>,
    /// Plan / package name (never from whoami's constant `pro`).
    #[serde(default)]
    pub plan_name: Option<String>,
    /// Balance as a decimal string (no float rounding).
    #[serde(default)]
    pub balance: Option<String>,
    /// Currency code of `balance`.
    #[serde(default)]
    pub currency: Option<String>,
    /// Plan expiry.
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    /// Usage windows.
    #[serde(default)]
    pub usage: Vec<UsageWindow>,
    /// The billing endpoints' answer (limit, used, remaining), when available.
    #[serde(default)]
    pub billing: Option<crate::usage::BillingSummary>,
    /// When the data was fetched from the site.
    #[serde(default)]
    pub fetched_at: Option<Timestamp>,
}

/// One model from a site's `/models` catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model slug.
    pub id: String,
    /// Display name, if provided.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Context window (Codex-native `context_window`, or OpenAI-list metadata
    /// such as `context_length` / `max_input_tokens`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Largest window an override may set (Codex-native entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    /// Service tiers the model offers (Codex-native entries).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_tiers: Vec<crate::models::ServiceTierSpec>,
    /// The Codex-native entry, unchanged (unknown fields preserved); `None` for
    /// OpenAI-style list items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

/// Result of a minimal authenticated Responses request (user-triggered full health
/// check, plan 4 #4; compatible-site validation, plan S10). Costs a few tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesProbe {
    /// A valid Responses answer was received.
    pub ok: bool,
    /// HTTP status, `None` on transport failure.
    pub status: Option<u16>,
    /// Time to the complete answer.
    pub latency_ms: Option<u32>,
    /// The legacy `/responses/compact` endpoint answered (when it was probed;
    /// Codex 0.155+ no longer calls it: remote compaction is a
    /// `compaction_trigger` on `/responses`, see [`crate::compaction`]).
    #[serde(default)]
    pub compact_ok: Option<bool>,
    /// Short redacted diagnostic.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Plan-time check that the configured model exists on the target (plan 4 #17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCheck {
    /// Model currently configured (`model` key), if any.
    pub current: Option<String>,
    /// True if it is in the target catalog (or no model is configured).
    pub available: bool,
    /// Suggested replacements, best first.
    #[serde(default)]
    pub suggestions: Vec<String>,
}

/// What [`crate::SwitchService::probe_site`] should check.
#[derive(Clone, Debug)]
pub struct SiteProbeRequest {
    /// Site.
    pub site_id: SiteId,
    /// Line (`None` = the site's default line).
    pub line_id: Option<LineId>,
    /// Key for the authenticated checks (`None` = latency and reachability only).
    pub key: Option<crate::plan::KeyInput>,
    /// Model for the minimal Responses request (`None` = the first model of the
    /// site's catalog). The site's [`crate::Site::model_map`] is applied.
    pub model: Option<String>,
    /// Send the minimal Responses request (costs a few tokens).
    pub responses: bool,
    /// Probe this site instead of the stored one with `site_id` (a custom site
    /// being added, tested before it is saved). Checked with the same rules as
    /// [`crate::SwitchService::save_site`]; never stored.
    pub site: Option<Site>,
}

impl SiteProbeRequest {
    /// Latency and reachability of `site_id`'s default line, without a key.
    pub fn new(site_id: SiteId) -> Self {
        Self {
            site_id,
            line_id: None,
            key: None,
            model: None,
            responses: false,
            site: None,
        }
    }

    /// Latency and reachability of an unsaved `site`'s default line, without a
    /// key.
    pub fn unsaved(site: Site) -> Self {
        Self {
            site_id: site.id.clone(),
            site: Some(site),
            ..Self::new(SiteId::new("unsaved"))
        }
    }
}

/// One entry of a site's model map, checked against its catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappedModel {
    /// Codex's model name.
    pub codex: String,
    /// The site's model name.
    pub site: String,
    /// The site's catalog lists it (`None` = no catalog).
    #[serde(default)]
    pub available: Option<bool>,
}

/// Result of [`crate::SwitchService::probe_site`]: latency and reachability,
/// key access, model catalog, model map and the minimal Responses request. Each
/// authenticated part is optional and reports its own failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SiteProbeReport {
    /// Site.
    pub site_id: SiteId,
    /// Line probed.
    pub line_id: LineId,
    /// Unauthenticated probe (latency, reachability).
    pub caps: LineCaps,
    /// Key check (access), when a key was given.
    #[serde(default)]
    pub key_check: Option<KeyCheck>,
    /// Model catalog, when a key was given and the site serves one.
    #[serde(default)]
    pub models: Option<Vec<ModelInfo>>,
    /// Why the catalog is missing.
    #[serde(default)]
    pub models_error: Option<crate::Error>,
    /// The site's model map, checked against the catalog.
    #[serde(default)]
    pub mapped: Vec<MappedModel>,
    /// Model used for the Responses request (site name).
    #[serde(default)]
    pub model: Option<String>,
    /// The minimal Responses request, when asked for.
    #[serde(default)]
    pub responses: Option<ResponsesProbe>,
    /// Why the Responses request was not sent or failed to start.
    #[serde(default)]
    pub responses_error: Option<crate::Error>,
    /// Identity and balance from the standard account endpoints, when a key
    /// was given and the site answers them (feature-detected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountSummary>,
    /// Why the account endpoints failed (a site without them is `None` here
    /// and in `account`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_error: Option<crate::Error>,
}
