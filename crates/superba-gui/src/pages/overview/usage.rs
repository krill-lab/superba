//! Data of the Overview (user request 27): the request log of the last seven
//! days, today's / yesterday's / this month's totals and the router's status,
//! read through the switch service, and the pure mapping from those rows to
//! what the cards show: today's cost and token split with its delta against
//! yesterday, the 7 × 24 usage heat-strip, the most recent sessions, a
//! 30-minute trace per router line and the models without a price.
//!
//! Sources: `SwitchService::router_requests` returns the local router's rows
//! (every tool that goes through it, tagged with its harness) merged with the
//! model requests read from Codex's rollout files and Claude Code's
//! transcripts ([`RequestRow::source`]), so everything but the line traces
//! works without the router. Tools that bypass the router and keep no
//! transcript Superba reads (opencode, pi, dsh, ...) are counted only when
//! routed. Nothing is invented: without rows the cards show their empty
//! states, and an unknown price stays unknown.
//!
//! The snapshot also carries what the request path draws per tool: the
//! harness summaries (route, provider, model) and Claude Code's view.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use superba_core::harness::HarnessSummary;
use superba_core::{
    ClaudeView, RequestClient, RequestHarness, RequestQuery, RequestRow, RequestSource,
    RequestTotals, Result, RouterView, SwitchService, Timestamp,
};

/// Days in the heat-strip (today included).
pub const DAYS: usize = 7;

/// Hours per day in the heat-strip.
pub const HOURS: usize = 24;

/// Minutes a line trace covers.
pub const TRACE_MINUTES: usize = 30;

/// Sessions the recent-sessions card lists.
pub const RECENT_SESSIONS: usize = 5;

/// Rows per read (the router's per-query maximum).
pub const PAGE_ROWS: u32 = 10_000;

/// Reads per snapshot: the seven days are read newest first in pages of
/// [`PAGE_ROWS`]; older rows beyond this many pages are left out (the
/// heat-strip says from when on it is complete).
pub const MAX_PAGES: usize = 4;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// A router line: (member profile id, line id).
pub type LineKey = (String, String);

/// One loaded snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Totals since local midnight.
    pub today: RequestTotals,
    /// Totals of yesterday up to the time of day it is now (the delta's base).
    pub yesterday_same: RequestTotals,
    /// Totals of all of yesterday.
    pub yesterday: RequestTotals,
    /// Totals since the first of the local month.
    pub month: RequestTotals,
    /// Usage per local day and hour.
    pub heat: Heat,
    /// The most recently active sessions, newest first, with their titles.
    pub sessions: Vec<SessionSummary>,
    /// 30-minute traces of the router's lines.
    pub traces: BTreeMap<LineKey, Trace>,
    /// Models with usage but no price in the seven days, most tokens first.
    pub unpriced: Vec<String>,
    /// The router's status (`None` when it could not be read).
    pub router: Option<RouterView>,
    /// Use per tool since local midnight, largest first.
    pub tools_today: Vec<ToolUsage>,
    /// Use per tool over the seven days, largest first (lane order).
    pub tools_week: Vec<ToolUsage>,
    /// The coding tools other than Codex and Claude Code (route, provider,
    /// model); empty when they could not be read.
    pub harnesses: Vec<HarnessSummary>,
    /// Claude Code's configuration (`None` when it could not be read).
    pub claude: Option<ClaudeView>,
    /// When it was read.
    pub at: Timestamp,
    /// Local UTC offset at `at`, seconds.
    pub offset_secs: i64,
}

/// Local midnight of `now` and the first of its month, as UTC timestamps.
pub fn day_and_month_start(now: Timestamp, offset_secs: i64) -> (Timestamp, Timestamp) {
    let offset_ms = offset_secs * 1000;
    let local = Timestamp(now.as_millis() + offset_ms);
    let (y, m, d) = local.utc_date();
    let at = |day| {
        Timestamp::from_utc_date(y, m, day)
            .map(|t| Timestamp(t.as_millis() - offset_ms))
            .unwrap_or(now)
    };
    (at(d), at(1))
}

