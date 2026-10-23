#![allow(missing_docs)]
//! A WSL distribution a switch left out because it was stopped (user bug 54):
//! the list says "stopped, update pending", the detail says why and offers
//! "Update now", which plans the same intent for that distribution alone.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{Locale, SwitchIntent, SwitchService, TargetId};
use superba_gui::pages::Action;
use superba_gui::pages::hosts::{self, State};
use support::fake::{FakeOptions, FakeService};
use support::fit::SIZES;

fn fake() -> Arc<FakeService> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.with_stopped_wsl_pending();
    service
}

fn wsl() -> TargetId {
    superba_core::wsl::target_id("Ubuntu")
}

fn harness(size: [f32; 2], locale: Locale, service: &Arc<FakeService>) -> Harness {
    let mut h = Harness::new(size, locale, service.clone(), true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let hosts = rt.block_on(service.hosts());
    h.data.hosts.finish(hosts);
    h
}

#[test]
fn a_stopped_distribution_with_a_pending_update_says_so_and_offers_update_now() {
    for size in SIZES {
        for (locale, list, badge, why, button) in [
            (
                Locale::ZhCn,
                "待更新",
                "已停止",
                "的切换没有更新它：当时该发行版未运行",
                "立即更新",
            ),
            (
                Locale::En,
                "update pending",
                "Stopped",
                "the distribution was stopped (Superba never starts it by itself)",
                "Update now",
            ),
        ] {
            let service = fake();
            let mut h = harness(size, locale, &service);
            let mut state = State::default();
            let f = h.until(
                |ui, cx, out| hosts::show(ui, cx, &mut state, out),
                |f| f.has("WSL: Ubuntu"),
            );
            assert!(
                f.texts.iter().any(|(t, _)| t.contains(list)),
                "{list} missing:\n{}",
                f.all_text()
            );
            state.selected = Some(wsl());
            let f = h.frames(4, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
            for want in [badge, button] {
                assert!(f.has(want), "{want} missing:\n{}", f.all_text());
            }
            assert!(
                f.texts.iter().any(|(t, _)| t.contains(why)),
                "{why} missing:\n{}",
                f.all_text()
            );
            assert!(f.fits(), "{size:?} {locale:?}");
            // Showing it never probes (that would start the distribution).
            assert!(
                !service.host_probes().contains(&wsl()),
                "{:?}",
                service.host_probes()
            );
            h.click_text(&f, button);
            h.until(
                |ui, cx, out| hosts::show(ui, cx, &mut state, out),
                |f| {
                    f.actions.iter().any(|a| {
                        matches!(a, Action::Plan(r)
                            if !r.targets.local
                                && r.targets.remotes == vec![wsl()]
                                && matches!(&r.intent, SwitchIntent::Profile { profile_id, .. }
                                    if profile_id.as_str() == "work"))
                    })
                },
            );
        }
    }
}

#[test]
fn a_failed_update_shows_its_stage_and_offers_retry_in_both_languages() {
    for (locale, retry, stopped) in [
        (Locale::En, "Update now", "the distribution was stopped"),
        (Locale::ZhCn, "立即更新", "当时该发行版未运行"),
    ] {
        let service = fake();
        let mut h = harness([1280.0, 800.0], locale, &service);
        let mut state = State::default();
        state.selected = Some(wsl());
        h.until(
            |ui, cx, out| hosts::show(ui, cx, &mut state, out),
            |f| f.has(retry),
        );
        let host = state
            .hosts
            .value
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|h| h.host.target_id == wsl())
            .unwrap();
        host.pending.as_mut().unwrap().failure = Some(superba_core::Error::Timeout {
            what: "WSL Ubuntu: files (attempt 2/2)".into(),
        });
        let f = h.frames(4, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
        assert!(f.all_text().contains("WSL Ubuntu: files (attempt 2/2)"));
        assert!(!f.all_text().contains(stopped));
        assert!(f.has(retry));
        assert!(f.fits());
    }
}

/// What `spb` changed while the GUI was open reaches the page (user bug 68):
/// "Check again" reads the list again through `hosts_now` (which asks Windows
/// which distributions run) and shows the stopped distribution with its pending
/// update and "Update now", without checking (starting) it.
#[test]
fn check_again_shows_what_another_process_left_pending() {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.with_running_wsl();
    let mut h = harness([1280.0, 800.0], Locale::En, &service);
    let mut state = State::default();
    state.selected = Some(wsl());
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("WSL: Ubuntu") && f.has("Running"),
    );
    assert!(!f.has("Update now"), "{}", f.all_text());
    assert_eq!(service.hosts_now_calls(), 1, "the page's first list");
    // A running distribution is checked on its own (that starts nothing).
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |_| service.host_probes().contains(&wsl()),
    );

    // `spb switch` while the distribution was stopped (another process).
    service.with_stopped_wsl_pending();
    let wsl_probes = || {
        service
            .host_probes()
            .iter()
            .filter(|t| **t == wsl())
            .count()
    };
    let probes = wsl_probes();
    let f = h.frames(4, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
    assert!(
        !f.has("Update now"),
        "no reload by itself:\n{}",
        f.all_text()
    );
    h.click_text(&f, "Check again");
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Update now"),
    );
    assert!(service.hosts_now_calls() >= 2);
    assert!(f.has("Stopped"), "{}", f.all_text());
    assert!(
        f.texts.iter().any(|(t, _)| t.contains("update pending")),
        "{}",
        f.all_text()
    );
    assert!(
        f.texts
            .iter()
            .any(|(t, _)| t.contains("Not checked yet (stopped; Check now starts it)"))
            || f.texts.iter().any(|(t, _)| t.contains("Checked")),
        "{}",
        f.all_text()
    );
    // The stopped distribution was not checked again (that would start it).
    let f = h.frames(6, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
    assert_eq!(wsl_probes(), probes, "{}", f.all_text());
}

/// A switch (this GUI's, or `spb`'s seen as a newer applied configuration in
/// the overview) and coming back to the page read the list again (user bug 68).
#[test]
fn a_switch_and_a_visit_read_the_list_again() {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.with_running_wsl();
    let mut h = harness([1280.0, 800.0], Locale::En, &service);
    let mut state = State::default();
    state.selected = Some(wsl());
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Running"),
    );
    let calls = service.hosts_now_calls();

    // This GUI's switch finished.
    service.with_stopped_wsl_pending();
    state.after_switch();
    // Wait for the read itself, not only the text: a probe of the running
    // distribution started by the frames before can land first and already
    // show the pending update (the fake answers probes from its current state).
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Update now") && service.hosts_now_calls() == calls + 1,
    );
    assert_eq!(service.hosts_now_calls(), calls + 1);

    // `spb switch`: the overview reports a newer applied configuration.
    service.with_running_wsl();
    let active = h
        .data
        .overview
        .value
        .as_mut()
        .and_then(|o| o.active.as_mut())
        .expect("an applied configuration in the fake overview");
    active.applied_at = active
        .applied_at
        .saturating_add(std::time::Duration::from_secs(60));
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Running") && !f.has("Update now") && service.hosts_now_calls() == calls + 2,
    );
    assert_eq!(service.hosts_now_calls(), calls + 2);

    // Coming back to the page.
    service.with_stopped_wsl_pending();
    state.entered();
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Update now") && service.hosts_now_calls() == calls + 3,
    );
    assert_eq!(service.hosts_now_calls(), calls + 3);
}
