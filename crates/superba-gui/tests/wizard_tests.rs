#![allow(missing_docs)]

//! Setup wizard (UX §3.2, plan §5 B5): Krill is pre-selected on a first run, a
//! re-run pre-selects the applied provider and edits it instead of creating a
//! duplicate, the custom-site path saves, probes and finishes with the saved
//! profile, "Skip" discards the draft, and every entry point opens the wizard.

mod support;

use std::sync::Arc;
use std::time::Duration;

use superba_core::{
    ActiveState, AuthMode, Locale, RestoreTarget, SiteKind, SwitchIntent, SwitchService, Timestamp,
    catalog,
};
use superba_gui::design::Palette;
use superba_gui::i18n::Tr;
use superba_gui::pages::wizard::{self, Step, Wizard, WizardOutcome};
use superba_gui::pages::{Action, Cx, Data, overview, superba::general};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

fn fake(first_run: bool) -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        first_run,
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
    data.sessions.finish(service.sessions().await);
    data
}

/// Run `f` with a [`Cx`] over `data` and `service`.
fn with_cx<R>(
    data: &Data,
    service: &Arc<dyn SwitchService>,
    locale: Locale,
    f: impl FnOnce(&Cx<'_>) -> R,
) -> R {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::DARK,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts,
        service,
        log: &[],
    };
    f(&cx)
}

#[tokio::test]
async fn the_default_preset_is_preselected_on_a_first_run() {
    let service = fake(true);
    let data = data_of(&service).await;
    assert!(data.providers().is_empty());
    let w = Wizard::start(&data);
    assert_eq!(w.preset, catalog::PRESET_KRILL);
    assert_eq!(
        w.presets.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        [
            catalog::PRESET_KRILL,
            catalog::PRESET_OFFICIAL,
            catalog::PRESET_CUSTOM
        ],
        "quick-pick order: Krill, official login, custom site"
    );
    assert!(w.presets[0].default);
    assert!(!w.rerun);
    assert_eq!(w.editing, None, "a first run creates the profile");
    assert_eq!(w.step, Step::Detect);
    assert_eq!(
        w.site.as_ref().map(|s| s.kind),
        Some(SiteKind::Krill),
        "the preset's site with its lines"
    );
    // Steps: the official login skips "Line & models".
    let mut w = w;
    w.step = Step::Credentials;
    assert_eq!(w.next_step(), Some(Step::LineModels));
    w.select_preset(&data, catalog::PRESET_OFFICIAL);
    assert_eq!(w.next_step(), Some(Step::Confirm));
    w.step = Step::Confirm;
    assert_eq!(w.prev_step(), Some(Step::Credentials));
}

#[tokio::test]
async fn a_rerun_edits_the_applied_provider_instead_of_duplicating_it() {
    let service = fake(false);
    let data = data_of(&service).await;
    let before = service.profile_set().await.unwrap().profiles.len();
    let applied = data.active_profile().unwrap().clone();
    let w = Wizard::start(&data);
    assert!(w.rerun);
    assert_eq!(w.preset, catalog::PRESET_KRILL);
    assert_eq!(w.editing.as_ref(), Some(&applied.id));
    // Walk to the end with another line and a default model.
    let mut w = w;
    let other = w
        .site
        .as_ref()
        .unwrap()
        .lines
        .iter()
        .find(|l| Some(&l.id) != applied.line_id.as_ref())
        .unwrap()
        .id
        .clone();
    w.line = Some(other.clone());
    w.model = Some("gpt-6-luna".into());
    w.step = Step::Confirm;
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    let mut out = Vec::new();
    // The router model list starts at the saved one (Codex models only) and
    // is handed back only when it was changed.
    assert_eq!(w.catalog_mode, superba_core::CatalogMode::Codex);
    let unchanged = with_cx(&data, &dyn_service, Locale::En, |cx| {
        wizard::finish(cx, &w, &mut out)
    });
    assert!(
        matches!(
            unchanged,
            WizardOutcome::Finish {
                catalog_mode: None,
                ..
            }
        ),
        "{unchanged:?}"
    );
    w.catalog_mode = superba_core::CatalogMode::Prefixed;
    let outcome = with_cx(&data, &dyn_service, Locale::En, |cx| {
        wizard::finish(cx, &w, &mut out)
    });
    let WizardOutcome::Finish {
        site,
        profile,
        key,
        request,
        catalog_mode,
    } = outcome
    else {
        panic!("expected Finish, got {outcome:?}");
    };
    assert_eq!(catalog_mode, Some(superba_core::CatalogMode::Prefixed));
    assert!(site.is_none(), "a preset site is never saved");
    assert_eq!(profile.id, applied.id, "the existing profile is edited");
    assert_eq!(profile.key_ref, applied.key_ref, "its key is kept");
    assert_eq!(profile.line_priority()[0], other);
    assert_eq!(profile.model.model.as_deref(), Some("gpt-6-luna"));
    assert!(key.is_none(), "no new key typed");
    assert!(
        matches!(&request.intent, SwitchIntent::Profile { profile_id, .. } if *profile_id == applied.id)
    );
    service.save_profile(*profile, key).await.unwrap();
    assert_eq!(
        service.profile_set().await.unwrap().profiles.len(),
        before,
        "no duplicate profile"
    );
}

