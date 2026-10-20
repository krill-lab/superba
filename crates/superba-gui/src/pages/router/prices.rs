//! Prices of the cost estimates (路由 page → 价格): the price table in use (public
//! list prices from Superba's feeds), a ratio per provider (relays usually sell
//! at a fraction of list price; default × 1) for every key-based provider, in
//! the router or not (costs of requests from Codex's session files use it too,
//! review ux-visual #4) and, for providers with a usage API
//! (Krill), "calibrate from billing": ratio = billed / estimated over the last
//! 7 days, shown before it is saved. Per-model absolute prices are edited on the
//! Models page. Every cost in the log is an estimate (`≈`).

use std::sync::Arc;

use egui::{Align, Layout, Ui};

use crate::design::tokens::{size, space};
use crate::design::widgets::Table;
use superba_core::{
    AccountRequest, Error, FeedOrigin, FeedsStatus, Multiplier, PriceTable, ProfileId, RequestRow,
    RequestStatus, Result, RouterStatsSettings, StatsGroupBy, StatsQuery, SwitchService,
    UsageQuery, Usd,
};
use superba_models::price::{PriceBasis, source_label};

use super::db::Loader;
use super::requests::{DASH, cost_text};
use super::{Dialog, State};
use crate::design::widgets::info_card::InfoLine;
use crate::design::widgets::{Button, Text, dialog_body};
use crate::i18n::Tr;
use crate::pages::{Action, Cx, error_text};

/// Days compared with billing.
pub const CALIBRATION_DAYS: u32 = 7;

/// The lines of the detail pane's cost card: the estimate, what it is based on
/// (OpenAI's list price with the date it was read and its source page, or the
/// price set on the Models page), and for a request without a cost why there
/// is none ("no official price; set one in Models").
pub fn cost_lines(tr: &Tr, row: &RequestRow) -> Vec<InfoLine> {
    // Short lines (cards are a third of the pane wide); the full wording is the
    // hover text.
    let line = |value: String, tip: Option<String>| InfoLine {
        value,
        tooltip: tip,
        ..InfoLine::default()
    };
    let Some(cost) = row.cost else {
        let why = if let Some(tool) = row.source.imported_from() {
            line(
                tr.f("requests.tip.unpriced_imported", &[("tool", &tool.name())]),
                None,
            )
        } else if row.input_tokens.is_some() || row.output_tokens.is_some() {
            line(
                tr.t("requests.detail.unpriced"),
                Some(tr.t("requests.tip.unpriced")),
            )
        } else if row.status == RequestStatus::Cancelled {
            line(tr.t("requests.tip.cancelled_cost"), None)
        } else {
            line(tr.t("requests.detail.no_usage"), None)
        };
        return vec![line(DASH.to_owned(), None), why];
    };
    if let Some(tool) = row.source.imported_from() {
        return vec![
            line(cost_text(Some(cost)), None),
            line(
                tr.f("requests.tip.cost_imported", &[("tool", &tool.name())]),
                None,
            ),
        ];
    }
    let mut lines = vec![line(cost_text(Some(cost)), Some(tr.t("requests.tip.cost")))];
    match row.price_rev.as_deref().and_then(PriceBasis::parse) {
        Some(PriceBasis::Override) => lines.push(line(
            tr.t("requests.detail.price_override"),
            Some(tr.t("requests.detail.price_override_tip")),
        )),
        Some(PriceBasis::List { rev, source }) => {
            lines.push(if rev.is_empty() {
                line(tr.t("requests.tip.cost"), None)
            } else {
                line(
                    tr.f("requests.detail.price_list", &[("rev", &rev)]),
                    Some(tr.t("requests.tip.cost")),
                )
            });
            if let Some(url) = source {
                lines.push(InfoLine {
                    label: Some(tr.t("requests.detail.price_source")),
                    value: source_label(&url),
                    tooltip: Some(url),
                    ..InfoLine::default()
                });
            }
        }
        None => lines.push(line(tr.t("requests.tip.cost"), None)),
    }
    lines
}

