//! The router's port across restarts (user bug 65): a router that is down
//! because another program holds its port moves to a free port and Codex
//! follows through a normal router-on switch; a router that answers on another
//! port than Codex uses gets Codex pointed at it; WSL distributions behind NAT
//! follow the moved listener; a move that does not work still falls back to
//! direct mode.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    Error, ExecuteOptions, NoEvents, ProfileId, ResolvedMode, RouterInstallRequest,
    RouterWatchState, SafeDefaults, SwitchRequest, SwitchService, WslNetworking,
};
use superba_switch::Switcher;

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

async fn router_on(sw: &Switcher) {
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
}

/// Codex goes through the router (port 18431 of the fake).
async fn route_codex(w: &World, sw: &Switcher) {
    let plan = sw.plan(req("work")).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert_eq!(codex_url(w), FakeRouter::base_url(18_431));
}

fn codex_url(w: &World) -> String {
    w.config
        .state
        .lock()
        .unwrap()
        .openai_base_url
        .clone()
        .unwrap_or_default()
}

/// The router goes down and another program takes its port.
fn take_port(w: &World, port: u16) {
    *w.router.up.lock().unwrap() = false;
    *w.router.taken.lock().unwrap() = Some(port);
}

#[tokio::test]
async fn a_router_down_on_a_taken_port_moves_and_codex_follows() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    take_port(&w, 18_431);

    // The service manager may still win: wait like for any outage.
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::Suspect { .. }),
        "{tick:?}"
    );
    assert!(!w.calls().iter().any(|c| c.starts_with("router.move")));

    // Down past the grace period with the port taken: move, then a router-on
    // switch points Codex at the new port.
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert_eq!(
        tick.state,
        RouterWatchState::Moved {
            from: 18_431,
            to: 19_440
        },
        "{tick:?}"
    );
    assert!(tick.outcome.is_some() && tick.error.is_none(), "{tick:?}");
    assert!(
        w.calls().contains(&"router.move 18431->19440".to_owned()),
        "{:?}",
        w.calls()
    );
    assert_eq!(codex_url(&w), FakeRouter::base_url(19_440));
    let view = sw.router_status().await.unwrap();
    assert!(view.settings.enabled, "router mode stays on");
    assert!(view.codex_routed);
    assert_eq!(view.port, Some(19_440));

    // Settled: the next ticks see a healthy router at Codex's port.
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Healthy
    );
}

#[tokio::test]
async fn a_router_that_cannot_move_still_falls_back_to_direct_mode() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    take_port(&w, 18_431);
    *w.router.fail_move.lock().unwrap() = Some(Error::other("no free port"));
    sw.router_watch().await.unwrap();
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::FellBack { .. }),
        "{tick:?}"
    );
    let view = sw.router_status().await.unwrap();
    assert!(!view.settings.enabled && !view.codex_routed);
    assert!(codex_url(&w).starts_with(OVERSEAS));
}

#[tokio::test]
async fn codex_follows_a_router_that_answers_on_another_port() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    // The router was moved (a reinstall with the port taken, or a repair
    // whose switch failed before) and answers on its new port.
    *w.router.port.lock().unwrap() = Some(19_000);
    let tick = sw.router_watch().await.unwrap();
    assert_eq!(
        tick.state,
        RouterWatchState::Moved {
            from: 18_431,
            to: 19_000
        },
        "{tick:?}"
    );
    assert_eq!(codex_url(&w), FakeRouter::base_url(19_000));
    assert!(
        !w.calls().iter().any(|c| c.starts_with("router.move")),
        "the router itself did not have to move"
    );
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Healthy
    );
}

