#![allow(missing_docs)]
//! User bug 10: the Router & usage detail pane is sized to its content (the
//! title, the tabs and two rows of 通用 cards), every card is drawn whole —
//! border included — at 1280x800 and 1440x900 in both languages and themes, all
//! six tabs stay inside the pane, the split between the log and the pane drags
//! within limits (double click: content height again), and collapsing then
//! expanding the pane keeps its height.

mod router_harness;
mod support;

use std::sync::Arc;

use egui::{Rect, pos2};
use router_harness::{Frame, Harness};
use superba_core::Locale;
use superba_gui::design::tokens::size;
use superba_gui::design::widgets::log_table::ROW_HEIGHT;
use superba_gui::design::widgets::{detail_pane, info_card, toolbar};
use superba_gui::pages::router::{self, State};
use support::fake::{FakeOptions, FakeService};
use support::fit::SIZES;

/// The row the tests select (it has a cache key, a thread and an upstream id,
/// so every card has its most lines).
const ROW: i64 = 12;

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

struct Words {
    cards: [&'static str; 6],
    tabs: [&'static str; 2],
    collapse: &'static str,
    expand: &'static str,
    log_header: &'static str,
    totals: &'static str,
    copy: &'static str,
    /// Tabs that no longer exist (request content is never recorded).
    gone: [&'static str; 4],
}

fn words(locale: Locale) -> Words {
    match locale {
        Locale::ZhCn => Words {
            cards: ["请求", "用量", "速度", "路由", "会话", "费用"],
            tabs: ["通用", "计时"],
            collapse: "收起详情",
            expand: "展开详情",
            log_header: "输入",
            totals: "合计",
            copy: "复制",
            gone: ["请求头", "响应头", "请求数据", "响应数据"],
        },
        _ => Words {
            cards: ["Request", "Usage", "Speed", "Route", "Session", "Cost"],
            tabs: ["General", "Timing"],
            collapse: "Hide details",
            expand: "Show details",
            log_header: "Input",
            totals: "Total",
            copy: "Copy",
            gone: [
                "Request headers",
                "Response headers",
                "Request data",
                "Response data",
            ],
        },
    }
}

/// Where the text equal to `text` was drawn below `y` (the first one).
fn text_below(f: &Frame, text: &str, y: f32) -> Option<Rect> {
    f.texts
        .iter()
        .find(|(t, r)| t == text && r.min.y > y)
        .map(|(_, r)| *r)
}

/// The tab strip: where the first tab's label is.
fn first_tab(f: &Frame, w: &Words) -> Rect {
    // The label is drawn under the log (below the toolbar).
    let bar = f
        .find(w.collapse)
        .unwrap_or_else(|| panic!("no toolbar\n{}", f.all_text()));
    text_below(f, w.tabs[0], bar.max.y).unwrap_or_else(|| panic!("no tabs\n{}", f.all_text()))
}

/// The frames of the six 通用 cards, with their clip rectangles.
fn card_frames(f: &Frame, w: &Words) -> Vec<(Rect, Rect)> {
    let tabs = first_tab(f, w);
    w.cards
        .iter()
        .map(|title| {
            let t = text_below(f, title, tabs.max.y)
                .unwrap_or_else(|| panic!("no card {title}\n{}", f.all_text()));
            f.outlines
                .iter()
                .filter(|(r, _)| r.contains_rect(t))
                .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
                .copied()
                .unwrap_or_else(|| panic!("no frame around {title}"))
        })
        .collect()
}

fn select(h: &mut Harness, state: &mut State, w: &Words) -> Frame {
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, state, out),
        |f| f.has("Krill") || f.has("登录页样式"),
    );
    state.recent.selected = Some(ROW);
    // The pane is open with the 通用 cards (the 会话 card's copy links), then a
    // few more frames for the request's detail.
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, state, out),
        |f| f.has(w.tabs[1]) && f.texts.iter().any(|(t, _)| t == w.copy),
    );
    h.frames(4, |ui, cx, out| router::show_usage(ui, cx, state, out))
}