/// The ratio of `profile` (× 1 when not set).
pub fn ratio_of(stats: &RouterStatsSettings, profile: &ProfileId) -> Multiplier {
    stats
        .prices
        .ratio_by_profile
        .get(profile)
        .copied()
        .unwrap_or(Multiplier::ONE)
}

/// `stats` with the ratio of `profile` set (× 1 removes the entry).
pub fn with_ratio(
    stats: &RouterStatsSettings,
    profile: &ProfileId,
    ratio: Multiplier,
) -> RouterStatsSettings {
    let mut next = stats.clone();
    if ratio == Multiplier::ONE {
        next.prices.ratio_by_profile.remove(profile);
    } else {
        next.prices.ratio_by_profile.insert(profile.clone(), ratio);
    }
    next
}

/// Parse a ratio typed by the user: `0.3`, `×0.3`, `x 0.3`, `30%`. Must be
/// above 0 and at most 100.
pub fn parse_ratio(text: &str) -> Option<Multiplier> {
    let t = text.trim().trim_start_matches(['×', 'x', 'X']).trim();
    let value = match t.strip_suffix('%') {
        Some(p) => p.trim().parse::<f64>().ok()? / 100.0,
        None => t.parse::<f64>().ok()?,
    };
    (value > 0.0 && value <= 100.0)
        .then(|| Multiplier::from_f64(value))
        .flatten()
        .filter(|m| m.ppm() > 0)
}

/// The ratio that makes the estimate match the bill: costs in the log were
/// computed with `current`, so the list-price estimate is `estimated / current`
/// and the new ratio is `billed / (estimated / current)`. `None` without data.
pub fn calibrated_ratio(billed: Usd, estimated: Usd, current: Multiplier) -> Option<Multiplier> {
    if billed.micros() <= 0 || estimated.micros() <= 0 || current.ppm() == 0 {
        return None;
    }
    let ratio = billed.as_dollars() * current.as_f64() / estimated.as_dollars();
    Multiplier::from_f64((ratio * 1e4).round() / 1e4).filter(|m| m.ppm() > 0)
}

/// Billed vs estimated cost of one provider over the last days.
#[derive(Clone, Debug, PartialEq)]
pub struct Calibration {
    /// Profile (router member).
    pub profile: ProfileId,
    /// Its name.
    pub name: String,
    /// Billed cost (`cost_usd` of the usage API).
    pub billed: Usd,
    /// Local estimate (with the current ratio).
    pub estimated: Usd,
    /// The ratio the estimate used.
    pub current: Multiplier,
    /// Per UTC day: (`YYYY-MM-DD`, estimated, billed).
    pub days: Vec<(String, Option<Usd>, Option<Usd>)>,
}

impl Calibration {
    /// The suggested ratio.
    pub fn ratio(&self) -> Option<Multiplier> {
        calibrated_ratio(self.billed, self.estimated, self.current)
    }
}

/// Load billing (the provider's usage API; rate-limited, so only on request) and
/// the local estimate over the same UTC days.
pub async fn load_calibration(
    service: Arc<dyn SwitchService>,
    profile: ProfileId,
    name: String,
    current: Multiplier,
) -> Result<Calibration> {
    let report = service
        .account(AccountRequest {
            profile_id: Some(profile.clone()),
            line_id: None,
            usage: Some(UsageQuery::last_days(CALIBRATION_DAYS)),
        })
        .await?;
    let usage = match (report.usage, report.usage_error) {
        (Some(u), _) => u,
        (None, Some(e)) => return Err(e),
        (None, None) => return Err(Error::other("the provider sent no usage report")),
    };
    let stats = service
        .router_stats(StatsQuery {
            source: None,
            from: Some(usage.start),
            to: Some(usage.end),
            by: StatsGroupBy::Day,
            member: Some(profile.clone()),
            utc_offset_minutes: 0,
            ..StatsQuery::default()
        })
        .await?;
    let billed = usage.total_cost.unwrap_or_else(|| {
        usage
            .daily
            .iter()
            .filter_map(|d| d.cost)
            .fold(Usd::ZERO, Usd::saturating_add)
    });
    let days = usage
        .daily
        .iter()
        .map(|d| {
            let (y, m, day) = d.start.utc_date();
            let key = format!("{y:04}-{m:02}-{day:02}");
            let est = stats
                .buckets
                .iter()
                .find(|b| b.key == key)
                .map(|b| b.totals.cost);
            (key, est, d.cost)
        })
        .collect();
    Ok(Calibration {
        profile,
        name,
        billed,
        estimated: stats.totals.cost,
        current,
        days,
    })
}

