//! Type-driven value editors of the detail pane (user bug 15). Short strings,
//! enums and numbers keep their inline field (`detail::editor`); this module
//! draws the rest across the pane's width:
//!
//! - long text (over [`LONG`] characters or several lines, e.g.
//!   `developer_instructions`): a wrapped read-only preview of up to six lines
//!   that fades out, "Show all" growing it in place, and "Edit" opening
//!   [`TextEditor`], a large monospace dialog (wrap toggle, line and character
//!   count, the default for comparison; Esc cancels, Cmd/Ctrl+Enter saves the
//!   text exactly as typed);
//! - paths: the field across the pane with "Choose…" (native dialogs builds) and
//!   "Show in folder" when the path exists;
//! - lists (`notify`, `writable_roots`): one row per item with move up / down
//!   and remove, plus "Add"; tables of scalars (`shell_environment_policy.set`):
//!   key = value rows. Every change goes straight into the pending edits as a TOML
//!   array or inline table. Longer lists open the same rows in a dialog;
//! - other composites: their TOML text as a read-only preview.
//!
//! A copy button sits next to every value.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Id, Key, Layout, Modifiers, RichText, TextEdit, Ui, vec2};

use super::State;
use super::model::{
    Change, Entry, Item, Kind, Model, ParseError, Pending, Value, list_items, list_value, map_rows,
    map_value, move_row,
};
use crate::design::elide::Shape;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, text};
use crate::design::widgets::value_chip::fit;
use crate::design::widgets::{Badge, Button, IconButton, Text, Toggle, dialog, link_button};
use crate::pages::shared::values::{self, Shown};
use crate::pages::{Action, Cx};

/// Longer strings (in characters) get the preview and the large editor.
pub const LONG: usize = 60;
/// Preview lines before "Show all".
const PREVIEW_LINES: usize = 6;
/// List and map rows shown in the pane; more open the rows dialog.
const PANE_ROWS: usize = 5;
/// Height of one editor row.
const ROW_H: f32 = 28.0;

/// True for text that gets the preview and the large editor.
pub fn is_long(s: &str) -> bool {
    s.chars().count() > LONG || s.contains('\n')
}

/// The value is drawn across the pane below its label (paths, long text, lists,
/// maps, composites) instead of inline.
pub fn wide(e: &Entry, pending: &Pending) -> bool {
    if e.owner.is_some() {
        return false;
    }
    let value = e.value_with(pending);
    match &e.kind {
        Kind::Text { path: true } | Kind::List { .. } | Kind::Map { .. } => true,
        Kind::Text { path: false } => value.and_then(Value::as_str).is_some_and(is_long),
        Kind::ReadOnly => value.is_some(),
        _ => false,
    }
}

/// The value for display (`on` / `off` translated).
pub fn shown(cx: &Cx<'_>, v: &Value) -> Shown {
    match v {
        Value::Bool(_) => Shown::label(super::view::value_text(cx, v)),
        Value::Str(s) => Shown::string(s),
        Value::Int(_) | Value::Float(_) => Shown::token(v.to_string()),
        Value::Other(repr) => Shown::repr(repr),
    }
}

/// What the copy button copies: a string as is, anything else as TOML.
pub fn copy_text(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The large text editor of one string setting.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEditor {
    /// Config path.
    pub path: String,
    /// Display name.
    pub title: String,
    /// The text being edited.
    pub buffer: String,
    /// The value when the editor opened (`None` = not set).
    pub original: Option<String>,
    /// Codex's default, for comparison.
    pub default: Option<String>,
    /// Lines wrap at the editor width.
    pub wrap: bool,
    /// Viewing only (the key is owned elsewhere, or not a string).
    pub read_only: bool,
    focused: bool,
}

impl TextEditor {
    /// Open on `current` (`None` = not set).
    pub fn new(
        path: &str,
        title: &str,
        current: Option<&str>,
        default: Option<String>,
        read_only: bool,
    ) -> Self {
        Self {
            path: path.to_owned(),
            title: title.to_owned(),
            buffer: current.unwrap_or_default().to_owned(),
            original: current.map(str::to_owned),
            default,
            wrap: true,
            read_only,
            focused: false,
        }
    }

    /// The edit saving makes: the text exactly as typed (whitespace, line breaks
    /// and quotes kept), [`Change::Unset`] for an empty text, `None` when nothing
    /// changed.
    pub fn change(&self) -> Option<Change> {
        if self.read_only || Some(self.buffer.as_str()) == self.original.as_deref() {
            return None;
        }
        if self.buffer.is_empty() {
            return self.original.is_some().then_some(Change::Unset);
        }
        Some(Change::Set(Value::Str(self.buffer.clone())))
    }

    /// Lines and characters of the text.
    pub fn counts(&self) -> (usize, usize) {
        let lines = self.buffer.lines().count();
        (lines, self.buffer.chars().count())
    }
}

/// Row buffers of a list or map editor, rebuilt whenever the value they were
/// built from changes (discard, reset, reload).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    /// The value the rows show.
    pub source: Option<Value>,
    /// List items.
    pub list: Vec<String>,
    /// Map rows.
    pub map: Vec<(String, String)>,
    /// Focus this row's field next frame.
    focus: Option<usize>,
}

