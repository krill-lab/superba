//! Fake service, area "router" (owner B7).
//! Router & usage page: request log, statistics, recording settings and session names.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file.
//!
//! The request log is an in-memory list of fixture rows filtered, sorted and
//! totalled like the router's SQLite reader. Session names come from a built-in
//! table that mirrors `tests/fixtures/router/state_5.sqlite`, or — after
//! [`FakeService::titles_from_home`] — from a real `state_5.sqlite` through the
//! real switch service (read-only), so tests exercise the real lookup.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use superba_core::*;

use super::FakeService;

/// Thread ids of the fixture sessions (synthetic).
const THREAD_LOGIN: &str = "019a7c00-0000-7000-8000-0000000000e1";
/// Second fixture thread (only a first user message).
const THREAD_REPORT: &str = "019a7c00-0000-7000-8000-0000000000e2";
/// Third fixture thread (a user-given name).
const THREAD_ROUTER: &str = "01950000-0000-7000-8000-00000000009b";
/// A key no thread matches.
const THREAD_UNKNOWN: &str = "019affff-0000-7000-8000-000000000000";

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {
    rows: Option<Vec<RequestRow>>,
    inflight: Vec<InflightRequest>,
    titles_home: Option<PathBuf>,
    saved: Vec<RouterStatsSettings>,
    cleared: Vec<ClearRequest>,
    ui_locales: Vec<Locale>,
    title_queries: usize,
    translation: Option<superba_router::translate::routing::TranslationConfig>,
    conversion_error: Option<Error>,
    /// Saved failover rules (`None`: the defaults of a configured router).
    failover: Option<superba_core::RouterFailover>,
}

/// `(id, name, title, first user message)` of a fixture thread.
type Thread = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);

/// `(name, title, first user message)` of the fixture threads (the same rows as
/// `tests/fixtures/router/state_5.sqlite`).
fn fixture_threads() -> Vec<Thread> {
    vec![
        (
            THREAD_LOGIN,
            None,
            Some("登录页样式"),
            Some("把登录页改成新的样式"),
        ),
        (
            THREAD_REPORT,
            None,
            None,
            Some("写周报：总结本周的 router 测试"),
        ),
        (
            THREAD_ROUTER,
            Some("router 测试"),
            Some("router tests"),
            None,
        ),
    ]
}

/// A fixture row.
#[allow(clippy::too_many_arguments)]
fn row(
    id: i64,
    at: Timestamp,
    key: &str,
    member: (&str, &str),
    line: (&str, &str, &str),
    model: &str,
    status: RequestStatus,
    usage: Option<(u64, u64, u64, u64)>,
    times: (u64, u64),
) -> RequestRow {
    let ok = status == RequestStatus::Ok;
    let (input, cached, write, output) = usage.unwrap_or_default();
    let has = usage.is_some();
    RequestRow {
        id,
        started_at: at,
        prompt_cache_key: Some(key.to_owned()),
        thread_id: (!key.contains(':')).then(|| key.to_owned()),
        turn_id: Some(format!("turn-{}", id / 3)),
        member_id: ProfileId::new(member.0),
        member_name: member.1.to_owned(),
        line_id: Some(line.0.to_owned()),
        line_name: Some(line.1.to_owned()),
        upstream_host: Some(line.2.to_owned()),
        failovers: 0,
        transport: RequestTransport::Http,
        originator: Some(codex::ORIGINATOR_DESKTOP.to_owned()),
        client: RequestClient::Desktop,
        request_kind: Some(RequestKind::Turn),
        thread_source: Some("user".into()),
        model: Some(model.to_owned()),
        upstream_model: None,
        response_model: ok.then(|| model.to_owned()),
        service_tier_requested: None,
        service_tier_answered: ok.then(|| "default".to_owned()),
        status,
        http_status: Some(if ok { 200 } else { 429 }),
        error: None,
        input_tokens: has.then_some(input),
        cached_tokens: has.then_some(cached),
        cache_write_tokens: has.then_some(write),
        output_tokens: has.then_some(output),
        reasoning_tokens: has.then_some(output / 4),
        ttfb_ms: has.then_some(times.0 / 2),
        first_token_ms: has.then_some(times.0),
        duration_ms: Some(times.1),
        tps_milli: (has && output >= 10)
            .then(|| output * 1_000_000 / times.1.saturating_sub(times.0 / 2).max(200)),
        cost: has
            .then(|| Usd::from_micros(((input - cached) * 2 + cached / 5 + output * 8) as i64)),
        price_rev: has.then(|| "2026-10".to_owned()),
        injected: false,
        stripped: 0,
        upstream_request_id: Some(format!("req_{id:08x}")),
        source: superba_core::RequestSource::Router,
        codex_host: None,
        harness: superba_core::RequestHarness::Codex,
        harness_version: None,
        response_id: None,
        protocol: superba_core::RequestProtocol::Responses,
        requested_model: None,
        session_id: None,
        agent_kind: None,
        attempts: Vec::new(),
    }
}

