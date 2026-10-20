//! Prices on the Models page (user requirement 115; until alpha.8 they were on
//! the Router page under 高级 → 价格): each row's price per 1M tokens with
//! where it comes from (the built-in table of official list prices, or the
//! user's own price for this provider), its long-context bands and time
//! windows on hover; and 「价格与校准」, a dialog with the price table in use,
//! each key-based provider's ratio and the calibration against its bill.
//!
//! The ratio and the calibration are per provider, not per model, and they
//! apply whether or not the router is on (costs from Codex's session files use
//! them too), so they sit in one dialog of the page rather than on the router
//! model list's provider chips (those only exist through the router). Cost
//! estimates keep reading the same settings (`RouterStatsSettings::prices`).

use egui::{Align, Layout, Ui};
use superba_core::{
    FeedOrigin, FeedsStatus, PriceEntry, PriceTable, PriceWindow, ProfileId, RouterStatsSettings,
};

use super::model::dollars;
use crate::design::tokens::{size, space};
use crate::design::widgets::{Button, Table, Text, dialog, dialog_body};
use crate::pages::Cx;
use crate::pages::router::db::Loader;
use crate::pages::router::prices::{
    Calibration, load_calibration, parse_ratio, providers, ratio_of, table_rows, with_ratio,
};
use crate::runtime::ReadTask;

/// State of the 「价格与校准」 dialog.
#[derive(Debug, Default)]
pub struct PriceTools {
    /// The dialog is open.
    pub open: bool,
    /// The full price table is listed in the dialog.
    pub show_table: bool,
    /// Ratio being edited: (profile, text).
    pub editing: Option<(ProfileId, String)>,
    /// Where the price table comes from.
    feeds: Loader<FeedsStatus>,
    /// The billing comparison being loaded / shown.
    calibration: Loader<Calibration>,
    /// Price settings saved but not yet in the loaded profile set.
    pending: Option<RouterStatsSettings>,
    /// The save in flight.
    save: Option<ReadTask<()>>,
    /// The last save error.
    error: Option<String>,
}

/// Hover text of a row's price: where it comes from, the base prices, cache
/// writes, long-context bands (「>272K：输入 $x / 输出 $y」) and time windows.
pub fn price_tip(cx: &Cx<'_>, shown: Option<&(PriceEntry, bool)>) -> String {
    let Some((p, own)) = shown else {
        return cx.tr.t("models.price.src_none");
    };
    let mut lines = vec![if *own {
        cx.tr.t("models.price.src_override")
    } else {
        let host = p
            .source
            .as_deref()
            .map(superba_models::price::source_label)
            .unwrap_or_default();
        cx.tr.f(
            "models.price.src_table",
            &[
                ("source", &host),
                ("rev", &p.rev.clone().unwrap_or_default()),
            ],
        )
    }];
    lines.push(cx.tr.f(
        "models.price.base_line",
        &[
            ("input", &dollars(p.input)),
            ("cached", &dollars(p.cached_input.unwrap_or(p.input))),
            ("output", &dollars(p.output)),
        ],
    ));
    if let Some(w) = p.cache_write {
        lines.push(match p.cache_write_1h {
            Some(h) => cx.tr.f(
                "models.price.write_line_1h",
                &[("write", &dollars(w)), ("hour", &dollars(h))],
            ),
            None => cx
                .tr
                .f("models.price.write_line", &[("write", &dollars(w))]),
        });
    }
    for b in &p.long_context {
        lines.push(cx.tr.f(
            "models.price.band_line",
            &[
                ("above", &tokens_short(b.above_input_tokens)),
                ("input", &dollars(b.input)),
                ("output", &dollars(b.output)),
            ],
        ));
    }
    for w in &p.windows {
        lines.push(window_line(cx, w));
    }
    if !p.tier_prices.is_empty() {
        let tiers: Vec<&str> = p.tier_prices.keys().map(String::as_str).collect();
        lines.push(
            cx.tr
                .f("models.price.tiers_line", &[("tiers", &tiers.join(", "))]),
        );
    }
    lines.push(cx.tr.t("models.price.tip"));
    lines.join("\n")
}