impl Rows {
    /// Rows of `value` (`None` = not set: no rows).
    pub fn of(value: Option<&Value>) -> Self {
        Self {
            source: value.cloned(),
            list: value.and_then(list_items).unwrap_or_default(),
            map: value.and_then(map_rows).unwrap_or_default(),
            focus: None,
        }
    }

    /// The edit the list rows make for `entry` (`None` when they show its value).
    pub fn list_change(&self, entry: &Entry, item: &Item) -> Result<Option<Change>, ParseError> {
        let v = list_value(&self.list, item)?;
        Ok(self.change_to(entry, v, "[]"))
    }

    /// The edit the map rows make for `entry`.
    pub fn map_change(&self, entry: &Entry, value: &Item) -> Result<Option<Change>, ParseError> {
        let v = map_value(&self.map, value)?;
        Ok(self.change_to(entry, v, "{}"))
    }

    fn change_to(&self, entry: &Entry, v: Value, empty: &str) -> Option<Change> {
        if Some(&v) == self.source.as_ref() {
            return None;
        }
        // Emptying a list or map that was not set leaves it unset.
        if entry.current.is_none() && v == Value::Other(empty.to_owned()) {
            return Some(Change::Unset);
        }
        Some(Change::Set(v))
    }
}

impl State {
    /// Open the large editor for the string setting at `path` (read-only for keys
    /// this page cannot edit).
    pub fn open_editor(&mut self, path: &str) {
        let Some(model) = &self.model else {
            return;
        };
        let Some(e) = model.entry(path) else {
            return;
        };
        let current = e.value_with(&self.pending);
        let text = current.map(copy_text);
        let editable = e.editable()
            && matches!(e.kind, Kind::Text { .. })
            && model.config_error.is_none()
            && model.profile_id.is_some();
        let default = e.default.as_ref().map(copy_text);
        self.editor = Some(TextEditor::new(
            path,
            e.title_for(false).text,
            text.as_deref(),
            default,
            !editable,
        ));
    }

    /// The open large editor.
    pub fn editor_mut(&mut self) -> Option<&mut TextEditor> {
        self.editor.as_mut()
    }

    /// Save the large editor's text into the pending edits and close it.
    pub fn save_editor(&mut self) {
        let Some(ed) = self.editor.take() else {
            return;
        };
        if let (Some(change), Some(model)) = (ed.change(), &self.model) {
            self.pending.set_path(model, &ed.path, change);
            self.buffers.remove(&ed.path);
            self.invalid.remove(&ed.path);
        }
    }

    /// Close the large editor without saving.
    pub fn cancel_editor(&mut self) {
        self.editor = None;
    }

    /// The list / map rows of `path` for its current value.
    pub fn rows_for(&mut self, path: &str) -> Option<&mut Rows> {
        let model = self.model.as_ref()?;
        let e = model.entry(path)?;
        let value = e.value_with(&self.pending).cloned();
        let rows = self
            .rows
            .entry(path.to_owned())
            .or_insert_with(|| Rows::of(value.as_ref()));
        if rows.source != value {
            *rows = Rows::of(value.as_ref());
        }
        Some(rows)
    }

    /// Record the edit of the rows of `path` (or the reason it is refused).
    pub fn commit_rows(&mut self, path: &str) {
        let Some(model) = self.model.clone() else {
            return;
        };
        let Some(e) = model.entry(path) else {
            return;
        };
        let Some(rows) = self.rows.get_mut(path) else {
            return;
        };
        let change = match &e.kind {
            Kind::List { item } => rows.list_change(e, item),
            Kind::Map { value } => rows.map_change(e, value),
            _ => return,
        };
        match change {
            Ok(Some(c)) => {
                self.invalid.remove(path);
                self.pending.set_path(&model, path, c);
                let now = model
                    .entry(path)
                    .and_then(|e| e.value_with(&self.pending).cloned());
                if let Some(rows) = self.rows.get_mut(path) {
                    rows.source = now;
                }
            }
            Ok(None) => {
                self.invalid.remove(path);
            }
            Err(err) => {
                self.invalid.insert(path.to_owned(), err.key());
            }
        }
    }
}

