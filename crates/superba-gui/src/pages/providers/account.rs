//! The Account & usage tab of a provider (UX §3.3): identity and balance from
//! the standard billing endpoints, and for sites with a usage API the daily cost
//! of the last 7 or 30 days and a paged per-model table. Usage reports are
//! rate-limited (Krill: 10 an hour), so a report is fetched only when the tab
//! opens without data for the provider and range, or on Refresh; when the site
//! refuses a new range (429) the report shown stays, with a note.

use egui::{Align, Align2, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use superba_core::{AccountReport, DailyUsage, UsageQuery, UsageReport, Usd};

use super::{AccountState, State, ops};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, size, text};
use crate::design::widgets::{Button, PagedState, PagedTable, Segmented, Status, Table, Text};
use crate::pages::shared::providers::Provider;
use crate::pages::shared::{compact, group_thousands, nice_axis};
use crate::pages::{Cx, callout, empty_state, error_text};

/// Ranges offered.
pub const RANGES: [u32; 2] = [7, 30];

/// Collect finished account loads; true while one runs.
pub(super) fn poll(state: &mut State) -> bool {
    let mut busy = false;
    for acc in state.accounts.values_mut() {
        let Some((days, task)) = &acc.task else {
            continue;
        };
        let days = *days;
        match task.poll() {
            Some(result) => {
                acc.task = None;
                finish(acc, days, result);
            }
            None => busy = true,
        }
    }
    busy
}

/// Store a result. A 429 for a range without a saved report keeps the report
/// that was shown (with a note) instead of an error page.
pub fn finish(acc: &mut AccountState, days: u32, result: superba_core::Result<AccountReport>) {
    let limited =
        |e: &superba_core::Error| matches!(e, superba_core::Error::Http { status: 429, .. });
    let refused = match &result {
        Ok(r) => r.usage.is_none() && r.usage_error.as_ref().is_some_and(limited),
        Err(e) => limited(e),
    };
    let previous = acc
        .report
        .as_ref()
        .and_then(|r| r.usage.as_ref())
        .map(|u| u.days);
    if refused && let Some(shown) = previous {
        acc.limited = Some(days);
        acc.days = shown;
        return;
    }
    match result {
        Ok(r) => {
            if r.usage.is_some() {
                acc.limited = None;
            }
            acc.days = days;
            acc.report = Some(r);
            acc.error = None;
        }
        Err(e) => acc.error = Some(e),
    }
}

/// Whether the tab should ask for a report now: nothing loaded or loading for
/// the shown range (opening the tab again never refetches).
pub fn needs_load(acc: &AccountState, usage_api: bool) -> bool {
    if acc.task.is_some() || acc.error.is_some() {
        return false;
    }
    match &acc.report {
        None => true,
        Some(r) => {
            usage_api
                && r.usage_error.is_none()
                && r.usage.as_ref().is_none_or(|u| u.days != acc.days)
        }
    }
}

fn start(acc: &mut AccountState, cx: &Cx<'_>, p: &Provider, days: u32, refresh: bool) {
    let Some(profile) = p.profile_id().cloned() else {
        return;
    };
    let usage = p.site.capabilities.usage_api.then_some(days);
    acc.error = None;
    acc.task = Some((
        days,
        crate::runtime::task(ops::account(cx.service.clone(), profile, usage, refresh)),
    ));
}

