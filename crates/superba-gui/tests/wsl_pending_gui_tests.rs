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
