//! The desktop app's "Run Codex in WSL" mode (user bug 54 (b)): the app runs
//! this computer's Codex inside the default distribution, reading this
//! computer's `config.toml` and `auth.json`. A local switch says so; router mode
//! is refused when that distribution cannot reach Windows on `127.0.0.1` (WSL 2
//! behind NAT), allowed on WSL 1 and with mirrored networking.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use superba_core::{
    BlockReason, ProfileId, RestartKind, RouterInstallRequest, SwitchPlan, SwitchRequest,
    SwitchService, WslNetworking,
};
use superba_switch::Switcher;

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn notice_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings.iter().map(|n| n.text.key.clone()).collect()
}

/// The default distribution `Ubuntu`; the app's setting on (global state or
/// `[desktop]` in config.toml) or off.
fn world(version: u8, networking: WslNetworking, setting: Option<&str>) -> World {
    let w = World::new();
    let mut host = wsl_host("Ubuntu", version, networking);
    host.wsl.as_mut().unwrap().default = true;
    w.remote.hosts.lock().unwrap().push(host);
    let home = w.ctx.codex_home();
    std::fs::create_dir_all(home.root()).unwrap();
    match setting {
        Some("global") => std::fs::write(
            home.global_state_json(),
            r#"{"runCodexInWindowsSubsystemForLinux": true}"#,
        )
        .unwrap(),
        Some("config") => std::fs::write(
            home.config_toml(),
            "[desktop]\nrunCodexInWindowsSubsystemForLinux = true\n",
        )
        .unwrap(),
        _ => {}
    }
    w
}

async fn router_on(sw: &Switcher) {
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
}

fn blocked_by_wsl(plan: &SwitchPlan) -> bool {
    matches!(&plan.primary, RestartKind::Blocked { reasons }
        if reasons.iter().any(|r| matches!(r, BlockReason::RouterFromWslNat { distro } if distro == "Ubuntu")))
}

#[tokio::test]
async fn a_switch_says_the_app_runs_codex_in_wsl() {
    for setting in ["global", "config"] {
        let w = world(2, WslNetworking::Nat, Some(setting));
        let plan = w.switcher().plan(req("work")).await.unwrap();
        assert!(
            notice_keys(&plan).contains(&"switch.wsl.legacy_mode".to_owned()),
            "{setting}: {:?}",
            notice_keys(&plan)
        );
        // Direct: nothing in the way.
        assert!(!plan.primary.is_blocked(), "{:?}", plan.primary);
    }
    let w = world(2, WslNetworking::Nat, None);
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(!notice_keys(&plan).contains(&"switch.wsl.legacy_mode".to_owned()));
}

#[tokio::test]
async fn router_mode_is_refused_when_that_distribution_cannot_reach_windows() {
    let w = world(2, WslNetworking::Nat, Some("global"));
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(blocked_by_wsl(&plan), "{:?}", plan.primary);
    let text = BlockReason::RouterFromWslNat {
        distro: "Ubuntu".into(),
    }
    .text();
    assert_eq!(text.key, "block.router_wsl_nat");
    // WSL 1 and mirrored networking reach 127.0.0.1 on Windows.
    for (version, networking) in [(1, WslNetworking::Nat), (2, WslNetworking::Mirrored)] {
        let w = world(version, networking, Some("global"));
        let sw = w.switcher();
        router_on(&sw).await;
        let plan = sw.plan(req("work")).await.unwrap();
        assert!(
            !blocked_by_wsl(&plan),
            "{version} {networking:?}: {:?}",
            plan.primary
        );
    }
    // Without the app's setting the distribution is just a host.
    let w = world(2, WslNetworking::Nat, None);
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!blocked_by_wsl(&plan));
}

#[tokio::test]
async fn the_wsl_overview_names_the_distribution_the_app_runs_codex_in() {
    let w = world(2, WslNetworking::Nat, Some("config"));
    let o = w.switcher().wsl_overview().await.unwrap();
    assert_eq!(o.app_runs_codex_in.as_deref(), Some("Ubuntu"));
    let w = world(2, WslNetworking::Nat, None);
    let o = w.switcher().wsl_overview().await.unwrap();
    assert_eq!(o.app_runs_codex_in, None);
}
