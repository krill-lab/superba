//! 统计 (UX §3.5): grouped totals of the request log. Range [24 小时 | 7 天 |
//! 30 天], grouping [模型 | 供应商 | 会话 | 日期], daily bars (hourly for 24 小时;
//! requests or cost)
//! and a paged table: 请求, 成功率, 输入, 输出, 缓存读取, 缓存写入, 命中率, 平均首字, 平均 TPS,
//! 费用. By 供应商 each provider (Krill, each custom site) is one row with the
//! current profile name; a provider that used several lines has their totals in
//! indented rows under it.
//! For a provider with a usage API (Krill) 与账单对比 puts the local estimate next
//! to the billed cost per day and offers to calibrate its price ratio.

use std::collections::BTreeMap;
use std::time::Duration;

use egui::{Align, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use superba_core::{
    RequestQuery, RequestStats, SessionTitle, StatsBucket, StatsGroupBy, StatsQuery,
};

use super::db::Loader;
use super::requests::{self, Range, cost_cell, hit_rate, hourly_buckets, success_rate};
use super::{State, mode, prices, recent};
use crate::design::fonts::{Weight, font};
use crate::design::layout::{self, Track};
use crate::design::tokens::text;
use crate::design::widgets::{Button, Dropdown, PagedState, PagedTable, Segmented, Table, Text};
use crate::pages::shared::format::compact;
use crate::pages::{Action, Cx, error_text};

/// Rows read for the hourly bars of the 24 h range; with more the bars stay
/// daily.
const HOURLY_ROWS: u32 = 20_000;

/// What the daily bars show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Metric {
    /// Requests per day.
    #[default]
    Requests,
    /// Estimated cost per day.
    Cost,
}

/// State of 统计.
#[derive(Debug, Default)]
pub struct Stats {
    /// Range.
    pub range: Range,
    /// Grouping.
    pub by: StatsGroupBy,
    /// Bars metric.
    pub metric: Metric,
    /// The aggregates.
    pub stats: Loader<RequestStats>,
    /// Table paging.
    pub table: PagedState,
    /// Hourly bars of the 24 h range (empty = too many rows, daily bars).
    pub hourly: Loader<Vec<StatsBucket>>,
    loaded_for: Option<(Range, StatsGroupBy, superba_core::RequestQuery)>,
}

impl Stats {
    /// A read is running.
    pub fn loading(&self) -> bool {
        self.stats.loading()
    }
}

/// Groupings offered, with their label keys.
pub const GROUPS: [(StatsGroupBy, &str); 6] = [
    (StatsGroupBy::Harness, "requests.stats.by.harness"),
    (
        StatsGroupBy::HarnessModel,
        "requests.stats.by.harness_model",
    ),
    (StatsGroupBy::Model, "requests.stats.by.model"),
    (StatsGroupBy::Provider, "requests.stats.by.provider"),
    (StatsGroupBy::Session, "requests.stats.by.session"),
    (StatsGroupBy::Day, "requests.stats.by.day"),
];

/// Value of one bucket for the bars.
pub fn bar_value(b: &StatsBucket, metric: Metric) -> f64 {
    match metric {
        Metric::Requests => b.totals.requests as f64,
        Metric::Cost => b.totals.cost.as_dollars(),
    }
}

