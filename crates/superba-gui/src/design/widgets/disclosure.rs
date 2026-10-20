//! A disclosure: a header line with a chevron that shows or hides the content
//! under it. Two weights: an inline one (13 px medium, muted, for "details"
//! expanders inside a section) and [`Disclosure::section`], a section heading
//! (15 px semibold) with an optional muted caption on the same line, for the
//! collapsed 「高级」 / "Advanced" group of a settings page (ui-spec rule 6).
//!
//! Only the chevron and the texts take clicks, so a disclosure never steals a
//! click meant for the row it sits in. Screen readers and UI Automation see a
//! disclosure triangle named by its label, with its expanded state (the
//! Windows e2e expands 「高级」 through it); AccessKit's Click toggles it.

use egui::collapsing_header::CollapsingState;
use egui::{AsIdSalt, CursorIcon, IdSalt, Rect, Response, Sense, Ui, WidgetInfo, pos2, vec2};

use super::{focus_ring, hover_t, show_focus};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, size, space, text};

/// Side of the chevron: a notch under an icon, so it reads as a marker of the
/// label rather than an icon of its own.
const CHEVRON: f32 = size::ICON - 2.0;

/// A header that shows or hides its body.
#[must_use = "call .show(ui, ..)"]
pub struct Disclosure {
    id_salt: IdSalt,
    label: String,
    caption: Option<String>,
    section: bool,
    default_open: bool,
}

/// What [`Disclosure::show`] did.
#[derive(Debug)]
pub struct DisclosureResponse<R> {
    /// The header (clicks, hover, accessibility).
    pub header: Response,
    /// The body is shown (also while it animates open).
    pub open: bool,
    /// What the body returned, when it was drawn.
    pub inner: Option<R>,
}

impl Disclosure {
    /// An inline disclosure labelled `label`; `id_salt` keeps its state (open
    /// or closed) apart from other disclosures in the same `Ui`.
    pub fn new(id_salt: impl AsIdSalt, label: impl Into<String>) -> Self {
        Self {
            id_salt: IdSalt::new(id_salt),
            label: label.into(),
            caption: None,
            section: false,
            default_open: false,
        }
    }

    /// A section heading ([`text::SECTION`]) on a [`size::ROW`] line.
    pub fn section(mut self) -> Self {
        self.section = true;
        self
    }

    /// A muted one-line caption after the label (what the section holds);
    /// shortened with "…" when the line is too narrow, full text on hover.
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// Open the first time it is shown (default: closed).
    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    /// Height of the header line.
    pub fn header_height(&self) -> f32 {
        if self.section {
            size::ROW
        } else {
            size::CONTROL_SM
        }
    }

    /// Draw the header and, when open, `add` under it (not indented: the body
    /// lines up with the label's left edge's parent column).
    pub fn show<R>(self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> DisclosureResponse<R> {
        let pal = Palette::of(ui.ctx());
        let id = ui
            .make_persistent_id(self.id_salt)
            .with("superba-disclosure");
        let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, self.default_open);
        let height = self.header_height();
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let label_font = if self.section {
            font(text::SECTION, Weight::Semibold)
        } else {
            font(text::SMALL, Weight::Medium)
        };
        let label = ui
            .painter()
            .layout_no_wrap(self.label.clone(), label_font, pal.text);
        let label_x = line.min.x + CHEVRON + space::S;
        let mut hit = Rect::from_min_max(
            pos2(line.min.x, line.center().y - height / 2.0),
            pos2(label_x + label.size().x, line.center().y + height / 2.0),
        );
        let caption = self.caption.as_ref().map(|c| {
            let room = (line.max.x - hit.max.x - space::S).max(0.0);
            super::log_table::elided(
                ui,
                c,
                font(text::SMALL, Weight::Regular),
                pal.text_subtle,
                room,
            )
        });
        if let Some(c) = &caption {
            hit.max.x += space::S + c.size().x;
        }
        let header = ui
            .interact(hit, id.with("header"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);
        if header.clicked() {
            state.toggle(ui);
        }
        let open = state.is_open();
        let enabled = ui.is_enabled();
        let name = self.label.clone();
        header.widget_info(|| WidgetInfo::labeled(egui::Role::DisclosureTriangle, enabled, &name));
        ui.ctx()
            .accesskit_node_builder(header.id, |node| node.set_expanded(open));
        if ui.is_rect_visible(line) {
            let t = hover_t(ui, &header);
            let rest = if self.section {
                pal.text
            } else {
                pal.text_muted
            };
            let color = rest.lerp_to_gamma(pal.text, t);
            let chevron = Rect::from_center_size(
                pos2(line.min.x + CHEVRON / 2.0, line.center().y),
                vec2(CHEVRON, CHEVRON),
            );
            let icon = if open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            icons::paint(ui.painter(), chevron, icon, color);
            ui.painter().galley(
                pos2(label_x, line.center().y - label.size().y / 2.0),
                label,
                color,
            );
            if let Some(c) = caption {
                let elided = c.elided;
                let at = pos2(hit.max.x - c.size().x, line.center().y - c.size().y / 2.0);
                ui.painter().galley(at, c, pal.text_subtle);
                if elided && let Some(full) = &self.caption {
                    header.clone().on_hover_text(full);
                }
            }
            if show_focus(&header) {
                focus_ring(ui.painter(), hit, radius::INNER, pal);
            }
        }
        let inner = state.show_body_unindented(ui, add).map(|body| body.inner);
        DisclosureResponse {
            header,
            open: inner.is_some(),
            inner,
        }
    }
}