/// `272K`, `1M`.
fn tokens_short(n: u64) -> String {
    // A ">= 200K" band is stored as "above 199,999".
    let n = if n % 1000 == 999 { n + 1 } else { n };
    if n >= 1_000_000 && n % 1_000_000 == 0 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1000 {
        format!("{}K", n / 1000)
    } else {
        n.to_string()
    }
}

/// 「off-peak 16:30–00:30 UTC：输入 $x / 输出 $y」.
fn window_line(cx: &Cx<'_>, w: &PriceWindow) -> String {
    let zone = match w.utc_offset_minutes {
        0 => "UTC".to_owned(),
        m => format!(
            "UTC{}{}{}",
            if m < 0 { '-' } else { '+' },
            m.abs() / 60,
            if m % 60 == 0 {
                String::new()
            } else {
                format!(":{:02}", m.abs() % 60)
            }
        ),
    };
    let days = match w.weekdays.as_slice() {
        [] => String::new(),
        [1, 2, 3, 4, 5] => format!(" · {}", cx.tr.t("models.price.weekdays")),
        [6, 7] => format!(" · {}", cx.tr.t("models.price.weekend")),
        d => format!(
            " · {}",
            d.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
        ),
    };
    let span = if w.start == w.end {
        cx.tr.t("models.price.all_day")
    } else {
        format!("{}–{}", w.start, w.end)
    };
    cx.tr.f(
        "models.price.window_line",
        &[
            ("label", &w.label.clone().unwrap_or_default()),
            ("span", &format!("{span} {zone}{days}")),
            ("input", &dollars(w.input)),
            ("output", &dollars(w.output)),
        ],
    )
}

impl PriceTools {
    /// The price settings in use: a save in flight, else the loaded ones.
    fn settings(&self, cx: &Cx<'_>) -> RouterStatsSettings {
        self.pending.clone().unwrap_or_else(|| {
            cx.data
                .profiles
                .value
                .as_ref()
                .map(|p| p.settings.router.stats.clone())
                .unwrap_or_default()
        })
    }

    fn save_settings(&mut self, cx: &Cx<'_>, next: RouterStatsSettings) {
        self.pending = Some(next.clone());
        self.error = None;
        let service = cx.service.clone();
        self.save = Some(crate::runtime::task(async move {
            match service.router_set_stats(next.clone()).await {
                Ok(_) => Ok(()),
                // No router support: the settings file still keeps them.
                Err(e) if e.is_not_implemented() => {
                    let mut settings = service.profile_set().await?.settings;
                    settings.router.stats = next;
                    service.save_settings(settings).await.map(|_| ())
                }
                Err(e) => Err(e),
            }
        }));
    }

    fn poll(&mut self, cx: &Cx<'_>) {
        self.feeds.poll();
        self.calibration.poll();
        if let Some(task) = &self.save
            && let Some(result) = task.poll()
        {
            self.save = None;
            if let Err(e) = result {
                self.pending = None;
                self.error = Some(crate::pages::error_text(cx.tr, &e));
            }
        }
        // The loaded settings caught up with the save.
        if self.save.is_none()
            && let Some(p) = &self.pending
            && cx
                .data
                .profiles
                .value
                .as_ref()
                .is_some_and(|s| &s.settings.router.stats == p)
        {
            self.pending = None;
        }
        if self.open && self.feeds.due(None) {
            let service = cx.service.clone();
            self.feeds
                .start(async move { service.feeds_status().await });
        }
    }
}

/// The 「价格与校准…」 button of the page's bottom row.
pub(super) fn open_button(ui: &mut Ui, cx: &Cx<'_>, tools: &mut PriceTools) {
    if ui
        .add(Button::ghost(cx.tr.t("models.prices.open")).small())
        .on_hover_text(cx.tr.t("models.prices.open_tip"))
        .clicked()
    {
        tools.open = true;
    }
}