/// Draw the tab.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider) {
    // Test from this tab refreshes the Connection tab's WebSocket / zstd checks.
    super::transports::follow(ui, cx, state, p);
    let Some(profile) = p.profile_id().cloned() else {
        return;
    };
    let usage_api = p.site.capabilities.usage_api;
    let acc = state
        .accounts
        .entry(profile.clone())
        .or_insert_with(|| AccountState {
            days: RANGES[0],
            ..AccountState::default()
        });
    if needs_load(acc, usage_api) {
        let days = acc.days;
        start(acc, cx, p, days, false);
    }
    let loading = acc.task.is_some();
    let usage = acc.report.as_ref().and_then(|r| r.usage.as_ref());
    // A refresh within a minute returns the same saved report (superba-net
    // debounces it): say so instead of a button that does nothing.
    let wait = usage.and_then(|u| {
        let age = (cx.now.as_millis() - u.fetched_at.as_millis()).max(0) as u64 / 1000;
        let window = UsageQuery::REFRESH_DEBOUNCE_SECS;
        (age < window).then(|| window - age)
    });
    if wait.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }
    let mut refresh = false;
    let mut range = None;
    ui.horizontal(|ui| {
        ui.set_min_height(size::CONTROL);
        // Refresh and the range first (right to left): the account line left
        // of them is shortened instead of drawn under them (960×640).
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui.add(
                Button::new(cx.tr.t("action.refresh"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(loading)
                    .enabled(!loading && wait.is_none()),
            );
            let r = match wait {
                Some(secs) => {
                    r.on_disabled_hover_text(cx.tr.f("usage.refresh_wait", &[("n", &secs)]))
                }
                None => r.on_hover_text(cx.tr.t("usage.refresh_hint")),
            };
            refresh = r.clicked();
            if usage_api {
                let mut d = acc.days;
                if Segmented::new(("providers-usage-range", profile.as_str()), &mut d)
                    .label(cx.tr.t("requests.range.name"))
                    .option(7, cx.tr.t("usage.range.7"))
                    .option(30, cx.tr.t("usage.range.30"))
                    .small()
                    .show(ui)
                    .changed()
                {
                    range = Some(d);
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                status_line(ui, cx, acc);
            });
        });
    });
    if refresh {
        let days = acc.days;
        start(acc, cx, p, days, true);
    } else if let Some(d) = range {
        acc.days = d;
        if usage_api
            && acc
                .report
                .as_ref()
                .and_then(|r| r.usage.as_ref())
                .is_none_or(|u| u.days != d)
        {
            start(acc, cx, p, d, false);
        }
    }
    ui.add_space(8.0);
    let report = acc.report.clone();
    let Some(report) = report else {
        match &acc.error {
            Some(e) => empty_state(
                ui,
                cx,
                Icon::Chart,
                &cx.tr.t("usage.unavailable.title"),
                &error_text(cx.tr, e),
            ),
            None => empty_state(
                ui,
                cx,
                Icon::Chart,
                &cx.tr.t("usage.loading.title"),
                &cx.tr.t("usage.loading.body"),
            ),
        }
        return;
    };
    if let Some(n) = report.usage.as_ref().and_then(|u| u.notice.as_ref()) {
        callout(ui, cx, Status::Warn, &cx.tr.text(&n.text()));
        ui.add_space(6.0);
    }
    if let (Some(refused), Some(u)) = (acc.limited, report.usage.as_ref()) {
        callout(
            ui,
            cx,
            Status::Warn,
            &cx.tr.f(
                "usage.limited",
                &[
                    ("refused", &refused),
                    ("days", &u.days),
                    ("ago", &cx.ago(u.fetched_at)),
                ],
            ),
        );
        ui.add_space(6.0);
    }
    tiles(ui, cx, &report);
    match (&report.usage, &report.usage_error, usage_api) {
        (Some(u), _, _) => {
            ui.add_space(10.0);
            daily_chart(ui, cx, u);
            ui.add_space(8.0);
            models_table(ui, cx, u, &mut state.usage_table);
        }
        (None, Some(e), true) => {
            let unsupported = matches!(
                e,
                superba_core::Error::Unsupported { .. }
                    | superba_core::Error::NotImplemented { .. }
            );
            let body = if unsupported {
                cx.tr.t("usage.unavailable.body")
            } else if matches!(e, superba_core::Error::Http { status: 429, .. }) {
                cx.tr.t("usage.rate_limited")
            } else {
                error_text(cx.tr, e)
            };
            ui.add_space(10.0);
            callout(ui, cx, Status::Idle, &body);
        }
        _ => {}
    }
}