/// The modifier key of shortcuts, as shown in hints.
fn command_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// The label row of a wide value: "Value" (+ a count), actions on the right.
fn header(ui: &mut Ui, cx: &Cx<'_>, count: Option<String>, actions: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(cx.tr.t("codex_settings.detail.value"))
                .font(font(text::SMALL, Weight::Regular))
                .color(cx.pal.text_muted),
        );
        if let Some(c) = count {
            ui.label(Text::caption(c, cx.pal));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
}

fn copy_button(ui: &mut Ui, cx: &Cx<'_>, v: Option<&Value>, out: &mut Vec<Action>) {
    if let Some(v) = v
        && ui
            .add(IconButton::new(Icon::Copy, cx.tr.t("codex_settings.detail.copy")).size(24.0))
            .clicked()
    {
        out.push(Action::Copy(copy_text(v)));
    }
}

/// The value of `e` across the pane (see the module docs).
pub(super) fn wide_value(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    model: &Model,
    e: &Entry,
    read_only: bool,
    out: &mut Vec<Action>,
) {
    let current = e.value_with(&state.pending).cloned();
    let enabled = !read_only && e.editable();
    match &e.kind {
        Kind::Text { path: true } => path_field(ui, cx, state, model, e, enabled, out),
        Kind::Text { path: false } => {
            let mut edit = false;
            header(ui, cx, None, |ui| {
                if ui
                    .add(
                        Button::ghost(cx.tr.t(if enabled {
                            "codex_settings.detail.edit"
                        } else {
                            "codex_settings.detail.view"
                        }))
                        .small(),
                    )
                    .clicked()
                {
                    edit = true;
                }
                copy_button(ui, cx, current.as_ref(), out);
            });
            if let Some(Value::Str(s)) = &current {
                preview(ui, cx, Id::unique(("codex-settings-preview", &e.path)), s);
            }
            if edit {
                state.open_editor(&e.path);
            }
        }
        Kind::List { item } => {
            let n = state.rows_for(&e.path).map_or(0, |r| r.list.len());
            header(
                ui,
                cx,
                Some(cx.tr.f("codex_settings.detail.items", &[("n", &n)])),
                |ui| copy_button(ui, cx, current.as_ref(), out),
            );
            list_rows(ui, cx, state, e, item, enabled, Some(PANE_ROWS));
        }
        Kind::Map { value } => {
            let n = state.rows_for(&e.path).map_or(0, |r| r.map.len());
            header(
                ui,
                cx,
                Some(cx.tr.f("codex_settings.detail.items", &[("n", &n)])),
                |ui| copy_button(ui, cx, current.as_ref(), out),
            );
            map_rows_ui(ui, cx, state, e, value, enabled, Some(PANE_ROWS));
        }
        _ => {
            let mut view = false;
            header(ui, cx, None, |ui| {
                if current.as_ref().is_some_and(|v| is_long(&v.to_string()))
                    && ui
                        .add(Button::ghost(cx.tr.t("codex_settings.detail.view")).small())
                        .clicked()
                {
                    view = true;
                }
                copy_button(ui, cx, current.as_ref(), out);
            });
            if let Some(v) = &current {
                let shown = shown(cx, v);
                let text = match &shown.items {
                    Some(_) => shown.full.clone(),
                    None => v.to_string(),
                };
                preview(
                    ui,
                    cx,
                    Id::unique(("codex-settings-preview", &e.path)),
                    &text,
                );
            }
            ui.label(Text::caption(
                cx.tr.t("codex_settings.detail.edit_in_file"),
                cx.pal,
            ));
            if view {
                let text = current.as_ref().map(ToString::to_string);
                state.editor = Some(TextEditor::new(
                    &e.path,
                    e.title_for(false).text,
                    text.as_deref(),
                    e.default.as_ref().map(ToString::to_string),
                    true,
                ));
            }
        }
    }
}

