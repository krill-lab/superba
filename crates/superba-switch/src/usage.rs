//! The Usage page's request log and statistics: the local router's database
//! merged with the model requests in Codex's rollout files
//! ([`superba_sessions::usage`]), so 最近请求 and 统计 work without the router.
//!
//! - Rollout rows get a provider from the thread's `model_provider` and the
//!   profile Superba had applied to the local Codex home at that time (the
//!   generation history), and a cost from the price table with that profile's
//!   ratio and overrides.
//! - A rollout row that describes a request the router recorded (same thread,
//!   same input and output tokens, router start inside the row's time window;
//!   failed and cancelled turns by thread, turn and time) is dropped: the
//!   router's row is richer.
//! - Filters, sorting, totals, source-list groups and statistics treat both
//!   sources alike; columns only the router measures stay `None`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::codex::PROVIDER_OPENAI;
use superba_core::{
    GenerationRecord, PriceSettings, PriceTable, ProfileId, ProfileSet, RequestClient,
    RequestDetail, RequestGroup, RequestGrouping, RequestPage, RequestQuery, RequestRow,
    RequestSortKey, RequestSource, RequestStats, RequestStatus, RequestStatusFilter, RequestTiming,
    RequestTotals, Result, StatsBucket, StatsGroupBy, StatsQuery, Timestamp,
};
use superba_sessions::usage::{RolloutRequest, UsageIndex};

use crate::Switcher;

/// A router row may start this long before a rollout row's derived start and
/// still describe the same request (the rollout start is the last input line,
/// the router's `t0` is when the request arrived).
pub const MATCH_BEFORE: Duration = Duration::from_secs(120);
/// ... and this long after its end (clock skew between the two writers).
pub const MATCH_AFTER: Duration = Duration::from_secs(5);
/// Most source-list groups returned (as the router's own query).
const MAX_GROUPS: usize = 500;

/// Shared rollout index of the local Codex home.
pub(crate) type SharedIndex = Arc<Mutex<UsageIndex>>;

/// What rollout rows are attributed and priced with.
#[derive(Clone, Debug, Default)]
pub struct Attribution {
    /// Generations of the local home, oldest first.
    pub generations: Vec<GenerationRecord>,
    /// Profile names by id.
    pub names: BTreeMap<ProfileId, String>,
    /// The price table.
    pub prices: PriceTable,
    /// Ratios and overrides.
    pub price_settings: PriceSettings,
}

impl Attribution {
    /// Names from a profile set (plus the built-in profiles).
    pub fn with_profiles(mut self, set: &ProfileSet) -> Self {
        for p in superba_core::catalog::builtin_profiles()
            .iter()
            .chain(set.profiles.iter())
        {
            self.names.insert(p.id.clone(), p.name.clone());
        }
        self.price_settings = set.settings.router.stats.prices.clone();
        self
    }

