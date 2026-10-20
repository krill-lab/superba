//! The Router page's body in reading order, one scrolling column at every
//! width: the status line, members and their line order, Claude Code's
//! members, the entry points, then the rarely changed settings in the closed
//! 「高级」 / "Advanced" group (reply stats line, prices, recording, service).
//! Sections are flat (ui-spec rule 2): a section title, an optional caption,
//! then rows separated by hairlines. Changes save immediately without a restart.

use egui::{ScrollArea, Ui};
use superba_core::{StatsLineClient, StatsLineStyle};

use super::stats_line::{self, Sample};
use super::{State, mode, prices, view};
use crate::design::tokens::{size, space};
use crate::design::widgets::{
    Button, Disclosure, FormRow, QuotePreview, Segmented, Status, Text, Toggle,
};
use crate::pages::{Action, Cx, clock_time, divider, section};

/// Edits in progress on the tab.
#[derive(Debug, Default)]
pub struct Prefs {
    /// Stats line template being typed (`None` = not editing; shows the saved one).
    pub template: Option<String>,
    /// Price section.
    pub prices: prices::Prices,
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    scroll_column(ui, "router-settings", |ui| {
        ui.spacing_mut().item_spacing.y = space::XS;
        super::status_line(ui, cx, state);
        view::notices(ui, cx, state);
        ui.add_space(space::XL);
        view::members_section(ui, cx, state, out);
        ui.add_space(space::XL);
        super::ingress::show(ui, cx, state);
        ui.add_space(space::XL);
        divider(ui, cx);
        ui.add_space(space::S);
        Disclosure::new("router-advanced", cx.tr.t("router.advanced.title"))
            .section()
            .caption(cx.tr.t("router.advanced.caption"))
            .show(ui, |ui| {
                ui.add_space(space::M);
                stats_line_section(ui, cx, state);
                ui.add_space(space::XL);
                prices::moved_row(ui, cx, out);
                ui.add_space(space::XL);
                recording_section(ui, cx, state);
                ui.add_space(space::XL);
                view::service_section(ui, cx, state);
            });
        ui.add_space(space::XL);
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

/// A setting whose control needs the full width (a group of check boxes, the
/// template field): the title and its help, the control under them, then a
/// hairline unless it is the `last` row. The stacked form of
/// [`crate::pages::superba::row`], whose rows it sits between.
fn wide_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    desc: &str,
    last: bool,
    add: impl FnOnce(&mut Ui),
) {
    ui.add_space(space::M);
    let label = ui.label(Text::strong(title, cx.pal));
    if !desc.is_empty() {
        ui.add(egui::Label::new(Text::small(desc, cx.pal)).wrap());
    }
    ui.add_space(space::XS);
    crate::design::a11y::label_unnamed_by(ui, label.id, add);
    ui.add_space(space::M);
    if !last {
        divider(ui, cx);
    }
}

fn stats_line_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let stats = state.stats_settings(cx);
    let saving = state.saving_stats();
    let title = cx.tr.t("stats_line.title");
    let desc = cx.tr.t("stats_line.body");
    section(ui, cx, &title, None);
    if !mode(cx).is_router() {
        // Why switching it on changes nothing yet, before the switch.
        crate::pages::note(ui, cx, Status::Warn, cx.tr.t("stats_line.needs_router"));
        ui.add_space(space::S);
    }
    crate::pages::superba::row(ui, cx, &cx.tr.t("stats_line.enabled"), &desc, false, |ui| {
        let mut on = stats.line.enabled;
        // Named by the section title: the e2e drivers (W31) and the a11y tests
        // switch it by this name.
        if ui
            .add(Toggle::new(&mut on, &title).enabled(!saving))
            .changed()
        {
            state.save_stats(cx, stats_line::with_enabled(&stats, on));
        }
    });
    let line = stats.line.clone();
    let custom = stats_line::is_custom(&line);
    wide_row(
        ui,
        cx,
        &cx.tr.t("stats_line.tools"),
        &cx.tr.t("stats_line.tools_hint"),
        false,
        |ui| {
            ui.horizontal_wrapped(|ui| {
                for harness in superba_core::RequestHarness::ALL {
                    let mut enabled = stats.line.harnesses.contains(&harness);
                    let label = cx.tr.text(&harness.text());
                    if ui
                        .add_enabled(!saving, egui::Checkbox::new(&mut enabled, label))
                        .changed()
                    {
                        let mut next = stats.clone();
                        next.line = stats_line::with_harness(&stats.line, harness, enabled);
                        state.save_stats(cx, next);
                    }
                }
            });
        },
    );
    wide_row(
        ui,
        cx,
        &cx.tr.t("stats_line.clients"),
        &cx.tr.t("stats_line.exec_never"),
        false,
        |ui| {
            ui.horizontal_wrapped(|ui| {
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
        },
    );
    wide_row(
        ui,
        cx,
        &cx.tr.t("stats_line.show"),
        &cx.tr.t("stats_line.show_hint"),
        false,
        |ui| {
            ui.add_enabled_ui(!custom, |ui| {
                ui.horizontal_wrapped(|ui| {
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
        },
    );
    template_row(ui, cx, state, &stats, &line, custom);
    crate::pages::superba::row(
        ui,
        cx,
        &cx.tr.t("stats_line.style_label"),
        &cx.tr.t("stats_line.style_hint"),
        true,
        |ui| {
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
        },
    );
    // Preview (updates while typing).
    ui.add_space(space::S);
    let text = state
        .prefs
        .template
        .clone()
        .unwrap_or_else(|| stats_line::editable(&stats_line::effective_template(cx.tr, &line)));
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
    ui.add_space(space::S);
    ui.add(egui::Label::new(Text::caption(cx.tr.t("stats_line.note.stored"), cx.pal)).wrap());
    ui.add(egui::Label::new(Text::caption(cx.tr.t("stats_line.note.off"), cx.pal)).wrap());
}

/// The template: a fixed ⚡ and the editable rest on one line, what it may
/// contain (or which placeholders are unknown) under it, and 恢复默认 while it
/// differs from the default.
fn template_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    stats: &superba_core::RouterStatsSettings,
    line: &superba_core::StatsLineSettings,
    custom: bool,
) {
    let saved = stats_line::editable(&stats_line::effective_template(cx.tr, line));
    let mut text = state
        .prefs
        .template
        .clone()
        .unwrap_or_else(|| saved.clone());
    let unknown = stats_line::unknown_placeholders(&text);
    let edited = custom || state.prefs.template.is_some();
    wide_row(
        ui,
        cx,
        &cx.tr.t("stats_line.template"),
        &stats_line::template_note(cx.tr, edited),
        false,
        |ui| {
            FormRow::unlabeled().height(size::CONTROL).show(ui, |ui| {
                // The router always starts the line with ⚡ (and its invisible
                // marker): a fixed prefix, the field edits the rest.
                let (r, _) = ui
                    .allocate_exact_size(egui::vec2(size::ICON, size::ICON), egui::Sense::hover());
                crate::design::icons::paint(
                    ui.painter(),
                    r,
                    crate::design::icons::Icon::Bolt,
                    cx.pal.text_muted,
                );
                let reset_w = if edited {
                    Button::ghost(cx.tr.t("stats_line.reset"))
                        .small()
                        .desired_size(ui)
                        .x
                        + space::S
                } else {
                    0.0
                };
                // A format string with {placeholders}: code, so monospace.
                let r = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width((ui.available_width() - reset_w).max(0.0)),
                );
                if r.changed() {
                    state.prefs.template = Some(text.clone());
                }
                if r.lost_focus() && state.prefs.template.is_some() {
                    let mut next = stats.clone();
                    next.line =
                        stats_line::with_template(cx.tr, line, &stats_line::with_bolt(&text));
                    state.prefs.template = None;
                    state.save_stats(cx, next);
                }
                if edited
                    && ui
                        .add(Button::ghost(cx.tr.t("stats_line.reset")).small())
                        .clicked()
                {
                    let mut next = stats.clone();
                    next.line = stats_line::with_default_template(line);
                    state.prefs.template = None;
                    state.save_stats(cx, next);
                }
            });
            if unknown.is_empty() {
                ui.label(Text::caption(cx.tr.t("stats_line.placeholders"), cx.pal));
            } else {
                ui.label(
                    Text::caption(
                        cx.tr
                            .f("stats_line.unknown", &[("names", &unknown.join(", "))]),
                        cx.pal,
                    )
                    .color(cx.pal.red),
                );
            }
        },
    );
}

fn recording_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let stats = state.stats_settings(cx);
    let saving = state.saving_stats();
    section(ui, cx, &cx.tr.t("recording.title"), None);
    let label = cx.tr.t("recording.usage");
    crate::pages::superba::row(ui, cx, &label, &cx.tr.t("recording.note"), true, |ui| {
        let mut v = stats.record;
        if ui
            .add(Toggle::new(&mut v, &label).enabled(!saving))
            .changed()
        {
            let mut next = stats.clone();
            next.record = v;
            state.save_stats(cx, next);
        }
    });
    let mut facts = vec![cx.tr.t("prefs.data.retention_here")];
    if let Some(r) = state.snapshot().and_then(|s| s.recording.clone()) {
        facts.push(recorder_text(cx, &r));
    }
    ui.add(egui::Label::new(Text::caption(facts.join(" · "), cx.pal)).wrap());
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