/// Wrapped preview of `text` (up to six lines, fading out; "Show all" grows it
/// to the room the pane has left).
fn preview(ui: &mut Ui, cx: &Cx<'_>, id: Id, text: &str) {
    let pal = cx.pal;
    let shape = Shape::of(text);
    let f = if shape.mono() && shape != Shape::Prose {
        mono_font(text::CAPTION, Weight::Regular)
    } else {
        font(text::SMALL, Weight::Regular)
    };
    let open = values::is_open(ui, id);
    let width = ui.available_width() - 20.0;
    let line_h = 18.0;
    // Room for the rows below the value (default, current, hosts, reset).
    let room = ((ui.available_height() - 150.0) / line_h).floor().max(3.0) as usize;
    let max_rows = if open {
        room.max(PREVIEW_LINES)
    } else {
        PREVIEW_LINES
    };
    let layout = |rows: usize| -> Arc<egui::Galley> {
        let mut job = LayoutJob::single_section(
            text.to_owned(),
            egui::TextFormat {
                font_id: f.clone(),
                color: pal.text,
                line_height: Some(line_h),
                ..Default::default()
            },
        );
        job.wrap = TextWrapping {
            max_width: width,
            max_rows: rows,
            break_anywhere: shape != Shape::Prose,
            overflow_character: Some('…'),
        };
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let full_rows = layout(usize::MAX).rows.len();
    let galley = layout(max_rows);
    let cut = full_rows > max_rows;
    let frame = egui::Frame::new()
        .fill(pal.bg_subtle)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::symmetric(10, 8));
    let r = frame.show(ui, |ui| {
        ui.set_width(width);
        let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
        ui.painter().galley(rect.min, galley.clone(), pal.text);
        if cut {
            // Fade the last two lines into the background.
            let fade_h = line_h * 2.0;
            let steps = 8;
            for i in 0..steps {
                let t = (i + 1) as f32 / steps as f32;
                let y0 = rect.max.y - fade_h + fade_h * i as f32 / steps as f32;
                let band = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x - 2.0, y0),
                    egui::pos2(rect.max.x + 2.0, y0 + fade_h / steps as f32),
                );
                ui.painter()
                    .rect_filled(band, 0.0, pal.bg_subtle.gamma_multiply(t * 0.85));
            }
        }
        rect
    });
    let _ = r;
    let chars = text.chars().count();
    let lines = text.lines().count();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if (cut || open) && full_rows > PREVIEW_LINES {
            let label = cx.tr.t(if open {
                "codex_settings.detail.collapse"
            } else {
                "codex_settings.detail.expand"
            });
            if link_button(ui, &label, false).clicked() {
                ui.data_mut(|d| d.insert_temp(id, !open));
            }
        }
        ui.label(Text::caption(
            cx.tr.f(
                "codex_settings.editor.count",
                &[("lines", &lines), ("chars", &chars)],
            ),
            cx.pal,
        ));
    });
}

/// A path setting: the field across the pane, then Choose… / Show in folder.
fn path_field(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    model: &Model,
    e: &Entry,
    enabled: bool,
    out: &mut Vec<Action>,
) {
    let current = e.value_with(&state.pending).cloned();
    header(ui, cx, None, |ui| {
        copy_button(ui, cx, current.as_ref(), out)
    });
    let initial = current.as_ref().map(copy_text).unwrap_or_default();
    let buffer = state
        .buffers
        .entry(e.path.clone())
        .or_insert_with(|| initial.clone());
    let hint = e
        .default
        .as_ref()
        .map(copy_text)
        .unwrap_or_else(|| cx.tr.t("codex_settings.value.codex_decides"));
    let width = ui.available_width();
    let r = field(
        ui,
        Id::unique(("codex-settings-path", &e.path)),
        buffer,
        width,
        &hint,
        enabled,
        &cx.tr.t("codex_settings.detail.value"),
    );
    let enter = r.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    let mut commit: Option<String> = None;
    if (r.lost_focus() || enter) && *buffer != initial {
        commit = Some(buffer.clone());
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        #[cfg(feature = "file-dialogs")]
        if enabled
            && link_button(ui, &cx.tr.t("codex_settings.detail.pick_file"), false).clicked()
            && let Some(p) = rfd::FileDialog::new().pick_file()
        {
            commit = Some(p.display().to_string());
        }
        if let Some(dir) = current
            .as_ref()
            .and_then(Value::as_str)
            .and_then(existing_parent)
            && link_button(ui, &cx.tr.t("codex_settings.detail.reveal"), true)
                .on_hover_text(superba_core::paths::display_path(&dir))
                .clicked()
        {
            out.push(Action::OpenUrl(dir.display().to_string()));
        }
    });
    if let Some(text) = commit {
        let text = text.trim().to_owned();
        state.buffers.insert(e.path.clone(), text.clone());
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

/// The folder holding `path` (or `path` itself when it is a folder), when it
/// exists; `~/` is the home folder.
fn existing_parent(path: &str) -> Option<std::path::PathBuf> {
    let p = match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(rest))?,
        None => std::path::PathBuf::from(path),
    };
    if !p.is_absolute() {
        return None;
    }
    if p.is_dir() {
        return Some(p);
    }
    p.parent()
        .filter(|d| d.is_dir())
        .map(std::path::Path::to_path_buf)
}

