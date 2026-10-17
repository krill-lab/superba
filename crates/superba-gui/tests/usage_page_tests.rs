#![allow(missing_docs)]
//! The split of Router and Usage (user decision 21): 路由 is the router's
//! settings only and opens there; 用量 has 最近请求 / 活动请求 / 统计, works
//! without the router (rows from Codex's session files with "—" and a tooltip
//! in the router-only columns), explains 活动请求 while the router is off, and
//! has its own sidebar entry, deep link and palette entries.

mod router_harness;
mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use router_harness::Harness;
use superba_core::{Locale, RequestSource, Timestamp};
use superba_gui::i18n::Tr;
use superba_gui::pages::router::requests::{self, Col};
use superba_gui::pages::router::{self, State, Tab};
use superba_gui::pages::{Action, NavGroup, Page, PageLayout};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

#[test]
fn usage_is_its_own_page_next_to_the_router() {
    let at = |p| Page::ALL.iter().position(|x| *x == p).unwrap();
    assert_eq!(at(Page::Usage), at(Page::Router) + 1);
    assert_eq!(Page::Usage.group(), NavGroup::Connect);
    assert_eq!(Page::Usage.layout(), PageLayout::Fit);
    let zh = Tr::new(Locale::ZhCn);
    let en = Tr::new(Locale::En);
    assert_eq!(zh.t(Page::Usage.title_key()), "用量");
    assert_eq!(en.t(Page::Usage.title_key()), "Usage");
    assert_eq!(zh.t(Page::Router.title_key()), "路由");
    assert_eq!(en.t(Page::Router.title_key()), "Router");
    // Deep links: `--page usage`, `--page router`.
    assert_eq!(superba_gui::args::parse_page("usage"), Some(Page::Usage));
    assert_eq!(superba_gui::args::parse_page("router"), Some(Page::Router));
    assert_eq!(Tab::ALL, [Tab::Recent, Tab::Active, Tab::Stats]);
}

#[test]
fn the_router_page_lands_on_its_settings() {
    for (locale, settings, tabs) in [
        (Locale::ZhCn, "统计行", ["最近请求", "活动请求"]),
        (
            Locale::En,
            "Reply stats line",
            ["Recent requests", "Active requests"],
        ),
    ] {
        let mut h = Harness::new([1280.0, 800.0], locale, fake(), true);
        let mut state = State::default();
        // Whatever tab the Usage page had open.
        state.tab = Tab::Stats;
        let f = h.until(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            |f| f.has(settings),
        );
        for t in tabs {
            assert!(!f.has(t), "{t} on the Router page:\n{}", f.all_text());
        }
        assert!(f.fits(), "{locale:?}");
    }
}

#[test]
fn the_usage_page_has_three_tabs_and_switches_between_them() {
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, fake(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("登录页样式"),
    );
    for t in ["最近请求", "活动请求", "统计"] {
        assert!(f.has(t), "{t}:\n{}", f.all_text());
    }
    assert!(!f.has("路由设置 "), "{}", f.all_text());
    assert!(f.fits() && !f.scrolled);
    for (command, tab) in [
        ("tab:active", Tab::Active),
        ("tab:stats", Tab::Stats),
        ("tab:recent", Tab::Recent),
    ] {
        h.frame(|_, cx, out| router::usage_command(&mut state, cx, command, out));
        assert_eq!(state.tab, tab);
    }
    // The banner's button opens the Router page (its settings).
    let f = h.frames(2, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    h.click_text(&f, "打开路由");
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::Go(Page::Router)))
        },
    );
}

#[test]
fn active_requests_explain_that_they_need_the_router() {
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, fake(), true);
    let mut state = State::default();
    state.tab = Tab::Active;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("只有本地路由能看到正在进行的请求"),
    );
    assert!(f.has("最近请求和统计不需要路由"), "{}", f.all_text());
}

