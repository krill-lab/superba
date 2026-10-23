#![allow(missing_docs)]

//! GH#7 (user bug 66): opening ChatGPT through Superba is one click away.
//!
//! - The state machine: what the Overview offers (open / reopen / nothing) and
//!   what a request does for each origin (the window, the "ChatGPT (Superba)"
//!   shortcut, the start-up preference).
//! - The hero's action and the reopen's confirmation (no
//!   "write only", running turns named) in the whole app.
//! - The handover: `superba --launch-chatgpt` reaches a running window through
//!   `gui-instance.json`; stale or foreign requests are refused.
//! - `--launch-chatgpt` without a window to hand over to, and the start-up
//!   preference.

mod a11y_harness;
mod support;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use superba_core::{
    AppSession, FullRestartReason, HotSwitchStatus, Locale, Overview, Settings, SwitchService,
    Timestamp,
};
use superba_gui::AppOptions;
use superba_gui::design::tokens::DARK;
use superba_gui::i18n::Tr;
use superba_gui::instance::{self, Endpoint, Request};
use superba_gui::launch::{
    AppLaunch, LaunchOrigin, LaunchResult, LaunchStep, app_launch, launch, launch_step,
};
use superba_gui::pages::overview::hero;
use superba_gui::pages::{Cx, Data, Page};
use support::fake::{FakeOptions, FakeService};

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

fn with_cx<R>(
    data: &Data,
    service: Arc<dyn SwitchService>,
    locale: Locale,
    f: impl FnOnce(&Cx<'_>) -> R,
) -> R {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &DARK,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts,
        service: &service,
        log: &[],
    };
    f(&cx)
}

fn with_status(mut o: Overview, status: HotSwitchStatus, running: bool) -> Overview {
    o.app.hot_switch = status;
    if !running {
        o.app.session = None;
    } else if o.app.session.is_none() {
        o.app.session = Some(AppSession {
            install_id: None,
            main_pid: 4242,
            started_at: None,
            executable: None,
            cdp: None,
            launched_by_us: false,
            authapi_env: Default::default(),
            home_override: None,
            codex_home: None,
            app_server_exe: None,
        });
    }
    o
}

// ---------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_overview_offers_open_or_reopen_only_where_it_helps() {
    use LaunchOrigin::{Shortcut, Startup, Window};
    use LaunchStep::{Activate, Nothing, Open, Reopen};
    let base = fake().overview().await.unwrap();
    let unavailable = |reason| HotSwitchStatus::Unavailable { reason };
    // (status, running, offered, window/shortcut step, start-up step)
    let cases = [
        (
            HotSwitchStatus::NotRunning,
            false,
            Some(AppLaunch::Open),
            Open,
            Open,
        ),
        (HotSwitchStatus::Ready, true, None, Activate, Nothing),
        (HotSwitchStatus::SafeMode, true, None, Activate, Nothing),
        // No desktop app: opening says why; never at start-up.
        (HotSwitchStatus::NotInstalled, false, None, Open, Nothing),
        (
            unavailable(FullRestartReason::NoCdp),
            true,
            Some(AppLaunch::Reopen(FullRestartReason::NoCdp)),
            Reopen,
            Nothing,
        ),
        (
            unavailable(FullRestartReason::AuthApiMissing),
            true,
            Some(AppLaunch::Reopen(FullRestartReason::AuthApiMissing)),
            Reopen,
            Nothing,
        ),
        (
            unavailable(FullRestartReason::AuthApiUnhealthy),
            true,
            Some(AppLaunch::Reopen(FullRestartReason::AuthApiUnhealthy)),
            Reopen,
            Nothing,
        ),
        // A reopen changes nothing here: bring it forward.
        (
            unavailable(FullRestartReason::NoBridge),
            true,
            None,
            Activate,
            Nothing,
        ),
        (
            unavailable(FullRestartReason::PlatformNoHot),
            true,
            None,
            Activate,
            Nothing,
        ),
    ];
    for (status, running, offered, step, startup) in cases {
        let o = with_status(base.clone(), status.clone(), running);
        assert_eq!(app_launch(&o), offered, "{status:?}");
        assert_eq!(launch_step(&o, Window), step, "{status:?} from the window");
        assert_eq!(
            launch_step(&o, Shortcut),
            step,
            "{status:?} from the shortcut"
        );
        assert_eq!(launch_step(&o, Startup), startup, "{status:?} at start-up");
    }
    // Only requests from outside the window bring it to the front.
    assert!(Shortcut.from_outside());
    assert!(!Window.from_outside());
    assert!(!Startup.from_outside());
    assert_eq!(AppLaunch::Open.label_key(), "action.open_app");
    assert_eq!(
        AppLaunch::Reopen(FullRestartReason::NoCdp).label_key(),
        "action.reopen_app"
    );
}