/// Edits and reads of the price section.
#[derive(Debug, Default)]
pub struct Prices {
    /// Ratio being edited: (profile, text).
    pub editing: Option<(ProfileId, String)>,
    /// Feed state (price table origin).
    pub feeds: Loader<FeedsStatus>,
    /// Billing comparison being loaded / shown.
    pub calibration: Loader<Calibration>,
    /// The price table in use (`SwitchService::price_table`), for "View…".
    pub table: Loader<PriceTable>,
    /// Give the ratio field the keyboard focus when it appears (after 编辑).
    pub focus: bool,
}

/// The providers of the price list, router members first: every key-based
/// provider (the official login is billed by ChatGPT), whether the router is
/// on or not: (profile, name, usage API).
pub fn providers(cx: &Cx<'_>) -> Vec<(ProfileId, String, bool)> {
    let Some(set) = cx.data.profiles.value.as_ref() else {
        return Vec::new();
    };
    let members = &set.settings.router.members;
    let mut list: Vec<&superba_core::Profile> = set
        .profiles
        .iter()
        .filter(|p| {
            p.builtin != Some(superba_core::BuiltinProfile::Official)
                && cx
                    .data
                    .sites
                    .iter()
                    .find(|s| s.id == p.site_id)
                    .is_none_or(|s| s.kind != superba_core::SiteKind::Official)
        })
        .collect();
    list.sort_by_key(|p| {
        members
            .iter()
            .position(|m| m == &p.id)
            .unwrap_or(usize::MAX)
    });
    list.into_iter()
        .map(|p| {
            let site = cx.data.sites.iter().find(|s| s.id == p.site_id);
            let usage = site.is_some_and(|s| s.capabilities.usage_api);
            let name = if p.name.is_empty() {
                site.map(|s| cx.tr.text(&s.name))
                    .unwrap_or_else(|| p.id.to_string())
            } else {
                p.name.clone()
            };
            (p.id.clone(), name, usage)
        })
        .collect()
}

fn origin_text(cx: &Cx<'_>, status: &FeedsStatus) -> String {
    match &status.source {
        FeedOrigin::Mirror { name } => name.clone(),
        FeedOrigin::Cache => cx.tr.t("prices.source.cache"),
        _ => cx.tr.t("prices.source.builtin"),
    }
}

/// Start loading a billing comparison for `profile`.
pub fn start_calibration(cx: &Cx<'_>, state: &mut State, profile: ProfileId, name: String) {
    let current = ratio_of(&state.stats_settings(cx), &profile);
    let service = cx.service.clone();
    state.prefs.prices.calibration.reset();
    state
        .prefs
        .prices
        .calibration
        .start(async move { load_calibration(service, profile, name, current).await });
}

