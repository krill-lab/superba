//! Codex setting values for display (user bug 15), shared by the Settings page
//! (list chips, detail pane, diff dialog), the switch preview and confirmation
//! (`shell::key_changes`) and Remote hosts → Sync settings: a value becomes a
//! [`Shown`] (its text, first line or item, [`Shape`] and how much is hidden);
//! [`change`] draws `before → after` in a fixed width, and [`full_diff`] the whole
//! change (line diff for text, item diff for lists and tables) in a bounded,
//! scrolling box that never widens its dialog.

use egui::{Id, RichText, Ui, vec2};
use superba_core::settings::SettingValue;

use crate::design::elide::{self, DiffOp, Shape};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{radius, text};
use crate::design::widgets::IconButton;
use crate::design::widgets::value_chip::{ValueChip, fit, value_font};
use crate::pages::Cx;

/// What a one-line display leaves out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum More {
    /// Nothing.
    #[default]
    None,
    /// Lines after the first.
    Lines(usize),
    /// List items or table rows after the first.
    Items(usize),
}

/// A value prepared for display.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    /// The whole value as text: a string's content, list items or `key = value`
    /// rows one per line, else the TOML representation.
    pub full: String,
    /// What one line shows: the first line of a string, the first item or row.
    pub head: String,
    /// Shape (of the first item for lists).
    pub shape: Shape,
    /// What `head` leaves out.
    pub more: More,
    /// List items or table rows (line diffs compare these).
    pub items: Option<Vec<String>>,
}

impl Shown {
    /// A short token shown as is (`on-request`, `42`, a translated on / off).
    pub fn token(t: impl Into<String>) -> Self {
        let t = t.into();
        Self {
            shape: Shape::of(&t),
            head: t.clone(),
            full: t,
            more: More::None,
            items: None,
        }
    }

    /// A word in the UI font (a muted "not set").
    pub fn label(t: impl Into<String>) -> Self {
        Self {
            shape: Shape::Prose,
            ..Self::token(t)
        }
    }

    /// A string value: its content (an empty string shows as `""`).
    pub fn string(s: &str) -> Self {
        if s.is_empty() {
            return Self::token("\"\"");
        }
        let (line, more) = elide::first_line(s);
        Self {
            full: s.to_owned(),
            head: line.to_owned(),
            shape: Shape::of(s),
            more: if more > 0 {
                More::Lines(more)
            } else {
                More::None
            },
            items: None,
        }
    }

    /// A parsed setting value.
    pub fn setting(v: &SettingValue) -> Self {
        match v {
            SettingValue::String(s) => Self::string(s),
            SettingValue::Array(items) => {
                let items: Vec<String> = items.iter().map(item_text).collect();
                Self::list(items, "[]")
            }
            SettingValue::Table(map) => {
                let rows: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{k} = {}", item_text(v)))
                    .collect();
                Self::list(rows, "{}")
            }
            other => Self::token(other.to_toml_repr()),
        }
    }

    /// A TOML value representation (`"live"`, `["a", "b"]`, `{ A = "1" }`);
    /// shown as is when it does not parse.
    pub fn repr(repr: &str) -> Self {
        match SettingValue::from_toml_repr(repr) {
            Some(v) => Self::setting(&v),
            None => Self::token(repr),
        }
    }

    fn list(items: Vec<String>, empty: &str) -> Self {
        let Some(first) = items.first() else {
            return Self::token(empty);
        };
        let (line, _) = elide::first_line(first);
        Self {
            full: items.join("\n"),
            head: line.to_owned(),
            shape: match Shape::of(first) {
                Shape::Prose => Shape::Token,
                s => s,
            },
            more: if items.len() > 1 {
                More::Items(items.len() - 1)
            } else {
                More::None
            },
            items: Some(items),
        }
    }

    /// The hint after a one-line display ("+3 lines", "+2 more").
    pub fn hint(&self, cx: &Cx<'_>) -> Option<String> {
        match self.more {
            More::None => None,
            More::Lines(n) => Some(cx.tr.f("values.more_lines", &[("n", &n)])),
            More::Items(n) => Some(cx.tr.f("values.more_items", &[("n", &n)])),
        }
    }

    /// Lines a full diff compares.
    pub fn lines(&self) -> Vec<&str> {
        match &self.items {
            Some(items) => items.iter().map(String::as_str).collect(),
            None => self.full.lines().collect(),
        }
    }

    /// One line is not the whole value.
    pub fn is_partial(&self) -> bool {
        self.more != More::None
    }
}

