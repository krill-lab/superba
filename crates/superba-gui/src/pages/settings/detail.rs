//! The detail pane of the selected setting (UX §3.6): Chinese name with the
//! English name and path, badges (stage, first version, availability per local
//! Codex, the `codex features list` cross-check), the Chinese explanation and the
//! official English text (clamped; "Show all" opens a dialog), the editor or the
//! link to the page that owns the key, default / current / recommended values,
//! what each connected remote host would skip when syncing, and reset.

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Layout, RichText, TextEdit, Ui, vec2};
use superba_core::Locale;

use super::State;
use super::model::{Availability, Change, Entry, Kind, Model, ReservedOwner, Stage, Value};
use super::view::{avail_icon, avail_mark, value_text};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::text;
use crate::design::widgets::{
    Badge, Dropdown, Status, StatusDot, Text, Toggle, dialog, link_button,
};
use crate::pages::shared::values;
use crate::pages::{Action, Cx, Page};

/// Lines of the Chinese explanation shown before "Show all".
const ZH_LINES: usize = 9;
/// Lines of the official English text shown before "Show all".
const EN_LINES: usize = 5;
/// The same above an editor across the pane (long text, paths, lists, maps).
const ZH_LINES_WIDE: usize = 5;
/// The same for the English text.
const EN_LINES_WIDE: usize = 3;