/// A one-line text field. While it is not being edited and its text does not fit,
/// it shows the text shortened by shape (a path keeps its file name, a URL its
/// host) inside the same field frame; a click edits it. The response is the text
/// field's (`changed`, `lost_focus`) while editing. `name` is its accessible
/// name (user bug 56).
fn field(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    width: f32,
    hint: &str,
    enabled: bool,
    name: &str,
) -> egui::Response {
    let margin = vec2(6.0, 4.0);
    let editing_key = id.with("editing");
    let wants_edit = ui
        .data(|d| d.get_temp::<bool>(editing_key))
        .unwrap_or(false);
    let focused = ui.memory(|m| m.has_focus(id));
    let f = mono_font(text::SMALL, Weight::Regular);
    let shape = Shape::of(text);
    let shown = if text.is_empty() {
        String::new()
    } else {
        fit(ui, text, &f, width - 2.0 * margin.x - 2.0, shape.ellipsis())
    };
    if focused || wants_edit || shown == *text {
        let r = ui.add_enabled(
            enabled,
            TextEdit::singleline(text)
                .id(id)
                .font(egui::TextStyle::Monospace)
                .hint_text(hint)
                .desired_width(width)
                .margin(margin),
        );
        let r = r.accessible_name(name);
        if wants_edit {
            r.request_focus();
            ui.data_mut(|d| d.remove::<bool>(editing_key));
        }
        return r;
    }
    let h = ui.fonts_mut(|fonts| fonts.row_height(&f)) + 2.0 * margin.y;
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), sense);
    // A text field to screen readers too: a click edits it.
    resp.widget_info(|| egui::WidgetInfo::text_edit(enabled, text.as_str(), text.as_str(), hint));
    let resp = resp.accessible_name(name);
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let w = if resp.hovered() && enabled {
            &v.widgets.hovered
        } else {
            &v.widgets.inactive
        };
        ui.painter().rect(
            rect,
            w.corner_radius,
            v.text_edit_bg_color(),
            w.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let color = if enabled {
            v.text_color()
        } else {
            v.weak_text_color()
        };
        let g = ui.painter().layout_no_wrap(shown, f, color);
        ui.painter().with_clip_rect(rect.shrink(1.0)).galley(
            egui::pos2(rect.min.x + margin.x, rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
    }
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(editing_key, true));
        ui.ctx().request_repaint();
    }
    crate::design::widgets::value_chip::value_tooltip(
        resp.on_hover_cursor(egui::CursorIcon::Text),
        text,
        shape,
    )
}

/// A small square icon button in a row.
fn row_button(ui: &mut Ui, icon: Icon, tip: String, enabled: bool) -> bool {
    ui.add(IconButton::new(icon, tip).size(24.0).enabled(enabled))
        .clicked()
}

