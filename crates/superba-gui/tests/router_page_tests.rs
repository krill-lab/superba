#![allow(missing_docs)]
//! Router and Usage pages (UX §3.5, plan §5 B7, user decision 21): the Usage
//! page's Fit layout with all 13 columns at 1280x800 and 1440x900 in both
//! languages, session names from a `state_5.sqlite`, the detail pane (通用 and
//! 计时 only), the toolbar, the active requests, statistics; the Router page with the stats
//! line preview, the price table and the palette.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::{Frame, Harness};
use superba_core::{InflightPhase, InflightRequest, Locale, ProfileId, RequestClient, Timestamp};
use superba_gui::pages::router::{self, State, Tab};
use superba_gui::pages::{Action, Page};
use support::fake::{FakeOptions, FakeService};
use support::fit::SIZES;

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn open_advanced(h: &mut Harness, state: &mut State, locale: Locale) {
    let title = superba_gui::i18n::Tr::new(locale).t("router.advanced.title");
    let frame = h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, state, out),
        |f| {
            f.clipped_texts
                .iter()
                .any(|(text, rect, clip)| text == &title && clip.contains_rect(*rect))
        },
    );
    // 「高级」 is a plain section since user bug 116: in view is enough.
    let _ = frame;
    h.settle(|ui, cx, out| router::show(ui, cx, state, out));
}

fn loaded(f: &Frame) -> bool {
    f.has("登录页样式") || f.has("Krill · 海外线路")
}

const ZH_COLUMNS: [&str; 6] = ["工具", "模型", "状态", "输入", "输出", "费用"];
const EN_COLUMNS: [&str; 6] = ["Tool", "Model", "Status", "Input", "Output", "Cost"];

#[test]
fn essential_columns_fit_without_page_scrolling_at_both_sizes_and_languages() {
    for size in SIZES {
        for (locale, columns) in [(Locale::ZhCn, ZH_COLUMNS), (Locale::En, EN_COLUMNS)] {
            let service = fake();
            let mut h = Harness::new(size, locale, service, true);
            let mut state = State::default();
            let f = h.until(
                |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
                loaded,
            );
            assert!(!f.scrolled, "{size:?} {locale:?}: page scrolls");
            assert!(f.fits(), "{size:?} {locale:?}: drawn below the page");
            for c in columns {
                // Headers may be elided ("Completed …") but every column is drawn
                // inside the page, so no horizontal scrolling is needed.
                let r = f
                    .texts
                    .iter()
                    .find(|(t, _)| {
                        t.starts_with(c) || t.starts_with(&c.chars().take(4).collect::<String>())
                    })
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| {
                        panic!("{size:?} {locale:?}: no column {c}\n{}", f.all_text())
                    });
                assert!(r.max.x <= f.rect.max.x + 1.0, "{c} is outside the page");
            }
            // Only the log rows and the source list scroll, inside their areas.
            assert!(f.scroll_areas <= 2, "{} scroll areas", f.scroll_areas);
            // With a selected row the detail pane opens and the page still fits.
            state.recent.selected = Some(12);
            let f = h.until(
                |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
                |f| {
                    f.has(if locale == Locale::ZhCn {
                        "通用"
                    } else {
                        "General"
                    })
                },
            );
            assert!(!f.scrolled && f.fits(), "{size:?} {locale:?} with details");
            let card = if locale == Locale::ZhCn {
                "用量"
            } else {
                "Usage"
            };
            assert!(f.has(card), "{}", f.all_text());
        }
    }
}

#[test]
fn session_names_resolve_from_a_state_db_and_are_never_stored() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home").join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/router/state_5.sqlite"
        ),
        home.join("state_5.sqlite"),
    )
    .unwrap();
    let service = fake();
    service.titles_from_home(dir.path().to_path_buf());
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("登录页样式") && f.has("写周报：总结本周的 router 测试") && f.has("router 测试"),
    );
    assert!(service.title_queries() > 0);
    // The unknown key falls back to its short form.
    assert!(f.has("019affff"), "{}", f.all_text());
    // Names live only in the page's cache: the log rows still carry keys only.
    assert!(
        service
            .request_rows()
            .iter()
            .all(|r| r.prompt_cache_key.as_deref() != Some("登录页样式"))
    );
    let _ = Timestamp::now();
}

