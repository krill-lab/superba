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

#[test]
fn simple_request_finish_time_uses_the_recorded_duration_and_keeps_unknowns_unknown() {
    use superba_gui::pages::simple_usage::completed_at;
    let now = Timestamp::from_millis(1_800_000_000_000);
    let mut r = FakeService::fixture_request_rows(now).remove(0);
    r.started_at = now;
    r.duration_ms = Some(4_567);
    assert_eq!(
        completed_at(&r),
        Some(Timestamp::from_millis(now.as_millis() + 4_567))
    );
    r.duration_ms = Some(0);
    assert_eq!(completed_at(&r), Some(now));
    r.duration_ms = None;
    assert_eq!(completed_at(&r), None);
}

#[test]
fn simple_sessions_explains_migration_in_a_clickable_popup_and_lists_completion_times() {
    use superba_gui::pages::simple_usage;
    for locale in [Locale::En, Locale::ZhCn] {
        let mut h =
            Harness::new([960.0, 640.0], locale, fake(), true).min_height(simple_usage::MIN_HEIGHT);
        let tr = Tr::new(locale);
        let mut state = simple_usage::State::default();
        let f = h.until(
            |ui, cx, out| simple_usage::show(ui, cx, &mut state, out),
            |f| f.has(&tr.t("simple.usage.completed")),
        );
        assert!(f.has(&tr.t("simple.sessions.what")));
        h.click_text(&f, &tr.t("simple.sessions.what"));
        let help = h.settle(|ui, cx, out| simple_usage::show(ui, cx, &mut state, out));
        assert!(
            help.has(&tr.t("simple.sessions.explanation")),
            "{}",
            help.all_text()
        );
        assert!(
            !help
                .actions
                .iter()
                .any(|a| matches!(a, Action::SessionBatch { .. }))
        );
    }
}

#[test]
fn simple_usage_queries_exactly_one_day_of_codex_and_plans_the_complete_session_batch() {
    use superba_core::{MigrationSelector, RequestHarness, SwitchIntent};
    use superba_gui::pages::simple_usage;
    let now = Timestamp::from_millis(1_800_000_000_000);
    let q = simple_usage::query(now, 0);
    assert_eq!(q.harnesses, [RequestHarness::Codex]);
    assert_eq!(
        q.from,
        Some(now.saturating_sub(std::time::Duration::from_secs(86_400)))
    );
    assert_eq!(q.to, Some(now));
    assert!(q.group.is_none() && q.member.is_none() && q.source.is_none());
    let request =
        superba_gui::app::planned_request(&Action::SessionBatch { rollback: false }).unwrap();
    let SwitchIntent::MigrateSessions { request: migration } = request.intent else {
        panic!("migration")
    };
    assert_eq!(migration.to_provider, "openai");
    assert!(migration.include_archived && migration.exclude.is_empty());
    assert_eq!(
        migration.selectors,
        [MigrationSelector::OtherProviders {
            except: "openai".into()
        }]
    );
    assert!(matches!(
        superba_gui::app::planned_request(&Action::SessionBatch { rollback: true })
            .unwrap()
            .intent,
        SwitchIntent::RollbackAllMigrations
    ));
}

#[test]
fn simple_usage_totals_and_rows_exclude_other_tools_and_older_requests() {
    use superba_core::RequestHarness;
    use superba_gui::pages::simple_usage;
    let service = fake();
    let now = Timestamp::now();
    let mut rows = FakeService::fixture_request_rows(now);
    rows.truncate(1);
    rows[0].harness = RequestHarness::Codex;
    rows[0].requested_model = Some("simple-codex-model".into());
    rows[0].model = Some("simple-codex-model".into());
    rows[0].response_model = None;
    rows[0].upstream_model = None;
    rows[0].started_at = now.saturating_sub(std::time::Duration::from_secs(60));
    let mut claude = rows[0].clone();
    claude.id += 100;
    claude.harness = RequestHarness::ClaudeCode;
    claude.requested_model = Some("hidden-other-tool".into());
    claude.model = Some("hidden-other-tool".into());
    let mut old = rows[0].clone();
    old.id += 200;
    old.requested_model = Some("hidden-old-model".into());
    old.model = Some("hidden-old-model".into());
    old.started_at = now.saturating_sub(std::time::Duration::from_secs(90_000));
    rows.extend([claude, old]);
    service.set_request_rows(rows);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = simple_usage::State::default();
    let frame = h.until(
        |ui, cx, out| simple_usage::show(ui, cx, &mut state, out),
        |f| f.has("simple-codex-model"),
    );
    let page = state.requests.value.as_ref().unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.totals.requests, 1);
    assert!(!frame.has("hidden-other-tool") && !frame.has("hidden-old-model"));
}

#[test]
fn simple_requests_page_in_thirties_or_fifties_and_clamp_after_refresh() {
    use superba_gui::pages::simple_usage::{State, visible_rows};
    let mut state = State::default();
    assert_eq!(visible_rows(&mut state, 121), 0..30);
    state.page = 2;
    assert_eq!(visible_rows(&mut state, 121), 60..90);
    state.per_page = 50;
    assert_eq!(visible_rows(&mut state, 121), 100..121);
    assert_eq!(visible_rows(&mut state, 31), 0..31);
    assert_eq!(visible_rows(&mut state, 0), 0..0);
    state.per_page = 99;
    assert_eq!(visible_rows(&mut state, 121), 0..30);
}