/// A list item or table value: strings without quotes, the rest as TOML.
fn item_text(v: &SettingValue) -> String {
    match v {
        SettingValue::String(s) => s.clone(),
        other => other.to_toml_repr(),
    }
}

/// The value as a chip of at most `max_width` points (list rows, detail pane).
pub fn chip(ui: &mut Ui, cx: &Cx<'_>, v: &Shown, max_width: f32, muted: bool) -> egui::Response {
    ui.add(
        ValueChip::new(&v.head, v.shape)
            .hint(v.hint(cx))
            .tooltip(v.full.clone())
            .muted(muted)
            .max_width(max_width),
    )
}

/// The value as plain text of at most `max_width` points (diff rows).
pub fn plain(
    ui: &mut Ui,
    cx: &Cx<'_>,
    v: &Shown,
    max_width: f32,
    color: egui::Color32,
) -> egui::Response {
    ui.add(
        ValueChip::new(&v.head, v.shape)
            .plain()
            .hint(v.hint(cx))
            .tooltip(v.full.clone())
            .color(color)
            .max_width(max_width),
    )
}

/// True when `v` does not show whole in `width` points as one line.
pub fn cut(ui: &Ui, v: &Shown, width: f32) -> bool {
    if v.is_partial() {
        return true;
    }
    let f = value_font(v.shape, text::CAPTION);
    fit(ui, &v.head, &f, width, v.shape.ellipsis()) != v.head
}

/// `before → after` in exactly `width` points (each side gets half): never wider.
/// Returns true when either side is cut (the full diff has more to show).
pub fn change(ui: &mut Ui, cx: &Cx<'_>, before: &Shown, after: &Shown, width: f32) -> bool {
    let side = ((width - 22.0) / 2.0).max(24.0);
    let cut_any = cut(ui, before, side - 2.0) || cut(ui, after, side - 2.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 20.0), egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.spacing_mut().item_spacing.x = 0.0;
    child.allocate_ui(vec2(side, 20.0), |ui| {
        plain(ui, cx, before, side, cx.pal.text_muted);
    });
    let (arrow, _) = child.allocate_exact_size(vec2(22.0, 20.0), egui::Sense::hover());
    icons::paint(
        child.painter(),
        egui::Rect::from_center_size(arrow.center(), vec2(12.0, 12.0)),
        Icon::ArrowRight,
        cx.pal.text_subtle,
    );
    child.allocate_ui(vec2(side, 20.0), |ui| {
        plain(ui, cx, after, side, cx.pal.text);
    });
    cut_any
}

/// A chevron that opens and closes something kept in egui memory under `id`;
/// returns whether it is open after this frame.
pub fn disclosure(ui: &mut Ui, cx: &Cx<'_>, id: Id) -> bool {
    let open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let (icon, tip) = if open {
        (Icon::ChevronDown, cx.tr.t("values.collapse"))
    } else {
        (Icon::ChevronRight, cx.tr.t("values.expand"))
    };
    let open = if ui.add(IconButton::new(icon, tip).size(22.0)).clicked() {
        !open
    } else {
        open
    };
    ui.data_mut(|d| d.insert_temp(id, open));
    open
}

/// Whether the disclosure `id` is open.
pub fn is_open(ui: &Ui, id: Id) -> bool {
    ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false)
}

/// The lines of the full change of `before` → `after` (`None` = not set): a
/// line diff of text, an item diff of lists and tables.
pub fn diff_rows<'a>(
    before: Option<&'a Shown>,
    after: Option<&'a Shown>,
) -> Vec<(DiffOp, &'a str)> {
    let a = before.map(Shown::lines).unwrap_or_default();
    let b = after.map(Shown::lines).unwrap_or_default();
    elide::diff_lines(&a, &b)
}