/// One row per list item: field, up, down, remove; then "Add". `limit` caps the
/// rows (the rest open the rows dialog).
fn list_rows(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    e: &Entry,
    item: &Item,
    enabled: bool,
    limit: Option<usize>,
) {
    let Some(rows) = state.rows_for(&e.path) else {
        return;
    };
    let mut rows = rows.clone();
    let n = rows.list.len();
    let shown_n = limit.map_or(n, |l| n.min(l));
    let mut changed = false;
    let mut op: Option<(usize, isize)> = None;
    let mut remove: Option<usize> = None;
    let focus = rows.focus.take();
    for i in 0..shown_n {
        ui.horizontal(|ui| {
            ui.set_min_height(ROW_H);
            ui.spacing_mut().item_spacing.x = 2.0;
            let w = ui.available_width() - 3.0 * 26.0;
            let r = field(
                ui,
                Id::unique(("codex-settings-item", &e.path, i)),
                &mut rows.list[i],
                w,
                "",
                enabled,
                &format!("{} {}", cx.tr.t("codex_settings.detail.value"), i + 1),
            );
            if focus == Some(i) {
                r.request_focus();
            }
            if r.changed() {
                changed = true;
            }
            if row_button(
                ui,
                Icon::ArrowUp,
                cx.tr.t("codex_settings.detail.move_up"),
                enabled && i > 0,
            ) {
                op = Some((i, -1));
            }
            if row_button(
                ui,
                Icon::ArrowDown,
                cx.tr.t("codex_settings.detail.move_down"),
                enabled && i + 1 < n,
            ) {
                op = Some((i, 1));
            }
            if row_button(
                ui,
                Icon::Close,
                cx.tr.t("codex_settings.detail.remove"),
                enabled,
            ) {
                remove = Some(i);
            }
        });
    }
    if n == 0 {
        ui.label(Text::small(
            cx.tr.t("codex_settings.detail.empty_list"),
            cx.pal,
        ));
    }
    let mut open_dialog = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if enabled
            && ui
                .add(
                    Button::ghost(cx.tr.t("codex_settings.detail.add"))
                        .icon(Icon::Plus)
                        .small(),
                )
                .clicked()
        {
            rows.list.push(String::new());
            rows.focus = Some(rows.list.len() - 1);
            if limit.is_some_and(|l| rows.list.len() > l) {
                open_dialog = true;
            }
        }
        if shown_n < n
            && link_button(
                ui,
                &cx.tr.f("codex_settings.detail.all_rows", &[("n", &n)]),
                false,
            )
            .clicked()
        {
            open_dialog = true;
        }
        #[cfg(feature = "file-dialogs")]
        if enabled
            && item.is_path()
            && link_button(ui, &cx.tr.t("codex_settings.detail.pick_folder"), false).clicked()
            && let Some(p) = rfd::FileDialog::new().pick_folder()
        {
            rows.list.push(p.display().to_string());
            changed = true;
        }
    });
    let _ = item;
    if let Some((i, d)) = op {
        changed |= move_row(&mut rows.list, i, d);
    }
    if let Some(i) = remove {
        rows.list.remove(i);
        changed = true;
    }
    if let Some(slot) = state.rows.get_mut(&e.path) {
        *slot = rows;
    }
    if changed {
        state.commit_rows(&e.path);
    }
    if open_dialog {
        state.rows_dialog = Some(e.path.clone());
    }
}

/// Key = value rows of a map, then "Add".
fn map_rows_ui(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    e: &Entry,
    value: &Item,
    enabled: bool,
    limit: Option<usize>,
) {
    let Some(rows) = state.rows_for(&e.path) else {
        return;
    };
    let mut rows = rows.clone();
    let n = rows.map.len();
    let shown_n = limit.map_or(n, |l| n.min(l));
    let mut changed = false;
    let mut remove: Option<usize> = None;
    let focus = rows.focus.take();
    let options = match value {
        Item::Enum(o) => Some(o.join(" / ")),
        _ => None,
    };
    for i in 0..shown_n {
        ui.horizontal(|ui| {
            ui.set_min_height(ROW_H);
            ui.spacing_mut().item_spacing.x = 4.0;
            let avail = ui.available_width() - 26.0 - 16.0;
            let key_w = (avail * 0.38).max(60.0);
            let (k, v) = &mut rows.map[i];
            let rk = field(
                ui,
                Id::unique(("codex-settings-key", &e.path, i)),
                k,
                key_w,
                &cx.tr.t("codex_settings.detail.key"),
                enabled,
                &format!("{} {}", cx.tr.t("codex_settings.detail.key"), i + 1),
            );
            if focus == Some(i) {
                rk.request_focus();
            }
            ui.label(Text::mono_muted("=", cx.pal));
            let hint = options
                .clone()
                .unwrap_or_else(|| cx.tr.t("codex_settings.detail.map_value"));
            let rv = field(
                ui,
                Id::unique(("codex-settings-val", &e.path, i)),
                v,
                avail - key_w - 8.0,
                &hint,
                enabled,
                &format!("{} {}", cx.tr.t("codex_settings.detail.map_value"), i + 1),
            );
            if rk.changed() || rv.changed() {
                changed = true;
            }
            if row_button(
                ui,
                Icon::Close,
                cx.tr.t("codex_settings.detail.remove"),
                enabled,
            ) {
                remove = Some(i);
            }
        });
    }
    if n == 0 {
        ui.label(Text::small(
            cx.tr.t("codex_settings.detail.empty_map"),
            cx.pal,
        ));
    }
    let mut open_dialog = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if enabled
            && ui
                .add(
                    Button::ghost(cx.tr.t("codex_settings.detail.add"))
                        .icon(Icon::Plus)
                        .small(),
                )
                .clicked()
        {
            rows.map.push((String::new(), String::new()));
            rows.focus = Some(rows.map.len() - 1);
            if limit.is_some_and(|l| rows.map.len() > l) {
                open_dialog = true;
            }
        }
        if shown_n < n
            && link_button(
                ui,
                &cx.tr.f("codex_settings.detail.all_rows", &[("n", &n)]),
                false,
            )
            .clicked()
        {
            open_dialog = true;
        }
    });
    if let Some(i) = remove {
        rows.map.remove(i);
        changed = true;
    }
    if let Some(slot) = state.rows.get_mut(&e.path) {
        *slot = rows;
    }
    if changed {
        state.commit_rows(&e.path);
    }
    if open_dialog {
        state.rows_dialog = Some(e.path.clone());
    }
}

