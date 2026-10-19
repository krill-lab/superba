//! `spb router enable` without a member that can carry traffic (user bug 28)
//! and `spb router doctor`.

mod common;

use clap::Parser;
use common::*;
use superba_core::{AuthChoice, KeyRef, ProfileId};

#[tokio::test]
async fn router_line_promotes_the_selected_line_without_planning_a_codex_switch() {
    let service = FakeService::new();
    {
        let mut set = service.profiles.lock().unwrap();
        set.settings.router.enabled = true;
        set.settings.router.members = vec!["work".into()];
        set.profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap()
            .set_line_priority(["cn-fast".into(), "cdn".into(), "overseas".into()]);
    }
    let (code, stdout, stderr) =
        invoke(service.clone(), &["router", "line", "work", "overseas"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let calls = service.calls();
    assert!(calls.plans.is_empty());
    assert_eq!(
        calls.saves[0].profile.line_priority(),
        ["overseas", "cn-fast", "cdn"]
            .map(superba_core::LineId::new)
            .to_vec()
    );
    let (code, _, _) = invoke(service.clone(), &["router", "line", "work", "missing"]).await;
    assert_ne!(code, 0);
    assert_eq!(service.calls().saves.len(), 1);
    let (code, _, _) = invoke(service.clone(), &["router", "line", "original", "overseas"]).await;
    assert_ne!(code, 0);
    assert_eq!(service.calls().saves.len(), 1);
}

fn key(service: &FakeService) {
    let mut set = service.profiles.lock().unwrap();
    let work = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    work.key_ref = Some(KeyRef::Keyring {
        service: "superba".into(),
        account: "profile:work".into(),
    });
}

#[tokio::test]
async fn enable_without_a_keyed_member_says_why_and_what_to_run_next() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(service.clone(), &["router", "enable", "--yes"]).await;
    assert_eq!(code, 3, "{stderr}");
    for needle in [
        "Turning on the local router needs at least one member with an API key.",
        "Profile work (work): no API key entered yet",
        "Next:",
        "spb key set --profile work --stdin",
        "spb router doctor",
    ] {
        assert!(stdout.contains(needle), "{needle:?} missing in {stdout}");
    }
    let calls = service.calls();
    assert!(calls.router_installs.is_empty(), "nothing installed");
    assert!(calls.plans.is_empty(), "nothing planned");

    // Chinese, same facts.
    let (code, stdout, _) = invoke(
        service.clone(),
        &["--locale", "zh-CN", "router", "enable", "--yes"],
    )
    .await;
    assert_eq!(code, 3);
    assert!(
        stdout.contains("开启路由需要至少一个有 API key 的成员。"),
        "{stdout}"
    );
    assert!(stdout.contains("还没有填写 API key"), "{stdout}");
    assert!(stdout.contains("下一步："), "{stdout}");
}

/// User bug 78: a member whose direct auth is PAT (`spb switch --auth pat`)
/// still has its key, which the router sends as an API key.
#[tokio::test]
async fn enable_with_a_member_set_to_pat_installs_the_router() {
    let service = FakeService::new();
    key(&service);
    service
        .profiles
        .lock()
        .unwrap()
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap()
        .set_auth(AuthChoice::Pat);
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["router", "enable", "--member", "work", "--yes"],
    )
    .await;
    assert_ne!(code, 3, "not refused: {stdout}{stderr}");
    assert!(!stdout.contains("needs at least one member"), "{stdout}");
    assert!(!stdout.contains("personal access token"), "{stdout}");
    assert_eq!(
        service.calls().router_installs,
        vec![vec![ProfileId::new("work")]],
        "{stderr}"
    );
    // The doctor lists it as a member that can carry traffic.
    let (code, stdout, _) = invoke(service.clone(), &["--json", "router", "doctor"]).await;
    assert_eq!(code, 0);
    assert!(!stdout.contains("pat_only"), "{stdout}");
}