#[tokio::test]
async fn a_request_opens_brings_forward_or_asks_for_the_reopen() {
    // Not running: opened through Superba, hot switching afterwards.
    let service = fake();
    service.quit_app();
    assert_eq!(
        launch(service.as_ref(), LaunchOrigin::Shortcut)
            .await
            .unwrap(),
        LaunchResult::Opened
    );
    assert_eq!(service.open_app_calls(), 1);
    let o = service.overview().await.unwrap();
    assert_eq!(o.app.hot_switch, HotSwitchStatus::Ready);
    assert_eq!(app_launch(&o), None);

    // Open through Superba: brought forward (the service activates it).
    assert_eq!(
        launch(service.as_ref(), LaunchOrigin::Shortcut)
            .await
            .unwrap(),
        LaunchResult::Activated
    );
    assert_eq!(service.open_app_calls(), 2);

    // Opened elsewhere: the caller plans the reopen; nothing is started here.
    service.open_app_elsewhere();
    for origin in [LaunchOrigin::Window, LaunchOrigin::Shortcut] {
        assert_eq!(
            launch(service.as_ref(), origin).await.unwrap(),
            LaunchResult::NeedsReopen
        );
    }
    // The start-up preference never restarts a running ChatGPT.
    assert_eq!(
        launch(service.as_ref(), LaunchOrigin::Startup)
            .await
            .unwrap(),
        LaunchResult::Nothing
    );
    assert_eq!(service.open_app_calls(), 2);
}

// ---------------------------------------------------------------------------
// The hero, the palette
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_hero_and_the_palette_carry_the_action() {
    let service = fake();
    for (setup, expected) in [
        (None, None),
        (Some("quit"), Some(AppLaunch::Open)),
        (
            Some("elsewhere"),
            Some(AppLaunch::Reopen(FullRestartReason::NoCdp)),
        ),
    ] {
        match setup {
            Some("quit") => service.quit_app(),
            Some("elsewhere") => {
                service.open_app().await.unwrap();
                service.open_app_elsewhere();
            }
            _ => {}
        }
        let data = data_of(&service).await;
        let overview = data.overview.value.clone().unwrap();
        for locale in [Locale::En, Locale::ZhCn] {
            with_cx(&data, service.clone(), locale, |cx| {
                let h = hero::model(cx, &overview, &[], None);
                assert_eq!(h.launch, expected, "{setup:?}");
            });
        }
    }
    let tr = Tr::new(Locale::En);
    assert_eq!(tr.t("action.open_app"), "Open ChatGPT with Superba");
    assert_eq!(
        tr.t("action.reopen_app"),
        "Reopen with Superba (enables hot switching)"
    );
    assert_eq!(
        Tr::new(Locale::ZhCn).t("action.reopen_app"),
        "用 Superba 重新打开（启用热切换）"
    );
}

#[test]
fn the_hero_button_opens_chatgpt_and_goes_away() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = fake();
        service.quit_app();
        let mut h = AppHarness::new(service.clone(), locale, Page::Overview);
        let open = tr.t("action.open_app");
        let t = h.wait_for(Some(Role::Button), &open);
        let id = t.find(Some(Role::Button), &open).unwrap();
        assert!(!t.node(id).is_disabled());
        h.click(Some(Role::Button), &open);
        h.until("ChatGPT opened and the button gone", |t| {
            service.open_app_calls() == 1 && t.find(Some(Role::Button), &open).is_none()
        });
        // Hot switching is available now: nothing extra on the hero.
        let t = h.settle();
        assert!(t.find(None, &tr.t("action.reopen_app")).is_none());
    }
}