/// Draw the price section: the price table in use, then one row per
/// key-based provider with its ratio (right-aligned) and its actions in the
/// last column — 按账单校准… where the provider reports usage, 编辑 — and,
/// while a ratio is edited, the field in the ratio column with 保存 / 取消.
pub fn section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let p = &mut state.prefs.prices;
    p.feeds.poll();
    if p.calibration.poll()
        && let Some(c) = p.calibration.value.clone()
    {
        state.dialog = Some(Dialog::Calibrate(c));
    }
    let p = &mut state.prefs.prices;
    if p.feeds.due(None) {
        let service = cx.service.clone();
        p.feeds.start(async move { service.feeds_status().await });
    }
    crate::pages::section(
        ui,
        cx,
        &cx.tr.t("prices.title"),
        Some(&cx.tr.t("prices.note")),
    );
    // The revision in use (the feed's), else the newest recorded row's.
    let rev = state
        .prefs
        .prices
        .feeds
        .value
        .as_ref()
        .and_then(|f| f.prices_rev.clone())
        .or_else(|| {
            state.recent.shown.iter().find_map(|r| {
                let basis = PriceBasis::parse(r.price_rev.as_deref()?)?;
                basis.rev().map(str::to_owned)
            })
        })
        .unwrap_or_default();
    let source = match &state.prefs.prices.feeds.value {
        Some(s) => origin_text(cx, s),
        None => cx.tr.t("prices.source.builtin"),
    };
    let mut view = false;
    crate::pages::superba::row(
        ui,
        cx,
        &cx.tr.t("prices.table_label"),
        &cx.tr
            .f("prices.table", &[("source", &source), ("rev", &rev)]),
        true,
        |ui| {
            view = ui
                .add(Button::new(cx.tr.t("prices.view")).small())
                .on_hover_text(cx.tr.t("prices.table_tip"))
                .clicked();
        },
    );
    if view {
        open_table(cx, state);
    }
    let stats = state.stats_settings(cx);
    let providers = providers(cx);
    if providers.is_empty() {
        ui.add(egui::Label::new(Text::small(cx.tr.t("prices.no_providers"), cx.pal)).wrap());
        return;
    }
    let calibrating = state.prefs.prices.calibration.loading();
    let headers = [
        cx.tr.t("prices.col.provider"),
        cx.tr.t("prices.col.ratio"),
        String::new(),
    ];
    // The actions column holds the widest pair of buttons of any row.
    let w = |b: Button| b.small().desired_size(ui).x;
    let actions_w = (w(Button::ghost(cx.tr.t("prices.calibrate")))
        + w(Button::ghost(cx.tr.t("prices.edit"))))
    .max(w(Button::new(cx.tr.t("prices.save"))) + w(Button::ghost(cx.tr.t("action.cancel"))))
        + space::XS
        + space::M;
    let ratio_w = size::CONTROL * 3.0;
    let table = Table::new(&[1.0, 0.0, 0.0])
        .min_widths(&[0.0, ratio_w, actions_w])
        .right_aligned(&[1, 2]);
    table.header(ui, &headers);
    let n = providers.len();
    for (i, (id, name, usage)) in providers.into_iter().enumerate() {
        let ratio = ratio_of(&stats, &id);
        let editing = state
            .prefs
            .prices
            .editing
            .as_ref()
            .filter(|(p, _)| p == &id)
            .map(|(_, t)| t.clone());
        let mut text = editing.clone();
        let mut save = false;
        let mut cancel = false;
        let mut edit = false;
        let mut calibrate = false;
        let mut enter = false;
        let mut escape = false;
        let focus = state.prefs.prices.focus && text.is_some();
        ui.push_id(&id, |ui| {
            table.row(ui, size::ROW, i + 1 == n, |ui, col| match col {
                0 => {
                    ui.label(Text::body(&name, cx.pal));
                }
                1 => match &mut text {
                    Some(t) => {
                        let r = ui
                            .add(
                                egui::TextEdit::singleline(t)
                                    .align(egui::Align2::RIGHT_CENTER)
                                    .desired_width(ui.available_width()),
                            )
                            .accessible_name(cx.tr.f("prices.ratio_of", &[("provider", &name)]));
                        if focus {
                            r.request_focus();
                        }
                        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        escape = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                    }
                    None => {
                        ui.label(Text::body(
                            cx.tr.f("prices.ratio", &[("ratio", &ratio)]),
                            cx.pal,
                        ));
                    }
                },
                _ => {
                    ui.spacing_mut().item_spacing.x = space::XS;
                    if let Some(t) = &text {
                        let valid = parse_ratio(t).is_some();
                        cancel = ui
                            .add(Button::ghost(cx.tr.t("action.cancel")).small())
                            .clicked();
                        save = ui
                            .add(Button::new(cx.tr.t("prices.save")).small().enabled(valid))
                            .clicked();
                    } else {
                        edit = ui
                            .add(Button::ghost(cx.tr.t("prices.edit")).small())
                            .clicked();
                        if usage {
                            calibrate = ui
                                .add(
                                    Button::ghost(cx.tr.t("prices.calibrate"))
                                        .small()
                                        .loading(calibrating)
                                        .enabled(!calibrating),
                                )
                                .on_hover_text(cx.tr.t("prices.calibrate_tip"))
                                .clicked();
                        }
                    }
                }
            });
        });
        let valid = text.as_deref().and_then(parse_ratio);
        if let Some(t) = &text
            && valid.is_none()
            && !t.trim().is_empty()
        {
            // Under the row being edited, before the next provider, at the
            // cells' left padding (under the provider's name).
            ui.horizontal(|ui| {
                ui.add_space(space::M);
                ui.label(Text::caption(cx.tr.t("prices.invalid"), cx.pal).color(cx.pal.red));
            });
        }
        if focus {
            state.prefs.prices.focus = false;
        }
        if (save || enter)
            && let Some(m) = valid
        {
            let next = with_ratio(&stats, &id, m);
            state.prefs.prices.editing = None;
            state.save_stats(cx, next);
        } else if cancel || escape {
            state.prefs.prices.editing = None;
        } else if edit {
            state.prefs.prices.editing = Some((id.clone(), ratio.to_string()));
            state.prefs.prices.focus = true;
        } else if let Some(t) = text {
            state.prefs.prices.editing = Some((id.clone(), t));
        }
        if calibrate {
            start_calibration(cx, state, id.clone(), name.clone());
        }
    }
    if let Some(e) = &state.prefs.prices.calibration.error {
        ui.add(
            egui::Label::new(Text::caption(error_text(cx.tr, e), cx.pal).color(cx.pal.red)).wrap(),
        );
    }
}