/// The 「价格与校准」 dialog: the price table in use, then one row per
/// key-based provider with its ratio, 编辑 and (with a usage API) 按账单校准….
pub(super) fn tools_dialog(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    tools: &mut PriceTools,
    table: Option<&PriceTable>,
) {
    tools.poll(cx);
    if !tools.open {
        return;
    }
    let mut close = false;
    let r = dialog(ctx, egui::Id::unique("models-price-tools"), 620.0, |ui| {
        ui.label(Text::h2(cx.tr.t("models.prices.title"), cx.pal));
        dialog_body(ui, |ui| {
            ui.add(egui::Label::new(Text::small(cx.tr.t("prices.note"), cx.pal)).wrap());
            ui.add_space(space::S);
            table_row(ui, cx, tools, table);
            ui.add_space(space::S);
            ratio_rows(ui, cx, tools);
            calibration_view(ui, cx, tools);
            if let Some(e) = &tools.error {
                ui.label(Text::caption(e, cx.pal).color(cx.pal.red));
            }
        });
        ui.add_space(space::S);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::ghost(cx.tr.t("action.close")).small().kbd("Esc"))
                .clicked()
            {
                close = true;
            }
        });
    });
    if close || r.should_close() {
        tools.open = false;
        tools.editing = None;
        tools.calibration.reset();
    }
}

fn table_row(ui: &mut Ui, cx: &Cx<'_>, tools: &mut PriceTools, table: Option<&PriceTable>) {
    let source = match tools.feeds.value.as_ref().map(|f| &f.source) {
        Some(FeedOrigin::Mirror { name }) => name.clone(),
        Some(FeedOrigin::Cache) => cx.tr.t("prices.source.cache"),
        _ => cx.tr.t("prices.source.builtin"),
    };
    let rev = table.map(|t| t.rev.clone()).unwrap_or_default();
    let n = table.map_or(0, |t| t.entries.len());
    ui.horizontal(|ui| {
        ui.label(Text::strong(cx.tr.t("prices.table_label"), cx.pal));
        ui.label(
            Text::small(
                cx.tr.f(
                    "models.prices.table",
                    &[("source", &source), ("rev", &rev), ("n", &n)],
                ),
                cx.pal,
            )
            .color(cx.pal.text),
        )
        .on_hover_text(cx.tr.t("prices.table_tip"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if tools.show_table {
                "models.prices.hide_table"
            } else {
                "prices.view"
            };
            if ui
                .add(Button::new(cx.tr.t(label)).small().enabled(n > 0))
                .clicked()
            {
                tools.show_table = !tools.show_table;
            }
        });
    });
    if tools.show_table
        && let Some(t) = table
    {
        let rows = table_rows(t);
        let headers = [
            "prices.col.model",
            "prices.col.input",
            "prices.col.cached",
            "prices.col.output",
            "prices.col.source",
        ];
        let grid = Table::new(&[1.6, 0.8, 0.8, 0.8, 1.2])
            .min_widths(&[0.0, 0.0, 0.0, Table::RIGHT_INSET, 0.0])
            .right_aligned(&[1, 2, 3]);
        grid.header(ui, &headers.map(|h| cx.tr.t(h)));
        egui::ScrollArea::vertical()
            .id_salt("models-price-table")
            .max_height(220.0)
            .show(ui, |ui| {
                for (i, row) in rows.iter().enumerate() {
                    grid.row(ui, size::ROW, i + 1 == rows.len(), |ui, col| {
                        ui.label(if col == 0 {
                            Text::mono(&row[col], cx.pal)
                        } else {
                            Text::small(&row[col], cx.pal).color(cx.pal.text)
                        });
                    });
                }
            });
    }
}