/// Table cells of one bucket (name first).
pub fn row_cells(label: String, b: &StatsBucket) -> Vec<String> {
    let t = &b.totals;
    vec![
        label,
        requests::tokens(Some(t.requests)),
        success_rate(t).map_or_else(|| requests::DASH.to_owned(), |p| format!("{p} %")),
        compact(
            t.input_tokens
                .saturating_sub(t.cached_tokens)
                .saturating_sub(t.cache_write_tokens),
        ),
        compact(t.output_tokens),
        compact(t.cached_tokens),
        compact(t.cache_write_tokens),
        hit_rate(t).map_or_else(|| requests::DASH.to_owned(), |p| format!("{p} %")),
        requests::seconds(t.avg_first_token_ms()),
        requests::tps(t.tps_milli()),
        if t.cost.is_zero() && t.unpriced > 0 {
            requests::DASH.to_owned()
        } else {
            cost_cell(Some(t.cost))
        },
    ]
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let _ = out;
    let live = mode(cx).is_router();
    recent::load_facets(cx, &mut state.recent, state.stats.range);
    let filters = RequestQuery {
        harnesses: state.recent.harnesses.clone(),
        model: state.recent.model.clone(),
        member: state.recent.member.clone(),
        host: state.recent.host.clone(),
        ..RequestQuery::default()
    };
    let s = &mut state.stats;
    s.stats.poll();
    let signature = (s.range, s.by, filters.clone());
    if s.loaded_for.as_ref() != Some(&signature) {
        s.stats.invalidate();
        s.hourly.invalidate();
        s.table = PagedState::default();
        s.loaded_for = Some(signature);
    }
    if s.stats.due(live.then_some(Duration::from_secs(10))) {
        let service = cx.service.clone();
        let q = StatsQuery {
            source: None,
            from: Some(s.range.from(cx.now)),
            to: None,
            by: s.by,
            member: filters.member.clone(),
            harnesses: filters.harnesses.clone(),
            model: filters.model.clone(),
            host: filters.host.clone(),
            protocol: None,
            utc_offset_minutes: (requests::local_offset_secs(cx.now).unwrap_or(0) / 60) as i32,
        };
        s.stats.start(async move { service.router_stats(q).await });
    }
    // The 24 h range gets hourly bars (ux-visual #8), counted from its rows.
    s.hourly.poll();
    if s.range != Range::Day {
        s.hourly.reset();
    } else if s.hourly.due(live.then_some(Duration::from_secs(30))) {
        let service = cx.service.clone();
        let (now, from) = (cx.now, s.range.from(cx.now));
        let offset = requests::local_offset_secs(cx.now).unwrap_or(0);
        s.hourly.start(async move {
            let page = service
                .router_requests(RequestQuery {
                    from: Some(from),
                    limit: HOURLY_ROWS,
                    ..filters
                })
                .await?;
            Ok(if page.truncated {
                Vec::new()
            } else {
                hourly_buckets(&page.rows, now, offset)
            })
        });
    }
    if s.by == StatsGroupBy::Session
        && let Some(v) = &s.stats.value
    {
        let keys: Vec<&str> = v
            .buckets
            .iter()
            .filter_map(|b| superba_core::split_harness_key(&b.key))
            .filter(|(h, _)| *h == superba_core::RequestHarness::Codex)
            .map(|(_, id)| id)
            .collect();
        state.titles.want(cx.service, keys);
    }
    let compare = prices::providers(cx)
        .into_iter()
        .find(|(_, _, usage)| *usage);
    layout::rows(
        ui,
        &[Track::Fixed(62.0), Track::Fixed(150.0), Track::Fill],
        10.0,
        |ui, i| match i {
            0 => controls(ui, cx, state),
            1 => {
                if let Some((id, name, _)) = &compare {
                    layout::columns(ui, &[Track::Fill, Track::Fixed(260.0)], 16.0, |ui, c| {
                        if c == 0 {
                            bars(ui, cx, &state.stats);
                        } else {
                            compare_card(ui, cx, state, id, name);
                        }
                    });
                } else {
                    bars(ui, cx, &state.stats);
                }
            }
            _ => table(ui, cx, state),
        },
    );
}

fn controls(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let s = &mut state.stats;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let mut seg = Segmented::new("router-stats-range", &mut s.range)
            .small()
            .label(cx.tr.t("requests.range.name"));
        for r in Range::ALL {
            seg = seg.option(r, cx.tr.t(r.key()));
        }
        seg.show(ui);
        let options: Vec<_> = GROUPS.iter().map(|(by, key)| (*by, cx.tr.t(key))).collect();
        Dropdown::new("router-stats-by")
            .label(cx.tr.t("requests.stats.group"))
            .fixed_width(160.0)
            .show_options(ui, &mut s.by, &options);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            Segmented::new("router-stats-metric", &mut s.metric)
                .small()
                .label(cx.tr.t("requests.stats.metric.name"))
                .option(Metric::Cost, cx.tr.t("requests.stats.metric.cost"))
                .option(Metric::Requests, cx.tr.t("requests.stats.metric.requests"))
                .show(ui);
        });
    });
    ui.add_space(6.0);
    recent::tool_filters(ui, cx, &mut state.recent);
}