#[tokio::test]
async fn a_failed_switch_to_the_new_port_is_retried_and_router_mode_stays_on() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    *w.router.port.lock().unwrap() = Some(19_000);
    *w.config.fail_apply.lock().unwrap() = Some(Error::other("disk full"));
    let tick = sw.router_watch().await.unwrap();
    assert!(tick.error.is_some(), "{tick:?}");
    assert!(
        matches!(&tick.state, RouterWatchState::Suspect { detail } if detail.contains("19000")),
        "Codex cannot reach the router meanwhile: {tick:?}"
    );
    assert_eq!(codex_url(&w), FakeRouter::base_url(18_431));
    assert!(w.profiles.set.lock().unwrap().settings.router.enabled);

    *w.config.fail_apply.lock().unwrap() = None;
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::Moved { to: 19_000, .. }),
        "{tick:?}"
    );
    assert_eq!(codex_url(&w), FakeRouter::base_url(19_000));
}

#[tokio::test]
async fn after_the_grace_period_the_switch_to_the_new_port_is_tried_before_a_fallback() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    *w.router.port.lock().unwrap() = Some(19_000);
    *w.config.fail_apply.lock().unwrap() = Some(Error::other("disk full"));
    sw.router_watch().await.unwrap();
    w.clock.advance(Duration::from_secs(25));
    *w.config.fail_apply.lock().unwrap() = None;
    // The repair works now: Codex follows, no fallback.
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::Moved { .. }),
        "{tick:?}"
    );
    assert!(sw.router_status().await.unwrap().settings.enabled);
}

#[tokio::test]
async fn reinstalling_a_router_whose_port_was_taken_points_codex_at_the_new_port() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    take_port(&w, 18_431);
    // Router page: Restart (install with the stored members).
    let view = sw
        .router_install(RouterInstallRequest { members: vec![] })
        .await
        .unwrap();
    assert_eq!(view.port, Some(19_440));
    assert!(view.running && view.codex_routed, "{view:?}");
    assert_eq!(codex_url(&w), FakeRouter::base_url(19_440));
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Healthy
    );
}

#[tokio::test]
async fn a_reinstall_keeps_the_port_and_codex_untouched_when_nothing_took_it() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&w, &sw).await;
    *w.router.up.lock().unwrap() = false;
    let applied = *w.config.applied.lock().unwrap();
    let view = sw
        .router_install(RouterInstallRequest { members: vec![] })
        .await
        .unwrap();
    assert_eq!(view.port, Some(18_431));
    assert_eq!(*w.config.applied.lock().unwrap(), applied, "no switch ran");
    assert_eq!(codex_url(&w), FakeRouter::base_url(18_431));
}

#[tokio::test]
async fn a_wsl2_distribution_behind_nat_follows_the_moved_listener() {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(wsl_host("Ubuntu", 2, WslNetworking::Nat));
    *w.remote.gateway.lock().unwrap() = Some("172.25.80.1".into());
    // The WSL listener answers on the gateway on whatever port the router
    // uses (the firewall rule names no port).
    for port in [18_431, 19_440] {
        w.remote
            .http
            .lock()
            .unwrap()
            .insert(format!("172.25.80.1:{port}"), router_health());
    }
    let sw = w.switcher();
    router_on(&sw).await;
    let target = superba_core::wsl::target_id("Ubuntu");
    sw.wsl_router_access(&target).await.unwrap();
    route_codex(&w, &sw).await;
    assert_eq!(wsl_base(&w), "http://172.25.80.1:18431/backend-api/codex");

    take_port(&w, 18_431);
    sw.router_watch().await.unwrap();
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::Moved { to: 19_440, .. }),
        "{tick:?}"
    );
    assert_eq!(codex_url(&w), FakeRouter::base_url(19_440));
    assert_eq!(wsl_base(&w), "http://172.25.80.1:19440/backend-api/codex");
    assert!(
        w.router.firewall.lock().unwrap().is_empty(),
        "no new administrator approval"
    );
}

/// The base URL of the last plan for the WSL distribution.
fn wsl_base(w: &World) -> String {
    let scoped = w.remote.scoped.lock().unwrap();
    let (_, _, desired) = scoped
        .iter()
        .rev()
        .find(|(label, _, _)| label.contains("Ubuntu"))
        .expect("a plan for the distribution");
    match &desired.mode {
        ResolvedMode::Token { endpoints, .. } => endpoints.openai_base_url.to_string(),
        other => panic!("unexpected {other:?}"),
    }
}
