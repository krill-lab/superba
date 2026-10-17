//! Choice cards (UX §4): a radio group of option cards (indicator, title, body
//! and badge), used by the wizard's and Add provider's quick-pick, and
//! [`choice_card_ui`], the same card around custom content (the wizard's line
//! tiles with their latency, its key and session choices).
//!
//! Every card is its own rounded rectangle with a complete 1 px border, separated
//! from its neighbours by [`GAP`] (no shared edges, so nothing reads as a tab
//! merged into the content below). Unselected cards are transparent; the selected
//! card gets a subtle neutral fill, a stronger border and a check in its
//! indicator. Arrow keys move the selection inside the group (radio-group
//! keyboard model); Space / Enter select the focused card.

use egui::{
    Color32, CursorIcon, FocusDirection, Id, Key, Margin, Modifiers, Painter, Pos2, Rect, Response,
    Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::{Badge, focus_ring, hover_t, show_focus};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};

/// Corner radius of a card.
pub const RADIUS: u8 = radius::CARD - 2;
/// Vertical gap between the cards of a group.
pub const GAP: f32 = 8.0;
/// Radius of the selection indicator (ring or filled check).
const INDICATOR: f32 = 8.0;
/// Left inset of a card's text (right of the indicator).
const CONTENT_X: f32 = 40.0;
/// Right inset of [`choice_card_ui`]'s content.
const CONTENT_RIGHT: f32 = 14.0;

/// The selection indicator centred at `c`: an empty ring, or a filled disc with
/// a check when selected (`hover` 0..1 darkens the ring).
fn paint_indicator(p: &Painter, c: Pos2, pal: &Palette, selected: bool, enabled: bool, hover: f32) {
    if selected {
        let disc = if enabled { pal.text } else { pal.text_disabled };
        p.circle_filled(c, INDICATOR, disc);
        icons::paint(
            p,
            Rect::from_center_size(c, vec2(11.0, 11.0)),
            Icon::Check,
            pal.surface,
        );
    } else {
        let ring = if enabled {
            pal.border_strong.lerp_to_gamma(pal.text_subtle, hover)
        } else {
            pal.text_disabled
        };
        p.circle_stroke(c, INDICATOR - 0.75, Stroke::new(1.5, ring));
    }
}

/// A choice card around custom content: the same rounded rectangle, fill,
/// border and indicator as [`ChoiceCard`], with `add` laid out right of the
/// indicator (its first line level with it). The whole card is one radio button
/// labelled `label`; `clicked()` on the response selects it.
pub fn choice_card_ui(
    ui: &mut Ui,
    id: Id,
    label: &str,
    selected: bool,
    enabled: bool,
    add: impl FnOnce(&mut Ui),
) -> Response {
    let pal = Palette::of(ui.ctx());
    // The card's shape goes under its content; it is filled in once the
    // content's height and the hover state are known.
    let background = ui.painter().add(Shape::Noop);
    let width = ui.available_width();
    let inner = egui::Frame::new()
        .inner_margin(Margin {
            left: CONTENT_X as i8,
            right: CONTENT_RIGHT as i8,
            top: 12,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.set_width((width - CONTENT_X - CONTENT_RIGHT).max(0.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
    let rect = inner.response.rect;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let mut response = ui.interact(rect, id, sense);
    if enabled {
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, label)
    });
    let t = hover_t(ui, &response);
    let (fill, stroke) = card_style(pal, selected, enabled, t);
    ui.painter().set(
        background,
        egui::epaint::RectShape::new(rect, RADIUS, fill, stroke, StrokeKind::Inside),
    );
    paint_indicator(
        ui.painter(),
        pos2(rect.min.x + 22.0, rect.min.y + 22.0),
        pal,
        selected,
        enabled,
        t,
    );
    if show_focus(&response) {
        focus_ring(ui.painter(), rect, RADIUS, pal);
    }
    response
}

/// Fill and border of a card: transparent with a thin border when unselected
/// (a faint fill and a stronger border while hovered, `hover` 0..1), a neutral
/// gray fill and a stronger border when selected. Disabled cards do not hover.
pub fn card_style(pal: &Palette, selected: bool, enabled: bool, hover: f32) -> (Color32, Stroke) {
    let hover = if enabled { hover.clamp(0.0, 1.0) } else { 0.0 };
    if selected {
        (
            pal.hover.lerp_to_gamma(pal.active, hover),
            Stroke::new(1.0, pal.border_strong),
        )
    } else {
        (
            pal.hover.gamma_multiply(0.5 * hover),
            Stroke::new(1.0, pal.border.lerp_to_gamma(pal.border_strong, hover)),
        )
    }
}

/// One selectable card.
#[must_use = "add it with ui.add(..)"]
pub struct ChoiceCard {
    title: String,
    body: Option<String>,
    badge: Option<String>,
    selected: bool,
    enabled: bool,
}

impl ChoiceCard {
    /// A card titled `title`.
    pub fn new(title: impl Into<String>, selected: bool) -> Self {
        Self {
            title: title.into(),
            body: None,
            badge: None,
            selected,
            enabled: true,
        }
    }

    /// Secondary text below the title (one or two lines).
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// A small outline badge right of the title ("默认", "推荐").
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Disable the card.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl egui::Widget for ChoiceCard {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let width = ui.available_width();
        let inner_w = width - 52.0;
        let body = self.body.as_ref().filter(|b| !b.is_empty()).map(|b| {
            ui.painter().layout(
                b.clone(),
                font(text::SMALL, Weight::Regular),
                pal.text_muted,
                inner_w.max(40.0),
            )
        });
        let height = 22.0 + 24.0 + body.as_ref().map_or(0.0, |g| g.size().y + 4.0);
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), sense);
        if self.enabled {
            response = response.on_hover_cursor(CursorIcon::PointingHand);
        }
        let title = self.title.clone();
        let (enabled, selected) = (self.enabled, self.selected);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, &title)
        });
        let t = hover_t(ui, &response);
        let (fill, stroke) = card_style(pal, selected, enabled, t);
        let p = ui.painter();
        // One shape: fill and all four borders, never overlapped by a neighbour.
        p.rect(rect, RADIUS, fill, stroke, StrokeKind::Inside);
        // Indicator: an empty ring, or a filled disc with a check when selected.
        let c = pos2(rect.min.x + 22.0, rect.min.y + 22.0);
        paint_indicator(p, c, pal, selected, enabled, t);
        let text_color = if enabled { pal.text } else { pal.text_disabled };
        let title_galley = p.layout_no_wrap(
            self.title.clone(),
            font(text::BODY, Weight::Semibold),
            text_color,
        );
        let tx = rect.min.x + CONTENT_X;
        let title_w = title_galley.size().x;
        p.galley(
            pos2(tx, c.y - title_galley.size().y / 2.0),
            title_galley,
            text_color,
        );
        if let Some(g) = body {
            p.galley(pos2(tx, rect.min.y + 34.0), g, pal.text_muted);
        }
        if let Some(b) = self.badge {
            let br = Rect::from_min_size(
                pos2(tx + title_w + 8.0, c.y - 10.0),
                vec2((rect.max.x - 12.0 - (tx + title_w + 8.0)).max(0.0), 20.0),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(br)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.add(Badge::new(b).outline());
        }
        if show_focus(&response) {
            focus_ring(ui.painter(), rect, RADIUS, pal);
        }
        response
    }
}