    /// Provider of a request of a thread with `model_provider` at `at`: for the
    /// provider id Superba writes (`openai`), the profile applied at that time;
    /// any other provider id (or no applied profile) stands for itself.
    pub fn member(&self, model_provider: Option<&str>, at: Timestamp) -> (ProfileId, String) {
        let provider = model_provider
            .filter(|p| !p.is_empty())
            .unwrap_or(PROVIDER_OPENAI);
        if provider == PROVIDER_OPENAI
            && let Some(id) = self
                .generations
                .iter()
                .rev()
                .find(|g| g.applied_at <= at)
                .and_then(|g| g.profile_id.clone())
        {
            let name = self
                .names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| id.to_string());
            return (id, name);
        }
        (
            ProfileId::new(format!("codex:{provider}")),
            provider.to_owned(),
        )
    }

    /// The log row of a rollout request.
    pub fn row(&self, r: &RolloutRequest) -> RequestRow {
        let (member_id, member_name) = self.member(r.model_provider.as_deref(), r.started_at);
        let usage = r.usage;
        // As the router prices its rows: the Models-page price wins, else the
        // list price; the basis goes to `price_rev`.
        let estimate = usage.zip(r.model.as_deref()).and_then(|(u, model)| {
            let entry = superba_models::price::lookup(
                &self.prices,
                &self.price_settings,
                &member_id,
                model,
            )
            .ok()
            .flatten()?;
            let tokens = superba_models::price::UsageTokens {
                input: u.input,
                cached: u.cached,
                cache_write: u.cache_write,
                output: u.output,
            };
            let ratio = superba_models::price::ratio_of(&self.price_settings, &member_id);
            let cost =
                superba_models::price::cost(&entry, &tokens, r.service_tier.as_deref(), ratio)
                    .ok()?;
            let is_override = self
                .price_settings
                .overrides
                .get(&member_id)
                .is_some_and(|o| {
                    o.contains_key(model)
                        || superba_models::split_tier_suffix(model)
                            .is_some_and(|(base, _)| o.contains_key(base))
                });
            let basis = if is_override {
                Some(superba_models::price::PriceBasis::Override)
            } else {
                superba_models::price::PriceBasis::of_list(&entry, &self.prices.rev)
            };
            Some((cost, basis.map(|b| b.encode())))
        });
        let (cost, price_rev) = match estimate {
            Some((c, basis)) => (Some(c), basis),
            None => (None, None),
        };
        RequestRow {
            id: r.id,
            started_at: r.started_at,
            prompt_cache_key: None,
            thread_id: Some(r.thread_id.clone()),
            turn_id: r.turn_id.clone(),
            member_id,
            member_name,
            line_id: None,
            line_name: None,
            upstream_host: None,
            failovers: 0,
            transport: Default::default(),
            originator: r.originator.clone(),
            client: RequestClient::from_originator(r.originator.as_deref()),
            request_kind: Some(r.kind),
            thread_source: r.thread_source.clone(),
            model: r.model.clone(),
            upstream_model: None,
            response_model: None,
            service_tier_requested: r.service_tier.clone(),
            service_tier_answered: None,
            status: r.status.clone(),
            http_status: None,
            error: None,
            input_tokens: usage.map(|u| u.input),
            cached_tokens: usage.map(|u| u.cached),
            cache_write_tokens: usage.map(|u| u.cache_write),
            output_tokens: usage.map(|u| u.output),
            reasoning_tokens: usage.map(|u| u.reasoning),
            ttfb_ms: None,
            first_token_ms: None,
            duration_ms: Some(r.duration_ms()),
            tps_milli: None,
            price_rev,
            cost,
            injected: false,
            stripped: 0,
            upstream_request_id: None,
            source: RequestSource::Rollout,
            codex_host: None,
            harness: superba_core::RequestHarness::Codex,
            harness_version: None,
            response_id: None,
            protocol: superba_core::RequestProtocol::Responses,
            requested_model: r.model.clone(),
            session_id: Some(r.thread_id.clone()),
            agent_kind: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

fn same_thread(router: &RequestRow, thread: &str) -> bool {
    router.thread_id.as_deref() == Some(thread)
        || router.prompt_cache_key.as_deref() == Some(thread)
}

fn end_of(row: &RequestRow) -> Timestamp {
    row.started_at
        .saturating_add(Duration::from_millis(row.duration_ms.unwrap_or(0)))
}

/// The rollout rows no router row describes. A router row matches at most one
/// rollout row: the same thread, a start inside the rollout row's window
/// ([`MATCH_BEFORE`] before its start to [`MATCH_AFTER`] after its end) and, for
/// rows with usage, the same input and output tokens; for failed and cancelled
/// rows an error row of the same turn (when both know it). The closest end wins.
pub fn dedupe(rollout: Vec<RequestRow>, router: &[RequestRow]) -> Vec<RequestRow> {
    let mut used = vec![false; router.len()];
    let mut out = Vec::with_capacity(rollout.len());
    for r in rollout {
        let Some(thread) = r.thread_id.as_deref() else {
            out.push(r);
            continue;
        };
        let lo = r.started_at.saturating_sub(MATCH_BEFORE);
        let hi = end_of(&r).saturating_add(MATCH_AFTER);
        let end = end_of(&r).as_millis();
        let best = router
            .iter()
            .enumerate()
            .filter(|(i, x)| {
                !used[*i] && same_thread(x, thread) && x.started_at >= lo && x.started_at <= hi
            })
            .filter(|(_, x)| match (r.input_tokens, r.output_tokens) {
                (Some(i), Some(o)) => x.input_tokens == Some(i) && x.output_tokens == Some(o),
                _ => {
                    x.status.is_error()
                        && x.input_tokens.is_none()
                        && (r.turn_id.is_none() || x.turn_id.is_none() || r.turn_id == x.turn_id)
                }
            })
            .min_by_key(|(_, x)| (end_of(x).as_millis() - end).abs())
            .map(|(i, _)| i);
        match best {
            Some(i) => used[i] = true,
            None => out.push(r),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Filters, sorting, totals
// ---------------------------------------------------------------------------

/// Session key of a row (prompt cache key, else thread id).
fn session_key(r: &RequestRow) -> Option<&str> {
    r.prompt_cache_key.as_deref().or(r.thread_id.as_deref())
}

fn status_kind(s: &RequestStatus) -> (&'static str, Option<&str>) {
    match s {
        RequestStatus::Ok => ("ok", None),
        RequestStatus::Http { code, .. } => ("http", code.as_deref()),
        RequestStatus::Failed { code } => ("failed", code.as_deref()),
        RequestStatus::Incomplete { reason } => ("incomplete", reason.as_deref()),
        RequestStatus::Broken => ("broken", None),
        RequestStatus::Cancelled => ("cancelled", None),
        _ => ("other", None),
    }
}

/// The row passes the query's filters (`with_group`: also the selected
/// source-list entry). `after_id` (live tail of router rows) matches no rollout
/// row.
pub fn matches(r: &RequestRow, q: &RequestQuery, with_group: bool) -> bool {
    if q.after_id.is_some() && r.source != RequestSource::Router {
        return false;
    }
    if q.source.is_some_and(|s| s != r.source) {
        return false;
    }
    if q.from.is_some_and(|f| r.started_at < f) || q.to.is_some_and(|t| r.started_at >= t) {
        return false;
    }
    if q.member.as_ref().is_some_and(|m| &r.member_id != m)
        || q.line
            .as_ref()
            .is_some_and(|l| r.line_id.as_ref() != Some(l))
        || q.model
            .as_ref()
            .is_some_and(|m| r.model.as_ref() != Some(m))
        || q.client.is_some_and(|c| r.client != c)
    {
        return false;
    }
    let status_ok = match q.status {
        RequestStatusFilter::Ok => !r.status.is_error(),
        RequestStatusFilter::Errors => r.status.is_error(),
        RequestStatusFilter::Failover => r.failovers > 0,
        _ => true,
    };
    if !status_ok {
        return false;
    }
    if with_group && let Some(g) = &q.group {
        let ok = match q.group_by {
            RequestGrouping::Provider => r.in_provider_group(g),
            _ => session_key(r) == Some(g.as_str()),
        };
        if !ok {
            return false;
        }
    }
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let needle = s.to_lowercase();
        let (kind, code) = status_kind(&r.status);
        let hay = [
            r.prompt_cache_key.as_deref(),
            r.thread_id.as_deref(),
            Some(r.member_name.as_str()),
            r.line_name.as_deref(),
            r.upstream_host.as_deref(),
            r.model.as_deref(),
            r.response_model.as_deref(),
            Some(kind),
            code,
            r.upstream_request_id.as_deref(),
        ];
        if !hay
            .iter()
            .flatten()
            .any(|h| h.to_lowercase().contains(&needle))
        {
            return false;
        }
    }
    true
}

fn fresh(r: &RequestRow) -> Option<u64> {
    r.fresh_input_tokens()
}

/// Order of two rows for the query's sort (as the router's SQL: a missing value
/// sorts before any value, ties by id).
pub fn compare(a: &RequestRow, b: &RequestRow, q: &RequestQuery) -> Ordering {
    let o = match q.sort.key {
        RequestSortKey::CacheKey => a.prompt_cache_key.cmp(&b.prompt_cache_key),
        RequestSortKey::Session => session_key(a).cmp(&session_key(b)),
        RequestSortKey::Status => status_kind(&a.status).0.cmp(status_kind(&b.status).0),
        RequestSortKey::Provider => a
            .member_name
            .cmp(&b.member_name)
            .then_with(|| a.line_name.cmp(&b.line_name)),
        RequestSortKey::Input => fresh(a).cmp(&fresh(b)),
        RequestSortKey::Output => a.output_tokens.cmp(&b.output_tokens),
        RequestSortKey::CacheRead => a.cached_tokens.cmp(&b.cached_tokens),
        RequestSortKey::CacheWrite => a.cache_write_tokens.cmp(&b.cache_write_tokens),
        RequestSortKey::FirstToken => a.first_token_ms.cmp(&b.first_token_ms),
        RequestSortKey::Duration => a.duration_ms.cmp(&b.duration_ms),
        RequestSortKey::Tps => a.tps_milli.cmp(&b.tps_milli),
        RequestSortKey::Cost => a.cost.map(|c| c.micros()).cmp(&b.cost.map(|c| c.micros())),
        _ => a.started_at.cmp(&b.started_at),
    }
    .then_with(|| a.id.cmp(&b.id));
    if q.sort.descending { o.reverse() } else { o }
}

/// Add one row to totals (as the router's SQL totals).
pub fn add_row(t: &mut RequestTotals, r: &RequestRow) {
    t.requests += 1;
    if r.status.is_error() {
        t.errors += 1;
    } else {
        t.ok += 1;
    }
    t.input_tokens += r.input_tokens.unwrap_or(0);
    t.cached_tokens += r.cached_tokens.unwrap_or(0);
    t.cache_write_tokens += r.cache_write_tokens.unwrap_or(0);
    t.output_tokens += r.output_tokens.unwrap_or(0);
    if let Some(c) = r.cost {
        t.cost = superba_core::Usd::from_micros(t.cost.micros().saturating_add(c.micros()));
    } else if r.input_tokens.is_some() {
        t.unpriced += 1;
    }
    t.add_timing(r.first_token_ms, r.tps_milli, r.output_tokens);
}

/// `a + b`.
pub fn add_totals(a: &mut RequestTotals, b: &RequestTotals) {
    a.requests += b.requests;
    a.ok += b.ok;
    a.errors += b.errors;
    a.input_tokens += b.input_tokens;
    a.cached_tokens += b.cached_tokens;
    a.cache_write_tokens += b.cache_write_tokens;
    a.output_tokens += b.output_tokens;
    a.cost = superba_core::Usd::from_micros(a.cost.micros().saturating_add(b.cost.micros()));
    a.unpriced += b.unpriced;
    a.first_token_rows += b.first_token_rows;
    a.first_token_ms_sum += b.first_token_ms_sum;
    a.tps_output_tokens += b.tps_output_tokens;
    a.tps_weighted_milli += b.tps_weighted_milli;
}

/// Source-list groups of rollout rows for `by`.
fn groups_of(rows: &[&RequestRow], by: RequestGrouping) -> Vec<RequestGroup> {
    let mut map: BTreeMap<String, RequestGroup> = BTreeMap::new();
    for r in rows {
        let (key, label) = match by {
            RequestGrouping::Provider => match r.provider_key() {
                Some(k) => (k.to_owned(), r.member_name.clone()),
                None => continue,
            },
            _ => match session_key(r) {
                Some(k) => (k.to_owned(), k.to_owned()),
                None => continue,
            },
        };
        let g = map.entry(key.clone()).or_insert_with(|| RequestGroup {
            key,
            label: label.clone(),
            count: 0,
            last: r.started_at,
            upstream_host: None,
            lines: Vec::new(),
        });
        g.count += 1;
        if r.started_at >= g.last {
            g.last = r.started_at;
            if !label.is_empty() {
                g.label = label;
            }
        }
    }
    map.into_values().collect()
}

/// Router groups plus rollout groups (by key), newest activity first.
fn merge_groups(router: Vec<RequestGroup>, extra: Vec<RequestGroup>) -> Vec<RequestGroup> {
    let mut out = router;
    for e in extra {
        match out.iter_mut().find(|g| g.key == e.key) {
            Some(g) => {
                g.count += e.count;
                g.last = g.last.max(e.last);
            }
            None => out.push(e),
        }
    }
    out.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.key.cmp(&b.key)));
    out.truncate(MAX_GROUPS);
    out
}

/// The router's page merged with unmatched rollout rows: filtered and sorted
/// like the router's rows, totals and groups over both, at most the query's
/// limit of rows.
pub fn merge_page(
    mut page: RequestPage,
    rollout: &[RequestRow],
    q: &RequestQuery,
    rollout_truncated: bool,
) -> RequestPage {
    let limit = match q.limit {
        0 => RequestQuery::DEFAULT_LIMIT,
        n => n,
    } as usize;
    let rows: Vec<&RequestRow> = rollout.iter().filter(|r| matches(r, q, true)).collect();
    let grouped: Vec<&RequestRow> = rollout.iter().filter(|r| matches(r, q, false)).collect();
    if rows.is_empty() && grouped.is_empty() {
        page.truncated |= rollout_truncated;
        return page;
    }
    for r in &rows {
        add_row(&mut page.totals, r);
    }
    page.groups = merge_groups(
        std::mem::take(&mut page.groups),
        groups_of(&grouped, q.group_by),
    );
    let mut all = std::mem::take(&mut page.rows);
    all.extend(rows.into_iter().cloned());
    all.sort_by(|a, b| compare(a, b, q));
    page.truncated |= all.len() > limit || rollout_truncated;
    all.truncate(limit);
    page.rows = all;
    page
}

/// Bucket key and label of a row for statistics.
fn bucket_of(r: &RequestRow, by: StatsGroupBy, offset_ms: i64) -> (String, String) {
    match by {
        StatsGroupBy::Provider => {
            let k = r.provider_key().unwrap_or("").to_owned();
            let l = if r.member_name.is_empty() {
                k.clone()
            } else {
                r.member_name.clone()
            };
            (k, l)
        }
        StatsGroupBy::Day => {
            let k = day_of(r.started_at, offset_ms);
            (k.clone(), k)
        }
        StatsGroupBy::Session => {
            let k = session_key(r).unwrap_or("").to_owned();
            (k.clone(), k)
        }
        _ => {
            let k = r
                .model
                .clone()
                .or_else(|| r.response_model.clone())
                .unwrap_or_default();
            (k.clone(), k)
        }
    }
}

fn day_of(t: Timestamp, offset_ms: i64) -> String {
    superba_router::store::day_label((t.as_millis() + offset_ms).div_euclid(86_400_000))
}

fn merge_buckets(
    router: Vec<StatsBucket>,
    extra: BTreeMap<String, StatsBucket>,
) -> Vec<StatsBucket> {
    let mut out = router;
    for (key, e) in extra {
        match out.iter_mut().find(|b| b.key == key) {
            Some(b) => add_totals(&mut b.totals, &e.totals),
            None => out.push(e),
        }
    }
    out
}

fn by_cost(a: &StatsBucket, b: &StatsBucket) -> Ordering {
    b.totals
        .cost
        .micros()
        .cmp(&a.totals.cost.micros())
        .then_with(|| b.totals.requests.cmp(&a.totals.requests))
        .then_with(|| a.key.cmp(&b.key))
}

/// The router's statistics plus unmatched rollout rows (range and member of the
/// query).
pub fn merge_stats(
    mut stats: RequestStats,
    rollout: &[RequestRow],
    q: &StatsQuery,
) -> RequestStats {
    let offset_ms = i64::from(q.utc_offset_minutes) * 60_000;
    let rows: Vec<&RequestRow> = rollout
        .iter()
        .filter(|r| {
            q.from.is_none_or(|f| r.started_at >= f)
                && q.to.is_none_or(|t| r.started_at < t)
                && q.member.as_ref().is_none_or(|m| &r.member_id == m)
                && q.source.is_none_or(|s| s == r.source)
        })
        .collect();
    if rows.is_empty() {
        return stats;
    }
    let mut buckets: BTreeMap<String, StatsBucket> = BTreeMap::new();
    let mut daily: BTreeMap<String, StatsBucket> = BTreeMap::new();
    let mut newest: BTreeMap<String, Timestamp> = BTreeMap::new();
    for r in &rows {
        add_row(&mut stats.totals, r);
        let (key, label) = bucket_of(r, q.by, offset_ms);
        let b = buckets.entry(key.clone()).or_insert_with(|| StatsBucket {
            key: key.clone(),
            label: label.clone(),
            totals: RequestTotals::default(),
            lines: Vec::new(),
        });
        add_row(&mut b.totals, r);
        let seen = newest.entry(key).or_insert(r.started_at);
        if r.started_at >= *seen {
            *seen = r.started_at;
            b.label = label;
        }
        let day = day_of(r.started_at, offset_ms);
        let d = daily.entry(day.clone()).or_insert_with(|| StatsBucket {
            key: day.clone(),
            label: day,
            totals: RequestTotals::default(),
            lines: Vec::new(),
        });
        add_row(&mut d.totals, r);
    }
    stats.buckets = merge_buckets(std::mem::take(&mut stats.buckets), buckets);
    if q.by == StatsGroupBy::Day {
        stats.buckets.sort_by(|a, b| a.key.cmp(&b.key));
    } else {
        stats.buckets.sort_by(by_cost);
    }
    stats.daily = merge_buckets(std::mem::take(&mut stats.daily), daily);
    stats.daily.sort_by(|a, b| a.key.cmp(&b.key));
    stats
}

/// Add an imported day's totals (requests it stands for, tokens, recorded
/// cost) to `t`.
pub fn add_day(t: &mut RequestTotals, d: &crate::usage_import::ImportedDay) {
    t.requests += d.requests;
    t.ok += d.ok.min(d.requests);
    t.errors += d.requests.saturating_sub(d.ok);
    t.input_tokens += d.input;
    t.cached_tokens += d.cached;
    t.cache_write_tokens += d.cache_write;
    t.output_tokens += d.output;
    match d.cost {
        Some(c) => {
            t.cost = superba_core::Usd::from_micros(t.cost.micros().saturating_add(c.micros()))
        }
        None if d.input > 0 || d.output > 0 => t.unpriced += 1,
        None => {}
    }
}

/// The statistics plus imported daily totals (days the other tool kept only
/// as totals) in the query's range, member and source. A day counts at the
/// local midnight of its date at the query's UTC offset.
pub fn merge_days(
    mut stats: RequestStats,
    days: &[crate::usage_import::ImportedDay],
    q: &StatsQuery,
) -> RequestStats {
    let offset_ms = i64::from(q.utc_offset_minutes) * 60_000;
    let picked: Vec<&crate::usage_import::ImportedDay> = days
        .iter()
        .filter(|d| {
            let Some(start) = crate::usage_import::day_start_ms(&d.day, offset_ms) else {
                return false;
            };
            let at = Timestamp::from_millis(start);
            q.from.is_none_or(|f| at >= f)
                && q.to.is_none_or(|t| at < t)
                && q.member.as_ref().is_none_or(|m| &d.member_id == m)
                && q.source
                    .is_none_or(|s| s == RequestSource::of_import(d.source))
        })
        .collect();
    if picked.is_empty() {
        return stats;
    }
    let mut buckets: BTreeMap<String, StatsBucket> = BTreeMap::new();
    let mut daily: BTreeMap<String, StatsBucket> = BTreeMap::new();
    for d in picked {
        add_day(&mut stats.totals, d);
        let (key, label) = match q.by {
            StatsGroupBy::Provider => (d.member_id.to_string(), d.member_name.clone()),
            StatsGroupBy::Day => (d.day.clone(), d.day.clone()),
            StatsGroupBy::Session => (String::new(), String::new()),
            _ => (d.model.clone(), d.model.clone()),
        };
        let b = buckets.entry(key.clone()).or_insert_with(|| StatsBucket {
            key,
            label,
            totals: RequestTotals::default(),
            lines: Vec::new(),
        });
        add_day(&mut b.totals, d);
        let day = daily.entry(d.day.clone()).or_insert_with(|| StatsBucket {
            key: d.day.clone(),
            label: d.day.clone(),
            totals: RequestTotals::default(),
            lines: Vec::new(),
        });
        add_day(&mut day.totals, d);
    }
    stats.buckets = merge_buckets(std::mem::take(&mut stats.buckets), buckets);
    if q.by == StatsGroupBy::Day {
        stats.buckets.sort_by(|a, b| a.key.cmp(&b.key));
    } else {
        stats.buckets.sort_by(by_cost);
    }
    stats.daily = merge_buckets(std::mem::take(&mut stats.daily), daily);
    stats.daily.sort_by(|a, b| a.key.cmp(&b.key));
    stats
}

// ---------------------------------------------------------------------------
// Service glue
// ---------------------------------------------------------------------------

impl Switcher {
    /// Attribution data (generations, names, prices); missing parts are empty.
    pub(crate) async fn attribution(&self) -> Attribution {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let mut generations = d.config.generations(&home).await.unwrap_or_default();
        generations.sort_by_key(|g| g.applied_at);
        let mut a = Attribution {
            generations,
            prices: self.price_table_impl().await.unwrap_or_default(),
            ..Attribution::default()
        };
        if let Ok(set) = d.profiles.load().await {
            a = a.with_profiles(&set);
        }
        a
    }

    /// Rollout rows started in `[from, to)` that the router did not record,
    /// attributed and priced; `true` when the rollout row cap was reached.
    pub(crate) async fn rollout_rows(
        &self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    ) -> Result<(Vec<RequestRow>, bool)> {
        let index = self.usage.clone();
        let page = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .requests(from, to)
        })
        .await
        .map_err(|e| superba_core::Error::other(format!("rollout usage: {e}")))?;
        if page.rows.is_empty() {
            return Ok((Vec::new(), page.truncated));
        }
        let a = self.attribution().await;
        let rows: Vec<RequestRow> = page.rows.iter().map(|r| a.row(r)).collect();
        let threads: BTreeSet<String> = rows.iter().filter_map(|r| r.thread_id.clone()).collect();
        let lo = rows
            .iter()
            .map(|r| r.started_at)
            .min()
            .map(|t| t.saturating_sub(MATCH_BEFORE));
        let hi = rows
            .iter()
            .map(end_of)
            .max()
            .map(|t| t.saturating_add(MATCH_AFTER + Duration::from_millis(1)));
        let router = self
            .deps
            .router
            .requests_for_threads(lo, hi, threads.into_iter().collect())
            .await
            .unwrap_or_default();
        Ok((dedupe(rows, &router), page.truncated))
    }

    /// The detail of a rollout row (negative id).
    pub(crate) async fn rollout_detail(&self, id: i64) -> Result<RequestDetail> {
        let index = self.usage.clone();
        let found = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .row(id)
        })
        .await
        .map_err(|e| superba_core::Error::other(format!("rollout usage: {e}")))?;
        let Some(r) = found else {
            return Err(superba_core::Error::NotFound {
                what: format!("request {id}"),
            });
        };
        let row = self.attribution().await.row(&r);
        Ok(RequestDetail {
            timing: RequestTiming {
                end_ms: row.duration_ms,
                ..RequestTiming::default()
            },
            row,
            attempts: Vec::new(),
            log: Vec::new(),
        })
    }
}