/// The default fixture log (newest first): three sessions, two providers, a
/// failover, a 429, a cancelled request, a `codex exec` row and a title request.
fn fixture_rows(now: Timestamp) -> Vec<RequestRow> {
    let ago = |s: u64| now.saturating_sub(Duration::from_secs(s));
    let krill = ("krill", "Krill");
    let api = ("my-api", "My API");
    let over = ("overseas", "海外线路", "api.krill-code.net");
    let cn = ("cn", "国内极速", "api-slb.krill-code.net");
    let myapi = ("default", "api.example.com", "api.example.com");
    let mut rows = vec![
        row(
            12,
            ago(20),
            THREAD_LOGIN,
            krill,
            over,
            "gpt-5.5",
            RequestStatus::Ok,
            Some((12_572, 11_904, 0, 321)),
            (820, 3_420),
        ),
        row(
            11,
            ago(47),
            THREAD_LOGIN,
            krill,
            cn,
            "gpt-5.5",
            RequestStatus::Ok,
            Some((9_210, 7_800, 0, 1_204)),
            (1_100, 17_900),
        ),
        row(
            10,
            ago(175),
            THREAD_REPORT,
            api,
            myapi,
            "gpt-6-luna",
            RequestStatus::Ok,
            Some((2_001, 0, 0, 88)),
            (2_310, 4_470),
        ),
        row(
            9,
            ago(192),
            THREAD_LOGIN,
            krill,
            over,
            "gpt-5.5",
            RequestStatus::Http {
                status: 429,
                code: Some("rate_limited".into()),
            },
            None,
            (0, 310),
        ),
        row(
            8,
            ago(217),
            THREAD_LOGIN,
            krill,
            over,
            "gpt-5.5",
            RequestStatus::Cancelled,
            None,
            (1_020, 6_800),
        ),
        row(
            7,
            ago(3_600),
            THREAD_ROUTER,
            krill,
            over,
            "gpt-6-astra",
            RequestStatus::Ok,
            Some((40_000, 36_000, 1_200, 2_400)),
            (640, 22_000),
        ),
        row(
            6,
            ago(3_700),
            &format!("thread_title:{THREAD_ROUTER}"),
            krill,
            over,
            "gpt-6-luna",
            RequestStatus::Ok,
            Some((900, 0, 0, 12)),
            (300, 800),
        ),
        row(
            5,
            ago(5_000),
            THREAD_UNKNOWN,
            krill,
            over,
            "gpt-5.5",
            RequestStatus::Ok,
            Some((1_000, 0, 0, 40)),
            (500, 1_900),
        ),
    ];
    rows[1].failovers = 1;
    rows[1].status = RequestStatus::Ok;
    rows[5].service_tier_requested = Some("priority".into());
    rows[5].service_tier_answered = Some("priority".into());
    rows[5].injected = true;
    rows[6].thread_source = Some("thread_title".into());
    rows[7].client = RequestClient::Exec;
    rows[7].originator = Some(codex::ORIGINATOR_EXEC.into());
    // A defensive check: even a (wrong) injected flag on an exec row is never
    // shown as "line added".
    rows[7].injected = true;
    rows
}

/// `n` generated rows (performance and paging tests), newest first.
fn many_rows(now: Timestamp, n: usize) -> Vec<RequestRow> {
    let keys = [THREAD_LOGIN, THREAD_REPORT, THREAD_ROUTER, THREAD_UNKNOWN];
    (0..n)
        .map(|i| {
            row(
                (n - i) as i64,
                now.saturating_sub(Duration::from_secs(i as u64 * 7)),
                keys[i % keys.len()],
                ("krill", "Krill"),
                ("overseas", "海外线路", "api.krill-code.net"),
                if i % 3 == 0 { "gpt-6-astra" } else { "gpt-5.5" },
                RequestStatus::Ok,
                Some((1_000 + i as u64, 500, 0, 100 + (i as u64 % 50))),
                (500 + (i as u64 % 900), 3_000 + (i as u64 % 5_000)),
            )
        })
        .collect()
}

