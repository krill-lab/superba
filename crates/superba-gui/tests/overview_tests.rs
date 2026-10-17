#![allow(missing_docs)]
//! The Overview (user request 27): the data mapping of each card (today's cost,
//! delta and token split, the 7 × 24 heat-strip, recent sessions, line traces,
//! models without a price), the to-do list and the hero's nodes (router on and
//! off, problem markers, clicks and popovers), tidy empty states and the fit at
//! 1280x800 and 1440x900.

mod support;

use egui::Rect;
use serde_json::json;
use superba_core::{
    DoctorFinding, LineCaps, Locale, LocalizedText, RequestClient, RequestRow, RequestTotals,
    RouterLineHealth, RouterLineView, RouterMemberView, RouterView, RouterWatchState, Severity,
    SwitchService, Timestamp, Usd, VersionsView,
};
use superba_gui::design::Palette;
use superba_gui::design::widgets::Status;
use superba_gui::i18n::Tr;
use superba_gui::pages::overview::hero::{self};
use superba_gui::pages::overview::todo::{self, Spot, TodoKind};
use superba_gui::pages::overview::usage::{self, Delta, Measure, Snapshot, TodayView};
use superba_gui::pages::overview::{self, UsageLoader};
use superba_gui::pages::{Action, Cx, Data, Page};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

/// 2026-10-17 11:30 at UTC+8 (03:30 UTC).
fn now() -> Timestamp {
    Timestamp(Timestamp::from_utc_date(2026, 10, 17).unwrap().as_millis() + 3 * HOUR + 30 * MIN)
}

const OFFSET: i64 = 8 * 3600;

#[allow(clippy::too_many_arguments)]
fn row(
    id: i64,
    started: Timestamp,
    session: &str,
    model: &str,
    tokens: (u64, u64, u64),
    cost: Option<i64>,
    line: Option<&str>,
    ttfb: Option<u64>,
) -> RequestRow {
    serde_json::from_value(json!({
        "id": id,
        "started_at": started.as_millis(),
        "member_id": "krill",
        "member_name": "Krill",
        "line_id": line,
        "prompt_cache_key": session,
        "model": model,
        "input_tokens": tokens.0,
        "cached_tokens": tokens.1,
        "output_tokens": tokens.2,
        "ttfb_ms": ttfb,
        "duration_ms": 4_000,
        // Dollars on the wire.
        "cost": cost.map(|c| c as f64 / 1e6),
        "client": "desktop",
        "source": if id < 0 { "rollout" } else { "router" },
    }))
    .expect("row")
}

fn at(ms: i64) -> Timestamp {
    Timestamp(now().as_millis() + ms)
}

// --- Today ------------------------------------------------------------------------

fn totals(requests: u64, input: u64, cached: u64, output: u64, cost: i64) -> RequestTotals {
    RequestTotals {
        requests,
        ok: requests,
        input_tokens: input,
        cached_tokens: cached,
        output_tokens: output,
        cost: Usd::from_micros(cost),
        ..RequestTotals::default()
    }
}

#[test]
fn today_compares_with_yesterday_by_this_time_and_splits_its_tokens() {
    let today = totals(30, 10_000, 8_000, 2_000, 3_000_000);
    let same = totals(20, 6_000, 3_000, 1_000, 2_000_000);
    let full = totals(80, 30_000, 20_000, 5_000, 9_000_000);
    let month = totals(900, 1, 0, 1, 120_000_000);
    let v = TodayView::of(&today, &same, &full, &month);
    assert_eq!(v.measure, Measure::Cost);
    assert_eq!(v.value, 3_000_000);
    assert_eq!(v.delta, Delta::Percent(50.0));
    assert_eq!(
        (v.yesterday_same, v.yesterday, v.month),
        (2_000_000, 9_000_000, 120_000_000)
    );
    assert_eq!((v.cached, v.uncached, v.output), (8_000, 2_000, 2_000));
    assert_eq!(v.split(), [8.0 / 12.0, 2.0 / 12.0, 2.0 / 12.0]);
    assert_eq!(v.cache_rate(), Some(0.8));
    assert_eq!(v.requests, 30);
    assert_eq!(overview::delta_text(v.delta).as_deref(), Some("50%"));
    assert_eq!(
        overview::figure(v.measure, v.value, "tokens").text(),
        "$3.00"
    );

    // Nothing priced: tokens are the measure (and the delta compares tokens).
    let t = |r, i, o| totals(r, i, 0, o, 0);
    let v = TodayView::of(
        &t(3, 900, 100),
        &t(1, 400, 100),
        &t(1, 400, 100),
        &t(4, 1300, 200),
    );
    assert_eq!(v.measure, Measure::Tokens);
    assert_eq!(v.value, 1_000);
    assert_eq!(v.delta, Delta::Percent(100.0));
    assert_eq!(
        overview::figure(v.measure, 12_345, "tokens").text(),
        "12.3 K tokens"
    );

    // Tidy edges: nothing today or yesterday, usage only today, no tokens.
    let zero = RequestTotals::default();
    let v = TodayView::of(&zero, &zero, &zero, &zero);
    assert!(v.is_empty());
    assert_eq!(v.delta, Delta::None);
    assert_eq!(v.split(), [0.0; 3]);
    assert_eq!(v.cache_rate(), None);
    assert_eq!(overview::delta_text(v.delta), None);
    assert_eq!(usage::delta(5, 0), Delta::New);
    assert_eq!(
        overview::delta_text(Delta::Percent(-0.4)).as_deref(),
        Some("<1%")
    );
    assert_eq!(
        overview::delta_text(Delta::Percent(-30.0)).as_deref(),
        Some("30%")
    );
    assert_eq!(
        overview::delta_text(Delta::Percent(2_400.0)).as_deref(),
        Some("25x")
    );
}