/// Body of the calibration dialog.
pub fn calibrate_dialog(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    c: &Calibration,
    close: &mut bool,
    out: &mut Vec<Action>,
) {
    let _ = out;
    ui.label(Text::title(
        cx.tr.f("prices.calibrate_title", &[("provider", &c.name)]),
        cx.pal,
    ));
    let ratio = c.ratio();
    let money = |u: Usd| u.amount(2);
    dialog_body(ui, |ui| {
        ui.add(
            egui::Label::new(Text::small(
                match ratio {
                    Some(r) => cx.tr.f(
                        "prices.calibrate_result",
                        &[
                            ("billed", &money(c.billed)),
                            ("est", &money(c.estimated)),
                            ("ratio", &r),
                        ],
                    ),
                    None => cx.tr.t("prices.calibrate_no_data"),
                },
                cx.pal,
            ))
            .wrap(),
        );
        let table = Table::new(&[1.0, 1.0, 1.0]).right_aligned(&[1, 2]);
        table.header(
            ui,
            &[
                cx.tr.t("prices.day"),
                cx.tr.t("prices.estimated"),
                cx.tr.t("prices.billed"),
            ],
        );
        let dollars =
            |u: Option<Usd>| u.map_or_else(|| "—".into(), |u| format!("${}", u.amount(2)));
        for (i, (day, est, billed)) in c.days.iter().enumerate() {
            table.row(ui, size::ROW, i + 1 == c.days.len(), |ui, col| {
                ui.label(
                    Text::small(
                        match col {
                            0 => day.clone(),
                            1 => dollars(*est),
                            _ => dollars(*billed),
                        },
                        cx.pal,
                    )
                    .color(cx.pal.text),
                );
            });
        }
        ui.add_space(space::S);
        ui.add(egui::Label::new(Text::caption(cx.tr.t("prices.calibrate_note"), cx.pal)).wrap());
    });
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::primary(cx.tr.t("prices.calibrate_apply"))
                        .small()
                        .enabled(ratio.is_some()),
                )
                .clicked()
                && let Some(r) = ratio
            {
                let next = with_ratio(&state.stats_settings(cx), &c.profile, r);
                state.save_stats(cx, next);
                *close = true;
            }
            if ui
                .add(Button::ghost(cx.tr.t("action.cancel")).small())
                .clicked()
            {
                *close = true;
            }
        });
    });
}