fn bars(ui: &mut Ui, cx: &Cx<'_>, s: &Stats) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_stroke(
        rect,
        crate::design::tokens::radius::CONTROL + 2,
        Stroke::new(1.0, cx.pal.border),
        egui::StrokeKind::Inside,
    );
    let Some(stats) = &s.stats.value else {
        let text = match &s.stats.error {
            Some(e) => error_text(cx.tr, e),
            None => cx.tr.t("requests.loading"),
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            crate::design::fonts::font(12.0, crate::design::fonts::Weight::Regular),
            cx.pal.text_muted,
        );
        ui.allocate_rect(rect, Sense::hover());
        return;
    };
    let inner = rect.shrink2(vec2(14.0, 12.0));
    let label_h = 14.0;
    let chart = Rect::from_min_max(inner.min, pos2(inner.max.x, inner.max.y - label_h));
    // 24 h: one bar per hour (one or two daily bars said little).
    let hourly = s
        .hourly
        .value
        .as_ref()
        .filter(|h| s.range == Range::Day && !h.is_empty());
    let days = hourly.unwrap_or(&stats.daily);
    let max = days
        .iter()
        .map(|b| bar_value(b, s.metric))
        .fold(0.0f64, f64::max);
    // The caption is the range total and says how the bars split it.
    let title = match (s.metric, hourly.is_some()) {
        (Metric::Requests, hourly) => cx.tr.f(
            if hourly {
                "requests.stats.bars_requests_hourly"
            } else {
                "requests.stats.bars_requests"
            },
            &[("n", &requests::tokens(Some(stats.totals.requests)))],
        ),
        (Metric::Cost, hourly) => cx.tr.f(
            if hourly {
                "requests.stats.bars_cost_hourly"
            } else {
                "requests.stats.bars_cost"
            },
            &[("cost", &requests::cost_text(Some(stats.totals.cost)))],
        ),
    };
    ui.painter().text(
        chart.min,
        egui::Align2::LEFT_TOP,
        title,
        crate::design::fonts::font(12.0, crate::design::fonts::Weight::Medium),
        cx.pal.text_muted,
    );
    let plot = Rect::from_min_max(pos2(chart.min.x, chart.min.y + 20.0), chart.max);
    if days.is_empty() || max <= 0.0 {
        ui.painter().text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            cx.tr.t("requests.stats.no_data"),
            crate::design::fonts::font(12.0, crate::design::fonts::Weight::Regular),
            cx.pal.text_subtle,
        );
        ui.allocate_rect(rect, Sense::hover());
        return;
    }
    let n = days.len() as f32;
    let slot = plot.width() / n;
    let bar_w = (slot * 0.6).clamp(2.0, 28.0);
    for (i, b) in days.iter().enumerate() {
        let v = bar_value(b, s.metric);
        let h = (v / max) as f32 * plot.height();
        let cx_ = plot.min.x + slot * (i as f32 + 0.5);
        let r = Rect::from_min_max(
            pos2(cx_ - bar_w / 2.0, plot.max.y - h.max(1.0)),
            pos2(cx_ + bar_w / 2.0, plot.max.y),
        );
        let hover = Rect::from_min_max(
            pos2(cx_ - slot / 2.0, plot.min.y),
            pos2(cx_ + slot / 2.0, plot.max.y),
        );
        let resp = ui.interact(hover, ui.scope_id().with(("router-bar", i)), Sense::hover());
        let fill = if resp.hovered() {
            cx.pal.text
        } else {
            cx.pal.text_muted
        };
        ui.painter().rect_filled(r, 2, fill);
        resp.on_hover_text(format!(
            "{} · {} · {}",
            b.key,
            requests::tokens(Some(b.totals.requests)),
            requests::cost_text(Some(b.totals.cost))
        ));
        let step = if hourly.is_some() {
            3
        } else {
            (days.len() / 8).max(1)
        };
        if i % step == 0 || i + 1 == days.len() {
            ui.painter().text(
                pos2(cx_, plot.max.y + 2.0),
                egui::Align2::CENTER_TOP,
                if hourly.is_some() {
                    b.key.get(..2).unwrap_or(&b.key)
                } else {
                    b.key.get(5..).unwrap_or(&b.key)
                },
                crate::design::fonts::mono_font(10.0, crate::design::fonts::Weight::Regular),
                cx.pal.text_subtle,
            );
        }
    }
    ui.painter()
        .hline(plot.x_range(), plot.max.y, Stroke::new(1.0, cx.pal.border));
    ui.allocate_rect(rect, Sense::hover());
}

