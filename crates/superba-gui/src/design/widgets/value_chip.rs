//! Setting values in a fixed width (user bug 15): [`ValueChip`] draws one line
//! that never grows past its maximum width, shortened with the ellipsis of the
//! value's [`Shape`] (middle for paths, URLs and commands, end for prose), in
//! Geist Mono for everything but prose, with an always-visible hint such as
//! "+3 lines" and the full value, wrapped, in the hover tooltip.

use egui::{Color32, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use crate::design::elide::{self, Ellipsis, Shape};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::{Palette, radius, text};

/// Characters a hover tooltip wraps at.
pub const TOOLTIP_CHARS: f32 = 60.0;
/// Characters of a value shown in a tooltip at most.
const TOOLTIP_MAX: usize = 1600;

/// Shorten `text` (one line) to `max_width` points in `font_id` with `mode`.
pub fn fit(ui: &Ui, text: &str, font_id: &FontId, max_width: f32, mode: Ellipsis) -> String {
    ui.ctx().fonts_mut(|f| {
        // Glyph advances, less a hair for kerning and rounding.
        elide::elide_by(text, (max_width - 1.0).max(0.0), mode, |c| {
            f.glyph_width(font_id, c)
        })
    })
}

/// The font a value of `shape` is drawn in at `size`.
pub fn value_font(shape: Shape, size: f32) -> FontId {
    if shape.mono() {
        mono_font(size, Weight::Regular)
    } else {
        font(size, Weight::Regular)
    }
}

/// `text` for a tooltip: at most [`TOOLTIP_MAX`] characters.
pub fn tooltip_text(text: &str) -> String {
    if text.chars().count() <= TOOLTIP_MAX {
        return text.to_owned();
    }
    let mut s: String = text.chars().take(TOOLTIP_MAX).collect();
    s.push('…');
    s
}

/// Show `text` as a wrapped tooltip about [`TOOLTIP_CHARS`] characters wide.
pub fn value_tooltip(response: Response, text: &str, shape: Shape) -> Response {
    let text = tooltip_text(text);
    response.on_hover_ui(move |ui| {
        let f = value_font(shape, text::CAPTION);
        let w = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&f, '0')) * TOOLTIP_CHARS;
        ui.set_max_width(w);
        let pal = Palette::of(ui.ctx());
        ui.add(
            egui::Label::new(egui::RichText::new(text.as_str()).font(f).color(pal.text))
                .wrap_mode(egui::TextWrapMode::Wrap),
        );
    })
}

/// A value in at most `max_width` points: pill (list rows) or plain text (diff
/// rows), first line only, shortened by shape, with a fixed hint after it.
#[must_use = "add it with ui.add(..)"]
pub struct ValueChip<'a> {
    text: &'a str,
    shape: Shape,
    hint: Option<String>,
    tooltip: Option<String>,
    muted: bool,
    pill: bool,
    max_width: f32,
    size: f32,
    color: Option<Color32>,
}

impl<'a> ValueChip<'a> {
    /// A chip for `text` (its first line is shown) of `shape`.
    pub fn new(text: &'a str, shape: Shape) -> Self {
        Self {
            text,
            shape,
            hint: None,
            tooltip: None,
            muted: false,
            pill: true,
            max_width: 160.0,
            size: text::CAPTION,
            color: None,
        }
    }

    /// Never wider than this (points).
    pub fn max_width(mut self, w: f32) -> Self {
        self.max_width = w;
        self
    }

    /// Always-visible hint after the value ("+3 lines", "+2 items").
    pub fn hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }

    /// Tooltip text (default: the whole value, when anything is cut).
    pub fn tooltip(mut self, text: impl Into<String>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Muted colours (a default, not a configured value).
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    /// Plain text without the pill.
    pub fn plain(mut self) -> Self {
        self.pill = false;
        self
    }

    /// Font size (default 12).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Text colour (default: text, or muted).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }
}

impl Widget for ValueChip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (line, more) = elide::first_line(self.text);
        let pad = if self.pill { 8.0 } else { 0.0 };
        let f = value_font(self.shape, self.size);
        let hint_font = font(self.size, Weight::Medium);
        let hint = self.hint.as_ref().map(|h| {
            ui.painter()
                .layout_no_wrap(h.clone(), hint_font.clone(), pal.text_subtle)
        });
        let max_w = self.max_width.min(ui.available_width()).max(24.0);
        // A hint that would leave the value almost no room is left to the tooltip.
        let hint = hint.filter(|g| max_w - 2.0 * pad - g.size().x - 6.0 >= 40.0);
        let hint_w = hint.as_ref().map_or(0.0, |g| g.size().x + 6.0);
        let room = (max_w - 2.0 * pad - hint_w).max(12.0);
        let shown = fit(ui, line, &f, room, self.shape.ellipsis());
        let color = self.color.unwrap_or(if self.muted {
            pal.text_subtle
        } else {
            pal.text
        });
        let galley = ui.painter().layout_no_wrap(shown.clone(), f, color);
        let w = (galley.size().x + 2.0 * pad + hint_w).min(max_w);
        let h = if self.pill {
            22.0
        } else {
            galley.size().y.max(18.0)
        };
        let (rect, response) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        // Read as the value's first line, not the shortened one (user bug 77).
        crate::design::a11y::text(&response, line);
        if ui.is_rect_visible(rect) {
            if self.pill {
                let fill = if self.muted { pal.bg_subtle } else { pal.hover };
                ui.painter().rect(
                    rect,
                    radius::PILL,
                    fill,
                    Stroke::new(1.0, pal.border),
                    StrokeKind::Inside,
                );
            }
            let clip = Rect::from_min_max(
                pos2(rect.min.x + pad, rect.min.y),
                pos2(rect.max.x - pad - hint_w, rect.max.y),
            );
            ui.painter().with_clip_rect(clip).galley(
                pos2(clip.min.x, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
            if let Some(g) = hint {
                ui.painter().galley(
                    pos2(
                        rect.max.x - pad - g.size().x,
                        rect.center().y - g.size().y / 2.0,
                    ),
                    g,
                    pal.text_subtle,
                );
            }
        }
        let cut = shown != line || more > 0 || self.tooltip.is_some();
        if cut {
            let tip = self.tooltip.as_deref().unwrap_or(self.text);
            value_tooltip(response, tip, self.shape)
        } else {
            response
        }
    }
}