/// One option of [`choice_cards`].
#[derive(Clone, Debug)]
pub struct Choice<T> {
    /// Value selected by the card.
    pub value: T,
    /// Title.
    pub title: String,
    /// Optional body.
    pub body: Option<String>,
    /// Optional badge.
    pub badge: Option<String>,
    /// Selectable.
    pub enabled: bool,
}

impl<T> Choice<T> {
    /// An enabled option without body or badge.
    pub fn new(value: T, title: impl Into<String>) -> Self {
        Self {
            value,
            title: title.into(),
            body: None,
            badge: None,
            enabled: true,
        }
    }
}

/// The enabled item `step` places (wrapping) from `from`, if any other is enabled.
fn step_enabled<T>(items: &[Choice<T>], from: usize, forward: bool) -> Option<usize> {
    let n = items.len();
    (1..n)
        .map(|k| {
            if forward {
                (from + k) % n
            } else {
                (from + n - k) % n
            }
        })
        .find(|&i| items[i].enabled)
}

/// A group of choice cards bound to `value`, [`GAP`] apart; true when it
/// changed. While a card has keyboard focus, the arrow keys select the previous
/// / next enabled card (wrapping) and move the focus with it.
pub fn choice_cards<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, items: &[Choice<T>]) -> bool {
    let mut changed = false;
    let mut ids = Vec::with_capacity(items.len());
    let mut focused = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = GAP;
        for (i, item) in items.iter().enumerate() {
            let mut card =
                ChoiceCard::new(item.title.clone(), *value == item.value).enabled(item.enabled);
            if let Some(b) = &item.body {
                card = card.body(b.clone());
            }
            if let Some(b) = &item.badge {
                card = card.badge(b.clone());
            }
            let r = ui.add(card);
            if r.clicked() && *value != item.value {
                *value = item.value.clone();
                changed = true;
            }
            if r.has_focus() {
                focused = Some(i);
            }
            ids.push(r.id);
        }
    });
    if let Some(i) = focused {
        let (back, forward) = ui.input_mut(|inp| {
            let mut key = |k| inp.consume_key(Modifiers::NONE, k);
            (
                key(Key::ArrowUp) | key(Key::ArrowLeft),
                key(Key::ArrowDown) | key(Key::ArrowRight),
            )
        });
        if back != forward
            && let Some(j) = step_enabled(items, i, forward)
        {
            if *value != items[j].value {
                *value = items[j].value.clone();
                changed = true;
            }
            ui.memory_mut(|m| {
                m.request_focus(ids[j]);
                // The arrow moved the selection; egui must not also move the focus.
                m.move_focus(FocusDirection::None);
            });
            ui.ctx().request_repaint();
        }
    }
    changed
}