/// "dev@example.com · Pro · nb_…7a3c · updated 3 min ago · from the costs API".
fn status_line(ui: &mut Ui, cx: &Cx<'_>, acc: &AccountState) {
    let Some(report) = &acc.report else {
        return;
    };
    let mut parts = Vec::new();
    if let Some(s) = &report.summary {
        if let Some(e) = s.email.as_ref().or(s.display_name.as_ref()) {
            parts.push(e.clone());
        }
        if let Some(plan) = s
            .billing
            .as_ref()
            .and_then(|b| b.plan_title.clone())
            .or_else(|| s.plan_name.clone())
        {
            parts.push(plan);
        }
        if let Some(l4) = &s.key_last4 {
            parts.push(format!("…{l4}"));
        }
    }
    if let Some(u) = &report.usage {
        parts.push(cx.tr.f("usage.fetched", &[("ago", &cx.ago(u.fetched_at))]));
        if let Some(src) = u.cost_source {
            parts.push(
                cx.tr
                    .f("usage.source", &[("source", &cx.tr.text(&src.text()))]),
            );
        }
    }
    // The whole line on hover when it is shortened.
    crate::pages::elided(ui, Text::small(parts.join(" · "), cx.pal));
}

/// Balance, expiry, cost of the range, average per day.
fn tiles(ui: &mut Ui, cx: &Cx<'_>, report: &AccountReport) {
    let billing = report.summary.as_ref().and_then(|s| s.billing.as_ref());
    let mut tiles: Vec<(String, String)> = Vec::new();
    if let Some(b) = billing {
        if b.unlimited {
            tiles.push((
                cx.tr.t("providers.account.balance"),
                cx.tr.t("sites.balance.unlimited"),
            ));
        } else if let Some(r) = b.remaining {
            tiles.push((cx.tr.t("providers.account.balance"), r.to_string()));
        } else if let Some(u) = b.used {
            tiles.push((cx.tr.t("providers.account.used"), u.to_string()));
        }
        if let Some(until) = b.access_until {
            tiles.push((cx.tr.t("providers.account.until"), until.utc_date_string()));
        }
    }
    if let Some(u) = &report.usage {
        let total = u.total_cost;
        let days_with_cost = u.daily.iter().filter(|d| d.cost.is_some()).count().max(1) as i64;
        let avg = total.map(|t| Usd::from_micros(t.micros() / days_with_cost));
        tiles.push((
            cx.tr.f("usage.stat.total", &[("n", &u.days)]),
            total.map_or_else(|| "—".into(), |t| t.to_string()),
        ));
        tiles.push((
            cx.tr.t("usage.stat.avg"),
            avg.map_or_else(|| "—".into(), |t| t.to_string()),
        ));
    }
    if tiles.is_empty() {
        let body = report
            .summary_error
            .as_ref()
            .map(|e| error_text(cx.tr, e))
            .unwrap_or_else(|| cx.tr.t("sites.balance.none"));
        ui.label(Text::small(body, cx.pal));
        return;
    }
    let n = tiles.len().min(4);
    let gap = 20.0;
    let w = ((ui.available_width() - gap * (n as f32 - 1.0)) / n as f32).floor();
    ui.horizontal_top(|ui| {
        for (i, (label, value)) in tiles.iter().take(4).enumerate() {
            if i > 0 {
                ui.add_space(gap - ui.spacing().item_spacing.x);
            }
            ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(w);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(Text::caption(label.clone(), cx.pal));
                ui.add(
                    egui::Label::new(
                        RichText::new(value.clone())
                            .font(font(20.0, Weight::Semibold))
                            .color(cx.pal.text),
                    )
                    .truncate(),
                );
            });
        }
    });
    if let Some(b) = billing
        && let (Some(used), Some(limit)) = (b.used, b.limit)
        && !b.unlimited
        && limit.micros() > 0
    {
        ui.add_space(6.0);
        let share = (used.micros() as f32 / limit.micros() as f32).clamp(0.0, 1.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 220.0).max(120.0);
            let (r, _) = ui.allocate_exact_size(vec2(w, 6.0), Sense::hover());
            ui.painter().rect_filled(r, 3, cx.pal.active);
            let mut fill = r;
            fill.set_width((r.width() * share).max(2.0));
            ui.painter().rect_filled(fill, 3, cx.pal.text_muted);
            ui.label(Text::caption(
                cx.tr.f(
                    "providers.account.month_used",
                    &[("used", &used), ("limit", &limit)],
                ),
                cx.pal,
            ));
        });
    }
}

