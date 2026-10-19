#![allow(missing_docs)]

mod a11y_harness;
mod router_harness;
mod support;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use router_harness::Harness;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use superba_core::{
    AgentKind, Locale, RequestHarness, RequestProtocol, RequestSource, StatsGroupBy, Timestamp,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::{
    Page,
    router::{
        self, State, Tab, ingress,
        requests::{self, Col},
        stats_line,
    },
};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..FakeOptions::default()
    }));
    let mut rows = service.request_rows();
    rows.truncate(4);
    for (row, harness) in rows.iter_mut().zip([
        RequestHarness::Codex,
        RequestHarness::ClaudeCode,
        RequestHarness::Dsh,
        RequestHarness::Pi,
    ]) {
        row.harness = harness;
        row.session_id = Some("shared-session".into());
        row.agent_kind = Some(AgentKind::Main);
        row.requested_model = row.model.clone();
    }
    rows[1].protocol = RequestProtocol::Messages;
    rows[1].requested_model = Some("haiku".into());
    rows[1].model = Some("gpt-5.4-mini".into());
    rows[1].upstream_model = rows[1].model.clone();
    rows[2].protocol = RequestProtocol::ChatCompletions;
    rows[2].codex_host = Some("wsl:Ubuntu".into());
    service.set_request_rows(rows);
    service
}

#[test]
fn tool_column_model_mapping_and_titles_have_readable_accessible_text() {
    let service = fake();
    let rows = service.request_rows();
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let cell = requests::cell(
            &tr,
            &BTreeMap::new(),
            &rows[1],
            Col::Harness,
            Timestamp::now(),
        );
        assert_eq!(cell.text, "Claude Code");
        assert!(cell.icon.is_some());
        assert_eq!(requests::model_text(&rows[1]), "haiku → gpt-5.4-mini");
        let name = requests::row_name(&tr, &BTreeMap::new(), &rows[1], Timestamp::now());
        assert!(name.contains("Claude Code") && name.contains("haiku → gpt-5.4-mini"));
        assert_eq!(requests::session_key(&rows[1]), Some("shared-session"));
        let key = rows[1].session_group_key().unwrap();
        assert_eq!(
            requests::session_group_label(&tr, &BTreeMap::new(), &key),
            "Claude Code · shared-s"
        );
    }
}

#[test]
fn recent_and_stats_filters_use_the_same_tool_model_provider_and_host() {
    let service = fake();
    let mut h = Harness::new([960.0, 640.0], Locale::En, service, true);
    let mut state = State::default();
    state.recent.harnesses = vec![RequestHarness::ClaudeCode, RequestHarness::Dsh];
    state.recent.host = Some("wsl:Ubuntu".into());
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("Ubuntu"),
    );
    assert!(
        f.fits() || f.scrolled,
        "used={} rect={:?} scrolled={}",
        f.used_bottom,
        f.rect,
        f.scrolled
    );
    for _ in 0..60 {
        h.frame(|ui, cx, out| router::show_usage(ui, cx, &mut state, out));
        if state.recent.shown.len() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(state.recent.shown.len(), 1);
    assert_eq!(state.recent.shown[0].harness, RequestHarness::Dsh);
    state.tab = Tab::Stats;
    state.stats.by = StatsGroupBy::HarnessModel;
    for _ in 0..60 {
        h.frame(|ui, cx, out| router::show_usage(ui, cx, &mut state, out));
        if state.stats.stats.value.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let stats = state.stats.stats.value.as_ref().unwrap();
    assert_eq!(stats.totals.requests, 1);
    assert!(stats.buckets[0].key.starts_with("dsh"));
}

#[test]
fn stats_loads_model_and_host_choices_without_visiting_recent_requests() {
    let mut h = Harness::new([960.0, 640.0], Locale::En, fake(), true);
    let mut state = State::default();
    state.tab = Tab::Stats;
    state.stats.by = StatsGroupBy::Harness;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("Claude Code"),
    );
    h.click_at(f.find("All models").unwrap().center());
    h.settle(|ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("gpt-5.4-mini"),
    );
    h.click_at(f.find("gpt-5.4-mini").unwrap().center());
    let f = h.settle(|ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    assert_eq!(state.recent.model.as_deref(), Some("gpt-5.4-mini"));
    h.click_at(f.find("All hosts").unwrap().center());
    let f = h.settle(|ui, cx, out| router::show_usage(ui, cx, &mut state, out));
    assert!(f.has("Ubuntu"), "{}", f.all_text());
}

#[test]
fn usage_filters_and_ingresses_fit_both_locales_and_window_sizes() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for size in [[1280.0, 800.0], [960.0, 640.0]] {
            let mut h = Harness::new(size, locale, fake(), true);
            let mut state = State::default();
            for tab in [Tab::Recent, Tab::Stats] {
                state.tab = tab;
                let f = h.frames(3, |ui, cx, out| router::show_usage(ui, cx, &mut state, out));
                assert!(
                    f.fits() || f.scrolled,
                    "{locale:?} {size:?} {tab:?} used={} rect={:?} scrolled={}\n{}",
                    f.used_bottom,
                    f.rect,
                    f.scrolled,
                    f.all_text()
                );
                assert!(f.has(&tr.t("requests.filter.all_tools")));
                assert!(f.has(&tr.t("requests.filter.all_hosts")));
                if tab == Tab::Stats {
                    let y = f.find(&tr.t("requests.col.model")).unwrap().center().y;
                    let mut headers: Vec<_> = f
                        .texts
                        .iter()
                        .filter(|(_, rect)| (rect.center().y - y).abs() < 1.0)
                        .collect();
                    headers.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
                    for pair in headers.windows(2) {
                        assert!(
                            pair[0].1.right() <= pair[1].1.left(),
                            "{locale:?} {size:?}: overlapping headers {pair:?}"
                        );
                    }
                }
                for (rect, clip) in &f.outlines {
                    if rect.height() >= 26.0 && rect.height() <= 33.0 && rect.intersects(*clip) {
                        assert!(
                            rect.right() <= f.rect.right() + 1.0,
                            "{rect:?} outside {:?}",
                            f.rect
                        );
                        assert!(
                            rect.top() >= clip.top() - 1.0 && rect.bottom() <= clip.bottom() + 1.0,
                            "{locale:?} {size:?} {tab:?}: control {rect:?} clipped by {clip:?}"
                        );
                    }
                }
            }
            let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
            assert!(
                f.fits() || f.scrolled,
                "used={} rect={:?} scrolled={}",
                f.used_bottom,
                f.rect,
                f.scrolled
            );
            assert!(f.has(&tr.t("router.ingress.title")));
            assert!(f.has(&tr.t("router.ingress.openai")));
        }
    }
}

