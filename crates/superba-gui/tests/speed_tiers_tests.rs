#![allow(missing_docs)]
//! Speed control (user bug 8): the Standard / Fast / Ultrafast selector paints its
//! track and selection inside its own rectangle for every selected tier, a mouse
//! click leaves no focus ring, and its keyboard focus ring is always whole (all
//! four edges inside the clip rectangle it is drawn with) — in a row exactly as
//! tall as the control and where the pages show it (the Models source strip and
//! the popover of the Overview's model node), in both themes and both locales.

mod support;

use std::sync::Arc;

use egui::{
    Align, Event, Key, Layout, PointerButton, Pos2, Rect, Shape, Theme, UiBuilder, pos2, vec2,
};
use superba_core::{
    AccountRequest, Locale, ModelsViewRequest, SpeedTier, SwitchService, Timestamp,
};
use superba_gui::design::layout;
use superba_gui::design::tokens::{focus, size};
use superba_gui::design::widgets::{SpeedTiers, TierText};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::models::{self as models_page, State as ModelsState};
use superba_gui::pages::{Action, Cx, Data, Page, PageLayout, overview};
use support::fake::{FakeOptions, FakeService};

/// Tolerance for strokes that end exactly on a clip edge.
const EPS: f32 = 0.01;

/// A headless window with Superba's fonts and style (focus-ring policy included).
struct Window {
    ctx: egui::Context,
    fonts: fonts::FontReport,
    screen: Rect,
}

/// What one frame painted: every shape (flattened) with its clip rectangle.
struct Drawn {
    shapes: Vec<(Rect, Shape)>,
}

impl Window {
    fn new(size: [f32; 2], theme: Theme) -> Self {
        let ctx = egui::Context::default();
        let fonts = fonts::FontReport::default();
        ctx.set_fonts(fonts::definitions(&fonts));
        style::install(&ctx, Some(theme));
        Self {
            ctx,
            fonts,
            screen: Rect::from_min_max(Pos2::ZERO, pos2(size[0], size[1])),
        }
    }

    fn pal(&self) -> &'static Palette {
        Palette::of(&self.ctx)
    }

    fn frame(&self, events: Vec<Event>, f: &mut dyn FnMut(&mut egui::Ui)) -> Drawn {
        let mut output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(self.screen),
                events,
                ..Default::default()
            },
            |ui| f(ui),
        );
        let clipped = std::mem::take(&mut output.shapes);
        output.drop_without_applying_deltas();
        let mut shapes = Vec::new();
        for c in clipped {
            flatten(c.clip_rect, c.shape, &mut shapes);
        }
        Drawn { shapes }
    }

    fn settle(&self, f: &mut dyn FnMut(&mut egui::Ui)) -> Drawn {
        self.frame(Vec::new(), f);
        self.frame(Vec::new(), f)
    }

    /// Press at `pos` in one frame, release in the next; returns the frame after.
    fn click(&self, pos: Pos2, f: &mut dyn FnMut(&mut egui::Ui)) -> Drawn {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![Event::PointerMoved(pos), button(true)], f);
        self.frame(vec![button(false)], f);
        // Off the control, so no hover state is left behind.
        self.frame(vec![Event::PointerMoved(pos2(1.0, 1.0))], f);
        self.settle(f)
    }

    /// Press `key`; returns the settled frame after it.
    fn key(&self, key: Key, modifiers: egui::Modifiers, f: &mut dyn FnMut(&mut egui::Ui)) -> Drawn {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.frame(vec![event(true)], f);
        self.frame(vec![event(false)], f);
        self.settle(f)
    }
}

fn flatten(clip: Rect, shape: Shape, out: &mut Vec<(Rect, Shape)>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(clip, s, out)),
        s => out.push((clip, s)),
    }
}

/// A focus ring: where its stroke paints and the clip it is painted with.
#[derive(Clone, Copy, Debug)]
struct Ring {
    painted: Rect,
    clip: Rect,
}

impl Ring {
    /// Every edge of the ring is inside its clip rectangle.
    fn whole(&self) -> bool {
        self.clip.expand(EPS).contains_rect(self.painted)
    }
}

impl Drawn {
    /// Every drawn text with its rectangle.
    fn texts(&self) -> Vec<(String, Rect)> {
        self.shapes
            .iter()
            .filter_map(|(_, s)| match s {
                Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                _ => None,
            })
            .collect()
    }

