//! Segmented control (Geist "Switch"): a track with an animated selected segment.
//! The track is one keyboard focus stop (a radio group); left / right arrow keys
//! move the selection while it has focus, and egui does not use them to move
//! focus away. A click selects and focuses the track without showing the focus
//! ring ([`super::focus`]).
//!
//! Only the track has a border. Segments share their edges ([`SegmentLayout`]),
//! with one 1 px divider between two neighbours and none next to the selected
//! segment (its indicator has its own border). Optional: a status dot before a
//! label, a spinner instead of it while what the option shows is loading (the
//! option is then busy for screen readers; user bug 83), a longest segment
//! (longer labels are elided and shown in full on hover) and a trailing
//! overflow segment ("More") that opens a menu of further options.

use egui::{
    Color32, CursorIcon, EventFilter, Id, Key, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, pos2, vec2,
};

use super::dropdown::MENU_GAP;
use super::log_table::elided;
use super::{Status, focus_ring, show_focus};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{ANIM_SECS, Palette, radius, size, text};

/// Text of the selected segment (on [`selected_fill`]).
pub const SELECTED_TEXT: Color32 = Color32::WHITE;

/// Fill of the selected segment: a darker gray that keeps white text at 4.5:1.
pub fn selected_fill(pal: &Palette) -> Color32 {
    if pal.dark {
        crate::design::tokens::hex(0x5D5E64)
    } else {
        crate::design::tokens::hex(0x75757B)
    }
}

/// One option of a [`Segmented`] control.
struct Segment<T> {
    value: T,
    label: String,
    enabled: bool,
    tooltip: Option<String>,
    dot: Option<Status>,
    busy: bool,
    icon: Option<Icon>,
}

/// The trailing overflow segment of a [`Segmented`] control and its menu.
struct Overflow<T> {
    label: String,
    items: Vec<(T, String)>,
}

/// Width of a status dot and the space after it.
const DOT_W: f32 = 12.0;
/// Width of a busy option's spinner and the space after it.
const SPIN_W: f32 = 16.0;
/// Size of the overflow segment's chevron (as in a dropdown) and the space
/// before it.
const CHEVRON: f32 = 14.0;
const CHEVRON_GAP: f32 = 4.0;
/// Longest label of an overflow menu entry.
const MENU_TEXT_MAX: f32 = 280.0;

/// A segmented control over values of `T`.
#[must_use = "call .show(ui)"]
pub struct Segmented<'a, T: PartialEq + Clone> {
    id: Id,
    value: &'a mut T,
    segments: Vec<Segment<T>>,
    full_width: bool,
    height: f32,
    min_segment: f32,
    max_segment: f32,
    overflow: Option<Overflow<T>>,
    label: Option<String>,
}

