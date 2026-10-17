//! The Router page (UX §3.5, user decision 21): two columns. Left: the router
//! (mode, background service, process, address, members — `view.rs`) and the
//! prices of the cost estimates (`prices.rs`). Right: the reply stats line with
//! its live preview and the recording settings (numbers only: request content
//! is never recorded). Recording, stats line and price changes are saved at once
//! and only reload the router (never a Codex restart). The column contents
//! scroll inside the page when the window is short.

use egui::{ScrollArea, Ui};
use superba_core::{StatsLineClient, StatsLineStyle};

use super::stats_line::{self, Sample};
use super::{State, mode, prices, view};
use crate::design::layout::{self, Track};
use crate::design::widgets::{Button, FormRow, QuotePreview, Segmented, Status, Text, Toggle};
use crate::pages::{Action, Cx, clock_time, note, section_gap};

/// Edits in progress on the tab.
#[derive(Debug, Default)]
pub struct Prefs {
    /// Stats line template being typed (`None` = not editing; shows the saved one).
    pub template: Option<String>,
    /// Price section.
    pub prices: prices::Prices,
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    layout::columns(ui, &[Track::Fill, Track::Fill], 28.0, |ui, i| {
        scroll_column(ui, ("router-settings-col", i), |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if i == 0 {
                view::router_column(ui, cx, state, out);
                section_gap(ui, cx);
                prices::section(ui, cx, state);
            } else {
                stats_line_section(ui, cx, state);
                section_gap(ui, cx);
                recording_section(ui, cx, state);
            }
            ui.add_space(12.0);
        });
    });
}

/// Height of the fades at a scrolled column's edges.
pub const EDGE_FADE: f32 = 20.0;

/// A column that scrolls when the window is short (review ux-visual #11): its
/// scroll bar shows at rest when the content overflows, and the edge where
/// content is out of view fades into the panel, so no line is cut in half
/// under the banner or at the panel's bottom. Returns which edges fade (top,
/// bottom).
pub fn scroll_column(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui),
) -> (bool, bool) {
    // The bar gets a gutter of its own (over the content it would sit on the
    // controls at the column's right edge) and stays visible at rest; the fades
    // below replace egui's own (drawn in the window color, not the panel's).
    let scroll = &mut ui.spacing_mut().scroll;
    scroll.floating_allocated_width = scroll.bar_width + 6.0;
    scroll.dormant_handle_opacity = 0.45;
    scroll.fade.strength = 0.0;
    let out = ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, add);
    let rect = out.inner_rect;
    let offset = out.state.clamped_offset().y;
    let top = offset > 0.5;
    let bottom = offset + rect.height() < out.content_size.y - 0.5;
    let card = crate::design::backdrop::Backdrop::of(ui.ctx()).card;
    let painter = ui.painter_at(rect);
    let fade = |painter: &egui::Painter, edge: egui::Rect, solid_top: bool| {
        let clear = card.gamma_multiply(0.0);
        let (t, b) = if solid_top {
            (card, clear)
        } else {
            (clear, card)
        };
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(edge.left_top(), t);
        mesh.colored_vertex(edge.right_top(), t);
        mesh.colored_vertex(edge.left_bottom(), b);
        mesh.colored_vertex(edge.right_bottom(), b);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        painter.add(mesh);
    };
    // The scroll bar's gutter stays uncovered.
    let width = rect.width();
    if top {
        let edge = egui::Rect::from_min_size(rect.min, egui::vec2(width, EDGE_FADE));
        fade(&painter, edge, true);
    }
    if bottom {
        let edge = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.max.y - EDGE_FADE),
            egui::vec2(width, EDGE_FADE),
        );
        fade(&painter, edge, false);
    }
    (top, bottom)
}

fn heading_with_toggle(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    on: &mut bool,
    enabled: bool,
) -> bool {
    let mut changed = false;
    FormRow::new(Text::section(title, cx.pal))
        .label_width(0.0)
        .height(ui.spacing().interact_size.y)
        .right()
        .show(ui, |ui| {
            changed = ui.add(Toggle::new(on, title).enabled(enabled)).changed();
            ui.label(Text::caption(
                cx.tr.t(if *on {
                    "stats_line.on"
                } else {
                    "stats_line.off"
                }),
                cx.pal,
            ));
        });
    changed
}