/// The page's value dialogs: the large text editor and the rows dialog.
pub(super) fn dialogs(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, model: &Model) {
    if state.editor.is_some() {
        editor_dialog(ctx, cx, state, model);
    }
    if state.rows_dialog.is_some() {
        rows_dialog(ctx, cx, state, model);
    }
}

/// Width of the large editor: 760 points, less on small windows.
fn editor_width(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().width() - 120.0).clamp(420.0, 760.0)
}

fn editor_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, model: &Model) {
    let Some(mut ed) = state.editor.clone() else {
        return;
    };
    let zh = cx.tr.locale() == superba_core::Locale::ZhCn;
    let title = model
        .entry(&ed.path)
        .map(|e| e.title_for(zh).text.to_owned())
        .unwrap_or_else(|| ed.title.clone());
    let width = editor_width(ctx);
    let height = (ctx.content_rect().height() - 330.0).clamp(160.0, 440.0);
    let mut save = false;
    let mut cancel = false;
    let mut copy: Option<String> = None;
    let r = dialog(ctx, Id::unique("codex-settings-editor"), width, |ui| {
        // Cmd/Ctrl+Enter saves; taken before the text field sees the Enter.
        if !ed.read_only && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            save = true;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add(
                egui::Label::new(Text::h2(
                    if ed.read_only {
                        title.clone()
                    } else {
                        cx.tr.f("codex_settings.editor.title", &[("name", &title)])
                    },
                    cx.pal,
                ))
                .truncate(),
            );
            if ed.read_only {
                ui.add(Badge::new(cx.tr.t("codex_settings.editor.read_only")).outline());
            }
        });
        ui.add(egui::Label::new(Text::mono_muted(&ed.path, cx.pal)).truncate());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add(Toggle::new(
                &mut ed.wrap,
                cx.tr.t("codex_settings.editor.wrap"),
            ));
            ui.label(Text::small(cx.tr.t("codex_settings.editor.wrap"), cx.pal));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        IconButton::new(Icon::Copy, cx.tr.t("codex_settings.detail.copy"))
                            .size(24.0),
                    )
                    .clicked()
                {
                    copy = Some(ed.buffer.clone());
                }
                let (lines, chars) = ed.counts();
                ui.label(Text::caption(
                    cx.tr.f(
                        "codex_settings.editor.count",
                        &[("lines", &lines), ("chars", &chars)],
                    ),
                    cx.pal,
                ));
            });
        });
        let wrap = ed.wrap;
        let color = cx.pal.text;
        let mono = mono_font(text::SMALL, Weight::Regular);
        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = LayoutJob::simple(
                buf.as_str().to_owned(),
                mono.clone(),
                color,
                if wrap { wrap_width } else { f32::INFINITY },
            );
            job.keep_trailing_whitespace = true;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, cx.pal.border))
            .corner_radius(radius::CONTROL)
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                let scroll = if wrap {
                    egui::ScrollArea::vertical()
                } else {
                    egui::ScrollArea::both()
                };
                scroll
                    .id_salt("codex-settings-editor-text")
                    .max_height(height)
                    .min_scrolled_height(height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        let rows = (height / 18.0) as usize;
                        let resp = if ed.read_only {
                            let mut view: &str = ed.buffer.as_str();
                            ui.add(
                                TextEdit::multiline(&mut view)
                                    .id(Id::unique("codex-settings-editor-field"))
                                    .layouter(&mut layouter)
                                    .desired_width(w)
                                    .desired_rows(rows)
                                    .frame(egui::Frame::NONE),
                            )
                            .accessible_name(ed.title.clone())
                        } else {
                            ui.add(
                                TextEdit::multiline(&mut ed.buffer)
                                    .id(Id::unique("codex-settings-editor-field"))
                                    .layouter(&mut layouter)
                                    .desired_width(w)
                                    .desired_rows(rows)
                                    .lock_focus(true)
                                    .hint_text(cx.tr.t("codex_settings.editor.empty_hint"))
                                    .frame(egui::Frame::NONE),
                            )
                            .accessible_name(ed.title.clone())
                        };
                        if !ed.focused {
                            resp.request_focus();
                            ed.focused = true;
                        }
                    });
            });
        // The default, for comparison.
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.allocate_ui_with_layout(
                vec2(64.0, 18.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(64.0);
                    ui.label(Text::small(
                        cx.tr.t("codex_settings.detail.default"),
                        cx.pal,
                    ));
                },
            );
            match &ed.default {
                Some(d) => {
                    let mut job = LayoutJob::single_section(
                        d.clone(),
                        egui::TextFormat {
                            font_id: mono_font(text::CAPTION, Weight::Regular),
                            color: cx.pal.text_muted,
                            ..Default::default()
                        },
                    );
                    job.wrap = TextWrapping {
                        max_width: ui.available_width(),
                        max_rows: 3,
                        break_anywhere: true,
                        overflow_character: Some('…'),
                    };
                    let r = ui.label(job);
                    crate::design::widgets::value_chip::value_tooltip(r, d, Shape::of(d));
                }
                None => {
                    ui.label(Text::small(
                        cx.tr.t("codex_settings.value.codex_decides"),
                        cx.pal,
                    ));
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if !ed.read_only {
                ui.add(
                    egui::Label::new(Text::caption(cx.tr.t("codex_settings.editor.note"), cx.pal))
                        .truncate(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !ed.read_only
                    && ui
                        .add(
                            Button::primary(cx.tr.t("codex_settings.editor.save"))
                                .small()
                                .kbd(format!("{} Enter", command_key())),
                        )
                        .clicked()
                {
                    save = true;
                }
                let close = if ed.read_only {
                    cx.tr.t("codex_settings.pending.close")
                } else {
                    cx.tr.t("action.cancel")
                };
                if ui.add(Button::ghost(close).small().kbd("Esc")).clicked() {
                    cancel = true;
                }
            });
        });
    });
    // Escape cancels; a click on the backdrop does not throw the text away.
    let escape = r.is_top_modal
        && !r.any_popup_open
        && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    if let Some(text) = copy {
        ctx.copy_text(text);
    }
    state.editor = Some(ed);
    if save {
        state.save_editor();
    } else if cancel || escape {
        state.cancel_editor();
    }
}

