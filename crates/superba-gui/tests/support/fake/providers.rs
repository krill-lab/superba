//! Fake service, area "providers" (owner B6).
//! Providers page: site probes and account reports.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file.
//!
//! Account reports: every key-based profile has a balance (billing endpoints);
//! Krill-kind sites also serve the usage API, rate-limited like the real relay
//! once a test turns that on. Site probes answer for saved sites with a model
//! list and an optional Responses result.

use std::time::Duration;

use superba_core::*;

use super::FakeService;

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {
    /// Every account request (profile, usage days, refresh).
    account_calls: Vec<(Option<ProfileId>, Option<u32>, bool)>,
    /// Answer usage requests with 429 from now on.
    rate_limited: bool,
    /// Sites probed before they were saved.
    unsaved_probes: Vec<Site>,
    /// Every `line_transports` request (profile, refresh).
    transport_calls: Vec<(ProfileId, TransportRefresh)>,
}

impl FakeService {
    fn providers_area(&self) -> std::sync::MutexGuard<'_, State> {
        self.areas
            .providers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Every `account` request so far: (profile, usage days, refresh).
    pub fn account_calls(&self) -> Vec<(Option<ProfileId>, Option<u32>, bool)> {
        self.providers_area().account_calls.clone()
    }

    /// Sites probed before they were saved (`SiteProbeRequest::site`).
    pub fn unsaved_probes(&self) -> Vec<Site> {
        self.providers_area().unsaved_probes.clone()
    }

    /// Every `line_transports` request so far: (profile, refresh).
    pub fn transport_calls(&self) -> Vec<(ProfileId, TransportRefresh)> {
        self.providers_area().transport_calls.clone()
    }

    /// Refuse usage reports with 429 (Krill's 10-an-hour limit).
    pub fn rate_limit_usage(&self, on: bool) {
        self.providers_area().rate_limited = on;
    }
}

/// Transport checks of a provider's lines: the first line upgrades the
/// WebSocket and decodes zstd, the second refuses the WebSocket (426) and
/// decodes zstd, the others refuse the WebSocket and do not tell about zstd.
/// `Cached` answers nothing until a check ran.
pub(super) async fn line_transports(
    fake: &FakeService,
    request: LineTransportsRequest,
) -> Result<Vec<LineTransports>> {
    fake.delay(150).await;
    let checked = {
        let mut area = fake.providers_area();
        area.transport_calls
            .push((request.profile.clone(), request.refresh));
        area.transport_calls
            .iter()
            .any(|(p, r)| p == &request.profile && *r != TransportRefresh::Cached)
    };
    let set = fake.profile_set().await?;
    let profile = set
        .profile(&request.profile)
        .ok_or_else(|| Error::NotFound {
            what: format!("profile {}", request.profile),
        })?;
    let site = fake.state().site(&profile.site_id)?;
    if !checked {
        return Ok(Vec::new());
    }
    let check = |s: TransportSupport, status: u16, detail: &str| {
        TransportCheck::new(s, Some(status), detail)
    };
    Ok(site
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let (ws, zstd) = match i {
                0 => (
                    check(TransportSupport::Supported, 101, "HTTP 101"),
                    check(TransportSupport::Supported, 400, "HTTP 400 (decoded, then rejected like the plain body): `model` is required"),
                ),
                1 => (
                    check(TransportSupport::Unsupported, 426, "HTTP 426: the line serves Responses over HTTP only"),
                    check(TransportSupport::Supported, 400, "HTTP 400 (decoded, then rejected like the plain body): `model` is required"),
                ),
                _ => (
                    check(TransportSupport::Unsupported, 426, "HTTP 426: the line serves Responses over HTTP only"),
                    check(TransportSupport::Unknown, 401, "HTTP 401: the key was rejected"),
                ),
            };
            LineTransports {
                site_id: site.id.clone(),
                line_id: l.id.clone(),
                checked_at: Timestamp::now(),
                websocket: ws,
                zstd,
            }
        })
        .collect())
}

/// A usage report of `days` days ending now.
pub fn usage_report(site: &SiteId, days: u32) -> UsageReport {
    let day = 86_400_000;
    let end = Timestamp::now();
    let start = Timestamp::from_millis(end.as_millis() - i64::from(days) * day);
    UsageReport {
        site_id: site.clone(),
        start,
        end,
        days,
        daily: (0..i64::from(days))
            .map(|i| DailyUsage {
                start: Timestamp::from_millis(start.as_millis() + i * day),
                cost: Usd::from_dollars(4.0 + (i % 5) as f64),
                requests: Some(300 + i as u64),
                input_tokens: Some(1_000_000),
                output_tokens: Some(200_000),
            })
            .collect(),
        models: ["gpt-6-astra", "gpt-5.5", "gpt-6-sol"]
            .iter()
            .enumerate()
            .map(|(i, m)| ModelUsage {
                model: (*m).into(),
                cost: Usd::from_dollars(27.98 - i as f64 * 7.0),
                requests: 1_354 - i as u64 * 300,
                input_tokens: 6_800_000,
                cached_input_tokens: 3_300_000,
                output_tokens: 1_900_000,
            })
            .collect(),
        total_cost: Usd::from_dollars(46.53),
        cost_source: Some(CostSource::Costs),
        usage_available: true,
        fetched_at: Timestamp::now(),
        from_cache: false,
        notice: None,
    }
}

