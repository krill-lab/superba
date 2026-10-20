#![allow(missing_docs)]
//! Exercise the app's real completion/reply path, without clicking Rescan.

mod a11y_harness;
mod support;

use std::sync::Arc;
use std::time::Duration;

use a11y_harness::{AppHarness, Tree};
use egui::accesskit::Role;
use superba_core::Locale;
use superba_gui::pages::Page;
use support::fake::{FakeOptions, FakeService};

fn app() -> AppHarness {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.set_session_counts(&[("krill", 600)]);
    let mut h = AppHarness::new(service, Locale::En, Page::Sessions);
    h.click(Some(Role::Button), "Session migration…");
    let t = h.wait_for(Some(Role::Button), "Migrate to openai…");
    assert_counts(&t, 0, 600);
    assert!(t.has("No migrations yet"), "{}", t.dump());
    h
}

fn count(t: &Tree, label: &str) -> Option<u32> {
    let labels: Vec<_> = t
        .walk()
        .into_iter()
        .filter(|(id, _)| t.node(*id).role() == Role::Label)
        .filter_map(|(id, _)| t.name(id))
        .collect();
    labels
        .windows(2)
        .find_map(|pair| (pair[0] == label).then(|| pair[1].parse().ok()).flatten())
}

fn assert_counts(t: &Tree, visible: u32, hidden: u32) {
    assert_eq!(count(t, "Visible"), Some(visible), "{}", t.dump());
    assert_eq!(count(t, "Hidden"), Some(hidden), "{}", t.dump());
}

fn migrate(h: &mut AppHarness) {
    h.click(Some(Role::Button), "Migrate to openai…");
    h.click(Some(Role::Button), "Full restart");
}

#[test]
fn migration_and_rollback_refresh_counts_and_history_without_rescan() {
    let mut h = app();
    migrate(&mut h);
    let t = h.until("migrated counts and history", |t| {
        count(t, "Visible") == Some(600) && t.has("Done") && t.has("krill → openai")
    });
    assert_counts(&t, 600, 0);
    assert!(!t.has("No migrations yet"));

    h.click(Some(Role::Button), "Roll back…");
    h.click(Some(Role::Button), "Full restart");
    let t = h.until("restored counts and history", |t| {
        count(t, "Hidden") == Some(600) && t.has("Rolled back")
    });
    assert_counts(&t, 0, 600);
    assert!(t.has("Migrate to openai…"), "{}", t.dump());
}

#[test]
fn a_local_refresh_finishing_on_another_host_keeps_each_hosts_counts() {
    let mut h = app();
    let (started, release) = h.service.gate_next_session_scan();
    migrate(&mut h);
    started
        .recv_timeout(Duration::from_secs(5))
        .expect("migration completion must request a new session snapshot");

    h.click(Some(Role::RadioButton), "devbox");
    let t = h.until("devbox scan", |t| count(t, "Visible") == Some(236));
    assert_counts(&t, 236, 12);
    release.send(()).unwrap();

    // The local result arrives while devbox is selected. Keep rendering until
    // it is observable locally, then return to the same remote-host snapshot.
    h.click(Some(Role::RadioButton), "This computer");
    let t = h.until("local migration history", |t| {
        count(t, "Visible") == Some(600) && t.has("Done")
    });
    assert_counts(&t, 600, 0);
    h.click(Some(Role::RadioButton), "devbox");
    let t = h.until("devbox still selected", |t| {
        t.has("Migration history · devbox") || count(t, "Visible") == Some(236)
    });
    assert_counts(&t, 236, 12);
    assert!(!t.has("krill → openai"), "{}", t.dump());
}
