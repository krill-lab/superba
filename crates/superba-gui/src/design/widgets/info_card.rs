//! Small titled card of label / value lines (UX §3.5 detail pane, like Surge's
//! request details): a caption title, then one line per fact. Values that do not
//! fit are elided and shown in full on hover; a line can end in a small "copy"
//! link.
//!
//! The card is compact (6 px vertical padding, 16 px rows) and its height is known
//! before it is drawn ([`height`]), so a pane can size itself to its cards. The
//! frame, border included, stays inside the width and height the card is given:
//! a clip rectangle around the card never cuts its border.

use egui::{CursorIcon, Frame, Margin, Response, Sense, Stroke, Ui, WidgetInfo, vec2};

use super::Status;
use super::log_table::elided;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::{Palette, radius, text};

/// Inner padding (compact: the detail pane shows two rows of cards).
const PAD: Margin = Margin {
    left: 10,
    right: 10,
    top: 6,
    bottom: 6,
};
/// Border width.
const STROKE: f32 = 1.0;
/// Height of the title row.
pub const TITLE_HEIGHT: f32 = 16.0;
/// Height of one line.
pub const LINE_HEIGHT: f32 = 16.0;
/// Space above each line.
pub const LINE_GAP: f32 = 2.0;
/// Gap between a value and its copy link.
const LINK_GAP: f32 = 8.0;

/// Outer height (border included) of a card with `lines` lines and no footer.
pub fn height(lines: usize) -> f32 {
    f32::from(PAD.top + PAD.bottom)
        + 2.0 * STROKE
        + TITLE_HEIGHT
        + lines as f32 * (LINE_GAP + LINE_HEIGHT)
}

/// A link at the end of an [`InfoLine`] that copies a text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopyLink {
    /// Link text ("复制").
    pub label: String,
    /// Hover text and accessible name ("复制缓存键").
    pub tooltip: String,
    /// Text put on the clipboard.
    pub text: String,
}

/// What an [`InfoCard`] did this frame.
#[derive(Debug)]
pub struct InfoCardResponse {
    /// The card.
    pub response: Response,
    /// Text of the copy link clicked this frame.
    pub copied: Option<String>,
}

/// One line of an [`InfoCard`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InfoLine {
    /// Label on the left (`None` = the value takes the whole line).
    pub label: Option<String>,
    /// Value.
    pub value: String,
    /// Monospace value (ids, keys, paths).
    pub mono: bool,
    /// A status dot before the value.
    pub dot: Option<Status>,
    /// Hover text of the value.
    pub tooltip: Option<String>,
    /// A copy link at the end of the line.
    pub copy: Option<CopyLink>,
}

/// Extra content drawn under the lines (copy buttons).
type FooterFn<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// A small card with a title and label / value lines.
#[must_use = "call .show(ui)"]
pub struct InfoCard<'a> {
    title: String,
    lines: Vec<InfoLine>,
    label_width: f32,
    min_height: f32,
    footer: Option<FooterFn<'a>>,
}

impl<'a> InfoCard<'a> {
    /// A card titled `title`.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            lines: Vec::new(),
            label_width: 64.0,
            min_height: 0.0,
            footer: None,
        }
    }

    /// A label / value line.
    pub fn line(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.lines.push(InfoLine {
            label: Some(label.into()),
            value: value.into(),
            ..InfoLine::default()
        });
        self
    }

    /// A line with only a value.
    pub fn text(mut self, value: impl Into<String>) -> Self {
        self.lines.push(InfoLine {
            value: value.into(),
            ..InfoLine::default()
        });
        self
    }

    /// A label / monospace value line.
    pub fn mono(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.lines.push(InfoLine {
            label: Some(label.into()),
            value: value.into(),
            mono: true,
            ..InfoLine::default()
        });
        self
    }

    /// Add a prepared line.
    pub fn push(mut self, line: InfoLine) -> Self {
        self.lines.push(line);
        self
    }

    /// Hover text of the last line.
    pub fn tip(mut self, tooltip: impl Into<String>) -> Self {
        if let Some(l) = self.lines.last_mut() {
            l.tooltip = Some(tooltip.into());
        }
        self
    }

    /// Status dot of the last line.
    pub fn dot(mut self, status: Status) -> Self {
        if let Some(l) = self.lines.last_mut() {
            l.dot = Some(status);
        }
        self
    }

    /// Width of the label column (default 64 px).
    pub fn label_width(mut self, width: f32) -> Self {
        self.label_width = width;
        self
    }

    /// Minimum height (cards of one row share a height).
    pub fn min_height(mut self, height: f32) -> Self {
        self.min_height = height;
        self
    }

    /// Extra content under the lines.
    pub fn footer(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.footer = Some(Box::new(add));
        self
    }

    /// The lines (tests).
    pub fn lines(&self) -> &[InfoLine] {
        &self.lines
    }

    /// Draw at the available width. The card, border included, is exactly that
    /// wide and (without a footer) [`height`] of its lines or `min_height` high.
    pub fn show(self, ui: &mut Ui) -> InfoCardResponse {
        let pal = Palette::of(ui.ctx());
        let width = ui.available_width();
        // The frame adds its padding and border around the content.
        let (pad_x, pad_y) = (
            f32::from(PAD.left + PAD.right) + 2.0 * STROKE,
            f32::from(PAD.top + PAD.bottom) + 2.0 * STROKE,
        );
        let mut copied = None;
        let response = Frame::new()
            .fill(pal.bg_subtle)
            .stroke(Stroke::new(STROKE, pal.border))
            .corner_radius(radius::CONTROL + 2)
            .inner_margin(PAD)
            .show(ui, |ui| {
                ui.set_width((width - pad_x).max(0.0));
                ui.set_min_height((self.min_height - pad_y).max(0.0));
                ui.spacing_mut().item_spacing.y = LINE_GAP;
                let w = ui.available_width();
                let (r, _) = ui.allocate_exact_size(vec2(w, TITLE_HEIGHT), Sense::hover());
                let g = elided(
                    ui,
                    &self.title,
                    font(text::CAPTION, Weight::Medium),
                    pal.text_subtle,
                    w,
                );
                ui.painter().galley(
                    egui::pos2(r.min.x, r.center().y - g.size().y / 2.0),
                    g,
                    pal.text_subtle,
                );
                for line in &self.lines {
                    if let Some(text) = info_line(ui, line, self.label_width, pal) {
                        copied = Some(text);
                    }
                }
                if let Some(f) = self.footer {
                    ui.add_space(2.0);
                    f(ui);
                }
            })
            .response;
        InfoCardResponse { response, copied }
    }
}