impl<'a, T: PartialEq + Clone> Segmented<'a, T> {
    /// New control bound to `value`.
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug, value: &'a mut T) -> Self {
        Self {
            id: Id::unique(id_salt),
            value,
            segments: Vec::new(),
            full_width: false,
            height: size::CONTROL,
            min_segment: 0.0,
            max_segment: f32::INFINITY,
            overflow: None,
            label: None,
        }
    }

    /// The accessible name of the group (what it chooses, e.g. "Time range"):
    /// for a control without a visible label next to it. Inside a
    /// [`super::FormRow`] the row's label names it.
    pub fn label(mut self, name: impl Into<String>) -> Self {
        self.label = Some(name.into());
        self
    }

    /// Add an option.
    pub fn option(mut self, value: T, label: impl Into<String>) -> Self {
        self.segments.push(Segment {
            value,
            label: label.into(),
            enabled: true,
            tooltip: None,
            dot: None,
            busy: false,
            icon: None,
        });
        self
    }

    /// Add an option with a hover explanation.
    pub fn option_with_tooltip(
        mut self,
        value: T,
        label: impl Into<String>,
        tooltip: impl Into<String>,
    ) -> Self {
        self.segments.push(Segment {
            value,
            label: label.into(),
            enabled: true,
            tooltip: Some(tooltip.into()),
            dot: None,
            busy: false,
            icon: None,
        });
        self
    }

    /// Add a disabled option with an explanation.
    pub fn disabled_option(
        mut self,
        value: T,
        label: impl Into<String>,
        why: impl Into<String>,
    ) -> Self {
        self.segments.push(Segment {
            value,
            label: label.into(),
            enabled: false,
            tooltip: Some(why.into()),
            dot: None,
            busy: false,
            icon: None,
        });
        self
    }

    /// Stretch to the available width with equal segments.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Small height (28 px).
    pub fn small(mut self) -> Self {
        self.height = size::CONTROL_SM;
        self
    }

    /// Leading icon of the option added last.
    pub fn icon(mut self, icon: Icon) -> Self {
        if let Some(segment) = self.segments.last_mut() {
            segment.icon = Some(icon);
        }
        self
    }

    /// Minimum segment width.
    pub fn min_segment(mut self, width: f32) -> Self {
        self.min_segment = width;
        self
    }

    /// Maximum segment width: longer labels are elided ("…") and shown in full
    /// on hover.
    pub fn max_segment(mut self, width: f32) -> Self {
        self.max_segment = width;
        self
    }

    /// A status dot before the label of the option added last.
    pub fn dot(mut self, status: Status) -> Self {
        if let Some(seg) = self.segments.last_mut() {
            seg.dot = Some(status);
        }
        self
    }

    /// What the option added last shows is loading: a spinner before its label
    /// (in place of its status dot), and it is busy for screen readers.
    pub fn busy(mut self, busy: bool) -> Self {
        if let Some(seg) = self.segments.last_mut() {
            seg.busy = busy;
        }
        self
    }

    /// A trailing segment that opens a menu of further options `items` (value,
    /// label). It reads `label` (e.g. "More") with a chevron, or the label of the
    /// selected item when one of them is selected (then it is the selected
    /// segment). A click opens the menu; with the track focused, the arrow keys
    /// continue from the last segment through the items.
    pub fn overflow(
        mut self,
        label: impl Into<String>,
        items: impl IntoIterator<Item = (T, String)>,
    ) -> Self {
        let items: Vec<_> = items.into_iter().collect();
        self.overflow = (!items.is_empty()).then(|| Overflow {
            label: label.into(),
            items,
        });
        self
    }

    /// Show; the response is `changed()` when the selection changed.
    pub fn show(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let font_id = font(
            if self.height < size::CONTROL {
                text::SMALL
            } else {
                text::BODY
            },
            Weight::Medium,
        );
        let pad = 14.0;
        let inset = 3.0;
        // The overflow segment reads its selected item's label, else its own.
        let overflow_sel = self
            .overflow
            .as_ref()
            .and_then(|o| o.items.iter().position(|(v, _)| *v == *self.value));
        let overflow_label = self.overflow.as_ref().map(|o| match overflow_sel {
            Some(i) => o.items[i].1.clone(),
            None => o.label.clone(),
        });
        // Room taken next to a label: the status dot, the overflow chevron.
        let extras: Vec<f32> = self
            .segments
            .iter()
            .map(|s| {
                if s.busy {
                    SPIN_W
                } else if s.dot.is_some() {
                    DOT_W
                } else if s.icon.is_some() {
                    20.0
                } else {
                    0.0
                }
            })
            .chain(overflow_label.iter().map(|_| CHEVRON_GAP + CHEVRON))
            .collect();
        let mut galleys: Vec<_> = self
            .segments
            .iter()
            .map(|s| s.label.as_str())
            .chain(overflow_label.as_deref())
            .zip(&extras)
            .map(|(label, extra)| {
                let room = self.max_segment - pad * 2.0 - extra;
                elided(ui, label, font_id.clone(), Color32::PLACEHOLDER, room)
            })
            .collect();
        // Whole points, so the segments' shared edges fall on whole points too.
        let natural: Vec<f32> = galleys
            .iter()
            .zip(&extras)
            .map(|(g, extra)| {
                (g.size().x + pad * 2.0 + extra)
                    .ceil()
                    .max(self.min_segment)
            })
            .collect();
        let total_natural: f32 = natural.iter().sum::<f32>() + inset * 2.0;
        let width = if self.full_width {
            ui.available_width().max(total_natural)
        } else {
            total_natural
        };
        let (track, mut response) =
            ui.allocate_exact_size(vec2(width, self.height), Sense::focusable_noninteractive());
        // The track is the group's node: a radio group of the segments below.
        crate::design::a11y::describe(
            &response,
            egui::accesskit::Role::RadioGroup,
            self.label.as_deref().unwrap_or_default(),
        );
        let n = self.segments.len().max(1);
        let cells = natural.len().max(1);
        let widths: Vec<f32> = if self.full_width {
            vec![(width - inset * 2.0) / cells as f32; cells]
        } else {
            natural
        };
        // Segments abut (no gaps); only the track is bordered.
        let layout = SegmentLayout::new(track, &widths, inset);
        let rects = &layout.segments;
        let overflow_galley = if overflow_label.is_some() {
            galleys.pop()
        } else {
            None
        };

        let selected = self.segments.iter().position(|s| s.value == *self.value);
        // The segment under the indicator (the overflow one for its items).
        let highlighted = selected.or(overflow_sel.map(|_| self.segments.len()));
        let mut new_selection = None;
        let mut new_value: Option<T> = None;

        // Keyboard: arrows move between enabled segments while focused (and are
        // not used by egui to move the focus to a neighbouring widget).
        if response.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                );
            });
        }
        if response.has_focus()
            && let Some(cur) = selected
        {
            let step = ui.input(|i| {
                if i.key_pressed(Key::ArrowRight) {
                    1i32
                } else if i.key_pressed(Key::ArrowLeft) {
                    -1
                } else {
                    0
                }
            });
            if step != 0 {
                let mut idx = cur as i32;
                loop {
                    idx += step;
                    if idx < 0 || idx >= n as i32 {
                        break;
                    }
                    if self.segments[idx as usize].enabled {
                        new_selection = Some(idx as usize);
                        break;
                    }
                }
            }
        }
        // The arrows continue from the last segment through the overflow items.
        if response.has_focus()
            && let Some(o) = &self.overflow
        {
            let step = ui.input(|i| {
                i32::from(i.key_pressed(Key::ArrowRight)) - i32::from(i.key_pressed(Key::ArrowLeft))
            });
            let last = self.segments.iter().rposition(|s| s.enabled);
            match (overflow_sel, step) {
                (Some(k), 1) if k + 1 < o.items.len() => new_value = Some(o.items[k + 1].0.clone()),
                (Some(0), -1) => new_selection = last,
                (Some(k), -1) => new_value = Some(o.items[k - 1].0.clone()),
                (None, 1) if selected.is_some() && selected == last => {
                    new_value = Some(o.items[0].0.clone());
                }
                _ => {}
            }
        }

        let painter = ui.painter().clone();
        painter.rect(
            track,
            radius::CONTROL,
            pal.hover,
            Stroke::new(1.0, pal.border),
            StrokeKind::Inside,
        );

        // Animated indicator under the selected segment; one divider between two
        // segments, none under or next to the indicator.
        let indicator = highlighted.map(|sel| {
            let target = rects[sel];
            let animation = if ui
                .ctx()
                .data(|d| d.get_temp::<bool>(Id::unique("superba-reduce-motion")))
                .unwrap_or(false)
            {
                0.0
            } else {
                ANIM_SECS
            };
            let ax = ui.ctx().animate_value_with_time(
                self.id.with("x"),
                target.min.x - track.min.x,
                animation,
            );
            let aw = ui
                .ctx()
                .animate_value_with_time(self.id.with("w"), target.width(), animation);
            Rect::from_min_size(
                pos2(track.min.x + ax, target.min.y),
                vec2(aw, target.height()),
            )
        });
        let dividers = match indicator {
            Some(ind) => layout.dividers_outside(ind),
            None => layout.dividers(),
        };
        layout.paint_dividers(&painter, &dividers, pal.border);
        if let Some(ind) = indicator {
            // A darker gray pill with white text on the light gray track (user
            // request 24, after Surge's 全部 | 仅代理).
            let fill = selected_fill(pal);
            painter.rect(
                ind,
                radius::INNER,
                fill,
                Stroke::new(1.0, fill.lerp_to_gamma(Color32::BLACK, 0.08)),
                StrokeKind::Inside,
            );
        }

        for (i, (seg, galley)) in self.segments.iter().zip(galleys).enumerate() {
            let rect = rects[i];
            let id = self.id.with(i);
            // Clickable but not focusable: the track is the control's only Tab stop.
            let sense = if seg.enabled {
                Sense::CLICK
            } else {
                Sense::hover()
            };
            let mut r = ui.interact(rect, id, sense);
            if seg.enabled {
                r = r.on_hover_cursor(CursorIcon::PointingHand);
            }
            if let Some(tip) = &seg.tooltip {
                r = r.on_hover_text(tip.clone());
            } else if galley.elided {
                r = r.on_hover_text(seg.label.clone());
            }
            if r.clicked() && seg.enabled {
                new_selection = Some(i);
                response.request_focus();
            }
            let is_sel = Some(i) == selected;
            let color = if is_sel && !seg.enabled {
                // A disabled choice that is still the value in effect (e.g. "Quit"
                // without a tray) stays readable on the selected fill.
                SELECTED_TEXT.gamma_multiply(0.75)
            } else if !seg.enabled {
                pal.text_disabled
            } else if is_sel {
                SELECTED_TEXT
            } else if r.hovered() {
                pal.text
            } else {
                pal.text_muted
            };
            let lead = if seg.busy {
                Lead::Spinner(color, ui.input(|i| i.time))
            } else if let Some(icon) = seg.icon {
                Lead::Icon(icon, color)
            } else {
                seg.dot.map_or(Lead::None, |s| Lead::Dot(pal.status(s)))
            };
            paint_content(&painter, rect, lead, galley, color, false);
            let label = seg.label.clone();
            let enabled = seg.enabled;
            r.widget_info(|| {
                WidgetInfo::selected(egui::Role::RadioButton, enabled, is_sel, &label)
            });
            if seg.busy {
                ui.ctx()
                    .accesskit_node_builder(r.id, |node| node.set_busy());
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(60));
            }
        }

        if let (Some(o), Some(galley), Some(label)) =
            (&self.overflow, overflow_galley, &overflow_label)
        {
            let rect = rects[self.segments.len()];
            let r = ui
                .interact(rect, self.id.with("overflow"), Sense::CLICK)
                .on_hover_cursor(CursorIcon::PointingHand);
            let r = if galley.elided {
                r.on_hover_text(label.clone())
            } else {
                r
            };
            if r.clicked() {
                response.request_focus();
            }
            let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&r));
            let is_sel = overflow_sel.is_some();
            let color = if is_sel {
                SELECTED_TEXT
            } else if open || r.hovered() {
                pal.text
            } else {
                pal.text_muted
            };
            paint_content(&painter, rect, Lead::None, galley, color, true);
            // Named by its own label ("More"), the chosen item as its value.
            let name = o.label.clone();
            let value = overflow_sel
                .map(|i| o.items[i].1.clone())
                .unwrap_or_default();
            r.widget_info(|| {
                let mut info = WidgetInfo::new(egui::Role::ComboBox);
                info.label = Some(name.clone());
                info.current_text_value = Some(value.clone());
                info.selected = Some(is_sel);
                info
            });
            // As wide as its longest entry (up to MENU_TEXT_MAX, longer ones are
            // elided and shown in full on hover), at least as wide as the segment.
            let item_font = egui::TextStyle::Button.resolve(ui.style());
            let fits: Vec<(f32, bool)> = o
                .items
                .iter()
                .map(|(_, l)| {
                    let g = elided(
                        ui,
                        l,
                        item_font.clone(),
                        Color32::PLACEHOLDER,
                        MENU_TEXT_MAX,
                    );
                    (g.size().x, g.elided)
                })
                .collect();
            let text_w = fits.iter().map(|f| f.0).fold(0.0, f32::max);
            let menu_w = (text_w + ui.spacing().button_padding.x * 2.0 + 2.0)
                .max(rect.width())
                .max(MENU_MIN);
            egui::Popup::menu(&r)
                .gap(MENU_GAP)
                .width(menu_w)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                .show(|ui| {
                    ui.set_width(menu_w);
                    for ((v, label), (_, long)) in o.items.iter().zip(&fits) {
                        let on = *v == *self.value;
                        let mut item =
                            ui.add(egui::Button::selectable(on, label.as_str()).truncate());
                        if *long {
                            item = item.on_hover_text(label.clone());
                        }
                        if item.clicked() {
                            new_value = Some(v.clone());
                        }
                    }
                });
        }

        if let Some(i) = new_selection
            && Some(i) != selected
        {
            *self.value = self.segments[i].value.clone();
            response.mark_changed();
        } else if let Some(v) = new_value
            && v != *self.value
        {
            *self.value = v;
            response.mark_changed();
        }
        if show_focus(&response) {
            focus_ring(&painter, track, radius::CONTROL, pal);
        }
        response
    }
}

