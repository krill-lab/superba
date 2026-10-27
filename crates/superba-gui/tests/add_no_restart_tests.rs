#![allow(missing_docs)]

//! Adding a provider only saves it (user report on 0.3.0-alpha.1: "I added a
//! provider and it prompted me about a hot restart"). The Add dialog's primary
//! button is "Add"; it plans nothing, so Codex is never restarted, and a toast
//! offers "Switch to this provider" as a separate click. Switching right away is
//! an opt-in checkbox (off) that plans exactly one switch. Ids of new providers
//! never collide with a preset id (the service would merge such a save into an
//! existing provider, possibly the applied one). A countdown that re-applies
//! the provider in use says so and names what changed.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Rect, pos2, vec2};
use superba_core::{
    ActiveState, AuthMode, KeyChange, Locale, LocalizedText, PlanDestination, ProfileId,
    RestartKind, SwitchIntent, SwitchPlan, SwitchRequest, SwitchService, Timestamp, catalog,
};
use superba_gui::app::{local_request, planned_request};
use superba_gui::design::{fonts, style, tokens::Palette, tokens::size};
use superba_gui::flow::{Flow, Stage};
use superba_gui::i18n::Tr;
use superba_gui::pages::providers::{self, AddDialog, AddStep, form};
use superba_gui::pages::shared::providers::ProviderId;
use superba_gui::pages::{Action, Cx, Data};
use superba_gui::runtime::Command;
use superba_gui::shell::{self, BannerState};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

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

/// The Add dialog at step 2 of the Krill preset with a name and a key typed.
fn preset_dialog() -> providers::State {
    let mut d = AddDialog::new();
    d.step = AddStep::Preset;
    d.name = "Krill 2".into();
    d.key = "nb_second_key".into();
    let mut state = providers::State::default();
    state.add = Some(d);
    state
}

/// Draw the Providers page (with its dialog) in a 1280 x 800 window.
fn page<'a>(
    data: &'a Data,
    service: &'a Arc<dyn SwitchService>,
    tr: &'a Tr,
    report: &'a fonts::FontReport,
    state: &'a mut providers::State,
    actions: &'a mut Vec<Action>,
) -> impl FnMut(&mut egui::Ui) + 'a {
    move |ui| {
        let cx = Cx {
            tr,
            pal: Palette::of(ui.ctx()),
            data,
            busy: false,
            now: Timestamp::now(),
            fonts: report,
            service,
            log: &[],
        };
        providers::show(ui, &cx, state, actions);
    }
}

/// Frames until the dialog's save finished (the dialog closes) or 3 s passed.
fn until_saved(
    h: &Harness,
    data: &Data,
    service: &Arc<dyn SwitchService>,
    tr: &Tr,
    state: &mut providers::State,
    actions: &mut Vec<Action>,
) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) && state.add.is_some() {
        let mut f = page(data, service, tr, &h.fonts, state, actions);
        h.frame(Vec::new(), &mut f);
        drop(f);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What the app would plan for `actions` (the same mapping it uses), fed into a
/// fresh flow: the backend commands it sends.
fn planned_commands(actions: &[Action]) -> Vec<Command> {
    let mut flow = Flow::new(5);
    for request in actions.iter().filter_map(planned_request) {
        flow.request(request, Duration::ZERO);
    }
    flow.take_commands()
}

fn texts(list: &[(String, Rect)]) -> Vec<&str> {
    list.iter().map(|(t, _)| t.as_str()).collect()
}

#[tokio::test]
async fn the_primary_add_button_only_saves_and_offers_the_switch() {
    for locale in [Locale::ZhCn, Locale::En] {
        let service = fake();
        let data = data_of(&service).await;
        let before = service.overview().await.unwrap().active;
        let svc: Arc<dyn SwitchService> = service.clone();
        let tr = Tr::new(locale);
        let h = Harness::new([1280.0, 800.0], egui::Theme::Light);
        let mut state = preset_dialog();
        let mut actions = Vec::new();
        let add = tr.t("providers.add.add");
        let add_switch = tr.t("providers.add.add_switch");
        let opt_in = tr.t("providers.add.switch_after");
        {
            let mut f = page(&data, &svc, &tr, &h.fonts, &mut state, &mut actions);
            let shown = h.settle(&mut f);
            let shown = texts(&shown);
            assert!(shown.contains(&add.as_str()), "{shown:?}");
            assert!(shown.contains(&opt_in.as_str()), "{shown:?}");
            assert!(
                shown.contains(&tr.t("providers.add.save_only_hint").as_str()),
                "says that adding only saves: {shown:?}"
            );
            assert!(
                !shown.contains(&add_switch.as_str()),
                "no Add and switch button by default: {shown:?}"
            );
            // The dialog's own "Add" (the page header says "Add provider").
            assert!(h.click_last_text(&add, &mut f));
        }
        until_saved(&h, &data, &svc, &tr, &mut state, &mut actions);
        assert!(state.add.is_none(), "the dialog closes after saving");
        assert_eq!(
            state.selected,
            Some(ProviderId::Profile(ProfileId::new("krill-2")))
        );
        let set = service.profile_set().await.unwrap();
        let added = set.profile(&ProfileId::new("krill-2")).expect("saved");
        assert!(added.key_ref.is_some());
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Action::OfferSwitch { profile, .. } if profile.as_str() == "krill-2"
            )),
            "{actions:?}"
        );
        assert!(
            actions.iter().all(|a| planned_request(a).is_none()),
            "a plain add plans nothing: {actions:?}"
        );
        assert!(planned_commands(&actions).is_empty());
        // Codex is untouched: the applied provider and its generation stay.
        assert_eq!(service.overview().await.unwrap().active, before);
    }
}

