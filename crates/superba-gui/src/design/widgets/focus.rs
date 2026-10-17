//! Keyboard focus rings: when they show and where they are painted.
//!
//! - **When** (like CSS `:focus-visible`): rings show only while the keyboard is
//!   in use. Tab / Shift+Tab, the arrow keys, Home / End / Page Up / Page Down and
//!   an AccessKit focus request turn them on; a pointer press or a touch turns them
//!   off. A tab, segment or row clicked with the mouse keeps keyboard focus (its
//!   arrow keys work) without a blue box, and the first arrow key shows the ring.
//!   Focused text fields are not affected: they always show their ring or cursor.
//! - **Where**: outside the widget with a 2 px gap when that whole ring fits the
//!   clip rectangle the widget is drawn in, otherwise just inside the widget's
//!   edge, so a pane, scroll area or dialog edge never cuts a ring. Rings are
//!   painted at the end of the pass, above everything else on the widget's layer,
//!   so a neighbour drawn later cannot cover them either.
//! - **Every focusable widget**: a focused widget that draws no ring of its own
//!   (list rows, rail items, underline tabs, links, stock combo boxes) gets one
//!   from the plugin ([`fallback_ring`]). A widget that draws its own ring
//!   ([`show_focus`]) is scrolled into view when Tab moves focus to it.
//!
//! [`install`] registers the plugin that does this (called by
//! [`crate::design::style::install`]). Without it rings are painted at once and
//! [`show_focus`] follows egui's focus alone.

use egui::{
    Context, CornerRadius, Event, Id, Key, LayerId, Painter, Rect, Response, Stroke, StrokeKind,
    TouchPhase, Ui, accesskit, plugin::Plugin,
};

use crate::design::tokens::{Palette, focus, radius};

/// Keys that move keyboard focus or a keyboard selection.
const NAV_KEYS: [Key; 9] = [
    Key::Tab,
    Key::ArrowUp,
    Key::ArrowDown,
    Key::ArrowLeft,
    Key::ArrowRight,
    Key::Home,
    Key::End,
    Key::PageUp,
    Key::PageDown,
];

/// Tolerance for rectangles that meet a clip edge exactly: egui rounds layout
/// positions to [`egui::emath::GUI_ROUNDING`] (1/32 pt), so an edge that should
/// coincide with a clip edge may land up to that far outside it.
const EPS: f32 = egui::emath::GUI_ROUNDING;

fn visible_id() -> Id {
    Id::unique("superba-focus-visible")
}

fn painted_id() -> Id {
    Id::unique("superba-focus-painted")
}

fn queue_id() -> Id {
    Id::unique("superba-focus-rings")
}

/// Where the ring of a widget goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RingPlacement {
    /// Outside the widget with the 2 px gap; the centre line of the stroke.
    Outside(Rect),
    /// Just inside the widget's edge; the centre line of the stroke.
    Inside(Rect),
}

impl RingPlacement {
    /// Centre line of the ring stroke.
    pub fn rect(self) -> Rect {
        match self {
            Self::Outside(r) | Self::Inside(r) => r,
        }
    }

    /// Everything the stroke covers.
    pub fn painted(self) -> Rect {
        self.rect().expand(focus::WIDTH / 2.0)
    }
}

/// The ring of a widget at `rect` drawn inside `clip`: outside when the whole
/// outside ring fits `clip`, otherwise inside the widget.
pub fn ring_placement(rect: Rect, clip: Rect) -> RingPlacement {
    let outside = rect.expand(focus::GAP + focus::WIDTH / 2.0);
    if clip
        .expand(EPS)
        .contains_rect(outside.expand(focus::WIDTH / 2.0))
    {
        RingPlacement::Outside(outside)
    } else {
        RingPlacement::Inside(rect.shrink(focus::WIDTH / 2.0))
    }
}

/// Register the focus-ring plugin (idempotent).
pub fn install(ctx: &Context) {
    ctx.add_plugin(FocusRings);
}

fn installed(ctx: &Context) -> bool {
    ctx.plugin_opt::<FocusRings>().is_some()
}

/// True while focus rings show: the keyboard was used after the last pointer
/// press. Always true without the plugin.
pub fn focus_visible(ctx: &Context) -> bool {
    !installed(ctx) || ctx.data(|d| d.get_temp(visible_id())).unwrap_or(false)
}

/// True when the widget has keyboard focus and rings show; the widget then
/// paints its own ring (and the plugin paints none for it). A widget that just
/// got keyboard focus is scrolled into view (Tab may land below the fold).
pub fn show_focus(response: &Response) -> bool {
    if !response.has_focus() || !focus_visible(&response.ctx) {
        return false;
    }
    response
        .ctx
        .data_mut(|d| d.insert_temp(painted_id(), Some(response.id)));
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    true
}

/// Paint the focus ring of a widget at `rect` with corner `radius`: 2 px wide
/// with a 2 px gap outside the widget, or just inside its edge where the clip
/// rectangle has no room for that.
pub fn focus_ring(painter: &Painter, rect: Rect, radius: impl Into<CornerRadius>, pal: &Palette) {
    let radius: CornerRadius = radius.into();
    match ring_placement(rect, painter.clip_rect()) {
        RingPlacement::Outside(ring) => {
            let grow = (focus::GAP + focus::WIDTH / 2.0) as u8;
            let r = CornerRadius {
                nw: radius.nw.saturating_add(grow),
                ne: radius.ne.saturating_add(grow),
                sw: radius.sw.saturating_add(grow),
                se: radius.se.saturating_add(grow),
            };
            paint(painter, ring, r, pal);
        }
        RingPlacement::Inside(ring) => paint(painter, ring, radius, pal),
    }
}