/// The overflow menu's narrowest width.
const MENU_MIN: f32 = 160.0;

/// What a segment shows before its label.
#[derive(Clone, Copy)]
enum Lead {
    None,
    /// A status dot of this colour.
    Dot(Color32),
    /// A spinner in this colour, at this time (seconds).
    Spinner(Color32, f64),
    Icon(Icon, Color32),
}

/// Paint a segment's content centred in `rect`: an optional status dot or
/// spinner, the label and, for the overflow segment, a chevron.
fn paint_content(
    painter: &egui::Painter,
    rect: Rect,
    lead: Lead,
    galley: std::sync::Arc<egui::Galley>,
    color: Color32,
    chevron: bool,
) {
    let lead_w = match lead {
        Lead::None => 0.0,
        Lead::Dot(_) => DOT_W,
        Lead::Spinner(..) => SPIN_W,
        Lead::Icon(..) => 20.0,
    };
    let trail = if chevron { CHEVRON_GAP + CHEVRON } else { 0.0 };
    let x = rect.center().x - (lead_w + galley.size().x + trail) / 2.0;
    let cy = rect.center().y;
    match lead {
        Lead::None => {}
        Lead::Dot(c) => {
            painter.circle_filled(pos2(x + 3.0, cy), 3.0, c);
        }
        Lead::Spinner(c, time) => icons::spinner(
            painter,
            Rect::from_center_size(pos2(x + 6.0, cy), vec2(12.0, 12.0)),
            c,
            time,
        ),
        Lead::Icon(icon, color) => icons::paint(
            painter,
            Rect::from_center_size(pos2(x + 7.0, cy), vec2(14.0, 14.0)),
            icon,
            color,
        ),
    }
    let lead = lead_w;
    let end = x + lead + galley.size().x;
    painter.galley(pos2(x + lead, cy - galley.size().y / 2.0), galley, color);
    if chevron {
        let at = Rect::from_min_size(
            pos2(end + CHEVRON_GAP, cy - CHEVRON / 2.0),
            vec2(CHEVRON, CHEVRON),
        );
        icons::paint(painter, at, Icon::ChevronDown, color);
    }
}