#[test]
fn day_and_month_start_follow_the_local_offset() {
    let (day, month) = usage::day_and_month_start(now(), OFFSET);
    assert_eq!(
        day.as_millis(),
        Timestamp::from_utc_date(2026, 10, 17).unwrap().as_millis() - 8 * HOUR
    );
    assert_eq!(
        month.as_millis(),
        Timestamp::from_utc_date(2026, 10, 1).unwrap().as_millis() - 8 * HOUR
    );
    // At UTC-5 it is still the 16th.
    let (day, _) = usage::day_and_month_start(now(), -5 * 3600);
    assert_eq!(
        day.as_millis(),
        Timestamp::from_utc_date(2026, 10, 16).unwrap().as_millis() + 5 * HOUR
    );
}

// --- Heat-strip -------------------------------------------------------------------

#[test]
fn the_heat_strip_buckets_rows_by_local_day_and_hour() {
    let (midnight, _) = usage::day_and_month_start(now(), OFFSET);
    let local = |days_ago: i64, hour: i64| {
        Timestamp(midnight.as_millis() - days_ago * DAY + hour * HOUR + 5 * MIN)
    };
    let rows = vec![
        // Today 09:05 and 11:05 (local).
        row(
            1,
            local(0, 9),
            "a",
            "gpt-5.5",
            (1_000, 0, 100),
            Some(10),
            None,
            None,
        ),
        row(
            2,
            local(0, 9),
            "a",
            "gpt-5.5",
            (2_000, 0, 200),
            Some(20),
            None,
            None,
        ),
        row(
            3,
            local(0, 11),
            "a",
            "gpt-5.5",
            (500, 0, 50),
            None,
            None,
            None,
        ),
        // Six days ago 23:05, seven days ago (outside), in the future (outside).
        row(
            4,
            local(6, 23),
            "b",
            "gpt-5.5",
            (8_000, 0, 800),
            Some(5),
            None,
            None,
        ),
        row(
            5,
            local(7, 12),
            "c",
            "gpt-5.5",
            (9_000, 0, 900),
            Some(5),
            None,
            None,
        ),
        row(
            6,
            at(30 * MIN),
            "c",
            "gpt-5.5",
            (9_000, 0, 900),
            Some(5),
            None,
            None,
        ),
    ];
    let heat = usage::heat(&rows, now(), OFFSET);
    assert_eq!(heat.cells.len(), usage::DAYS);
    assert_eq!(heat.days.len(), usage::DAYS);
    assert_eq!(heat.days[6], midnight);
    assert_eq!(heat.now_hour, 11);
    let today = heat.cells[6];
    assert_eq!(today[9].requests, 2);
    assert_eq!(today[9].tokens, 3_300);
    assert_eq!(today[9].cost_micros, 30);
    assert_eq!(today[11].requests, 1);
    assert_eq!(heat.cells[0][23].tokens, 8_800);
    let total: u32 = heat
        .cells
        .iter()
        .flat_map(|d| d.iter())
        .map(|c| c.requests)
        .sum();
    assert_eq!(
        total, 4,
        "outside the seven days and in the future: left out"
    );
    // Shades: the busiest hour is 4, a light one still shows, none is 0.
    assert_eq!(heat.max_tokens(), 8_800);
    assert_eq!(heat.level(&heat.cells[0][23]), 4);
    assert!((1..4).contains(&heat.level(&today[11])));
    assert_eq!(heat.level(&today[0]), 0);
    // 2026-10-17 is a Saturday; six days earlier a Sunday (0 = Monday).
    assert_eq!(heat.weekday(6, OFFSET), 5);
    assert_eq!(heat.weekday(0, OFFSET), 6);
    assert!(!heat.is_empty());
    assert!(usage::heat(&[], now(), OFFSET).is_empty());
}