/// At both window sizes, in both languages and themes, the six cards are drawn
/// whole inside the pane: each frame (border included) lies inside the clip
/// rectangle it is drawn in and above the page's bottom, the pane needs no
/// scrolling, and every text of a card is inside its frame.
#[test]
fn the_general_cards_are_drawn_whole_at_both_sizes() {
    for size in SIZES {
        for locale in [Locale::ZhCn, Locale::En] {
            for dark in [false, true] {
                let w = words(locale);
                let mut h = Harness::new(size, locale, fake(), true);
                if dark {
                    h = h.dark();
                }
                let mut state = State::default();
                let f = select(&mut h, &mut state, &w);
                let ctx = format!("{size:?} {locale:?} dark={dark}");
                assert!(!f.scrolled && f.fits(), "{ctx}: the page scrolls");
                let frames = card_frames(&f, &w);
                for (i, (frame, clip)) in frames.iter().enumerate() {
                    assert!(
                        clip.expand(0.01).contains_rect(*frame),
                        "{ctx}: card {} {frame:?} is cut by {clip:?}",
                        w.cards[i]
                    );
                    assert!(
                        frame.max.y <= f.rect.max.y,
                        "{ctx}: card {} ends below the page",
                        w.cards[i]
                    );
                    // Laid out for its three lines, so nothing inside is cut.
                    assert!(
                        frame.height() >= info_card::height(3) - 0.5,
                        "{ctx}: card {} is {} px",
                        w.cards[i],
                        frame.height()
                    );
                    for (text, r) in &f.texts {
                        if frame.contains(r.center()) {
                            assert!(
                                r.min.y >= frame.min.y && r.max.y <= frame.max.y,
                                "{ctx}: {text:?} {r:?} sticks out of card {} {frame:?}",
                                w.cards[i]
                            );
                        }
                    }
                }
                // Two rows of three; the pane ends right under the second row (it
                // is sized to its cards, not to a fixed height).
                let bottom = frames.iter().map(|(r, _)| r.max.y).fold(0.0, f32::max);
                assert!(frames[3].0.min.y > frames[0].0.max.y, "{ctx}: one row");
                assert!(
                    f.rect.max.y - bottom <= 3.0,
                    "{ctx}: {} px under the cards",
                    f.rect.max.y - bottom
                );
                // No scroll area besides the log's and the source list's.
                assert!(
                    f.scroll_areas <= 2,
                    "{ctx}: {} scroll areas",
                    f.scroll_areas
                );
            }
        }
    }
}

