//! Dropdown (a select): a field of fixed height showing the current value and a
//! chevron. A click, or Space / Enter while it has keyboard focus, opens the
//! options in a menu below it. Every page uses this instead of egui's `ComboBox`.
//!
//! The stock `ComboBox` was as tall as its font's row height plus
//! `button_padding` (about 30 px for 14 px Geist), so in the 28 px rows it sits in
//! (the Router & usage filter bar) the row's clip rectangle cut off its bottom
//! border. This field has a fixed height instead, [`size::CONTROL_SM`] (28 px,
//! the default) or [`size::CONTROL`] (32 px, [`Dropdown::medium`]), so any row
//! that holds a small button holds a dropdown. Its border is drawn inside the
//! rectangle it allocates, so it never needs room outside that rectangle. The
//! keyboard focus ring follows the shared policy ([`super::focus_ring`]). The menu
//! opens [`MENU_GAP`] below the field, so it covers neither the field's bottom
//! border nor its focus ring. In the menu the selected option has the gray
//! selected-row fill of lists and tables ([`Palette::active`]), not egui's blue
//! text-selection colour (review ux-visual #16).

use egui::{
    AsIdSalt, CursorIcon, FontSelection, Id, IdSalt, InnerResponse, Popup, PopupCloseBehavior,
    Rect, Response, ScrollArea, Sense, Stroke, StrokeKind, TextWrapMode, Ui, WidgetInfo,
    WidgetText, pos2, vec2,
};

use super::{focus_ring, hover_t, show_focus};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, size, text};

/// Space between the field and its open menu.
pub const MENU_GAP: f32 = 4.0;

/// Left padding of the value; the chevron gets the same padding on the right.
const PAD: f32 = 10.0;
/// Space between the value and the chevron.
const GAP: f32 = 6.0;
/// Chevron size.
const CHEVRON: f32 = 14.0;

/// A dropdown field. Build it, then call [`Dropdown::show_ui`] with the menu
/// contents (like `ComboBox::show_ui`) or [`Dropdown::show_options`] with a list
/// of values.
#[must_use = "call .show_ui(..) or .show_options(..)"]
pub struct Dropdown {
    id_salt: IdSalt,
    selected: WidgetText,
    width: f32,
    height: f32,
    menu_height: Option<f32>,
    label: Option<String>,
}

impl Dropdown {
    /// A small (28 px) dropdown; `id_salt` must be unique within the parent `Ui`.
    pub fn new(id_salt: impl AsIdSalt) -> Self {
        Self {
            id_salt: IdSalt::new(id_salt),
            selected: WidgetText::default(),
            width: 0.0,
            height: size::CONTROL_SM,
            menu_height: None,
            label: None,
        }
    }

    /// The accessible name (what the dropdown chooses, e.g. "Provider"): for a
    /// dropdown without a visible label next to it. Inside a
    /// [`super::FormRow`] the row's label names it.
    pub fn label(mut self, name: impl Into<String>) -> Self {
        self.label = Some(name.into());
        self
    }

    /// The text shown in the field (a `RichText` keeps its font and colour).
    pub fn selected_text(mut self, text: impl Into<WidgetText>) -> Self {
        self.selected = text.into();
        self
    }

    /// Minimum width of the field. A longer value widens it up to the available
    /// width and is then shortened with an ellipsis.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Medium height (32 px, 14 px text), for forms of medium controls.
    pub fn medium(mut self) -> Self {
        self.height = size::CONTROL;
        self
    }

    /// Maximum height of the open menu (its list scrolls beyond it); default
    /// `Spacing::combo_height`.
    pub fn menu_height(mut self, height: f32) -> Self {
        self.menu_height = Some(height);
        self
    }

    /// Height of the field: every row that holds it must be at least this tall.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// The id of the open menu of the dropdown that `ui` gives `id_salt`.
    pub fn menu_id(ui: &Ui, id_salt: impl AsIdSalt) -> Id {
        ui.make_persistent_id(IdSalt::new(id_salt)).with("popup")
    }

