#![allow(missing_docs)]

//! Providers (UX §3.3, plan §5 B6): the provider list (order, kinds, key-less
//! sites, the official login), the master/detail page in its Fit rectangle at
//! both window sizes and locales, the Connection tab (lines, auth, key, models
//! and settings summaries), the Account & usage tab and its fetch rules, the Add
//! provider dialog (presets with Krill first and pre-selected, preset and custom
//! flows, test results), commands, delete, and the helpers in `pages::shared`
//! it shares with Overview, the wizard and the router page.

mod support;

use superba_core::{
    AccountReport, AccountSummary, AuthChoice, AuthMode, BillingSummary, CodexPath, DailyUsage,
    LineCaps, LineId, Locale, ModelUsage, ProfileId, SiteId, SwitchService, Timestamp, UsageReport,
    Usd,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Cx, Data, Page, providers, shared};
use support::fake::{FakeOptions, FakeService};

#[test]
fn line_priority_moves_and_orders_lines() {
    let ids: Vec<LineId> = ["a", "b", "c"].into_iter().map(LineId::new).collect();
    assert_eq!(
        shared::moved(ids.clone(), 2, 0),
        vec![LineId::new("c"), LineId::new("a"), LineId::new("b")]
    );
    assert_eq!(
        shared::moved(ids.clone(), 0, 9),
        ids,
        "out of range: unchanged"
    );
    let site = superba_core::catalog::krill_site();
    let mut profile = shared::new_profile(&site, "Krill".into());
    assert_eq!(profile.auth_choice(), AuthChoice::Auto);
    assert_eq!(profile.auth_mode, AuthMode::Pat);
    let default: Vec<_> = shared::ordered_lines(&site, Some(&profile))
        .into_iter()
        .map(|l| l.id)
        .collect();
    assert_eq!(default[0], site.default_line.clone().unwrap());
    assert_eq!(default.len(), site.lines.len());
    profile.set_line_priority([LineId::new("cdn"), LineId::new("cn-fast")]);
    let ordered: Vec<_> = shared::ordered_lines(&site, Some(&profile))
        .into_iter()
        .map(|l| l.id.to_string())
        .collect();
    assert_eq!(ordered, vec!["cdn", "cn-fast", "overseas"]);
}

fn caps(site: &SiteId, line: &str, pat: bool) -> LineCaps {
    LineCaps {
        site_id: site.clone(),
        line_id: LineId::new(line),
        checked_at: Timestamp::EPOCH,
        reachable: true,
        latency_ms: Some(40),
        pat_ok: pat,
        apikey_ok: true,
        codex_path: Some(CodexPath::BackendApiCodex),
        blocked_by_edge: !pat,
        key_access: None,
        checks: vec![],
        detail: None,
    }
}

