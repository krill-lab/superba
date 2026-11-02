#![allow(missing_docs)]
#![cfg(target_os = "linux")]
//! The actual egui shell on an isolated HOME and fake backend. No live router,
//! provider, credential, desktop window or system service is touched.
mod a11y_harness;
mod support;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use std::sync::Arc;
use superba_core::{CloseAction, Locale, Settings, SwitchService};
use superba_gui::{
    AppOptions,
    i18n::Tr,
    pages::{Page, SuperbaSection},
    tray::{Sink, TrayAction, TrayEvent, TrayOption},
};
use support::fake::{FakeOptions, FakeService};

const PASSWORD: &str = "isolated-password-测试";

fn app(service: Arc<FakeService>, sink: &Sink) -> AppHarness {
    AppHarness::with_options(
        service,
        AppOptions {
            locale: Some(Locale::En),
            page: Some(Page::Superba(SuperbaSection::General)),
            tray: TrayOption::Simulated(sink.clone()),
            ..Default::default()
        },
    )
}

fn configure(h: &mut AppHarness, tr: &Tr) {
    h.set_value(&tr.t("superba.search"), &tr.t("app_lock.title"));
    h.click(Some(Role::Button), &tr.t("app_lock.configure"));
}

fn unlock(h: &mut AppHarness, tr: &Tr) {
    h.set_value(&tr.t("app_lock.password"), PASSWORD);
    h.click(Some(Role::Button), &tr.t("app_lock.unlock"));
    h.until("management window unlocked", |t| {
        t.find(Some(Role::Button), &tr.t("app_lock.unlock"))
            .is_none()
    });
}

#[test]
fn settings_unlock_tray_restart_boot_and_disable_work_end_to_end() {
    // Set the throwaway HOME before any runtime or background thread starts.
    let _ = a11y_harness::data_dir();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    rt.block_on(service.save_settings(Settings {
        close_action: Some(CloseAction::Tray),
        ..Settings::default()
    }))
    .unwrap();
    let tr = Tr::new(Locale::En);
    let sink = Sink::default();
    let mut h = app(service.clone(), &sink);
    configure(&mut h, &tr);
    h.click(Some(Role::CheckBox), &tr.t("app_lock.enabled"));
    h.set_value(&tr.t("app_lock.new"), PASSWORD);
    h.set_value(&tr.t("app_lock.confirm"), "mismatched-password");
    h.click(Some(Role::Button), &tr.t("app_lock.save"));
    h.wait_for(None, &tr.t("app_lock.mismatch"));
    h.set_value(&tr.t("app_lock.confirm"), PASSWORD);
    h.click(Some(Role::Button), &tr.t("app_lock.save"));
    h.wait_for(None, &tr.t("app_lock.saved"));
    h.click(Some(Role::Button), &tr.t("app_lock.close"));

    // Closing to tray locks every-open mode. Queued management events must not
    // execute even if they originated from the old, unlocked tray menu.
    h.close_window();
    assert!(h.app().is_hidden_in_tray());
    let open_calls = service.open_app_calls();
    sink.push(TrayEvent::Action(TrayAction::LaunchApp));
    let tree = h.wait_for(Some(Role::Button), &tr.t("app_lock.unlock"));
    assert_eq!(service.open_app_calls(), open_calls);
    assert!(
        tree.find(Some(Role::Button), &tr.t("app_lock.configure"))
            .is_none()
    );
    let menu = h.app().tray_menu().unwrap();
    assert_eq!(menu.actions().len(), 2);
    assert!(menu.find(&TrayAction::RouterOff).is_none());
    assert!(menu.find(&TrayAction::RestoreOfficial).is_none());
    h.shortcut(egui::Key::K);
    h.wait_for(Some(Role::Button), &tr.t("app_lock.unlock"));
    h.set_value(&tr.t("app_lock.password"), "wrong-password");
    h.click(Some(Role::Button), &tr.t("app_lock.unlock"));
    h.wait_for(None, &tr.t("app_lock.wrong"));
    h.until("password retry delay finishes", |t| {
        t.find(None, &tr.t("app_lock.wait")).is_none()
    });
    unlock(&mut h, &tr);

    // Switch mode without changing the password, then restart the whole GUI.
    configure(&mut h, &tr);
    h.click(Some(Role::RadioButton), &tr.t("app_lock.per_boot"));
    h.set_value(&tr.t("app_lock.current"), PASSWORD);
    h.click(Some(Role::Button), &tr.t("app_lock.save"));
    h.wait_for(None, &tr.t("app_lock.saved"));
    h.click(Some(Role::Button), &tr.t("app_lock.close"));
    h.close_window();
    sink.push(TrayEvent::Activate);
    h.wait_for(Some(Role::Button), &tr.t("app_lock.configure"));
    drop(h);
    let mut h = app(service.clone(), &sink);
    configure(&mut h, &tr); // No password required in this same boot.
    h.click(Some(Role::Button), &tr.t("app_lock.lock_now"));
    h.wait_for(Some(Role::Button), &tr.t("app_lock.unlock"));
    let tree = h.shortcut(egui::Key::Z);
    let password_input = tree
        .find(Some(Role::PasswordInput), &tr.t("app_lock.password"))
        .unwrap();
    assert!(
        tree.node(password_input).value().unwrap_or("").is_empty(),
        "undo restored a password after locking"
    );
    drop(h);

    // Manual lock also invalidates the remembered boot session across restarts.
    let mut settings = rt.block_on(service.profile_set()).unwrap().settings;
    settings.router.enabled = true;
    rt.block_on(service.save_settings(settings)).unwrap();
    let mut h = app(service.clone(), &sink);
    h.wait_for(Some(Role::Button), &tr.t("app_lock.unlock"));
    let initial_watch_calls = service.router_watch_calls();
    h.until("router watchdog runs while UI locked", |_| {
        service.router_watch_calls() > initial_watch_calls
    });
    unlock(&mut h, &tr);
    drop(h);

    // Simulate a previous boot's marker; no real reboot or live data is used.
    let dir = superba_switch::detect_context()
        .unwrap()
        .data_dir
        .anchor()
        .join("gui-lock");
    let session_path = dir.join("session.json");
    let mut session: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&session_path).unwrap()).unwrap();
    session["boot_id"] = "previous-boot".into();
    std::fs::write(&session_path, serde_json::to_vec(&session).unwrap()).unwrap();
    let mut h = app(service.clone(), &sink);
    h.wait_for(Some(Role::Button), &tr.t("app_lock.unlock"));
    unlock(&mut h, &tr);
    configure(&mut h, &tr);
    h.click(Some(Role::CheckBox), &tr.t("app_lock.enabled"));
    h.set_value(&tr.t("app_lock.current"), PASSWORD);
    h.click(Some(Role::Button), &tr.t("app_lock.save"));
    h.wait_for(None, &tr.t("app_lock.saved"));
    drop(h);
    let mut h = app(service, &sink);
    configure(&mut h, &tr); // Disabling protection persists too.
    let contents = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(!contents.contains(PASSWORD));
    assert!(!session_path.exists());
}
