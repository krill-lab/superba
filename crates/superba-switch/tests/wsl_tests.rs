//! WSL distributions in the switch service (user bug 45): they follow this
//! computer by default (connection and settings, filtered for their own Codex),
//! router mode reaches them where the network allows (each with its own source
//! token), router access for WSL 2 behind NAT, restores and skips.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ExecuteOptions, HostKind, NoEvents, ProfileId, RemoteApplyScope, ResolvedMode, RestoreTarget,
    RouterInstallRequest, SafeDefaults, SwitchIntent, SwitchPlan, SwitchRequest, SwitchService,
    WslNetworking,
};
use superba_router::token::verify_source_token;
use superba_switch::Switcher;

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn notice_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings.iter().map(|n| n.text.key.clone()).collect()
}

async fn router_on(sw: &Switcher) {
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
}

fn ubuntu(version: u8, networking: WslNetworking) -> World {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(wsl_host("Ubuntu", version, networking));
    w
}

fn wsl_target() -> superba_core::TargetId {
    superba_core::wsl::target_id("Ubuntu")
}

/// The desired resolution of the plan for the distribution.
fn wsl_desired(plan: &SwitchPlan) -> &superba_core::ResolvedProfile {
    plan.remotes
        .iter()
        .find(|p| p.target_id == wsl_target())
        .expect("a plan for the distribution")
        .desired
        .as_ref()
        .unwrap()
}

