//! Reply stats line preview (UX §3.5): a sample reply as the desktop app shows
//! it, followed by the stats line as a markdown blockquote. Verified rendering in
//! the app (spike RS 3.2): a grey left rule and the text in the **normal body
//! colour**, not grey text; `> *...*` renders italic.

use egui::{Frame, Margin, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, radius, text};

/// The preview.
#[must_use = "call .show(ui)"]
#[derive(Clone, Debug, Default)]
pub struct QuotePreview {
    caption: String,
    reply: String,
    line: String,
    italic: bool,
}

impl QuotePreview {
    /// A preview of `line` under `reply`.
    pub fn new(reply: impl Into<String>, line: impl Into<String>) -> Self {
        Self {
            caption: String::new(),
            reply: reply.into(),
            line: line.into(),
            italic: false,
        }
    }

    /// Caption above the reply ("Preview · last turn (14:32:07)").
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = caption.into();
        self
    }

    /// Italic quote style.
    pub fn italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    /// The markdown the router appends for `line` in this style (`> ...` or
    /// `> *...*`), without the invisible marker the router adds.
    pub fn markdown(line: &str, italic: bool) -> String {
        if italic {
            format!("> *{line}*")
        } else {
            format!("> {line}")
        }
    }

    /// Draw at the available width.
    pub fn show(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let width = ui.available_width();
        Frame::new()
            .fill(pal.surface)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(radius::CONTROL + 2)
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(width - 28.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                if !self.caption.is_empty() {
                    ui.label(
                        RichText::new(&self.caption)
                            .font(font(text::CAPTION, Weight::Medium))
                            .color(pal.text_subtle),
                    );
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.reply)
                            .font(font(text::BODY, Weight::Regular))
                            .color(pal.text),
                    )
                    .wrap(),
                );
                // Blockquote: grey rule, body-colour text. A leading ⚡ is drawn
                // as the bolt icon (the UI fonts have no emoji).
                let inner_w = ui.available_width();
                let (bolt, line) = match self.line.strip_prefix('⚡') {
                    Some(rest) => (true, rest.trim_start().to_owned()),
                    None => (false, self.line.clone()),
                };
                let bolt_w = if bolt { 18.0 } else { 0.0 };
                let mut rt = RichText::new(line)
                    .font(font(text::BODY, Weight::Regular))
                    .color(pal.text);
                if self.italic {
                    rt = rt.italics();
                }
                let galley = egui::WidgetText::from(rt).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Wrap),
                    inner_w - 14.0 - bolt_w,
                    egui::TextStyle::Body,
                );
                let h = galley.size().y + 4.0;
                let (r, _) = ui.allocate_exact_size(vec2(inner_w, h), Sense::hover());
                ui.painter().rect_filled(
                    Rect::from_min_size(r.min, vec2(3.0, r.height())),
                    1,
                    pal.border_strong,
                );
                if bolt {
                    let ir =
                        Rect::from_min_size(pos2(r.min.x + 14.0, r.min.y + 4.0), vec2(14.0, 14.0));
                    crate::design::icons::paint(
                        ui.painter(),
                        ir,
                        crate::design::icons::Icon::Bolt,
                        pal.text,
                    );
                }
                ui.painter().galley(
                    pos2(r.min.x + 14.0 + bolt_w, r.min.y + 2.0),
                    galley,
                    pal.text,
                );
            })
            .response
    }
}
