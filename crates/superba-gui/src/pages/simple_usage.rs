//! The Codex-focused usage page: totals, batch session actions, recent requests.

use std::time::Duration;

use egui::{Align, Layout, Ui};
use superba_core::{
    MigrationRequest, MigrationSelector, RequestHarness, RequestPage, RequestQuery, SwitchIntent,
    Timestamp, codex::PROVIDER_OPENAI,
};

use super::router::db::{Loader, Titles};
use super::router::requests::{self, Col};
use super::{Action, Cx, error_text};
use crate::design::icons::Icon;
use crate::design::widgets::log_table::{LogColumn, LogTable};
use crate::design::widgets::{Button, Card, Dropdown, Pager, Text};

const REFRESH: Duration = Duration::from_secs(5);
const DAY: Duration = Duration::from_secs(86_400);
/// The summary and actions stay fixed while the request list scrolls itself.
pub const MIN_HEIGHT: f32 = 480.0;
const COLUMNS: [Col; 9] = [
    Col::Date,
    Col::Model,
    Col::Session,
    Col::Provider,
    Col::Status,
    Col::Input,
    Col::Output,
    Col::CacheRead,
    Col::Cost,
];

/// Independent filters prevent advanced-mode selections leaking into simple mode.
#[derive(Debug, Default)]
pub struct State {
    /// The most recent Codex requests and totals over the whole day.
    pub requests: Loader<RequestPage>,
    /// Locally resolved session names.
    pub titles: Titles,
    /// Number of recent rows requested (the service caps this at 10,000).
    pub limit: u32,
    /// Zero-based request page; page size is 30 unless 50 was selected.
    pub page: usize,
    /// User-selected request page size (30 or 50).
    pub per_page: usize,
}

/// Clamp paging after a refresh so a shortened log never leaves a blank page.
pub fn visible_rows(state: &mut State, total: usize) -> std::ops::Range<usize> {
    state.per_page = if state.per_page == 50 { 50 } else { 30 };
    state.page = state
        .page
        .min(total.div_ceil(state.per_page).saturating_sub(1));
    let start = (state.page * state.per_page).min(total);
    start..(start + state.per_page).min(total)
}

/// The same fixed Codex-only range supplies both the totals and the request log.
pub fn query(now: Timestamp, limit: u32) -> RequestQuery {
    RequestQuery {
        from: Some(now.saturating_sub(DAY)),
        to: Some(now),
        harnesses: vec![RequestHarness::Codex],
        limit: limit.max(500),
        ..Default::default()
    }
}

/// A finish timestamp is known only when the request recorded its duration.
pub fn completed_at(row: &superba_core::RequestRow) -> Option<Timestamp> {
    row.duration_ms
        .map(|ms| row.started_at.saturating_add(Duration::from_millis(ms)))
}

/// Resolved by the planner at click time, including new and archived sessions.
pub fn migration_request() -> superba_core::SwitchRequest {
    crate::app::local_request(SwitchIntent::MigrateSessions {
        request: MigrationRequest {
            to_provider: PROVIDER_OPENAI.into(),
            selectors: vec![MigrationSelector::OtherProviders {
                except: PROVIDER_OPENAI.into(),
            }],
            include_archived: true,
            exclude: Vec::new(),
        },
    })
}

/// Draw the three vertically ordered sections within the page's Fit rectangle.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.requests.poll();
    state.titles.poll();
    let scan_due = cx.data.sessions.value.as_ref().is_none_or(|(report, _)| {
        cx.now
            .duration_since(report.scanned_at)
            .is_some_and(|age| age >= Duration::from_secs(30))
    });
    if scan_due && !cx.data.sessions.loading && cx.data.sessions.error.is_none() {
        out.push(Action::LoadSessions);
    }
    if state.requests.due(Some(REFRESH)) {
        let service = cx.service.clone();
        let q = query(cx.now, state.limit);
        state
            .requests
            .start(async move { service.router_requests(q).await });
    }
    if let Some(page) = &state.requests.value {
        state.titles.want(
            cx.service,
            page.rows.iter().filter_map(requests::session_key),
        );
    }
    ui.ctx()
        .request_repaint_after(if state.requests.loading() || state.titles.loading() {
            Duration::from_millis(100)
        } else {
            REFRESH
        });

    ui.horizontal(|ui| {
        ui.label(Text::page_title(cx.tr.t("simple.usage.title"), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::new(cx.tr.t("simple.refresh"))
                        .small()
                        .loading(state.requests.loading()),
                )
                .clicked()
            {
                state.requests.invalidate();
                out.push(Action::LoadSessions);
            }
        });
    });
    ui.add_space(12.0);
    totals(ui, cx, state);
    ui.add_space(12.0);
    sessions(ui, cx, out);
    ui.add_space(12.0);
    recent(ui, cx, state);
}