/// Open the price table dialog (loads the table in use the first time).
pub fn open_table(cx: &Cx<'_>, state: &mut State) {
    let t = &mut state.prefs.prices.table;
    if t.value.is_none() && !t.loading() {
        let service = cx.service.clone();
        t.start(async move { service.price_table().await });
    }
    state.dialog = Some(Dialog::Prices);
}

/// Rows of the price table dialog: model, input, cached input, output (USD per
/// 1M tokens) and the source page's host.
pub fn table_rows(table: &PriceTable) -> Vec<[String; 5]> {
    table
        .entries
        .iter()
        .map(|(model, e)| {
            let host = e
                .source
                .as_deref()
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "—".to_owned());
            [
                model.clone(),
                e.input.to_string(),
                e.cached_input.unwrap_or(e.input).to_string(),
                e.output.to_string(),
                host,
            ]
        })
        .collect()
}

/// The price table dialog body.
pub(super) fn table_dialog(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, close: &mut bool) {
    let t = &mut state.prefs.prices.table;
    t.poll();
    ui.label(Text::title(cx.tr.t("prices.view_title"), cx.pal));
    dialog_body(ui, |ui| match (&t.value, &t.error) {
        (Some(table), _) if table.entries.is_empty() => {
            ui.add(egui::Label::new(Text::small(cx.tr.t("prices.view_empty"), cx.pal)).wrap());
        }
        (Some(table), _) => {
            ui.label(Text::small(
                cx.tr.f("prices.view_body", &[("rev", &table.rev)]),
                cx.pal,
            ));
            ui.add_space(space::S);
            let rows = table_rows(table);
            let headers = [
                "prices.col.model",
                "prices.col.input",
                "prices.col.cached",
                "prices.col.output",
                "prices.col.source",
            ];
            // The output column keeps the inset before the hosts on top of the
            // width the other prices get.
            let table = Table::new(&[1.6, 0.8, 0.8, 0.8, 1.0])
                .min_widths(&[0.0, 0.0, 0.0, Table::RIGHT_INSET, 0.0])
                .right_aligned(&[1, 2, 3]);
            table.header(ui, &headers.map(|h| cx.tr.t(h)));
            // The dialog's body scrolls the rows (its title and 关闭 stay).
            for (i, row) in rows.iter().enumerate() {
                table.row(ui, size::ROW, i + 1 == rows.len(), |ui, col| {
                    let text = Text::small(&row[col], cx.pal);
                    // Model ids are ids: monospace; prices and hosts are not.
                    ui.label(if col == 0 {
                        Text::mono(&row[col], cx.pal)
                    } else if col == 4 {
                        text
                    } else {
                        text.color(cx.pal.text)
                    });
                });
            }
        }
        (None, Some(e)) => {
            ui.label(Text::small(error_text(cx.tr, e), cx.pal));
        }
        _ => {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
            ui.label(Text::small(cx.tr.t("prices.view_loading"), cx.pal));
        }
    });
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::ghost(cx.tr.t("action.close")).small())
                .clicked()
            {
                *close = true;
            }
        });
    });
}