#[test]
fn a_codex_exec_row_never_shows_an_added_stats_line() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        loaded,
    );
    let exec = service
        .request_rows()
        .into_iter()
        .find(|r| r.client == RequestClient::Exec)
        .unwrap();
    assert!(exec.injected);
    state.recent.selected = Some(exec.id);
    state.recent.detail_tab = 1;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("统计行：codex exec 从不添加"),
    );
    assert!(!f.has("这条回复已添加统计行"));
    // A desktop answer with the line says so.
    state.recent.selected = Some(7);
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("这条回复已添加统计行"),
    );
}

#[test]
fn toolbar_pause_and_clear_reach_the_service() {
    let service = fake();
    // Pausing and clearing act on the router's records: a router with a member.
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(superba_core::SwitchService::router_install(
            service.as_ref(),
            superba_core::RouterInstallRequest {
                members: vec![ProfileId::new("krill")],
            },
        ))
        .unwrap();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        loaded,
    );
    h.click_text(&f, "暂停记录");
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |_| service.saved_stats().last().is_some_and(|s| !s.record),
    );
    // Clear asks first, then deletes everything.
    let f = h.frames(2, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    h.click_text(&f, "清空");
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("删除请求记录？"),
    );
    assert!(router::status(&state).modal);
    let _ = f;
    // Let the dialog settle (it re-centres once its size is known).
    let f = h.frames(3, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    let r = f
        .texts
        .iter()
        .rev()
        .find(|(t, _)| t == "清空")
        .map(|(_, r)| *r)
        .unwrap();
    h.click_at(r.center());
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |_| !service.clears().is_empty(),
    );
    assert!(service.clears()[0].before.is_some());
    assert!(service.request_rows().is_empty());
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("这个时间范围内没有 Codex 请求") || f.has("还没有请求"),
    );
}

#[test]
fn active_requests_show_what_is_in_flight() {
    let service = fake();
    service.set_inflight(vec![InflightRequest {
        id: 7,
        started_at: Timestamp::now(),
        member_id: ProfileId::new("krill"),
        member_name: "Krill".into(),
        line_name: Some("海外线路".into()),
        model: Some("gpt-5.5".into()),
        prompt_cache_key: Some(FakeService::THREAD_LOGIN.into()),
        client: RequestClient::Desktop,
        transport: Default::default(),
        phase: InflightPhase::Streaming,
        output_deltas: 42,
        first_token_ms: Some(910),
        elapsed_ms: 1_500,
    }]);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    state.tab = Tab::Active;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("输出中") && f.has("登录页样式"),
    );
    assert!(f.fits() && !f.scrolled);
}

#[test]
fn statistics_group_rows_and_show_hourly_then_daily_bars() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service, true);
    let mut state = State::default();
    state.tab = Tab::Stats;
    // 24 h: a bar per hour, and the caption is the range total (ux-visual #8:
    // "247 requests per day" over two daily bars).
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("gpt-5.5") && f.has("requests, by hour"),
    );
    assert!(!f.has("per day"), "{}", f.all_text());
    assert!(f.has("Success rate"));
    assert!(f.has("Avg TTFT") && f.has("Avg TPS"), "{}", f.all_text());
    assert!(f.fits() && !f.scrolled);
    // Numeric headers are right-aligned over their right-aligned values.
    // The table header (the metric toggle above the bars says Cost too).
    let header = f
        .texts
        .iter()
        .filter(|(t, _)| t == "Cost")
        .map(|(_, r)| *r)
        .max_by(|a, b| a.min.y.total_cmp(&b.min.y))
        .unwrap();
    let below: Vec<_> = f
        .texts
        .iter()
        .filter(|(t, r)| t.starts_with('≈') && r.min.y > header.max.y)
        .map(|(_, r)| *r)
        .collect();
    assert!(!below.is_empty(), "{}", f.all_text());
    for r in below {
        assert!((r.max.x - header.max.x).abs() < 1.0, "{header:?} vs {r:?}");
    }
    // 7 days: daily bars.
    state.stats.range = superba_gui::pages::router::requests::Range::Week;
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("requests, by day"),
    );
    state.stats.by = superba_core::StatsGroupBy::Session;
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("登录页样式"),
    );
}

