#![allow(missing_docs)]

mod support;

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use superba_core::*;
use superba_gui::design::widgets::{Status, tile_columns};
use superba_gui::pages::{
    Data, auth_mode_for_line, key_access_status,
    wizard::{Step, Wizard},
};
use superba_gui::runtime::{config_matches_site, read_original_key};
use support::fake::{FakeOptions, FakeService};

fn service() -> FakeService {
    FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    })
    .with_account_summary(true)
}

#[tokio::test]
async fn context_preference_survives_a_pat_blocked_line() {
    let service = service();
    let caps = service.probe_lines(&"krill".into()).await.unwrap();
    let overseas = caps.iter().find(|c| c.line_id == "overseas").unwrap();
    let china = caps.iter().find(|c| c.line_id == "cn-fast").unwrap();
    assert_eq!(
        auth_mode_for_line(AuthMode::Pat, Some(overseas)),
        AuthMode::Pat
    );
    assert_eq!(
        auth_mode_for_line(AuthMode::Pat, Some(china)),
        AuthMode::ApiKey
    );
    assert_eq!(
        auth_mode_for_line(AuthMode::Pat, Some(overseas)),
        AuthMode::Pat
    );
    assert_eq!(
        auth_mode_for_line(AuthMode::ApiKey, Some(overseas)),
        AuthMode::ApiKey
    );
    assert_eq!(auth_mode_for_line(AuthMode::Pat, None), AuthMode::Pat);
}

#[test]
fn edge_restrictions_are_warnings_and_key_failures_are_errors() {
    assert_eq!(key_access_status(&KeyAccess::Ok), Status::Ok);
    assert_eq!(key_access_status(&KeyAccess::EdgeBlocked), Status::Warn);
    assert_eq!(key_access_status(&KeyAccess::Unreachable), Status::Warn);
    for access in [
        KeyAccess::InvalidKey,
        KeyAccess::QuotaExhausted,
        KeyAccess::ChannelDenied,
    ] {
        assert_eq!(key_access_status(&access), Status::Error);
    }
}

#[tokio::test]
async fn account_summary_capability_follows_the_simulated_relay() {
    for enabled in [false, true] {
        let service = service().with_account_summary(enabled);
        let mut data = Data {
            sites: service.sites().await.unwrap(),
            ..Default::default()
        };
        data.overview.finish(service.overview().await);
        data.profiles.finish(service.profile_set().await);
        assert_eq!(
            data.active_site().unwrap().capabilities.account_summary,
            enabled
        );
        assert_eq!(
            data.overview.value.as_ref().unwrap().account.is_some(),
            enabled
        );
        assert_eq!(data.active_key_last4(), Some("9f3a"));
    }
}

#[tokio::test]
async fn a_key_suffix_is_shown_only_for_the_applied_site() {
    let service = service();
    let mut data = Data::default();
    data.overview.finish(service.overview().await);
    let overview = data.overview.value.as_mut().unwrap();
    let config = overview.config.as_mut().unwrap();
    config.model_provider = Some("openai".into());
    config.openai_base_url = Some("https://api.example.com/v1".into());
    // The configuration points at another site: never offered as a Krill key.
    assert!(!config_matches_site(config, &catalog::krill_site()));
    // Nothing applied by Superba: the suffix belongs to nobody it knows.
    overview.active = None;
    assert_eq!(data.active_key_last4(), None);
}

#[test]
fn tiles_wrap_before_their_text_loses_its_padding() {
    assert_eq!(tile_columns(900.0, 3), 3);
    assert_eq!(tile_columns(900.0, 5), 2);
    assert_eq!(tile_columns(530.0, 3), 2);
    assert_eq!(tile_columns(400.0, 3), 1);
    assert_eq!(tile_columns(900.0, 0), 1);
}

#[test]
fn legacy_migration_starts_with_the_recommended_choice() {
    assert_eq!(
        Wizard::start(&Data::default()).legacy,
        Some(LegacySessionChoice::Migrate)
    );
}