/// The copy link at the right end of the line `r`; returns its left edge and
/// whether it was clicked.
fn copy_link(ui: &mut Ui, link: &CopyLink, r: egui::Rect, pal: &Palette) -> (f32, bool) {
    let g = ui.painter().layout_no_wrap(
        link.label.clone(),
        font(text::CAPTION, Weight::Medium),
        pal.link,
    );
    let size = vec2(g.size().x, LINE_HEIGHT);
    let rect = egui::Rect::from_min_size(egui::pos2(r.max.x - size.x, r.min.y), size);
    let resp = ui
        .interact(
            rect,
            ui.scope_id().with(("copy", &link.tooltip, &link.text)),
            Sense::click(),
        )
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(&link.tooltip);
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Link, true, &link.tooltip));
    ui.painter().galley(
        egui::pos2(rect.min.x, rect.center().y - g.size().y / 2.0),
        g,
        pal.link,
    );
    if resp.hovered() {
        ui.painter().hline(
            rect.x_range(),
            rect.max.y - 1.0,
            Stroke::new(1.0, pal.link.gamma_multiply(0.6)),
        );
    }
    (rect.min.x - LINK_GAP, resp.clicked())
}

/// Draw one line; returns the copied text when its copy link was clicked.
fn info_line(ui: &mut Ui, line: &InfoLine, label_width: f32, pal: &Palette) -> Option<String> {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, LINE_HEIGHT), Sense::hover());
    let (right, copied) = match &line.copy {
        Some(link) => {
            let (right, clicked) = copy_link(ui, link, r, pal);
            (right, clicked.then(|| link.text.clone()))
        }
        None => (r.max.x, None),
    };
    let mut x = r.min.x;
    if let Some(label) = &line.label {
        let g = elided(
            ui,
            label,
            font(text::CAPTION, Weight::Regular),
            pal.text_muted,
            label_width - 6.0,
        );
        ui.painter().galley(
            egui::pos2(x, r.center().y - g.size().y / 2.0),
            g,
            pal.text_muted,
        );
        x += label_width;
    }
    if let Some(status) = line.dot {
        ui.painter()
            .circle_filled(egui::pos2(x + 3.0, r.center().y), 3.0, pal.status(status));
        x += 10.0;
    }
    let f = if line.mono {
        mono_font(text::CAPTION, Weight::Regular)
    } else {
        font(text::CAPTION, Weight::Regular)
    };
    let g = elided(ui, &line.value, f, pal.text, right - x);
    let was_elided = g.elided;
    ui.painter()
        .galley(egui::pos2(x, r.center().y - g.size().y / 2.0), g, pal.text);
    let tip = line
        .tooltip
        .clone()
        .or_else(|| was_elided.then(|| line.value.clone()));
    if let Some(t) = tip {
        // Only over the label and value: the copy link has its own hover text.
        let value = egui::Rect::from_min_max(r.min, egui::pos2(right, r.max.y));
        ui.interact(value, resp.id.with("value"), Sense::hover())
            .on_hover_text(t);
    }
    copied
}