#[tokio::test]
async fn enable_with_an_eligible_provider_that_is_not_a_member_names_it() {
    let service = FakeService::new();
    key(&service);
    // Router members stored: none. `work` can be one.
    let (code, stdout, _) = invoke(service.clone(), &["router", "enable", "--yes"]).await;
    assert_eq!(code, 3);
    assert!(
        stdout.contains("can be a member (has an API key)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("spb router enable --member work"),
        "{stdout}"
    );
    // With it as a member the router is installed and planned.
    let (_, _, stderr) = invoke(
        service.clone(),
        &["router", "enable", "--member", "work", "--yes"],
    )
    .await;
    assert_eq!(
        service.calls().router_installs,
        vec![vec![ProfileId::new("work")]],
        "{stderr}"
    );
}

#[tokio::test]
async fn doctor_lists_providers_with_reasons_even_without_router_facts() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(service.clone(), &["router", "doctor"]).await;
    assert_eq!(code, 0, "{stderr}");
    for needle in [
        "local router diagnostics",
        "! Turning on the local router needs at least one member with an API key.",
        "[Providers]",
        "Profile work (work): not a member · no API key entered yet",
    ] {
        assert!(stdout.contains(needle), "{needle:?} missing in {stdout}");
    }
    let (code, stdout, _) =
        invoke(service.clone(), &["--locale", "zh-CN", "router", "doctor"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("本地路由诊断") && stdout.contains("[供应商]"),
        "{stdout}"
    );
    let (code, stdout, _) = invoke(service.clone(), &["--json", "router", "doctor"]).await;
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["findings"].as_array().is_some_and(|f| !f.is_empty()));
    // Without router facts there is nothing to run in the foreground.
    let (code, _, stderr) = invoke(service.clone(), &["router", "doctor", "--foreground"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("not known here"), "{stderr}");
}

#[test]
fn doctor_parses_its_foreground_flag() {
    let cli =
        superba_cli::Cli::try_parse_from(["spb", "router", "doctor", "--foreground"]).unwrap();
    assert!(format!("{cli:?}").contains("Doctor { foreground: true }"));
}

/// User bug 39: Krill ("work") with its key is already the member and the
/// router is answering.
fn member_running(service: &FakeService) {
    key(service);
    service.profiles.lock().unwrap().settings.router.members = vec![ProfileId::new("work")];
    let mut v2 = service.v2.lock().unwrap();
    v2.router.installed = true;
    v2.router.running = true;
    v2.router.port = Some(50_947);
    v2.router.settings.members = vec![ProfileId::new("work")];
}

#[tokio::test]
async fn enable_with_a_keyed_member_and_a_running_router_plans_router_mode() {
    let service = FakeService::new();
    member_running(&service);
    // The doctor and enable agree: work is a member that can carry traffic.
    let (code, stdout, _) =
        invoke(service.clone(), &["--locale", "zh-CN", "router", "doctor"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("成员 · 可以作为成员（有 API key）"),
        "{stdout}"
    );
    assert!(!stdout.contains("开启路由需要至少一个"), "{stdout}");
    let (code, stdout, stderr) = invoke(service.clone(), &["router", "enable", "--yes"]).await;
    assert!(
        !stdout.contains("needs at least one member"),
        "{stdout}{stderr}"
    );
    assert_ne!(code, 3, "not blocked: {stdout}{stderr}");
    let calls = service.calls();
    assert!(
        calls.router_installs.is_empty(),
        "the running router is kept"
    );
    assert!(
        calls.plans.iter().any(|r| matches!(
            r.intent,
            superba_core::SwitchIntent::Router { enable: true }
        )),
        "{:?}",
        calls.plans
    );
}

#[tokio::test]
async fn enable_while_the_router_runs_plans_router_mode_for_a_member_set_to_pat() {
    let service = FakeService::new();
    member_running(&service);
    service
        .profiles
        .lock()
        .unwrap()
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap()
        .set_auth(AuthChoice::Pat);
    let (code, stdout, stderr) = invoke(service.clone(), &["router", "enable", "--yes"]).await;
    assert_ne!(code, 3, "not blocked: {stdout}{stderr}");
    assert!(!stdout.contains("needs at least one member"), "{stdout}");
    assert!(
        service.calls().plans.iter().any(|r| matches!(
            r.intent,
            superba_core::SwitchIntent::Router { enable: true }
        )),
        "router mode planned"
    );
}