fn stats_line_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let stats = state.stats_settings(cx);
    let saving = state.saving_stats();
    let mut on = stats.line.enabled;
    if heading_with_toggle(ui, cx, &cx.tr.t("stats_line.title"), &mut on, !saving) {
        state.save_stats(cx, stats_line::with_enabled(&stats, on));
    }
    ui.add(egui::Label::new(Text::small(cx.tr.t("stats_line.body"), cx.pal)).wrap());
    if !mode(cx).is_router() {
        note(ui, cx, Status::Warn, cx.tr.t("stats_line.needs_router"));
    }
    let line = stats.line.clone();
    let custom = stats_line::is_custom(&line);
    // A form: labels in one column, every row on one center line.
    let labels = [
        cx.tr.t("stats_line.clients"),
        cx.tr.t("stats_line.show"),
        cx.tr.t("stats_line.template"),
        cx.tr.t("stats_line.style_label"),
    ];
    let label_w = FormRow::label_width_of(ui, labels.iter().map(String::as_str));
    let row_h = ui.spacing().interact_size.y;
    let form = |label: &str| {
        FormRow::new(Text::small(label, cx.pal))
            .label_width(label_w)
            .gap(8.0)
            .height(row_h)
    };
    // Used for.
    form(&labels[0]).wrap().show(ui, |ui| {
        for (client, key) in [
            (StatsLineClient::Desktop, "stats_line.client.app"),
            (StatsLineClient::Tui, "stats_line.client.tui"),
        ] {
            let mut v = line.clients.contains(&client);
            if ui.checkbox(&mut v, cx.tr.t(key)).changed() {
                let mut next = stats.clone();
                next.line = stats_line::with_client(&line, client, v);
                state.save_stats(cx, next);
            }
        }
    });
    ui.label(Text::caption(cx.tr.t("stats_line.exec_never"), cx.pal));
    // Metrics.
    form(&labels[1]).wrap().show(ui, |ui| {
        ui.add_enabled_ui(!custom, |ui| {
            for (metric, key) in stats_line::METRICS {
                let mut v = line.metrics.contains(&metric);
                if ui.checkbox(&mut v, cx.tr.t(key)).changed() {
                    let mut next = stats.clone();
                    next.line = stats_line::with_metric(&line, metric, v);
                    state.prefs.template = None;
                    state.save_stats(cx, next);
                }
            }
        });
    });
    // Template.
    let saved = stats_line::editable(&stats_line::effective_template(cx.tr, &line));
    let mut text = state
        .prefs
        .template
        .clone()
        .unwrap_or_else(|| saved.clone());
    form(&labels[2]).show(ui, |ui| {
        // The router always starts the line with ⚡ (and its invisible marker):
        // a fixed prefix, the field edits the rest.
        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 20.0), egui::Sense::hover());
        crate::design::icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(r.center(), egui::vec2(13.0, 13.0)),
            crate::design::icons::Icon::Bolt,
            cx.pal.text_muted,
        );
        let r = ui.add(
            egui::TextEdit::singleline(&mut text)
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width()),
        );
        if r.changed() {
            state.prefs.template = Some(text.clone());
        }
        if r.lost_focus() && state.prefs.template.is_some() {
            let mut next = stats.clone();
            next.line = stats_line::with_template(cx.tr, &line, &stats_line::with_bolt(&text));
            state.prefs.template = None;
            state.save_stats(cx, next);
        }
    });
    let unknown = stats_line::unknown_placeholders(&text);
    if !unknown.is_empty() {
        ui.label(
            Text::caption(
                cx.tr
                    .f("stats_line.unknown", &[("names", &unknown.join(", "))]),
                cx.pal,
            )
            .color(cx.pal.red),
        );
    } else {
        ui.label(Text::caption(cx.tr.t("stats_line.placeholders"), cx.pal));
    }
    // 默认（跟随界面语言）, or the custom note with 恢复默认 on its center line.
    let edited = custom || state.prefs.template.is_some();
    FormRow::unlabeled().height(row_h).show(ui, |ui| {
        ui.label(Text::caption(
            stats_line::template_note(cx.tr, edited),
            cx.pal,
        ));
        if edited
            && ui
                .add(Button::ghost(cx.tr.t("stats_line.reset")).small())
                .clicked()
        {
            let mut next = stats.clone();
            next.line = stats_line::with_default_template(&line);
            state.prefs.template = None;
            state.save_stats(cx, next);
        }
    });
    // Style.
    form(&labels[3]).show(ui, |ui| {
        let mut style = line.style;
        if Segmented::new("router-stats-style", &mut style)
            .small()
            .option(StatsLineStyle::Quote, cx.tr.t("stats_line.style.quote"))
            .option(
                StatsLineStyle::QuoteItalic,
                cx.tr.t("stats_line.style.italic"),
            )
            .show(ui)
            .changed()
        {
            state.save_stats(cx, stats_line::with_style(&stats, style));
        }
    });
    // Preview (updates while typing).
    let sample = Sample::last_turn(&state.recent.shown);
    let caption = match sample.as_ref().and_then(|s| s.at) {
        Some(at) => cx
            .tr
            .f("stats_line.preview", &[("time", &clock_time(at, true))]),
        None => cx.tr.t("stats_line.preview_sample"),
    };
    let sample = sample.unwrap_or_else(Sample::example);
    let rendered = stats_line::render(&stats_line::with_bolt(&text), &sample);
    QuotePreview::new(cx.tr.t("stats_line.sample_reply"), rendered)
        .caption(caption)
        .italic(line.style == StatsLineStyle::QuoteItalic)
        .show(ui);
    ui.add(egui::Label::new(Text::caption(cx.tr.t("stats_line.note.stored"), cx.pal)).wrap());
    ui.add(egui::Label::new(Text::caption(cx.tr.t("stats_line.note.off"), cx.pal)).wrap());
}