/// Every tab draws inside the pane at both sizes, and the pane keeps its height
/// (the tab strip does not move) when the tab changes.
#[test]
fn every_tab_stays_inside_the_pane() {
    for size in SIZES {
        for locale in [Locale::ZhCn, Locale::En] {
            let w = words(locale);
            let service = fake();
            let mut h = Harness::new(size, locale, service, true);
            let mut state = State::default();
            let f = select(&mut h, &mut state, &w);
            let strip = first_tab(&f, &w);
            // Only 通用 and 计时: request content is never recorded.
            for gone in w.gone {
                // (响应头 is also the TTFB label of the 速度 card: only the strip counts.)
                assert!(
                    !f.texts
                        .iter()
                        .any(|(t, r)| t == gone && (r.center().y - strip.center().y).abs() < 2.0),
                    "{gone} is still a tab"
                );
            }
            for (row, tab) in [(ROW, 0), (ROW, 1), (11, 0), (11, 1)] {
                state.recent.selected = Some(row);
                state.recent.detail_tab = tab;
                let f = h.frames(3, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
                let ctx = format!("{size:?} {locale:?} row {row} tab {tab}");
                assert!(!f.scrolled && f.fits(), "{ctx}: drawn below the page");
                // (The label's width changes with its weight; its line does not.)
                assert!(
                    (first_tab(&f, &w).center().y - strip.center().y).abs() < 0.5,
                    "{ctx}: the pane moved"
                );
                for (r, clip) in &f.outlines {
                    if r.min.y > strip.max.y {
                        assert!(
                            clip.expand(0.01).contains_rect(*r),
                            "{ctx}: {r:?} is cut by {clip:?}"
                        );
                    }
                }
                for (text, r) in &f.texts {
                    assert!(
                        r.min.y < f.rect.max.y,
                        "{ctx}: {text:?} drawn under the page"
                    );
                }
            }
        }
    }
}

/// Where to grab the split: the gap between the toolbar and the pane.
fn split_y(f: &Frame, w: &Words) -> f32 {
    let bar = f.find(w.collapse).or_else(|| f.find(w.expand)).unwrap();
    bar.center().y + toolbar::HEIGHT / 2.0 + 3.0
}

#[test]
fn the_split_drags_within_limits_and_double_click_restores_the_content_height() {
    for size in SIZES {
        let w = words(Locale::ZhCn);
        let mut h = Harness::new(size, Locale::ZhCn, fake(), true);
        let mut state = State::default();
        let f = select(&mut h, &mut state, &w);
        let natural = first_tab(&f, &w).center().y;
        let x = f.find(w.collapse).unwrap().min.x - 200.0;
        let show = |h: &mut Harness, state: &mut State| {
            h.settle(|ui, cx, out| router::show_usage(ui, cx, state, out))
        };
        // Shrink first so even the smallest layout has room to grow by 60.
        let y = split_y(&f, &w);
        h.drag(pos2(x, y), pos2(x, y + 60.0));
        let f = show(&mut h, &mut state);
        let shrunk = first_tab(&f, &w).center().y;
        assert!((shrunk - natural - 60.0).abs() < 1.0, "{size:?}");
        let y = split_y(&f, &w);
        h.drag(pos2(x, y), pos2(x, y - 60.0));
        let f = show(&mut h, &mut state);
        assert!((first_tab(&f, &w).center().y - natural).abs() < 1.0);
        assert!(state.recent.detail_height.is_some());
        // Far down: the pane stops at its minimum (title, tabs, some body) and
        // the cards scroll inside it instead of being cut.
        let y = split_y(&f, &w);
        h.drag(pos2(x, y), pos2(x, size[1] - 2.0));
        let f = show(&mut h, &mut state);
        assert_eq!(state.recent.detail_height, Some(detail_pane::MIN_HEIGHT));
        assert!(f.fits() && !f.scrolled);
        assert!(f.scroll_areas >= 3, "the cards do not scroll");
        // Far up: the log keeps its header, three rows and the totals row.
        let y = split_y(&f, &w);
        h.drag(pos2(x, y), pos2(x, 2.0));
        let f = show(&mut h, &mut state);
        let header = f.find(w.log_header).unwrap();
        let totals = f.find(w.totals).unwrap();
        assert!(
            totals.center().y - header.center().y
                >= size::TABLE_HEADER / 2.0 + 3.0 * ROW_HEIGHT + ROW_HEIGHT / 2.0 - 1.0,
            "{size:?}: the log is squeezed to {}",
            totals.center().y - header.center().y
        );
        assert!(f.fits() && !f.scrolled);
        // Double click: back to the content height.
        let y = split_y(&f, &w);
        h.double_click_at(pos2(x, y));
        let f = show(&mut h, &mut state);
        assert_eq!(state.recent.detail_height, None);
        assert!((first_tab(&f, &w).center().y - natural).abs() < 0.5);
    }
}

#[test]
fn collapsing_and_expanding_keeps_the_dragged_height() {
    let w = words(Locale::ZhCn);
    let mut h = Harness::new(SIZES[0], Locale::ZhCn, fake(), true);
    let mut state = State::default();
    let f = select(&mut h, &mut state, &w);
    let x = f.find(w.collapse).unwrap().min.x - 200.0;
    let y = split_y(&f, &w);
    h.drag(pos2(x, y), pos2(x, y - 40.0));
    let f = h.settle(|ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    let dragged = state.recent.detail_height.expect("dragged");
    let strip = first_tab(&f, &w);
    h.click_text(&f, w.collapse);
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has(w.expand),
    );
    assert!(state.recent.detail_hidden);
    assert!(!f.has(w.tabs[1]), "the pane is still drawn");
    h.click_text(&f, w.expand);
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has(w.collapse) && f.has(w.tabs[1]),
    );
    assert_eq!(state.recent.detail_height, Some(dragged));
    assert_eq!(first_tab(&f, &w), strip);
}
