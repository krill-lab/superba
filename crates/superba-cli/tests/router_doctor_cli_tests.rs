//! `spb router enable` without a member that can carry traffic (user bug 28)
//! and `spb router doctor`.

mod common;

use clap::Parser;
use common::*;
use superba_core::{AuthChoice, KeyRef, ProfileId};

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
        "Turning on the local router needs at least one member that uses an API key.",
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
        stdout.contains("开启路由需要至少一个用 API key 的成员。"),
        "{stdout}"
    );
    assert!(stdout.contains("还没有填写 API key"), "{stdout}");
    assert!(stdout.contains("下一步："), "{stdout}");
}

#[tokio::test]
async fn enable_with_a_pat_only_member_suggests_switching_its_auth() {
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
    let (code, stdout, _) = invoke(
        service.clone(),
        &["router", "enable", "--member", "work", "--yes"],
    )
    .await;
    assert_eq!(code, 3);
    assert!(
        stdout.contains("(work, member): set to use a personal access token (PAT) only"),
        "{stdout}"
    );
    assert!(
        stdout.contains("spb profile edit work --auth auto"),
        "{stdout}"
    );
    assert!(service.calls().router_installs.is_empty());

    // JSON: one document naming each provider and why.
    let (code, stdout, _) = invoke(
        service.clone(),
        &["--json", "router", "enable", "--member", "work", "--yes"],
    )
    .await;
    assert_eq!(code, 3);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["error"], "no_eligible_member");
    assert_eq!(v["providers"][0]["id"], "work");
    assert_eq!(v["providers"][0]["eligibility"], "pat_only");
}

#[tokio::test]
async fn enable_with_an_eligible_provider_that_is_not_a_member_names_it() {
    let service = FakeService::new();
    key(&service);
    // Router members stored: none. `work` can be one.
    let (code, stdout, _) = invoke(service.clone(), &["router", "enable", "--yes"]).await;
    assert_eq!(code, 3);
    assert!(
        stdout.contains("can be a member (uses an API key)"),
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
        "! Turning on the local router needs at least one member that uses an API key.",
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
