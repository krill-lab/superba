//! Page harness of the B7 page tests (Router & usage, Remote hosts, Updates):
//! draws a page the way `app::content` does — the Fit (or Scroll) rectangle right
//! of the sidebar — frame after frame on one egui context with a given service,
//! so background reads finish, clicks can be sent to drawn texts, and the result
//! of the last frame can be inspected (texts, actions, page-level scrolling).

#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Pos2, Rect, pos2};
use superba_core::{Locale, SwitchService, Timestamp};
use superba_gui::design::layout;
use superba_gui::design::tokens::size;
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Cx, Data};

use crate::support::fake::FakeService;

/// What one frame produced.
#[derive(Debug)]
pub struct Frame {
    /// The page rectangle (header included).
    pub rect: Rect,
    /// Bottom of what the page drew.
    pub used_bottom: f32,
    /// The short-window scroll fallback was used.
    pub scrolled: bool,
    /// Texts drawn, with where.
    pub texts: Vec<(String, Rect)>,
    /// Text rectangles with their effective clip; used for cell-fit regressions.
    pub clipped_texts: Vec<(String, Rect, Rect)>,
    /// Actions returned.
    pub actions: Vec<Action>,
    /// Scroll areas created so far on the context.
    pub scroll_areas: usize,
    /// How often the page was drawn in this frame (egui may run a pass twice).
    pub passes: usize,
    /// Outlined rectangles drawn (control frames), each with the clip rectangle
    /// it was drawn in.
    pub outlines: Vec<(Rect, Rect)>,
    /// Texts drawn elided (with `…`).
    pub elided: Vec<String>,
    /// Bounding rectangles of the meshes drawn (gradients such as edge fades).
    pub meshes: Vec<Rect>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            rect: Rect::NOTHING,
            used_bottom: 0.0,
            scrolled: false,
            texts: Vec::new(),
            clipped_texts: Vec::new(),
            actions: Vec::new(),
            scroll_areas: 0,
            passes: 0,
            outlines: Vec::new(),
            elided: Vec::new(),
            meshes: Vec::new(),
        }
    }
}

impl Frame {
    /// Nothing was drawn below the page rectangle.
    pub fn fits(&self) -> bool {
        self.used_bottom <= self.rect.max.y + 0.5
    }

    /// Every text, one per line.
    pub fn all_text(&self) -> String {
        self.texts
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A text containing `needle` was drawn.
    pub fn has(&self, needle: &str) -> bool {
        self.texts.iter().any(|(t, _)| t.contains(needle))
    }

    /// Where the first text equal to `text` (else containing it) was drawn.
    pub fn find(&self, text: &str) -> Option<Rect> {
        self.texts
            .iter()
            .find(|(t, _)| t == text)
            .or_else(|| self.texts.iter().find(|(t, _)| t.contains(text)))
            .map(|(_, r)| *r)
    }
}

/// The harness.
pub struct Harness {
    ctx: egui::Context,
    tr: Tr,
    fonts: fonts::FontReport,
    /// The service pages read through.
    pub service: Arc<dyn SwitchService>,
    /// Shell data.
    pub data: Data,
    window: [f32; 2],
    fit: bool,
    min_height: f32,
    /// Input events per coming frame.
    pending: std::collections::VecDeque<Vec<egui::Event>>,
}

/// Shell data loaded from `service` (overview, profiles, sites).
pub fn data_from(service: &FakeService) -> Data {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut data = Data {
            sites: service.sites().await.unwrap(),
            ..Data::default()
        };
        data.overview.finish(service.overview().await);
        data.profiles.finish(service.profile_set().await);
        data
    })
}

impl Harness {
    /// A harness for a `window` sized window in `locale`; `fit` = Fit page.
    pub fn new(window: [f32; 2], locale: Locale, service: Arc<FakeService>, fit: bool) -> Self {
        let ctx = egui::Context::default();
        let report = fonts::FontReport::default();
        ctx.set_fonts(fonts::definitions(&report));
        style::install(&ctx, Some(egui::Theme::Light));
        let data = data_from(&service);
        Self {
            ctx,
            tr: Tr::new(locale),
            fonts: report,
            service,
            data,
            window,
            fit,
            min_height: layout::FIT_MIN_HEIGHT,
            pending: Default::default(),
        }
    }

    /// Use the real page's minimum height (the Superba rail fits a 960px window).
    pub fn min_height(mut self, height: f32) -> Self {
        self.min_height = height;
        self
    }

    /// Use the dark theme.
    pub fn dark(self) -> Self {
        style::install(&self.ctx, Some(egui::Theme::Dark));
        self
    }

    /// Reload the shell data from the fake (after a save).
    pub fn reload(&mut self, service: &FakeService) {
        self.data = data_from(service);
    }

    /// Click at `pos` over the next three frames (hover, press, release), like a
    /// real pointer.
    pub fn click_at(&mut self, pos: Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.pending.push_back(vec![egui::Event::PointerMoved(pos)]);
        self.pending
            .push_back(vec![egui::Event::PointerMoved(pos), button(true)]);
        self.pending.push_back(vec![button(false)]);
    }

