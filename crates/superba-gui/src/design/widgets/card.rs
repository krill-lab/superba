//! Cards: 12 px radius, 1 px border, surface fill, optional footer strip.

use egui::{CornerRadius, Frame, InnerResponse, Margin, Stroke, Ui};

use crate::design::tokens::{Palette, radius};

/// Show `add` inside a plain card with 20 px padding.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    Card::new().show(ui, add)
}

/// Card builder.
#[must_use = "call .show(ui, ..)"]
pub struct Card {
    padding: i8,
    fill: Option<egui::Color32>,
    hover_border: bool,
    fill_height: bool,
    flat: bool,
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

impl Card {
    /// Default card.
    pub fn new() -> Self {
        Self {
            padding: 20,
            fill: None,
            hover_border: false,
            fill_height: false,
            flat: false,
        }
    }

    /// Inner padding.
    pub fn padding(mut self, padding: i8) -> Self {
        self.padding = padding;
        self
    }

    /// Override the fill.
    pub fn fill(mut self, fill: egui::Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    /// Emphasize the border (selected card).
    pub fn selected(mut self, selected: bool) -> Self {
        self.hover_border = selected;
        self
    }

    /// No frame, fill or padding: the content sits in the page's flat section and
    /// a footer is separated by a hairline (v2 layout, fewer nested cards).
    pub fn flat(mut self) -> Self {
        self.flat = true;
        self.padding = 0;
        self
    }

    /// Stretch to the height offered by the parent (equal-height grids).
    pub fn fill_height(mut self) -> Self {
        self.fill_height = true;
        self
    }

    /// The frame this card paints.
    pub fn frame(&self, pal: &Palette) -> Frame {
        if self.flat {
            return Frame::new().inner_margin(Margin::ZERO);
        }
        Frame::new()
            .fill(self.fill.unwrap_or(pal.surface))
            .stroke(Stroke::new(
                1.0,
                if self.hover_border {
                    pal.text_subtle
                } else {
                    pal.border
                },
            ))
            .corner_radius(radius::CARD)
            .inner_margin(Margin::same(self.padding))
    }

    /// Show the card filling the available width.
    pub fn show<R>(self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
        let pal = Palette::of(ui.ctx());
        let cell = ui.scope_id();
        let target = self.fill_height.then(|| grid_target(ui, cell)).flatten();
        let chrome = f32::from(self.padding) * 2.0 + 2.0;
        let resp = self.frame(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            // The minimum height must be set before any content: egui measures it
            // from the cursor.
            if let Some(t) = target {
                ui.set_min_height((t - chrome).max(0.0));
            }
            let body = ui.vertical(|ui| add(ui));
            (body.inner, body.response.rect.height())
        });
        if self.fill_height {
            report_natural(ui, cell, resp.inner.1 + chrome);
        }
        InnerResponse::new(resp.inner.0, resp.response)
    }

    /// Show a card with a body and a footer strip (subtle fill, top divider).
    pub fn show_with_footer<R>(
        self,
        ui: &mut Ui,
        body: impl FnOnce(&mut Ui) -> R,
        footer: impl FnOnce(&mut Ui),
    ) -> InnerResponse<R> {
        let pal = Palette::of(ui.ctx());
        if self.flat {
            let r = ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                let inner = body(ui);
                ui.add_space(12.0);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                ui.painter().hline(
                    rect.x_range(),
                    rect.center().y,
                    Stroke::new(1.0, pal.border),
                );
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| footer(ui));
                inner
            });
            return InnerResponse::new(r.inner, r.response);
        }
        let padding = self.padding;
        let cell = ui.scope_id();
        let target = self.fill_height.then(|| grid_target(ui, cell)).flatten();
        // Footer height from the previous frame (12 px padding twice + a 28 px
        // control on the first frame).
        let footer_h: f32 = ui
            .ctx()
            .data(|d| d.get_temp(cell.with("footer_h")))
            .unwrap_or(52.0);
        let body_chrome = f32::from(padding) * 2.0;
        let mut frame = self.frame(pal);
        frame.inner_margin = Margin::ZERO;
        let resp = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let (r, body_natural) = Frame::new()
                .inner_margin(Margin::same(padding))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 8.0;
                    if let Some(t) = target {
                        ui.set_min_height((t - 2.0 - footer_h - body_chrome).max(0.0));
                    }
                    let b = ui.vertical(|ui| body(ui));
                    (b.inner, b.response.rect.height())
                })
                .inner;
            let top = ui.cursor().min.y;
            let inner = radius::CARD - 1;
            let footer_rect = Frame::new()
                .fill(pal.bg_subtle)
                .corner_radius(CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: inner,
                    se: inner,
                })
                .inner_margin(Margin::symmetric(padding, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(28.0);
                    footer(ui);
                })
                .response
                .rect
                .height();
            ui.ctx()
                .data_mut(|d| d.insert_temp(cell.with("footer_h"), footer_rect));
            ui.painter()
                .hline(ui.min_rect().x_range(), top, Stroke::new(1.0, pal.border));
            (r, body_natural + body_chrome + footer_rect + 2.0)
        });
        if self.fill_height {
            report_natural(ui, cell, resp.inner.1);
        }
        InnerResponse::new(resp.inner.0, resp.response)
    }
}

/// Height the enclosing grid cell asks cards to fill (see `pages::grid_row`).
pub fn grid_target(ui: &Ui, cell: egui::Id) -> Option<f32> {
    ui.ctx().data(|d| d.get_temp(cell.with("grid_target")))
}

/// Report a card's natural (unstretched) height to the enclosing grid cell.
pub fn report_natural(ui: &Ui, cell: egui::Id, height: f32) {
    ui.ctx()
        .data_mut(|d| d.insert_temp(cell.with("grid_natural"), height));
}