fn rows_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, model: &Model) {
    let Some(path) = state.rows_dialog.clone() else {
        return;
    };
    let Some(e) = model.entry(&path).cloned() else {
        state.rows_dialog = None;
        return;
    };
    let zh = cx.tr.locale() == superba_core::Locale::ZhCn;
    let read_only = model.config_error.is_some() || model.profile_id.is_none() || cx.busy;
    let enabled = !read_only && e.editable();
    let mut close = false;
    let r = dialog(ctx, Id::unique("codex-settings-rows"), 620.0, |ui| {
        ui.add(
            egui::Label::new(Text::h2(
                cx.tr.f(
                    "codex_settings.editor.title",
                    &[("name", &e.title_for(zh).text)],
                ),
                cx.pal,
            ))
            .truncate(),
        );
        ui.add(egui::Label::new(Text::mono_muted(&e.path, cx.pal)).truncate());
        let height = (ctx.content_rect().height() - 300.0).clamp(160.0, 420.0);
        egui::ScrollArea::vertical()
            .id_salt("codex-settings-rows-list")
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| match &e.kind {
                Kind::List { item } => list_rows(ui, cx, state, &e, item, enabled, None),
                Kind::Map { value } => map_rows_ui(ui, cx, state, &e, value, enabled, None),
                _ => {}
            });
        if let Some(key) = state.invalid.get(&e.path) {
            ui.label(Text::small(cx.tr.t(key), cx.pal).color(cx.pal.red));
        }
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::new(cx.tr.t("codex_settings.pending.close"))
                            .small()
                            .kbd("Esc"),
                    )
                    .clicked()
                {
                    close = true;
                }
            });
        });
    });
    if close || r.should_close() {
        state.rows_dialog = None;
    }
}