#[test]
fn the_hero_reopen_is_confirmed_first_without_write_only() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = fake();
        service.open_app_elsewhere();
        let mut h = AppHarness::new(service.clone(), locale, Page::Overview);
        let reopen = tr.t("action.reopen_app");
        h.click(Some(Role::Button), &reopen);
        let title = tr.t("confirm.title_reopen");
        let t = h.wait_for(None, &title);
        assert!(
            t.with_role(Role::Dialog)
                .iter()
                .any(|d| t.name(*d).as_deref() == Some(title.as_str())),
            "{}",
            t.dump()
        );
        // One button of that name: the To-do's "Reopen" behind the dialog is
        // another one (UI Automation finds controls by name).
        assert_ne!(tr.t("confirm.reopen"), tr.t("overview.todo.fix.reopen"));
        assert_eq!(
            t.find_all(Some(Role::Button), &tr.t("confirm.reopen"))
                .len(),
            1,
            "{}",
            t.dump()
        );
        // Reopening writes nothing: no "write config only".
        assert!(
            t.find(Some(Role::Button), &tr.t("confirm.write_only"))
                .is_none()
        );
        // Nothing happened before the confirmation.
        assert_eq!(service.open_app_calls(), 0);
        h.key(egui::Key::Escape);
    }
}

// ---------------------------------------------------------------------------
// The handover between two starts
// ---------------------------------------------------------------------------

#[test]
fn a_second_start_hands_its_request_to_the_running_window() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    let listener = instance::listen(dir.path(), move |r| tx.send(r).unwrap()).unwrap();
    let endpoint = Endpoint::read(dir.path()).expect("registered");
    assert_eq!(&endpoint, listener.endpoint());
    assert_eq!(endpoint.pid, std::process::id());
    assert_eq!(endpoint.token.len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(instance::FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "only this user reads the token");
    }

    assert_eq!(
        instance::forward(dir.path(), Request::LaunchChatGpt),
        Some(std::process::id())
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Request::LaunchChatGpt
    );

    // A wrong token, an unknown command or another protocol is refused.
    let other = tempfile::tempdir().unwrap();
    std::fs::write(
        other.path().join(instance::FILE),
        format!(
            r#"{{"pid": 1, "port": {}, "token": "00000000000000000000000000000000"}}"#,
            endpoint.port
        ),
    )
    .unwrap();
    assert_eq!(
        instance::forward(other.path(), Request::LaunchChatGpt),
        None
    );
    for line in [
        format!("superba-gui/1 {} launch-chatgpt\n", "x".repeat(32)),
        format!("superba-gui/1 {} open-settings\n", endpoint.token),
        format!("superba-gui/2 {} launch-chatgpt\n", endpoint.token),
        format!("superba-gui/1 {} launch-chatgpt extra\n", endpoint.token),
        String::new(),
    ] {
        assert_eq!(
            instance::parse_line(&line, &endpoint.token),
            None,
            "{line:?}"
        );
    }
    assert_eq!(
        instance::parse_line(
            &format!("superba-gui/1 {} launch-chatgpt\r\n", endpoint.token),
            &endpoint.token
        ),
        Some(Request::LaunchChatGpt)
    );
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

    // Gone: the file goes with it, and a later start finds nobody.
    drop(listener);
    assert!(!dir.path().join(instance::FILE).exists());
    assert_eq!(instance::forward(dir.path(), Request::LaunchChatGpt), None);
}