// --- Recent sessions --------------------------------------------------------------

#[test]
fn recent_sessions_are_the_newest_with_their_tokens_model_client_and_cost() {
    let mut rows = vec![
        row(
            1,
            at(-3 * HOUR),
            "old",
            "gpt-5.5",
            (1_000, 0, 100),
            Some(1_000_000),
            None,
            None,
        ),
        row(
            2,
            at(-20 * MIN),
            "busy",
            "gpt-5.5",
            (2_000, 0, 200),
            Some(2_000_000),
            None,
            None,
        ),
        row(
            3,
            at(-5 * MIN),
            "busy",
            "gpt-6-astra",
            (3_000, 0, 300),
            Some(500_000),
            None,
            None,
        ),
        row(
            4,
            at(-10 * MIN),
            "free",
            "gpt-x",
            (4_000, 0, 400),
            None,
            None,
            None,
        ),
        row(
            5,
            at(-2 * DAY),
            "older",
            "gpt-5.5",
            (10, 0, 10),
            Some(1),
            None,
            None,
        ),
    ];
    rows[2].client = RequestClient::Tui;
    let s = usage::recent_sessions(&rows, 3);
    assert_eq!(
        s.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
        ["busy", "free", "old"],
        "newest last request first, at most n"
    );
    let busy = &s[0];
    assert_eq!(busy.requests, 2);
    assert_eq!(busy.tokens, 5_500);
    assert_eq!(busy.cost_micros, Some(2_500_000));
    assert!(!busy.unpriced);
    assert_eq!(
        busy.model.as_deref(),
        Some("gpt-6-astra"),
        "the newest request's model"
    );
    assert_eq!(busy.client, RequestClient::Tui);
    assert_eq!(busy.last, at(-5 * MIN + 4_000), "the newest request's end");
    let free = &s[1];
    assert_eq!(free.cost_micros, None);
    assert!(free.unpriced);
    assert_eq!(free.label(), "free", "no title: the short key");
    let mut titled = s[2].clone();
    titled.title = Some("Refactor the loader".into());
    assert_eq!(titled.label(), "Refactor the loader");
    assert_eq!(usage::short_key("subagent:019a2f10-5b1c"), "019a2f10");
    assert!(usage::recent_sessions(&[], 5).is_empty());
}

// --- Line traces and prices ------------------------------------------------------------

#[test]
fn line_traces_take_the_router_rows_of_the_last_30_minutes() {
    let ok = |id: i64, ago: i64, line: &str, ttfb: u64| {
        row(
            id,
            at(-ago),
            "s",
            "gpt-5.5",
            (10, 0, 10),
            Some(1),
            Some(line),
            Some(ttfb),
        )
    };
    let mut failed = ok(5, 3 * MIN, "overseas", 0);
    failed.status = superba_core::RequestStatus::Failed {
        code: Some("upstream_unavailable".into()),
    };
    let rows = vec![
        ok(1, 30_000, "overseas", 600),
        ok(2, 40_000, "overseas", 400),
        ok(3, 50_000, "overseas", 900),
        ok(4, 12 * MIN, "cn-fast", 300),
        failed,
        // Too old, and a rollout row (no line timing).
        ok(6, 31 * MIN, "overseas", 50),
        ok(-7, MIN, "overseas", 70),
    ];
    let traces = usage::line_traces(&rows, now());
    let o = &traces[&("krill".to_owned(), "overseas".to_owned())];
    assert_eq!(o.points.len(), usage::TRACE_MINUTES);
    assert_eq!(
        o.points[usage::TRACE_MINUTES - 1],
        Some(600),
        "median of the last minute"
    );
    assert_eq!(o.samples, 3);
    assert_eq!(o.errors, 1);
    assert_eq!(o.median_ms, Some(600));
    let c = &traces[&("krill".to_owned(), "cn-fast".to_owned())];
    assert_eq!(c.points[usage::TRACE_MINUTES - 13], Some(300));
    assert_eq!(c.points.iter().flatten().count(), 1);
    assert_eq!(traces.len(), 2);
    assert!(usage::line_traces(&[], now()).is_empty());
}

#[test]
fn models_without_a_price_are_listed_by_tokens() {
    let rows = vec![
        row(
            1,
            at(-MIN),
            "a",
            "gpt-5.5",
            (10, 0, 10),
            Some(5),
            None,
            None,
        ),
        row(2, at(-MIN), "a", "spark", (10, 0, 10), None, None, None),
        row(3, at(-MIN), "a", "mini", (100, 0, 10), None, None, None),
        row(4, at(-MIN), "a", "mini", (100, 0, 10), None, None, None),
    ];
    assert_eq!(usage::unpriced_models(&rows), ["mini", "spark"]);
}

