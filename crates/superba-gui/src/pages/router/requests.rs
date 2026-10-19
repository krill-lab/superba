//! View model of the request log (Usage → 最近请求 / 活动请求 / 统计):
//! the request columns in display order, cell texts, the totals row, search over
//! session names, ranges, sorting and CSV export. Pure functions (unit tested);
//! drawing lives in `recent.rs`, `active.rs` and `stats.rs`.
//!
//! Token semantics follow the router (OpenAI): `input_tokens` includes cached and
//! cache-write tokens, so the 输入 column shows the non-cached part; unknown
//! values are `—`, never 0. Costs are estimates (`≈`).

use std::collections::BTreeMap;
use std::time::Duration;

use superba_core::{
    InflightPhase, InflightRequest, ProfileSet, RequestClient, RequestGroup, RequestGrouping,
    RequestHarness, RequestKind, RequestPage, RequestRow, RequestSortKey, RequestSource,
    RequestStatus, RequestTotals, SessionTitle, Site, SpeedTier, StatsBucket, Timestamp,
    TitleSource, Usd, provider_line_key, split_provider_key,
};
use superba_router::members::router_id;

use crate::design::widgets::Status;
use crate::design::widgets::log_table::{LogCell, LogColumn};
use crate::design::widgets::source_list::SourceEntry;
use crate::i18n::Tr;
use crate::pages::shared::format::{compact, group_thousands};

/// Placeholder for an unknown value.
pub const DASH: &str = "—";

/// Time range of the log and the statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Range {
    /// Last 24 hours.
    #[default]
    Day,
    /// Last 7 days.
    Week,
    /// Last 30 days.
    Month,
}

impl Range {
    /// All ranges in display order.
    pub const ALL: [Range; 3] = [Range::Day, Range::Week, Range::Month];

    /// Length.
    pub fn duration(self) -> Duration {
        Duration::from_secs(match self {
            Range::Day => 86_400,
            Range::Week => 7 * 86_400,
            Range::Month => 30 * 86_400,
        })
    }

    /// Start of the range ending at `now`.
    pub fn from(self, now: Timestamp) -> Timestamp {
        now.saturating_sub(self.duration())
    }

    /// i18n key of the label.
    pub fn key(self) -> &'static str {
        match self {
            Range::Day => "requests.range.24h",
            Range::Week => "requests.range.7d",
            Range::Month => "requests.range.30d",
        }
    }
}

/// Columns of 最近请求, in the user's order (feedback 9/10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Col {
    /// `prompt_cache_key`.
    CacheKey,
    /// Start time.
    Date,
    /// Tool that sent the request.
    Harness,
    /// Requested and served models.
    Model,
    /// Session name.
    Session,
    /// Status.
    Status,
    /// Provider · line.
    Provider,
    /// Non-cached input.
    Input,
    /// Output.
    Output,
    /// Cache read.
    CacheRead,
    /// Cache write.
    CacheWrite,
    /// First token.
    FirstToken,
    /// Completed in.
    Duration,
    /// Tokens per second.
    Tps,
    /// Estimated cost.
    Cost,
}

impl Col {
    /// The columns in display order.
    pub const ALL: [Col; 15] = [
        Col::CacheKey,
        Col::Date,
        Col::Harness,
        Col::Model,
        Col::Session,
        Col::Status,
        Col::Provider,
        Col::Input,
        Col::Output,
        Col::CacheRead,
        Col::CacheWrite,
        Col::FirstToken,
        Col::Duration,
        Col::Tps,
        Col::Cost,
    ];

    /// i18n key of the header.
    pub fn key(self) -> &'static str {
        match self {
            Col::CacheKey => "requests.col.cache_key",
            Col::Date => "requests.col.date",
            Col::Harness => "requests.col.harness",
            Col::Model => "requests.col.model",
            Col::Session => "requests.col.session",
            Col::Status => "requests.col.status",
            Col::Provider => "requests.col.provider",
            Col::Input => "requests.col.input",
            Col::Output => "requests.col.output",
            Col::CacheRead => "requests.col.cache_read",
            Col::CacheWrite => "requests.col.cache_write",
            Col::FirstToken => "requests.col.ttft",
            Col::Duration => "requests.col.duration",
            Col::Tps => "requests.col.tps",
            Col::Cost => "requests.col.cost",
        }
    }

    /// Preferred width (UX §3.5; the 834 px the table gets at 1280×800):
    /// a full `MM-DD HH:MM`, a six-digit token count, the status label with its
    /// dot and the Chinese headers fit (review ux-visual #3 / #13).
    pub fn width(self) -> f32 {
        match self {
            Col::CacheKey => 76.0,
            Col::Date => 80.0,
            Col::Harness => 112.0,
            Col::Model => 170.0,
            Col::Session => 94.0,
            Col::Status => 72.0,
            Col::Provider => 94.0,
            Col::Input => 56.0,
            Col::Output => 52.0,
            Col::CacheRead => 58.0,
            Col::CacheWrite => 58.0,
            Col::FirstToken => 44.0,
            Col::Duration => 58.0,
            Col::Tps => 36.0,
            Col::Cost => 56.0,
        }
    }

    /// Narrowest useful width: numbers keep their digits (a narrower table
    /// hides columns instead, see [`Col::drop_rank`]).
    pub fn min_width(self) -> f32 {
        match self {
            Col::CacheKey => 76.0,
            Col::Date => 62.0,
            Col::Harness => 100.0,
            Col::Model => 112.0,
            Col::Session => 72.0,
            Col::Status => 60.0,
            Col::Provider => 72.0,
            Col::Input => 56.0,
            Col::Output => 52.0,
            Col::CacheRead => 56.0,
            Col::CacheWrite => 50.0,
            Col::FirstToken => 42.0,
            Col::Duration => 58.0,
            Col::Tps => 34.0,
            Col::Cost => 54.0,
        }
    }

    /// Share of the room beyond the preferred widths: the session name first,
    /// then the status (with its reason).
    pub fn grow(self) -> f32 {
        match self {
            Col::Model => 3.0,
            Col::Session => 2.0,
            Col::Status => 1.0,
            _ => 0.0,
        }
    }

    /// Order in which a narrow table hides columns (the 960 px window): cache
    /// write, cache key, TPS, cache read; `None` = always shown.
    pub fn drop_rank(self) -> Option<u8> {
        match self {
            Col::CacheWrite => Some(0),
            Col::CacheKey => Some(1),
            Col::Tps => Some(2),
            Col::CacheRead => Some(3),
            Col::Duration => Some(4),
            Col::FirstToken => Some(5),
            Col::Provider => Some(6),
            Col::Session => Some(7),
            Col::Date => Some(8),
            _ => None,
        }
    }

    /// Numbers are right-aligned.
    pub fn numeric(self) -> bool {
        matches!(
            self,
            Col::Input
                | Col::Output
                | Col::CacheRead
                | Col::CacheWrite
                | Col::FirstToken
                | Col::Duration
                | Col::Tps
                | Col::Cost
        )
    }

    /// Sort key of the column.
    pub fn sort_key(self) -> RequestSortKey {
        match self {
            Col::CacheKey => RequestSortKey::CacheKey,
            Col::Date => RequestSortKey::Time,
            Col::Harness => RequestSortKey::Harness,
            Col::Model => RequestSortKey::Model,
            Col::Session => RequestSortKey::Session,
            Col::Status => RequestSortKey::Status,
            Col::Provider => RequestSortKey::Provider,
            Col::Input => RequestSortKey::Input,
            Col::Output => RequestSortKey::Output,
            Col::CacheRead => RequestSortKey::CacheRead,
            Col::CacheWrite => RequestSortKey::CacheWrite,
            Col::FirstToken => RequestSortKey::FirstToken,
            Col::Duration => RequestSortKey::Duration,
            Col::Tps => RequestSortKey::Tps,
            Col::Cost => RequestSortKey::Cost,
        }
    }

    /// The column sorting by `key`.
    pub fn of_sort_key(key: RequestSortKey) -> Option<Col> {
        Col::ALL.into_iter().find(|c| c.sort_key() == key)
    }
}