#[tokio::test]
async fn opting_in_to_switch_plans_exactly_one_switch_to_the_new_provider() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let tr = Tr::new(Locale::ZhCn);
    let h = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let mut state = preset_dialog();
    let mut actions = Vec::new();
    let add_switch = tr.t("providers.add.add_switch");
    let opt_in = tr.t("providers.add.switch_after");
    {
        let mut f = page(&data, &svc, &tr, &h.fonts, &mut state, &mut actions);
        assert!(h.click_text(&opt_in, &mut f), "the opt-in checkbox");
        let shown = h.settle(&mut f);
        let shown = texts(&shown);
        assert!(shown.contains(&add_switch.as_str()), "{shown:?}");
        assert!(
            shown.contains(&tr.t("providers.add.switch_after_hint").as_str()),
            "says what switching does: {shown:?}"
        );
        assert!(h.click_last_text(&add_switch, &mut f));
    }
    until_saved(&h, &data, &svc, &tr, &mut state, &mut actions);
    assert!(state.add.is_none());
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::OfferSwitch { .. })),
        "{actions:?}"
    );
    let planned: Vec<SwitchRequest> = actions.iter().filter_map(planned_request).collect();
    assert_eq!(planned.len(), 1, "{actions:?}");
    assert!(matches!(
        &planned[0].intent,
        SwitchIntent::Profile { profile_id, .. } if profile_id.as_str() == "krill-2"
    ));
    let commands = planned_commands(&actions);
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::Plan { .. }));
}

#[tokio::test]
async fn a_custom_site_saves_untested_but_switching_to_it_needs_a_passed_test() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let tr = Tr::new(Locale::En);
    let h = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    let mut d = AddDialog::new();
    d.step = AddStep::Custom;
    d.name = "My API".into();
    d.url = "https://api.example.com/v1".into();
    d.key = "sk-test-a91f".into();
    d.switch_after = true;
    let mut state = providers::State::default();
    state.add = Some(d);
    let mut actions = Vec::new();
    {
        let mut f = page(&data, &svc, &tr, &h.fonts, &mut state, &mut actions);
        // Opted in but untested: the primary button is disabled.
        assert!(h.click_last_text("Add and switch", &mut f));
    }
    assert!(state.add.as_ref().is_some_and(|d| !d.saving()));
    assert!(
        actions
            .iter()
            .all(|a| planned_request(a).is_none() && !matches!(a, Action::OfferSwitch { .. })),
        "{actions:?}"
    );
    state.add.as_mut().unwrap().switch_after = false;
    {
        let mut f = page(&data, &svc, &tr, &h.fonts, &mut state, &mut actions);
        assert!(h.click_last_text("Add", &mut f));
    }
    until_saved(&h, &data, &svc, &tr, &mut state, &mut actions);
    assert!(state.add.is_none(), "saved without a test");
    let set = service.profile_set().await.unwrap();
    assert!(set.custom_sites.iter().any(|s| s.id.as_str() == "my-api"));
    assert!(actions.iter().all(|a| planned_request(a).is_none()));
    assert!(
        actions.iter().any(
            |a| matches!(a, Action::OfferSwitch { profile, .. } if profile.as_str() == "my-api")
        )
    );
}

#[test]
fn the_offered_switch_is_one_plan_and_saving_actions_plan_nothing() {
    let offer = Action::Switch {
        profile: ProfileId::new("krill-2"),
        line: None,
        auth: None,
    };
    let request = planned_request(&offer).expect("a switch");
    assert_eq!(
        request,
        local_request(SwitchIntent::Profile {
            profile_id: ProfileId::new("krill-2"),
            line_id: None,
            auth_mode: None,
        })
    );
    assert!(request.targets.local && request.targets.remotes.is_empty());
    let site = catalog::krill_site();
    let profile = superba_gui::pages::shared::new_profile(&site, "Krill 3".into());
    for saving in [
        Action::SaveProfile {
            profile: Box::new(profile),
            key: None,
        },
        Action::SaveSettings,
        Action::Refresh,
        Action::OfferSwitch {
            text: "Added Krill 3".into(),
            profile: ProfileId::new("krill-3"),
        },
        Action::Toast {
            status: superba_gui::design::widgets::Status::Ok,
            text: "Added".into(),
        },
    ] {
        assert!(planned_request(&saving).is_none(), "{saving:?}");
    }
}