#[tokio::test]
async fn a_rerun_with_the_official_login_applied_preselects_it_and_restores() {
    let service = fake(false);
    let mut data = data_of(&service).await;
    let o = data.overview.value.as_mut().unwrap();
    o.active = Some(ActiveState {
        profile_id: None,
        site_id: Some(catalog::OFFICIAL_SITE_ID.into()),
        line_id: None,
        auth_mode: Some(AuthMode::OfficialChatGpt),
        generation: 3,
        applied_at: Timestamp::EPOCH,
    });
    let mut w = Wizard::start(&data);
    assert_eq!(w.preset, catalog::PRESET_OFFICIAL);
    assert!(w.official());
    w.step = Step::Confirm;
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    let mut out = Vec::new();
    let outcome = with_cx(&data, &dyn_service, Locale::ZhCn, |cx| {
        wizard::finish(cx, &w, &mut out)
    });
    assert!(matches!(outcome, WizardOutcome::Close));
    assert!(matches!(
        out.as_slice(),
        [Action::Restore(RestoreTarget::Official)]
    ));
}

/// Draw the wizard until `done` (background tasks finish between frames).
fn drive(
    harness: &Harness,
    data: &Data,
    service: &Arc<dyn SwitchService>,
    w: &mut Wizard,
    out: &mut Vec<Action>,
    done: impl Fn(&Wizard) -> bool,
) {
    let tr = Tr::new(Locale::En);
    for _ in 0..200 {
        if done(w) {
            return;
        }
        harness.frame(Vec::new(), &mut |ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: false,
                now: Timestamp::now(),
                fonts: &harness.fonts,
                service,
                log: &[],
            };
            let _ = wizard::show(ui.ctx(), &cx, w, out);
        });
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the wizard did not reach the expected state: {w:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_custom_site_is_tested_before_it_is_saved_and_saved_only_on_apply() {
    let service = fake(true);
    let data = data_of(&service).await;
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    let mut w = Wizard::start(&data);
    w.select_preset(&data, catalog::PRESET_CUSTOM);
    assert!(w.custom());
    assert_eq!(w.site, None, "a new custom site");
    w.step = Step::Credentials;
    w.custom_name = "My API".into();
    w.custom_url = "https://api.example.com/v1".into();
    w.key = "sk-wizard-test-0042".into();
    let mut out = Vec::new();
    with_cx(&data, &dyn_service, Locale::En, |cx| {
        assert!(wizard::can_continue(cx, &w));
        wizard::advance(cx, &mut w, &mut out);
    });
    assert_eq!(w.step, Step::LineModels, "the form became a draft");
    assert!(
        service.profile_set().await.unwrap().custom_sites.is_empty(),
        "nothing is saved before Apply"
    );
    // The draft is tested as it is (`SiteProbeRequest::site`) with the typed key.
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    drive(&harness, &data, &dyn_service, &mut w, &mut out, |w| {
        w.custom_test_passed()
    });
    let probed = service.unsaved_probes();
    assert_eq!(probed.len(), 1);
    assert_eq!(probed[0].id, "my-api");
    assert!(service.profile_set().await.unwrap().custom_sites.is_empty());
    // Apply hands the app the site, the profile (with the chosen model) and the
    // key to save, then the switch to plan.
    w.model = Some("my-model".into());
    w.step = Step::Confirm;
    let outcome = with_cx(&data, &dyn_service, Locale::En, |cx| {
        wizard::finish(cx, &w, &mut out)
    });
    let WizardOutcome::Finish {
        site,
        profile,
        key,
        request,
        ..
    } = outcome
    else {
        panic!("expected Finish, got {outcome:?}");
    };
    let site = site.expect("the custom site is saved with the profile");
    assert_eq!(site.id, "my-api");
    assert_eq!(
        wizard::site_base_url(&site),
        "https://api.example.com/v1",
        "the base path survives"
    );
    assert_eq!(profile.id, "my-api");
    assert_eq!(profile.model.model.as_deref(), Some("my-model"));
    assert_eq!(
        key.as_ref().map(|k| k.expose()),
        Some("sk-wizard-test-0042")
    );
    assert!(matches!(
        &request.intent,
        superba_core::SwitchIntent::Profile { profile_id, .. } if profile_id.as_str() == "my-api"
    ));
    // What the app does with it (`Command::SaveSiteAndProfile`).
    dyn_service.save_site(*site).await.unwrap();
    dyn_service.save_profile(*profile, key).await.unwrap();

    // A re-run with that site applied pre-fills the form and edits it.
    let mut data = data;
    data.profiles.finish(service.profile_set().await);
    data.sites = service.sites().await.unwrap();
    data.overview.value.as_mut().unwrap().active = Some(ActiveState {
        profile_id: Some("my-api".into()),
        site_id: Some("my-api".into()),
        line_id: None,
        auth_mode: Some(AuthMode::ApiKey),
        generation: 1,
        applied_at: Timestamp::EPOCH,
    });
    let again = Wizard::start(&data);
    assert_eq!(again.preset, catalog::PRESET_CUSTOM);
    assert_eq!(again.custom_name, "My API");
    assert_eq!(again.custom_url, "https://api.example.com/v1");
    assert_eq!(again.editing.as_ref().map(|p| p.as_str()), Some("my-api"));
}

#[tokio::test]
async fn the_custom_form_refuses_a_bad_address_without_writing() {
    let service = fake(true);
    let data = data_of(&service).await;
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    let mut w = Wizard::start(&data);
    w.select_preset(&data, catalog::PRESET_CUSTOM);
    w.step = Step::Credentials;
    w.custom_name = "Bad".into();
    w.custom_url = "not a url".into();
    w.key = "sk-x".into();
    let mut out = Vec::new();
    with_cx(&data, &dyn_service, Locale::En, |cx| {
        wizard::advance(cx, &mut w, &mut out);
    });
    assert_eq!(w.step, Step::Credentials);
    assert!(out.is_empty());
    assert!(service.profile_set().await.unwrap().custom_sites.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn skipping_discards_the_unsaved_provider_draft() {
    for key in [None, Some(egui::Key::Escape)] {
        let service = fake(true);
        let data = data_of(&service).await;
        let before = service.profile_set().await.unwrap();
        let dyn_service: Arc<dyn SwitchService> = service.clone();
        let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
        let mut w = Wizard::start(&data);
        let mut out = Vec::new();
        let mut closed = false;
        let tr = Tr::new(Locale::ZhCn);
        let mut draw = |ui: &mut egui::Ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data: &data,
                busy: false,
                now: Timestamp::now(),
                fonts: &harness.fonts,
                service: &dyn_service,
                log: &[],
            };
            if matches!(
                wizard::show(ui.ctx(), &cx, &mut w, &mut out),
                WizardOutcome::Close
            ) {
                closed = true;
            }
        };
        match key {
            Some(k) => harness.key(k, &mut draw),
            None => assert!(harness.click_text(&tr.t("wizard.skip"), &mut draw)),
        }
        assert!(closed, "{key:?} closes the wizard");
        assert!(
            out.iter().all(|a| matches!(a, Action::LoadSessions)),
            "only reads: {out:?}"
        );
        assert_eq!(service.profile_set().await.unwrap(), before);
    }
}

#[tokio::test]
async fn every_entry_point_opens_the_wizard_with_an_existing_profile() {
    let service = fake(false);
    let data = data_of(&service).await;
    assert!(!data.providers().is_empty());
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    // Preferences is a Scroll page: a tall window shows all of it.
    let tall = Harness::new([1280.0, 2600.0], egui::Theme::Dark);
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        // Preferences → Setup wizard → Run setup again.
        let mut out = Vec::new();
        let mut settings = superba_core::Settings::default();
        let mut prefs = |ui: &mut egui::Ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data: &data,
                busy: false,
                now: Timestamp::now(),
                fonts: &tall.fonts,
                service: &dyn_service,
                log: &[],
            };
            general::show(ui, &cx, &mut settings, &mut out);
        };
        // The row title and its button share the text: the button is drawn last.
        assert!(tall.click_last_text(&tr.t("prefs.wizard.run"), &mut prefs));
        assert!(
            out.iter().any(|a| matches!(a, Action::StartWizard)),
            "Preferences ({locale:?}): {out:?}"
        );
        // The `wizard` page command (tray, deep links).
        let mut out = Vec::new();
        with_cx(&data, &dyn_service, locale, |cx| {
            overview::command(&mut overview::State::default(), cx, "wizard", &mut out);
        });
        assert!(matches!(out.as_slice(), [Action::StartWizard]));
    }
    // A re-run opened from any of them edits the existing profile.
    let w = Wizard::start(&data);
    assert!(w.rerun && w.editing.is_some());
}