/// Read a snapshot through the service.
pub async fn load(
    service: Arc<dyn SwitchService>,
    now: Timestamp,
    offset_secs: i64,
) -> Result<Snapshot> {
    load_for(service, now, offset_secs, false).await
}

/// The simple dashboard counts Codex alone, including unrouted rollouts.
pub async fn load_codex(
    service: Arc<dyn SwitchService>,
    now: Timestamp,
    offset_secs: i64,
) -> Result<Snapshot> {
    load_for(service, now, offset_secs, true).await
}

async fn load_for(
    service: Arc<dyn SwitchService>,
    now: Timestamp,
    offset_secs: i64,
    codex_only: bool,
) -> Result<Snapshot> {
    let harnesses = if codex_only {
        vec![RequestHarness::Codex]
    } else {
        Vec::new()
    };
    let (today_start, month_start) = day_and_month_start(now, offset_secs);
    let yesterday_start = Timestamp(today_start.as_millis() - DAY_MS);
    let week_start = Timestamp(today_start.as_millis() - (DAYS as i64 - 1) * DAY_MS);
    // Rows of the seven days, newest first, page by page.
    let mut rows: Vec<RequestRow> = Vec::new();
    let mut cursor: Option<Timestamp> = None;
    let mut complete = true;
    for page_no in 0..MAX_PAGES {
        let page = service
            .router_requests(RequestQuery {
                from: Some(week_start),
                to: cursor,
                limit: PAGE_ROWS,
                harnesses: harnesses.clone(),
                ..RequestQuery::default()
            })
            .await?;
        let oldest = page.rows.last().map(|r| r.started_at);
        rows.extend(page.rows);
        match (page.truncated, oldest) {
            (true, Some(t)) if page_no + 1 < MAX_PAGES => cursor = Some(t),
            (true, _) => {
                complete = false;
                break;
            }
            _ => break,
        }
    }
    let totals = |from: Timestamp, to: Option<Timestamp>| RequestQuery {
        from: Some(from),
        to,
        // Totals only (one row is the smallest page).
        limit: 1,
        harnesses: harnesses.clone(),
        ..RequestQuery::default()
    };
    let today = service.router_requests(totals(today_start, None)).await?;
    let same = Timestamp(now.as_millis() - DAY_MS);
    let yesterday_same = service
        .router_requests(totals(yesterday_start, Some(same)))
        .await?;
    let yesterday = service
        .router_requests(totals(yesterday_start, Some(today_start)))
        .await?;
    let month = service.router_requests(totals(month_start, None)).await?;
    let router = service.router_status().await.ok();
    let mut heat = heat(&rows, now, offset_secs);
    if !complete {
        heat.complete_since = rows.last().map(|r| r.started_at);
    }
    let mut sessions = recent_sessions(&rows, RECENT_SESSIONS);
    // Names come from Codex's state database: only Codex sessions have one.
    let keys: Vec<String> = sessions
        .iter()
        .filter(|s| s.harness == RequestHarness::Codex)
        .map(|s| s.id().to_owned())
        .collect();
    if !keys.is_empty()
        && let Ok(titles) = service.session_titles(keys).await
    {
        for s in &mut sessions {
            if s.harness == RequestHarness::Codex {
                s.title = titles.get(s.id()).map(|t| t.title.clone());
            }
        }
    }
    let harnesses = if codex_only {
        Vec::new()
    } else {
        service.harness_list().await.unwrap_or_default()
    };
    let claude = if codex_only {
        None
    } else {
        service.claude_view().await.ok()
    };
    Ok(Snapshot {
        today: today.totals,
        yesterday_same: yesterday_same.totals,
        yesterday: yesterday.totals,
        month: month.totals,
        heat,
        sessions,
        traces: line_traces(&rows, now),
        unpriced: unpriced_models(&rows),
        router,
        tools_today: tool_usage(&rows, today_start, now),
        tools_week: tool_usage(&rows, week_start, now),
        harnesses,
        claude,
        at: now,
        offset_secs,
    })
}

