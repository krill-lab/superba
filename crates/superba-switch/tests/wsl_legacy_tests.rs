//! The desktop app's "Run Codex in WSL" mode (user bug 54 (b)): the app runs
//! this computer's Codex inside the default distribution, reading this
//! computer's `config.toml` and `auth.json`. A local switch says so; router mode
//! is refused when that distribution cannot reach Windows on `127.0.0.1` (WSL 2
//! behind NAT), allowed on WSL 1 and with mirrored networking.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use superba_core::{
    BlockReason, ExecuteOptions, NoEvents, OutcomeStatus, ProfileId, ResolvedMode, RestartKind,
    RouterInstallRequest, SafeDefaults, SwitchPlan, SwitchRequest, SwitchService, WslNetworking,
};
use superba_switch::Switcher;
use superba_switch::router::RouterControl;

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
    w.remote
        .http
        .lock()
        .unwrap()
        .insert("127.0.0.1:18431".into(), router_health());
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

async fn execute(sw: &Switcher, plan: SwitchPlan) -> superba_core::SwitchOutcome {
    sw.execute(
        plan,
        ExecuteOptions {
            allow_full_restart: true,
            ..Default::default()
        },
        std::sync::Arc::new(SafeDefaults),
        std::sync::Arc::new(NoEvents),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn legacy_router_auth_environment_is_written_before_the_app_reopens() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.idle.lock().unwrap() = true;
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    let ResolvedMode::Token {
        auth, endpoints, ..
    } = &plan.remotes[0].desired.as_ref().unwrap().mode
    else {
        panic!("token mode");
    };
    assert_eq!(*auth, superba_core::AuthMode::ApiKey);
    assert_eq!(
        endpoints.authapi_origin.as_ref().map(|o| o.as_str()),
        Some(format!(
            "http://127.0.0.1:{}",
            w.router.endpoint().unwrap().port
        )),
        "the WSL API-key home must also prepare its local legacy PAT consumer"
    );
    w.log.lock().unwrap().clear();
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    let calls = w.calls();
    let applied = calls
        .iter()
        .position(|c| c == "remote.apply WSL: Ubuntu")
        .unwrap();
    let launched = calls
        .iter()
        .position(|c| c.starts_with("app.launch "))
        .unwrap();
    assert!(applied < launched, "{calls:#?}");
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "remote.apply WSL: Ubuntu")
            .count(),
        1
    );
}

#[tokio::test]
async fn legacy_direct_line_switch_prepares_wsl_before_the_local_hot_restart() {
    let w = world(1, WslNetworking::Nat, Some("config"));
    *w.remote.idle.lock().unwrap() = true;
    let sw = w.switcher();
    let mut request = req("work");
    if let superba_core::SwitchIntent::Profile { line_id, .. } = &mut request.intent {
        *line_id = Some(superba_core::LineId::new("cn-fast"));
    }
    let plan = sw.plan(request).await.unwrap();
    assert!(plan.primary.is_hot(), "{:?}", plan.primary);
    w.log.lock().unwrap().clear();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    let calls = w.calls();
    let applied = calls
        .iter()
        .position(|c| c == "remote.apply WSL: Ubuntu")
        .unwrap();
    let restarted = calls
        .iter()
        .position(|c| c.starts_with("app.hot_restart"))
        .unwrap();
    assert!(applied < restarted, "{calls:#?}");
}

#[tokio::test]
async fn a_failed_legacy_wsl_write_prevents_the_local_restart() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.idle.lock().unwrap() = true;
    w.remote.fail_apply.lock().unwrap().insert(
        "WSL: Ubuntu".into(),
        superba_core::Error::other("write failed"),
    );
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    w.log.lock().unwrap().clear();
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Partial);
    assert_eq!(outcome.remotes.len(), 1);
    assert_eq!(outcome.remotes[0].status, OutcomeStatus::Failed);
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.text.key == "switch.wsl.legacy_restart_skipped")
    );
    assert!(
        !w.calls()
            .iter()
            .any(|c| c.starts_with("app.launch ") || c.starts_with("app.hot_restart"))
    );
}