// --- Loading through the service ------------------------------------------------------------

#[tokio::test]
async fn a_snapshot_reads_the_week_the_totals_the_router_and_titles() {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    let now = Timestamp::now();
    let rows = vec![
        row(
            1,
            Timestamp(now.as_millis() - MIN),
            "019a-new",
            "gpt-5.5",
            (100, 50, 10),
            Some(7),
            Some("overseas"),
            Some(420),
        ),
        row(
            2,
            Timestamp(now.as_millis() - 3 * DAY),
            "019a-old",
            "gpt-5.5",
            (100, 0, 10),
            Some(3),
            None,
            None,
        ),
        row(
            3,
            Timestamp(now.as_millis() - 9 * DAY),
            "019a-gone",
            "gpt-5.5",
            (100, 0, 10),
            Some(3),
            None,
            None,
        ),
    ];
    service.set_request_rows(rows);
    let svc: std::sync::Arc<dyn SwitchService> = std::sync::Arc::new(service);
    let s = usage::load(svc, now, 0).await.expect("snapshot");
    assert_eq!(s.today.requests, 1);
    assert_eq!(
        s.sessions
            .iter()
            .map(|s| s.key.as_str())
            .collect::<Vec<_>>(),
        ["019a-new", "019a-old"],
        "seven days only"
    );
    assert!(s.router.is_some());
    assert_eq!(s.traces.len(), 1);
    assert!(s.unpriced.is_empty());
    assert!(s.heat.complete_since.is_none());
}

// --- Rendering helpers ------------------------------------------------------------------------