fn axis_label(v: f64) -> String {
    if v == 0.0 {
        "$0".into()
    } else if v >= 1000.0 {
        format!("${:.1}K", v / 1000.0)
    } else if v.fract() == 0.0 {
        format!("${v:.0}")
    } else {
        format!("${v:.1}")
    }
}

/// Daily cost as columns over UTC days (140 px high).
fn daily_chart(ui: &mut Ui, cx: &Cx<'_>, u: &UsageReport) {
    ui.label(Text::caption(cx.tr.t("usage.daily.title"), cx.pal));
    if u.cost_source.is_none() {
        ui.label(Text::small(cx.tr.t("usage.daily.no_cost"), cx.pal));
        return;
    }
    let days: &[DailyUsage] = &u.daily;
    let max = days
        .iter()
        .filter_map(|d| d.cost)
        .map(Usd::as_dollars)
        .fold(0.0, f64::max);
    let (top, step) = nice_axis(max);
    let height = 120.0;
    let axis_w = 44.0;
    let label_h = 18.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height + label_h), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(rect.min.x + axis_w, rect.min.y + 6.0),
        pos2(rect.max.x, rect.min.y + height),
    );
    let painter = ui.painter_at(rect.expand(2.0));
    let grid = Stroke::new(1.0, cx.pal.border);
    let mut v = 0.0;
    while v <= top + step * 0.01 {
        let y = plot.max.y - (v / top) as f32 * plot.height();
        painter.hline(plot.x_range(), y, grid);
        painter.text(
            pos2(plot.min.x - 8.0, y),
            Align2::RIGHT_CENTER,
            axis_label(v),
            mono_font(text::CAPTION - 1.0, Weight::Regular),
            cx.pal.text_subtle,
        );
        v += step;
    }
    let n = days.len().max(1);
    let slot = plot.width() / n as f32;
    let bar_w = (slot - 2.0).clamp(2.0, 22.0);
    let hover = response.hover_pos();
    let mut tip: Option<(Rect, &DailyUsage)> = None;
    let label_every = if n > 14 { 5 } else { 1 };
    for (i, d) in days.iter().enumerate() {
        let x = plot.min.x + slot * (i as f32 + 0.5);
        let value = d.cost.map_or(0.0, Usd::as_dollars);
        let h = ((value / top) as f32 * plot.height()).max(if value > 0.0 { 2.0 } else { 0.0 });
        let bar = Rect::from_min_max(
            pos2(x - bar_w / 2.0, plot.max.y - h),
            pos2(x + bar_w / 2.0, plot.max.y),
        );
        let col = Rect::from_min_max(
            pos2(x - slot / 2.0, plot.min.y),
            pos2(x + slot / 2.0, plot.max.y + label_h),
        );
        let hovered = hover.is_some_and(|p| col.contains(p));
        if hovered {
            painter.rect_filled(
                Rect::from_min_max(col.min, pos2(col.max.x, plot.max.y)),
                2,
                cx.pal.hover,
            );
            tip = Some((bar, d));
        }
        let color = if hover.is_some() && !hovered {
            cx.pal.text.gamma_multiply(0.45)
        } else {
            cx.pal.text
        };
        if h > 0.0 {
            let r = (bar_w / 2.0).min(4.0) as u8;
            painter.rect_filled(
                bar,
                egui::CornerRadius {
                    nw: r,
                    ne: r,
                    sw: 0,
                    se: 0,
                },
                color,
            );
        }
        if i % label_every == 0 || i + 1 == n {
            let date = d.start.utc_date_string();
            let short = date.get(5..).unwrap_or(&date).replace('-', "/");
            painter.text(
                pos2(x, plot.max.y + 4.0),
                Align2::CENTER_TOP,
                short,
                mono_font(text::CAPTION - 1.0, Weight::Regular),
                cx.pal.text_subtle,
            );
        }
    }
    painter.hline(
        plot.x_range(),
        plot.max.y,
        Stroke::new(1.0, cx.pal.border_strong),
    );
    if let Some((bar, d)) = tip {
        let lines = [
            d.start.utc_date_string(),
            d.cost.map_or_else(|| "—".into(), |c| c.to_string()),
            d.requests
                .map(|r| cx.tr.f("usage.tip.requests", &[("n", &group_thousands(r))]))
                .unwrap_or_default(),
        ];
        let anchor = pos2(bar.center().x, bar.min.y.min(plot.max.y - 4.0) - 8.0);
        egui::Area::new(ui.scope_id().with("providers-usage-tip"))
            .order(egui::Order::Tooltip)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(anchor)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(cx.pal.surface)
                    .stroke(Stroke::new(1.0, cx.pal.border_strong))
                    .corner_radius(radius::CONTROL + 2)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(Text::caption(lines[0].clone(), cx.pal));
                        ui.label(
                            RichText::new(&lines[1])
                                .font(font(text::BODY, Weight::Semibold))
                                .color(cx.pal.text),
                        );
                        if !lines[2].is_empty() {
                            ui.label(Text::small(lines[2].clone(), cx.pal));
                        }
                    });
            });
    }
}