#[test]
fn new_provider_ids_never_take_a_preset_id() {
    for preset in catalog::presets() {
        assert!(
            form::RESERVED_IDS.contains(&preset.id.as_str()),
            "preset {} must be reserved",
            preset.id
        );
        // A provider named like a preset ("Custom") gets its own id.
        let name = preset.id.to_uppercase();
        assert_eq!(form::slug_id(&name, &[]), format!("{}-2", preset.id));
    }
    // The Chinese duplicate of "Custom" is "Custom 副本": still not `custom`.
    assert_eq!(form::slug_id("Custom 副本", &[]), "custom-2");
}

// ---------------------------------------------------------------------------
// The countdown of a re-apply names what changed
// ---------------------------------------------------------------------------

fn plan(from: Option<&str>, to: &str, keys: &[&str]) -> SwitchPlan {
    SwitchPlan {
        id: format!("reapply-{to}-{}", keys.len()),
        created_at: Timestamp::EPOCH,
        request: SwitchRequest::profile(to.into()),
        from: from.map(|p| ActiveState {
            profile_id: Some(p.into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::ApiKey),
            generation: 3,
            applied_at: Timestamp::EPOCH,
        }),
        to: PlanDestination {
            profile_id: Some(to.into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::ApiKey),
            label: LocalizedText::new("test.destination", "Krill"),
        },
        primary: RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
        restarts: vec![],
        config: Some(superba_core::ApplyPlan {
            write_mode: superba_core::ConfigWriteMode::Managed,
            id: "apply".into(),
            codex_home: std::path::PathBuf::from("/tmp/none"),
            generation: 4,
            profile_id: Some(to.into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::ApiKey),
            files: vec![],
            key_changes: keys
                .iter()
                .map(|k| KeyChange {
                    path: (*k).into(),
                    before: Some("a".into()),
                    after: Some("b".into()),
                })
                .collect(),
            auth_change: None,
            notes: vec![],
            desired: None,
            releases_ownership: false,
        }),
        env: None,
        certificate_env: None,
        migration: None,
        remotes: vec![],
        active_threads: vec![],
        model_check: None,
        choices: vec![],
        alternatives: vec![],
        warnings: vec![],
        cleanup: vec![],
    }
}

fn banner_text(locale: Locale, plan: SwitchPlan) -> Vec<String> {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(egui::Theme::Light));
    let mut flow = Flow::new(5);
    flow.request(plan.request.clone(), Duration::ZERO);
    let commands = flow.take_commands();
    let [Command::Plan { seq, .. }] = commands.as_slice() else {
        panic!("plan command");
    };
    flow.on_planned(*seq, Ok(plan), Duration::ZERO);
    assert!(matches!(flow.stage(), Stage::Armed { .. }));
    let mut state = BannerState::default();
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            time: Some(2.0),
            ..Default::default()
        },
        |ui| {
            let cx = Cx {
                tr: &Tr::new(locale),
                pal: Palette::of(&ctx),
                data: &Data::default(),
                busy: false,
                now: Timestamp::EPOCH,
                fonts: &fonts::FontReport::default(),
                service: &support::idle_service(),
                log: &[],
            };
            ui.set_width(1280.0 - size::SIDEBAR - 64.0);
            shell::banner(
                ui,
                &cx,
                &mut flow,
                &mut state,
                Duration::from_secs(2),
                f32::INFINITY,
            );
        },
    );
    let text = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(s) => Some(s.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    text
}

#[test]
fn a_countdown_that_reapplies_the_provider_in_use_says_why() {
    let text = banner_text(
        Locale::En,
        plan(
            Some("krill"),
            "krill",
            &["model_reasoning_effort", "service_tier"],
        ),
    );
    assert!(
        text.iter()
            .any(|t| t.starts_with("Applying the changes to “Krill · Overseas · API key” in")),
        "{text:?}"
    );
    assert!(
        text.iter().any(|t| t
            == "Why: “Krill · Overseas · API key” is in use and its settings changed: \
                model_reasoning_effort, service_tier"),
        "{text:?}"
    );
    assert!(text.iter().any(|t| t == "Apply now"), "{text:?}");
    let text = banner_text(
        Locale::ZhCn,
        plan(
            Some("krill"),
            "krill",
            &["a", "b", "c", "d", "e", "openai_base_url"],
        ),
    );
    assert!(
        text.iter().any(|t| t.starts_with("原因：「Krill")
            && t.ends_with("」正在使用，而它的这些设置改了：a, b, c, d 等另外 2 项")),
        "{text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("秒后把改动应用到「Krill")),
        "{text:?}"
    );
}

#[test]
fn a_switch_to_another_provider_keeps_the_switching_title() {
    for from in [Some("krill"), None] {
        let text = banner_text(Locale::En, plan(from, "krill-2", &["openai_base_url"]));
        assert!(
            text.iter().any(|t| t.starts_with("Switching to “Krill")),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t == "Switch now"), "{text:?}");
        assert!(!text.iter().any(|t| t.starts_with("Why:")), "{text:?}");
    }
}