fn totals(ui: &mut Ui, cx: &Cx<'_>, state: &State) {
    Card::new().padding(16).show(ui, |ui| {
        ui.label(Text::section(cx.tr.t("simple.usage.stats"), cx.pal));
        ui.add_space(8.0);
        if let Some(page) = &state.requests.value {
            let t = &page.totals;
            let success = requests::success_rate(t).map_or_else(|| "—".into(), |n| format!("{n}%"));
            let cost = if t.cost.is_zero() && t.unpriced > 0 {
                "—".into()
            } else {
                requests::cost_cell(Some(t.cost))
            };
            let fresh_input = t
                .input_tokens
                .saturating_sub(t.cached_tokens)
                .saturating_sub(t.cache_write_tokens);
            let values = [
                ("simple.usage.requests", requests::tokens(Some(t.requests))),
                ("simple.usage.success", success),
                ("requests.col.input", requests::tokens(Some(fresh_input))),
                (
                    "requests.col.output",
                    requests::tokens(Some(t.output_tokens)),
                ),
                (
                    "requests.col.cache_read",
                    requests::tokens(Some(t.cached_tokens)),
                ),
                ("requests.col.cost", cost),
            ];
            // Two rows at narrow widths keep the numbers legible at 960 px.
            let columns = if ui.available_width() < 800.0 { 3 } else { 6 };
            for row in values.chunks(columns) {
                ui.columns(columns, |uis| {
                    for (cell, (key, value)) in uis.iter_mut().zip(row) {
                        cell.label(Text::caption(cx.tr.t(key), cx.pal));
                        cell.label(Text::title(value, cx.pal));
                    }
                });
            }
        } else if state.requests.error.is_none() {
            ui.spinner();
        }
        if let Some(error) = &state.requests.error {
            ui.label(Text::muted(error_text(cx.tr, error), cx.pal));
        }
    });
}

fn sessions(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    Card::new().padding(16).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(Text::section(cx.tr.t("simple.sessions.title"), cx.pal));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let help = ui.add(
                    Button::ghost(cx.tr.t("simple.sessions.what"))
                        .icon(Icon::Help)
                        .small(),
                );
                let text = cx.tr.t("simple.sessions.explanation");
                let help = help.on_hover_ui(|ui| {
                    ui.set_max_width(340.0);
                    ui.label(Text::body(&text, cx.pal));
                });
                egui::Popup::from_toggle_button_response(&help)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.set_max_width(340.0);
                        ui.add(egui::Label::new(Text::body(&text, cx.pal)).wrap());
                    });
            });
        });
        let loaded = cx.data.sessions.value.as_ref();
        let count = loaded.map(|(report, _)| report.hidden_under(PROVIDER_OPENAI));
        ui.horizontal_wrapped(|ui| {
            if let Some(n) = count {
                let key = if n > 0 {
                    "simple.sessions.pending"
                } else {
                    "simple.sessions.ready"
                };
                ui.label(Text::small(cx.tr.f(key, &[("n", &n)]), cx.pal));
            } else if let Some(error) = &cx.data.sessions.error {
                ui.label(Text::small(error_text(cx.tr, error), cx.pal));
            } else {
                ui.spinner();
            }
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let rollback =
                loaded.is_some_and(|(_, history)| history.iter().any(|m| !m.rolled_back));
            if rollback
                && ui
                    .add(
                        Button::new(cx.tr.t("simple.sessions.rollback"))
                            .enabled(!cx.busy && !cx.data.sessions.loading),
                    )
                    .on_hover_text(cx.tr.t("simple.sessions.hint"))
                    .clicked()
            {
                out.push(Action::SessionBatch { rollback: true });
            }
            if ui
                .add(
                    Button::primary(cx.tr.t("simple.sessions.migrate"))
                        .enabled(
                            !cx.busy && !cx.data.sessions.loading && count.is_some_and(|n| n > 0),
                        )
                        .loading(cx.data.sessions.loading),
                )
                .on_hover_text(cx.tr.t("simple.sessions.hint"))
                .clicked()
            {
                out.push(Action::SessionBatch { rollback: false });
            }
        });
    });
}

