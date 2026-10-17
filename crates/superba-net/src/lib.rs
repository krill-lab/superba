//! # superba-net
//!
//! Network probes for Superba. [`HttpLineProber`] implements
//! [`superba_core::LineProber`] with `reqwest` (rustls):
//!
//! - **latency** — a warm-up request, then timed unauthenticated
//!   `GET <line>/v1/models` (the relay answers 401 with CORS); the median is
//!   classified by [`superba_core::LatencyGrade`] (< 300 / < 500 / < 800 ms);
//!   all lines of a site are probed concurrently;
//! - **health** (relay sites) — unauthenticated whoami,
//!   `/backend-api/wham/accounts/check`, `/backend-api/codex/models` and
//!   `/codex/v1/models`, each expected to give the relay's JSON 401. An edge
//!   interception (`l-block` header, or a 2xx with an empty / non-JSON body)
//!   marks PAT (whoami, wham) or a Codex path unusable, and
//!   [`superba_core::LineCaps`] says why;
//! - **key check** — authenticated whoami (the four fields Codex requires),
//!   `wham/accounts/check` (PAT) and `GET /backend-api/codex/models?client_version=…`
//!   (or `/codex/v1/models`), whose error message tells access point, quota and
//!   channel denials apart ([`superba_core::KeyAccess`]);
//! - **auth API health** for plan 7.1 rule 7, **model catalogs**, a minimal
//!   **Responses probe** (+ compact), and **account data** (see
//!   [below](crate#account-data));
//! - **transports** ([`LineProber::probe_transports`]): the Responses
//!   WebSocket handshake (closed at once on 101) and a tiny invalid zstd body
//!   compared with the same body plain and with the compressed bytes sent as
//!   plain JSON, so neither costs a turn;
//! - **provider model lists** ([`LineProber::model_list`]):
//!   `/backend-api/codex/models?client_version=<x.y.z>` (Codex-native entries kept
//!   unchanged) on Codex backend paths, `<base>/models` with and without `/v1`
//!   (OpenAI list, context-window metadata parsed) on compatible sites; cached
//!   for [`MODEL_LIST_TTL`] with `ETag` / `304` reuse
//!   ([`HttpLineProber::with_model_cache_dir`] for a disk cache);
//! - the **site catalog** ([`SiteCatalog`], [`validate_site`],
//!   [`new_compatible_site`]): built-in sites, user-defined compatible sites and
//!   loopback e2e lines from [`ENV_SUPERBA_TEST_LINES`].
//!
//! ## Account data
//!
//! Only standard endpoints are used, all optional and feature-detected (404 / 405
//! or a network error hides that piece of data and never blocks switching):
//!
//! - **account card** ([`LineProber::account_summary`]): the email from whoami
//!   (Krill; its plan type is a constant and never shown) and limit / used /
//!   remaining / plan / expiry from the legacy OpenAI billing endpoints
//!   `/v1/dashboard/billing/subscription` and `/v1/dashboard/billing/usage` (each
//!   with a fallback without `/v1`) on any key-based site; cached in memory for a
//!   minute;
//! - **usage view** ([`LineProber::usage_report`], Krill): daily cost from
//!   `/v1/organization/costs` (else the summed `cost_usd` of the usage API) and
//!   per-model usage from `/v1/organization/usage/completions?group_by=model`.
//!   Krill allows 10 such requests per hour, so reports are fetched on demand
//!   only, cached on disk ([`HttpLineProber::with_cache_dir`]) for ten minutes,
//!   and a 429 keeps the saved report with a "rate limited" notice.
//!
//! ## Secrets
//!
//! Keys are sent only to the origins of the key's own site, only over https (or
//! loopback http for tests and local forwarders), never along a redirect, and are
//! never logged; error texts are redacted.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod account;
mod cache;
mod catalog;
mod classify;
mod fetch;
mod http;
mod keys;
mod models;
mod org;
mod probe;
mod responses;
mod transports;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use superba_core::{
    AccountSummary, AuthApiHealth, AuthMode, Clock, KeyCheck, Line, LineCaps, LineProber,
    ModelInfo, ModelList, ModelListRequest, Origin, ResolvedEndpoints, ResponsesProbe, Result,
    Secret, Site, SystemClock, TransportProbe, UsageQuery, UsageReport,
};
use url::Url;

pub use models::MODEL_LIST_TTL;

pub use catalog::{
    ENV_SUPERBA_TEST_LINES, SiteCatalog, is_builtin_site_id, new_compatible_site, parse_test_lines,
    valid_model_name, validate_custom_site, validate_site,
};

/// Codex version sent as `client_version` on `/backend-api/codex/models` when the
/// caller does not know the installed one (the version the facts were verified
/// with).
pub const DEFAULT_CLIENT_VERSION: &str = "0.162.0";