/// Header cells of the 13 columns.
pub fn columns(tr: &Tr) -> Vec<LogColumn> {
    Col::ALL
        .iter()
        .map(|c| {
            let label = tr.t(c.key());
            let mut col = if c.numeric() {
                LogColumn::right(label, c.width())
            } else {
                LogColumn::left(label, c.width())
            }
            .min(c.min_width())
            .grow(c.grow());
            if let Some(rank) = c.drop_rank() {
                col = col.drop_rank(rank);
            }
            match c {
                Col::Tps => col.tooltip(tr.t("requests.tip.tps")),
                Col::Input => col.tooltip(tr.t("requests.tip.input_col")),
                Col::Cost => col.tooltip(tr.t("requests.tip.cost")),
                Col::CacheRead => col.tooltip(tr.t("requests.tip.cache_read")),
                Col::CacheWrite => col.tooltip(tr.t("requests.tip.cache_write")),
                Col::FirstToken => col.tooltip(tr.t("requests.tip.ttft")),
                Col::Duration => col.tooltip(tr.t("requests.tip.duration")),
                _ => col,
            }
        })
        .collect()
}

/// `12,345`, or `—` when unknown.
pub fn tokens(n: Option<u64>) -> String {
    n.map_or_else(|| DASH.to_owned(), group_thousands)
}

/// `0.82s`, `17.9s`, `2m 05s`, or `—`.
pub fn seconds(ms: Option<u64>) -> String {
    let Some(ms) = ms else {
        return DASH.to_owned();
    };
    if ms < 10_000 {
        format!("{:.2}s", ms as f64 / 1000.0)
    } else if ms < 100_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let s = ms / 1000;
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

/// `87` tok/s from the stored milli value, or `—`.
pub fn tps(milli: Option<u64>) -> String {
    match milli {
        Some(m) if m >= 10_000 => format!("{}", (m + 500) / 1000),
        Some(m) => format!("{:.1}", m as f64 / 1000.0),
        None => DASH.to_owned(),
    }
}

/// A table cost: `≈0.019`, `≈12.84`, `≈<0.001`, or `—`.
pub fn cost_cell(cost: Option<Usd>) -> String {
    match cost {
        None => DASH.to_owned(),
        Some(c) if c.is_zero() => "0".to_owned(),
        Some(c) if c.micros().unsigned_abs() < 500 => "≈<0.001".to_owned(),
        Some(c) if c.micros().unsigned_abs() < 1_000_000 => format!("≈{}", c.amount(3)),
        Some(c) => format!("≈{}", c.amount(2)),
    }
}

/// A cost with the currency: `≈ $0.019`, or `—`.
pub fn cost_text(cost: Option<Usd>) -> String {
    match cost {
        None => DASH.to_owned(),
        Some(c) => format!("≈ ${}", c.amount(c.display_decimals().clamp(3, 4))),
    }
}

/// The local UTC offset at `t` in seconds (`None` = unknown, shown in UTC).
pub fn local_offset_secs(t: Timestamp) -> Option<i64> {
    superba_switch::localtime::offset_secs(t)
}

/// `t` in local time as RFC 3339 without the zone (`2026-10-17T14:32:07`).
fn local_rfc3339(t: Timestamp) -> String {
    let offset = local_offset_secs(t).unwrap_or(0);
    Timestamp::from_millis(t.as_millis() + offset * 1000).to_rfc3339()
}

/// The 日期 cell: `HH:MM:SS` for today, `MM-DD HH:MM` otherwise (local time).
pub fn date_cell(t: Timestamp, now: Timestamp) -> String {
    let s = local_rfc3339(t);
    let today = local_rfc3339(now);
    if s.get(..10) == today.get(..10) {
        s.get(11..19).unwrap_or(&s).to_owned()
    } else {
        format!(
            "{} {}",
            s.get(5..10).unwrap_or(""),
            s.get(11..16).unwrap_or("")
        )
    }
}

/// Full local timestamp for tooltips: `2026-10-17 14:32:07`.
pub fn full_time(t: Timestamp) -> String {
    let s = local_rfc3339(t);
    format!(
        "{} {}",
        s.get(..10).unwrap_or(""),
        s.get(11..19).unwrap_or("")
    )
}

/// Local calendar day `YYYY-MM-DD` of `t`.
pub fn local_day(t: Timestamp) -> String {
    local_rfc3339(t).get(..10).unwrap_or("").to_owned()
}

/// The 缓存键 cell: `019a7c…e1` (first 6 and last 2 characters).
pub fn short_cache_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return key.to_owned();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 2..].iter().collect();
    format!("{head}…{tail}")
}