impl FakeService {
    /// The failover rules a configured router has.
    pub fn router_failover(&self) -> superba_core::RouterFailover {
        self.areas
            .router
            .lock()
            .unwrap()
            .failover
            .clone()
            .unwrap_or_default()
    }

    pub async fn edit_router_failover(
        &self,
        failover: superba_core::RouterFailover,
    ) -> Result<RouterView> {
        failover.validate()?;
        self.areas.router.lock().unwrap().failover = Some(failover);
        self.router_status().await
    }

    pub fn set_router_translation(
        &self,
        translation: Option<superba_router::translate::routing::TranslationConfig>,
    ) {
        self.areas.router.lock().unwrap().translation = translation;
    }

    pub fn router_translation(&self) -> Option<Vec<RouterConversion>> {
        self.areas
            .router
            .lock()
            .unwrap()
            .translation
            .as_ref()
            .map(|t| t.view())
    }

    pub fn fail_router_conversion(&self, error: Option<Error>) {
        self.areas.router.lock().unwrap().conversion_error = error;
    }

    pub async fn edit_router_conversion(&self, conversion: RouterConversion) -> Result<RouterView> {
        {
            let mut state = self.areas.router.lock().unwrap();
            if let Some(error) = &state.conversion_error {
                return Err(error.clone());
            }
            state
                .translation
                .as_mut()
                .ok_or_else(|| Error::NotFound {
                    what: "router configuration".into(),
                })?
                .set_conversion(conversion)?;
        }
        self.router_status().await
    }

    /// Fixture thread with a title (登录页样式).
    pub const THREAD_LOGIN: &str = THREAD_LOGIN;
    /// Fixture thread with only a first user message.
    pub const THREAD_REPORT: &str = THREAD_REPORT;
    /// Fixture thread with a user-given name (router 测试).
    pub const THREAD_ROUTER: &str = THREAD_ROUTER;
    /// A key no fixture thread matches.
    pub const THREAD_UNKNOWN: &str = THREAD_UNKNOWN;

    /// The default fixture log at `now`.
    pub fn fixture_request_rows(now: Timestamp) -> Vec<RequestRow> {
        fixture_rows(now)
    }

    /// `n` generated rows.
    pub fn many_request_rows(now: Timestamp, n: usize) -> Vec<RequestRow> {
        many_rows(now, n)
    }

    /// Replace the request log.
    pub fn set_request_rows(&self, rows: Vec<RequestRow>) {
        self.areas.router.lock().unwrap().rows = Some(rows);
    }

    /// The request log now.
    pub fn request_rows(&self) -> Vec<RequestRow> {
        rows(self)
    }

    /// Replace the requests in flight.
    pub fn set_inflight(&self, inflight: Vec<InflightRequest>) {
        self.areas.router.lock().unwrap().inflight = inflight;
    }

    /// Resolve session names from `<root>/home/.codex/state_*.sqlite` with the
    /// real (read-only) lookup of the switch service.
    pub fn titles_from_home(&self, root: PathBuf) {
        self.areas.router.lock().unwrap().titles_home = Some(root);
    }

    /// Stats settings saved through `router_set_stats`, in order.
    pub fn saved_stats(&self) -> Vec<RouterStatsSettings> {
        self.areas.router.lock().unwrap().saved.clone()
    }

    /// UI languages reported through `set_ui_locale`, in order.
    pub fn ui_locales(&self) -> Vec<Locale> {
        self.areas.router.lock().unwrap().ui_locales.clone()
    }

    /// Clears requested, in order.
    pub fn clears(&self) -> Vec<ClearRequest> {
        self.areas.router.lock().unwrap().cleared.clone()
    }

    /// Session name lookups served.
    pub fn title_queries(&self) -> usize {
        self.areas.router.lock().unwrap().title_queries
    }

    /// Change the stored profiles and settings (router member scenarios).
    pub fn edit_profiles(&self, f: impl FnOnce(&mut ProfileSet)) {
        f(&mut self.state().profiles);
    }
}

fn rows(fake: &FakeService) -> Vec<RequestRow> {
    let mut s = fake.areas.router.lock().unwrap();
    s.rows
        .get_or_insert_with(|| fixture_rows(Timestamp::now()))
        .clone()
}