#[test]
fn automatic_auth_is_shown_per_line_and_left_to_the_planner_when_switching() {
    let site = superba_core::catalog::krill_site();
    let mut data = Data {
        sites: vec![site.clone()],
        ..Data::default()
    };
    data.caps
        .entry(site.id.clone())
        .or_default()
        .finish(Ok(vec![
            caps(&site.id, "cn-fast", false),
            caps(&site.id, "overseas", true),
        ]));
    let profile = shared::new_profile(&site, "Krill".into());
    let line = |id: &str| site.line(&LineId::new(id)).cloned();
    let caps_of = |id: &str| {
        data.caps[&site.id]
            .value
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.line_id == id)
    };
    let shown =
        |id: &str| shared::decision(&site, line(id).as_ref(), caps_of(id), Some(&profile)).mode;
    assert_eq!(shown("overseas"), Some(AuthMode::Pat));
    assert_eq!(shown("cn-fast"), Some(AuthMode::ApiKey));
    assert_eq!(shown("cdn"), Some(AuthMode::Pat), "not probed: PAT assumed");
    // Switching never pins a mode: an explicit one would become a manual choice.
    for id in ["overseas", "cn-fast"] {
        match shared::switch_action(&data, &profile, &LineId::new(id)) {
            Action::Switch { auth, line, .. } => {
                assert_eq!(auth, None);
                assert_eq!(line_str(line.as_ref()), id);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn line_str(line: Option<&LineId>) -> &str {
    line.map_or("", |l| l.as_str())
}

#[test]
fn usage_numbers_are_compact_grouped_and_axes_are_round() {
    assert_eq!(shared::group_thousands(0), "0");
    assert_eq!(shared::group_thousands(1_284), "1,284");
    assert_eq!(shared::group_thousands(1_234_567), "1,234,567");
    assert_eq!(shared::compact(9_999), "9,999");
    assert_eq!(shared::compact(12_900), "12.9K");
    assert_eq!(shared::compact(3_400_000), "3.4M");
    assert_eq!(shared::nice_axis(9.4), (10.0, 2.5));
    assert_eq!(shared::nice_axis(0.0), (1.0, 0.5));
    let (top, step) = shared::nice_axis(137.0);
    assert!(top >= 137.0 && (top / step).round() <= 5.0, "{top} {step}");
}

#[test]
fn key_less_custom_sites_get_an_api_key_profile_from_the_connection_page() {
    let mut site = superba_core::catalog::krill_site();
    site.id = SiteId::new("example");
    site.kind = superba_core::SiteKind::Compatible;
    site.capabilities.auth_modes = vec![AuthMode::ApiKey];
    let profile = shared::new_profile(&site, "Example".into());
    assert_eq!(profile.auth_mode, AuthMode::ApiKey);
    assert_eq!(profile.auth_choice(), AuthChoice::ApiKey);
    let line = site.lines[0].clone();
    let d = shared::decision(&site, Some(&line), None, Some(&profile));
    assert_eq!(d.choice, AuthChoice::Auto, "one mode: shown as Automatic");
    assert_eq!(d.mode, Some(AuthMode::ApiKey));
}

#[test]
fn automatic_authentication_shows_what_the_planner_will_pick() {
    let site = superba_core::catalog::krill_site();
    let line = site.lines[0].clone();
    let profile = shared::new_profile(&site, "Krill".into());
    assert_eq!(
        profile.codex_settings,
        superba_core::CodexSettings::krill_recommended()
    );
    let pick = |facts| shared::decision_with(&site, Some(&line), None, Some(&profile), facts);
    assert_eq!(pick(shared::AutoFacts::default()).mode, Some(AuthMode::Pat));
    let too_old = pick(shared::AutoFacts {
        pat_version_ok: Some(false),
        router: false,
    });
    assert_eq!(too_old.mode, Some(AuthMode::ApiKey));
    assert!(too_old.fell_back());
    let routed = pick(shared::AutoFacts {
        pat_version_ok: Some(true),
        router: true,
    });
    assert_eq!(routed.mode, Some(AuthMode::ApiKey));
    assert_eq!(routed.reason, superba_core::AuthReason::RouterMode);
    // A manual PAT choice is left to the version gate (blocked there, not here).
    let mut manual = profile.clone();
    manual.set_auth(AuthChoice::Pat);
    let d = shared::decision_with(
        &site,
        Some(&line),
        None,
        Some(&manual),
        shared::AutoFacts {
            pat_version_ok: Some(false),
            router: false,
        },
    );
    assert_eq!(d.mode, Some(AuthMode::Pat));
    // Router mode sends the key as an API key, also for a manual PAT choice
    // (user bug 78): what the Providers page says matches the plan.
    let d = shared::decision_with(
        &site,
        Some(&line),
        None,
        Some(&manual),
        shared::AutoFacts {
            pat_version_ok: Some(true),
            router: true,
        },
    );
    assert_eq!(
        (d.mode, d.reason),
        (Some(AuthMode::ApiKey), superba_core::AuthReason::RouterMode)
    );
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_gui::pages::providers::account::{self, needs_load};
use superba_gui::pages::providers::{AccountState, AddDialog, AddStep, Tab, ops};
use superba_gui::pages::shared::providers::{ProviderId, ProviderKind, list};
use support::fit::{SIZES, render_fit};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

async fn data_of(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

/// A custom site without a key and a second Krill provider.
async fn more_providers(service: &FakeService) {
    let site = superba_switch::new_compatible_site(
        SiteId::new("example"),
        "Example API",
        &url::Url::parse("https://api.example.com/v1").unwrap(),
    )
    .unwrap();
    service.save_site(site).await.unwrap();
    let krill = superba_core::catalog::krill_site();
    let mut second = shared::new_profile(&krill, "Krill 2".into());
    second.id = ProfileId::new("krill-2");
    service
        .save_profile(second, Some(superba_core::Secret::new("nb_second")))
        .await
        .unwrap();
}

fn with_cx<T>(
    data: &Data,
    service: &Arc<dyn SwitchService>,
    locale: Locale,
    f: impl FnOnce(&Cx<'_>) -> T,
) -> T {
    let tr = Tr::new(locale);
    let fonts_report = fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts_report,
        service,
        log: &[],
    };
    f(&cx)
}

/// Draw the page with `service` until `done` holds (or 3 s); returns the actions
/// and the texts of the last frame.
fn drive(
    data: &Data,
    service: Arc<dyn SwitchService>,
    state: &mut providers::State,
    locale: Locale,
    mut done: impl FnMut(&providers::State, &[Action]) -> bool,
) -> (Vec<Action>, String) {
    let ctx = egui::Context::default();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(locale);
    let mut actions = Vec::new();
    let mut text = String::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1060.0, 760.0),
                )),
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &report,
                    service: &service,
                    log: &[],
                };
                providers::show(ui, &cx, state, &mut actions);
            },
        );
        text = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.drop_without_applying_deltas();
        if done(state, &actions) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (actions, text)
}

