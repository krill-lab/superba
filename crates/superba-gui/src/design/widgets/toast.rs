//! Toast stack in the bottom-right corner (plan 7.3: 4 s, actions, partial results).

use egui::{Align2, Area, Color32, Frame, Id, Margin, Order, Shadow, Stroke, Ui, pos2, vec2};

use super::{Button, Status};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{ANIM_SECS, Palette, TOAST_SECS, radius, text};

/// One toast. `A` is the caller's action type.
#[derive(Clone, Debug)]
pub struct Toast<A> {
    id: u64,
    /// Status dot.
    pub status: Status,
    /// Main line.
    pub title: String,
    /// Optional second line.
    pub body: Option<String>,
    /// Buttons (label, action).
    pub actions: Vec<(String, A)>,
    /// Lifetime in seconds; `None` = until dismissed.
    pub ttl: Option<f64>,
    born: Option<f64>,
    dying: Option<f64>,
}

impl<A> Toast<A> {
    /// A toast with the default 4 s lifetime.
    pub fn new(status: Status, title: impl Into<String>) -> Self {
        Self {
            id: 0,
            status,
            title: title.into(),
            body: None,
            actions: Vec::new(),
            ttl: Some(TOAST_SECS),
            born: None,
            dying: None,
        }
    }

    /// Second line.
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Add an action button.
    pub fn action(mut self, label: impl Into<String>, action: A) -> Self {
        self.actions.push((label.into(), action));
        self
    }

    /// Keep until dismissed (failures with actions).
    pub fn sticky(mut self) -> Self {
        self.ttl = None;
        self
    }
}

/// The stack.
#[derive(Debug)]
pub struct Toasts<A> {
    items: Vec<Toast<A>>,
    next_id: u64,
    bottom_inset: f32,
}

impl<A> Default for Toasts<A> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
            bottom_inset: 0.0,
        }
    }
}

impl<A> Toasts<A> {
    /// Keep the stack above a floating surface of this height at the bottom of the
    /// window (the switch overlay).
    pub fn set_bottom_inset(&mut self, inset: f32) {
        self.bottom_inset = inset.max(0.0);
    }
}

impl<A: Clone> Toasts<A> {
    /// Push a toast.
    pub fn push(&mut self, mut toast: Toast<A>) {
        toast.id = self.next_id;
        self.next_id += 1;
        self.items.push(toast);
        if self.items.len() > 4 {
            self.items.remove(0);
        }
    }

    /// Number of live toasts.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Titles of live toasts (tests, scenes).
    pub fn titles(&self) -> Vec<&str> {
        self.items.iter().map(|t| t.title.as_str()).collect()
    }

    /// Draw the stack; returns a clicked action.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<A> {
        if self.items.is_empty() {
            return None;
        }
        let pal = Palette::of(ctx);
        let now = ctx.input(|i| i.time);
        let mut clicked = None;
        let mut close = Vec::new();
        let width = 360.0;
        let mut bottom = ctx.content_rect().max.y - 16.0 - self.bottom_inset;
        let right = ctx.content_rect().max.x - 16.0;
        for toast in self.items.iter_mut().rev() {
            let born = *toast.born.get_or_insert(now);
            let appear = ((now - born) as f32 / ANIM_SECS).clamp(0.0, 1.0);
            let vanish = toast
                .dying
                .map(|d| 1.0 - ((now - d) as f32 / ANIM_SECS).clamp(0.0, 1.0))
                .unwrap_or(1.0);
            let alpha = appear.min(vanish);
            let id = Id::unique(("toast", toast.id));
            let area = Area::new(id)
                .order(Order::Foreground)
                .pivot(Align2::RIGHT_BOTTOM)
                .fixed_pos(pos2(right, bottom + 8.0 * (1.0 - appear)))
                .interactable(true);
            let resp = area.show(ctx, |ui| {
                ui.set_opacity(alpha);
                Frame::new()
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(radius::CARD)
                    .inner_margin(Margin::symmetric(16, 14))
                    .shadow(Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: pal.shadow,
                    })
                    .show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        toast_body(ui, toast, pal, &mut clicked, &mut close)
                    });
            });
            let hovered = resp.response.contains_pointer();
            if let Some(ttl) = toast.ttl
                && toast.dying.is_none()
            {
                if hovered {
                    // Hover pauses expiry: restart the clock.
                    toast.born = Some(now - (ANIM_SECS as f64));
                } else if now - born > ttl {
                    toast.dying = Some(now);
                }
            }
            if let Some(d) = toast.dying
                && now - d > ANIM_SECS as f64
            {
                close.push(toast.id);
            }
            bottom = resp.response.rect.min.y - 8.0;
        }
        self.items.retain(|t| !close.contains(&t.id));
        if !self.items.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            if self
                .items
                .iter()
                .any(|t| t.dying.is_some() || t.born.is_some_and(|b| now - b < ANIM_SECS as f64))
            {
                ctx.request_repaint();
            }
        }
        clicked
    }
}

fn toast_body<A: Clone>(
    ui: &mut Ui,
    toast: &mut Toast<A>,
    pal: &Palette,
    clicked: &mut Option<A>,
    close: &mut Vec<u64>,
) {
    ui.horizontal_top(|ui| {
        let (dot_rect, _) = ui.allocate_exact_size(vec2(10.0, 20.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 4.0, pal.status(toast.status));
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() - 28.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(
                egui::RichText::new(&toast.title)
                    .font(font(text::BODY, Weight::Medium))
                    .color(pal.text),
            );
            if let Some(body) = &toast.body {
                ui.label(
                    egui::RichText::new(body)
                        .font(font(text::SMALL, Weight::Regular))
                        .color(pal.text_muted),
                );
            }
            if !toast.actions.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    for (label, action) in &toast.actions {
                        if ui.add(Button::new(label.clone()).small()).clicked() {
                            *clicked = Some(action.clone());
                            toast.dying = Some(ui.input(|i| i.time));
                        }
                    }
                });
            }
        });
        let (x_rect, x_resp) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::click());
        let color = if x_resp.hovered() {
            pal.text
        } else {
            pal.text_subtle
        };
        icons::paint(ui.painter(), x_rect.shrink(4.0), Icon::Close, color);
        if x_resp.clicked() {
            close.push(toast.id);
        }
        let _ = Color32::TRANSPARENT;
    });
}