fn session_key(r: &RequestRow) -> Option<String> {
    r.session_group_key()
}

fn matches(q: &RequestQuery, r: &RequestRow, with_group: bool, with_after: bool) -> bool {
    if q.from.is_some_and(|f| r.started_at < f) || q.to.is_some_and(|t| r.started_at >= t) {
        return false;
    }
    if q.member.as_ref().is_some_and(|m| &r.member_id != m)
        || q.line
            .as_ref()
            .is_some_and(|l| r.line_id.as_ref() != Some(l))
        || q.model.as_ref().is_some_and(|m| {
            r.served_model() != Some(m.as_str())
                && r.model.as_ref() != Some(m)
                && r.response_model.as_ref() != Some(m)
                && r.requested_model.as_ref() != Some(m)
        })
        || q.client.is_some_and(|c| r.client != c)
        || (!q.harnesses.is_empty() && !q.harnesses.contains(&r.harness))
        || q.host.as_deref().is_some_and(|h| h != r.host_key())
        || q.protocol.is_some_and(|p| p != r.protocol)
        || q.source.is_some_and(|s| s != r.source)
    {
        return false;
    }
    match q.status {
        RequestStatusFilter::Ok if r.status != RequestStatus::Ok => return false,
        RequestStatusFilter::Errors if r.status == RequestStatus::Ok => return false,
        RequestStatusFilter::Failover if r.failovers == 0 => return false,
        _ => {}
    }
    if with_group && let Some(g) = &q.group {
        let hit = match q.group_by {
            RequestGrouping::Provider => r.in_provider_group(g),
            RequestGrouping::Harness => r.harness.id() == g,
            RequestGrouping::HarnessModel => r.harness_model_key() == *g,
            _ => session_key(r).as_deref() == Some(g.as_str()),
        };
        if !hit {
            return false;
        }
    }
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let s = s.to_lowercase();
        let fields = [
            r.prompt_cache_key.as_deref(),
            r.thread_id.as_deref(),
            Some(r.member_name.as_str()),
            r.line_name.as_deref(),
            r.upstream_host.as_deref(),
            r.model.as_deref(),
            r.requested_model.as_deref(),
            Some(r.harness.id()),
            r.session_id.as_deref(),
            r.upstream_request_id.as_deref(),
        ];
        if !fields
            .into_iter()
            .flatten()
            .any(|f| f.to_lowercase().contains(&s))
        {
            return false;
        }
    }
    !(with_after && q.after_id.is_some_and(|a| r.id <= a))
}

fn totals(rows: &[&RequestRow]) -> RequestTotals {
    let mut t = RequestTotals::default();
    for r in rows {
        t.requests += 1;
        if r.status == RequestStatus::Ok {
            t.ok += 1;
        } else {
            t.errors += 1;
        }
        t.input_tokens += r.input_tokens.unwrap_or(0);
        t.cached_tokens += r.cached_tokens.unwrap_or(0);
        t.cache_write_tokens += r.cache_write_tokens.unwrap_or(0);
        t.output_tokens += r.output_tokens.unwrap_or(0);
        t.add_timing(r.first_token_ms, r.tps_milli, r.output_tokens);
        match r.cost {
            Some(c) => t.cost = t.cost.saturating_add(c),
            None if r.input_tokens.is_some() => t.unpriced += 1,
            None => {}
        }
    }
    t
}

