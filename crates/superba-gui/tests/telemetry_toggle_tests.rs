#![allow(missing_docs)]

//! "Disable OpenAI telemetry" on the Connection tab's settings row (user bug
//! 137): off by default for a new Krill provider (its preset no longer writes
//! `analytics.enabled = false`); the toggle (or its label) saves the profile
//! and, for the provider Codex uses now, plans the switch that writes it; a
//! profile saved with telemetry off shows the toggle on with a warning of what
//! it costs, and turning it off stops managing the key; in the app the
//! switch's AccessKit state follows the saved profile.

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::{Role, Toggled};

use superba_core::{
    CodexSettings, Locale, Profile, ProfileId, SettingEntry, SettingValue, SwitchService, Timestamp,
};
use superba_gui::design::Palette;
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Cx, Data, Page, providers, shared};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

const TOGGLE_EN: &str = "Disable OpenAI telemetry";
const TOGGLE_ZH: &str = "关闭 OpenAI 遥测";

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

async fn krill(service: &FakeService) -> Profile {
    service
        .profile_set()
        .await
        .unwrap()
        .profiles
        .into_iter()
        .find(|p| p.id.as_str() == "krill")
        .unwrap()
}

/// The providers page (the applied Krill provider's Connection tab) in `h`.
fn page<'a>(
    data: &'a Data,
    service: &'a Arc<dyn SwitchService>,
    tr: &'a Tr,
    h: &'a Harness,
    state: &'a mut providers::State,
    actions: &'a mut Vec<Action>,
) -> impl FnMut(&mut egui::Ui) + 'a {
    move |ui: &mut egui::Ui| {
        let cx = Cx {
            tr,
            pal: Palette::of(ui.ctx()),
            data,
            busy: false,
            now: Timestamp::now(),
            fonts: &h.fonts,
            service,
            log: &[],
        };
        providers::show(ui, &cx, state, actions);
    }
}

fn texts(h: &Harness, f: &mut dyn FnMut(&mut egui::Ui)) -> String {
    h.settle(f)
        .into_iter()
        .map(|(t, _)| t)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_krill_preset_keeps_openai_telemetry_on() {
    let preset = CodexSettings::krill_recommended();
    assert!(!preset.openai_telemetry_off());
    assert!(!preset.contains("analytics.enabled"));
    let site = superba_core::catalog::krill_site();
    let profile = shared::new_profile(&site, "Krill".into());
    assert!(!profile.codex_settings.openai_telemetry_off());

    // The toggle's model: on writes `false`, off stops managing the key; a
    // value the user chose on the Settings page (`true`) is not "off".
    let mut s = CodexSettings::krill_recommended();
    s.set_openai_telemetry_off(true);
    assert_eq!(
        s.get("analytics.enabled"),
        Some(&SettingEntry::Set(SettingValue::Bool(false)))
    );
    assert!(s.openai_telemetry_off());
    s.set_openai_telemetry_off(false);
    assert!(!s.contains("analytics.enabled"));
    assert_eq!(s, CodexSettings::krill_recommended());
    s.set("analytics.enabled", true).unwrap();
    assert!(!s.openai_telemetry_off());
    s.set_openai_telemetry_off(false);
    assert!(
        s.contains("analytics.enabled"),
        "the user's own value stays"
    );
}

#[tokio::test]
async fn telemetry_actions_save_and_switch_only_the_applied_provider() {
    let service = fake();
    let data = data_of(&service).await;
    let applied = krill(&service).await;
    assert!(shared::telemetry_actions(&data, &applied, false).is_empty());
    let actions = shared::telemetry_actions(&data, &applied, true);
    match actions.as_slice() {
        [
            Action::SaveProfile { profile, key: None },
            Action::Switch {
                profile: id,
                line: None,
                auth: None,
            },
        ] => {
            assert!(profile.codex_settings.openai_telemetry_off());
            assert_eq!(id, &applied.id);
        }
        other => panic!("{other:?}"),
    }
    let mut other = applied.clone();
    other.id = ProfileId::new("krill-2");
    match shared::telemetry_actions(&data, &other, true).as_slice() {
        [Action::SaveProfile { profile, key: None }] => {
            assert!(profile.codex_settings.openai_telemetry_off());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_settings_row_offers_the_toggle_and_it_saves_the_profile() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let h = Harness::new([1280.0, 900.0], egui::Theme::Light);
    let tr = Tr::new(Locale::En);
    let mut state = providers::State::default();
    let mut actions = Vec::new();
    let mut f = page(&data, &svc, &tr, &h, &mut state, &mut actions);
    let text = texts(&h, &mut f);
    assert!(text.contains(TOGGLE_EN), "{text}");
    // On by default: the cost is in the tooltip, no warning under the row.
    assert!(
        !text.contains("does not cache its remote settings"),
        "{text}"
    );
    assert!(h.click_text(TOGGLE_EN, &mut f));
    drop(f);
    let saved = actions
        .iter()
        .find_map(|a| match a {
            Action::SaveProfile { profile, .. } => Some(profile.clone()),
            _ => None,
        })
        .expect("the toggle saves the profile");
    assert!(saved.codex_settings.openai_telemetry_off());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Switch { profile, .. } if profile.as_str() == "krill")),
        "the applied provider is switched to again: {actions:?}"
    );
}

#[tokio::test]
async fn a_profile_saved_with_telemetry_off_shows_it_and_turns_it_back_on_in_chinese() {
    let service = fake();
    let mut profile = krill(&service).await;
    let before = profile.codex_settings.clone();
    // What an earlier Superba saved (its Krill preset switched telemetry off).
    profile.codex_settings.set_openai_telemetry_off(true);
    service.save_profile(profile, None).await.unwrap();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let h = Harness::new([1280.0, 900.0], egui::Theme::Light);
    let tr = Tr::new(Locale::ZhCn);
    let mut state = providers::State::default();
    let mut actions = Vec::new();
    let mut f = page(&data, &svc, &tr, &h, &mut state, &mut actions);
    let text = texts(&h, &mut f);
    assert!(text.contains(TOGGLE_ZH), "{text}");
    assert!(text.contains("关闭遥测后 App 不缓存远程开关"), "{text}");
    assert!(h.click_text(TOGGLE_ZH, &mut f));
    drop(f);
    let saved = actions
        .iter()
        .find_map(|a| match a {
            Action::SaveProfile { profile, .. } => Some(profile.clone()),
            _ => None,
        })
        .expect("the toggle saves the profile");
    assert!(!saved.codex_settings.contains("analytics.enabled"));
    assert_eq!(saved.codex_settings, before);
}

#[test]
fn in_the_app_the_switch_starts_off_and_follows_the_saved_profile() {
    let service = fake();
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(service.clone(), Locale::ZhCn, Page::Providers);
    let name = tr.t("providers.telemetry.toggle");
    assert_eq!(name, TOGGLE_ZH);
    let t = h.wait_for(Some(Role::Switch), &name);
    let id = t.find(Some(Role::Switch), &name).unwrap();
    assert_eq!(t.node(id).toggled(), Some(Toggled::False), "{}", t.dump());
    h.click(Some(Role::Switch), &name);
    let t = h.until("the toggle is on after the save", |t| {
        t.find(Some(Role::Switch), &name)
            .is_some_and(|id| t.node(id).toggled() == Some(Toggled::True))
    });
    assert!(t.has(&tr.t("providers.telemetry.cost")), "{}", t.dump());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let saved = rt.block_on(service.profile_set()).unwrap();
    let krill = saved.profile(&ProfileId::new("krill")).unwrap();
    assert!(krill.codex_settings.openai_telemetry_off());
}