#[test]
fn rollout_rows_are_listed_with_dashes_in_the_router_only_columns() {
    let service = fake();
    let mut rows = service.request_rows();
    for r in rows.iter_mut().take(3) {
        r.source = RequestSource::Rollout;
        r.first_token_ms = None;
        r.tps_milli = None;
        r.line_id = None;
        r.line_name = None;
        r.upstream_host = None;
    }
    service.set_request_rows(rows.clone());
    let zh = Tr::new(Locale::ZhCn);
    let titles = BTreeMap::new();
    let now = Timestamp::now();
    for col in [Col::FirstToken, Col::Tps] {
        let c = requests::cell(&zh, &titles, &rows[0], col, now);
        assert_eq!(c.text, requests::DASH);
        assert_eq!(c.tooltip.as_deref(), Some("仅本地路由可提供"));
    }
    let c = requests::cell(&zh, &titles, &rows[0], Col::Provider, now);
    assert!(
        c.tooltip
            .as_deref()
            .is_some_and(|t| t.contains("线路仅本地路由可提供")),
        "{:?}",
        c.tooltip
    );
    // A router row keeps its numbers.
    let router_row = rows
        .iter()
        .find(|r| r.source == RequestSource::Router)
        .unwrap();
    let c = requests::cell(&zh, &titles, router_row, Col::FirstToken, now);
    assert!(c.tooltip.is_none());
    // CSV names the source of every row.
    let csv = requests::csv(&zh, &titles, &rows);
    assert!(csv.lines().next().unwrap().ends_with("来源"), "{csv}");
    assert!(csv.contains("Codex 会话记录"), "{csv}");
    assert!(csv.contains("本地路由"), "{csv}");
    // The detail pane of a rollout row says where it comes from.
    let mut h = Harness::new([1440.0, 900.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("登录页样式"),
    );
    state.recent.selected = Some(rows[0].id);
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("Codex 会话记录"),
    );
    assert!(f.has("仅本地路由可提供"), "{}", f.all_text());
}

#[test]
fn palette_entries_open_the_usage_tabs() {
    let mut h = Harness::new([1280.0, 800.0], Locale::En, fake(), true);
    let state = State::default();
    h.frame(|_, cx, _| {
        let items = router::palette(cx, &state);
        let find = |title: &str| {
            items
                .iter()
                .find(|i| i.title == title)
                .unwrap_or_else(|| panic!("{title}"))
        };
        for (title, command) in [
            ("Open recent requests", "tab:recent"),
            ("Open active requests", "tab:active"),
            ("Open usage statistics", "tab:stats"),
        ] {
            match &find(title).action {
                Action::PageCommand { page, command: c } => {
                    assert_eq!(*page, Page::Usage);
                    assert_eq!(c, command);
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            &find("Turn the stats line on").action,
            Action::PageCommand {
                page: Page::Router,
                ..
            }
        ));
    });
}

#[test]
fn without_a_router_clearing_and_pausing_are_off() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("登录页样式"),
    );
    // No 记录请求内容 toggle: request content is never recorded.
    assert!(!f.has("记录请求内容"), "{}", f.all_text());
    h.click_text(&f, "清空");
    h.click_text(&f, "暂停记录");
    h.frames(4, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    assert!(state.dialog.is_none(), "{:?}", state.dialog);
    assert!(service.saved_stats().is_empty());
}

#[test]
fn the_log_fits_every_window_size_without_cut_filters_or_elided_numbers() {
    // Review ux-visual #13 / #3: at 960x640 digits were elided ("18,…"), the
    // "App + CLI" filter was cut at the right edge and the status read
    // "faile…"; at 1440 the extra width went to gaps.
    for (window, locale) in [
        ([960.0, 640.0], Locale::ZhCn),
        ([960.0, 640.0], Locale::En),
        ([1280.0, 800.0], Locale::ZhCn),
        ([1280.0, 800.0], Locale::En),
        ([1440.0, 900.0], Locale::ZhCn),
        ([1440.0, 900.0], Locale::En),
    ] {
        let tr = Tr::new(locale);
        let mut h = Harness::new(window, locale, fake(), true);
        let mut state = State::default();
        let f = h.until(
            |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
            |f| f.has("Krill") && f.has(" %"),
        );
        let label = format!("{window:?} {locale:?}");
        let right = f.rect.max.x + 0.5;
        let beyond: Vec<&str> = f
            .texts
            .iter()
            .filter(|(_, r)| r.max.x > right)
            .map(|(t, _)| t.as_str())
            .collect();
        assert!(beyond.is_empty(), "{label}: past the page edge: {beyond:?}");
        let client = tr.t("requests.filter.client.all");
        assert!(f.has(&client), "{label}");
        assert!(!f.elided.contains(&client), "{label}: {client} cut");
        // Numbers are never elided: a narrow table hides columns instead.
        let number = |t: &str| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_digit() || ",.≈<sm ".contains(c))
        };
        let cut: Vec<&String> = f.elided.iter().filter(|t| number(t)).collect();
        assert!(cut.is_empty(), "{label}: elided numbers {cut:?}");
        // No status shows a wire code.
        assert!(!f.has("failed:"), "{label}");
        let hidden = [
            tr.t("requests.col.cache_write"),
            tr.t("requests.col.cache_key"),
        ];
        if window[0] < 1000.0 {
            for h in &hidden {
                assert!(!f.texts.iter().any(|(t, _)| t == h), "{label}: {h} shown");
            }
        } else {
            for h in &hidden {
                assert!(f.texts.iter().any(|(t, _)| t == h), "{label}: {h} missing");
            }
        }
    }
}