    /// Drag with the primary button from `from` to `to` over the next frames
    /// (hover, press, a few moves, release).
    pub fn drag(&mut self, from: Pos2, to: Pos2) {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.pending
            .push_back(vec![egui::Event::PointerMoved(from)]);
        self.pending
            .push_back(vec![egui::Event::PointerMoved(from), button(from, true)]);
        for i in 1..=4 {
            let p = from + (to - from) * (i as f32 / 4.0);
            self.pending.push_back(vec![egui::Event::PointerMoved(p)]);
        }
        self.pending.push_back(vec![button(to, false)]);
    }

    /// Scroll the column under `pos` by a wheel delta in points.
    pub fn scroll_at(&mut self, pos: Pos2, delta: egui::Vec2) {
        self.pending.push_back(vec![egui::Event::PointerMoved(pos)]);
        self.pending.push_back(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Default::default(),
        }]);
    }

    /// Double click at `pos`.
    pub fn double_click_at(&mut self, pos: Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.pending.push_back(vec![egui::Event::PointerMoved(pos)]);
        self.pending
            .push_back(vec![egui::Event::PointerMoved(pos), button(true)]);
        self.pending.push_back(vec![button(false)]);
        self.pending.push_back(vec![button(true)]);
        self.pending.push_back(vec![button(false)]);
    }

    /// Draw frames until the queued input is used up, then two more; returns
    /// the last one.
    pub fn settle(
        &mut self,
        mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
    ) -> Frame {
        while !self.pending.is_empty() {
            self.frame(&mut show);
        }
        self.frames(2, show)
    }

    /// Draw one frame.
    pub fn frame(
        &mut self,
        mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
    ) -> Frame {
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(self.window[0], self.window[1]));
        let central = Rect::from_min_max(pos2(size::SIDEBAR, 0.0), screen.max);
        let page = layout::page_rect(central, self.fit);
        let events = self.pending.pop_front().unwrap_or_default();
        let mut frame = Frame {
            rect: page,
            ..Frame::default()
        };
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ui| {
                frame.passes += 1;
                let cx = Cx {
                    tr: &self.tr,
                    pal: Palette::of(ui.ctx()),
                    data: &self.data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &self.fonts,
                    service: &self.service,
                    log: &[],
                };
                let mut area = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(page)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                if self.fit {
                    let fit = layout::fit_area(&mut area, self.min_height, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        show(ui, &cx, &mut frame.actions);
                        ui.min_rect().max.y
                    });
                    frame.used_bottom = fit.inner;
                    frame.scrolled = fit.scrolled;
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("harness-scroll-page")
                        .auto_shrink([false, false])
                        .show(&mut area, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            show(ui, &cx, &mut frame.actions);
                        });
                }
            },
        );
        frame.outlines = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.stroke.width > 0.0 => {
                    Some((s.shape.visual_bounding_rect(), s.clip_rect))
                }
                _ => None,
            })
            .collect();
        frame.texts = output
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
        frame.clipped_texts = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                    s.clip_rect,
                )),
                _ => None,
            })
            .collect();
        frame.meshes = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Mesh(_) => Some(s.shape.visual_bounding_rect()),
                _ => None,
            })
            .collect();
        frame.elided = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.elided => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        frame.scroll_areas = self
            .ctx
            .data(|d| d.count::<egui::containers::scroll_area::State>());
        output.drop_without_applying_deltas();
        frame
    }

    /// Draw frames until `done` holds for one (at most 20 s); returns that frame.
    pub fn until(
        &mut self,
        mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
        done: impl Fn(&Frame) -> bool,
    ) -> Frame {
        let start = Instant::now();
        let mut times = Vec::new();
        loop {
            let t = Instant::now();
            let f = self.frame(&mut show);
            times.push(t.elapsed().as_millis());
            if done(&f) {
                return f;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "condition not reached after {} frames (slowest {} ms); last frame:\n{}",
                times.len(),
                times.iter().max().unwrap_or(&0),
                f.all_text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Find content below the ingress card by scrolling the left settings column.
    pub fn until_scrolled(
        &mut self,
        mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
        done: impl Fn(&Frame) -> bool,
    ) -> Frame {
        let mut frame = self.settle(&mut show);
        for _ in 0..24 {
            if done(&frame) {
                return frame;
            }
            self.scroll_at(
                pos2(frame.rect.min.x + 100.0, frame.rect.center().y),
                egui::vec2(0.0, -90.0),
            );
            frame = self.settle(&mut show);
        }
        panic!("content not reachable by scrolling: {}", frame.all_text());
    }

    /// Draw `n` frames, keeping the actions of all of them in the last one.
    pub fn frames(
        &mut self,
        n: usize,
        mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
    ) -> Frame {
        let mut actions = Vec::new();
        let mut last = Frame::default();
        for _ in 0..n {
            last = self.frame(&mut show);
            actions.append(&mut last.actions);
            std::thread::sleep(Duration::from_millis(2));
        }
        last.actions = actions;
        last
    }

    /// Click the center of the first drawn text `text` (press + release in the
    /// next frame).
    pub fn click_text(&mut self, frame: &Frame, text: &str) {
        let r = frame
            .find(text)
            .unwrap_or_else(|| panic!("no text {text:?} in:\n{}", frame.all_text()));
        self.click_at(r.center());
    }
}