/// Body of `SwitchService::router_requests`.
pub(super) async fn router_requests(
    fake: &FakeService,
    query: RequestQuery,
) -> Result<RequestPage> {
    let all = rows(fake);
    let mut hit: Vec<&RequestRow> = all
        .iter()
        .filter(|r| matches(&query, r, true, true))
        .collect();
    let sort_key = |r: &RequestRow| -> (i128, String) {
        match query.sort.key {
            RequestSortKey::CacheKey | RequestSortKey::Session => {
                (0, session_key(r).unwrap_or_default())
            }
            RequestSortKey::Provider => (
                0,
                format!(
                    "{} {}",
                    r.member_name,
                    r.line_name.clone().unwrap_or_default()
                ),
            ),
            RequestSortKey::Status => (0, format!("{:?}", r.status)),
            RequestSortKey::Input => (r.fresh_input_tokens().map_or(-1, i128::from), String::new()),
            RequestSortKey::Output => (r.output_tokens.map_or(-1, i128::from), String::new()),
            RequestSortKey::CacheRead => (r.cached_tokens.map_or(-1, i128::from), String::new()),
            RequestSortKey::CacheWrite => {
                (r.cache_write_tokens.map_or(-1, i128::from), String::new())
            }
            RequestSortKey::FirstToken => (r.first_token_ms.map_or(-1, i128::from), String::new()),
            RequestSortKey::Duration => (r.duration_ms.map_or(-1, i128::from), String::new()),
            RequestSortKey::Tps => (r.tps_milli.map_or(-1, i128::from), String::new()),
            RequestSortKey::Cost => (r.cost.map_or(-1, |c| i128::from(c.micros())), String::new()),
            _ => (i128::from(r.started_at.as_millis()), String::new()),
        }
    };
    hit.sort_by(|a, b| {
        let o = sort_key(a).cmp(&sort_key(b)).then(a.id.cmp(&b.id));
        if query.sort.descending {
            o.reverse()
        } else {
            o
        }
    });
    let limit = if query.limit == 0 {
        RequestQuery::DEFAULT_LIMIT
    } else {
        query.limit
    } as usize;
    let truncated = hit.len() > limit;
    let shown: Vec<RequestRow> = hit.iter().take(limit).map(|r| (*r).clone()).collect();
    let tot: Vec<&RequestRow> = all
        .iter()
        .filter(|r| matches(&query, r, true, false))
        .collect();
    let by_provider = query.group_by == RequestGrouping::Provider;
    let mut groups: BTreeMap<String, RequestGroup> = BTreeMap::new();
    let mut lines: BTreeMap<(String, String), RequestGroup> = BTreeMap::new();
    let group = |key: String, label: String, r: &RequestRow| RequestGroup {
        key,
        label,
        count: 0,
        last: r.started_at,
        upstream_host: r.upstream_host.clone(),
        lines: Vec::new(),
    };
    let add = |g: &mut RequestGroup, r: &RequestRow, label: &str| {
        g.count += 1;
        if r.started_at >= g.last {
            g.last = r.started_at;
            g.label = label.to_owned();
            g.upstream_host = r.upstream_host.clone();
        }
    };
    for r in all.iter().filter(|r| matches(&query, r, false, false)) {
        let k = match query.group_by {
            RequestGrouping::Provider => r.provider_key().map(str::to_owned),
            RequestGrouping::Harness => Some(r.harness.id().to_owned()),
            RequestGrouping::HarnessModel => Some(r.harness_model_key()),
            _ => session_key(r),
        };
        let Some(k) = k else {
            continue;
        };
        let label = if by_provider {
            r.member_name.clone()
        } else {
            k.clone()
        };
        add(
            groups
                .entry(k.clone())
                .or_insert_with(|| group(k.clone(), label.clone(), r)),
            r,
            &label,
        );
        if by_provider && let Some(l) = r.line_label() {
            let key = provider_line_key(&k, l);
            add(
                lines
                    .entry((k.clone(), l.to_owned()))
                    .or_insert_with(|| group(key, l.to_owned(), r)),
                r,
                l,
            );
        }
    }
    let mut lines: Vec<((String, String), RequestGroup)> = lines.into_iter().collect();
    lines.sort_by_key(|(_, l)| std::cmp::Reverse(l.count));
    for ((provider, _), line) in lines {
        if let Some(g) = groups.get_mut(&provider) {
            g.lines.push(line);
        }
    }
    let mut groups: Vec<RequestGroup> = groups.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.last));
    Ok(RequestPage {
        rows: shown,
        totals: totals(&tot),
        groups,
        truncated,
        recording: true,
    })
}