#[tokio::test]
async fn a_stopped_legacy_distribution_is_not_started_or_restarted_with_stale_env() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    w.remote.hosts.lock().unwrap()[0]
        .wsl
        .as_mut()
        .unwrap()
        .set_running(false);
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(plan.remotes.is_empty());
    w.log.lock().unwrap().clear();
    let result = sw
        .execute(
            plan,
            ExecuteOptions {
                allow_full_restart: true,
                ..Default::default()
            },
            std::sync::Arc::new(SafeDefaults),
            std::sync::Arc::new(NoEvents),
        )
        .await;
    assert!(
        matches!(result, Err(superba_core::Error::Wsl { .. })),
        "{result:?}"
    );
    assert!(w.remote.probes.lock().unwrap().is_empty());
    assert!(
        !w.calls()
            .iter()
            .any(|c| c.starts_with("app.quit") || c == "config.apply")
    );
}

#[tokio::test]
async fn follow_off_keeps_its_env_and_refuses_a_conflicting_local_restart() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.authapi_env.lock().unwrap() = superba_core::EnvObservation::Present(OVERSEAS.into());
    let sw = w.switcher();
    sw.set_host_follow(&superba_core::wsl::target_id("Ubuntu"), false)
        .await
        .unwrap();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(plan.remotes.is_empty());
    w.log.lock().unwrap().clear();
    let result = sw
        .execute(
            plan,
            ExecuteOptions {
                allow_full_restart: true,
                ..Default::default()
            },
            std::sync::Arc::new(SafeDefaults),
            std::sync::Arc::new(NoEvents),
        )
        .await;
    assert!(
        matches!(result, Err(superba_core::Error::Wsl { .. })),
        "{result:?}"
    );
    assert!(!w.calls().iter().any(|c| c.starts_with("remote.apply")
        || c.starts_with("app.quit")
        || c == "config.apply"));
}

#[tokio::test]
async fn without_a_running_app_a_stopped_distro_stays_pending() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    w.set_session(None);
    w.remote.hosts.lock().unwrap()[0]
        .wsl
        .as_mut()
        .unwrap()
        .set_running(false);
    w.remote.written.lock().unwrap().push("WSL: Ubuntu".into());
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    let result = execute(&sw, plan).await;
    assert_eq!(result.status, OutcomeStatus::Switched);
    assert!(result.remotes.is_empty());
    assert!(w.remote.probes.lock().unwrap().is_empty());
    assert!(!w.calls().iter().any(|c| c.starts_with("app.launch ")));
    assert!(sw.hosts().await.unwrap().iter().any(|h| h.host.target_id
        == superba_core::wsl::target_id("Ubuntu")
        && h.pending.is_some()));
}

#[tokio::test]
async fn follow_off_with_matching_env_does_not_write_the_independent_home() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.authapi_env.lock().unwrap() = superba_core::EnvObservation::Present(OVERSEAS.into());
    let sw = w.switcher();
    sw.set_host_follow(&superba_core::wsl::target_id("Ubuntu"), false)
        .await
        .unwrap();
    let plan = sw.plan(req("work")).await.unwrap();
    w.log.lock().unwrap().clear();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert!(!w.calls().iter().any(|c| c.starts_with("remote.apply")));
}

#[tokio::test]
async fn a_normal_wsl_host_still_switches_after_the_local_app() {
    let w = world(1, WslNetworking::Nat, None);
    *w.remote.idle.lock().unwrap() = true;
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    let ResolvedMode::Token { endpoints, .. } = &plan.remotes[0].desired.as_ref().unwrap().mode
    else {
        panic!("token")
    };
    assert!(endpoints.authapi_origin.is_none());
    w.log.lock().unwrap().clear();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    let calls = w.calls();
    let launched = calls
        .iter()
        .position(|c| c.starts_with("app.launch "))
        .unwrap();
    let applied = calls
        .iter()
        .position(|c| c == "remote.apply WSL: Ubuntu")
        .unwrap();
    assert!(launched < applied, "{calls:#?}");
}

#[tokio::test]
async fn legacy_router_off_prepares_direct_auth_before_restarting() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.idle.lock().unwrap() = true;
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    sw.router_set_enabled(false).await.unwrap();
    let plan = sw.plan(req("work")).await.unwrap();
    w.log.lock().unwrap().clear();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert_eq!(w.remote.authapi_env.lock().unwrap().value(), Some(OVERSEAS));
    let calls = w.calls();
    let applied = calls
        .iter()
        .position(|c| c == "remote.apply WSL: Ubuntu")
        .unwrap();
    let restarted = calls
        .iter()
        .position(|c| c.starts_with("app.launch ") || c == "app.hot_restart local")
        .unwrap();
    assert!(applied < restarted, "{calls:#?}");
}