#[test]
fn the_stats_line_preview_follows_the_template_while_typing() {
    let service = fake();
    let mut h = Harness::new([1440.0, 900.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    open_advanced(&mut h, &mut state, Locale::ZhCn);
    let f = h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| {
            f.has("87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out")
                && f.has("预览 · 示例数据")
                && f.has("默认（跟随界面语言）")
        },
    );
    assert!(f.has("预览 · 示例数据"));
    assert!(!f.has("已自定义模板"));
    // The default follows the UI language (user bug 23).
    assert!(f.has("默认（跟随界面语言）"), "{}", f.all_text());
    state.prefs.template = Some("X {tps} Y {output}".into());
    let f = h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("X 87 Y 321") && f.has("已自定义模板 · 恢复默认后可用勾选"),
    );
    // While a custom template is typed the metric checkboxes are disabled.
    assert!(f.has("已自定义模板 · 恢复默认后可用勾选"));
    // English default and the italic style.
    let mut h = Harness::new([1280.0, 800.0], Locale::En, fake(), true);
    let mut state = State::default();
    open_advanced(&mut h, &mut state, Locale::En);
    let f = h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| {
            f.has("87 tok/s · first token 0.82s · turn 12,345 in / 321 out")
                && f.has("Default (follows the UI language)")
        },
    );
    assert!(f.has("Default (follows the UI language)"));
}

#[test]
fn stats_line_and_recording_commands_save_settings_without_restarting_codex() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service.clone(), true);
    let mut state = State::default();
    h.frame(|_, cx, out| router::command(&mut state, cx, "stats_line:on", out));
    h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |_| service.saved_stats().last().is_some_and(|s| s.line.enabled),
    );
    h.reload(&service);
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    // Saving reloads the shell data (no switch is planned).
    assert!(f.actions.iter().all(|a| !matches!(a, Action::Plan(_))));
    let items = h.frame(|_, cx, _| {
        let items = router::palette(cx, &state);
        let titles: Vec<String> = items.iter().map(|i| i.title.clone()).collect();
        assert!(
            titles.contains(&"Open recent requests".to_owned()),
            "{titles:?}"
        );
        assert!(
            titles.contains(&"Turn the stats line off".to_owned()),
            "{titles:?}"
        );
    });
    let _ = items;
    h.frame(|_, cx, out| router::usage_command(&mut state, cx, "tab:active", out));
    assert_eq!(state.tab, Tab::Active);
    h.frame(|_, cx, out| router::usage_command(&mut state, cx, "search", out));
    assert_eq!(state.tab, Tab::Active);
}

/// User bug 9: the filter dropdowns' bottom border was cut off by the filter
/// row. Each dropdown's frame is drawn whole inside the clip rectangle it is
/// drawn in, in both themes and languages.
#[test]
fn the_filter_dropdowns_are_drawn_whole() {
    for (locale, labels) in [
        (
            Locale::ZhCn,
            ["全部供应商", "全部模型", "全部状态", "App + CLI"],
        ),
        (
            Locale::En,
            ["All providers", "All models", "All statuses", "App + CLI"],
        ),
    ] {
        for dark in [false, true] {
            let mut h = Harness::new(SIZES[0], locale, fake(), true);
            if dark {
                h = h.dark();
            }
            let mut state = State::default();
            let f = h.until(
                |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
                loaded,
            );
            for label in labels {
                let text = f
                    .find(label)
                    .unwrap_or_else(|| panic!("{locale:?}: no {label}\n{}", f.all_text()));
                // The dropdown's frame: the smallest outline around its text.
                let (frame, clip) = f
                    .outlines
                    .iter()
                    .filter(|(r, _)| r.contains_rect(text))
                    .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
                    .copied()
                    .unwrap_or_else(|| panic!("{locale:?} dark={dark}: no frame around {label}"));
                assert!(
                    clip.expand(0.01).contains_rect(frame),
                    "{locale:?} dark={dark}: {label}'s frame {frame:?} is clipped by {clip:?}"
                );
                assert!(
                    frame.height() <= superba_gui::design::tokens::size::CONTROL_SM + 0.01,
                    "{locale:?}: {label} is {} px tall",
                    frame.height()
                );
            }
        }
    }
}