/// Tokens of a row: input (cached included) plus output.
pub fn row_tokens(row: &RequestRow) -> u64 {
    row.input_tokens.unwrap_or(0) + row.output_tokens.unwrap_or(0)
}

/// The session a row belongs to, qualified by its tool
/// ([`RequestRow::session_group_key`]: a Codex and a Claude Code session never
/// merge).
pub fn session_key(row: &RequestRow) -> Option<String> {
    row.session_group_key()
}

/// A short form of a session key: the first eight characters of its last part.
pub fn short_key(key: &str) -> String {
    let key = superba_core::split_harness_key(key).map_or(key, |(_, id)| id);
    let tail = key.rsplit(':').next().unwrap_or(key);
    tail.chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// Today
// ---------------------------------------------------------------------------

/// What today's big figure measures: the estimated cost when any request of
/// today or yesterday was priced, else the tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// Micro-dollars.
    Cost,
    /// Input + output tokens.
    Tokens,
}

/// Today compared with yesterday at the same time of day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delta {
    /// Nothing to compare (no usage today and yesterday).
    None,
    /// Usage today, none yesterday by this time.
    New,
    /// Change in percent (+18.0 = 18 % more than yesterday by this time).
    Percent(f64),
}

/// The numbers of the 今日 card.
#[derive(Clone, Debug, PartialEq)]
pub struct TodayView {
    /// Cost or tokens.
    pub measure: Measure,
    /// Today's value in the measure (micro-dollars or tokens).
    pub value: i64,
    /// Yesterday by this time, in the measure.
    pub yesterday_same: i64,
    /// All of yesterday, in the measure.
    pub yesterday: i64,
    /// This month, in the measure.
    pub month: i64,
    /// Today vs yesterday by this time.
    pub delta: Delta,
    /// Requests today.
    pub requests: u64,
    /// Failed requests today.
    pub errors: u64,
    /// Requests today with usage but no price.
    pub unpriced: u64,
    /// Cached input tokens today.
    pub cached: u64,
    /// Input tokens not served from the cache (cache writes included).
    pub uncached: u64,
    /// Output tokens today.
    pub output: u64,
}

impl TodayView {
    /// Build from the snapshot's totals.
    pub fn of(
        today: &RequestTotals,
        yesterday_same: &RequestTotals,
        yesterday: &RequestTotals,
        month: &RequestTotals,
    ) -> Self {
        let priced = [today, yesterday_same, yesterday, month]
            .iter()
            .any(|t| t.cost.micros() > 0);
        let measure = if priced {
            Measure::Cost
        } else {
            Measure::Tokens
        };
        let value = |t: &RequestTotals| match measure {
            Measure::Cost => t.cost.micros(),
            Measure::Tokens => (t.input_tokens + t.output_tokens) as i64,
        };
        let (now, base) = (value(today), value(yesterday_same));
        Self {
            measure,
            value: now,
            yesterday_same: base,
            yesterday: value(yesterday),
            month: value(month),
            delta: delta(now, base),
            requests: today.requests,
            errors: today.errors,
            unpriced: today.unpriced,
            cached: today.cached_tokens.min(today.input_tokens),
            uncached: today.input_tokens.saturating_sub(today.cached_tokens),
            output: today.output_tokens,
        }
    }

    /// No request today.
    pub fn is_empty(&self) -> bool {
        self.requests == 0
    }

    /// Share of cached tokens in today's input (0..=1; `None` without input).
    pub fn cache_rate(&self) -> Option<f64> {
        let input = self.cached + self.uncached;
        (input > 0).then(|| self.cached as f64 / input as f64)
    }

