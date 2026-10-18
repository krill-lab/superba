//! Collapsible detail pane under a log (UX §3.5, like Surge): a hairline, a
//! title line with a status on the right, a row of tabs, then the body of the
//! selected tab in the rest of the pane.
//!
//! The page sizes the pane to its content ([`CHROME`] + the body it needs),
//! limited by [`clamp_height`] so the log above keeps a minimum height; the
//! hairline on top is a [`split_handle`] the user drags to resize it (double
//! click: back to the content height). A body taller than the pane scrolls
//! inside it.
//!
//! Screen readers and UI Automation (user bug 56) see the title and the status
//! as texts, the tabs as selectable, invokable tabs named by their labels, and
//! the body as the selected tab's panel.

use egui::accesskit::Role;
use egui::{CursorIcon, Id, Rangef, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use super::Status;
use super::log_table::elided;
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, text};

/// Space above the title line (under the hairline).
const TOP: f32 = 4.0;
/// Height of the title line.
const TITLE_H: f32 = 24.0;
/// Height of the tab row.
const TABS_H: f32 = 26.0;
/// Space between the tab row and the body.
const BODY_GAP: f32 = 6.0;

/// Height of everything above the body: hairline, title line, tabs.
pub const CHROME: f32 = TOP + TITLE_H + TABS_H + BODY_GAP;

/// Smallest pane: the title, the tabs and 64 px of body (which then scrolls).
pub const MIN_HEIGHT: f32 = CHROME + 64.0;

/// How far above and below the pane's top edge the split handle reacts.
const HANDLE_BELOW: f32 = TOP;

/// Height of a pane that would like to be `desired` tall, sharing `room` points
/// with a log that needs at least `min_rest`: at least [`MIN_HEIGHT`], at most
/// what leaves the log its minimum, never more than `room`.
pub fn clamp_height(desired: f32, room: f32, min_rest: f32) -> f32 {
    let max = (room - min_rest).max(MIN_HEIGHT);
    desired.clamp(MIN_HEIGHT, max).min(room).max(0.0)
}

/// What the split handle did this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SplitResponse {
    /// Pointer movement while dragging (points; negative = up = a taller pane).
    pub delta: f32,
    /// Double clicked: back to the content height.
    pub reset: bool,
    /// Hovered or dragged.
    pub active: bool,
}

/// The draggable edge between a log and the pane under it (`pane` = the pane's
/// rectangle, `gap` = the space above it). Pointer only (like egui's panel
/// edges); it is not a keyboard focus stop.
pub fn split_handle(ui: &mut Ui, id: Id, pane: Rect, gap: f32) -> SplitResponse {
    let pal = Palette::of(ui.ctx());
    let rect = Rect::from_x_y_ranges(
        pane.x_range(),
        Rangef::new(pane.min.y - gap, pane.min.y + HANDLE_BELOW),
    );
    let resp = ui
        .interact(rect, id, Sense::CLICK | Sense::DRAG)
        .on_hover_cursor(CursorIcon::ResizeVertical);
    let active = resp.hovered() || resp.dragged();
    if active {
        ui.painter().hline(
            pane.x_range(),
            pane.min.y + 0.5,
            Stroke::new(if resp.dragged() { 2.0 } else { 1.0 }, pal.border_strong),
        );
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    SplitResponse {
        delta: if resp.dragged() {
            resp.drag_delta().y
        } else {
            0.0
        },
        reset: resp.double_clicked(),
        active,
    }
}

/// What happened in a [`DetailPane`] this frame.
#[derive(Debug, Default)]
pub struct DetailPaneResponse {
    /// The tab changed.
    pub tab_changed: bool,
}

/// The pane.
#[must_use = "call .show(ui, ..)"]
pub struct DetailPane<'a> {
    id: Id,
    tab: &'a mut usize,
    tabs: Vec<String>,
    title: String,
    status: Option<(Status, String)>,
}