fn compare_card(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    id: &superba_core::ProfileId,
    name: &str,
) {
    let loading = state.prefs.prices.calibration.loading();
    crate::design::widgets::InfoCard::new(cx.tr.t("requests.stats.compare"))
        .text(
            cx.tr
                .f("requests.stats.compare_body", &[("provider", &name)]),
        )
        .footer(|ui| {
            if ui
                .add(
                    Button::new(cx.tr.t("requests.stats.calibrate"))
                        .small()
                        .loading(loading)
                        .enabled(!loading),
                )
                .on_hover_text(cx.tr.t("prices.calibrate_tip"))
                .clicked()
            {
                prices::start_calibration(cx, state, id.clone(), name.to_owned());
            }
            if let Some(e) = &state.prefs.prices.calibration.error {
                ui.label(Text::caption(error_text(cx.tr, e), cx.pal).color(cx.pal.red));
            }
        })
        .show(ui);
    // The comparison opens as a dialog once loaded (prices::section polls it on
    // the settings tab; here it is polled too).
    if state.prefs.prices.calibration.poll()
        && let Some(c) = state.prefs.prices.calibration.value.clone()
    {
        state.dialog = Some(super::Dialog::Calibrate(c));
    }
}

fn table(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let titles = state.titles.map();
    let s = &mut state.stats;
    let headers: Vec<String> = [
        match s.by {
            StatsGroupBy::Harness => "requests.col.harness",
            StatsGroupBy::HarnessModel => "requests.stats.by.harness_model",
            StatsGroupBy::Provider => "requests.col.provider",
            StatsGroupBy::Session => "requests.col.session",
            StatsGroupBy::Day => "requests.col.date",
            _ => "requests.col.model",
        },
        "requests.stats.requests",
        "requests.stats.success",
        "requests.col.input",
        "requests.col.output",
        "requests.col.cache_read",
        "requests.col.cache_write",
        "requests.stats.hit",
        "requests.stats.avg_ttft",
        "requests.stats.avg_tps",
        "requests.col.cost",
    ]
    .iter()
    .map(|k| cx.tr.t(k))
    .collect();
    let mut buckets: Vec<StatsBucket> = s
        .stats
        .value
        .as_ref()
        .map(|v| {
            let mut b = v.buckets.clone();
            if s.by == StatsGroupBy::Day {
                b.reverse();
            }
            b
        })
        .unwrap_or_default();
    if s.by == StatsGroupBy::Provider {
        let set = cx.data.profiles.value.as_ref();
        requests::ProviderNames::new(cx.tr, set, &cx.data.sites).relabel_buckets(&mut buckets);
    }
    let rows = table_rows(cx.tr, titles, s.by, &buckets);
    let table = Table::new(&[2.2, 0.9, 0.9, 0.9, 0.9, 0.9, 0.9, 0.8, 0.9, 0.8, 0.9]);
    // Numbers as in 最近请求: right-aligned under right-aligned headers, in the
    // UI font at caption size (ux-visual #8).
    PagedTable::new("router-stats-table", table, rows.len(), &mut s.table)
        .label(cx.tr.t("usage.tab.stats"))
        .header(headers)
        .right_aligned(&(1..11).collect::<Vec<_>>())
        .show(ui, |ui, row, col| {
            let (line, cells) = &rows[row];
            let value = &cells[col];
            let color = if *line {
                cx.pal.text_muted
            } else {
                cx.pal.text
            };
            if col == 0 {
                if *line {
                    ui.add_space(LINE_INDENT);
                }
                crate::pages::elided(ui, Text::small(value, cx.pal).color(color));
            } else {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(value)
                            .font(font(text::CAPTION, Weight::Regular))
                            .color(color),
                    );
                });
            }
        });
}

/// Indent of a provider's line rows in the table.
const LINE_INDENT: f32 = 14.0;

/// Table rows `(is a line row, cells)`: one per bucket; by provider, a provider
/// that used several lines is followed by one row per line.
pub fn table_rows(
    tr: &crate::i18n::Tr,
    titles: &BTreeMap<String, SessionTitle>,
    by: StatsGroupBy,
    buckets: &[StatsBucket],
) -> Vec<(bool, Vec<String>)> {
    let mut rows = Vec::new();
    for b in buckets {
        let label = match by {
            StatsGroupBy::Session if !b.key.is_empty() => {
                requests::session_group_label(tr, titles, &b.key)
            }
            StatsGroupBy::Harness => requests::harness_label(tr, &b.key),
            StatsGroupBy::HarnessModel => requests::harness_model_label(tr, &b.key),
            _ if b.label.is_empty() => requests::DASH.to_owned(),
            _ => b.label.clone(),
        };
        rows.push((false, row_cells(label, b)));
        if by == StatsGroupBy::Provider && b.lines.len() > 1 {
            rows.extend(
                b.lines
                    .iter()
                    .map(|l| (true, row_cells(l.label.clone(), l))),
            );
        }
    }
    rows
}
