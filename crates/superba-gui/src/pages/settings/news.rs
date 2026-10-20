//! "New in latest" (新版本新增): the config keys the newest Codex releases add,
//! grouped by release (newest first, prereleases included), with the catalog's
//! freshness on top so "no new keys" reads differently from "catalog not
//! updated yet". A click selects the key in the normal detail pane.

use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use superba_core::Locale;

use super::State;
use super::model::{Entry, Model, NewKey, first_sentence};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::{radius, size, space, text};
use crate::design::widgets::{Badge, Status, Text};
use crate::pages::Cx;

/// Height of one key row (two lines).
const ROW_H: f32 = size::ROW_LIST2;

/// Draw the list; true when a row was clicked.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, model: &Model) -> bool {
    let zh = cx.tr.locale() == Locale::ZhCn;
    freshness(ui, cx, model);
    ui.add_space(space::S);
    let selected = state.selected().map(|e| e.path.clone());
    let mut pick: Option<String> = None;
    egui::ScrollArea::vertical()
        .id_salt("codex-settings-news")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::XS;
            for r in &model.releases {
                ui.horizontal(|ui| {
                    let title = if r.added.is_empty() {
                        cx.tr
                            .f("codex_settings.news.none", &[("version", &r.version)])
                    } else {
                        cx.tr.f(
                            "codex_settings.news.group",
                            &[("version", &r.version), ("n", &r.added.len().to_string())],
                        )
                    };
                    ui.label(
                        RichText::new(title)
                            .font(font(text::SECTION, Weight::Medium))
                            .color(if r.added.is_empty() {
                                cx.pal.text_muted
                            } else {
                                cx.pal.text
                            }),
                    );
                    if r.prerelease {
                        ui.add(Badge::new(cx.tr.t("codex_settings.news.prerelease")).outline());
                    }
                });
                for k in &r.added {
                    let on = selected.as_deref() == Some(k.path.as_str());
                    if row(ui, cx, k, model.entry(&k.path), on, zh) {
                        pick = Some(k.path.clone());
                    }
                }
                ui.add_space(space::M);
            }
        });
    let Some(path) = pick else {
        return false;
    };
    if let Some(i) = state.rows().iter().position(|e| e.path == path) {
        state.table.selected = Some(i);
    }
    true
}

/// "The catalog covers Codex X · generated D", and "The catalog does not
/// include Y yet" when a local Codex is newer.
fn freshness(ui: &mut Ui, cx: &Cx<'_>, model: &Model) {
    let f = &model.freshness;
    let covers = f
        .newest
        .as_deref()
        .map(|v| cx.tr.f("codex_settings.news.covers", &[("version", &v)]));
    let generated = match &f.generated {
        Some(d) => cx.tr.f("codex_settings.news.generated", &[("date", d)]),
        None => cx.tr.t("codex_settings.news.generated_unknown"),
    };
    let line = match covers {
        Some(c) => format!("{c} · {generated}"),
        None => generated,
    };
    ui.add(egui::Label::new(Text::small(line, cx.pal)).wrap());
    if let Some(v) = &f.ahead {
        ui.add(
            Badge::new(cx.tr.f("codex_settings.news.behind", &[("version", v)]))
                .status(Status::Warn),
        );
    }
    ui.add(
        egui::Label::new(Text::caption(
            cx.tr.t("codex_settings.news.caption"),
            cx.pal,
        ))
        .wrap(),
    );
}

/// One key: title, then its stage and whether `config.toml` sets it, then the
/// first sentence of its description. A key no local Codex knows (or a
/// templated path) has no entry: it is described from the release's catalog
/// and not clickable.
fn row(ui: &mut Ui, cx: &Cx<'_>, k: &NewKey, e: Option<&Entry>, on: bool, zh: bool) -> bool {
    let width = ui.available_width();
    let sense = if e.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut r) = ui.allocate_exact_size(vec2(width, ROW_H), sense);
    if e.is_some() {
        r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    if on || (e.is_some() && r.hovered()) {
        ui.painter().rect_filled(
            rect,
            radius::ROW,
            if on { cx.pal.active } else { cx.pal.hover },
        );
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(space::S, space::XS)))
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut child;
    ui.spacing_mut().item_spacing = vec2(6.0, 2.0);
    let (name, name_zh, text, stage) = match e {
        Some(e) => (&e.name, &e.name_zh, &e.text, e.stage),
        None => (&k.name, &k.name_zh, &k.text, k.stage),
    };
    let named = if zh { name_zh.as_ref() } else { name.as_ref() };
    let title = named.cloned().unwrap_or_else(|| k.path.clone());
    let is_path = named.is_none();
    let explained = if zh {
        text.zh.as_deref().or(text.en.as_deref())
    } else {
        text.en.as_deref().or(text.en_note.as_deref())
    };
    let desc = explained
        .map(|t| super::detail::strip_code_ticks(&first_sentence(t)))
        .or_else(|| (!is_path).then(|| k.path.clone()));
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), size::ROW_LIST2 / 2.0 - space::XS),
        Layout::right_to_left(Align::Center),
        |ui| {
            match e {
                Some(e) if e.file_value.is_some() => {
                    ui.add(Badge::new(cx.tr.t("codex_settings.news.set")).status(Status::Ok));
                }
                Some(_) => {
                    ui.add(Badge::new(cx.tr.t("codex_settings.news.unset")).outline());
                }
                None if k.path.contains('<') => {
                    ui.add(Badge::new(cx.tr.t("codex_settings.news.not_listed")).outline());
                }
                None => {
                    ui.add(Badge::new(cx.tr.t("codex_settings.news.not_local")).outline());
                }
            }
            ui.add(Badge::new(cx.tr.t(stage.label_key())).outline());
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let t = if is_path {
                    RichText::new(&title).font(mono_font(text::SMALL, Weight::Medium))
                } else {
                    RichText::new(&title).font(font(text::BODY, Weight::Medium))
                };
                ui.add(egui::Label::new(t.color(cx.pal.text)).truncate())
                    .on_hover_text(&k.path);
            });
        },
    );
    if let Some(d) = desc {
        ui.add(egui::Label::new(Text::caption(d, cx.pal)).truncate());
    }
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &title));
    e.is_some() && r.clicked()
}