#[tokio::test]
async fn an_old_login_export_after_writing_is_a_failed_dependency() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.idle.lock().unwrap() = true;
    *w.remote.preserve_authapi.lock().unwrap() = true;
    *w.remote.authapi_env.lock().unwrap() = superba_core::EnvObservation::Present(OVERSEAS.into());
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    w.log.lock().unwrap().clear();
    let result = execute(&sw, plan).await;
    assert_eq!(result.status, OutcomeStatus::Partial);
    assert!(matches!(
        result.remotes[0].error,
        Some(superba_core::Error::Wsl { .. })
    ));
    let calls = w.calls();
    assert!(calls.iter().any(|c| c == "remote.rollback WSL: Ubuntu"));
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("app.launch ") || c.starts_with("app.hot_restart"))
    );
}

#[tokio::test]
async fn only_the_legacy_distribution_is_a_local_restart_dependency() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    // Also connected as an independent WSL host before the full app restart.
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(wsl_host("Debian", 1, WslNetworking::Nat));
    w.remote
        .fail_apply
        .lock()
        .unwrap()
        .insert("WSL: Debian".into(), superba_core::Error::other("offline"));
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    w.log.lock().unwrap().clear();
    let result = execute(&sw, plan).await;
    assert_eq!(result.status, OutcomeStatus::Partial);
    let calls = w.calls();
    let ubuntu = calls
        .iter()
        .position(|c| c == "remote.apply WSL: Ubuntu")
        .unwrap();
    let launched = calls
        .iter()
        .position(|c| c.starts_with("app.launch "))
        .unwrap();
    let debian = calls
        .iter()
        .position(|c| c == "remote.apply WSL: Debian")
        .unwrap();
    assert!(ubuntu < launched && launched < debian, "{calls:#?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("app.hot_restart")),
        "no IPC to the closed app: {calls:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_local_rollback_also_undoes_the_prepared_distribution() {
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.idle.lock().unwrap() = true;
    *w.config.tamper_after_apply.lock().unwrap() = true;
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    w.log.lock().unwrap().clear();
    let result = execute(&sw, plan).await;
    assert_eq!(result.status, OutcomeStatus::RolledBack);
    assert_eq!(result.remotes[0].status, OutcomeStatus::RolledBack);
    let calls = w.calls();
    let remote = calls
        .iter()
        .position(|c| c == "remote.rollback WSL: Ubuntu")
        .unwrap();
    let local = calls.iter().position(|c| c == "config.rollback").unwrap();
    assert!(remote < local, "{calls:#?}");
    let entries = superba_switch::journal::SwitchJournal::new(&w.ctx.data_dir)
        .entries()
        .unwrap();
    assert!(entries[0].remotes[0].rolled_back);
}

#[tokio::test]
async fn recovery_restores_the_dependency_first_without_starting_a_stopped_distro() {
    use superba_switch::journal::{JournalStatus, SwitchJournal};
    let w = world(1, WslNetworking::Nat, Some("global"));
    *w.remote.idle.lock().unwrap() = true;
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    let journal = SwitchJournal::new(&w.ctx.data_dir);
    let mut entry = journal.entries().unwrap().remove(0);
    entry.status = JournalStatus::RollingBack;
    journal.write(&entry).unwrap();
    // Crash after journaling the undo, before restoring the early dependency.
    w.remote.hosts.lock().unwrap()[0]
        .wsl
        .as_mut()
        .unwrap()
        .set_running(false);
    w.log.lock().unwrap().clear();
    assert_eq!(
        sw.recover().await.unwrap().entries[0].action,
        superba_core::JournalAction::Drift
    );
    assert!(
        !w.calls()
            .iter()
            .any(|c| c == "config.rollback" || c.starts_with("remote.rollback"))
    );
    w.remote.hosts.lock().unwrap()[0]
        .wsl
        .as_mut()
        .unwrap()
        .set_running(true);
    w.log.lock().unwrap().clear();
    assert_eq!(
        sw.recover().await.unwrap().entries[0].action,
        superba_core::JournalAction::RolledBack
    );
    let calls = w.calls();
    let remote = calls
        .iter()
        .position(|c| c == "remote.rollback WSL: Ubuntu")
        .unwrap();
    let local = calls.iter().position(|c| c == "config.rollback").unwrap();
    assert!(remote < local, "{calls:#?}");
    assert!(journal.entries().unwrap()[0].remotes[0].rolled_back);
}
