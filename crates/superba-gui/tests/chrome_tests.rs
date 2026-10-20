#![allow(missing_docs)]

//! The window frame without a title bar (user bug 85): where the window moves
//! from, where Superba's own window buttons sit (Windows, Linux), their names
//! for screen readers and UI Automation, and the resize edges (Linux). The
//! geometry follows `design::tokens::chrome` for the platform the tests run
//! on; the native parts (the Windows frame, the macOS traffic lights) are
//! checked on real machines.

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use egui::{Event, PointerButton, Rect, ViewportCommand, pos2};
use superba_core::Locale;
use superba_gui::chrome::{self, Buttons, buttons::WindowButton};
use superba_gui::design::backdrop;
use superba_gui::design::layout;
use superba_gui::design::tokens::{chrome as tokens, size};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use support::fake::{FakeOptions, FakeService};

const WINDOWS: [[f32; 2]; 3] = [size::MIN_WINDOW, [1280.0, 800.0], [1440.0, 900.0]];

fn screen(window: [f32; 2]) -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(window[0], window[1]))
}

fn central(window: [f32; 2]) -> Rect {
    Rect::from_min_max(pos2(size::SIDEBAR, 0.0), pos2(window[0], window[1]))
}

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

#[test]
fn the_card_floats_inside_the_window_and_holds_the_page() {
    for window in WINDOWS {
        let card = backdrop::panel_rect(central(window));
        assert_eq!(card.min.x, size::SIDEBAR, "flush with the sidebar");
        assert_eq!(card.max.x, window[0] - tokens::CARD_INSET);
        assert_eq!(card.max.y, window[1] - tokens::CARD_INSET);
        assert_eq!(card.min.y, tokens::CARD_TOP);
        for wide in [true, false] {
            let page = layout::page_rect(central(window), wide);
            assert!(card.contains_rect(page), "{window:?}: {page:?} in {card:?}");
            assert!(page.min.x - card.min.x >= tokens::CARD_PAD_NARROW);
            assert!(card.max.x - page.max.x >= tokens::CARD_PAD_NARROW);
            assert_eq!(page.min.y - card.min.y, tokens::CARD_PAD_TOP);
        }
    }
}

#[test]
fn the_empty_top_moves_the_window_and_nothing_below_it() {
    for window in WINDOWS {
        let [sidebar, strip] = chrome::drag_rects(screen(window));
        // The sidebar above its first row (the traffic lights on macOS).
        assert_eq!(sidebar.min, pos2(0.0, 0.0));
        assert_eq!(sidebar.max.x, size::SIDEBAR);
        assert!(sidebar.max.y <= tokens::NAV_TOP);
        assert!(
            sidebar.height() >= 28.0,
            "a comfortable target: {sidebar:?}"
        );
        // Across the content above the page header, left of the window
        // buttons.
        assert_eq!(strip.min, pos2(size::SIDEBAR, 0.0));
        assert_eq!(strip.max.x, window[0] - chrome::buttons_size().x);
        for (button, r) in chrome::buttons::rects(screen(window)) {
            assert!(!r.intersects(strip.shrink(0.5)), "{button:?}");
        }
        let page = layout::page_rect(central(window), true);
        assert!(strip.max.y <= page.min.y, "{strip:?} vs {page:?}");
        assert!(strip.height() >= 24.0);
    }
}

