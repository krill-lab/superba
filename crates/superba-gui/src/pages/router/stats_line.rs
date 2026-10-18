//! The reply stats line (路由设置 → 回复统计行, feedback 5b): an opt-in markdown
//! quote the local router appends to the final answer of a user turn, e.g.
//! `> ⚡ 87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out`. The router fills the
//! placeholders and never translates. The default template is stored as a
//! marker (no template), shown resolved in the UI language with the hint
//! 默认（跟随界面语言）, and resolved in the same language whenever the settings
//! reach the router, so it follows a language change. Checkboxes build the
//! default template; a hand-edited template is kept verbatim and greys them
//! out until 恢复默认. The preview mimics the app (grey rule, body-colour text),
//! renders through the router's own renderer and updates while typing.

use superba_core::{
    Catalog, RequestClient, RequestKind, RequestRow, RequestStatus, RouterStatsSettings,
    StatsLineClient, StatsLineSettings, StatsLineStyle, StatsMetric, Timestamp, Usd,
};

use crate::i18n::Tr;
use crate::pages::shared::format::group_thousands;

/// Metrics offered as checkboxes, in order, with their label keys.
pub const METRICS: [(StatsMetric, &str); 5] = [
    (StatsMetric::Tps, "stats_line.metric.tps"),
    (StatsMetric::FirstToken, "stats_line.metric.ttft"),
    (StatsMetric::TurnTokens, "stats_line.metric.turn_tokens"),
    (StatsMetric::Cached, "stats_line.metric.cached"),
    (StatsMetric::Cost, "stats_line.metric.cost"),
];

/// The default template of `metrics` in the UI language.
pub fn default_template(tr: &Tr, metrics: &[StatsMetric]) -> String {
    StatsLineSettings::default_template(metrics, &Catalog::new(tr.locale()))
}

/// The template without its leading `⚡` (the router always puts `⚡` and its
/// invisible marker first, so the editor shows the bolt as a fixed prefix).
pub fn editable(template: &str) -> String {
    let t = template.trim_start();
    let t = t.strip_prefix('⚡').unwrap_or(t);
    t.strip_prefix('\u{2060}')
        .unwrap_or(t)
        .trim_start()
        .to_owned()
}

/// The template for the edited text: `⚡ ` followed by it.
pub fn with_bolt(text: &str) -> String {
    format!("⚡ {}", editable(text))
}

/// The template in effect: the custom one, else the default resolved in the UI
/// language (the same text the router gets, [`StatsLineSettings::resolved_template`]).
pub fn effective_template(tr: &Tr, line: &StatsLineSettings) -> String {
    line.resolved_template(tr.locale())
}

/// A custom template is in use (the metric checkboxes are greyed out and
/// 恢复默认 is offered); otherwise the default follows the UI language.
pub fn is_custom(line: &StatsLineSettings) -> bool {
    line.custom_template().is_some()
}

/// The caption under the template field: the custom-template note (a custom
/// template is stored or being typed; 恢复默认 is offered next to it), else the
/// default hint 默认（跟随界面语言）.
pub fn template_note(tr: &Tr, custom: bool) -> String {
    tr.t(if custom {
        "stats_line.custom"
    } else {
        "stats_line.default_hint"
    })
}

/// Placeholders in `template` the router does not know.
pub fn unknown_placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tail in template.split('{').skip(1) {
        if let Some((name, _)) = tail.split_once('}')
            && !StatsLineSettings::PLACEHOLDERS.contains(&name)
            && !out.iter().any(|o| o == name)
        {
            out.push(name.to_owned());
        }
    }
    out
}

/// `line` after the template field was edited to `text`: nothing, `default` or
/// the default template of the current metrics as shown means the default
/// marker; anything else is stored as written.
pub fn with_template(tr: &Tr, line: &StatsLineSettings, text: &str) -> StatsLineSettings {
    let mut next = line.clone();
    let t = text.trim();
    if t == default_template(tr, &line.metrics) {
        next.template = None;
    } else {
        next.set_template(t);
    }
    next
}

/// `line` back on the default template (恢复默认).
pub fn with_default_template(line: &StatsLineSettings) -> StatsLineSettings {
    let mut next = line.clone();
    next.template = None;
    next
}

/// `line` with `metric` turned on or off (keeps the fixed order).
pub fn with_metric(line: &StatsLineSettings, metric: StatsMetric, on: bool) -> StatsLineSettings {
    let mut next = line.clone();
    next.metrics.retain(|m| *m != metric);
    if on {
        next.metrics.push(metric);
    }
    next.metrics.sort();
    next
}

