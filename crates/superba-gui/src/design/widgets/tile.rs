//! Small neutral identities for tools and status cards.
use crate::design::{
    a11y,
    fonts::{Weight, mono_font},
    icons::Icon,
    tokens::Palette,
};
use egui::{Response, Sense, Ui, Widget, vec2};
/// A rounded icon tile.
pub struct IconTile {
    icon: Icon,
    side: f32,
    warning: bool,
}
impl IconTile {
    /// A 30 point tile.
    pub fn new(icon: Icon) -> Self {
        Self {
            icon,
            side: 30.0,
            warning: false,
        }
    }
    /// Tile side.
    pub fn size(mut self, side: f32) -> Self {
        self.side = side;
        self
    }
    /// Amber attention tint.
    pub fn warning(mut self) -> Self {
        self.warning = true;
        self
    }
    /// Draw in a row that has already allocated its space.
    pub fn paint_at(&self, ui: &Ui, center: egui::Pos2) {
        let p = Palette::of(ui.ctx());
        super::list_row::icon_tile(
            ui,
            center,
            self.side,
            self.icon,
            self.warning.then(|| p.status(super::Status::Warn)),
        );
    }
}
impl Widget for IconTile {
    fn ui(self, ui: &mut Ui) -> Response {
        let (r, response) = ui.allocate_exact_size(vec2(self.side, self.side), Sense::hover());
        self.paint_at(ui, r.center());
        response
    }
}
/// A tool's identity: its logo (see [`crate::design::tool_icons`]) or,
/// without one, two letters.
pub struct MonogramTile {
    text: String,
    side: f32,
}
impl MonogramTile {
    /// Create a 30 point identity.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            side: 30.0,
        }
    }
    /// Tile side (usually 22, 30 or 40).
    pub fn size(mut self, side: f32) -> Self {
        self.side = side;
        self
    }
    /// Draw in a row that already exposes the full tool name to accessibility.
    pub fn paint_at(&self, ui: &Ui, center: egui::Pos2) {
        let p = Palette::of(ui.ctx());
        let r = egui::Rect::from_center_size(center, vec2(self.side, self.side));
        ui.painter().rect_filled(r, 6, p.hover);
        // The tool's own logo when one is licensed for redistribution.
        if crate::design::tool_icons::paint(ui, &self.text, r) {
            return;
        }
        ui.painter().text(
            center,
            egui::Align2::CENTER_CENTER,
            self.text.chars().take(2).collect::<String>(),
            mono_font(self.side * 0.4, Weight::Medium),
            p.text_muted,
        );
    }
}
impl Widget for MonogramTile {
    fn ui(self, ui: &mut Ui) -> Response {
        let (r, response) = ui.allocate_exact_size(vec2(self.side, self.side), Sense::hover());
        self.paint_at(ui, r.center());
        a11y::text(&response, &self.text);
        response
    }
}