async fn fake_data() -> Data {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

fn with_cx<R>(data: &Data, locale: Locale, f: impl FnOnce(&Cx<'_>) -> R) -> R {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let svc = support::idle_service();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts,
        service: &svc,
        log: &[],
    };
    f(&cx)
}

fn caps(data: &Data, line: &str, reachable: bool, ms: u32) -> LineCaps {
    let site = data.active_site_id().unwrap().clone();
    LineCaps {
        site_id: site,
        line_id: line.into(),
        checked_at: Timestamp::now(),
        reachable,
        latency_ms: Some(ms),
        pat_ok: true,
        apikey_ok: true,
        codex_path: None,
        blocked_by_edge: false,
        key_access: None,
        checks: Vec::new(),
        detail: None,
    }
}

fn router_on(data: &mut Data) {
    data.profiles
        .value
        .as_mut()
        .unwrap()
        .settings
        .router
        .enabled = true;
    let o = data.overview.value.as_mut().unwrap();
    o.config.as_mut().unwrap().openai_base_url =
        Some("http://127.0.0.1:18431/backend-api/codex".into());
}

fn finding(id: &str, severity: Severity, text: &str) -> DoctorFinding {
    DoctorFinding {
        id: id.into(),
        severity,
        title: LocalizedText::new("test.finding", text),
        detail: None,
        fixable: false,
    }
}

// --- To do -----------------------------------------------------------------------------------

#[tokio::test]
async fn todos_list_problems_first_with_their_fixes_and_nodes() {
    let mut data = fake_data().await;
    router_on(&mut data);
    data.router_watch = Some(RouterWatchState::Suspect {
        detail: "connection refused".into(),
    });
    let site = data.active_site_id().unwrap().clone();
    let c = caps(&data, "overseas", false, 0);
    data.caps
        .entry(site.clone())
        .or_default()
        .finish(Ok(vec![c]));
    let mut v = VersionsView::default();
    v.app_codex.mismatch = Some(LocalizedText::new("test.mismatch", "runs /x, not /y"));
    v.app.update_available = true;
    data.versions.finish(Ok(v));
    data.doctor.finish(Ok(vec![
        finding("plaintext_secrets", Severity::Warning, "plain"),
        finding(
            "stale_processes",
            Severity::Warning,
            "2 codex processes use the old config",
        ),
        finding("codex_home_consistent", Severity::Info, "fine"),
    ]));
    let snap = Snapshot {
        unpriced: vec!["spark".into(), "mini".into(), "nano".into()],
        router: Some(RouterView {
            configured: true,
            running: true,
            members: vec![RouterMemberView {
                profile_id: "krill".into(),
                name: "Krill".into(),
                lines: vec![RouterLineView {
                    id: "cdn".into(),
                    name: "CDN".into(),
                    health: RouterLineHealth::Open,
                    ttfb_ms: None,
                    requests: 3,
                    failovers_away: 3,
                    last_error: Some("HTTP 502".into()),
                }],
            }],
            ..RouterView::default()
        }),
        ..Snapshot::default()
    };
    for locale in [Locale::ZhCn, Locale::En] {
        with_cx(&data, locale, |cx| {
            let todos = todo::todos(cx, Some(&snap));
            let kinds: Vec<&TodoKind> = todos.iter().map(|t| &t.kind).collect();
            assert_eq!(
                kinds,
                [
                    &TodoKind::RouterDown,
                    &TodoKind::LineUnreachable,
                    &TodoKind::MemberDown,
                    &TodoKind::PinMismatch,
                    &TodoKind::PlainKeys,
                    &TodoKind::Finding("stale_processes".into()),
                    &TodoKind::Unpriced,
                    &TodoKind::AppUpdate,
                ],
                "errors, then warnings, then offers; info findings left out"
            );
            let get = |k: &TodoKind| todos.iter().find(|t| &t.kind == k).unwrap();
            assert_eq!(get(&TodoKind::RouterDown).status, Status::Error);
            assert_eq!(
                get(&TodoKind::RouterDown).detail.as_deref(),
                Some("connection refused")
            );
            assert_eq!(get(&TodoKind::LineUnreachable).spot, Some(Spot::Provider));
            assert_eq!(get(&TodoKind::PinMismatch).spot, Some(Spot::ThisComputer));
            assert_eq!(get(&TodoKind::PlainKeys).spot, Some(Spot::ThisComputer));
            assert_eq!(get(&TodoKind::Unpriced).spot, None);
            assert!(get(&TodoKind::MemberDown).title.contains("CDN"));
            let unpriced = get(&TodoKind::Unpriced);
            assert!(unpriced.title.contains('3'), "{}", unpriced.title);
            assert!(unpriced.detail.as_ref().unwrap().contains("spark"));
            // Each has a fix: the page that fixes it, a probe for the line.
            let fix = |k: &TodoKind| get(k).fix(cx);
            assert!(matches!(
                fix(&TodoKind::RouterDown),
                Action::Go(Page::Router)
            ));
            assert!(matches!(fix(&TodoKind::LineUnreachable), Action::Probe(s) if s == site));
            assert!(matches!(
                fix(&TodoKind::PinMismatch),
                Action::Go(Page::Updates)
            ));
            assert!(matches!(
                fix(&TodoKind::AppUpdate),
                Action::Go(Page::Updates)
            ));
            assert!(matches!(fix(&TodoKind::Unpriced), Action::Go(Page::Models)));
            assert!(matches!(
                fix(&TodoKind::PlainKeys),
                Action::Go(Page::Diagnostics)
            ));
            for t in &todos {
                assert!(!t.fix_label(cx).contains("overview."), "{:?}", t.kind);
                assert!(!t.title.contains("overview."), "{:?}", t.kind);
            }
            // Markers: the worst problem per node; offers mark nothing.
            assert_eq!(
                todo::worst(&todo::markers(&todos, Spot::Router)),
                Some(Status::Error)
            );
            assert_eq!(
                todo::worst(&todo::markers(&todos, Spot::ThisComputer)),
                Some(Status::Warn)
            );
            assert_eq!(todo::markers(&todos, Spot::Model).len(), 0);
        });
    }
}

// --- Hero ------------------------------------------------------------------------------------

#[tokio::test]
async fn the_hero_shows_the_router_as_a_node_only_when_router_mode_is_on() {
    let mut data = fake_data().await;
    let o = data.overview.value.clone().unwrap();
    with_cx(&data, Locale::ZhCn, |cx| {
        let h = hero::model(cx, &o, &[], None);
        assert!(!h.router_on);
        assert!(h.router.ghost, "off: a dashed ghost beside the bypass");
        assert_eq!(h.router.detail, cx.tr.t("overview.hero.router_off"));
        assert_eq!(h.this.title, cx.tr.t("overview.hero.this"));
        assert!(
            h.provider.title.starts_with("Krill"),
            "{}",
            h.provider.title
        );
        assert!(
            h.provider.menu && h.model.menu,
            "popovers on provider and model"
        );
        assert!(!h.this.menu && !h.router.menu);
        assert!(h.nodes().iter().all(|n| n.marker.is_none()));
    });
    router_on(&mut data);
    let o = data.overview.value.clone().unwrap();
    with_cx(&data, Locale::ZhCn, |cx| {
        let h = hero::model(cx, &o, &[], None);
        assert!(h.router_on);
        assert!(!h.router.ghost);
        assert_eq!(h.router.detail, "127.0.0.1:18431");
    });
    // No model in config.toml: the model of the newest request.
    data.overview
        .value
        .as_mut()
        .unwrap()
        .config
        .as_mut()
        .unwrap()
        .model = None;
    let o = data.overview.value.clone().unwrap();
    let snap = busy_snapshot();
    with_cx(&data, Locale::ZhCn, |cx| {
        let h = hero::model(cx, &o, &[], None);
        assert_eq!(h.model.title, cx.tr.t("overview.hero.default_model"));
        let h = hero::model(cx, &o, &[], Some(&snap));
        assert_eq!(h.model.title, "gpt-5.5");
        assert!(h.model.tip.contains("gpt-5.5"), "{}", h.model.tip);
    });
    // Router mode on but Codex direct: the node says so.
    data.overview
        .value
        .as_mut()
        .unwrap()
        .config
        .as_mut()
        .unwrap()
        .openai_base_url = None;
    let o = data.overview.value.clone().unwrap();
    with_cx(&data, Locale::En, |cx| {
        let h = hero::model(cx, &o, &todo::todos(cx, None), None);
        assert_eq!(h.router.detail, cx.tr.t("overview.hero.router_not_routed"));
        assert_eq!(h.router.marker, Some(Status::Warn));
    });
}

#[tokio::test]
async fn problem_markers_sit_on_the_nodes_they_concern() {
    let mut data = fake_data().await;
    let site = data.active_site_id().unwrap().clone();
    let c = caps(&data, "overseas", false, 0);
    data.caps.entry(site).or_default().finish(Ok(vec![c]));
    let mut v = VersionsView::default();
    v.app_codex.mismatch = Some(LocalizedText::new("test.mismatch", "runs /x, not /y"));
    data.versions.finish(Ok(v));
    let o = data.overview.value.clone().unwrap();
    with_cx(&data, Locale::ZhCn, |cx| {
        let todos = todo::todos(cx, None);
        let h = hero::model(cx, &o, &todos, None);
        assert_eq!(h.this.marker, Some(Status::Warn), "pin mismatch");
        assert!(h.this.problems[0].contains(&cx.tr.t("overview.todo.pin_mismatch")));
        assert_eq!(
            h.provider.marker,
            Some(Status::Error),
            "the line does not answer"
        );
        assert_eq!(h.router.marker, None);
        assert_eq!(h.model.marker, None);
        assert_eq!(
            h.latency,
            superba_gui::design::widgets::Latency::Unreachable
        );
    });
}

/// A closure that draws the overview page (`$state`, `$out`: `&mut` borrows
/// that end with the closure).
macro_rules! page {
    ($data:expr, $tr:expr, $fonts:expr, $state:expr, $out:expr) => {{
        let svc = support::idle_service();
        let (data, tr, fonts, state, out) = ($data, $tr, $fonts, $state, $out);
        move |ui: &mut egui::Ui| {
            let cx = Cx {
                tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: false,
                now: Timestamp::now(),
                fonts,
                service: &svc,
                log: &[],
            };
            overview::show(ui, &cx, state, out);
        }
    }};
}

fn busy_snapshot() -> Snapshot {
    let now = Timestamp::now();
    let rows: Vec<RequestRow> = (0..40)
        .map(|i| {
            row(
                i,
                Timestamp(now.as_millis() - i * 7 * MIN),
                ["019a-alpha", "019a-beta", "019a-gamma"][i as usize % 3],
                "gpt-5.5",
                (20_000, 15_000, 900),
                Some(40_000),
                Some(if i % 2 == 0 { "overseas" } else { "cn-fast" }),
                Some(500 + i as u64 * 10),
            )
        })
        .collect();
    let mut sessions = usage::recent_sessions(&rows, usage::RECENT_SESSIONS);
    sessions[0].title = Some("Polish the overview cards".into());
    Snapshot {
        today: totals(40, 800_000, 600_000, 36_000, 1_600_000),
        yesterday_same: totals(20, 400_000, 300_000, 18_000, 800_000),
        yesterday: totals(50, 900_000, 700_000, 40_000, 2_000_000),
        month: totals(900, 1, 0, 1, 52_000_000),
        heat: usage::heat(&rows, now, 0),
        sessions,
        traces: usage::line_traces(&rows, now),
        unpriced: Vec::new(),
        router: None,
        at: now,
        offset_secs: 0,
    }
}

#[tokio::test]
async fn the_cards_show_today_sessions_lines_and_a_calm_todo_list() {
    let data = fake_data().await;
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let mut state = overview::State::default();
        state.usage = UsageLoader::preloaded(busy_snapshot());
        let mut out = Vec::new();
        let mut f = page!(&data, &tr, &harness.fonts, &mut state, &mut out);
        let texts: Vec<String> = harness.settle(&mut f).into_iter().map(|(t, _)| t).collect();
        let all = texts.join("\n");
        for key in [
            "overview.card.today",
            "overview.card.lines",
            "overview.card.sessions",
            "overview.card.todo",
            "overview.hero.caption",
            "overview.todo.all_clear",
            "overview.today.cached",
            "overview.today.uncached",
            "overview.today.output",
            "overview.heat.today",
        ] {
            assert!(all.contains(&tr.t(key)), "{key}: {all}");
        }
        assert!(texts.contains(&"1.60".to_owned()), "today's cost: {all}");
        assert!(all.contains("100%"), "delta: {all}");
        assert!(
            all.contains(&tr.f("overview.today.requests", &[("n", &"40")])),
            "{all}"
        );
        assert!(all.contains("Polish the overview cards"), "{all}");
        assert!(all.contains("$52.00"), "this month: {all}");
        assert!(all.contains("$0.56"), "a session's cost: {all}");
        assert!(all.contains("Krill"), "{all}");
        assert!(!all.contains("NaN") && !all.contains("inf"), "{all}");
        assert!(!all.contains("overview."), "untranslated: {all}");
    }
}