fn recent(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    ui.horizontal(|ui| {
        ui.label(Text::section(cx.tr.t("simple.usage.recent"), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(page) = &state.requests.value {
                if page.truncated && state.limit.max(500) < 10_000 {
                    if ui
                        .add(Button::new(cx.tr.t("simple.usage.more")).small())
                        .clicked()
                    {
                        state.limit = (state.limit.max(500) + 500).min(10_000);
                        state.requests.invalidate();
                    }
                } else if page.truncated {
                    ui.label(Text::caption(cx.tr.t("simple.usage.limit"), cx.pal));
                }
            }
        });
    });
    ui.add_space(6.0);
    let total = state.requests.value.as_ref().map_or(0, |p| p.rows.len());
    let visible = visible_rows(state, total);
    let Some(page) = &state.requests.value else {
        return;
    };
    if page.rows.is_empty() {
        ui.label(Text::muted(cx.tr.t("simple.usage.empty"), cx.pal));
        return;
    }
    let names =
        requests::ProviderNames::new(cx.tr, cx.data.profiles.value.as_ref(), &cx.data.sites);
    let mut columns: Vec<_> = COLUMNS
        .iter()
        .map(|c| {
            let col = if c.numeric() {
                LogColumn::right(cx.tr.t(c.key()), c.width())
            } else {
                LogColumn::left(cx.tr.t(c.key()), c.width())
            }
            .min(c.min_width())
            .grow(c.grow())
            .fixed_order();
            if let Some(rank) = c.drop_rank() {
                col.drop_rank(rank)
            } else {
                col
            }
        })
        .collect();
    columns.insert(
        1,
        LogColumn::left(cx.tr.t("simple.usage.completed"), 78.0)
            .min(72.0)
            .fixed_order(),
    );
    let rect = ui.available_rect_before_wrap();
    let table = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 38.0));
    crate::design::layout::fixed(ui, table, |ui| {
        LogTable::new(
            ("simple-codex-requests", state.page, state.per_page),
            &columns,
            visible.len(),
        )
        .label(cx.tr.t("simple.usage.recent"))
        .show(ui, |row, column| {
            let mut r = page.rows[visible.start + row].clone();
            if column == 1 {
                return completed_at(&r).map_or_else(
                    || crate::design::widgets::log_table::LogCell::text("—"),
                    |at| {
                        crate::design::widgets::log_table::LogCell::text(requests::date_cell(
                            at, cx.now,
                        ))
                        .tooltip(requests::full_time(at))
                    },
                );
            }
            names.relabel_row(&mut r);
            requests::cell(
                cx.tr,
                state.titles.map(),
                &r,
                COLUMNS[if column > 1 { column - 1 } else { column }],
                cx.now,
            )
        });
    });
    let footer = egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - 30.0), rect.max);
    crate::design::layout::fixed(ui, footer, |ui| {
        ui.horizontal(|ui| {
            let before = state.per_page;
            Dropdown::new("simple-request-page-size")
                .label(cx.tr.t("simple.usage.per_page"))
                .show_options(
                    ui,
                    &mut state.per_page,
                    &[
                        (30, cx.tr.f("simple.usage.page_size", &[("n", &30)])),
                        (50, cx.tr.f("simple.usage.page_size", &[("n", &50)])),
                    ],
                );
            if before != state.per_page {
                state.page = 0;
            }
            let pager = Pager::new(
                "simple-request-pages",
                &mut state.page,
                total,
                state.per_page,
            )
            .labels(cx.tr.t("simple.usage.prev"), cx.tr.t("simple.usage.next"));
            if ui.available_width() < 520.0 {
                pager.compact().show(ui);
            } else {
                pager.show(ui);
            }
        });
    });
}
