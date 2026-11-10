#![allow(missing_docs)]
//! user bug 130 / GH#28: installation finishes after the window stops drawing.
mod a11y_harness;
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use a11y_harness::AppHarness;
use egui::{ViewportCommand, accesskit::Role};
use superba_core::{
    CloseAction, Locale, SelfInstallOutcome, SelfRelease, SelfUpdateReport, SelfUpdateStatus,
    SelfUpdateView, Settings, SwitchService,
};
use superba_gui::{
    AppOptions,
    pages::{Page, SuperbaSection},
    tray::{Sink, TrayOption},
};
use support::fake::{FakeOptions, FakeService};

fn finish_without_drawing(minimized: bool) {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run: false,
        ..Default::default()
    }));
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(service.save_settings(Settings {
        close_action: Some(CloseAction::Tray),
        ..Default::default()
    }))
    .unwrap();
    service.set_self_update(SelfUpdateView {
        current: "0.1.1-alpha.10".into(),
        status: SelfUpdateStatus::Available,
        latest: Some(SelfRelease {
            tag: "v0.1.1-alpha.11".into(),
            version: "0.1.1-alpha.11".into(),
            prerelease: true,
            notes: String::new(),
            published_at: None,
            url: "https://example.com/release".into(),
            asset: Some("superba.zip".into()),
            asset_size: None,
        }),
        ..Default::default()
    });
    service.set_self_update_report(SelfUpdateReport {
        version: "0.1.1-alpha.11".into(),
        outcome: SelfInstallOutcome::Installed,
        package: None,
        steps: vec![],
        router_restarted: false,
        relaunching: true,
        replaced: vec!["Superba.app".into()],
    });
    let gate = service.hold_self_update_install();
    let mut h = AppHarness::with_options(
        service.clone(),
        AppOptions {
            locale: Some(Locale::En),
            page: Some(Page::Superba(SuperbaSection::Updates)),
            command: Some("focus:superba".into()),
            tray: TrayOption::Simulated(Sink::default()),
            ..Default::default()
        },
    );
    h.click(Some(Role::Button), "Update now");
    assert_eq!(service.self_update_installs().len(), 1);
    assert!(!h.take_commands().contains(&ViewportCommand::Close));
    if minimized {
        h.minimize();
    } else {
        h.close_window();
        assert!(!h.visible());
        assert!(h.app().is_hidden_in_tray());
    }
    h.take_commands();
    gate.notify_one();
    let start = Instant::now();
    loop {
        h.logic_only();
        let commands = h.take_commands();
        assert!(!commands.contains(&ViewportCommand::Visible(true)));
        if commands.contains(&ViewportCommand::Close) {
            assert!(!commands.contains(&ViewportCommand::CancelClose));
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the finished update never requested exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // A Close sent by the app must go through even with close-to-tray enabled.
    h.close_window();
    assert!(!h.take_commands().contains(&ViewportCommand::CancelClose));
}

#[test]
fn self_update_quits_after_the_window_was_hidden_in_the_tray() {
    finish_without_drawing(false);
}

#[test]
fn self_update_quits_after_the_window_was_minimized() {
    finish_without_drawing(true);
}