#[tokio::test]
async fn without_requests_the_cards_show_tidy_empty_states() {
    let data = fake_data().await;
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let snap = Snapshot {
            heat: usage::heat(&[], Timestamp::now(), 0),
            at: Timestamp::now(),
            ..Snapshot::default()
        };
        let mut state = overview::State::default();
        state.usage = UsageLoader::preloaded(snap);
        let mut out = Vec::new();
        let mut f = page!(&data, &tr, &harness.fonts, &mut state, &mut out);
        let all = harness
            .settle(&mut f)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>()
            .join("\n");
        for key in [
            "overview.today.empty",
            "overview.heat.empty",
            "overview.sessions.empty",
            "overview.lines.failover_off",
            "overview.hero.router_off",
        ] {
            assert!(all.contains(&tr.t(key)), "{key}: {all}");
        }
        assert!(!all.contains("$0.00"), "no fake zero: {all}");
    }
}

#[tokio::test]
async fn nodes_open_their_pages_and_the_popovers_switch_line_and_tier() {
    use superba_core::SpeedTier;
    let mut data = fake_data().await;
    let profile = data.active_profile().unwrap().clone();
    let fake = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    data.active_models.finish(
        fake.models_view(superba_core::ModelsViewRequest::default())
            .await,
    );
    assert_eq!(
        data.active_models.value.as_ref().unwrap().profile_id,
        profile.id
    );
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = Tr::new(Locale::ZhCn);
    let mut state = overview::State::default();
    state.usage = UsageLoader::preloaded(busy_snapshot());
    let mut out = Vec::new();
    let nodes = [
        (tr.t("overview.hero.this"), Page::Updates),
        (tr.t("overview.hero.router"), Page::Router),
    ];
    {
        let mut f = page!(&data, &tr, &harness.fonts, &mut state, &mut out);
        for (text, _) in &nodes {
            assert!(harness.click_text(text, &mut f), "{text}");
        }
    }
    for (text, page) in nodes {
        assert!(
            out.iter().any(|a| matches!(a, Action::Go(p) if *p == page)),
            "{text}: {out:?}"
        );
    }
    // The provider's chevron: its lines (a click plans the switch).
    out.clear();
    let title_rect = |texts: &[(String, Rect)], prefix: &str| {
        texts
            .iter()
            .find(|(t, _)| t.starts_with(prefix))
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{prefix}: {texts:?}"))
    };
    {
        let mut f = page!(&data, &tr, &harness.fonts, &mut state, &mut out);
        let texts = harness.settle(&mut f);
        let r = title_rect(&texts, "Krill");
        harness.click_at(egui::pos2(r.max.x + 12.0, r.center().y), &mut f);
        let texts = harness.settle(&mut f);
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            all.contains(&tr.t("overview.other_providers").as_str()),
            "{all:?}"
        );
        // The popover (drawn last, above the cards) repeats the line names of
        // the 线路 card.
        assert!(harness.click_last_text("国内极速", &mut f));
    }
    assert!(
        out.iter().any(|a| matches!(
            a,
            Action::Switch { line: Some(l), .. } if l.as_str() == "cn-fast"
        )),
        "{out:?}"
    );
    // The model's chevron: the speed tier.
    out.clear();
    {
        let mut f = page!(&data, &tr, &harness.fonts, &mut state, &mut out);
        let texts = harness.settle(&mut f);
        let model = data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.config.as_ref())
            .and_then(|c| c.model.clone())
            .unwrap_or_else(|| tr.t("overview.hero.default_model"));
        let r = title_rect(&texts, &model);
        harness.click_at(egui::pos2(r.max.x + 12.0, r.center().y), &mut f);
        let texts = harness.settle(&mut f);
        assert!(
            texts
                .iter()
                .any(|(t, _)| *t == tr.t("overview.speed.label")),
            "{texts:?}"
        );
        assert!(harness.click_text("Fast", &mut f));
    }
    assert!(
        out.iter().any(|a| matches!(
            a,
            Action::SetSpeed { profile: p, tier: SpeedTier::Fast } if *p == profile.id
        )),
        "{out:?}"
    );
}