    /// Show the field and, while it is open, the menu filled by `add_contents`.
    /// The inner value is `Some` while the menu is open.
    pub fn show_ui<R>(
        self,
        ui: &mut Ui,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        let pal = Palette::of(ui.ctx());
        let id = ui.make_persistent_id(self.id_salt);
        let popup_id = id.with("popup");
        let font_size = if self.height < size::CONTROL {
            text::SMALL
        } else {
            text::BODY
        };
        let chrome = PAD * 2.0 + GAP + CHEVRON;
        let max_text = (ui.available_width().max(self.width) - chrome).max(0.0);
        let accessible = self.selected.text().to_owned();
        let galley = self.selected.into_galley(
            ui,
            Some(TextWrapMode::Truncate),
            max_text,
            FontSelection::FontId(font(font_size, Weight::Regular)),
        );
        let has_text = !galley.is_empty();
        let natural = if has_text {
            chrome + galley.size().x
        } else {
            self.height
        };
        let size = vec2(natural.max(self.width).max(self.height), self.height);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let enabled = ui.is_enabled();
        let response = if enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        };
        let name = self.label.clone();
        response.widget_info(|| {
            let mut info = WidgetInfo::new(egui::Role::ComboBox);
            info.enabled = enabled;
            info.label = name.clone();
            info.current_text_value = Some(accessible.clone());
            info
        });

        let menu_height = self
            .menu_height
            .unwrap_or_else(|| ui.spacing().combo_height);
        let inner = Popup::menu(&response)
            .id(popup_id)
            .gap(MENU_GAP)
            .width(rect.width())
            .close_behavior(PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_min_width(ui.available_width());
                // Selected rows (`selectable_label`) as in lists: gray fill,
                // normal text colour.
                let v = ui.visuals_mut();
                v.selection.bg_fill = pal.active;
                v.selection.stroke = Stroke::new(1.0, pal.text);
                ScrollArea::vertical()
                    .max_height(menu_height)
                    .show(ui, |ui| {
                        // Options keep their natural width (a narrow field would
                        // otherwise wrap them early).
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
                        add_contents(ui)
                    })
                    .inner
            })
            .map(|r| r.inner);
        if inner.is_some() {
            ui.ctx().accesskit_node_builder(popup_id, |node| {
                node.set_role(egui::accesskit::Role::ListBox);
                if let Some(name) = &self.label {
                    node.set_label(name.clone());
                }
            });
        }

        if ui.is_rect_visible(rect) {
            let open = Popup::is_id_open(ui.ctx(), popup_id);
            let t = if open { 1.0 } else { hover_t(ui, &response) };
            let fill = pal.surface.lerp_to_gamma(pal.hover, t);
            let border = pal.border.lerp_to_gamma(pal.border_strong, t);
            let painter = ui.painter();
            painter.rect(
                rect,
                radius::CONTROL,
                fill,
                Stroke::new(1.0, border),
                StrokeKind::Inside,
            );
            let chevron = if has_text {
                Rect::from_min_size(
                    pos2(rect.max.x - PAD - CHEVRON, rect.center().y - CHEVRON / 2.0),
                    vec2(CHEVRON, CHEVRON),
                )
            } else {
                Rect::from_center_size(rect.center(), vec2(CHEVRON, CHEVRON))
            };
            icons::paint(
                painter,
                chevron,
                Icon::ChevronDown,
                pal.text_muted.lerp_to_gamma(pal.text, t),
            );
            if has_text {
                let pos = pos2(rect.min.x + PAD, rect.center().y - galley.size().y / 2.0);
                let room = Rect::from_min_max(
                    pos2(rect.min.x + PAD, rect.min.y + 1.0),
                    pos2(chevron.min.x - GAP / 2.0, rect.max.y - 1.0),
                );
                painter
                    .with_clip_rect(room.intersect(painter.clip_rect()))
                    .galley(pos, galley, pal.text);
            }
            if show_focus(&response) {
                focus_ring(painter, rect, radius::CONTROL, pal);
            }
        }
        InnerResponse { inner, response }
    }

    /// Show the field over `options` (value, label): the field shows the label of
    /// `value`, a click on an option sets `value`. The response is `changed()`
    /// when the value changed.
    pub fn show_options<T: PartialEq + Clone>(
        self,
        ui: &mut Ui,
        value: &mut T,
        options: &[(T, String)],
    ) -> Response {
        let label = options
            .iter()
            .find(|(v, _)| v == value)
            .map(|(_, l)| l.clone())
            .unwrap_or_default();
        let mut changed = false;
        let mut response = self.selected_text(label).show_ui(ui, |ui| {
            for (v, label) in options {
                if ui.selectable_label(v == value, label).clicked() && v != value {
                    *value = v.clone();
                    changed = true;
                }
            }
        });
        if changed {
            response.response.mark_changed();
        }
        response.response
    }
}