/// `line` with `client` added or removed.
pub fn with_client(
    line: &StatsLineSettings,
    client: StatsLineClient,
    on: bool,
) -> StatsLineSettings {
    let mut next = line.clone();
    next.clients.retain(|c| *c != client);
    if on {
        next.clients.push(client);
    }
    next.clients.sort();
    next
}

/// `line` with `harness` (Claude Code, …) added or removed.
pub fn with_harness(
    line: &StatsLineSettings,
    harness: superba_core::RequestHarness,
    on: bool,
) -> StatsLineSettings {
    let mut next = line.clone();
    next.harnesses.retain(|h| *h != harness);
    if on {
        next.harnesses.push(harness);
    }
    next.harnesses.sort();
    next
}

/// Values for the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Tokens per second (milli) of the answering request.
    pub tps_milli: Option<u64>,
    /// First token of the answering request.
    pub first_token_ms: Option<u64>,
    /// Turn input (raw, incl. cached).
    pub input: u64,
    /// Turn output.
    pub output: u64,
    /// Turn cached input.
    pub cached: u64,
    /// Turn cost.
    pub cost: Option<Usd>,
    /// Requests in the turn.
    pub requests: u64,
    /// When the turn ended (`None` = sample data).
    pub at: Option<Timestamp>,
}

impl Sample {
    /// The sample of the UX spec (labelled "sample data").
    pub fn example() -> Self {
        Self {
            tps_milli: Some(87_000),
            first_token_ms: Some(820),
            input: 12_345,
            output: 321,
            cached: 11_904,
            cost: Some(Usd::from_micros(19_000)),
            requests: 3,
            at: None,
        }
    }

    /// The last finished user turn among `rows` (newest first or any order):
    /// its final request's speed and the turn's totals.
    pub fn last_turn(rows: &[RequestRow]) -> Option<Self> {
        let last = rows
            .iter()
            .filter(|r| {
                matches!(r.client, RequestClient::Desktop | RequestClient::Tui)
                    && matches!(r.request_kind, Some(RequestKind::Turn))
                    && r.thread_source.as_deref().is_none_or(|s| s == "user")
                    && r.status == RequestStatus::Ok
                    && r.output_tokens.is_some()
            })
            .max_by_key(|r| (r.started_at, r.id))?;
        let turn: Vec<&RequestRow> = match &last.turn_id {
            Some(t) => rows
                .iter()
                .filter(|r| r.turn_id.as_deref() == Some(t.as_str()))
                .collect(),
            None => vec![last],
        };
        let sum = |f: fn(&RequestRow) -> Option<u64>| turn.iter().filter_map(|r| f(r)).sum();
        let costs: Vec<Usd> = turn.iter().filter_map(|r| r.cost).collect();
        Some(Self {
            tps_milli: last.tps_milli,
            first_token_ms: last.first_token_ms,
            input: sum(|r| r.input_tokens),
            output: sum(|r| r.output_tokens),
            cached: sum(|r| r.cached_tokens),
            cost: (!costs.is_empty())
                .then(|| costs.iter().fold(Usd::ZERO, |a, c| a.saturating_add(*c))),
            requests: turn.len() as u64,
            at: Some(
                last.started_at
                    .saturating_add(std::time::Duration::from_millis(
                        last.duration_ms.unwrap_or(0),
                    )),
            ),
        })
    }
}

/// Fill the placeholders of `template` with `sample` exactly as the router does
/// (`superba_router::stats::inject::render_text`), after the bolt.
pub fn render(template: &str, sample: &Sample) -> String {
    use superba_router::stats::inject::{LineValues, render_text};
    let values = LineValues {
        tps: sample.tps_milli.map(|m| group_thousands((m + 500) / 1000)),
        ttft: sample
            .first_token_ms
            .map(|ms| format!("{:.2}s", ms as f64 / 1000.0)),
        input: sample.input,
        output: sample.output,
        cached: sample.cached,
        cost: sample.cost,
        requests: u32::try_from(sample.requests).unwrap_or(u32::MAX),
    };
    let text = render_text(template, &values);
    if text.is_empty() {
        "⚡".to_owned()
    } else {
        format!("⚡ {text}")
    }
}

/// The stats line settings after toggling it on or off.
pub fn with_enabled(stats: &RouterStatsSettings, on: bool) -> RouterStatsSettings {
    let mut next = stats.clone();
    next.line.enabled = on;
    next
}

/// The stats line settings with another style.
pub fn with_style(stats: &RouterStatsSettings, style: StatsLineStyle) -> RouterStatsSettings {
    let mut next = stats.clone();
    next.line.style = style;
    next
}