    /// The three token parts (cached, uncached, output) as shares of their sum.
    pub fn split(&self) -> [f32; 3] {
        let all = (self.cached + self.uncached + self.output) as f32;
        if all <= 0.0 {
            return [0.0; 3];
        }
        [
            self.cached as f32 / all,
            self.uncached as f32 / all,
            self.output as f32 / all,
        ]
    }
}

/// Today's `now` against yesterday's `base` by the same time of day.
pub fn delta(now: i64, base: i64) -> Delta {
    match (now, base) {
        (0, 0) => Delta::None,
        (_, 0) => Delta::New,
        _ => Delta::Percent((now - base) as f64 * 100.0 / base as f64),
    }
}

// ---------------------------------------------------------------------------
// Heat-strip
// ---------------------------------------------------------------------------

/// Usage of one local hour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeatCell {
    /// Input + output tokens.
    pub tokens: u64,
    /// Requests.
    pub requests: u32,
    /// Known cost, micro-dollars.
    pub cost_micros: i64,
}

/// Usage per local day and hour of the last seven days.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heat {
    /// `cells[day][hour]`: day 0 is six days ago, day 6 today.
    pub cells: Vec<[HeatCell; HOURS]>,
    /// Local midnight of each day (as a UTC timestamp).
    pub days: Vec<Timestamp>,
    /// The local hour it is now (today's later cells lie in the future).
    pub now_hour: usize,
    /// Rows older than this were not read (more rows than [`MAX_PAGES`] pages).
    pub complete_since: Option<Timestamp>,
}

impl Heat {
    /// The busiest hour's tokens.
    pub fn max_tokens(&self) -> u64 {
        self.cells
            .iter()
            .flat_map(|d| d.iter())
            .map(|c| c.tokens)
            .max()
            .unwrap_or(0)
    }

    /// Shade of a cell, 0 (no use) to 4 (the busiest hours): the square root of
    /// its share of the busiest hour, so light use stays visible.
    pub fn level(&self, cell: &HeatCell) -> u8 {
        let max = self.max_tokens();
        if cell.tokens == 0 || max == 0 {
            return 0;
        }
        let share = (cell.tokens as f64 / max as f64).sqrt();
        (share * 4.0).ceil().clamp(1.0, 4.0) as u8
    }

    /// No request in the seven days.
    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|d| d.iter().all(|c| c.requests == 0))
    }

    /// Day of the week of day `i` (0 = Monday).
    pub fn weekday(&self, i: usize, offset_secs: i64) -> usize {
        let local = self
            .days
            .get(i)
            .map_or(0, |t| t.as_millis() + offset_secs * 1000);
        // 1970-01-01 was a Thursday (3 counted from Monday).
        (local.div_euclid(DAY_MS) + 3).rem_euclid(7) as usize
    }
}

/// The [`Heat`] of `rows` for the seven local days ending today.
pub fn heat(rows: &[RequestRow], now: Timestamp, offset_secs: i64) -> Heat {
    let (today_start, _) = day_and_month_start(now, offset_secs);
    let first = today_start.as_millis() - (DAYS as i64 - 1) * DAY_MS;
    let mut cells = vec![[HeatCell::default(); HOURS]; DAYS];
    for row in rows {
        let t = row.started_at.as_millis() - first;
        if t < 0 || row.started_at > now {
            continue;
        }
        let (day, hour) = ((t / DAY_MS) as usize, ((t % DAY_MS) / HOUR_MS) as usize);
        let Some(cell) = cells.get_mut(day).and_then(|d| d.get_mut(hour)) else {
            continue;
        };
        cell.tokens += row_tokens(row);
        cell.requests += 1;
        cell.cost_micros += row.cost.map_or(0, |c| c.micros());
    }
    let now_hour = ((now.as_millis() - today_start.as_millis()).max(0) / HOUR_MS) as usize;
    Heat {
        cells,
        days: (0..DAYS as i64)
            .map(|i| Timestamp(first + i * DAY_MS))
            .collect(),
        now_hour: now_hour.min(HOURS - 1),
        complete_since: None,
    }
}