    /// Strokes in the focus colour (only focus rings use it for strokes).
    fn rings(&self, pal: &Palette) -> Vec<Ring> {
        self.shapes
            .iter()
            .filter_map(|(clip, s)| match s {
                Shape::Rect(r) if r.stroke.width > 0.0 && r.stroke.color == pal.focus => {
                    Some(Ring {
                        painted: r.visual_bounding_rect(),
                        clip: *clip,
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// The speed control's labels: the topmost `Ultrafast` and the `Fast` on its
    /// row (the Models table repeats tier names further down).
    fn speed_labels(&self) -> Option<(Rect, Rect)> {
        let texts = self.texts();
        let ultra = texts
            .iter()
            .filter(|(t, _)| t == "Ultrafast")
            .map(|(_, r)| *r)
            .min_by(|a, b| a.min.y.total_cmp(&b.min.y))?;
        let fast = texts
            .iter()
            .filter(|(t, r)| {
                t == "Fast"
                    && (r.center().y - ultra.center().y).abs() < 1.0
                    && r.max.x < ultra.min.x
            })
            .map(|(_, r)| *r)
            .max_by(|a, b| a.min.x.total_cmp(&b.min.x))?;
        Some((fast, ultra))
    }

    /// The ring around the speed control (it encloses both labels).
    fn speed_ring(&self, pal: &Palette) -> Option<Ring> {
        let (fast, ultra) = self.speed_labels()?;
        self.rings(pal)
            .into_iter()
            .find(|r| r.painted.contains(fast.center()) && r.painted.contains(ultra.center()))
    }
}

// --- the widget in a row exactly as tall as the control ----------------------------

/// The speed control in a `row_h` row clipped to exactly that row (the tightest
/// container: no room outside the row), right-aligned like the Models strip.
fn tight_row(
    tier: &mut SpeedTier,
    row_h: f32,
    response: &mut Option<Rect>,
) -> impl FnMut(&mut egui::Ui) {
    move |ui: &mut egui::Ui| {
        let row = Rect::from_min_size(pos2(40.0, 40.0), vec2(420.0, row_h));
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(row)
                .layout(Layout::right_to_left(Align::Center)),
        );
        child.set_clip_rect(row);
        let r = SpeedTiers::new(
            "tight-speed",
            tier,
            &[SpeedTier::Fast, SpeedTier::Ultrafast],
        )
        .text(
            SpeedTier::Standard,
            TierText {
                label: "Standard".into(),
                tooltip: None,
            },
        )
        .small()
        .show(&mut child);
        *response = Some(r.rect);
    }
}

#[test]
fn every_selected_tier_paints_inside_the_control() {
    for theme in [Theme::Dark, Theme::Light] {
        for selected in SpeedTier::ALL {
            let window = Window::new([600.0, 200.0], theme);
            let mut tier = selected;
            let mut rect = None;
            let drawn = window.settle(&mut tight_row(&mut tier, size::CONTROL, &mut rect));
            let rect = rect.unwrap();
            assert_eq!(rect.height(), size::CONTROL_SM);
            let mut painted = 0;
            for (clip, shape) in &drawn.shapes {
                let bounds = shape.visual_bounding_rect();
                if !bounds.is_positive() {
                    continue;
                }
                painted += 1;
                assert!(
                    rect.expand(EPS).contains_rect(bounds),
                    "{theme:?} {selected:?}: {shape:?} paints outside the control {rect:?}"
                );
                assert!(
                    clip.expand(EPS).contains_rect(bounds),
                    "{theme:?} {selected:?}: {shape:?} is clipped by {clip:?}"
                );
            }
            // Track, selection and three labels.
            assert!(painted >= 5, "{theme:?} {selected:?}: {painted} shapes");
            assert!(drawn.rings(window.pal()).is_empty(), "no focus, no ring");
        }
    }
}

#[test]
fn a_click_leaves_no_ring_and_the_keyboard_ring_is_whole_in_a_tight_row() {
    for theme in [Theme::Dark, Theme::Light] {
        let window = Window::new([600.0, 200.0], theme);
        let pal = window.pal();
        let mut tier = SpeedTier::Standard;
        let mut rect = None;
        let mut draw = tight_row(&mut tier, size::CONTROL_SM, &mut rect);
        let drawn = window.settle(&mut draw);
        let (fast, _) = drawn.speed_labels().expect("labels drawn");
        let drawn = window.click(fast.center(), &mut draw);
        assert!(
            drawn.rings(pal).is_empty(),
            "{theme:?}: a mouse click leaves no focus ring"
        );
        // The arrow keys keep working after the click and show the ring.
        let drawn = window.key(Key::ArrowRight, Default::default(), &mut draw);
        let ring = drawn
            .speed_ring(pal)
            .unwrap_or_else(|| panic!("{theme:?}: keyboard focus shows a ring"));
        assert!(ring.whole(), "{theme:?}: {ring:?}");
        drop(draw);
        let rect = rect.unwrap();
        assert!(
            rect.expand(EPS).contains_rect(ring.painted),
            "{theme:?}: no room outside the row, so the ring is inside the control: {ring:?} {rect:?}"
        );
        assert_eq!(
            tier,
            SpeedTier::Ultrafast,
            "the arrow key moved the selection"
        );
    }
}

// --- the real containers ---------------------------------------------------------

async fn data(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    // The balance (as from a site with billing) takes a column of the Overview.
    data.account
        .finish(service.account(AccountRequest::default()).await);
    data
}

/// Draw `page` the way `app::content` does: a Fit page in its page rectangle
/// (`layout::fit_area`), a Scroll page in the shell's scroll area.
fn page_frame(ui: &mut egui::Ui, page: Page, show: impl FnOnce(&mut egui::Ui)) {
    let fit = page.layout() == PageLayout::Fit;
    let full = Rect::from_min_max(pos2(size::SIDEBAR, 0.0), ui.max_rect().max);
    let rect = layout::page_rect(full, fit);
    if fit {
        let mut area = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        layout::fit_area(&mut area, layout::FIT_MIN_HEIGHT, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            show(ui);
        });
    } else {
        let mut area = ui.new_child(
            UiBuilder::new()
                .max_rect(full)
                .layout(Layout::top_down(Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("page")
            .auto_shrink([false, false])
            .show(&mut area, |ui| {
                ui.add_space(28.0);
                ui.horizontal(|ui| {
                    ui.add_space(rect.min.x - full.min.x);
                    ui.vertical(|ui| {
                        ui.set_width(rect.width());
                        ui.spacing_mut().item_spacing.y = 8.0;
                        show(ui);
                    });
                });
            });
    }
}

/// Click Fast (no ring), press an arrow key (ring), then Tab through the page
/// until the speed control has keyboard focus, and check that ring; with `room`
/// the page gives the control room for the outside ring.
fn check_page(window: &Window, page: &str, room: bool, draw: &mut dyn FnMut(&mut egui::Ui)) {
    let pal = window.pal();
    let drawn = window.settle(draw);
    let (fast, ultra) = drawn
        .speed_labels()
        .unwrap_or_else(|| panic!("{page}: speed control drawn"));
    let drawn = window.click(fast.center(), draw);
    assert!(
        drawn.rings(pal).is_empty(),
        "{page}: a mouse click leaves no focus ring: {:?}",
        drawn.rings(pal)
    );
    let drawn = window.key(Key::ArrowRight, Default::default(), draw);
    let ring = drawn
        .speed_ring(pal)
        .unwrap_or_else(|| panic!("{page}: an arrow key shows the ring"));
    assert!(ring.whole(), "{page}: ring cut by its clip: {ring:?}");
    // Keyboard only: drop the focus (Escape), then Tab through the page until
    // the control has it, as a keyboard user would.
    window.key(Key::Escape, Default::default(), draw);
    let (drawn, ring) = (0..60)
        .find_map(|_| {
            let drawn = window.key(Key::Tab, Default::default(), draw);
            let ring = drawn.speed_ring(pal)?;
            Some((drawn, ring))
        })
        .unwrap_or_else(|| panic!("{page}: Tab reaches the control and shows the ring"));
    assert!(ring.whole(), "{page}: ring cut by its clip: {ring:?}");
    assert!(
        ring.painted.contains_rect(fast) && ring.painted.contains_rect(ultra),
        "{page}: the ring surrounds the control: {ring:?}"
    );
    // The regular ring is outside the 28 px control with the 2 px gap; where the
    // container leaves no room for it, the shared policy draws it just inside.
    let outside = (ring.painted.height() - (size::CONTROL_SM + 2.0 * focus::BLEED)).abs() < 0.5;
    let inside = (ring.painted.height() - size::CONTROL_SM).abs() < 0.5;
    assert!(outside || inside, "{page}: {ring:?}");
    if room {
        assert!(
            outside,
            "{page}: the page leaves room for an outside ring: {ring:?}"
        );
    }
    // No other text runs under the ring's stroke.
    for (text, r) in drawn.texts() {
        if ring.painted.contains_rect(r) || !ring.painted.intersects(r) {
            continue;
        }
        panic!("{page}: the ring crosses the text {text:?} at {r:?}: {ring:?}");
    }
}

/// The Overview's speed control lives in the popover of the hero's model node:
/// open it with the chevron right of the model's name, click Fast (no ring),
/// press an arrow key (the whole ring, outside the control: the popover leaves
/// room for it, and no text runs under it).
fn check_popover(window: &Window, page: &str, model: &str, draw: &mut dyn FnMut(&mut egui::Ui)) {
    let pal = window.pal();
    let drawn = window.settle(draw);
    let title = drawn
        .texts()
        .into_iter()
        .find(|(t, _)| t.starts_with(model))
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("{page}: the model node is drawn"));
    let drawn = window.click(pos2(title.max.x + 12.0, title.center().y), draw);
    let (fast, ultra) = drawn
        .speed_labels()
        .unwrap_or_else(|| panic!("{page}: the popover shows the speed control"));
    let drawn = window.click(fast.center(), draw);
    assert!(
        drawn.rings(pal).is_empty(),
        "{page}: a mouse click leaves no focus ring: {:?}",
        drawn.rings(pal)
    );
    let drawn = window.key(Key::ArrowRight, Default::default(), draw);
    let ring = drawn
        .speed_ring(pal)
        .unwrap_or_else(|| panic!("{page}: an arrow key shows the ring"));
    assert!(ring.whole(), "{page}: ring cut by its clip: {ring:?}");
    assert!(
        ring.painted.contains_rect(fast) && ring.painted.contains_rect(ultra),
        "{page}: the ring surrounds the control: {ring:?}"
    );
    let outside = (ring.painted.height() - (size::CONTROL_SM + 2.0 * focus::BLEED)).abs() < 0.5;
    assert!(
        outside,
        "{page}: the popover leaves room for an outside ring: {ring:?}"
    );
    for (text, r) in drawn.texts() {
        if ring.painted.contains_rect(r) || !ring.painted.intersects(r) {
            continue;
        }
        panic!("{page}: the ring crosses the text {text:?} at {r:?}: {ring:?}");
    }
}

#[tokio::test]
async fn the_models_and_overview_pages_show_the_whole_ring() {
    let fake = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let mut data = data(&fake).await;
    let view = fake
        .models_view(ModelsViewRequest::default())
        .await
        .unwrap();
    assert!(
        view.speed.offered.contains(&SpeedTier::Ultrafast),
        "the fake list offers every tier: {:?}",
        view.speed.offered
    );
    data.active_models.finish(Ok(view.clone()));
    let service: Arc<dyn SwitchService> = fake.clone();
    for theme in [Theme::Dark, Theme::Light] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            // Models: the speed control is right-aligned in the source strip.
            let window = Window::new([1280.0, 800.0], theme);
            let mut state = ModelsState::default();
            state.set_price_table(None);
            state.set_view(view.clone());
            let mut out: Vec<Action> = Vec::new();
            let mut draw = |ui: &mut egui::Ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data: &data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &window.fonts,
                    service: &service,
                    log: &[],
                };
                page_frame(ui, Page::Models, |ui| {
                    models_page::show(ui, &cx, &mut state, &mut out)
                });
            };
            check_page(
                &window,
                &format!("models {theme:?} {locale:?}"),
                true,
                &mut draw,
            );

            // Overview: the profile's speed.
            let window = Window::new([1280.0, 800.0], theme);
            let mut state = overview::State::default();
            let mut out: Vec<Action> = Vec::new();
            let mut draw = |ui: &mut egui::Ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data: &data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &window.fonts,
                    service: &service,
                    log: &[],
                };
                page_frame(ui, Page::Overview, |ui| {
                    overview::show(ui, &cx, &mut state, &mut out)
                });
            };
            // The model node's popover leaves room for the outside ring.
            let model = data
                .overview
                .value
                .as_ref()
                .and_then(|o| o.config.as_ref())
                .and_then(|c| c.model.clone())
                .unwrap_or_else(|| tr.t("overview.hero.default_model"));
            check_popover(
                &window,
                &format!("overview {theme:?} {locale:?}"),
                &model,
                &mut draw,
            );
            assert!(
                out.iter().any(|a| matches!(
                    a,
                    Action::SetSpeed {
                        tier: SpeedTier::Fast,
                        ..
                    }
                )),
                "the click chose Fast: {out:?}"
            );
        }
    }
}