#[test]
fn empty_states_explain_the_router_and_the_filters() {
    let service = fake();
    service.set_request_rows(Vec::new());
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("这个时间范围内没有 Codex 请求") || f.has("还没有请求"),
    );
    assert!(f.fits());
    state.recent.model = Some("nothing".into());
    state.recent.changed();
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("没有符合筛选条件的请求"),
    );
}

#[test]
fn the_banner_says_traffic_goes_through_a_local_process() {
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, fake(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("路由已关闭") || f.has("路由已开启"),
    );
    assert!(
        f.has("路由已关闭") || f.has("路由已开启"),
        "{}",
        f.all_text()
    );
    // The Router page is settings only: no request log, no tabs.
    assert!(!f.has("最近请求"), "{}", f.all_text());
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("SSH 主机的请求不在这里显示"),
    );
    assert!(
        f.has("路由未开启") || f.has("路由已开启"),
        "{}",
        f.all_text()
    );
    let _ = Page::Router;
}

#[test]
fn the_price_table_in_use_can_be_viewed() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service, true);
    let mut state = State::default();
    h.frames(2, |ui, cx, out| router::show(ui, cx, &mut state, out));
    h.frame(|_, cx, _| router::prices::open_table(cx, &mut state));
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("gpt-6-astra"),
    );
    for want in [
        "Price table",
        "gpt-6-luna",
        "$40",
        "openai.com",
        "revision 2026-10-01",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    assert!(router::status(&state).modal);
}

#[test]
fn service_facts_and_buttons_line_up_under_the_status_line() {
    // User bug 25 stays fixed now that the service sits under the status line
    // (user bug 116): its facts and its buttons start at the status text.
    for locale in [Locale::En, Locale::ZhCn] {
        let mut h = Harness::new([1280.0, 800.0], locale, fake(), true);
        let mut state = State::default();
        let tr = superba_gui::i18n::Tr::new(locale);
        let install = tr.t("router.service.install");
        let f = h.until(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            |f| f.has(&install),
        );
        // A small button's text starts 12 px into it.
        let button = f.find(&install).unwrap();
        let caption = f.find(&tr.t("router.service_label")).unwrap();
        assert!(
            (button.min.x - 12.0 - caption.min.x).abs() < 0.5,
            "{locale:?}: button at {}, facts at {}",
            button.min.x - 12.0,
            caption.min.x
        );
        assert!(
            caption.max.y <= button.min.y,
            "{locale:?}: facts above the buttons"
        );
        let members = f.find(&tr.t("router.members.title")).unwrap();
        assert!(
            button.max.y < members.min.y,
            "{locale:?}: above the members"
        );
        assert_eq!(f.scroll_areas, 1, "one scroll area for all router settings");
    }
}

#[test]
fn every_key_based_provider_has_a_price_ratio_without_the_router() {
    // Review ux-visual #4: with the router off the price section only said
    // "add members in Router settings", while direct-mode costs use the ratio.
    let service = fake();
    for (locale, ratio) in [(Locale::ZhCn, "× 1"), (Locale::En, "× 1")] {
        let tr = superba_gui::i18n::Tr::new(locale);
        let mut h = Harness::new([1440.0, 900.0], locale, service.clone(), true);
        let mut state = State::default();
        open_advanced(&mut h, &mut state, locale);
        let f = h.until_scrolled(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            // The ratio cell itself (the section's note mentions "× 1" too).
            |f| f.has(&tr.t("prices.title")) && f.texts.iter().any(|(t, _)| t == ratio),
        );
        assert!(f.has("Krill"), "{}", f.all_text());
        assert!(
            !f.has("路由设置") && !f.has("Router settings"),
            "{}",
            f.all_text()
        );
        assert!(!f.has(&tr.t("prices.no_providers")));
        // No official login among them (ChatGPT bills it).
        let official = tr.t("providers.official.name");
        let prices_at = f.find(&tr.t("prices.title")).unwrap().min.y;
        assert!(
            !f.texts
                .iter()
                .any(|(t, r)| *t == official && r.min.y > prices_at),
            "{}",
            f.all_text()
        );
    }
}