/// Tunables for [`HttpLineProber`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeConfig {
    /// Whole-request timeout of probe and key-check requests (connect, headers and
    /// body), also used for the latency warm-up.
    pub request_timeout: Duration,
    /// Timed samples per latency probe (after one warm-up request). `0` uses the
    /// warm-up time (which includes the TLS handshake).
    pub latency_samples: u32,
    /// `User-Agent` header.
    pub user_agent: String,
    /// TCP/TLS connect timeout.
    pub connect_timeout: Duration,
    /// Timeout of one timed latency sample.
    pub latency_timeout: Duration,
    /// Timeout of the Responses / compact probe (complete answer).
    pub responses_timeout: Duration,
    /// `client_version` query of `/backend-api/codex/models` (Codex sends its own
    /// version; set the installed version when known).
    pub client_version: String,
    /// Statuses an unauthenticated request to a relay endpoint must return to count
    /// as "the relay answered" (Krill: 401).
    pub unauth_expect: Vec<u16>,
    /// Use the system proxy configuration for non-loopback origins (as the desktop
    /// app does). Loopback origins are always reached directly.
    pub use_system_proxy: bool,
    /// Also probe `/responses/compact` after a successful Responses probe.
    pub probe_compact: bool,
    /// Largest model catalog accepted by `list_models`, in bytes.
    pub max_catalog_bytes: usize,
    /// Report PAT as unavailable on plain-http lines (Codex refuses a plain-http
    /// `chatgpt_base_url`). Only mock-server tests turn this off.
    pub pat_requires_https: bool,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(8),
            latency_samples: 3,
            user_agent: format!("superba/{}", env!("CARGO_PKG_VERSION")),
            connect_timeout: Duration::from_secs(5),
            latency_timeout: Duration::from_secs(5),
            responses_timeout: Duration::from_secs(90),
            client_version: DEFAULT_CLIENT_VERSION.to_owned(),
            unauth_expect: vec![401],
            use_system_proxy: true,
            probe_compact: true,
            max_catalog_bytes: 8 * 1024 * 1024,
            pat_requires_https: true,
        }
    }
}

/// [`LineProber`] over HTTPS.
#[derive(Debug, Clone)]
pub struct HttpLineProber {
    config: ProbeConfig,
    http: http::Http,
    clock: Arc<dyn Clock>,
    caches: Arc<Caches>,
    models: Arc<models::ModelListCache>,
}

/// Account data caches shared by clones of one prober.
#[derive(Debug, Default)]
struct Caches {
    summary: cache::SummaryCache,
    usage: cache::UsageCache,
}

impl Default for HttpLineProber {
    fn default() -> Self {
        Self::new(ProbeConfig::default())
    }
}

impl HttpLineProber {
    /// Create a prober. Never panics: if the HTTP client cannot be built, every
    /// request reports it as a network error.
    pub fn new(config: ProbeConfig) -> Self {
        let http = http::Http::new(&config);
        Self {
            config,
            http,
            clock: Arc::new(SystemClock::new()),
            caches: Arc::default(),
            models: Arc::default(),
        }
    }

    /// Use `clock` for `checked_at` timestamps and cache ages (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Keep usage reports under `dir` (e.g. `<data dir>/cache/usage`), so the
    /// rate-limited usage API is asked at most once per
    /// [`UsageQuery::CACHE_TTL_SECS`] even across processes. Without it reports
    /// are not cached. Also resets the in-memory account summary cache.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.caches = Arc::new(Caches {
            summary: cache::SummaryCache::default(),
            usage: cache::UsageCache::new(Some(dir.into())),
        });
        self
    }

    /// Current configuration.
    pub fn config(&self) -> &ProbeConfig {
        &self.config
    }
}

#[async_trait]
impl LineProber for HttpLineProber {
    async fn probe(&self, site: &Site, line: &Line) -> Result<LineCaps> {
        Ok(self.probe_line(site, line).await)
    }

    async fn probe_all(&self, site: &Site) -> Result<Vec<LineCaps>> {
        let probes = site.lines.iter().map(|line| self.probe_line(site, line));
        Ok(futures_util::future::join_all(probes).await)
    }

    async fn check_key(
        &self,
        site: &Site,
        line: &Line,
        mode: AuthMode,
        key: &Secret,
    ) -> Result<KeyCheck> {
        self.check_key_impl(site, line, mode, key).await
    }

    async fn check_authapi(
        &self,
        site: &Site,
        origin: &Origin,
        key: &Secret,
    ) -> Result<AuthApiHealth> {
        self.check_authapi_impl(site, origin, key).await
    }

    async fn list_models(&self, openai_base_url: &Url, key: &Secret) -> Result<Vec<ModelInfo>> {
        self.list_models_impl(openai_base_url, key).await
    }

    async fn model_list(&self, request: ModelListRequest<'_>) -> Result<ModelList> {
        self.model_list_impl(request).await
    }

    async fn probe_responses(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<ResponsesProbe> {
        self.probe_responses_impl(endpoints, key, model).await
    }

    async fn account_summary(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Result<Option<AccountSummary>> {
        self.account_summary_impl(site, line, key).await
    }

    async fn usage_report(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
        query: &UsageQuery,
    ) -> Result<UsageReport> {
        self.usage_report_impl(site, line, key, query).await
    }

    async fn probe_with_key(&self, site: &Site, line: &Line, key: &Secret) -> Result<LineCaps> {
        self.probe_with_key_impl(site, line, key).await
    }

    async fn probe_transports(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
    ) -> Result<TransportProbe> {
        self.probe_transports_impl(endpoints, key).await
    }
}