fn base_and_key(r: &superba_core::ResolvedProfile) -> (String, String) {
    match &r.mode {
        ResolvedMode::Token { endpoints, key, .. } => (
            endpoints.openai_base_url.to_string(),
            key.expose().to_owned(),
        ),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn a_profile_switch_also_switches_a_wsl_distribution_with_its_settings() {
    let w = ubuntu(1, WslNetworking::Nat);
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    {
        let mut set = w.profiles.set.lock().unwrap();
        let work = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        work.codex_settings.set("approval_policy", "never").unwrap();
        // The SSH host follows too (explicitly): connection only.
        set.settings.hosts.insert(
            superba_core::TargetId::remote("dev"),
            superba_core::HostSettings {
                follow_switches: true,
                ..Default::default()
            },
        );
    }
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.primary.is_blocked(), "{:?}", plan.primary);
    let scoped = w.remote.scoped.lock().unwrap().clone();
    let by_host: Vec<(String, RemoteApplyScope, bool)> = scoped
        .iter()
        .map(|(h, scope, d)| (h.clone(), *scope, d.settings.contains("approval_policy")))
        .collect();
    assert_eq!(
        by_host,
        vec![
            ("dev".to_owned(), RemoteApplyScope::Connection, false),
            ("WSL: Ubuntu".to_owned(), RemoteApplyScope::Both, true),
        ]
    );
    // Direct (no router): the line's own base URL and key.
    let (base, key) = base_and_key(wsl_desired(&plan));
    assert!(base.starts_with(OVERSEAS), "{base}");
    assert_eq!(key, KEY);
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert!(w.calls().iter().any(|c| c == "remote.apply WSL: Ubuntu"));
}

#[tokio::test]
async fn turning_follow_off_is_remembered_and_a_distribution_without_codex_is_left_alone() {
    let w = ubuntu(2, WslNetworking::Nat);
    let sw = w.switcher();
    let view = sw.set_host_follow(&wsl_target(), false).await.unwrap();
    assert_eq!(view.host.kind, HostKind::Wsl);
    let stored = w
        .profiles
        .set
        .lock()
        .unwrap()
        .settings
        .hosts
        .get(&wsl_target())
        .cloned()
        .unwrap();
    assert!(stored.follow_chosen && !stored.follow_switches);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(plan.remotes.is_empty());

    let w = ubuntu(2, WslNetworking::Nat);
    *w.remote.no_codex.lock().unwrap() = true;
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(plan.remotes.is_empty());
    assert!(notice_keys(&plan).contains(&"switch.wsl.no_codex".to_owned()));
}

#[tokio::test]
async fn router_mode_routes_wsl1_over_loopback_with_its_own_token() {
    let w = ubuntu(1, WslNetworking::Nat);
    w.remote
        .http
        .lock()
        .unwrap()
        .insert("127.0.0.1:18431".into(), router_health());
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    let (base, key) = base_and_key(wsl_desired(&plan));
    assert_eq!(base, FakeRouter::base_url(18_431));
    assert_ne!(key, ROUTER_TOKEN, "a distribution has its own token");
    assert_eq!(
        verify_source_token(ROUTER_TOKEN.as_bytes(), key.as_bytes()).as_deref(),
        Some("wsl:Ubuntu")
    );
    assert!(notice_keys(&plan).contains(&"switch.wsl.routed".to_owned()));
    assert_eq!(
        *w.remote.http_calls.lock().unwrap(),
        vec!["WSL: Ubuntu 127.0.0.1:18431 /_superba/v1/health".to_owned()]
    );
    // Mirrored networking: loopback as well.
    let w = ubuntu(2, WslNetworking::Mirrored);
    w.remote
        .http
        .lock()
        .unwrap()
        .insert("127.0.0.1:18431".into(), router_health());
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(
        base_and_key(wsl_desired(&plan)).0,
        FakeRouter::base_url(18_431)
    );
    // Another router answering on the port is not ours: direct.
    let w = ubuntu(1, WslNetworking::Nat);
    w.remote.http.lock().unwrap().insert(
        "127.0.0.1:18431".into(),
        router_health().replace("\"i\"", "\"other\""),
    );
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(base_and_key(wsl_desired(&plan)).0.starts_with(OVERSEAS));
    assert!(notice_keys(&plan).contains(&"switch.wsl.route_loopback_failed".to_owned()));
}

#[tokio::test]
async fn wsl2_behind_nat_stays_direct_until_router_access_is_on() {
    let w = ubuntu(2, WslNetworking::Nat);
    *w.remote.gateway.lock().unwrap() = Some("172.25.80.1".into());
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    let b = base_and_key(wsl_desired(&plan)).0;
    assert!(b.starts_with(OVERSEAS), "{b} {:?}", notice_keys(&plan));
    assert!(notice_keys(&plan).contains(&"switch.wsl.route_needs_access".to_owned()));

    // Access: the router listens on the WSL adapter address; Windows Firewall
    // lets it through already, so no rule is added.
    w.remote
        .http
        .lock()
        .unwrap()
        .insert("172.25.80.1:18431".into(), router_health());
    let report = sw.wsl_router_access(&wsl_target()).await.unwrap();
    assert!(report.reachable && !report.firewall_changed, "{report:?}");
    assert_eq!(report.address.as_deref(), Some("http://172.25.80.1:18431"));
    assert_eq!(
        *w.router.wsl_listen.lock().unwrap(),
        vec!["172.25.80.1".parse::<std::net::Ipv4Addr>().unwrap()]
    );
    assert!(w.router.firewall.lock().unwrap().is_empty());
    let plan = sw.plan(req("work")).await.unwrap();
    let (base, key) = base_and_key(wsl_desired(&plan));
    assert_eq!(base, "http://172.25.80.1:18431/backend-api/codex");
    assert_eq!(
        verify_source_token(ROUTER_TOKEN.as_bytes(), key.as_bytes()).as_deref(),
        Some("wsl:Ubuntu")
    );
}

#[tokio::test]
async fn router_access_adds_the_firewall_rule_when_windows_blocks_wsl() {
    let w = ubuntu(2, WslNetworking::Nat);
    *w.remote.gateway.lock().unwrap() = Some("172.25.80.1".into());
    let sw = w.switcher();
    router_on(&sw).await;
    // Still blocked after the rule (e.g. the Hyper-V firewall): reported.
    let report = sw.wsl_router_access(&wsl_target()).await.unwrap();
    assert!(!report.reachable && report.firewall_changed, "{report:?}");
    assert_eq!(
        *w.router.firewall.lock().unwrap(),
        vec!["172.25.80.1:18431".to_owned()]
    );
    let keys: Vec<String> = report.notes.iter().map(|n| n.text.key.clone()).collect();
    assert!(keys.contains(&"switch.wsl.access_firewall_added".to_owned()));
    assert!(keys.contains(&"switch.wsl.access_unreachable".to_owned()));
    // The user declines the UAC prompt.
    *w.router.firewall_error.lock().unwrap() = Some(superba_core::Error::Cancelled);
    let report = sw.wsl_router_access(&wsl_target()).await.unwrap();
    assert!(!report.reachable);
    let keys: Vec<String> = report.notes.iter().map(|n| n.text.key.clone()).collect();
    assert_eq!(keys, vec!["switch.wsl.access_cancelled".to_owned()]);
    // Without a router there is nothing to reach.
    let w = ubuntu(2, WslNetworking::Nat);
    assert!(w.switcher().wsl_router_access(&wsl_target()).await.is_err());
}

#[tokio::test]
async fn a_restore_restores_distributions_superba_changed_and_skips_the_others() {
    let w = ubuntu(1, WslNetworking::Nat);
    let sw = w.switcher();
    let restore = |target| SwitchRequest {
        intent: SwitchIntent::Restore { target },
        ..req("work")
    };
    let plan = sw
        .plan(restore(RestoreTarget::Original {
            provider: superba_core::ProviderRestore::KeepOpenai,
        }))
        .await
        .unwrap();
    let scoped = w.remote.scoped.lock().unwrap().clone();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].1, RemoteApplyScope::Both);
    assert!(matches!(scoped[0].2.mode, ResolvedMode::Original { .. }));
    assert_eq!(plan.remotes.len(), 1);
    // Never written there: nothing to restore, no failure.
    w.remote.blocked.lock().unwrap().insert(
        "WSL: Ubuntu".into(),
        vec![superba_core::BlockReason::NoBaseline],
    );
    let plan = sw
        .plan(restore(RestoreTarget::Original {
            provider: superba_core::ProviderRestore::KeepOpenai,
        }))
        .await
        .unwrap();
    assert!(plan.remotes.is_empty());
    assert!(notice_keys(&plan).contains(&"switch.wsl.nothing_to_restore".to_owned()));
}