#[tokio::test]
async fn the_list_puts_the_applied_provider_first_then_presets_custom_sites_and_the_official_login()
{
    let service = fake();
    more_providers(&service).await;
    let data = data_of(&service).await;
    let all = list(&data);
    let shown: Vec<(String, ProviderKind, bool)> = all
        .iter()
        .map(|p| {
            (
                match &p.id {
                    ProviderId::Profile(id) => id.to_string(),
                    ProviderId::Site(id) => format!("site:{id}"),
                },
                p.kind,
                p.applied,
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            ("krill".into(), ProviderKind::Preset, true),
            ("krill-2".into(), ProviderKind::Preset, false),
            ("site:example".into(), ProviderKind::Custom, false),
            ("official".into(), ProviderKind::Official, false),
        ]
    );
    assert!(!all[2].has_key(), "the key-less site gets its key here");
    assert!(all[3].has_key(), "the official login needs no key");
}

fn account_state(site: &SiteId, days: u32) -> AccountState {
    let mut acc = AccountState {
        days,
        ..AccountState::default()
    };
    account::finish(
        &mut acc,
        days,
        Ok(AccountReport {
            profile_id: ProfileId::new("krill"),
            profile_name: "Krill".into(),
            site_id: site.clone(),
            line_id: LineId::new("overseas"),
            summary: Some(AccountSummary {
                email: Some("dev@example.com".into()),
                billing: Some(BillingSummary {
                    limit: Usd::from_dollars(300.0),
                    used: Usd::from_dollars(139.42),
                    remaining: Usd::from_dollars(160.58),
                    ..BillingSummary::default()
                }),
                ..AccountSummary::default()
            }),
            summary_error: None,
            usage: Some(UsageReport {
                site_id: site.clone(),
                start: Timestamp::EPOCH,
                end: Timestamp::now(),
                days,
                daily: (0..i64::from(days))
                    .map(|i| DailyUsage {
                        start: Timestamp::from_millis(i * 86_400_000),
                        cost: Usd::from_dollars(4.0 + i as f64),
                        requests: Some(300),
                        input_tokens: Some(1_000_000),
                        output_tokens: Some(200_000),
                    })
                    .collect(),
                models: (0..14)
                    .map(|i| ModelUsage {
                        model: format!("model-{i}"),
                        cost: Usd::from_dollars(1.0 + i as f64),
                        requests: 2_100,
                        input_tokens: 7_000_000,
                        cached_input_tokens: 3_000_000,
                        output_tokens: 1_400_000,
                    })
                    .collect(),
                total_cost: Usd::from_dollars(46.53),
                cost_source: Some(superba_core::CostSource::Costs),
                usage_available: true,
                fetched_at: Timestamp::now(),
                from_cache: false,
                notice: None,
            }),
            usage_error: None,
        }),
    );
    acc
}

#[tokio::test]
async fn providers_fit_both_sizes_and_locales_on_both_tabs() {
    let service = fake();
    let data = data_of(&service).await;
    let site = SiteId::new("krill");
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            for tab in [Tab::Connection, Tab::Account] {
                let mut state = providers::State::default();
                state.tab = tab;
                state
                    .accounts
                    .insert(ProfileId::new("krill"), account_state(&site, 7));
                let report = render_fit(size, locale, egui::Theme::Dark, &data, |ui, cx, out| {
                    providers::show(ui, cx, &mut state, out);
                });
                assert!(!report.scrolled, "{size:?} {locale:?} {tab:?}");
                assert_eq!(report.scroll_areas, 0, "{size:?} {locale:?} {tab:?}");
                assert!(report.fits(), "{size:?} {locale:?} {tab:?}: {report:?}");
                let text = report.all_text();
                assert!(text.contains("Krill"), "{text}");
                match tab {
                    Tab::Connection => {
                        assert!(text.contains("nb_…"), "the key shows only its tail: {text}");
                        assert!(text.contains("api.krill-code.net"), "{text}");
                    }
                    Tab::Account => {
                        assert!(text.contains("$46.53"), "{text}");
                        assert!(text.contains("$160.58"), "{text}");
                        assert!(text.contains("model-0"), "{text}");
                    }
                }
                assert!(!text.to_lowercase().contains("demo"), "{text}");
            }
        }
    }
}