/// The whole change in a box as wide as the space it is given, at most
/// `max_height` points high (it scrolls inside): unchanged lines muted, removed
/// lines with `-`, added lines with `+`; lines wrap. `unset` names a missing side.
pub fn full_diff(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: Id,
    before: Option<&Shown>,
    after: Option<&Shown>,
    unset: &str,
    max_height: f32,
) {
    let pal = cx.pal;
    let rows = diff_rows(before, after);
    let mono = before
        .or(after)
        .is_none_or(|v| v.shape.mono() || v.items.is_some());
    let width = ui.available_width();
    egui::Frame::new()
        .fill(pal.bg_subtle)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            // Inner margin and stroke on both sides: the box is exactly `width`.
            ui.set_width((width - 18.0).max(0.0));
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let line = |ui: &mut Ui,
                                mark: &str,
                                s: &str,
                                color: egui::Color32,
                                fill: egui::Color32| {
                        egui::Frame::new().fill(fill).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.add_sized(
                                    vec2(10.0, 16.0),
                                    egui::Label::new(
                                        RichText::new(mark)
                                            .font(value_font(Shape::Token, text::CAPTION))
                                            .color(color),
                                    ),
                                );
                                let f = value_font(
                                    if mono { Shape::Token } else { Shape::Prose },
                                    text::CAPTION,
                                );
                                // An empty line keeps its height.
                                let s = if s.is_empty() { " " } else { s };
                                ui.add(
                                    egui::Label::new(RichText::new(s).font(f).color(color))
                                        .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                            });
                        });
                    };
                    if before.is_none() {
                        line(ui, "-", unset, pal.text_subtle, egui::Color32::TRANSPARENT);
                    }
                    for (op, s) in &rows {
                        match op {
                            DiffOp::Same => {
                                line(ui, " ", s, pal.text_muted, egui::Color32::TRANSPARENT)
                            }
                            DiffOp::Removed => {
                                line(ui, "-", s, pal.red, pal.red.gamma_multiply(0.08))
                            }
                            DiffOp::Added => {
                                line(ui, "+", s, pal.green, pal.green.gamma_multiply(0.08))
                            }
                        }
                    }
                    if after.is_none() {
                        line(ui, "+", unset, pal.text_subtle, egui::Color32::TRANSPARENT);
                    }
                });
        });
}

/// One text of at most `width` points in a fixed column (cut with `mode`, the
/// whole text in the tooltip when cut).
pub fn column(
    ui: &mut Ui,
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
    width: f32,
    mode: elide::Ellipsis,
) {
    let shown = fit(ui, text, &font_id, width, mode);
    let (r, resp) = ui.allocate_exact_size(vec2(width, 20.0), egui::Sense::hover());
    ui.painter().text(
        egui::pos2(r.min.x, r.center().y),
        egui::Align2::LEFT_CENTER,
        &shown,
        font_id,
        color,
    );
    if shown != text {
        resp.on_hover_text(text);
    }
}

/// A `path  before → after` row of a change list in fixed columns across the
/// available width — an optional `prefix` (the Codex home), the path, the change
/// (TOML representations, `None` = not set, shown as `unset`), an optional
/// `reason` — and, when a value is cut, a chevron that opens the full change
/// below the row (kept open under `id`). Never wider than the space given.
#[allow(clippy::too_many_arguments)]
pub fn change_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: Id,
    prefix: Option<&str>,
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
    reason: Option<&str>,
    unset: &str,
) {
    let width = ui.available_width();
    let missing = Shown::label(unset);
    let b = before.map(Shown::repr).unwrap_or_else(|| missing.clone());
    let a = after.map(Shown::repr).unwrap_or_else(|| missing.clone());
    let gap = 8.0;
    let prefix_w = if prefix.is_some() { 84.0 } else { 0.0 };
    let reason_w = if reason.is_some() { 96.0 } else { 0.0 };
    let rest = width - 22.0 - prefix_w - reason_w - gap * 3.0;
    let path_w = (rest * 0.36).clamp(60.0, 220.0);
    let change_w = (rest - path_w - gap).max(60.0);
    let side = ((change_w - 22.0) / 2.0).max(24.0);
    let is_cut = cut(ui, &b, side - 2.0) || cut(ui, &a, side - 2.0);
    let (row, _) = ui.allocate_exact_size(vec2(width, 22.0), egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(row.intersect(ui.clip_rect()));
    child.spacing_mut().item_spacing.x = gap;
    let pal = cx.pal;
    if let Some(p) = prefix {
        column(
            &mut child,
            p,
            crate::design::fonts::font(text::CAPTION, crate::design::fonts::Weight::Medium),
            pal.text_subtle,
            prefix_w,
            elide::Ellipsis::End,
        );
    }
    column(
        &mut child,
        path,
        crate::design::fonts::mono_font(text::CAPTION, crate::design::fonts::Weight::Regular),
        pal.text,
        path_w,
        elide::Ellipsis::Middle,
    );
    change(&mut child, cx, &b, &a, change_w);
    if let Some(r) = reason {
        column(
            &mut child,
            r,
            crate::design::fonts::font(text::CAPTION, crate::design::fonts::Weight::Medium),
            pal.text_subtle,
            reason_w,
            elide::Ellipsis::End,
        );
    }
    if is_cut || is_open(&child, id) {
        disclosure(&mut child, cx, id);
    }
    if is_open(ui, id) {
        full_diff(
            ui,
            cx,
            id.with("full"),
            before.map(|_| &b),
            after.map(|_| &a),
            unset,
            160.0,
        );
    }
}