/// Session key of a row (as the router groups it).
pub fn session_key(row: &RequestRow) -> Option<&str> {
    row.session_key()
}

/// Status of a row: severity and the short label of the 状态 cell (完成, 失败,
/// `429`, …; never a wire code).
pub fn status_cell(tr: &Tr, row: &RequestRow) -> (Status, String) {
    match &row.status {
        RequestStatus::Ok if row.failovers > 0 => (Status::Warn, tr.t("requests.status.failover")),
        RequestStatus::Ok => (Status::Ok, tr.t("requests.status.ok")),
        RequestStatus::Http { status, .. } => (Status::Error, status.to_string()),
        RequestStatus::Failed { .. } => (Status::Error, tr.t("requests.status.failed")),
        RequestStatus::Incomplete { .. } => (Status::Warn, tr.t("requests.status.incomplete")),
        RequestStatus::Broken => (Status::Error, tr.t("requests.status.broken")),
        RequestStatus::Cancelled => (Status::Idle, tr.t("requests.status.cancelled")),
        _ => (Status::Idle, tr.text(&row.status.text())),
    }
}

/// The error code of a row's status (`response_stream_disconnected`,
/// `rate_limit_exceeded`, an incomplete reason), when there is one.
pub fn status_code(row: &RequestRow) -> Option<&str> {
    match &row.status {
        RequestStatus::Http { code, .. } | RequestStatus::Failed { code } => code.as_deref(),
        RequestStatus::Incomplete { reason } => reason.as_deref(),
        _ => None,
    }
    .filter(|c| !c.is_empty())
}

/// A short localized reason for a failure: from the error code where it is a
/// known one, else from the HTTP status.
pub fn status_reason(tr: &Tr, row: &RequestRow) -> Option<String> {
    let by_code = status_code(row).and_then(|c| match c {
        "response_stream_disconnected" => Some("disconnected"),
        "response_stream_connection_failed" | "http_connection_failed" => Some("connection"),
        "context_window_exceeded" | "context_length_exceeded" => Some("context"),
        "usage_limit_exceeded" | "insufficient_quota" => Some("quota"),
        "rate_limit_exceeded" | "rate_limited" => Some("rate_limit"),
        "internal_server_error" | "server_error" => Some("server"),
        "server_is_overloaded" | "overloaded" | "service_unavailable" => Some("overloaded"),
        "unauthorized" | "invalid_api_key" => Some("unauthorized"),
        "bad_request" | "invalid_request_error" | "invalid_prompt" => Some("bad_request"),
        "response_too_many_failed_attempts" => Some("retries"),
        "router_unavailable" => Some("no_member"),
        "request_too_large" => Some("too_large"),
        "max_output_tokens" => Some("max_tokens"),
        "content_filter" => Some("content_filter"),
        _ => None,
    });
    let by_status = || match row.status {
        RequestStatus::Http { status, .. } => match status {
            400 => Some("bad_request"),
            401 | 403 => Some("unauthorized"),
            413 => Some("too_large"),
            429 => Some("rate_limit"),
            500 => Some("server"),
            502..=504 => Some("upstream"),
            _ => None,
        },
        _ => None,
    };
    by_code
        .or_else(by_status)
        .map(|k| tr.t(&format!("requests.reason.{k}")))
}

/// The status in full for tooltips, the detail pane and CSV: label, reason
/// and the wire code (`失败 · 断流 · response_stream_disconnected`).
pub fn status_detail(tr: &Tr, row: &RequestRow) -> String {
    let label = match &row.status {
        RequestStatus::Http { status, .. } => format!("HTTP {status}"),
        _ => status_cell(tr, row).1,
    };
    let mut parts = vec![label];
    parts.extend(status_reason(tr, row));
    parts.extend(status_code(row).map(str::to_owned));
    parts.join(" · ")
}

/// A wire `service_tier` by its display name: `priority` / `fast` → Fast,
/// `default` / `auto` → 标准, `ultrafast` → Ultrafast, `flex` → Flex.
pub fn tier_name(tr: &Tr, wire: &str) -> String {
    match wire {
        "auto" => tr.text(&SpeedTier::Standard.text()),
        "flex" => "Flex".to_owned(),
        w => SpeedTier::from_config_value(w).map_or_else(|| w.to_owned(), |t| tr.text(&t.text())),
    }
}

/// The speed tier line of the detail pane (ux-visual #9): `Fast` (asked and
/// answered alike), `Fast · 实际 标准`, or `None` for a plain Standard request
/// (no tier sent, Standard or nothing answered).
pub fn tier_text(tr: &Tr, row: &RequestRow) -> Option<String> {
    let standard = tr.text(&SpeedTier::Standard.text());
    let requested = row
        .service_tier_requested
        .as_deref()
        .map_or_else(|| standard.clone(), |w| tier_name(tr, w));
    let answered = row
        .service_tier_answered
        .as_deref()
        .map(|w| tier_name(tr, w));
    match answered {
        Some(a) if a == requested && a == standard => None,
        Some(a) if a == requested => Some(a),
        Some(a) => Some(tr.f(
            "requests.detail.tier",
            &[("requested", &requested), ("answered", &a)],
        )),
        None if requested == standard => None,
        None => Some(requested),
    }
}

/// `Krill · 海外线路` (member and line), or the member alone.
pub fn provider_cell(row: &RequestRow) -> String {
    let member = if row.member_name.is_empty() {
        row.member_id.to_string()
    } else {
        row.member_name.clone()
    };
    match row.line_name.as_deref().or(row.line_id.as_deref()) {
        Some(line) if !line.is_empty() => format!("{member} · {line}"),
        // Imported rows name the tool they come from.
        _ => match row.source.imported_from() {
            Some(tool) => format!("{member} · {}", tool.name()),
            None => member,
        },
    }
}