#[test]
fn simple_requests_render_only_the_selected_page() {
    use superba_core::RequestHarness;
    use superba_gui::pages::simple_usage;
    let service = fake();
    let now = Timestamp::now();
    let base = FakeService::fixture_request_rows(now).remove(0);
    let rows: Vec<_> = (0..81)
        .map(|i| {
            let mut r = base.clone();
            r.id = i;
            r.harness = RequestHarness::Codex;
            r.started_at = now.saturating_sub(std::time::Duration::from_secs(60 + i as u64));
            r.requested_model = Some(format!("page-model-{i:02}"));
            r.model = r.requested_model.clone();
            r.response_model = None;
            r.upstream_model = None;
            r
        })
        .collect();
    service.set_request_rows(rows);
    let mut h = Harness::new([1280.0, 1000.0], Locale::En, service, true);
    let mut state = simple_usage::State::default();
    let f = h.until(
        |ui, cx, out| simple_usage::show(ui, cx, &mut state, out),
        |f| f.has("1–30 / 81"),
    );
    assert!(!f.has("page-model-30"), "{}", f.all_text());
    state.page = 1;
    let f = h.settle(|ui, cx, out| simple_usage::show(ui, cx, &mut state, out));
    assert!(f.has("31–60 / 81"), "{}", f.all_text());
    assert!(f.has("page-model-30"));
    assert!(!f.has("page-model-00"));
    state.page = 0;
    state.per_page = 50;
    let f = h.settle(|ui, cx, out| simple_usage::show(ui, cx, &mut state, out));
    assert!(f.has("1–50 / 81"), "{}", f.all_text());
}

#[test]
fn simple_usage_sections_fit_and_batch_buttons_never_ask_for_individual_providers() {
    use superba_core::{MigrationReceipt, ProviderSessions, SwitchService};
    use superba_gui::pages::simple_usage;
    for (window, locale) in [
        ([960.0, 640.0], Locale::ZhCn),
        ([960.0, 640.0], Locale::En),
        ([1280.0, 800.0], Locale::ZhCn),
        ([1440.0, 900.0], Locale::En),
    ] {
        let service = fake();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (mut report, _) = rt.block_on(service.sessions()).unwrap();
        report.scanned_at = Timestamp::now();
        report.by_provider = BTreeMap::from([
            (
                "openai".into(),
                ProviderSessions {
                    distinct: 10,
                    ..Default::default()
                },
            ),
            (
                "provider-a".into(),
                ProviderSessions {
                    distinct: 3,
                    ..Default::default()
                },
            ),
            (
                "provider-b".into(),
                ProviderSessions {
                    distinct: 4,
                    ..Default::default()
                },
            ),
        ]);
        let receipt = MigrationReceipt {
            id: "batch-old".into(),
            codex_home: report.codex_home.clone(),
            migrated_at: Timestamp::now(),
            to_provider: "openai".into(),
            by_source_provider: BTreeMap::new(),
            threads: 1,
            rollouts: 1,
            backup_dir: Default::default(),
            ledger_path: Default::default(),
            rolled_back: false,
        };
        let mut h =
            Harness::new(window, locale, service, true).min_height(simple_usage::MIN_HEIGHT);
        h.data.sessions.finish(Ok((report, vec![receipt])));
        let tr = Tr::new(locale);
        let mut state = simple_usage::State::default();
        let f = h.until(
            |ui, cx, out| simple_usage::show(ui, cx, &mut state, out),
            |f| f.has(&tr.t("simple.usage.success")),
        );
        let stats = f.find(&tr.t("simple.usage.stats")).unwrap();
        let sessions = f.find(&tr.t("simple.sessions.title")).unwrap();
        let recent = f.find(&tr.t("simple.usage.recent")).unwrap();
        assert!(stats.max.y < sessions.min.y && sessions.max.y < recent.min.y);
        assert!(
            f.fits(),
            "{window:?} {locale:?}: bottom {}, page {:?}, stats {:?}, sessions {:?}, recent {:?}: {}",
            f.used_bottom,
            f.rect,
            stats,
            sessions,
            recent,
            f.all_text()
        );
        assert!(
            f.texts.iter().all(|(_, r)| r.max.x <= f.rect.max.x + 0.5),
            "horizontal overflow: {}",
            f.all_text()
        );
        assert!(f.has(&tr.f("simple.sessions.pending", &[("n", &7)])));
        for (key, rollback) in [
            ("simple.sessions.migrate", false),
            ("simple.sessions.rollback", true),
        ] {
            h.click_text(&f, &tr.t(key));
            let mut batches = Vec::new();
            h.settle(|ui, cx, out| {
                simple_usage::show(ui, cx, &mut state, out);
                batches.extend(out.iter().filter_map(|a| match a {
                    Action::SessionBatch { rollback } => Some(*rollback),
                    _ => None,
                }));
            });
            assert_eq!(batches, [rollback]);
        }
        h.data.sessions.value.as_mut().unwrap().1[0].rolled_back = true;
        let f = h.settle(|ui, cx, out| simple_usage::show(ui, cx, &mut state, out));
        assert!(!f.has(&tr.t("simple.sessions.rollback")));
    }
}

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
    assert_eq!(
        Tab::ALL,
        [Tab::Recent, Tab::Routing, Tab::Active, Tab::Stats]
    );
}

#[test]
fn the_router_page_lands_on_its_settings() {
    for (locale, settings, tabs) in [
        (Locale::ZhCn, "成员与线路顺序", ["最近请求", "活动请求"]),
        (
            Locale::En,
            "Members & line order",
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
            |f| f.has("Codex") && f.has(" %"),
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
        }
    }
}
