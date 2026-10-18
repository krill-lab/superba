//! Modal dialogs: dimmed backdrop, centered card, 24 px padding. The first
//! text a dialog shows (its title) names it for screen readers (user bug 56).

use egui::{Frame, Id, Margin, Modal, ModalResponse, Shadow, Stroke, Ui};

use crate::design::tokens::{Palette, radius};

/// Show a modal dialog of `width` points. Escape / backdrop clicks are reported
/// through [`ModalResponse::should_close`]; callers decide what closing means.
pub fn dialog<R>(
    ctx: &egui::Context,
    id: Id,
    width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> ModalResponse<R> {
    let pal = Palette::of(ctx);
    let frame = Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(24))
        .shadow(Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: pal.shadow,
        });
    Modal::new(id)
        .backdrop_color(pal.backdrop)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 8.0;
            let (inner, title) = crate::design::a11y::first_text(ui, add);
            if let Some(title) = title {
                crate::design::a11y::name_dialog(ui, title);
            }
            inner
        })
}