#[tokio::test]
async fn a_recent_session_opens_usage_filtered_to_it() {
    let data = fake_data().await;
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    let tr = Tr::new(Locale::En);
    let mut state = overview::State::default();
    state.usage = UsageLoader::preloaded(busy_snapshot());
    let mut out = Vec::new();
    {
        let mut f = page!(&data, &tr, &harness.fonts, &mut state, &mut out);
        assert!(harness.click_text("Polish the overview cards", &mut f));
    }
    let key = busy_snapshot().sessions[0].key.clone();
    assert!(
        out.iter().any(|a| matches!(
            a,
            Action::PageCommand { page: Page::Usage, command } if *command == overview::usage_command(&key)
        )),
        "{out:?}"
    );
}

// --- Fit ----------------------------------------------------------------------------------------

#[test]
fn the_grid_is_two_thirds_and_one_third_with_rows_that_do_not_line_up() {
    for [w, h] in support::fit::SIZES {
        // The page rectangle below the title row (as `show` gets it).
        let page_w = (w - 220.0 - 64.0).min(1200.0);
        let page_h = h - 28.0 - 16.0;
        let area = Rect::from_min_size(
            egui::pos2(252.0, 28.0 + overview::TITLE_ROW + 8.0),
            egui::vec2(page_w, page_h - overview::TITLE_ROW - 8.0),
        );
        let g = overview::layout(area);
        for r in [g.hero, g.today, g.sessions, g.lines, g.todo] {
            assert!(area.expand(0.5).contains_rect(r), "{r:?} in {area:?}");
        }
        assert_eq!(g.hero.width(), area.width());
        assert!((g.today.width() - (area.width() - overview::GAP) * 2.0 / 3.0).abs() <= 1.0);
        assert_eq!(g.today.width(), g.sessions.width());
        assert_eq!(g.lines.width(), g.todo.width());
        assert_eq!(g.sessions.max.y, g.todo.max.y);
        assert!(
            (g.today.max.y - g.lines.max.y).abs() > 8.0,
            "the rows split at different heights"
        );
        for (a, b) in [(g.today, g.sessions), (g.lines, g.todo)] {
            assert!(b.min.y - a.max.y >= overview::GAP - 0.5);
        }
        assert!(g.lines.min.x - g.today.max.x >= overview::GAP - 0.5);
        assert!(
            g.sessions.height() >= 200.0 && g.todo.height() >= 200.0,
            "{g:?}"
        );
    }
}