/// Draw the pane for the selected row.
pub(super) fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    model: &Model,
    out: &mut Vec<Action>,
) {
    let Some(e) = state.selected().cloned() else {
        ui.add_space(24.0);
        ui.label(Text::small(cx.tr.t("codex_settings.detail.none"), cx.pal));
        return;
    };
    let zh = cx.tr.locale() == Locale::ZhCn;
    let width = ui.available_width();
    ui.spacing_mut().item_spacing.y = 6.0;
    // 1. Names and path.
    let title = e.title_for(zh);
    if title.is_path {
        ui.add(
            egui::Label::new(
                RichText::new(title.text)
                    .font(mono_font(text::TITLE, Weight::Semibold))
                    .color(cx.pal.text),
            )
            .wrap(),
        );
    } else {
        ui.add(
            egui::Label::new(
                RichText::new(title.text)
                    .font(font(text::TITLE, Weight::Semibold))
                    .color(cx.pal.text),
            )
            .wrap(),
        );
    }
    let mut second = Vec::new();
    if zh && let Some(en) = &e.name {
        second.push(en.clone());
    }
    if !title.is_path {
        second.push(e.path.clone());
    }
    if !second.is_empty() {
        ui.add(egui::Label::new(Text::mono_muted(second.join(" · "), cx.pal)).wrap());
    }
    // 2. Badges.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        if title.zh_missing {
            ui.add(Badge::new(cx.tr.t("codex_settings.zh_missing")).outline());
        }
        let status = match e.stage {
            Stage::Stable => Status::Ok,
            Stage::Experimental => Status::Idle,
            Stage::UnderDevelopment | Stage::Deprecated => Status::Warn,
            Stage::Removed => Status::Error,
        };
        ui.add(
            Badge::new(cx.tr.t(e.stage.label_key()))
                .status(status)
                .outline(),
        );
        if let Some(since) = &e.since {
            ui.add(
                Badge::new(
                    cx.tr
                        .f("codex_settings.detail.since", &[("version", since)]),
                )
                .mono()
                .outline(),
            );
        }
        for t in &model.targets {
            let a = e.availability_for(&t.key);
            let mut label = t.label.clone();
            if a == Availability::No
                && let Some(since) = &e.since
            {
                label.push(' ');
                label.push_str(
                    &cx.tr
                        .f("codex_settings.needs_version", &[("version", since)]),
                );
            }
            let status = match a {
                Availability::Yes => Status::Ok,
                Availability::No => Status::Error,
                Availability::Unknown => Status::Idle,
            };
            let version = t.version.clone().unwrap_or_default();
            let key = match a {
                Availability::Yes => "codex_settings.avail.yes",
                Availability::No => "codex_settings.avail.no",
                Availability::Unknown => "codex_settings.avail.unknown",
            };
            let badge = match avail_icon(a) {
                Some(icon) => Badge::new(label).icon(icon),
                None => Badge::new(format!("? {label}")),
            };
            ui.add(badge.status(status))
                .on_hover_text(cx.tr.f(key, &[("target", &t.label), ("version", &version)]));
        }
        if e.legacy.is_some() {
            ui.add(Badge::new(cx.tr.t("codex_settings.badge.legacy")).outline());
        }
        if let Some(os) = &e.platform {
            ui.add(Badge::new(cx.tr.f("codex_settings.only_on", &[("os", os)])).outline())
                .on_hover_text(cx.tr.t("codex_settings.platform_hint"));
        }
    });
    if e.runtime_mismatch() {
        let effective = e.effective.unwrap_or_default();
        let predicted = !effective;
        crate::pages::note(
            ui,
            cx,
            Status::Warn,
            cx.tr.f(
                "codex_settings.detail.runtime",
                &[
                    ("effective", &bool_text(cx, effective)),
                    ("predicted", &bool_text(cx, predicted)),
                ],
            ),
        );
    }
    // 3. Explanation (Chinese first in Chinese) and 4. the official English text,
    // shorter above an editor that takes the pane's width.
    let wide = super::editor::wide(&e, &state.pending);
    let (zh_lines, en_lines) = if wide {
        (ZH_LINES_WIDE, EN_LINES_WIDE)
    } else {
        (ZH_LINES, EN_LINES)
    };
    let mut truncated = false;
    if zh {
        match &e.text.zh {
            Some(text) => {
                truncated |= clamped(ui, text, zh_lines, cx.pal.text, width);
                if e.text.zh_stale {
                    ui.add(Badge::new(cx.tr.t("codex_settings.zh_stale")).status(Status::Warn));
                }
            }
            None => {
                ui.label(Text::small(cx.tr.t("codex_settings.no_zh"), cx.pal).italics());
            }
        }
        ui.add_space(2.0);
        ui.label(Text::caption(
            cx.tr.t("codex_settings.detail.official"),
            cx.pal,
        ));
    }
    match (&e.text.en, &e.text.en_note) {
        (Some(en), _) => {
            let color = if zh { cx.pal.text_muted } else { cx.pal.text };
            truncated |= clamped(ui, en, if zh { en_lines } else { zh_lines }, color, width);
        }
        (None, Some(note)) => {
            truncated |= clamped(ui, note, en_lines, cx.pal.text_muted, width);
            ui.add(Badge::new(cx.tr.t("codex_settings.contributor_note")).outline())
                .on_hover_text(cx.tr.t("codex_settings.contributor_note_hint"));
        }
        (None, None) => {
            ui.label(
                Text::small(cx.tr.t("codex_settings.detail.no_official"), cx.pal)
                    .color(cx.pal.text_subtle)
                    .italics(),
            );
        }
    }
    if truncated && link_button(ui, &cx.tr.t("codex_settings.detail.expand"), false).clicked() {
        state.expanded = Some(e.path.clone());
    }
    ui.add_space(4.0);
    crate::pages::divider(ui, cx);
    ui.add_space(4.0);
    // 5. Editor (or where the key is managed).
    let read_only = model.config_error.is_some() || model.profile_id.is_none() || cx.busy;
    if wide {
        super::editor::wide_value(ui, cx, state, model, &e, read_only, out);
    } else {
        kv(ui, cx, &cx.tr.t("codex_settings.detail.value"), |ui| {
            editor(ui, cx, state, model, &e, read_only, out);
        });
    }
    if let Some(key) = state.invalid.get(&e.path) {
        ui.label(Text::small(cx.tr.t(key), cx.pal).color(cx.pal.red));
    }
    // 6. Default, current, recommendation: one line each, the whole value in the
    // tooltip.
    let chip_w = (width - 104.0).max(80.0);
    kv(
        ui,
        cx,
        &cx.tr.t("codex_settings.detail.default"),
        |ui| match &e.default {
            Some(d) => {
                values::chip(ui, cx, &super::editor::shown(cx, d), chip_w, true);
            }
            None => {
                ui.label(Text::small(
                    cx.tr.t("codex_settings.value.codex_decides"),
                    cx.pal,
                ));
            }
        },
    );
    kv(
        ui,
        cx,
        &cx.tr.t("codex_settings.detail.current"),
        |ui| match &e.file_value {
            Some(v) => {
                let source = cx.tr.t(if e.managed {
                    "codex_settings.detail.source_managed"
                } else {
                    "codex_settings.detail.source_file"
                });
                let source_w = ui
                    .painter()
                    .layout_no_wrap(
                        source.clone(),
                        font(text::CAPTION, Weight::Medium),
                        cx.pal.text_subtle,
                    )
                    .size()
                    .x;
                let shown = super::editor::shown(cx, v);
                values::chip(ui, cx, &shown, chip_w - source_w - 8.0, false);
                ui.label(Text::caption(source, cx.pal));
            }
            None => {
                ui.label(Text::small(cx.tr.t("codex_settings.value.unset"), cx.pal));
            }
        },
    );
    if let Some(v) = &e.recommended {
        let differs = e.value_with(&state.pending) != Some(v);
        let mut use_it = false;
        kv(
            ui,
            cx,
            &cx.tr.t("codex_settings.detail.recommended"),
            |ui| {
                values::chip(ui, cx, &super::editor::shown(cx, v), chip_w - 90.0, true);
                if differs
                    && !read_only
                    && e.editable()
                    && link_button(ui, &cx.tr.t("codex_settings.use_recommended"), false).clicked()
                {
                    use_it = true;
                }
            },
        );
        if use_it {
            state
                .pending
                .set_path(model, &e.path, Change::Set(v.clone()));
            state.buffers.remove(&e.path);
        }
    }
    // 7. Remote hosts (when syncing).
    if !model.hosts.is_empty() {
        kv(ui, cx, &cx.tr.t("codex_settings.detail.remote"), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (i, h) in model.hosts.iter().enumerate() {
                    if i > 0 {
                        ui.label(Text::small("·", cx.pal));
                    }
                    let a = e
                        .host_support
                        .iter()
                        .find(|(k, _)| *k == h.key)
                        .map(|(_, a)| *a)
                        .unwrap_or(Availability::Unknown);
                    avail_mark(ui, cx, a);
                    let mut s = h.label.clone();
                    if a == Availability::No
                        && let Some(since) = &e.since
                    {
                        s.push(' ');
                        s.push_str(
                            &cx.tr
                                .f("codex_settings.needs_version", &[("version", since)]),
                        );
                    }
                    ui.label(Text::small(s, cx.pal));
                }
            })
            .response
            .on_hover_text(cx.tr.t("codex_settings.detail.remote_tip"));
        });
    }
    // Legacy key: what replaced it, with a one-click migration.
    if let Some(legacy) = &e.legacy {
        let t = match (&legacy.canonical, legacy.ignored) {
            (Some(c), _) => cx.tr.f("codex_settings.legacy.replaced", &[("path", c)]),
            (None, true) => cx.tr.t("codex_settings.legacy.ignored"),
            (None, false) => cx.tr.t("codex_settings.legacy.deprecated"),
        };
        ui.horizontal_wrapped(|ui| {
            ui.add(StatusDot::new(Status::Warn));
            ui.label(Text::small(t, cx.pal));
            if e.current.is_some()
                && legacy.canonical.is_some()
                && state.pending.get(&e.path).is_none()
                && !read_only
                && link_button(ui, &cx.tr.t("codex_settings.migrate.one"), false)
                    .on_hover_text(cx.tr.t("codex_settings.migrate.restore_note"))
                    .clicked()
            {
                for (path, change) in model.migration_changes(&e) {
                    state.pending.set_path(model, &path, change);
                }
            }
        });
    }
    // 8. Reset.
    if e.value_with(&state.pending).is_some() && e.editable() && !read_only {
        ui.add_space(4.0);
        if ui
            .add(crate::design::widgets::Button::new(cx.tr.t("codex_settings.reset")).small())
            .on_hover_text(cx.tr.t("codex_settings.reset_hint"))
            .clicked()
        {
            state.pending.set_path(model, &e.path, Change::Unset);
            state.buffers.remove(&e.path);
            state.invalid.remove(&e.path);
        }
    }
}