fn recording_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let stats = state.stats_settings(cx);
    let saving = state.saving_stats();
    ui.label(Text::section(cx.tr.t("recording.title"), cx.pal));
    // Switches: text left, switch right; retention: labels in one column. Every
    // row on one center line.
    let row_h = ui.spacing().interact_size.y;
    let switch = |label: String| {
        FormRow::new(Text::small(label, cx.pal))
            .label_width(0.0)
            .height(row_h)
            .right()
    };
    let days_labels = [cx.tr.t("recording.usage_days")];
    let days_w = FormRow::label_width_of(ui, days_labels.iter().map(String::as_str));
    let retention = |label: &str| {
        FormRow::new(Text::small(label, cx.pal))
            .label_width(days_w)
            .gap(8.0)
    };
    switch(cx.tr.t("recording.usage")).show(ui, |ui| {
        let mut v = stats.record;
        if ui
            .add(Toggle::new(&mut v, cx.tr.t("recording.usage")).enabled(!saving))
            .changed()
        {
            let mut next = stats.clone();
            next.record = v;
            state.save_stats(cx, next);
        }
    });
    retention(&days_labels[0]).show(ui, |ui| {
        let mut days = stats.usage_days;
        let mut seg = Segmented::new("router-usage-days", &mut days).small();
        let mut options = vec![30u32, 90, 180];
        if !options.contains(&stats.usage_days) {
            options.push(stats.usage_days);
            options.sort_unstable();
        }
        for d in options {
            seg = seg.option(d, cx.tr.f("recording.days", &[("n", &d)]));
        }
        if seg.show(ui).changed() {
            let mut next = stats.clone();
            next.usage_days = days;
            state.save_stats(cx, next);
        }
    });
    if let Some(r) = state.snapshot().and_then(|s| s.recording.clone()) {
        ui.label(Text::caption(recorder_text(cx, &r), cx.pal));
    }
    ui.add(egui::Label::new(Text::caption(cx.tr.t("recording.note"), cx.pal)).wrap());
}

/// "Since the router started: n rows written, m dropped · database 12.3 MB".
fn recorder_text(cx: &Cx<'_>, r: &superba_core::RouterStatsStatus) -> String {
    let mut text = cx.tr.f(
        "recording.counters",
        &[
            (
                "written",
                &crate::pages::shared::format::group_thousands(r.rows_written),
            ),
            (
                "dropped",
                &crate::pages::shared::format::group_thousands(r.dropped),
            ),
        ],
    );
    if let Some(bytes) = r.db_bytes {
        text.push_str(" · ");
        text.push_str(&cx.tr.f(
            "recording.db_size",
            &[("size", &crate::pages::shared::format::megabytes(bytes))],
        ));
    }
    text
}