/// Geometry of a segmented control: segments that share their edges (no gap, no
/// overlap, every edge on a whole point) inside one track, and one divider on
/// each edge between two segments. Only the track has a border; segments never
/// draw their own, so neighbours never show a double line.
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentLayout {
    /// The track: the control's outer border.
    pub track: Rect,
    /// One rectangle per segment, left to right.
    pub segments: Vec<Rect>,
}

impl SegmentLayout {
    /// Segments of `widths` left to right inside `track`, `inset` from its edges.
    /// Edges are rounded to whole points, so a segment ends exactly where the
    /// next one starts.
    pub fn new(track: Rect, widths: &[f32], inset: f32) -> Self {
        let (top, bottom) = (track.min.y + inset, track.max.y - inset);
        let mut end = track.min.x + inset;
        let mut left = end.round();
        let segments = widths
            .iter()
            .map(|w| {
                end += w;
                let right = end.round().max(left);
                let rect = Rect::from_min_max(pos2(left, top), pos2(right, bottom));
                left = right;
                rect
            })
            .collect();
        Self { track, segments }
    }

    /// The x of each divider: one per edge shared by two segments.
    pub fn dividers(&self) -> Vec<f32> {
        self.segments.windows(2).map(|w| w[0].max.x).collect()
    }

    /// The dividers drawn while `highlight` (the selected segment's indicator,
    /// possibly moving) is shown: none under or touching it.
    pub fn dividers_outside(&self, highlight: Rect) -> Vec<f32> {
        self.dividers()
            .into_iter()
            .filter(|x| *x < highlight.min.x - 0.5 || *x > highlight.max.x + 0.5)
            .collect()
    }

    /// The dividers drawn with segment `selected` selected (all without one).
    pub fn visible_dividers(&self, selected: Option<usize>) -> Vec<f32> {
        match selected.and_then(|i| self.segments.get(i)) {
            Some(rect) => self.dividers_outside(*rect),
            None => self.dividers(),
        }
    }

    /// Paint 1 px dividers at `xs`, a little shorter than the segments.
    pub fn paint_dividers(&self, painter: &egui::Painter, xs: &[f32], color: Color32) {
        let Some(first) = self.segments.first() else {
            return;
        };
        let y = egui::Rangef::new(first.min.y + DIVIDER_INSET, first.max.y - DIVIDER_INSET);
        for x in xs {
            painter.vline(*x, y, Stroke::new(1.0, color));
        }
    }
}

/// Space between a divider's ends and the segment's top / bottom.
const DIVIDER_INSET: f32 = 4.0;