/// Paint the focus ring just inside `rect` (rows and items whose neighbours
/// touch them).
pub fn focus_ring_inside(
    painter: &Painter,
    rect: Rect,
    radius: impl Into<CornerRadius>,
    pal: &Palette,
) {
    paint(painter, rect.shrink(focus::WIDTH / 2.0), radius.into(), pal);
}

/// A ring waiting for the end of the pass.
#[derive(Clone, Copy, Debug)]
struct QueuedRing {
    layer: LayerId,
    clip: Rect,
    rect: Rect,
    radius: CornerRadius,
    stroke: Stroke,
}

impl QueuedRing {
    fn paint(&self, painter: &Painter) {
        painter.with_clip_rect(self.clip).rect_stroke(
            self.rect,
            self.radius,
            self.stroke,
            StrokeKind::Middle,
        );
    }
}

#[derive(Clone, Debug, Default)]
struct RingQueue(Vec<QueuedRing>);

fn paint(painter: &Painter, ring: Rect, radius: CornerRadius, pal: &Palette) {
    if !painter.is_visible() {
        return;
    }
    let queued = QueuedRing {
        layer: painter.layer_id(),
        clip: painter.clip_rect(),
        rect: ring,
        radius,
        stroke: Stroke::new(focus::WIDTH, pal.focus),
    };
    let ctx = painter.ctx();
    if installed(ctx) {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_default::<RingQueue>(queue_id())
                .0
                .push(queued);
        });
    } else {
        queued.paint(painter);
    }
}

/// Whether `event` says the keyboard (`Some(true)`) or a pointer
/// (`Some(false)`) is in use.
fn modality(event: &Event) -> Option<bool> {
    match event {
        Event::Key {
            key, pressed: true, ..
        } if NAV_KEYS.contains(key) => Some(true),
        Event::AccessKitActionRequest(request) if request.action == accesskit::Action::Focus => {
            Some(true)
        }
        Event::PointerButton { pressed: true, .. }
        | Event::Touch {
            phase: TouchPhase::Start,
            ..
        } => Some(false),
        _ => None,
    }
}

/// Widgets smaller than this (links, clickable labels) get the fallback ring
/// outside, rows and larger items inside.
const SMALL: f32 = 24.0;

/// The ring the plugin paints for a focused widget that draws none of its own:
/// `rect` is the widget, `visible` the part inside its clip rectangle. Small,
/// fully visible widgets get it outside; anything else just inside the visible
/// part. `None` when nothing of the widget is visible.
pub fn fallback_ring(rect: Rect, visible: Rect) -> Option<RingPlacement> {
    if !visible.is_positive() || !visible.is_finite() {
        return None;
    }
    let whole = visible.expand(EPS).contains_rect(rect);
    if whole && rect.width().min(rect.height()) < SMALL {
        Some(RingPlacement::Outside(
            rect.expand(focus::GAP + focus::WIDTH / 2.0),
        ))
    } else {
        Some(RingPlacement::Inside(visible.shrink(focus::WIDTH / 2.0)))
    }
}

/// The plugin: tracks the input modality and paints the rings of the pass.
struct FocusRings;

impl Plugin for FocusRings {
    fn debug_name(&self) -> &'static str {
        "superba-focus-rings"
    }

    fn input_hook(&mut self, ctx: &Context, input: &mut egui::RawInput) {
        if let Some(visible) = input.events.iter().rev().find_map(modality) {
            ctx.data_mut(|d| d.insert_temp(visible_id(), visible));
        }
    }

    fn on_end_pass(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let (queue, painted) = ctx.data_mut(|d| {
            (
                d.remove_temp::<RingQueue>(queue_id()).unwrap_or_default(),
                d.remove_temp::<Option<Id>>(painted_id()).flatten(),
            )
        });
        for ring in &queue.0 {
            ring.paint(&ctx.layer_painter(ring.layer));
        }
        if !focus_visible(&ctx) {
            return;
        }
        // A focused widget without a ring of its own (text fields show a cursor).
        let Some(id) = ctx.memory(|m| m.focused()) else {
            return;
        };
        if painted == Some(id) || egui::TextEdit::load_state(&ctx, id).is_some() {
            return;
        }
        let Some(response) = ctx.read_response(id) else {
            return;
        };
        if !response.enabled() {
            return;
        }
        let Some(ring) = fallback_ring(response.rect, response.interact_rect) else {
            return;
        };
        let (clip, radius) = match ring {
            RingPlacement::Outside(_) => (
                ring.painted(),
                CornerRadius::same(radius::INNER + (focus::GAP + focus::WIDTH / 2.0) as u8),
            ),
            RingPlacement::Inside(_) => {
                (response.interact_rect, CornerRadius::same(radius::CONTROL))
            }
        };
        let pal = Palette::of(&ctx);
        QueuedRing {
            layer: response.layer_id,
            clip,
            rect: ring.rect(),
            radius,
            stroke: Stroke::new(focus::WIDTH, pal.focus),
        }
        .paint(&ctx.layer_painter(response.layer_id));
    }
}