#[test]
fn disabling_router_shows_off_banner_for_a_direct_local_upstream() {
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, fake(), true);
    let mut state = State::default();
    let endpoint: url::Url = "http://127.0.0.1:18431/backend-api/codex".parse().unwrap();
    h.data
        .profiles
        .value
        .as_mut()
        .unwrap()
        .settings
        .router
        .enabled = true;
    let overview = h.data.overview.value.as_mut().unwrap();
    overview.router_base_url = Some(endpoint.clone());
    overview.config.as_mut().unwrap().openai_base_url = Some(endpoint.to_string());
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(f.has("路由已开启"), "{}", f.all_text());

    // After the disable switch the original upstream is a local model server.
    h.data
        .profiles
        .value
        .as_mut()
        .unwrap()
        .settings
        .router
        .enabled = false;
    h.data
        .overview
        .value
        .as_mut()
        .unwrap()
        .config
        .as_mut()
        .unwrap()
        .openai_base_url = Some("http://127.0.0.1:11434/v1".into());
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(f.has("路由已关闭"), "{}", f.all_text());
    assert!(
        f.has("直连 · 本机服务（127.0.0.1:11434）"),
        "{}",
        f.all_text()
    );
    assert!(!f.has("路由已开启"), "{}", f.all_text());

    // Even an unchanged/stale config cannot turn the saved off choice back on.
    h.data
        .overview
        .value
        .as_mut()
        .unwrap()
        .config
        .as_mut()
        .unwrap()
        .openai_base_url = Some(endpoint.to_string());
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(f.has("路由已关闭"), "{}", f.all_text());
    assert!(!f.has("路由已开启"), "{}", f.all_text());
}

/// The price editor stays in its table row: the ratio field in the ratio
/// column, 保存 and 取消 in the actions column where 按账单校准… and 编辑 were,
/// nothing outside the row; 取消 puts the ratio back.
#[test]
fn the_ratio_editor_replaces_the_ratio_and_its_actions_in_place() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = superba_gui::i18n::Tr::new(locale);
        let mut h = Harness::new([960.0, 640.0], locale, fake(), true);
        let mut state = State::default();
        open_advanced(&mut h, &mut state, locale);
        let edit = tr.t("prices.edit");
        let f = h.until_scrolled(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            |f| {
                f.clipped_texts
                    .iter()
                    .any(|(t, r, clip)| t == &edit && clip.contains_rect(*r))
            },
        );
        let ratio = f
            .texts
            .iter()
            .find(|(t, _)| t == "× 1")
            .map(|(_, r)| *r)
            .expect("ratio cell");
        h.click_text(&f, &edit);
        let f = h.settle(|ui, cx, out| router::show(ui, cx, &mut state, out));
        // The row's line (the focused field may scroll it a little into view).
        let row = f.find("Krill").expect("Krill's row");
        let (save, cancel) = (tr.t("prices.save"), tr.t("action.cancel"));
        for text in [&save, &cancel] {
            let (_, r, clip) = f
                .clipped_texts
                .iter()
                .find(|(t, _, _)| t == text)
                .unwrap_or_else(|| panic!("{text} missing:\n{}", f.all_text()));
            assert!(clip.contains_rect(*r), "{locale:?}: {text} clipped");
            // On the row's line, in the actions column right of the ratio.
            assert!(
                (r.center().y - row.center().y).abs() < 1.0,
                "{locale:?}: {text} at {r:?}, the row at {row:?}"
            );
            assert!(
                r.min.x > ratio.max.x - 0.5,
                "{locale:?}: {text} over the ratio"
            );
        }
        // The field holds the ratio being edited.
        assert!(
            f.texts
                .iter()
                .any(|(t, r)| t == "1" && (r.center().y - row.center().y).abs() < 1.0)
        );
        assert!(
            !f.texts
                .iter()
                .any(|(t, r)| t == "× 1" && (r.center().y - row.center().y).abs() < 1.0)
        );
        h.click_text(&f, &cancel);
        let f = h.settle(|ui, cx, out| router::show(ui, cx, &mut state, out));
        let drawn = |s: &str| f.texts.iter().any(|(t, _)| t == s);
        assert!(drawn(&edit) && !drawn(&save), "{}", f.all_text());
    }
}