#[test]
fn a_stale_registration_is_not_a_window() {
    let dir = tempfile::tempdir().unwrap();
    // Nothing registered.
    assert_eq!(instance::forward(dir.path(), Request::LaunchChatGpt), None);
    // A window that is gone left its file: the port answers no more.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::fs::write(
        dir.path().join(instance::FILE),
        format!(
            r#"{{"pid": 999999, "port": {port}, "token": "0123456789abcdef0123456789abcdef"}}"#
        ),
    )
    .unwrap();
    assert_eq!(instance::forward(dir.path(), Request::LaunchChatGpt), None);
    // Garbage is no registration.
    std::fs::write(dir.path().join(instance::FILE), "not json").unwrap();
    assert!(Endpoint::read(dir.path()).is_none());
}

#[test]
fn an_older_window_leaves_a_newer_registration_alone() {
    let dir = tempfile::tempdir().unwrap();
    let first = instance::listen(dir.path(), |_| {}).unwrap();
    let second = instance::listen(dir.path(), |_| {}).unwrap();
    assert_eq!(Endpoint::read(dir.path()).as_ref(), Some(second.endpoint()));
    drop(first);
    assert_eq!(Endpoint::read(dir.path()).as_ref(), Some(second.endpoint()));
    drop(second);
    assert!(Endpoint::read(dir.path()).is_none());
}

#[test]
fn the_running_window_opens_chatgpt_for_the_shortcut() {
    let tr = Tr::new(Locale::En);
    let service = fake();
    service.quit_app();
    let mut h = AppHarness::with_options(
        service.clone(),
        AppOptions {
            theme: Some(egui::Theme::Light),
            locale: Some(Locale::En),
            page: Some(Page::Overview),
            single_instance: true,
            ..AppOptions::default()
        },
    );
    h.wait_for(Some(Role::Button), &tr.t("action.open_app"));
    // What `superba --launch-chatgpt` does first.
    let dir = a11y_harness::data_dir();
    assert!(instance::forward(&dir, Request::LaunchChatGpt).is_some());
    h.until("ChatGPT opened for the shortcut", |t| {
        service.open_app_calls() == 1
            && t.find(Some(Role::Button), &tr.t("action.open_app"))
                .is_none()
    });

    // Opened elsewhere since: the shortcut brings up the reopen's confirmation.
    service.open_app_elsewhere();
    assert!(instance::forward(&dir, Request::LaunchChatGpt).is_some());
    h.wait_for(None, &tr.t("confirm.title_reopen"));
    assert_eq!(service.open_app_calls(), 1);
    h.key(egui::Key::Escape);
}

#[test]
fn launch_chatgpt_without_a_window_and_the_start_up_preference() {
    let tr = Tr::new(Locale::En);
    // `superba --launch-chatgpt` with ChatGPT opened elsewhere and no window to
    // hand over to: the window opens with the reopen to confirm.
    let service = fake();
    service.open_app_elsewhere();
    let mut h = AppHarness::with_options(
        service.clone(),
        AppOptions {
            theme: Some(egui::Theme::Light),
            locale: Some(Locale::En),
            launch_chatgpt: true,
            ..AppOptions::default()
        },
    );
    h.wait_for(None, &tr.t("confirm.title_reopen"));
    h.key(egui::Key::Escape);
    drop(h);

    // The preference opens a ChatGPT that is not running ...
    let service = fake();
    service.quit_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(service.save_settings(Settings {
        open_app_on_start: true,
        ..Settings::default()
    }))
    .unwrap();
    let mut h = AppHarness::new(service.clone(), Locale::En, Page::Overview);
    h.until("opened at start-up", |_| service.open_app_calls() == 1);
    drop(h);

    // ... and leaves a running one alone, also one opened elsewhere.
    let service = fake();
    service.open_app_elsewhere();
    rt.block_on(service.save_settings(Settings {
        open_app_on_start: true,
        ..Settings::default()
    }))
    .unwrap();
    let mut h = AppHarness::new(service.clone(), Locale::En, Page::Overview);
    h.wait_for(Some(Role::Button), &tr.t("action.reopen_app"));
    let t = h.settle();
    assert!(t.find(None, &tr.t("confirm.title_reopen")).is_none());
    assert_eq!(service.open_app_calls(), 0);

    // Off by default.
    assert!(!Settings::default().open_app_on_start);
}