#[tokio::test]
async fn the_overview_empty_state_starts_the_wizard() {
    let service = fake(true);
    let data = data_of(&service).await;
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    let tr = Tr::new(Locale::ZhCn);
    let mut out = Vec::new();
    let mut state = overview::State::default();
    let mut draw = |ui: &mut egui::Ui| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &harness.fonts,
            service: &dyn_service,
            log: &[],
        };
        assert!(overview::is_empty(&cx));
        overview::show(ui, &cx, &mut state, &mut out);
    };
    let texts = harness.settle(&mut draw);
    assert!(
        texts
            .iter()
            .any(|(t, _)| *t == tr.t("overview.empty.title")),
        "{texts:?}"
    );
    assert!(harness.click_text(&tr.t("overview.empty.start"), &mut draw));
    assert!(
        out.iter().any(|a| matches!(a, Action::StartWizard)),
        "{out:?}"
    );
}

/// Shell data of `service` with the probed lines of the Krill site (the app
/// fills them on Action::Probe); `manual_pat`: the applied provider chose PAT
/// by hand.
async fn data_with_caps(service: &FakeService, manual_pat: bool) -> Data {
    let mut data = data_of(service).await;
    if manual_pat && let Some(set) = data.profiles.value.as_mut() {
        for p in &mut set.profiles {
            p.set_auth(superba_core::AuthChoice::Pat);
        }
    }
    let site = superba_core::SiteId::from("krill");
    let caps = service.probe_lines(&site).await.unwrap();
    data.caps.entry(site).or_default().finish(Ok(caps));
    data
}