#[test]
fn tools_can_be_multiselected_and_read_by_assistive_technology() {
    let mut h = AppHarness::new(fake(), Locale::En, Page::Usage);
    h.resize([960.0, 640.0]);
    let tree = h.until("Usage rows", |t| t.has_containing("haiku → gpt-5.4-mini"));
    assert!(tree.unnamed().is_empty(), "{}", tree.dump());
    let row = tree
        .with_role(Role::Row)
        .into_iter()
        .find(|id| {
            tree.name(*id)
                .is_some_and(|n| n.contains("haiku → gpt-5.4-mini"))
        })
        .unwrap();
    h.click_id(row);
    h.until("Messages request details", |t| {
        t.has_containing("POST /messages")
    });
    h.click(Some(Role::ComboBox), "Tool");
    h.click(Some(Role::CheckBox), "Claude Code");
    // A multi-select menu stays open, making a second selection possible.
    let tree = h.click(Some(Role::CheckBox), "dsh");
    assert!(tree.find(Some(Role::CheckBox), "Claude Code").is_some());
    h.key(egui::Key::Escape);
    let tree = h.until("two selected tools", |t| {
        t.nodes.values().any(|n| {
            n.role() == Role::ComboBox
                && n.value()
                    .is_some_and(|v| v.contains("Claude Code") && v.contains("dsh"))
        })
    });
    assert!(tree.unnamed().is_empty(), "{}", tree.dump());
    h.click(Some(Role::ComboBox), "Group by");
    h.click(None, "Tool × model");
    h.until("tool/model group", |t| {
        t.has_containing("Claude Code · gpt-5.4-mini")
    });
}

#[test]
fn per_tool_stats_switches_keep_the_defaults_and_save_independently() {
    let defaults = superba_core::StatsLineSettings::default();
    assert!(!defaults.enabled);
    assert_eq!(
        defaults.harnesses,
        [RequestHarness::Codex, RequestHarness::ClaudeCode]
    );
    let on = stats_line::with_harness(&defaults, RequestHarness::Dsh, true);
    let off = stats_line::with_harness(&on, RequestHarness::Codex, false);
    assert_eq!(
        off.harnesses,
        [RequestHarness::ClaudeCode, RequestHarness::Dsh]
    );
    let service = fake();
    let mut h = AppHarness::new(service.clone(), Locale::En, Page::Router);
    let tree = h.wait_for(Some(Role::CheckBox), "dsh");
    assert!(tree.unnamed().is_empty(), "{}", tree.dump());
    h.click(Some(Role::CheckBox), "dsh");
    h.until("settings saved", |_| {
        service
            .saved_stats()
            .last()
            .is_some_and(|s| s.line.harnesses.contains(&RequestHarness::Dsh))
    });
    let saved = service.saved_stats();
    assert!(
        saved
            .last()
            .unwrap()
            .line
            .harnesses
            .contains(&RequestHarness::Codex)
    );
    assert!(
        saved
            .last()
            .unwrap()
            .line
            .harnesses
            .contains(&RequestHarness::ClaudeCode)
    );
}

#[tokio::test]
async fn ingress_counts_exclude_direct_and_old_records_and_urls_use_the_configured_port() {
    let service = fake();
    let mut rows = service.request_rows();
    let mut direct = rows[0].clone();
    direct.id = -1;
    direct.source = RequestSource::Rollout;
    let mut old = rows[1].clone();
    old.id = 100;
    old.started_at = Timestamp::now().saturating_sub(Duration::from_secs(90_000));
    rows.extend([direct, old]);
    service.set_request_rows(rows);
    assert_eq!(
        ingress::recent_counts(service.as_ref(), Timestamp::now())
            .await
            .unwrap(),
        [1, 1, 2]
    );
    assert_eq!(
        ingress::address(Some(18431), 2).as_deref(),
        Some("http://127.0.0.1:18431/openai/v1")
    );
    assert!(ingress::address(None, 0).is_none());
}