/// Body of `SwitchService::probe_site`: the saved site (or the unsaved one the
/// request carries) answers, its model list has four models, the Responses call
/// (when asked) succeeds. Unsaved probes are counted.
pub(super) async fn probe_site(
    fake: &FakeService,
    request: SiteProbeRequest,
) -> Result<SiteProbeReport> {
    fake.delay(200).await;
    let site = match &request.site {
        Some(site) => {
            fake.providers_area().unsaved_probes.push(site.clone());
            site.clone()
        }
        None => fake.state().site(&request.site_id)?,
    };
    let line = match &request.line_id {
        Some(id) => site.line(id).cloned(),
        None => site.default_line().cloned(),
    }
    .ok_or_else(|| Error::NotFound {
        what: format!("a line of site {}", site.id),
    })?;
    let caps = LineCaps {
        site_id: site.id.clone(),
        line_id: line.id.clone(),
        checked_at: Timestamp::now(),
        reachable: true,
        latency_ms: Some(210),
        pat_ok: false,
        apikey_ok: true,
        codex_path: Some(CodexPath::AsIs),
        blocked_by_edge: false,
        key_access: None,
        checks: Vec::new(),
        detail: None,
    };
    let keyed = request.key.is_some();
    let models: Vec<ModelInfo> = ["deepseek-chat", "deepseek-reasoner", "qwen-max", "gpt-5.5"]
        .into_iter()
        .map(|id| ModelInfo {
            id: id.into(),
            ..ModelInfo::default()
        })
        .collect();
    Ok(SiteProbeReport {
        site_id: site.id.clone(),
        line_id: line.id.clone(),
        caps,
        key_check: keyed.then(|| KeyCheck {
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            checked_at: Timestamp::now(),
            key_last4: "9f3a".into(),
            whoami: None,
            identity: None,
            access: KeyAccess::Ok,
            codex_path: Some(CodexPath::AsIs),
        }),
        models: keyed.then_some(models),
        models_error: None,
        mapped: Vec::new(),
        model: None,
        responses: request.responses.then_some(ResponsesProbe {
            ok: true,
            status: Some(200),
            latency_ms: Some(640),
            compact_ok: None,
            detail: None,
        }),
        responses_error: None,
        account: None,
        account_error: None,
    })
}

/// Body of `SwitchService::account`: a balance for every key-based profile and,
/// on Krill-kind sites, the usage report asked for (or 429 when rate-limited).
pub(super) async fn account(fake: &FakeService, request: AccountRequest) -> Result<AccountReport> {
    fake.providers_area().account_calls.push((
        request.profile_id.clone(),
        request.usage.as_ref().map(|q| q.days),
        request.usage.as_ref().is_some_and(|q| q.refresh),
    ));
    fake.delay(150).await;
    let (profile, site) = {
        let s = fake.state();
        let id = request
            .profile_id
            .clone()
            .or_else(|| s.profiles.last_selected.clone())
            .ok_or_else(|| Error::invalid("no profile"))?;
        let profile = s
            .profiles
            .profile(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                what: format!("profile {id}"),
            })?;
        let site = s.site(&profile.site_id)?;
        (profile, site)
    };
    let line = profile
        .line_id
        .clone()
        .or_else(|| site.default_line().map(|l| l.id.clone()))
        .unwrap_or_else(|| LineId::new("default"));
    let mut billing = BillingSummary {
        limit: Usd::from_dollars(300.0),
        used: Usd::from_dollars(139.42),
        plan_title: Some("Pro".into()),
        access_until: Some(Timestamp::now().saturating_add(Duration::from_secs(23 * 86_400))),
        ..BillingSummary::default()
    };
    billing.derive_remaining();
    let summary = AccountSummary {
        email: Some("dev@example.com".into()),
        key_last4: Some("7a3c".into()),
        billing: Some(billing),
        ..AccountSummary::default()
    };
    let (usage, usage_error) = match (&request.usage, site.capabilities.usage_api) {
        (Some(_), _) if fake.providers_area().rate_limited => (
            None,
            Some(Error::Http {
                status: 429,
                url: "https://relay.example/v1/organization/costs".into(),
                message: "rate limited".into(),
            }),
        ),
        (Some(q), true) => (Some(usage_report(&site.id, q.days)), None),
        (Some(_), false) => (
            None,
            Some(Error::Unsupported {
                what: "usage API".into(),
            }),
        ),
        (None, _) => (None, None),
    };
    Ok(AccountReport {
        profile_id: profile.id.clone(),
        profile_name: profile.name.clone(),
        site_id: site.id.clone(),
        line_id: line,
        summary: Some(summary),
        summary_error: None,
        usage,
        usage_error,
    })
}