#[test]
fn window_buttons_sit_in_the_title_band_clear_of_the_card() {
    for window in WINDOWS {
        let rects = chrome::buttons::rects(screen(window));
        if tokens::BUTTONS == Buttons::TrafficLights {
            assert!(rects.is_empty(), "macOS draws its own");
            continue;
        }
        let order: Vec<_> = rects.iter().map(|(b, _)| *b).collect();
        assert_eq!(order, WindowButton::ALL, "minimize, maximize, close");
        let card = backdrop::panel_rect(central(window));
        for (button, r) in &rects {
            assert!(r.max.y <= tokens::BAND, "{button:?} in the band: {r:?}");
            assert!(!r.intersects(card.shrink(0.5)), "{button:?} over the card");
        }
        let (_, close) = rects[2];
        match tokens::BUTTONS {
            // Flush with the corner: a maximized window closes from the
            // screen's corner pixel.
            Buttons::Caption => {
                assert_eq!(close.max.x, window[0]);
                assert_eq!(close.min.y, 0.0);
                assert_eq!(close.size(), egui::vec2(46.0, 32.0));
            }
            Buttons::Round => {
                assert!(close.max.x < window[0] && close.min.y > 0.0);
                assert_eq!(close.width(), close.height());
            }
            Buttons::TrafficLights => unreachable!(),
        }
        for pair in rects.windows(2) {
            assert!(pair[0].1.max.x <= pair[1].1.min.x, "no overlap: {pair:?}");
        }
    }
}

#[test]
fn resize_edges_stay_on_the_window_margin() {
    for window in WINDOWS {
        let card = backdrop::panel_rect(central(window));
        // The sidebar's rows: from NAV_TOP down to the footer's hairline and
        // version line (23 px) above the card's bottom inset.
        let rows = Rect::from_min_max(
            pos2(size::NAV_INSET_LEFT, tokens::NAV_TOP),
            pos2(
                size::SIDEBAR - size::NAV_INSET_RIGHT,
                window[1] - tokens::CARD_INSET - 23.0,
            ),
        );
        let mut top = 0;
        for (direction, r) in chrome::resize_rects(screen(window)) {
            assert!(screen(window).contains_rect(r), "{direction:?}");
            assert!(
                !r.intersects(card.shrink(0.5)),
                "{direction:?} reaches the card: {r:?}"
            );
            assert!(
                !r.intersects(rows),
                "{direction:?} over the sidebar rows: {r:?}"
            );
            assert!(r.width().min(r.height()) <= tokens::RESIZE_CORNER);
            if r.min.y == 0.0 {
                top += 1;
            }
        }
        assert_eq!(top, 3, "two corners and the top edge");
    }
}

#[test]
fn window_buttons_are_named_buttons_that_minimize_maximize_and_close() {
    if tokens::BUTTONS == Buttons::TrafficLights {
        return;
    }
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(), locale, Page::Overview);
        let t = h.settle();
        for (button, name) in [
            (WindowButton::Minimize, "shell.window_minimize"),
            (WindowButton::Maximize, "shell.window_maximize"),
            (WindowButton::Close, "shell.window_close"),
        ] {
            assert_eq!(button.name(&tr, false), tr.t(name));
            let id = t.find(Some(Role::Button), &tr.t(name));
            assert!(id.is_some(), "{locale:?} {button:?}:\n{}", t.dump());
            // Not the dialogs' "Close" / the Overview's "Restore".
            assert_ne!(tr.t(name), tr.t("action.close"));
            assert_ne!(tr.t(name), tr.t("action.restore"));
        }
        assert_eq!(
            WindowButton::Maximize.name(&tr, true),
            tr.t("shell.window_restore")
        );
        h.take_commands();
        h.click(Some(Role::Button), &tr.t("shell.window_minimize"));
        assert!(
            h.take_commands()
                .contains(&ViewportCommand::Minimized(true))
        );
        h.click(Some(Role::Button), &tr.t("shell.window_maximize"));
        assert!(
            h.take_commands()
                .contains(&ViewportCommand::Maximized(true))
        );
        h.click(Some(Role::Button), &tr.t("shell.window_close"));
        assert!(h.take_commands().contains(&ViewportCommand::Close));
    }
}

fn press(pos: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}