#[tokio::test]
async fn the_wsl_overview_lists_distributions_and_router_listeners() {
    let w = ubuntu(2, WslNetworking::Nat);
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    let sw = w.switcher();
    let o = sw.wsl_overview().await.unwrap();
    assert_eq!(o.distros.len(), 1);
    assert_eq!(o.distros[0].host.target_id, wsl_target());
    assert!(o.distros[0].settings.follows(HostKind::Wsl));
    assert!(o.router_wsl_listen.is_empty());
}

#[derive(Default)]
struct Lines(std::sync::Mutex<Vec<String>>);

impl superba_core::SwitchEvents for Lines {
    fn emit(&self, event: superba_core::SwitchEvent) {
        if let superba_core::SwitchEvent::Log { text, .. } = event {
            self.0.lock().unwrap().push(text.render_fallback());
        }
    }
}

#[tokio::test]
async fn sessions_in_a_distribution_migrate_with_the_linux_helper_inside_it() {
    let mut w = ubuntu(2, WslNetworking::Nat);
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("spb-linux-x86_64");
    std::fs::write(&helper, b"#!/bin/sh\n").unwrap();
    w.ctx.env = Arc::new(
        superba_core::MapEnv::new().with(superba_switch_wsl_helper_env(), helper.as_os_str()),
    );
    let request = superba_core::MigrationRequest {
        to_provider: "openai".into(),
        selectors: vec![superba_core::MigrationSelector::Provider {
            provider: "krill".into(),
        }],
        include_archived: false,
        exclude: vec!["t-1".into()],
    };
    // While the app's WSL app-server runs (the fake host has one), it waits.
    let lines = Arc::new(Lines::default());
    let e = w
        .switcher()
        .host_migrate_sessions(
            &wsl_target(),
            &request,
            superba_core::Locale::ZhCn,
            lines.clone(),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, superba_core::Error::Blocked { .. }), "{e}");
    assert!(
        e.to_string().contains("ChatGPT app 正连接着 WSL: Ubuntu"),
        "{e}"
    );
    *w.remote.idle.lock().unwrap() = true;
    let report = w
        .switcher()
        .host_migrate_sessions(
            &wsl_target(),
            &request,
            superba_core::Locale::En,
            lines.clone(),
        )
        .await
        .unwrap();
    assert!(report.ok && report.report.is_some());
    assert_eq!(
        *w.remote.helper_runs.lock().unwrap(),
        vec![(
            "WSL: Ubuntu".to_owned(),
            [
                "sessions",
                "migrate",
                "--from",
                "krill",
                "--to",
                "openai",
                "--yes",
                "--locale",
                "en",
                "--no-archived",
                "--exclude-thread",
                "t-1"
            ]
            .map(str::to_owned)
            .to_vec()
        )]
    );
    let seen = lines.0.lock().unwrap().clone();
    assert!(seen[0].contains("WSL: Ubuntu"), "{seen:?}");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("Migrated 2 of 2 sessions")
    );
    // The sessions view offers the command for this computer.
    let sessions = w.switcher().host_sessions(&wsl_target()).await.unwrap();
    assert!(!sessions.read_only);
    assert_eq!(
        sessions.migrate_command.as_deref(),
        Some("spb sessions --host wsl:Ubuntu migrate --from krill")
    );
}

fn superba_switch_wsl_helper_env() -> &'static str {
    "SUPERBA_WSL_HELPER"
}