#[test]
fn the_account_tab_fetches_only_without_data_for_the_range_and_keeps_a_report_on_429() {
    let site = SiteId::new("krill");
    let fresh = AccountState {
        days: 7,
        ..AccountState::default()
    };
    assert!(needs_load(&fresh, true));
    let mut acc = account_state(&site, 7);
    assert!(
        !needs_load(&acc, true),
        "opening the tab again fetches nothing"
    );
    acc.days = 30;
    assert!(needs_load(&acc, true), "another range is fetched once");
    assert!(
        !needs_load(&acc, false),
        "no usage API: the balance is enough"
    );
    // The site refuses the 30-day report: the 7-day one stays, with a note.
    account::finish(
        &mut acc,
        30,
        Err(superba_core::Error::Http {
            status: 429,
            url: "https://relay.example/v1/organization/costs".into(),
            message: "rate limited".into(),
        }),
    );
    assert_eq!(acc.limited, Some(30));
    assert_eq!(acc.days, 7);
    assert_eq!(
        acc.report
            .as_ref()
            .and_then(|r| r.usage.as_ref())
            .map(|u| u.days),
        Some(7)
    );
    assert!(!needs_load(&acc, true));
    // An error stops automatic retries (Refresh asks again).
    let mut failed = AccountState {
        days: 7,
        ..AccountState::default()
    };
    account::finish(&mut failed, 7, Err(superba_core::Error::other("offline")));
    assert!(!needs_load(&failed, true));
}

#[tokio::test]
async fn the_account_tab_asks_once_per_provider_and_range() {
    let service = fake();
    let data = data_of(&service).await;
    let mut state = providers::State::default();
    state.tab = Tab::Account;
    let svc: Arc<dyn SwitchService> = service.clone();
    let (_, text) = drive(&data, svc.clone(), &mut state, Locale::En, |s, _| {
        s.accounts
            .get(&ProfileId::new("krill"))
            .is_some_and(|a| a.report.is_some())
    });
    assert!(text.contains("Krill"), "{text}");
    drive(&data, svc, &mut state, Locale::En, |_, _| true);
    assert_eq!(
        service.account_calls(),
        vec![(Some(ProfileId::new("krill")), Some(7), false)]
    );
}

#[test]
fn add_provider_offers_the_presets_with_krill_first_and_pre_selected() {
    let presets = superba_core::catalog::presets();
    assert_eq!(presets[0].id, superba_core::catalog::PRESET_KRILL);
    assert!(presets[0].default);
    let d = AddDialog::new();
    assert_eq!(d.step, AddStep::Choose);
    assert_eq!(d.pick, superba_core::catalog::PRESET_KRILL);
}

#[tokio::test]
async fn adding_a_preset_provider_saves_its_key_and_switches_to_it() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let mut d = AddDialog::new();
    d.step = AddStep::Preset;
    d.name = "Krill 2".into();
    d.key = "nb_second_key".into();
    with_cx(&data, &svc, Locale::En, |cx| d.start_save(cx, true));
    state.add = Some(d);
    let (actions, _) = drive(&data, svc, &mut state, Locale::En, |_, a| {
        a.iter().any(|a| matches!(a, Action::Switch { .. }))
    });
    let set = service.profile_set().await.unwrap();
    let added = set
        .profile(&ProfileId::new("krill-2"))
        .expect("a second Krill provider");
    assert_eq!(added.name, "Krill 2");
    assert!(added.key_ref.is_some());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Switch { profile, .. } if profile.as_str() == "krill-2"
        )),
        "{actions:?}"
    );
    assert!(state.add.is_none(), "the dialog closes");
    assert_eq!(
        state.selected,
        Some(ProviderId::Profile(ProfileId::new("krill-2")))
    );
}