/// Body of `SwitchService::router_request`.
pub(super) async fn router_request(fake: &FakeService, id: i64) -> Result<RequestDetail> {
    let row = rows(fake)
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| Error::NotFound {
            what: format!("request {id}"),
        })?;
    let mut attempts = Vec::new();
    if row.failovers > 0 {
        attempts.push(RequestAttempt {
            member_id: row.member_id.clone(),
            line_name: Some("国内极速".into()),
            upstream_host: Some("api-slb.krill-code.net".into()),
            status: Some(502),
            outcome: "502".into(),
            elapsed_ms: Some(120),
            compression: Some(superba_core::RequestCompression::RouterZstd),
            upstream_protocol: None,
        });
    }
    attempts.push(RequestAttempt {
        member_id: row.member_id.clone(),
        line_name: row.line_name.clone(),
        upstream_host: row.upstream_host.clone(),
        status: row.http_status,
        outcome: "answered".into(),
        elapsed_ms: row.duration_ms,
        compression: (row.transport == superba_core::RequestTransport::Http)
            .then_some(superba_core::RequestCompression::RouterZstd),
        upstream_protocol: None,
    });
    Ok(RequestDetail {
        timing: RequestTiming {
            sent_ms: Some(2),
            headers_ms: row.ttfb_ms,
            first_event_ms: row.ttfb_ms.map(|t| t + 15),
            first_token_ms: row.first_token_ms,
            last_delta_ms: row.duration_ms.map(|d| d.saturating_sub(12)),
            end_ms: row.duration_ms,
        }
        .normalized(),
        attempts,
        log: vec![format!(
            "attempt 1 · {}",
            row.line_name.clone().unwrap_or_default()
        )],
        row,
    })
}

/// Body of `SwitchService::router_inflight`.
pub(super) async fn router_inflight(fake: &FakeService) -> Result<Vec<InflightRequest>> {
    Ok(fake.areas.router.lock().unwrap().inflight.clone())
}

fn day_of(t: Timestamp, offset_minutes: i32) -> String {
    let (y, m, d) =
        Timestamp::from_millis(t.as_millis() + i64::from(offset_minutes) * 60_000).utc_date();
    format!("{y:04}-{m:02}-{d:02}")
}

/// Body of `SwitchService::router_stats`.
pub(super) async fn router_stats(fake: &FakeService, query: StatsQuery) -> Result<RequestStats> {
    let all = rows(fake);
    let hit: Vec<&RequestRow> = all
        .iter()
        .filter(|r| {
            query.from.is_none_or(|f| r.started_at >= f)
                && query.to.is_none_or(|t| r.started_at < t)
                && query.member.as_ref().is_none_or(|m| &r.member_id == m)
                && (query.harnesses.is_empty() || query.harnesses.contains(&r.harness))
                && query.model.as_deref().is_none_or(|m| {
                    r.served_model() == Some(m)
                        || r.model.as_deref() == Some(m)
                        || r.response_model.as_deref() == Some(m)
                        || r.requested_model.as_deref() == Some(m)
                })
                && query.host.as_deref().is_none_or(|h| h == r.host_key())
                && query.protocol.is_none_or(|p| p == r.protocol)
                && query.source.is_none_or(|s| s == r.source)
        })
        .collect();
    let bucket = |key_of: &dyn Fn(&RequestRow) -> (String, String)| {
        let mut map: BTreeMap<String, (String, Vec<&RequestRow>)> = BTreeMap::new();
        for r in &hit {
            let (k, l) = key_of(r);
            map.entry(k).or_insert_with(|| (l, Vec::new())).1.push(r);
        }
        map.into_iter()
            .map(|(key, (label, rows))| StatsBucket {
                label: if label.is_empty() { key.clone() } else { label },
                key,
                totals: totals(&rows),
                lines: Vec::new(),
            })
            .collect::<Vec<_>>()
    };
    let off = query.utc_offset_minutes;
    let mut buckets = match query.by {
        StatsGroupBy::Provider => {
            let mut providers = bucket(&|r| {
                (
                    r.provider_key().unwrap_or("").to_owned(),
                    r.member_name.clone(),
                )
            });
            let lines = bucket(&|r| match r.line_label() {
                Some(l) => (
                    provider_line_key(r.provider_key().unwrap_or(""), l),
                    l.to_owned(),
                ),
                None => (String::new(), String::new()),
            });
            for p in &mut providers {
                p.lines = lines
                    .iter()
                    .filter(|l| !l.key.is_empty() && split_provider_key(&l.key).0 == p.key)
                    .cloned()
                    .collect();
                p.lines.sort_by_key(|l| std::cmp::Reverse(l.totals.cost));
            }
            providers
        }
        StatsGroupBy::Harness => bucket(&|r| (r.harness.id().to_owned(), String::new())),
        StatsGroupBy::HarnessModel => bucket(&|r| (r.harness_model_key(), String::new())),
        StatsGroupBy::Host => bucket(&|r| (r.host_key().to_owned(), String::new())),
        StatsGroupBy::Day => bucket(&|r| (day_of(r.started_at, off), String::new())),
        StatsGroupBy::Session => bucket(&|r| (session_key(r).unwrap_or_default(), String::new())),
        _ => bucket(&|r| {
            (
                r.served_model().unwrap_or_default().to_owned(),
                String::new(),
            )
        }),
    };
    if query.by != StatsGroupBy::Day {
        buckets.sort_by(|a, b| {
            b.totals
                .cost
                .cmp(&a.totals.cost)
                .then(b.totals.requests.cmp(&a.totals.requests))
        });
    }
    Ok(RequestStats {
        buckets,
        daily: bucket(&|r| (day_of(r.started_at, off), String::new())),
        totals: totals(&hit),
    })
}