#[test]
fn dragging_the_empty_top_moves_the_window_and_a_double_click_maximizes_it() {
    // Below the resize edge Linux has along the top.
    for at in [pos2(110.0, 20.0), pos2(700.0, 20.0)] {
        let mut h = AppHarness::new(fake(), Locale::En, Page::Providers);
        h.settle();
        h.take_commands();
        let moved = pos2(at.x + 40.0, at.y + 10.0);
        h.input(vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![Event::PointerMoved(pos2(at.x + 20.0, at.y + 5.0))],
            vec![Event::PointerMoved(moved)],
            vec![press(moved, false)],
        ]);
        assert!(
            h.take_commands().contains(&ViewportCommand::StartDrag),
            "a drag at {at:?} moves the window"
        );
        h.input(vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![press(at, false)],
            vec![press(at, true)],
            vec![press(at, false)],
        ]);
        let commands = h.take_commands();
        // macOS zooms through the window itself (no viewport command).
        assert!(
            cfg!(target_os = "macos") || commands.contains(&ViewportCommand::Maximized(true)),
            "a double click at {at:?} maximizes: {commands:?}"
        );
        // Windows, Linux: the first press hands the pointer to the move loop
        // (as a title bar does); the second one does not. macOS: the move
        // starts with the pointer's motion.
        let drags = commands
            .iter()
            .filter(|c| **c == ViewportCommand::StartDrag)
            .count();
        let want = if cfg!(target_os = "macos") { 0 } else { 1 };
        assert_eq!(drags, want, "{commands:?}");
    }
}

#[test]
fn the_window_still_moves_while_a_modal_dialog_is_open() {
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(fake(), Locale::En, Page::Providers);
    let t = h.click(Some(Role::Button), &tr.t("providers.add.button"));
    assert!(!t.with_role(Role::Dialog).is_empty(), "{}", t.dump());
    h.take_commands();
    let at = pos2(110.0, 20.0);
    let moved = pos2(160.0, 40.0);
    let t = h.input(vec![
        vec![Event::PointerMoved(at)],
        vec![press(at, true)],
        vec![Event::PointerMoved(pos2(130.0, 30.0))],
        vec![Event::PointerMoved(moved)],
        vec![press(moved, false)],
    ]);
    assert!(h.take_commands().contains(&ViewportCommand::StartDrag));
    // The dialog is still open (the press did not reach its backdrop).
    assert!(!t.with_role(Role::Dialog).is_empty(), "{}", t.dump());
}

#[test]
fn a_press_on_a_sidebar_row_or_the_page_never_moves_the_window() {
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(fake(), Locale::En, Page::Overview);
    let t = h.settle();
    // The first sidebar row, and the middle of the page.
    let row = t
        .find(Some(Role::Tab), &tr.t(Page::Overview.title_key()))
        .and_then(|id| t.node(id).bounds())
        .map(|b| pos2(b.x0 as f32 + 40.0, b.y0 as f32 + 10.0))
        .expect("the Overview tab");
    for at in [row, pos2(700.0, 400.0)] {
        h.take_commands();
        h.input(vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![Event::PointerMoved(pos2(at.x + 30.0, at.y + 30.0))],
            vec![press(pos2(at.x + 30.0, at.y + 30.0), false)],
        ]);
        assert!(
            !h.take_commands().contains(&ViewportCommand::StartDrag),
            "{at:?}"
        );
    }
}