/// Display names of the providers in the request log and of their lines: the
/// current name of the profile a row was recorded for (the router member id is
/// the profile id as a router id) and the name of the line in that profile's
/// site, in the UI language. Rows of a provider that no longer exists keep the
/// names stored with them.
#[derive(Clone, Debug, Default)]
pub struct ProviderNames {
    providers: Vec<ProviderName>,
}

#[derive(Clone, Debug)]
struct ProviderName {
    member: String,
    name: String,
    /// Router line id, stored (fallback) name, display name.
    lines: Vec<(String, String, String)>,
}

impl ProviderName {
    fn line(&self, line: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|(id, fallback, shown)| id == line || fallback == line || shown == line)
            .map(|(_, _, shown)| shown.as_str())
    }
}

impl ProviderNames {
    /// Names of the profiles in `profiles` over `sites`.
    pub fn new(tr: &Tr, profiles: Option<&ProfileSet>, sites: &[Site]) -> Self {
        let providers = profiles
            .map(|set| {
                set.profiles
                    .iter()
                    .map(|p| {
                        let site = sites.iter().find(|s| s.id == p.site_id);
                        let name = if p.name.is_empty() {
                            site.map(|s| tr.text(&s.name))
                                .unwrap_or_else(|| p.id.to_string())
                        } else {
                            p.name.clone()
                        };
                        let lines = site
                            .map(|s| {
                                s.lines
                                    .iter()
                                    .map(|l| {
                                        (
                                            router_id(l.id.as_str()),
                                            l.name.fallback.clone(),
                                            tr.text(&l.name),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        ProviderName {
                            member: router_id(p.id.as_str()),
                            name,
                            lines,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { providers }
    }

    /// A signature of what the names depend on (UI language, profiles and
    /// sites): rows labelled before it changed are read again.
    pub fn signature(tr: &Tr, profiles: Option<&ProfileSet>, sites: &[Site]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        tr.locale().hash(&mut h);
        for p in profiles.map(|s| &s.profiles[..]).unwrap_or_default() {
            (p.id.as_str(), p.name.as_str(), p.site_id.as_str()).hash(&mut h);
        }
        for s in sites {
            (s.id.as_str(), s.lines.len()).hash(&mut h);
        }
        h.finish()
    }

    fn find(&self, member: &str) -> Option<&ProviderName> {
        self.providers.iter().find(|p| p.member == member)
    }

    /// Name of provider `key` (a router member id), else `stored`.
    pub fn provider(&self, key: &str, stored: &str) -> String {
        self.find(key)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| stored.to_owned())
    }

    /// Name of `line` (a line id or a stored line name) of provider `key`, else
    /// `line` itself.
    pub fn line(&self, key: &str, line: &str) -> String {
        self.find(key)
            .and_then(|p| p.line(line))
            .unwrap_or(line)
            .to_owned()
    }

    /// `row` with the current provider and line names.
    pub fn relabel_row(&self, row: &mut RequestRow) {
        let Some(p) = self.find(row.member_id.as_str()) else {
            return;
        };
        row.member_name = p.name.clone();
        let line = row
            .line_id
            .as_deref()
            .and_then(|id| p.line(id))
            .or_else(|| row.line_name.as_deref().and_then(|n| p.line(n)));
        if let Some(line) = line {
            row.line_name = Some(line.to_owned());
        }
    }

    /// Provider groups (and their lines) with the current names.
    pub fn relabel_groups(&self, groups: &mut [RequestGroup]) {
        for g in groups {
            g.label = self.provider(&g.key, &g.label);
            for l in &mut g.lines {
                let (provider, line) = split_provider_key(&l.key);
                l.label = self.line(provider, line.unwrap_or(&l.label));
            }
        }
    }

    /// Provider buckets (and their lines) with the current names.
    pub fn relabel_buckets(&self, buckets: &mut [StatsBucket]) {
        for b in buckets {
            if !b.key.is_empty() {
                b.label = self.provider(&b.key, &b.label);
            }
            for l in &mut b.lines {
                let (provider, line) = split_provider_key(&l.key);
                l.label = self.line(provider, line.unwrap_or(&l.label));
            }
        }
    }

    /// A page with the current names in its rows and, when it is grouped by
    /// provider, its groups.
    pub fn relabel_page(&self, page: &mut RequestPage, by_provider: bool) {
        if self.providers.is_empty() {
            return;
        }
        for row in &mut page.rows {
            self.relabel_row(row);
        }
        if by_provider {
            self.relabel_groups(&mut page.groups);
        }
    }
}

/// Entries of the source list for `groups` of `grouping`: sessions by name;
/// providers by name, followed by their lines as indented sub-entries when a
/// provider used more than one line (a single line is only in the 供应商 ·
/// 线路 cell). The sub-entries add up to the provider: rows without a known
/// line (from Codex rollouts, or failed before a line answered) get a 线路未知
/// entry of their own (review ux-visual #17). Tooltips name the upstream host
/// and the last request.
pub fn source_entries(
    tr: &Tr,
    grouping: RequestGrouping,
    groups: &[RequestGroup],
    titles: &BTreeMap<String, SessionTitle>,
) -> Vec<SourceEntry> {
    let tip = |parts: &[Option<&str>], last: Timestamp| {
        let mut out: Vec<&str> = parts.iter().flatten().copied().collect();
        let time = full_time(last);
        out.push(&time);
        out.join(" · ")
    };
    let mut entries = Vec::new();
    for g in groups {
        if grouping != RequestGrouping::Provider {
            entries.push(SourceEntry {
                label: match grouping {
                    RequestGrouping::Harness => harness_label(tr, &g.key),
                    RequestGrouping::HarnessModel => harness_model_label(tr, &g.key),
                    _ => session_group_label(tr, titles, &g.key),
                },
                tooltip: Some(tip(&[Some(&g.key)], g.last)),
                key: g.key.clone(),
                count: g.count,
                child: false,
            });
            continue;
        }
        let unknown = g
            .count
            .saturating_sub(g.lines.iter().map(|l| l.count).sum::<u64>());
        let split = g.lines.len() > 1 || (!g.lines.is_empty() && unknown > 0);
        let lines = if split { &g.lines[..] } else { &[] };
        let host = if lines.is_empty() {
            g.upstream_host.as_deref()
        } else {
            None
        };
        entries.push(SourceEntry {
            label: g.label.clone(),
            tooltip: Some(tip(&[Some(&g.label), host], g.last)),
            key: g.key.clone(),
            count: g.count,
            child: false,
        });
        entries.extend(lines.iter().map(|l| SourceEntry {
            label: l.label.clone(),
            tooltip: Some(tip(
                &[Some(&g.label), Some(&l.label), l.upstream_host.as_deref()],
                l.last,
            )),
            key: l.key.clone(),
            count: l.count,
            child: true,
        }));
        if split && unknown > 0 {
            let label = tr.t("requests.group.line_unknown");
            entries.push(SourceEntry {
                tooltip: Some(format!(
                    "{} · {label} · {}",
                    g.label,
                    tr.t("requests.group.line_unknown_tip")
                )),
                label,
                key: provider_line_key(&g.key, ""),
                count: unknown,
                child: true,
            });
        }
    }
    entries
}

/// Tool label from an aggregate key.
pub fn harness_label(tr: &Tr, key: &str) -> String {
    RequestHarness::from_id(key)
        .map(|h| tr.text(&h.text()))
        .unwrap_or_else(|| key.to_owned())
}

/// Tool and model from an aggregate key.
pub fn harness_model_label(tr: &Tr, key: &str) -> String {
    superba_core::split_harness_key(key)
        .map(|(h, m)| {
            format!(
                "{} · {}",
                tr.text(&h.text()),
                if m.is_empty() { DASH } else { m }
            )
        })
        .unwrap_or_else(|| key.to_owned())
}

/// Session list labels always name their tool.
pub fn session_group_label(tr: &Tr, titles: &BTreeMap<String, SessionTitle>, key: &str) -> String {
    match superba_core::split_harness_key(key) {
        Some((h, _)) => format!("{} · {}", tr.text(&h.text()), title_of(titles, key)),
        None => title_of(titles, key),
    }
}

/// Readable host filter label.
pub fn host_label(tr: &Tr, key: &str) -> String {
    if key == "local" {
        tr.t("requests.host.local")
    } else if let Some(d) = superba_core::wsl::distro_of_router_source(key) {
        tr.text(&superba_core::wsl_title(d))
    } else {
        key.to_owned()
    }
}

/// Requested → served model when a mapping was applied.
pub fn model_text(row: &RequestRow) -> String {
    match (
        row.requested_model.as_deref().or(row.model.as_deref()),
        row.served_model(),
    ) {
        (Some(requested), Some(served)) if requested != served => format!("{requested} → {served}"),
        (_, Some(served)) => served.to_owned(),
        (Some(requested), None) => requested.to_owned(),
        _ => DASH.to_owned(),
    }
}

/// Shared tool glyph; the adjacent name identifies the tool without relying on an icon.
pub fn harness_icon(harness: RequestHarness) -> crate::design::icons::Icon {
    use crate::design::icons::Icon;
    match harness {
        RequestHarness::Codex | RequestHarness::Dsh | RequestHarness::Pi => Icon::Terminal,
        RequestHarness::ClaudeCode | RequestHarness::OpenCode => Icon::Sessions,
        RequestHarness::ZCode | RequestHarness::WorkBuddy => Icon::Targets,
        _ => Icon::Tools,
    }
}

/// Name of the session of `key`: the resolved title, else the short key.
pub fn title_of(titles: &BTreeMap<String, SessionTitle>, key: &str) -> String {
    let (tool, id) = superba_core::split_harness_key(key).unwrap_or((RequestHarness::Codex, key));
    titles
        .get(key)
        .or_else(|| {
            (tool == RequestHarness::Codex)
                .then(|| titles.get(id))
                .flatten()
        })
        .map(|t| t.title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| id.chars().take(8).collect())
}

/// The 会话 cell of a row: the session name, with "· 标题生成" for title
/// generation requests.
pub fn session_cell(tr: &Tr, titles: &BTreeMap<String, SessionTitle>, row: &RequestRow) -> String {
    let base = match session_key(row) {
        Some(k) => title_of(titles, &superba_core::harness_group_key(row.harness, k)),
        None => DASH.to_owned(),
    };
    let base = if row.thread_source.as_deref() == Some("thread_title") {
        format!("{base} · {}", tr.t("requests.group.title_gen"))
    } else {
        base
    };
    // A request from a WSL distribution names it first, so a narrow column
    // still shows it (user bug 45); the tooltip and the detail say "WSL: …".
    match row
        .codex_host
        .as_deref()
        .and_then(superba_core::wsl::distro_of_router_source)
    {
        Some(distro) => format!("{distro} · {base}"),
        None => base,
    }
}

/// Whether a title is a real title (not the short-key fallback).
pub fn is_resolved(title: &SessionTitle) -> bool {
    title.source != TitleSource::ShortKey
}

/// A request row's accessible name (user bug 56): when, which session, how it
/// ended, the model and the cost, as a screen reader reads the row.
pub fn row_name(
    tr: &Tr,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
    now: Timestamp,
) -> String {
    let (_, status) = status_cell(tr, row);
    let status = match status_reason(tr, row) {
        Some(reason) => format!("{status} · {reason}"),
        None => status,
    };
    [
        date_cell(row.started_at, now),
        session_cell(tr, titles, row),
        status,
        tr.text(&row.harness.text()),
        model_text(row),
        match row.cost {
            Some(_) => format!("{} {}", tr.t("requests.col.cost"), cost_cell(row.cost)),
            None => String::new(),
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty() && s != DASH)
    .collect::<Vec<_>>()
    .join(", ")
}

/// One cell of the 13 columns.
pub fn cell(
    tr: &Tr,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
    col: Col,
    now: Timestamp,
) -> LogCell {
    match col {
        Col::Harness => LogCell::text(tr.text(&row.harness.text())).icon(harness_icon(row.harness)),
        Col::Model => LogCell::text(model_text(row)),
        Col::CacheKey => match session_key(row) {
            Some(k) => LogCell::mono(short_cache_key(k)).tooltip(k),
            None => LogCell::text(DASH),
        },
        Col::Date => {
            LogCell::text(date_cell(row.started_at, now)).tooltip(full_time(row.started_at))
        }
        Col::Session => {
            let text = session_cell(tr, titles, row);
            let mut tip = text.clone();
            if let Some(m) = &row.model {
                tip = format!("{tip} · {m}");
            }
            if let Some(d) = row
                .codex_host
                .as_deref()
                .and_then(superba_core::wsl::distro_of_router_source)
            {
                tip = format!("{tip}\n{}", tr.text(&superba_core::wsl_title(d)));
            }
            if tip == text {
                LogCell::text(text)
            } else {
                LogCell::text(text).tooltip(tip)
            }
        }
        Col::Status => {
            // A localized label (and reason where it fits); the wire code
            // only in the tooltip and the detail pane (ux-visual #3).
            let (status, label) = status_cell(tr, row);
            let mut c = match status_reason(tr, row) {
                Some(reason) => LogCell::text(format!("{label} · {reason}")).short(label),
                None => LogCell::text(label),
            }
            .dot(status);
            let mut tip = vec![status_detail(tr, row)];
            if row.failovers > 0 {
                tip.push(tr.f(
                    "requests.tip.failover",
                    &[("n", &row.failovers), ("line", &provider_cell(row))],
                ));
            }
            if let Some(e) = row.error.as_ref().filter(|e| !e.is_empty()) {
                tip.push(e.clone());
            }
            c = c.tooltip(tip.join("\n"));
            c
        }
        Col::Provider => {
            let mut c = LogCell::text(provider_cell(row));
            if let Some(h) = &row.upstream_host {
                c = c.tooltip(format!("{} · {h}", provider_cell(row)));
            } else if row.source == RequestSource::Rollout {
                c = c.tooltip(format!(
                    "{} · {}",
                    provider_cell(row),
                    tr.t("requests.detail.line_router_only")
                ));
            } else if row.source.imported_from().is_some() {
                c = c.tooltip(tr.text(&row.source.text()));
            }
            c
        }
        Col::Input => {
            let mut c = LogCell::text(tokens(row.fresh_input_tokens()));
            if let Some(raw) = row.input_tokens {
                c = c.tooltip(tr.f("requests.tip.input", &[("raw", &group_thousands(raw))]));
            }
            c
        }
        Col::Output => {
            let mut c = LogCell::text(tokens(row.output_tokens));
            if let Some(r) = row.reasoning_tokens {
                c = c.tooltip(tr.f("requests.tip.output", &[("reasoning", &group_thousands(r))]));
            }
            c
        }
        Col::CacheRead => LogCell::text(tokens(row.cached_tokens)),
        Col::CacheWrite => LogCell::text(tokens(row.cache_write_tokens)),
        // Codex's session files have no first token or TPS.
        Col::FirstToken | Col::Tps if row.source == RequestSource::Rollout => {
            LogCell::text(DASH).tooltip(tr.t("requests.tip.router_only"))
        }
        Col::FirstToken => LogCell::text(seconds(row.first_token_ms)),
        Col::Duration => LogCell::text(seconds(row.duration_ms)),
        Col::Tps => LogCell::text(tps(row.tps_milli)),
        Col::Cost => {
            let mut c = LogCell::text(cost_cell(row.cost));
            if let Some(tool) = row.source.imported_from() {
                c = c.tooltip(if row.cost.is_some() {
                    tr.f("requests.tip.cost_imported", &[("tool", &tool.name())])
                } else {
                    tr.f("requests.tip.unpriced_imported", &[("tool", &tool.name())])
                });
            } else if row.cost.is_some() {
                c = c.tooltip(tr.t("requests.tip.cost"));
            } else if row.input_tokens.is_some() || row.output_tokens.is_some() {
                c = c.tooltip(tr.t("requests.tip.unpriced"));
            } else if row.status == RequestStatus::Cancelled {
                c = c.tooltip(tr.t("requests.tip.cancelled_cost"));
            }
            c
        }
    }
}

/// Average first token (ms) over rows that have one.
pub fn avg_first_token(rows: &[RequestRow]) -> Option<u64> {
    let v: Vec<u64> = rows.iter().filter_map(|r| r.first_token_ms).collect();
    (!v.is_empty()).then(|| v.iter().sum::<u64>() / v.len() as u64)
}

/// Output-weighted tokens per second (milli) over rows with a TPS value.
pub fn weighted_tps(rows: &[RequestRow]) -> Option<u64> {
    let mut out = 0u128;
    let mut weighted = 0u128;
    for r in rows {
        if let (Some(t), Some(o)) = (r.tps_milli, r.output_tokens) {
            out += u128::from(o);
            weighted += u128::from(o) * u128::from(t);
        }
    }
    (out > 0).then(|| (weighted / out) as u64)
}

/// Totals of `rows` (used when the shown rows are a client-side filter of a
/// page, e.g. a search that also matched session names).
pub fn totals_of(rows: &[RequestRow]) -> RequestTotals {
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

/// The 24 hourly buckets of the last 24 hours ending with the current local
/// hour (oldest first; keys `HH:00`, empty hours included) from `rows`.
pub fn hourly_buckets(rows: &[RequestRow], now: Timestamp, offset_secs: i64) -> Vec<StatsBucket> {
    const HOUR: i64 = 3_600_000;
    let local = |t: Timestamp| t.as_millis() + offset_secs * 1000;
    let current = local(now).div_euclid(HOUR);
    (0..24)
        .rev()
        .map(|back| {
            let hour = current - back;
            let in_hour: Vec<RequestRow> = rows
                .iter()
                .filter(|r| local(r.started_at).div_euclid(HOUR) == hour)
                .cloned()
                .collect();
            let key = format!("{:02}:00", hour.rem_euclid(24));
            StatsBucket {
                label: key.clone(),
                key,
                totals: totals_of(&in_hour),
                lines: Vec::new(),
            }
        })
        .collect()
}

/// Success rate in percent (`None` without rows).
pub fn success_rate(t: &RequestTotals) -> Option<u64> {
    (t.requests > 0).then(|| (t.ok * 100 + t.requests / 2) / t.requests)
}

/// Cache hit rate in percent: cached / input.
pub fn hit_rate(t: &RequestTotals) -> Option<u64> {
    (t.input_tokens > 0).then(|| (t.cached_tokens * 100 + t.input_tokens / 2) / t.input_tokens)
}

/// The pinned totals row: count, success rate, token sums, average first token,
/// output-weighted TPS, estimated cost (+ "n without a price").
pub fn totals_cells(tr: &Tr, t: &RequestTotals, rows: &[RequestRow]) -> Vec<LogCell> {
    Col::ALL
        .iter()
        .map(|c| match c {
            Col::CacheKey => LogCell::text(tr.t("requests.total")),
            Col::Date => LogCell::text(tr.f(
                "requests.total_count",
                &[("n", &group_thousands(t.requests))],
            )),
            Col::Status => match success_rate(t) {
                Some(p) => LogCell::text(format!("{p} %")).tooltip(tr.t("requests.stats.success")),
                None => LogCell::text(DASH),
            },
            Col::Input => {
                let fresh = t
                    .input_tokens
                    .saturating_sub(t.cached_tokens)
                    .saturating_sub(t.cache_write_tokens);
                LogCell::text(compact(fresh)).tooltip(tr.f(
                    "requests.tip.input",
                    &[("raw", &group_thousands(t.input_tokens))],
                ))
            }
            Col::Output => LogCell::text(compact(t.output_tokens)),
            Col::CacheRead => LogCell::text(compact(t.cached_tokens)),
            Col::CacheWrite => LogCell::text(compact(t.cache_write_tokens)),
            Col::FirstToken => LogCell::text(seconds(avg_first_token(rows))),
            Col::Tps => LogCell::text(tps(weighted_tps(rows))),
            Col::Cost => {
                let mut c = LogCell::text(if t.cost.is_zero() && t.unpriced == t.requests {
                    DASH.to_owned()
                } else {
                    cost_cell(Some(t.cost))
                });
                if t.unpriced > 0 {
                    c = c.tooltip(tr.f("requests.no_price", &[("n", &t.unpriced)]));
                }
                c
            }
            _ => LogCell::default(),
        })
        .collect()
}

/// Move the texts of hidden leading totals cells (合计 in the 缓存键 column of
/// a narrow table) in front of the first shown cell's text. `widths` are the
/// table's column widths (0 = hidden).
pub fn fold_hidden_totals(cells: &mut [LogCell], widths: &[f32]) {
    let Some(first) = widths.iter().position(|w| *w > 0.0) else {
        return;
    };
    let lead: Vec<String> = cells
        .iter_mut()
        .take(first)
        .map(|c| std::mem::take(&mut c.text))
        .filter(|t| !t.is_empty())
        .collect();
    if let Some(c) = cells.get_mut(first)
        && !lead.is_empty()
    {
        // The cell's own text alone when both do not fit (`278 条`).
        let own = std::mem::take(&mut c.text);
        c.text = format!("{} {own}", lead.join(" ")).trim().to_owned();
        if !own.is_empty() {
            c.short = Some(own);
        }
    }
}

/// True when `row` matches `needle` (lowercase) on its key, member, line, host,
/// model, status, upstream request id or session name.
pub fn row_matches(row: &RequestRow, title: Option<&str>, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let fields = [
        row.prompt_cache_key.as_deref(),
        row.thread_id.as_deref(),
        Some(row.member_name.as_str()),
        row.line_name.as_deref(),
        row.upstream_host.as_deref(),
        row.model.as_deref(),
        row.upstream_model.as_deref(),
        row.response_model.as_deref(),
        row.requested_model.as_deref(),
        Some(row.harness.id()),
        row.session_id.as_deref(),
        row.turn_id.as_deref(),
        row.codex_host.as_deref(),
        row.upstream_request_id.as_deref(),
        row.error.as_deref(),
        title,
    ];
    fields
        .into_iter()
        .flatten()
        .any(|f| f.to_lowercase().contains(needle))
}

/// Session keys whose name matches `needle` (lowercase).
pub fn keys_matching(titles: &BTreeMap<String, SessionTitle>, needle: &str) -> Vec<String> {
    if needle.is_empty() {
        return Vec::new();
    }
    titles
        .iter()
        .filter(|(_, t)| t.title.to_lowercase().contains(needle))
        .map(|(k, _)| k.clone())
        .collect()
}

/// Merge `extra` rows (title matches found client-side) into `rows` (server
/// matches), without duplicates, newest first.
pub fn merge_rows(mut rows: Vec<RequestRow>, extra: Vec<RequestRow>) -> Vec<RequestRow> {
    for r in extra {
        if !rows.iter().any(|x| x.id == r.id) {
            rows.push(r);
        }
    }
    rows.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    rows
}

/// Rows newer than the newest id of `shown` (the "n new requests" pill).
pub fn new_rows(shown: &[RequestRow], latest: &[RequestRow]) -> usize {
    let max = shown.iter().map(|r| r.id).max().unwrap_or(i64::MIN);
    latest.iter().filter(|r| r.id > max).count()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// The rows as CSV (header in the UI language; full keys and timestamps; no
/// content, keys or tokens ever exist in rows). The 供应商 column holds the
/// provider alone, so a spreadsheet can group by it; its line and the upstream
/// host follow at the end.
pub fn csv(tr: &Tr, titles: &BTreeMap<String, SessionTitle>, rows: &[RequestRow]) -> String {
    let mut out = String::new();
    let mut header: Vec<String> = Col::ALL.iter().map(|c| tr.t(c.key())).collect();
    header.push(tr.t("requests.csv.client"));
    header.push(tr.t("requests.csv.line"));
    header.push(tr.t("requests.csv.host"));
    header.push(tr.t("requests.csv.source"));
    out.push_str(
        &header
            .iter()
            .map(|h| csv_field(h))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for r in rows {
        let status = status_detail(tr, r);
        let fields = [
            session_key(r).unwrap_or("").to_owned(),
            full_time(r.started_at),
            tr.text(&r.harness.text()),
            model_text(r),
            session_cell(tr, titles, r),
            status,
            if r.member_name.is_empty() {
                r.member_id.to_string()
            } else {
                r.member_name.clone()
            },
            r.fresh_input_tokens()
                .map(|n| n.to_string())
                .unwrap_or_default(),
            r.output_tokens.map(|n| n.to_string()).unwrap_or_default(),
            r.cached_tokens.map(|n| n.to_string()).unwrap_or_default(),
            r.cache_write_tokens
                .map(|n| n.to_string())
                .unwrap_or_default(),
            r.first_token_ms
                .map(|n| format!("{:.3}", n as f64 / 1000.0))
                .unwrap_or_default(),
            r.duration_ms
                .map(|n| format!("{:.3}", n as f64 / 1000.0))
                .unwrap_or_default(),
            r.tps_milli
                .map(|n| format!("{:.1}", n as f64 / 1000.0))
                .unwrap_or_default(),
            r.cost.map(|c| c.amount(6)).unwrap_or_default(),
            tr.text(&r.client_text()),
            r.line_label().unwrap_or_default().to_owned(),
            r.upstream_host.clone().unwrap_or_default(),
            tr.text(&r.source.text()),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// Columns of 活动请求.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveCol {
    /// Session name.
    Session,
    /// Provider · line.
    Provider,
    /// Model.
    Model,
    /// Phase.
    Status,
    /// First output delta.
    FirstToken,
    /// Elapsed.
    Elapsed,
    /// Output deltas received.
    Deltas,
    /// Delta rate.
    Rate,
    /// Client.
    Client,
    /// HTTP / WebSocket.
    Transport,
}

impl ActiveCol {
    /// All columns in order.
    pub const ALL: [ActiveCol; 10] = [
        ActiveCol::Session,
        ActiveCol::Provider,
        ActiveCol::Model,
        ActiveCol::Status,
        ActiveCol::FirstToken,
        ActiveCol::Elapsed,
        ActiveCol::Deltas,
        ActiveCol::Rate,
        ActiveCol::Client,
        ActiveCol::Transport,
    ];

    /// Header key and preferred width.
    pub fn spec(self) -> (&'static str, f32, bool) {
        match self {
            ActiveCol::Session => ("requests.col.session", 160.0, false),
            ActiveCol::Provider => ("requests.col.provider", 130.0, false),
            ActiveCol::Model => ("requests.col.model", 110.0, false),
            ActiveCol::Status => ("requests.col.status", 96.0, false),
            ActiveCol::FirstToken => ("requests.col.ttft", 56.0, true),
            ActiveCol::Elapsed => ("requests.active.elapsed", 70.0, true),
            ActiveCol::Deltas => ("requests.active.deltas", 80.0, true),
            ActiveCol::Rate => ("requests.active.rate", 64.0, true),
            ActiveCol::Client => ("requests.col.client", 80.0, false),
            ActiveCol::Transport => ("requests.col.transport", 60.0, false),
        }
    }
}

/// Header cells of 活动请求.
pub fn active_columns(tr: &Tr) -> Vec<LogColumn> {
    ActiveCol::ALL
        .iter()
        .map(|c| {
            let (key, w, right) = c.spec();
            let col = if right {
                LogColumn::right(tr.t(key), w)
            } else {
                LogColumn::left(tr.t(key), w)
            };
            col.fixed_order()
        })
        .collect()
}

/// The ≈ tok/s estimate of a request in flight from its delta rate.
pub fn delta_rate(r: &InflightRequest) -> Option<f64> {
    (r.elapsed_ms >= 500 && r.output_deltas > 0)
        .then(|| r.output_deltas as f64 / (r.elapsed_ms as f64 / 1000.0))
}

/// One cell of 活动请求.
pub fn active_cell(
    tr: &Tr,
    titles: &BTreeMap<String, SessionTitle>,
    r: &InflightRequest,
    col: ActiveCol,
) -> LogCell {
    match col {
        ActiveCol::Session => match &r.prompt_cache_key {
            Some(k) => LogCell::text(title_of(titles, k)).tooltip(k.clone()),
            None => LogCell::text(DASH),
        },
        ActiveCol::Provider => LogCell::text(match &r.line_name {
            Some(l) => format!("{} · {l}", r.member_name),
            None => r.member_name.clone(),
        }),
        ActiveCol::Model => LogCell::text(r.model.clone().unwrap_or_else(|| DASH.to_owned())),
        ActiveCol::Status => {
            let (status, key) = match r.phase {
                InflightPhase::Waiting => (Status::Busy, "requests.status.waiting"),
                InflightPhase::Holding => (Status::Ok, "requests.status.holding"),
                _ => (Status::Ok, "requests.status.streaming"),
            };
            LogCell::text(tr.t(key)).dot(status)
        }
        ActiveCol::FirstToken => LogCell::text(seconds(r.first_token_ms)),
        ActiveCol::Elapsed => LogCell::text(seconds(Some(r.elapsed_ms))),
        ActiveCol::Deltas => LogCell::text(group_thousands(r.output_deltas)),
        ActiveCol::Rate => {
            LogCell::text(delta_rate(r).map_or_else(|| DASH.to_owned(), |v| format!("≈{v:.0}")))
        }
        ActiveCol::Client => LogCell::text(tr.text(&r.client.text())),
        ActiveCol::Transport => LogCell::text(transport_text(tr, r.transport)),
    }
}

/// `SSE` / `WebSocket`.
pub fn transport_text(tr: &Tr, t: superba_core::RequestTransport) -> String {
    match t {
        superba_core::RequestTransport::WebSocket => "WebSocket".to_owned(),
        _ => tr.t("requests.transport.sse"),
    }
}

/// Request kind text (turn / prewarm / compaction / memory / title).
pub fn kind_text(tr: &Tr, row: &RequestRow) -> String {
    if row.thread_source.as_deref() == Some("thread_title") {
        return tr.t("requests.kind.title");
    }
    tr.t(match row.request_kind {
        Some(RequestKind::Turn) => "requests.kind.turn",
        Some(RequestKind::Prewarm) => "requests.kind.prewarm",
        Some(RequestKind::Compaction) => "requests.kind.compaction",
        Some(RequestKind::Memory) => "requests.kind.memory",
        _ => "requests.kind.other",
    })
}

/// Whether the stats line could ever be added to this row's response (never
/// for `codex exec`, unknown clients or non-turn requests).
pub fn line_applicable(row: &RequestRow) -> bool {
    matches!(row.client, RequestClient::Desktop | RequestClient::Tui)
        && matches!(row.request_kind, Some(RequestKind::Turn) | None)
        && row.thread_source.as_deref().is_none_or(|s| s == "user")
}