fn ratio_rows(ui: &mut Ui, cx: &Cx<'_>, tools: &mut PriceTools) {
    let list = providers(cx);
    if list.is_empty() {
        ui.add(egui::Label::new(Text::small(cx.tr.t("prices.no_providers"), cx.pal)).wrap());
        return;
    }
    let stats = tools.settings(cx);
    let grid = Table::new(&[1.0, 0.0, 0.0])
        .min_widths(&[0.0, size::CONTROL * 3.0, 220.0])
        .right_aligned(&[1, 2]);
    grid.header(
        ui,
        &[
            cx.tr.t("prices.col.provider"),
            cx.tr.t("prices.col.ratio"),
            String::new(),
        ],
    );
    let calibrating = tools.calibration.loading();
    let n = list.len();
    for (i, (id, name, usage)) in list.into_iter().enumerate() {
        let ratio = ratio_of(&stats, &id);
        let mut text = tools
            .editing
            .as_ref()
            .filter(|(p, _)| p == &id)
            .map(|(_, t)| t.clone());
        let (mut save, mut cancel, mut edit, mut calibrate) = (false, false, false, false);
        ui.push_id(("models-ratio", &id), |ui| {
            grid.row(ui, size::ROW, i + 1 == n, |ui, col| match col {
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
                        save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                        cancel = ui
                            .add(Button::ghost(cx.tr.t("action.cancel")).small())
                            .clicked();
                        save |= ui
                            .add(
                                Button::new(cx.tr.t("prices.save"))
                                    .small()
                                    .enabled(parse_ratio(t).is_some()),
                            )
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
            ui.label(Text::caption(cx.tr.t("prices.invalid"), cx.pal).color(cx.pal.red));
        }
        if save && let Some(m) = valid {
            tools.editing = None;
            let next = with_ratio(&stats, &id, m);
            tools.save_settings(cx, next);
        } else if cancel {
            tools.editing = None;
        } else if edit {
            tools.editing = Some((id.clone(), ratio.to_string()));
        } else if let Some(t) = text {
            tools.editing = Some((id.clone(), t));
        }
        if calibrate {
            let service = cx.service.clone();
            tools.calibration.reset();
            tools
                .calibration
                .start(async move { load_calibration(service, id, name, ratio).await });
        }
    }
}

/// A finished billing comparison under the providers: its result and 使用这个比例.
fn calibration_view(ui: &mut Ui, cx: &Cx<'_>, tools: &mut PriceTools) {
    if let Some(e) = &tools.calibration.error {
        ui.add(
            egui::Label::new(
                Text::caption(crate::pages::error_text(cx.tr, e), cx.pal).color(cx.pal.red),
            )
            .wrap(),
        );
    }
    let Some(c) = tools.calibration.value.clone() else {
        return;
    };
    ui.add_space(space::S);
    ui.label(Text::strong(
        cx.tr.f("prices.calibrate_title", &[("provider", &c.name)]),
        cx.pal,
    ));
    let ratio = c.ratio();
    ui.add(
        egui::Label::new(Text::small(
            match ratio {
                Some(r) => cx.tr.f(
                    "prices.calibrate_result",
                    &[
                        ("billed", &c.billed.amount(2)),
                        ("est", &c.estimated.amount(2)),
                        ("ratio", &r),
                    ],
                ),
                None => cx.tr.t("prices.calibrate_no_data"),
            },
            cx.pal,
        ))
        .wrap(),
    );
    ui.add(egui::Label::new(Text::caption(cx.tr.t("prices.calibrate_note"), cx.pal)).wrap());
    ui.horizontal(|ui| {
        if ui
            .add(
                Button::primary(cx.tr.t("prices.calibrate_apply"))
                    .small()
                    .enabled(ratio.is_some()),
            )
            .clicked()
            && let Some(r) = ratio
        {
            let next = with_ratio(&tools.settings(cx), &c.profile, r);
            tools.save_settings(cx, next);
            tools.calibration.reset();
        }
        if ui
            .add(Button::ghost(cx.tr.t("action.cancel")).small())
            .clicked()
        {
            tools.calibration.reset();
        }
    });
}