#[test]
fn after_the_system_move_loop_ate_the_release_clicks_still_work() {
    // The system's move loop (macOS performWindowDrag, the Windows modal move
    // loop) consumes the mouse-up: egui never sees the release of the drag.
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(fake(), Locale::En, Page::Providers);
    h.settle();
    h.take_commands();
    let at = pos2(110.0, 20.0);
    h.input(vec![
        vec![Event::PointerMoved(at)],
        vec![press(at, true)],
        vec![Event::PointerMoved(pos2(130.0, 30.0))],
        vec![Event::PointerMoved(pos2(160.0, 40.0))],
    ]);
    assert!(h.take_commands().contains(&ViewportCommand::StartDrag));
    // A double click right after, elsewhere on the top (the window moved).
    let at2 = pos2(150.0, 30.0);
    h.input(vec![
        vec![Event::PointerMoved(at2)],
        vec![press(at2, true)],
        vec![press(at2, false)],
        vec![press(at2, true)],
        vec![press(at2, false)],
    ]);
    let commands = h.take_commands();
    if cfg!(target_os = "macos") {
        // The window's own zoom: no viewport command, and no move.
        assert!(
            !commands.contains(&ViewportCommand::StartDrag),
            "{commands:?}"
        );
    } else {
        assert!(
            commands.contains(&ViewportCommand::Maximized(true)),
            "{commands:?}"
        );
    }
    // Another drag, then a click on a sidebar row: it navigates.
    h.input(vec![
        vec![press(at, true)],
        vec![Event::PointerMoved(pos2(140.0, 30.0))],
        vec![Event::PointerMoved(pos2(170.0, 40.0))],
    ]);
    assert!(h.take_commands().contains(&ViewportCommand::StartDrag));
    let t = h.settle();
    let row = t
        .find(Some(Role::Tab), &tr.t(Page::Models.title_key()))
        .and_then(|id| t.node(id).bounds())
        .map(|b| pos2(b.x0 as f32 + 40.0, b.y0 as f32 + 10.0))
        .expect("the Models tab");
    let t = h.input(vec![
        vec![Event::PointerMoved(row)],
        vec![press(row, true)],
        vec![press(row, false)],
    ]);
    // The Models page shows its title (a heading besides the sidebar tab).
    let title = tr.t(Page::Models.title_key());
    assert!(
        t.find_all(None, &title)
            .iter()
            .any(|id| t.node(*id).role() != Role::Tab),
        "{}",
        t.dump()
    );
}

#[test]
fn a_press_on_a_window_edge_resizes_it_also_where_the_top_moves_it() {
    if tokens::BUTTONS != Buttons::Round {
        return;
    }
    let mut h = AppHarness::new(fake(), Locale::En, Page::Providers);
    h.settle();
    for (direction, rect) in chrome::resize_rects(screen([1280.0, 800.0])) {
        h.take_commands();
        let at = rect.center();
        h.input(vec![
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![press(at, false)],
        ]);
        let commands = h.take_commands();
        assert!(
            commands.contains(&ViewportCommand::BeginResize(direction)),
            "{direction:?} at {at:?}: {commands:?}"
        );
        assert!(
            !commands.contains(&ViewportCommand::StartDrag),
            "{direction:?}"
        );
    }
}

#[test]
fn sidebar_tabs_align_with_the_card_and_remain_visible_at_minimum_size() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for window in WINDOWS {
            let mut h = AppHarness::new(fake(), locale, Page::Overview);
            let tree = h.resize(window);
            let overview = tree
                .find(Some(Role::Tab), &tr.t(Page::Overview.title_key()))
                .unwrap();
            let first = tree.node(overview).bounds().unwrap();
            let expected_top = if tokens::BUTTONS == Buttons::TrafficLights {
                48.0
            } else {
                backdrop::panel_rect(central(window)).top()
            };
            assert!(
                (first.y0 - f64::from(expected_top)).abs() < 0.5,
                "{window:?}: {first:?}"
            );
            let drag = chrome::drag_rects(screen(window))[0];
            assert!(f64::from(drag.bottom()) <= first.y0);
            let mut previous = first.y0;
            // The tools' entries show only when installed (user request 114;
            // sidebar_tools_tests).
            for page in Page::ALL
                .into_iter()
                .filter(|p| !matches!(p, Page::Tool(_)))
            {
                let name = tr.t(page.sidebar_page().title_key());
                let id = tree
                    .find_all(Some(Role::Tab), &name)
                    .into_iter()
                    .find(|id| {
                        tree.node(*id)
                            .bounds()
                            .is_some_and(|r| r.x1 <= f64::from(size::SIDEBAR))
                    })
                    .unwrap_or_else(|| panic!("{name}: {}", tree.dump()));
                let node = tree.node(id);
                let rect = node.bounds().unwrap();
                assert!(node.supports_action(egui::accesskit::Action::Click));
                assert!(
                    rect.y0 >= previous && rect.y1 <= f64::from(window[1] - tokens::CARD_INSET),
                    "{name}: {rect:?}"
                );
                previous = rect.y1;
            }
            assert!(tree.unnamed().is_empty(), "{:?}", tree.unnamed());
        }
    }
}
