//! Text helpers with the Geist type scale (no egui `TextStyle`s needed in pages).

use egui::{Color32, CursorIcon, Label, Response, RichText, Sense, Ui, WidgetInfo, vec2};

use super::{focus_ring, show_focus};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};

/// Styled text constructors.
pub struct Text;

impl Text {
    /// Page title: 24 pt semibold, tight tracking.
    pub fn h1(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::H1, Weight::Semibold))
            .color(pal.text)
            .extra_letter_spacing(-0.5)
    }

    /// Page title in the compact header: 20 pt semibold, tight tracking.
    pub fn page_title(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::H2, Weight::Semibold))
            .color(pal.text)
            .extra_letter_spacing(-0.4)
    }

    /// Flat section heading: 15 pt semibold.
    pub fn section(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::SECTION, Weight::Semibold))
            .color(pal.text)
            .extra_letter_spacing(-0.1)
    }

    /// Section heading: 20 pt semibold.
    pub fn h2(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::H2, Weight::Semibold))
            .color(pal.text)
            .extra_letter_spacing(-0.3)
    }

    /// Card title: 16 pt semibold.
    pub fn title(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::TITLE, Weight::Semibold))
            .color(pal.text)
            .extra_letter_spacing(-0.2)
    }

    /// Body text: 14 pt regular, primary color.
    pub fn body(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::BODY, Weight::Regular))
            .color(pal.text)
    }

    /// Emphasized body text: 14 pt medium.
    pub fn strong(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::BODY, Weight::Medium))
            .color(pal.text)
    }

    /// Secondary text: 14 pt, muted color.
    pub fn muted(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::BODY, Weight::Regular))
            .color(pal.text_muted)
    }

    /// Small secondary text: 13 pt, muted.
    pub fn small(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::SMALL, Weight::Regular))
            .color(pal.text_muted)
    }

    /// Caption / label: 12 pt medium, subtle color.
    pub fn caption(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::CAPTION, Weight::Medium))
            .color(pal.text_subtle)
    }

    /// Monospace: 13 pt.
    pub fn mono(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(mono_font(text::SMALL, Weight::Regular))
            .color(pal.text)
    }

    /// Small monospace in the muted color.
    pub fn mono_muted(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(mono_font(text::CAPTION, Weight::Regular))
            .color(pal.text_muted)
    }

    /// Large number.
    pub fn display(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s)
            .font(font(text::DISPLAY, Weight::Semibold))
            .color(pal.text)
            .extra_letter_spacing(-0.8)
    }
}

/// A keyboard hint chip (`Enter`, `Esc`).
pub fn kbd(ui: &mut Ui, key: &str) -> Response {
    let pal = Palette::of(ui.ctx());
    let galley = ui.painter().layout_no_wrap(
        key.to_owned(),
        mono_font(text::CAPTION - 1.0, Weight::Medium),
        Color32::PLACEHOLDER,
    );
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 10.0, 18.0), Sense::hover());
    ui.painter().rect(
        rect,
        4,
        pal.hover,
        egui::Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(
        egui::pos2(rect.min.x + 5.0, rect.center().y - galley.size().y / 2.0),
        galley,
        pal.text_muted,
    );
    response
}

/// A link-styled text button with a trailing arrow (opens in the browser when the
/// caller handles the click).
pub fn link_button(ui: &mut Ui, label: &str, external: bool) -> Response {
    let pal = Palette::of(ui.ctx());
    let galley = ui.painter().layout(
        label.to_owned(),
        font(text::SMALL, Weight::Medium),
        pal.link,
        ui.available_width(),
    );
    let (pos, galley, response) = Label::new(galley).sense(Sense::click()).layout_in_ui(ui);
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let text_rect = galley.rect.translate(pos.to_vec2());
    let mut ring = text_rect;
    if external {
        let r = text_rect;
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(r.max.x + 3.0, r.center().y - 6.0),
            vec2(12.0, 12.0),
        );
        ui.allocate_exact_size(vec2(15.0, 12.0), Sense::hover());
        icons::paint(ui.painter(), icon_rect, Icon::External, pal.link);
        ring = ring.union(icon_rect);
    }
    if show_focus(&response) {
        focus_ring(ui.painter(), ring, radius::INNER, pal);
    }
    if response.hovered() {
        for row in &galley.rows {
            let r = row.rect_without_leading_space().translate(pos.to_vec2());
            ui.painter().hline(
                r.x_range(),
                r.max.y - 1.0,
                egui::Stroke::new(1.0, pal.link.gamma_multiply(0.6)),
            );
        }
    }
    ui.painter().galley(pos, galley, pal.link);
    let l = label.to_owned();
    response.widget_info(|| WidgetInfo::labeled(egui::Role::Link, true, &l));
    response
}