#[tokio::test]
async fn the_overview_fits_1280x800_and_1440x900_without_scrolling() {
    let data = fake_data().await;
    for window in support::fit::SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            for snap in [Some(busy_snapshot()), None] {
                let report = support::fit::render_fit(
                    window,
                    locale,
                    egui::Theme::Light,
                    &data,
                    |ui, cx, out| {
                        let mut state = overview::State::default();
                        state.usage = snap
                            .clone()
                            .map_or_else(UsageLoader::default, UsageLoader::preloaded);
                        overview::show(ui, cx, &mut state, out);
                    },
                );
                assert!(report.fits(), "{window:?} {locale:?}: {report:?}");
                assert!(!report.scrolled, "{window:?}");
                assert_eq!(report.scroll_areas, 0, "{window:?}");
            }
        }
    }
}

#[tokio::test]
async fn the_usage_page_shows_one_session_for_the_overview_link() {
    use superba_core::RequestGrouping;
    use superba_gui::pages::router::{self, Tab};
    let data = fake_data().await;
    let mut state = router::State::default();
    state.tab = Tab::Stats;
    state.search = "old search".into();
    state.recent.model = Some("gpt-5.5".into());
    with_cx(&data, Locale::ZhCn, |cx| {
        let mut out = Vec::new();
        router::usage_command(
            &mut state,
            cx,
            &overview::usage_command("019a-beta"),
            &mut out,
        );
        assert!(out.is_empty());
    });
    assert_eq!(state.tab, Tab::Recent);
    assert!(state.search.is_empty());
    assert_eq!(state.recent.grouping, RequestGrouping::Session);
    assert_eq!(state.recent.group.as_deref(), Some("019a-beta"));
    assert_eq!(state.recent.model, None, "other filters cleared");
    assert_eq!(
        state.recent.range.duration(),
        std::time::Duration::from_secs(7 * 86_400),
        "the seven days the card covers"
    );
}