/// Per-model usage as a paged table filling the rest of the tab.
fn models_table(ui: &mut Ui, cx: &Cx<'_>, u: &UsageReport, state: &mut PagedState) {
    if !u.usage_available || u.models.is_empty() {
        ui.label(Text::small(
            cx.tr.t(if u.usage_available {
                "usage.models.empty"
            } else {
                "usage.models.unavailable"
            }),
            cx.pal,
        ));
        return;
    }
    let headers = vec![
        cx.tr.t("usage.col.model"),
        cx.tr.t("usage.col.requests"),
        cx.tr.t("usage.col.input"),
        cx.tr.t("usage.col.cached"),
        cx.tr.t("usage.col.output"),
        cx.tr.t("usage.col.cost"),
    ];
    let note = cx.tr.t("providers.account.limit_note");
    let table = Table::new(&[2.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    let rows = &u.models;
    PagedTable::new("providers-usage-models", table, rows.len(), state)
        .label(cx.tr.t("usage.models.title"))
        .header(headers)
        .row_height(size::ROW)
        .footer(|ui| {
            ui.add(egui::Label::new(Text::caption(note, cx.pal)).truncate());
        })
        .show(ui, |ui, row, col| {
            let m = &rows[row];
            let t = |s: String| {
                RichText::new(s)
                    .font(font(text::SMALL, Weight::Regular))
                    .color(cx.pal.text)
            };
            match col {
                0 => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&m.model)
                                .font(mono_font(text::SMALL, Weight::Medium))
                                .color(cx.pal.text),
                        )
                        .truncate(),
                    );
                }
                1 => {
                    ui.label(t(group_thousands(m.requests)));
                }
                2 => {
                    ui.label(t(compact(m.input_tokens)));
                }
                3 => {
                    ui.label(t(compact(m.cached_input_tokens)));
                }
                4 => {
                    ui.label(t(compact(m.output_tokens)));
                }
                _ => {
                    ui.label(t(m.cost.map_or_else(|| "—".into(), |c| c.to_string())));
                }
            }
        });
}