/// Body of `SwitchService::router_clear`.
pub(super) async fn router_clear(fake: &FakeService, request: ClearRequest) -> Result<ClearReport> {
    let mut all = rows(fake);
    let mut s = fake.areas.router.lock().unwrap();
    s.cleared.push(request.clone());
    let before = all.len();
    all.retain(|r| request.before.is_some_and(|b| r.started_at >= b));
    let removed = (before - all.len()) as u64;
    s.rows = Some(all);
    Ok(ClearReport { rows: removed })
}

/// Body of `SwitchService::router_set_stats`.
pub(super) async fn router_set_stats(
    fake: &FakeService,
    settings: RouterStatsSettings,
) -> Result<RouterView> {
    fake.areas
        .router
        .lock()
        .unwrap()
        .saved
        .push(settings.clone());
    fake.state().profiles.settings.router.stats = settings;
    fake.router_status().await
}

/// Body of `SwitchService::set_ui_locale`.
pub(super) async fn set_ui_locale(fake: &FakeService, locale: Locale) -> Result<()> {
    fake.areas.router.lock().unwrap().ui_locales.push(locale);
    Ok(())
}

/// Body of `SwitchService::session_titles`.
/// Body of `SwitchService::price_table`: two public list prices.
pub(super) async fn price_table(_fake: &FakeService) -> Result<PriceTable> {
    let usd = |v: f64| Usd::from_dollars(v).expect("price");
    let entry = |input: f64, cached: f64, output: f64| PriceEntry {
        cache_write_1h: None,
        windows: Vec::new(),
        input: usd(input),
        cached_input: Some(usd(cached)),
        cache_write: None,
        output: usd(output),
        long_context: Vec::new(),
        tiers: BTreeMap::new(),
        tier_prices: BTreeMap::new(),
        source: Some("https://openai.com/api/pricing/".into()),
        rev: Some("2026-10-01".into()),
    };
    Ok(PriceTable {
        rev: "2026-10-01".into(),
        entries: [
            ("gpt-6-astra".to_owned(), entry(5.0, 0.5, 40.0)),
            ("gpt-6-luna".to_owned(), entry(0.25, 0.025, 2.0)),
        ]
        .into(),
    })
}

pub(super) async fn session_titles(
    fake: &FakeService,
    keys: Vec<String>,
) -> Result<BTreeMap<String, SessionTitle>> {
    let home = {
        let mut s = fake.areas.router.lock().unwrap();
        s.title_queries += 1;
        s.titles_home.clone()
    };
    if let Some(root) = home {
        let ctx = LocalContext::for_tests(&root);
        let real = superba_switch::Switcher::new(superba_switch::SwitcherDeps::with_defaults(ctx));
        return real.session_titles(keys).await;
    }
    let threads = fixture_threads();
    Ok(keys
        .into_iter()
        .map(|k| {
            let thread = k
                .split_once(':')
                .filter(|(src, _)| !src.is_empty() && !src.contains('-'))
                .map_or(k.as_str(), |(_, parent)| parent)
                .to_owned();
            let found = threads.iter().find(|(id, ..)| *id == thread);
            let title = match found {
                Some((_, Some(name), _, _)) | Some((_, None, Some(name), _)) => SessionTitle {
                    thread_id: Some(thread.clone()),
                    title: (*name).to_owned(),
                    source: TitleSource::Title,
                },
                Some((_, None, None, Some(first))) => SessionTitle {
                    thread_id: Some(thread.clone()),
                    title: (*first).to_owned(),
                    source: TitleSource::FirstUserMessage,
                },
                _ => SessionTitle {
                    thread_id: None,
                    title: k.chars().take(8).collect(),
                    source: TitleSource::ShortKey,
                },
            };
            (k, title)
        })
        .collect())
}
