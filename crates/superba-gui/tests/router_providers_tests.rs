#![allow(missing_docs)]
//! Router & usage grouped by provider (user report 0.3.0-alpha.1, item 11): the
//! source list's second grouping is 供应商 / "Providers" — one entry per provider
//! (Krill, each custom site) with the current profile name, a provider's lines
//! as indented sub-entries with their own counts — and the statistics tab
//! totals each provider with its lines under it.

mod router_harness;
mod support;

use std::sync::Arc;

use egui::Rect;
use router_harness::{Frame, Harness};
use superba_core::{Locale, RequestGrouping, StatsGroupBy, provider_line_key};
use superba_gui::pages::router::{self, State, Tab};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

/// The leftmost drawn text `text` (the source list is the left column).
fn leftmost(f: &Frame, text: &str) -> Rect {
    f.texts
        .iter()
        .filter(|(t, _)| t == text)
        .map(|(_, r)| *r)
        .min_by(|a, b| a.min.x.total_cmp(&b.min.x))
        .unwrap_or_else(|| panic!("no {text:?} in:\n{}", f.all_text()))
}

/// The count drawn on the row of `label` in the source list.
fn count_of(f: &Frame, label: &str) -> String {
    let row = leftmost(f, label);
    f.texts
        .iter()
        .filter(|(t, r)| {
            (r.center().y - row.center().y).abs() < 4.0
                && r.min.x > row.max.x
                && r.max.x < row.min.x + 200.0
                && t.chars().all(|c| c.is_ascii_digit() || c == ',')
        })
        .map(|(t, _)| t.clone())
        .next()
        .unwrap_or_else(|| panic!("no count next to {label:?}"))
}

#[test]
fn the_source_list_groups_by_provider_with_krills_lines_under_it() {
    // The fixture rows store Chinese line names (the router stores the English
    // ones, which resolve to the UI language): line entries keep them in English.
    for (locale, tab, all, overseas) in [
        (Locale::ZhCn, "供应商", "全部", "海外线路"),
        (Locale::En, "Providers", "All", "海外线路"),
    ] {
        let mut h = Harness::new([1280.0, 800.0], locale, fake(), true);
        let mut state = State::default();
        let f = h.until(
            |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
            |f| f.has("登录页样式"),
        );
        h.click_at(leftmost(&f, tab).center());
        let f = h.until(
            |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
            |f| f.has("My API") && f.texts.iter().any(|(t, _)| t == "Krill"),
        );
        assert_eq!(
            state.recent.grouping,
            RequestGrouping::Provider,
            "{locale:?}"
        );
        // "All" first, then Krill with its two lines indented under it, then the
        // custom site (one line: no sub-entry).
        let all_r = leftmost(&f, all);
        let krill = leftmost(&f, "Krill");
        let over = leftmost(&f, overseas);
        let cn = leftmost(&f, "国内极速");
        let api = leftmost(&f, "My API");
        assert!(all_r.min.y < krill.min.y, "{locale:?}: All comes first");
        assert!(krill.min.y < over.min.y && over.min.y < cn.min.y && cn.min.y < api.min.y);
        assert!(
            over.min.x > krill.min.x + 8.0,
            "{locale:?}: lines are indented"
        );
        assert_eq!(api.min.x, krill.min.x);
        // The column starts 6 px left of the labels and is 150 px wide here.
        let column_end = all_r.min.x - 6.0 + 150.0;
        assert!(
            api.max.x < column_end && cn.max.x < column_end,
            "inside the column"
        );
        assert_eq!(count_of(&f, all), "8");
        assert_eq!(count_of(&f, "Krill"), "7");
        assert_eq!(count_of(&f, overseas), "6");
        assert_eq!(count_of(&f, "国内极速"), "1");
        assert_eq!(count_of(&f, "My API"), "1");
        assert!(!f.has("api.example.com · "), "the host is no group");
        // Selecting a line filters the log to its rows.
        h.click_at(cn.center());
        let mut f = h.frames(3, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
        assert_eq!(
            state.recent.group.as_deref(),
            Some(provider_line_key("krill", "国内极速").as_str())
        );
        for _ in 0..2000 {
            if state.recent.shown.len() == 1 {
                break;
            }
            f = h.frames(1, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(state.recent.shown.len(), 1, "{locale:?}");
        assert_eq!(state.recent.shown[0].id, 11);
        assert!(f.fits() && !f.scrolled, "{locale:?}");
    }
}

#[test]
fn statistics_total_each_provider_with_its_lines_under_it() {
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, fake(), true);
    let mut state = State::default();
    state.tab = Tab::Stats;
    state.stats.by = StatsGroupBy::Provider;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("My API") && f.has("海外线路"),
    );
    let krill = leftmost(&f, "Krill");
    let over = leftmost(&f, "海外线路");
    let cn = leftmost(&f, "国内极速");
    let api = leftmost(&f, "My API");
    assert!(
        krill.min.y < over.min.y.min(cn.min.y),
        "lines follow their provider"
    );
    assert!(over.min.y.max(cn.min.y) < api.min.y);
    assert!(over.min.x > krill.min.x + 8.0, "line rows are indented");
    assert_eq!(api.min.x, krill.min.x, "a single line gets no row");
    assert!(f.has("供应商"));
    assert!(f.fits() && !f.scrolled);
}
