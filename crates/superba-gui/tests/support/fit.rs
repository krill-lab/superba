//! Fit-page harness (UX §2.3, plan §5 B5): render a page the way `app::content`
//! does at a window size — the page rectangle right of the sidebar, inside
//! `design::layout::fit_area` — and report whether it fits that rectangle without
//! a page-level scroll area. Used by the shell, B6 and B7 page tests.

use egui::{Rect, pos2};
use superba_core::{Locale, Timestamp};
use superba_gui::design::layout;
use superba_gui::design::tokens::size;
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Cx, Data};

/// The window sizes every Fit page is checked at (UX §8).
pub const SIZES: [[f32; 2]; 2] = [[1280.0, 800.0], [1440.0, 900.0]];

/// The body height of a Fit page below its header in a `window` sized window
/// (UX §2.3, user bug 85): 684 px at 1280x800 and 784 px at 1440x900 on macOS
/// (the content card 10 px below the window's top), 662 / 762 px on Windows
/// and Linux (the card below the 32 px title band with the window buttons).
pub fn body_height(window: [f32; 2]) -> f32 {
    use superba_gui::design::tokens::chrome::{BUTTONS, Buttons};
    let at_800 = match BUTTONS {
        Buttons::TrafficLights => 684.0,
        Buttons::Caption | Buttons::Round => 662.0,
    };
    at_800 + (window[1] - 800.0)
}

/// What one render produced (second frame).
#[derive(Debug)]
pub struct FitReport {
    /// The page rectangle (header included) the page was given.
    pub rect: Rect,
    /// Height below the page header ([`body_height`]).
    pub body_height: f32,
    /// Bottom edge of what the page drew.
    pub used_bottom: f32,
    /// The page was drawn inside the short-window scroll fallback.
    pub scrolled: bool,
    /// Scroll areas the page created. A Fit page has none, except log-style
    /// tables that scroll inside their own area and Phase 0 pages still inside
    /// `layout::scroll_body`.
    pub scroll_areas: usize,
    /// Every text shape drawn.
    pub texts: Vec<String>,
    /// Every text shape drawn with its rectangle and whether it was elided.
    pub text_shapes: Vec<(String, Rect, bool)>,
    /// Actions the page returned in the second frame.
    pub actions: Vec<Action>,
}

impl FitReport {
    /// Nothing was drawn below the page rectangle.
    pub fn fits(&self) -> bool {
        self.used_bottom <= self.rect.max.y + 0.5
    }

    /// All texts, one per line (for `contains` checks).
    pub fn all_text(&self) -> String {
        self.texts.join("\n")
    }

    /// Texts drawn (partly) right of the page rectangle (cut off by the window).
    pub fn beyond_right(&self) -> Vec<&str> {
        self.text_shapes
            .iter()
            .filter(|(_, r, _)| r.max.x > self.rect.max.x + 0.5)
            .map(|(t, _, _)| t.as_str())
            .collect()
    }

    /// Whether `text` was drawn whole (at least once, not elided).
    pub fn whole(&self, text: &str) -> bool {
        self.text_shapes
            .iter()
            .any(|(t, _, elided)| t == text && !elided)
    }
}

/// Render `show` as a Fit page in a `window` sized window and report. Runs two
/// frames (the first lays out, the second is measured) on a fresh context.
pub fn render_fit(
    window: [f32; 2],
    locale: Locale,
    theme: egui::Theme,
    data: &Data,
    mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
) -> FitReport {
    let ctx = egui::Context::default();
    let fonts_report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&fonts_report));
    style::install(&ctx, Some(theme));
    let tr = Tr::new(locale);
    let service = super::idle_service();
    let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(window[0], window[1]));
    let central = Rect::from_min_max(pos2(size::SIDEBAR, 0.0), screen.max);
    let page = layout::page_rect(central, true);
    let mut report = FitReport {
        rect: page,
        body_height: page.height() - size::PAGE_HEADER,
        used_bottom: 0.0,
        scrolled: false,
        scroll_areas: 0,
        texts: Vec::new(),
        text_shapes: Vec::new(),
        actions: Vec::new(),
    };
    for frame in 0..2 {
        let mut actions = Vec::new();
        let mut used = 0.0;
        let mut scrolled = false;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &fonts_report,
                    service: &service,
                    log: &[],
                };
                let mut area = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(page)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                let fit = layout::fit_area(&mut area, layout::FIT_MIN_HEIGHT, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    show(ui, &cx, &mut actions);
                    ui.min_rect().max.y
                });
                used = fit.inner;
                scrolled = fit.scrolled;
            },
        );
        if frame == 1 {
            report.used_bottom = used;
            report.scrolled = scrolled;
            report.actions = actions;
            report.texts = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            report.text_shapes = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((
                        t.galley.text().to_owned(),
                        // Right-aligned galleys extend left of `pos`.
                        t.galley.rect.translate(t.pos.to_vec2()),
                        t.galley.elided,
                    )),
                    _ => None,
                })
                .collect();
        }
        output.drop_without_applying_deltas();
    }
    report.scroll_areas = ctx.data(|d| d.count::<egui::containers::scroll_area::State>());
    report
}