#[tokio::test]
async fn wizard_keyboard_focus_stays_visible_and_can_change_the_legacy_choice() {
    use superba_gui::design::{Palette, fonts, style};
    use superba_gui::i18n::Tr;
    use superba_gui::pages::{Cx, wizard};

    let service = FakeService::new(FakeOptions {
        first_run: true,
        fast: true,
        ..Default::default()
    });
    let mut data = Data::default();
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data.sessions.finish(service.sessions().await);
    let ctx = egui::Context::default();
    let fonts = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&fonts));
    style::install(&ctx, Some(egui::Theme::Dark));
    let tr = Tr::new(Locale::En);
    let cx = Cx {
        tr: &tr,
        pal: Palette::of(&ctx),
        data: &data,
        busy: false,
        now: Timestamp::EPOCH,
        fonts: &fonts,
        service: &support::idle_service(),
        log: &[],
    };
    let mut wizard = Wizard::start(&data);
    wizard.step = Step::Confirm;
    let frame = |wizard: &mut Wizard, key: Option<egui::Key>| {
        let mut outside = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(880.0, 600.0),
                )),
                events: key
                    .map(|key| egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    })
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
            |ui| {
                outside = Some(ui.button("Background control").id);
                let _ = wizard::show(ui.ctx(), &cx, wizard, &mut Vec::new());
            },
        );
        let reached_skip = output.platform_output.events.iter().any(|event| matches!(event, egui::output::OutputEvent::FocusGained(info) if info.role == egui::Role::RadioButton && info.label.as_deref() == Some("Not now")));
        if let Some(focused) = ctx.memory(|memory| memory.focused()) {
            assert_ne!(
                Some(focused),
                outside,
                "hidden page controls must not receive Tab"
            );
        }
        output.drop_without_applying_deltas();
        reached_skip
    };
    frame(&mut wizard, None);
    frame(&mut wizard, None);
    let reached_skip = (0..8).any(|_| frame(&mut wizard, Some(egui::Key::Tab)));
    assert!(reached_skip, "Tab must reach the visible legacy choices");
    frame(&mut wizard, Some(egui::Key::Enter));
    assert_eq!(wizard.legacy, Some(LegacySessionChoice::Skip));
}

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "superba-gui-legacy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn context(&self) -> LocalContext {
        let mut ctx = LocalContext::for_tests(&self.0);
        ctx.env = Arc::new(MapEnv::new().with("SUPERBA_SECRET_BACKEND", "file"));
        ctx
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn legacy_fixture(config: &str, pat: bool) -> (Home, ConfigState) {
    let home = Home::new();
    let ctx = home.context();
    let root = ctx.codex_home();
    std::fs::create_dir_all(root.root()).unwrap();
    std::fs::write(root.root().join("config.toml"), config).unwrap();
    let auth = if pat {
        r#"{"personal_access_token":"nb_fixture_legacy_5678","OPENAI_API_KEY":"nb_fixture_legacy_5678"}"#
    } else {
        r#"{"auth_mode":"apikey","OPENAI_API_KEY":"nb_fixture_legacy_1234"}"#
    };
    std::fs::write(root.root().join("auth.json"), auth).unwrap();
    let config = superba_switch::SwitcherDeps::with_defaults(ctx)
        .config
        .read_state(&root)
        .await
        .unwrap();
    (home, config)
}

#[tokio::test]
async fn legacy_krill_key_is_read_as_a_secret_for_both_auth_forms() {
    for pat in [false, true] {
        let (home, config) = legacy_fixture("model_provider = \"krill\"\n", pat).await;
        let key = read_original_key(home.context(), &config, &catalog::krill_site())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.last4(), if pat { "5678" } else { "1234" });
        assert!(!format!("{key:?}").contains("nb_fixture"));
    }
}

#[tokio::test]
async fn legacy_key_rejects_other_sites_and_changed_source_files() {
    let (home, config) = legacy_fixture(
        "model_provider = \"openai\"\nopenai_base_url = \"https://api.example.com/v1\"\n",
        false,
    )
    .await;
    let krill = catalog::krill_site();
    assert!(!config_matches_site(&config, &krill));
    assert!(
        read_original_key(home.context(), &config, &krill)
            .await
            .unwrap()
            .is_none()
    );

    let (home, config) = legacy_fixture("model_provider = \"openai\"\nopenai_base_url = \"https://api.krill-code.com/backend-api/codex\"\n", false).await;
    assert!(config_matches_site(&config, &krill));
    assert!(
        read_original_key(home.context(), &config, &krill)
            .await
            .unwrap()
            .is_some()
    );
    std::fs::write(
        home.context().codex_home().root().join("auth.json"),
        r#"{"OPENAI_API_KEY":"nb_fixture_replaced_9999"}"#,
    )
    .unwrap();
    assert!(
        read_original_key(home.context(), &config, &krill)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_wizard_does_not_continue_with_a_key_the_line_rejected() {
    use superba_core::{KeyAccess, KeyCheck};
    use superba_gui::design::{Palette, fonts};
    use superba_gui::i18n::Tr;
    use superba_gui::pages::{Cx, wizard};

    let service = FakeService::new(FakeOptions {
        first_run: true,
        fast: true,
        ..Default::default()
    });
    let mut data = Data::default();
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    let site = catalog::krill_site();
    let line = site.lines[0].id.clone();
    let check = |access: KeyAccess| KeyCheck {
        site_id: site.id.clone(),
        line_id: line.clone(),
        checked_at: Timestamp::EPOCH,
        key_last4: "abcd".into(),
        whoami: None,
        identity: None,
        access,
        codex_path: None,
    };
    let mut w = Wizard::start(&data);
    w.step = Step::LineModels;
    w.line = Some(line.clone());
    let ctx = egui::Context::default();
    let fonts = fonts::FontReport::default();
    let tr = Tr::new(Locale::En);
    let service = support::idle_service();
    for (access, ok) in [
        (KeyAccess::Ok, true),
        (KeyAccess::InvalidKey, false),
        (KeyAccess::QuotaExhausted, false),
        (KeyAccess::Unreachable, true),
    ] {
        data.key_checks
            .insert((line.clone(), AuthMode::Pat), Ok(check(access.clone())));
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(&ctx),
            data: &data,
            busy: false,
            now: Timestamp::EPOCH,
            fonts: &fonts,
            service: &service,
            log: &[],
        };
        assert_eq!(wizard::can_continue(&cx, &w), ok, "{access:?}");
    }
}