/// Draw `n` frames of the wizard (background reads finish in between); the
/// texts of the last one.
fn wizard_frames(
    harness: &Harness,
    data: &Data,
    service: &Arc<dyn SwitchService>,
    w: &mut Wizard,
    locale: Locale,
    n: usize,
) -> Vec<String> {
    let tr = Tr::new(locale);
    let mut texts = Vec::new();
    let mut out = Vec::new();
    for _ in 0..n {
        texts = harness.frame(Vec::new(), &mut |ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: false,
                now: Timestamp::now(),
                fonts: &harness.fonts,
                service,
                log: &[],
            };
            let _ = wizard::show(ui.ctx(), &cx, w, &mut out);
        });
        std::thread::sleep(Duration::from_millis(10));
    }
    texts.into_iter().map(|(t, _)| t).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fastest_line_is_preselected_only_on_a_first_run() {
    // Review ux-visual #6: a re-run moved the provider to the fastest line.
    let fastest = superba_core::LineId::from("cn-fast");
    for first_run in [true, false] {
        let service = fake(first_run);
        let data = data_with_caps(&service, false).await;
        let dyn_service: Arc<dyn SwitchService> = service.clone();
        let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
        let mut w = Wizard::start(&data);
        assert_eq!(w.rerun, !first_run);
        if !first_run {
            assert_eq!(w.line, Some("overseas".into()), "the applied line");
            assert!(w.line_chosen, "a re-run keeps it");
        }
        w.key = "nb_wizard_test_key_0123456789abcdef".into();
        w.step = Step::Credentials;
        let mut out = Vec::new();
        with_cx(&data, &dyn_service, Locale::ZhCn, |cx| {
            wizard::advance(cx, &mut w, &mut out)
        });
        assert_eq!(w.step, Step::LineModels);
        let mut texts = Vec::new();
        for _ in 0..30 {
            texts = wizard_frames(&harness, &data, &dyn_service, &mut w, Locale::ZhCn, 5);
            if first_run && w.line.as_ref() == Some(&fastest) {
                break;
            }
        }
        if first_run {
            assert_eq!(w.line.as_ref(), Some(&fastest), "a new provider: fastest");
            assert!(texts.iter().any(|t| t.contains("已预选最快的线路")));
        } else {
            assert_eq!(w.line, Some("overseas".into()), "the probe kept it");
            assert!(
                texts
                    .iter()
                    .any(|t| t.contains("已选中这个供应商当前使用的线路")),
                "{texts:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_keeps_the_auth_choice_and_confirm_shows_old_and_new_values() {
    // Review ux-visual #6: Confirm said "Automatic → API key" with nothing
    // showing that line and auth change, and Apply reset a manual PAT choice.
    let service = fake(false);
    let data = data_with_caps(&service, true).await;
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    let harness = Harness::new([1280.0, 900.0], egui::Theme::Light);
    let site = data.site(&"krill".into()).unwrap();
    let tr = Tr::new(Locale::ZhCn);
    let name = |id: &str| tr.text(&site.line(&id.into()).unwrap().name);

    // Same line: the manual PAT choice is kept, Confirm says nothing changes.
    let mut w = Wizard::start(&data);
    w.step = Step::Confirm;
    let texts = wizard_frames(&harness, &data, &dyn_service, &mut w, Locale::ZhCn, 4);
    for want in ["与现在相比", "认证方式：PAT（手动选择）"] {
        assert!(texts.iter().any(|t| t.contains(want)), "{want}: {texts:?}");
    }
    assert_eq!(
        texts.iter().filter(|t| *t == "不变").count(),
        3,
        "line, auth and model unchanged: {texts:?}"
    );
    let outcome = with_cx(&data, &dyn_service, Locale::ZhCn, |cx| {
        wizard::finish(cx, &w, &mut Vec::new())
    });
    let WizardOutcome::Finish { profile, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(profile.auth_choice(), superba_core::AuthChoice::Pat, "kept");
    assert_eq!(profile.line_priority()[0], "overseas");

    // China Express blocks PAT: the manual choice cannot work there, so Apply
    // uses Automatic (an API key) and Confirm shows each change as old → new.
    let mut w = Wizard::start(&data);
    w.line = Some("cn-fast".into());
    w.line_chosen = true;
    w.model = Some("gpt-6-luna".into());
    w.step = Step::Confirm;
    let texts = wizard_frames(&harness, &data, &dyn_service, &mut w, Locale::ZhCn, 4);
    for want in [
        name("overseas"),
        name("cn-fast"),
        "PAT".to_owned(),
        "自动（API Key）".to_owned(),
        "Codex 默认".to_owned(),
        "gpt-6-luna".to_owned(),
    ] {
        assert!(texts.contains(&want), "{want}: {texts:?}");
    }
    assert_eq!(texts.iter().filter(|t| *t == "→").count(), 3, "{texts:?}");
    assert!(!texts.iter().any(|t| t == "不变"), "{texts:?}");
    let outcome = with_cx(&data, &dyn_service, Locale::ZhCn, |cx| {
        wizard::finish(cx, &w, &mut Vec::new())
    });
    let WizardOutcome::Finish { profile, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(profile.auth_choice(), superba_core::AuthChoice::Auto);
    assert_eq!(profile.line_priority()[0], "cn-fast");
}

#[tokio::test(flavor = "multi_thread")]
async fn detect_counts_the_hosts_the_app_knows_and_how_many_are_online() {
    // Review ux-visual #15: Detect said "app 已连接 2 台远程主机" with a green dot
    // while none of them was connected.
    use superba_gui::design::widgets::Status;
    let service = fake(false);
    let dyn_service: Arc<dyn SwitchService> = service.clone();
    for (online, locale, want) in [
        (false, Locale::ZhCn, "app 记录了 2 台远程主机（0 台在线）"),
        (true, Locale::ZhCn, "app 记录了 2 台远程主机（1 台在线）"),
        (false, Locale::En, "The app knows 2 remote hosts (0 online)"),
    ] {
        let mut data = data_of(&service).await;
        let mut hosts = service.hosts().await.unwrap();
        hosts.retain(|h| h.host.is_app_known());
        for h in &mut hosts {
            h.host.connected = Some(online && h.host.label == "devbox");
        }
        data.hosts.finish(Ok(hosts));
        let tr = Tr::new(locale);
        let ctx = egui::Context::default();
        let fonts = superba_gui::design::fonts::FontReport::default();
        ctx.set_fonts(superba_gui::design::fonts::definitions(&fonts));
        superba_gui::design::style::install(&ctx, Some(egui::Theme::Light));
        let mut w = Wizard::start(&data);
        let mut out = Vec::new();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let cx = Cx {
                        tr: &tr,
                        pal: Palette::of(ui.ctx()),
                        data: &data,
                        busy: false,
                        now: Timestamp::now(),
                        fonts: &fonts,
                        service: &dyn_service,
                        log: &[],
                    };
                    let _ = wizard::show(ui.ctx(), &cx, &mut w, &mut out);
                },
            );
            shapes = output.shapes.clone();
            output.drop_without_applying_deltas();
        }
        let row = shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == want => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{want} not drawn"));
        // The row's dot: left of the text, at its height.
        let pal = superba_gui::design::tokens::LIGHT;
        let dot: Vec<egui::Color32> = shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Circle(c)
                    if c.center.x < row.min.x && (c.center.y - row.center().y).abs() < 8.0 =>
                {
                    Some(c.fill)
                }
                _ => None,
            })
            .collect();
        let (expected, other) = if online {
            (Status::Ok, Status::Idle)
        } else {
            (Status::Idle, Status::Ok)
        };
        // The wizard layer may still be fading in: compare without alpha.
        let rgb = |c: egui::Color32| {
            let [r, g, b, _] = c.to_srgba_unmultiplied();
            [r, g, b]
        };
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3);
        let dots: Vec<[u8; 3]> = dot.iter().map(|c| rgb(*c)).collect();
        assert!(
            dots.iter().any(|d| near(*d, rgb(pal.status(expected)))),
            "{want}: {dot:?}"
        );
        assert!(
            !dots.iter().any(|d| near(*d, rgb(pal.status(other)))),
            "{want}: {dot:?}"
        );
    }
}