impl<'a> DetailPane<'a> {
    /// A pane bound to the selected `tab` index.
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug, tab: &'a mut usize) -> Self {
        Self {
            id: Id::unique(("superba-detail-pane", id_salt)),
            tab,
            tabs: Vec::new(),
            title: String::new(),
            status: None,
        }
    }

    /// Tab labels.
    pub fn tabs(mut self, labels: Vec<String>) -> Self {
        self.tabs = labels;
        self
    }

    /// Title line.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Status on the right of the title line.
    pub fn status(mut self, status: Status, text: impl Into<String>) -> Self {
        self.status = Some((status, text.into()));
        self
    }

    /// Draw in the available rectangle; `body(ui, tab)` fills the tab's area.
    pub fn show(self, ui: &mut Ui, body: impl FnOnce(&mut Ui, usize)) -> DetailPaneResponse {
        let pal = Palette::of(ui.ctx());
        let rect = ui.available_rect_before_wrap();
        let mut out = DetailPaneResponse::default();
        ui.painter().hline(
            rect.x_range(),
            rect.min.y + 0.5,
            Stroke::new(1.0, pal.border),
        );
        // Title line.
        let title_r = Rect::from_min_size(
            pos2(rect.min.x, rect.min.y + TOP),
            vec2(rect.width(), TITLE_H),
        );
        let mut right = title_r.max.x - 4.0;
        if let Some((status, label)) = &self.status {
            let g = ui.painter().layout_no_wrap(
                label.clone(),
                font(text::CAPTION, Weight::Medium),
                pal.text_muted,
            );
            let x = right - g.size().x;
            let status_r =
                Rect::from_min_size(pos2(x, title_r.center().y - g.size().y / 2.0), g.size());
            crate::design::a11y::text(
                &ui.interact(status_r, self.id.with("status"), Sense::hover()),
                label,
            );
            ui.painter().galley(status_r.min, g, pal.text_muted);
            ui.painter()
                .circle_filled(pos2(x - 8.0, title_r.center().y), 3.5, pal.status(*status));
            right = x - 18.0;
        }
        let g = elided(
            ui,
            &self.title,
            font(text::SMALL, Weight::Medium),
            pal.text,
            right - title_r.min.x - 4.0,
        );
        let title_text_r = Rect::from_min_size(
            pos2(title_r.min.x + 4.0, title_r.center().y - g.size().y / 2.0),
            g.size(),
        );
        if !self.title.is_empty() {
            crate::design::a11y::text(
                &ui.interact(title_text_r, self.id.with("title"), Sense::hover()),
                &self.title,
            );
        }
        ui.painter().galley(title_text_r.min, g, pal.text);
        // Tabs.
        let tabs_r =
            Rect::from_min_size(pos2(rect.min.x, title_r.max.y), vec2(rect.width(), TABS_H));
        let mut x = tabs_r.min.x + 4.0;
        for (i, label) in self.tabs.iter().enumerate() {
            let selected = *self.tab == i;
            let weight = if selected {
                Weight::Medium
            } else {
                Weight::Regular
            };
            let g =
                ui.painter()
                    .layout_no_wrap(label.clone(), font(text::CAPTION, weight), pal.text);
            let r = Rect::from_min_size(pos2(x, tabs_r.min.y), vec2(g.size().x + 16.0, TABS_H));
            let resp = ui
                .interact(r, self.id.with(("tab", i)), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            let color = if selected || resp.hovered() {
                pal.text
            } else {
                pal.text_muted
            };
            ui.painter().galley(
                pos2(r.min.x + 8.0, r.center().y - g.size().y / 2.0),
                g,
                color,
            );
            if selected {
                ui.painter().hline(
                    (r.min.x + 6.0)..=(r.max.x - 6.0),
                    r.max.y - 1.0,
                    Stroke::new(2.0, pal.text),
                );
            }
            crate::design::a11y::selectable(&resp, Role::Tab, label, selected);
            if resp.clicked() && !selected {
                *self.tab = i;
                out.tab_changed = true;
            }
            x = r.max.x + 2.0;
        }
        ui.painter().hline(
            tabs_r.x_range(),
            tabs_r.max.y - 0.5,
            Stroke::new(1.0, pal.border),
        );
        // Body.
        let body_r = Rect::from_min_max(pos2(rect.min.x, tabs_r.max.y + BODY_GAP), rect.max);
        let mut builder = UiBuilder::new()
            .max_rect(body_r)
            .id_salt(self.id.with("body"))
            .accessibility_role(Role::TabPanel)
            .layout(egui::Layout::top_down(egui::Align::Min));
        if let Some(label) = self.tabs.get(*self.tab) {
            builder = builder.accessibility_label(label.clone());
        }
        let mut child = ui.new_child(builder);
        child.set_clip_rect(body_r.intersect(ui.clip_rect()));
        body(&mut child, *self.tab);
        ui.allocate_rect(rect, Sense::hover());
        out
    }
}