fn bool_text(cx: &Cx<'_>, b: bool) -> String {
    cx.tr.t(if b {
        "codex_settings.value.on"
    } else {
        "codex_settings.value.off"
    })
}

/// A label / value row: the label and the value's widgets share one center line
/// ([`FormRow`](crate::design::widgets::FormRow)); a value taller than a line (a
/// multi-line editor or preview) keeps the label on its first line.
fn kv(ui: &mut Ui, cx: &Cx<'_>, label: &str, value: impl FnOnce(&mut Ui)) {
    crate::design::widgets::FormRow::new(Text::small(label, cx.pal))
        .label_width(96.0)
        .gap(8.0)
        .height(26.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            value(ui);
        });
}

/// Draw `text` wrapped to at most `lines` lines; true when it was cut.
fn clamped(ui: &mut Ui, text: &str, lines: usize, color: egui::Color32, width: f32) -> bool {
    let pal = crate::design::Palette::of(ui.ctx());
    let mut job = code_spans(text, font(text::SMALL, Weight::Regular), color, pal);
    job.wrap.max_width = width;
    let full = ui.painter().layout_job(job.clone());
    let cut = full.rows.len() > lines;
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: lines,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    ui.label(job);
    cut
}

/// An explanation as a layout job: Markdown `code` spans (key names, values)
/// in the mono font on a soft chip, without their backticks (review ux-visual
/// #14); an unpaired backtick stays as it is.
pub fn code_spans(
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
    pal: &crate::design::Palette,
) -> LayoutJob {
    let plain = egui::TextFormat {
        font_id: font_id.clone(),
        color,
        line_height: Some(19.0),
        ..Default::default()
    };
    let code = egui::TextFormat {
        font_id: mono_font(font_id.size - 0.5, Weight::Regular),
        color: pal.text,
        background: pal.hover,
        line_height: Some(19.0),
        valign: egui::Align::Center,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    for (part, is_code) in split_code(text) {
        let format = if is_code { code.clone() } else { plain.clone() };
        job.append(part, 0.0, format);
    }
    job
}

/// `text` split into plain parts and the insides of its `code` spans.
fn split_code(text: &str) -> Vec<(&str, bool)> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`').filter(|&e| e > 0) else {
            break;
        };
        parts.push((&rest[..start], false));
        parts.push((&after[..end], true));
        rest = &after[end + 1..];
    }
    parts.push((rest, false));
    parts
}

/// `text` without the backticks of its `code` spans (one-line summaries).
pub fn strip_code_ticks(text: &str) -> String {
    split_code(text).into_iter().map(|(p, _)| p).collect()
}

/// The editor of the selected setting; edits go straight into the pending set.
fn editor(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    model: &Model,
    e: &Entry,
    read_only: bool,
    out: &mut Vec<Action>,
) {
    let current = e.value_with(&state.pending).cloned();
    if let Some(owner) = e.owner {
        // One line (a long base URL keeps its host), the whole value in the tooltip.
        match &current {
            Some(v) => {
                let w = (ui.available_width() - 160.0).max(60.0);
                values::chip(ui, cx, &super::editor::shown(cx, v), w, true);
                if ui
                    .add(
                        crate::design::widgets::IconButton::new(
                            crate::design::icons::Icon::Copy,
                            cx.tr.t("codex_settings.detail.copy"),
                        )
                        .size(24.0),
                    )
                    .clicked()
                {
                    out.push(Action::Copy(super::editor::copy_text(v)));
                }
            }
            None => {
                ui.label(Text::small(cx.tr.t("codex_settings.value.unset"), cx.pal));
            }
        }
        let profile = model.profile_id.clone();
        let (key, page) = match owner {
            ReservedOwner::Connection => (
                "codex_settings.detail.managed_by_provider",
                Some(Page::Providers),
            ),
            ReservedOwner::ModelDefaults if e.path == "service_tier" => {
                ("codex_settings.detail.managed_by_speed", Some(Page::Models))
            }
            ReservedOwner::ModelDefaults => (
                "codex_settings.detail.managed_by_models",
                Some(Page::Models),
            ),
            _ => ("codex_settings.detail.managed_by_superba", None),
        };
        match page {
            Some(page) => {
                if link_button(ui, &cx.tr.t(key), false).clicked() {
                    out.push(Action::PageCommand {
                        page,
                        command: profile
                            .map(|p| format!("profile:{p}"))
                            .unwrap_or_else(|| "refresh".into()),
                    });
                }
            }
            None => {
                ui.label(Text::caption(cx.tr.t(key), cx.pal));
            }
        }
        return;
    }
    let enabled = !read_only && e.editable();
    match &e.kind {
        Kind::Bool => {
            let known = e.shown_value(&state.pending).and_then(Value::as_bool);
            let mut on = known.unwrap_or(false);
            let r = ui.add(Toggle::new(&mut on, e.title_for(false).text).enabled(enabled));
            let label = match known {
                Some(b) => bool_text(cx, b),
                None => cx.tr.t("codex_settings.value.unset"),
            };
            ui.label(Text::small(label, cx.pal));
            if r.changed() {
                state
                    .pending
                    .set_path(model, &e.path, Change::Set(Value::Bool(on)));
            }
        }
        Kind::Enum { options, .. } => {
            let shown = match e.shown_value(&state.pending) {
                Some(v) => RichText::new(value_text(cx, v)).font(mono_font(12.0, Weight::Regular)),
                None => RichText::new(cx.tr.t("codex_settings.value.codex_decides")),
            };
            let mut picked: Option<String> = None;
            ui.add_enabled_ui(enabled, |ui| {
                Dropdown::new(("codex-settings-enum", &e.path))
                    .selected_text(shown)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for o in options {
                            let selected =
                                current.as_ref().and_then(Value::as_str) == Some(o.value.as_str());
                            let mut r = ui.selectable_label(selected, &o.value);
                            let help = match (&o.zh, &o.en, cx.tr.locale()) {
                                (Some(zh), _, Locale::ZhCn) => Some(zh.clone()),
                                (_, Some(en), _) => Some(en.clone()),
                                _ => None,
                            };
                            if let Some(help) = help {
                                r = r.on_hover_text(help);
                            }
                            if r.clicked() {
                                picked = Some(o.value.clone());
                            }
                        }
                    });
            });
            if let Some(v) = picked {
                state
                    .pending
                    .set_path(model, &e.path, Change::Set(Value::Str(v)));
            }
        }
        Kind::Integer { .. } | Kind::Number { .. } | Kind::Text { .. } => {
            let initial = current
                .as_ref()
                .map(|v| match v {
                    Value::Str(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let buffer = state
                .buffers
                .entry(e.path.clone())
                .or_insert_with(|| initial.clone());
            let hint = e
                .default
                .as_ref()
                .map(|d| value_text(cx, d))
                .unwrap_or_else(|| cx.tr.t("codex_settings.value.codex_decides"));
            let monospace = !matches!(e.kind, Kind::Text { path: false });
            let prose = matches!(e.kind, Kind::Text { path: false });
            let mut edit = TextEdit::singleline(buffer)
                .id(egui::Id::unique(("codex-settings-text", &e.path)))
                .hint_text(hint)
                .desired_width(if prose { 150.0 } else { 200.0 })
                .margin(vec2(8.0, 5.0));
            if monospace {
                edit = edit.font(egui::TextStyle::Monospace);
            }
            let r = ui.add_enabled(enabled, edit);
            // Text that needs more than one line: the large editor.
            if prose
                && ui
                    .add(
                        crate::design::widgets::Button::ghost(
                            cx.tr.t("codex_settings.detail.edit"),
                        )
                        .small()
                        .enabled(enabled),
                    )
                    .clicked()
            {
                state.open_editor(&e.path);
            }
            if let Some(v) = &current
                && ui
                    .add(
                        crate::design::widgets::IconButton::new(
                            crate::design::icons::Icon::Copy,
                            cx.tr.t("codex_settings.detail.copy"),
                        )
                        .size(24.0),
                    )
                    .clicked()
            {
                out.push(Action::Copy(super::editor::copy_text(v)));
            }
            let Some(buffer) = state.buffers.get_mut(&e.path) else {
                return;
            };
            let enter = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (r.lost_focus() || enter) && *buffer != initial {
                let text = buffer.trim().to_owned();
                if text.is_empty() {
                    state.invalid.remove(&e.path);
                    state.pending.set_path(model, &e.path, Change::Unset);
                } else {
                    match e.kind.parse(&text) {
                        Ok(v) => {
                            state.invalid.remove(&e.path);
                            state.pending.set_path(model, &e.path, Change::Set(v));
                        }
                        Err(err) => {
                            state.invalid.insert(e.path.clone(), err.key());
                        }
                    }
                }
            }
        }
        // Composite values are drawn across the pane (`editor::wide_value`); here
        // only when nothing is set.
        Kind::ReadOnly | Kind::List { .. } | Kind::Map { .. } => {
            match &current {
                Some(v) => {
                    let w = (ui.available_width() - 130.0).max(60.0);
                    values::chip(ui, cx, &super::editor::shown(cx, v), w, true);
                }
                None => {
                    ui.label(Text::small(cx.tr.t("codex_settings.value.unset"), cx.pal));
                }
            }
            ui.label(Text::caption(
                cx.tr.t("codex_settings.detail.edit_in_file"),
                cx.pal,
            ));
        }
    }
}

/// The full explanation of a setting whose text was cut in the pane.
pub(super) fn expanded_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, model: &Model) {
    let Some(path) = state.expanded.clone() else {
        return;
    };
    let Some(e) = model.entry(&path) else {
        state.expanded = None;
        return;
    };
    let zh = cx.tr.locale() == Locale::ZhCn;
    let mut close = false;
    let r = dialog(
        ctx,
        egui::Id::unique("codex-settings-expanded"),
        560.0,
        |ui| {
            ui.label(Text::h2(e.title_for(zh).text, cx.pal));
            ui.label(Text::mono_muted(&e.path, cx.pal));
            egui::ScrollArea::vertical()
                .id_salt("codex-settings-expanded-text")
                .max_height(420.0)
                .show(ui, |ui| {
                    let width = ui.available_width();
                    let job = |t: &str, color| {
                        let mut job =
                            code_spans(t, font(text::SMALL, Weight::Regular), color, cx.pal);
                        job.wrap.max_width = width;
                        job
                    };
                    if zh && let Some(t) = &e.text.zh {
                        ui.add(egui::Label::new(job(t, cx.pal.text)).wrap());
                        ui.add_space(8.0);
                        ui.label(Text::caption(
                            cx.tr.t("codex_settings.detail.official"),
                            cx.pal,
                        ));
                    }
                    if let Some(t) = e.text.en.as_ref().or(e.text.en_note.as_ref()) {
                        ui.add(egui::Label::new(job(t, cx.pal.text_muted)).wrap());
                    }
                });
            ui.add_space(8.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                if ui
                    .add(
                        crate::design::widgets::Button::new(
                            cx.tr.t("codex_settings.pending.close"),
                        )
                        .kbd("Esc"),
                    )
                    .clicked()
                {
                    close = true;
                }
            });
        },
    );
    if close || r.should_close() {
        state.expanded = None;
    }
}