fn tested_report() -> ops::SiteReport {
    ops::SiteReport {
        at: Some(Timestamp::now()),
        caps: Some(Ok(LineCaps {
            reachable: true,
            latency_ms: Some(210),
            ..caps(&SiteId::new("superba-site-test"), "default", false)
        })),
        key: Some(Ok(superba_core::KeyCheck {
            site_id: SiteId::new("superba-site-test"),
            line_id: LineId::new("default"),
            checked_at: Timestamp::now(),
            key_last4: "a91f".into(),
            whoami: None,
            identity: None,
            access: superba_core::KeyAccess::Ok,
            codex_path: Some(CodexPath::AsIs),
        })),
        models: Some(Ok((0..37)
            .map(|i| superba_core::ModelInfo {
                id: format!("model-{i}"),
                ..Default::default()
            })
            .collect())),
        account: None,
        mapped: Vec::new(),
        responses: Some(Ok(superba_core::ResponsesProbe {
            ok: true,
            status: Some(200),
            latency_ms: Some(600),
            compact_ok: None,
            detail: None,
        })),
    }
}

#[tokio::test]
async fn a_custom_site_must_pass_its_test_before_add_and_switch() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut d = AddDialog::new();
    d.step = AddStep::Custom;
    d.name = "My API".into();
    d.url = "https://api.example.com/v1".into();
    d.key = "sk-test-a91f".into();
    with_cx(&data, &svc, Locale::En, |cx| d.start_save(cx, true));
    assert!(!d.saving());
    assert_eq!(
        d.error.as_deref(),
        Some("Test the site first (with the address and API key above).")
    );
    d.set_report(tested_report());
    assert!(d.tested_ok());
    // Editing the key after the test asks for a new test.
    d.key.push('x');
    assert!(!d.tested_ok());
    d.key.pop();
    with_cx(&data, &svc, Locale::En, |cx| d.start_save(cx, true));
    assert!(d.saving());
    let mut state = providers::State::default();
    state.add = Some(d);
    let (actions, _) = drive(&data, svc, &mut state, Locale::En, |_, a| {
        a.iter().any(|a| matches!(a, Action::Switch { .. }))
    });
    let set = service.profile_set().await.unwrap();
    assert!(set.custom_sites.iter().any(|s| s.id.as_str() == "my-api"));
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Switch { profile, .. } if profile.as_str() == "my-api"
        )),
        "{actions:?}"
    );
}

#[tokio::test]
async fn test_results_show_latency_key_model_count_and_the_responses_call() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut d = AddDialog::new();
    d.step = AddStep::Custom;
    d.url = "https://api.example.com/v1".into();
    d.set_report(tested_report());
    let mut state = providers::State::default();
    state.add = Some(d);
    let mut frames = 0;
    let (_, text) = drive(&data, svc, &mut state, Locale::ZhCn, |_, _| {
        frames += 1;
        frames > 2
    });
    assert!(text.contains("210 ms"), "{text}");
    assert!(text.contains("这个站点提供了 37 个模型"), "{text}");
    // "Responses" with a drawn check mark.
    assert!(text.lines().any(|l| l == "Responses"), "{text}");
}

#[tokio::test]
async fn commands_pick_tabs_and_providers() {
    let service = fake();
    more_providers(&service).await;
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let mut out = Vec::new();
    with_cx(&data, &svc, Locale::En, |cx| {
        for (command, tab) in [
            ("tab:usage", Tab::Account),
            ("tab:account", Tab::Account),
            ("tab:connection", Tab::Connection),
        ] {
            providers::command(&mut state, cx, command, &mut out);
            assert_eq!(state.tab, tab, "{command}");
        }
        providers::command(&mut state, cx, "profile:krill-2", &mut out);
        assert_eq!(
            state.selected,
            Some(ProviderId::Profile(ProfileId::new("krill-2")))
        );
        providers::command(&mut state, cx, "site:example", &mut out);
        assert_eq!(
            state.selected,
            Some(ProviderId::Site(SiteId::new("example")))
        );
        assert!(!providers::status(&state).modal);
        providers::command(&mut state, cx, "add", &mut out);
        assert!(providers::status(&state).modal, "`add` opens the dialog");
        // Every palette entry of the page is addressed to Providers.
        for item in providers::palette(cx, &state) {
            if let Action::PageCommand { page, .. } = item.action {
                assert_eq!(page, Page::Providers);
            }
        }
    });
    assert!(out.is_empty(), "{out:?}");
}