// ---------------------------------------------------------------------------
// Recent sessions
// ---------------------------------------------------------------------------

/// One row of the recent-sessions card.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    /// Session key (the Usage page's session group).
    pub key: String,
    /// Name from Codex's state database, when known.
    pub title: Option<String>,
    /// When its newest request ended (or started, without a duration).
    pub last: Timestamp,
    /// Input + output tokens over the seven days.
    pub tokens: u64,
    /// Known cost over the seven days (`None` when no request was priced).
    pub cost_micros: Option<i64>,
    /// Some request had usage but no price.
    pub unpriced: bool,
    /// Model of the newest request.
    pub model: Option<String>,
    /// Client of the newest request.
    pub client: RequestClient,
    /// The tool.
    pub harness: RequestHarness,
    /// Requests.
    pub requests: u64,
}

impl SessionSummary {
    /// The tool's own session id (the key without its tool).
    pub fn id(&self) -> &str {
        superba_core::split_harness_key(&self.key).map_or(self.key.as_str(), |(_, id)| id)
    }

    /// The name to show: the title, else the short key.
    pub fn label(&self) -> String {
        self.title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| short_key(&self.key))
    }
}

/// The `n` sessions whose last request is newest, newest first.
pub fn recent_sessions(rows: &[RequestRow], n: usize) -> Vec<SessionSummary> {
    let mut map: BTreeMap<String, (SessionSummary, Timestamp)> = BTreeMap::new();
    for row in rows {
        let Some(key) = session_key(row) else {
            continue;
        };
        let end = row.duration_ms.map_or(row.started_at, |d| {
            row.started_at.saturating_add(Duration::from_millis(d))
        });
        let (s, newest) = map.entry(key.clone()).or_insert_with(|| {
            (
                SessionSummary {
                    key,
                    title: None,
                    last: end,
                    tokens: 0,
                    cost_micros: None,
                    unpriced: false,
                    model: row.model.clone(),
                    client: row.client,
                    harness: row.harness,
                    requests: 0,
                },
                row.started_at,
            )
        });
        s.requests += 1;
        s.tokens += row_tokens(row);
        match row.cost {
            Some(c) => *s.cost_micros.get_or_insert(0) += c.micros(),
            None if row.input_tokens.is_some() => s.unpriced = true,
            None => {}
        }
        s.last = s.last.max(end);
        if row.started_at > *newest {
            *newest = row.started_at;
            if row.model.is_some() {
                s.model = row.model.clone();
            }
            s.client = row.client;
        }
    }
    let mut out: Vec<SessionSummary> = map.into_values().map(|(s, _)| s).collect();
    out.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.key.cmp(&b.key)));
    out.truncate(n);
    out
}

// ---------------------------------------------------------------------------
// Line traces
// ---------------------------------------------------------------------------

/// The last [`TRACE_MINUTES`] minutes of one router line: time to the response
/// headers of the requests it answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trace {
    /// Median per minute, oldest first (`None`: no answered request that minute).
    pub points: Vec<Option<u64>>,
    /// Answered requests.
    pub samples: u32,
    /// Failed requests on the line.
    pub errors: u32,
    /// Median over the whole window.
    pub median_ms: Option<u64>,
}

fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

