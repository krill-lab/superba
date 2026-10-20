//! Modal dialogs: dimmed backdrop, centered card, XL padding. The first text a
//! dialog shows (its title) names it for screen readers (user bug 56).
//!
//! A dialog never grows past the window (UI spec rule 8, user bug 103): its
//! card is at most the window minus [`WINDOW_MARGIN`] on every side. Inside,
//! whatever the caller draws before [`dialog_body`] is the header, the body
//! scrolls, and whatever follows it is the footer. Header and footer stay put;
//! the body gets the height they leave. The header is measured as it is drawn;
//! the footer comes after the body, so its height is the one measured in the
//! previous pass (the pass is discarded and drawn again when it changed, so
//! no frame shows the wrong split).

use egui::{Frame, Id, Margin, Modal, ModalResponse, ScrollArea, Shadow, Stroke, Ui};

use crate::design::tokens::{Palette, radius, size, space};

/// Space kept between the card and each window edge.
pub const WINDOW_MARGIN: f32 = space::XL;
/// Padding inside the card.
const PADDING: i8 = space::XL as i8;
/// The body never gets less than this, even when the header and footer leave
/// less (the card then exceeds the window rather than hiding the content).
const MIN_BODY: f32 = 3.0 * size::ROW;
/// The footer height assumed before it was measured: a button row after a gap.
const FOOTER_GUESS: f32 = size::CONTROL + space::M + space::S;

/// Layout of the dialog being drawn, kept in egui's temporary data while its
/// contents run, so [`dialog_body`] finds it at any nesting depth.
#[derive(Clone, Copy, Debug)]
struct Slot {
    /// The dialog's id (keys the measured footer and the body's scroll state).
    id: Id,
    /// Lowest y the card's contents may reach.
    bottom: f32,
    /// Footer height used for this pass.
    footer: f32,
    /// Cursor y right after the body (where the footer starts), once drawn.
    body_end: Option<f32>,
    /// The body's content did not fit.
    overflow: bool,
}

fn slot_key() -> Id {
    Id::unique("superba-dialog-slot")
}

fn footer_key(id: Id) -> Id {
    id.with("superba-dialog-footer")
}

/// The height the card's contents may take in a window of `screen` height.
pub fn max_content_height(screen: f32) -> f32 {
    screen - 2.0 * WINDOW_MARGIN - 2.0 * f32::from(PADDING)
}

/// Show a modal dialog of `width` points (narrower when the window is). Escape /
/// backdrop clicks are reported through [`ModalResponse::should_close`]; callers
/// decide what closing means. Draw the title first, then the content inside one
/// [`dialog_body`], then the buttons.
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
        .inner_margin(Margin::same(PADDING))
        .shadow(Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: pal.shadow,
        });
    let screen = ctx.content_rect();
    let max_width = screen.width() - 2.0 * WINDOW_MARGIN - 2.0 * f32::from(PADDING);
    let max_height = max_content_height(screen.height());
    Modal::new(id)
        .backdrop_color(pal.backdrop)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width.min(max_width).max(0.0));
            ui.spacing_mut().item_spacing.y = space::S;
            bounded(ui, id, max_height, |ui| {
                let (inner, title) = crate::design::a11y::first_text(ui, add);
                if let Some(title) = title {
                    crate::design::a11y::name_dialog(ui, title);
                }
                inner
            })
        })
}

/// Lay out `add` as header, [`dialog_body`] and footer within `max_height`
/// points: the body scrolls and gets what the header and footer leave. The
/// dialog's own layout; the switch preview overlay uses it too. `id` keys the
/// footer measurement and the body's scroll state.
pub fn bounded<R>(ui: &mut Ui, id: Id, max_height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let footer = ui
        .data(|d| d.get_temp::<f32>(footer_key(id)))
        .unwrap_or(FOOTER_GUESS);
    let outer = ui.data_mut(|d| {
        let outer = d.get_temp::<Slot>(slot_key());
        d.insert_temp(
            slot_key(),
            Slot {
                id,
                bottom: ui.cursor().top() + max_height,
                footer,
                body_end: None,
                overflow: false,
            },
        );
        outer
    });
    let inner = add(ui);
    let slot = ui.data_mut(|d| {
        let slot = d.get_temp::<Slot>(slot_key());
        if let Some(o) = outer {
            d.insert_temp(slot_key(), o);
        } else {
            d.remove::<Slot>(slot_key());
        }
        slot
    });
    if let Some(slot) = slot
        && let Some(end) = slot.body_end
    {
        let measured = (ui.min_rect().bottom() - end).max(0.0);
        if (measured - footer).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(footer_key(id), measured));
            // Only a body that hit its limit was laid out with the wrong
            // height; a shorter one looks the same either way.
            if slot.overflow {
                ui.ctx().request_discard("dialog footer height changed");
            }
        }
    }
    inner
}

/// The scrolling part of a [`dialog`] (or of a [`bounded`] area), between its
/// title and its buttons. Draw it once per dialog. Outside one (and inside
/// another body) it draws `add` in place.
pub fn dialog_body<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let Some(mut slot) = ui.data(|d| d.get_temp::<Slot>(slot_key())) else {
        return add(ui);
    };
    let top = ui.cursor().top();
    let gap = ui.spacing().item_spacing.y;
    let max = (slot.bottom - top - gap - slot.footer).max(MIN_BODY);
    let pal = Palette::of(ui.ctx());
    // The modal's own area offers only last frame's size; the body may take
    // up to its limit now.
    let rect = egui::Rect::from_min_max(
        ui.cursor().min,
        egui::pos2(ui.max_rect().right(), top + max),
    );
    // Anything inside the body that asks for a body of its own draws in place.
    ui.data_mut(|d| d.remove::<Slot>(slot_key()));
    let out = ui
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            // A thin bar that stays visible while there is more to scroll.
            let scroll = &mut ui.spacing_mut().scroll;
            scroll.floating_allocated_width = space::S;
            scroll.dormant_handle_opacity = 0.4;
            ScrollArea::vertical()
                .id_salt(slot.id.with("dialog-body"))
                .max_height(max)
                .min_scrolled_height(MIN_BODY.min(max))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    add(ui)
                })
        })
        .inner;
    let overflow = out.content_size.y > out.inner_rect.height() + 0.5;
    if overflow {
        // A hairline where the body is cut, above the pinned buttons.
        let y = out.inner_rect.bottom() + gap / 2.0;
        ui.painter()
            .hline(ui.min_rect().x_range(), y, Stroke::new(1.0, pal.border));
    }
    slot.body_end = Some(ui.cursor().top());
    slot.overflow = overflow;
    ui.data_mut(|d| d.insert_temp(slot_key(), slot));
    out.inner
}