#[tokio::test]
async fn the_applied_provider_cannot_be_deleted_and_another_one_can() {
    let service = fake();
    more_providers(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let err = ops::remove_provider(
        svc.clone(),
        Some(ProfileId::new("krill")),
        SiteId::new("krill"),
        false,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, superba_core::Error::Conflict { .. }),
        "{err:?}"
    );
    ops::remove_provider(
        svc.clone(),
        Some(ProfileId::new("krill-2")),
        SiteId::new("krill"),
        false,
    )
    .await
    .unwrap();
    ops::remove_provider(svc, None, SiteId::new("example"), true)
        .await
        .unwrap();
    let set = service.profile_set().await.unwrap();
    assert!(set.profile(&ProfileId::new("krill-2")).is_none());
    assert!(set.profile(&ProfileId::new("krill")).is_some());
    assert!(set.custom_sites.is_empty());
}

#[tokio::test]
async fn the_custom_test_probes_the_typed_site_through_the_service_without_saving_it() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut d = AddDialog::new();
    d.step = AddStep::Custom;
    d.name = "My API".into();
    d.url = "https://api.example.com/v1".into();
    d.key = "sk-test-a91f".into();
    with_cx(&data, &svc, Locale::En, |cx| d.start_test(cx));
    let mut state = providers::State::default();
    state.add = Some(d);
    drive(&data, svc, &mut state, Locale::En, |s, _| {
        s.add.as_ref().is_some_and(AddDialog::tested_ok)
    });
    assert!(state.add.as_ref().unwrap().tested_ok());
    let probed = service.unsaved_probes();
    assert_eq!(probed.len(), 1);
    assert!(
        probed[0].lines[0]
            .origin
            .to_string()
            .contains("api.example.com")
    );
    // Nothing was stored by the test.
    let set = service.profile_set().await.unwrap();
    assert!(set.custom_sites.is_empty());
}

#[tokio::test]
async fn the_official_login_detail_says_when_the_login_was_saved() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let mut out = Vec::new();
    with_cx(&data, &svc, Locale::En, |cx| {
        providers::command(&mut state, cx, "profile:official", &mut out);
    });
    let mut frames = 0;
    let (_, text) = drive(&data, svc, &mut state, Locale::En, |_, _| {
        frames += 1;
        frames > 2
    });
    assert!(
        text.contains("Your ChatGPT login is kept encrypted") && text.contains("ago"),
        "{text}"
    );
}

#[tokio::test]
async fn the_base_url_row_of_a_custom_provider_shows_its_whole_auth_note() {
    // Review ux-visual #12: the auth note beside the Base URL field was cut to
    // "仅 …".
    let service = fake();
    more_providers(&service).await;
    let site = SiteId::new("example");
    // With a key: the WS / zstd badges sit in the row too.
    let example = service
        .sites()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == site)
        .unwrap();
    let mut profile = shared::new_profile(&example, "Example API".into());
    profile.id = ProfileId::new("example");
    service
        .save_profile(profile, Some(superba_core::Secret::new("sk-example")))
        .await
        .unwrap();
    let mut data = data_of(&service).await;
    let mut caps = service.probe_lines(&site).await.unwrap();
    for c in &mut caps {
        // An OpenAI-compatible site: API key only.
        c.pat_ok = false;
    }
    data.caps.entry(site.clone()).or_default().finish(Ok(caps));
    for (locale, note) in [(Locale::ZhCn, "仅 API Key"), (Locale::En, "API key only")] {
        for size in support::fit::SIZES {
            let mut state = providers::State::default();
            state.select(ProviderId::Profile(ProfileId::new("example")));
            let report =
                support::fit::render_fit(size, locale, egui::Theme::Light, &data, |ui, cx, out| {
                    providers::show(ui, cx, &mut state, out)
                });
            assert!(
                report.whole(note),
                "{locale:?} {size:?}: {:?}",
                report
                    .text_shapes
                    .iter()
                    .filter(|t| t.2)
                    .collect::<Vec<_>>()
            );
        }
    }
}
