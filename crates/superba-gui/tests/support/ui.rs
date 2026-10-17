//! Drive egui frames in tests: render a closure, find where a text was drawn,
//! click it (press in one frame, release in the next, as a real pointer does).

use egui::{Event, PointerButton, Pos2, Rect, pos2};
use superba_gui::design::{fonts, style};

/// A headless window of a fixed size.
pub struct Harness {
    /// The context (fonts and style installed).
    pub ctx: egui::Context,
    /// The font report the pages read.
    pub fonts: fonts::FontReport,
    screen: Rect,
}

impl Harness {
    /// A `size` window in `theme`.
    pub fn new(size: [f32; 2], theme: egui::Theme) -> Self {
        let ctx = egui::Context::default();
        let fonts = fonts::FontReport::default();
        ctx.set_fonts(fonts::definitions(&fonts));
        style::install(&ctx, Some(theme));
        Self {
            ctx,
            fonts,
            screen: Rect::from_min_max(pos2(0.0, 0.0), pos2(size[0], size[1])),
        }
    }

    /// Run one frame with `events`; returns every drawn text with its rectangle.
    pub fn frame(
        &self,
        events: Vec<Event>,
        f: &mut dyn FnMut(&mut egui::Ui),
    ) -> Vec<(String, Rect)> {
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(self.screen),
                events,
                ..Default::default()
            },
            |ui| f(ui),
        );
        let texts = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        texts
    }

    /// Two frames to settle the layout; returns the texts of the second.
    pub fn settle(&self, f: &mut dyn FnMut(&mut egui::Ui)) -> Vec<(String, Rect)> {
        self.frame(Vec::new(), f);
        self.frame(Vec::new(), f)
    }

    /// Click the first text equal to `text` (after settling); `false` when it
    /// was not drawn.
    pub fn click_text(&self, text: &str, f: &mut dyn FnMut(&mut egui::Ui)) -> bool {
        let texts = self.settle(f);
        let Some((_, rect)) = texts.iter().find(|(t, _)| t == text) else {
            return false;
        };
        self.click_at(rect.center(), f);
        true
    }

    /// Click the last text equal to `text` (a button that repeats a label above
    /// it); `false` when it was not drawn.
    pub fn click_last_text(&self, text: &str, f: &mut dyn FnMut(&mut egui::Ui)) -> bool {
        let texts = self.settle(f);
        let Some((_, rect)) = texts.iter().rev().find(|(t, _)| t == text) else {
            return false;
        };
        self.click_at(rect.center(), f);
        true
    }

    /// Press at `pos` in one frame and release in the next.
    pub fn click_at(&self, pos: Pos2, f: &mut dyn FnMut(&mut egui::Ui)) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![Event::PointerMoved(pos), button(true)], f);
        self.frame(vec![button(false)], f);
        self.frame(Vec::new(), f);
    }

    /// Press `key` in one frame.
    pub fn key(&self, key: egui::Key, f: &mut dyn FnMut(&mut egui::Ui)) {
        self.frame(
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            f,
        );
    }
}