/// Traces of every (member, line) the router used in the last
/// [`TRACE_MINUTES`] minutes. Rows read from Codex's rollouts carry no line
/// and no timing, so without the router there are none.
pub fn line_traces(rows: &[RequestRow], now: Timestamp) -> BTreeMap<LineKey, Trace> {
    let window = TRACE_MINUTES as i64 * 60_000;
    // Per line: the time to headers of each minute's answered requests, and
    // the failed requests.
    let mut minutes: BTreeMap<LineKey, (Vec<Vec<u64>>, u32)> = BTreeMap::new();
    for row in rows {
        let age = now.as_millis() - row.started_at.as_millis();
        if row.source != RequestSource::Router || !(0..window).contains(&age) {
            continue;
        }
        let Some(line) = row.line_id.as_deref().filter(|l| !l.is_empty()) else {
            continue;
        };
        let entry = minutes
            .entry((row.member_id.to_string(), line.to_owned()))
            .or_insert_with(|| (vec![Vec::new(); TRACE_MINUTES], 0));
        if row.status.is_error() {
            entry.1 += 1;
            continue;
        }
        if let Some(ttfb) = row.ttfb_ms {
            let i = TRACE_MINUTES - 1 - (age / 60_000) as usize;
            entry.0[i].push(ttfb);
        }
    }
    minutes
        .into_iter()
        .map(|(k, (mut per_minute, errors))| {
            let mut all: Vec<u64> = per_minute.iter().flatten().copied().collect();
            let trace = Trace {
                samples: all.len() as u32,
                errors,
                median_ms: median(&mut all),
                points: per_minute.iter_mut().map(|m| median(m)).collect(),
            };
            (k, trace)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Prices
// ---------------------------------------------------------------------------

/// Models whose requests had usage but no price, most tokens first.
pub fn unpriced_models(rows: &[RequestRow]) -> Vec<String> {
    let mut tokens: BTreeMap<&str, u64> = BTreeMap::new();
    for row in rows {
        if row.cost.is_none()
            && row.input_tokens.is_some()
            && let Some(model) = row.model.as_deref().filter(|m| !m.is_empty())
        {
            *tokens.entry(model).or_default() += row_tokens(row);
        }
    }
    let mut out: Vec<(&str, u64)> = tokens.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    out.into_iter().map(|(m, _)| m.to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Per tool
// ---------------------------------------------------------------------------

/// One tool's use over a period.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolUsage {
    /// The tool.
    pub harness: RequestHarness,
    /// Requests.
    pub requests: u64,
    /// Input + output tokens.
    pub tokens: u64,
    /// Known cost, micro-dollars (only meaningful when `priced`).
    pub cost_micros: i64,
    /// Some request had a price.
    pub priced: bool,
    /// Requests with usage but no price.
    pub unpriced: u64,
}

impl ToolUsage {
    /// The known cost; `None` when no request had a price (never zero for
    /// unknown).
    pub fn cost(&self) -> Option<i64> {
        self.priced.then_some(self.cost_micros)
    }
}

/// Use per tool of the rows started in `[from, to]`, largest first: by cost
/// when any tool is priced, then by tokens, then by requests; ties keep the
/// tools' display order.
pub fn tool_usage(rows: &[RequestRow], from: Timestamp, to: Timestamp) -> Vec<ToolUsage> {
    let mut map: BTreeMap<RequestHarness, ToolUsage> = BTreeMap::new();
    for row in rows {
        if row.started_at < from || row.started_at > to {
            continue;
        }
        let t = map.entry(row.harness).or_insert_with(|| ToolUsage {
            harness: row.harness,
            ..ToolUsage::default()
        });
        t.requests += 1;
        t.tokens += row_tokens(row);
        match row.cost {
            Some(c) => {
                t.cost_micros += c.micros();
                t.priced = true;
            }
            None if row.input_tokens.is_some() => t.unpriced += 1,
            None => {}
        }
    }
    let mut out: Vec<ToolUsage> = map.into_values().collect();
    let order = |h: RequestHarness| RequestHarness::ALL.iter().position(|x| *x == h);
    out.sort_by(|a, b| {
        b.cost()
            .unwrap_or(-1)
            .cmp(&a.cost().unwrap_or(-1))
            .then(b.tokens.cmp(&a.tokens))
            .then(b.requests.cmp(&a.requests))
            .then(order(a.harness).cmp(&order(b.harness)))
    });
    out
}
