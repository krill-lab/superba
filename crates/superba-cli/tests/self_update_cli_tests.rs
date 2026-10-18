//! `spb self-update`: check output, install confirmation, JSON.

mod common;

use std::sync::Arc;

use clap::Parser;
use common::{FakeService, invoke_runtime, one_json, runtime};
use superba_cli::{Cli, Output};
use superba_core::{
    SelfInstallOutcome, SelfRelease, SelfUpdateReport, SelfUpdateStatus, SelfUpdateView, Timestamp,
};

fn release(version: &str) -> SelfRelease {
    SelfRelease {
        tag: format!("v{version}"),
        version: version.into(),
        prerelease: false,
        notes: "notes".into(),
        published_at: None,
        url: String::new(),
        asset: Some(format!("superba-{version}-linux-x86_64.tar.gz")),
        asset_size: Some(10),
    }
}

fn view(status: SelfUpdateStatus, latest: Option<SelfRelease>) -> SelfUpdateView {
    SelfUpdateView {
        current: "0.1.0".into(),
        status,
        latest,
        checked_at: Some(Timestamp::from_millis(1_800_000_000_000)),
        ..SelfUpdateView::default()
    }
}

fn report(outcome: SelfInstallOutcome) -> SelfUpdateReport {
    SelfUpdateReport {
        version: "0.2.0".into(),
        outcome,
        package: None,
        steps: Vec::new(),
        router_restarted: true,
        relaunching: false,
        replaced: vec!["spb".into()],
    }
}

fn set(service: &Arc<FakeService>, v: SelfUpdateView) {
    service.self_update.lock().unwrap().view = v;
}

#[tokio::test]
async fn check_reports_an_available_update_and_never_installs() {
    let service = FakeService::new();
    set(
        &service,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let (code, stdout, _) =
        invoke_runtime(&runtime(service.clone()), &["self-update", "check"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("0.1.0") && stdout.contains("0.2.0"),
        "{stdout}"
    );
    assert!(stdout.contains("spb self-update install"), "{stdout}");
    let s = service.self_update.lock().unwrap();
    assert!(s.checks[0].manual, "a typed command is a manual check");
    assert!(s.installs.is_empty());
}

#[tokio::test]
async fn check_json_is_the_view() {
    let service = FakeService::new();
    set(
        &service,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let (code, stdout, _) =
        invoke_runtime(&runtime(service), &["self-update", "check", "--json"]).await;
    assert_eq!(code, 0);
    let v = one_json(&stdout);
    assert_eq!(v["status"], "available");
    assert_eq!(v["current"], "0.1.0");
    assert_eq!(v["latest"]["version"], "0.2.0");
}

#[tokio::test]
async fn an_inaccessible_release_page_is_a_calm_line_with_exit_zero() {
    let service = FakeService::new();
    set(&service, view(SelfUpdateStatus::PageUnavailable, None));
    let (code, stdout, stderr) = invoke_runtime(&runtime(service), &["self-update", "check"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Cannot check for updates right now"),
        "{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");
}

#[tokio::test]
async fn install_without_yes_refuses_when_not_interactive() {
    let service = FakeService::new();
    set(
        &service,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let (code, _, stderr) =
        invoke_runtime(&runtime(service.clone()), &["self-update", "install"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(service.self_update.lock().unwrap().installs.is_empty());
}

#[tokio::test]
async fn install_with_yes_installs_without_a_relaunch() {
    let service = FakeService::new();
    set(
        &service,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    service.self_update.lock().unwrap().report = Some(report(SelfInstallOutcome::Installed));
    let (code, stdout, _) = invoke_runtime(
        &runtime(service.clone()),
        &["self-update", "install", "--yes"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("Superba 0.2.0 is installed"), "{stdout}");
    assert!(stdout.contains("router was restarted"), "{stdout}");
    let s = service.self_update.lock().unwrap();
    assert_eq!(s.installs.len(), 1);
    assert!(!s.installs[0].relaunch, "the CLI never relaunches");
}

#[tokio::test]
async fn install_in_a_terminal_asks_first() {
    for (answer, installs) in [("y", 1), ("n", 0), ("", 0)] {
        let service = FakeService::new();
        set(
            &service,
            view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
        );
        service.self_update.lock().unwrap().report = Some(report(SelfInstallOutcome::Installed));
        let mut rt = runtime(service.clone());
        let a = answer.to_owned();
        rt.read_stdin_line = Arc::new(move || Ok(a.clone()));
        let cli = Cli::try_parse_from(["spb", "self-update", "install"]).unwrap();
        let (out, stdout, _) = Output::capture();
        let mut out = out.with_terminals(true, true);
        let code = superba_cli::run(&cli, &rt, &mut out).await;
        assert_eq!(code, 0);
        assert!(stdout.contents().contains("[y/N]"), "{}", stdout.contents());
        assert_eq!(
            service.self_update.lock().unwrap().installs.len(),
            installs,
            "answer {answer:?}"
        );
    }
}

#[tokio::test]
async fn nothing_to_install_is_not_an_error() {
    let service = FakeService::new();
    set(
        &service,
        view(SelfUpdateStatus::UpToDate, Some(release("0.1.0"))),
    );
    let (code, stdout, _) = invoke_runtime(
        &runtime(service.clone()),
        &["self-update", "install", "--yes"],
    )
    .await;
    assert_eq!(code, 0);
    assert!(stdout.contains("up to date"), "{stdout}");
    assert!(service.self_update.lock().unwrap().installs.is_empty());
}

#[tokio::test]
async fn a_manual_outcome_prints_the_steps() {
    let service = FakeService::new();
    set(
        &service,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let mut r = report(SelfInstallOutcome::Manual);
    r.router_restarted = false;
    r.steps = vec![
        superba_core::LocalizedText::new(
            "selfupdate.manual.package",
            "The update was downloaded and verified: {path}",
        )
        .arg("path", "/tmp/x.tar.gz"),
    ];
    service.self_update.lock().unwrap().report = Some(r);
    let (code, stdout, _) =
        invoke_runtime(&runtime(service), &["self-update", "install", "--yes"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("cannot be installed in place"), "{stdout}");
    assert!(stdout.contains("/tmp/x.tar.gz"), "{stdout}");
}

#[tokio::test]
async fn the_hidden_restart_router_step_asks_the_service() {
    let service = FakeService::new();
    let (code, stdout, _) = invoke_runtime(
        &runtime(service.clone()),
        &["self-update", "restart-router", "--json"],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(one_json(&stdout)["restarted"], true);
    assert_eq!(service.self_update.lock().unwrap().router_restarts, 1);
}

#[tokio::test]
async fn check_tells_when_the_last_update_failed_and_where_its_log_is() {
    let service = FakeService::new();
    let mut v = view(SelfUpdateStatus::Available, Some(release("0.2.0")));
    v.problem = Some(superba_core::SelfUpdateProblem {
        version: "0.2.0".into(),
        at: Timestamp::from_millis(1_800_000_000_000),
        installed: false,
        reason: superba_core::LocalizedText::new(
            "selfupdate.problem.wait_timeout",
            "Superba did not quit within {secs} s, so the update to {version} was not installed.",
        )
        .arg("secs", 60)
        .arg("version", "0.2.0"),
        log: "/data/logs/selfupdate.log".into(),
        seen: true,
    });
    set(&service, v);
    let (code, stdout, _) = invoke_runtime(&runtime(service), &["self-update", "check"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains(
            "The last update, to 0.2.0, failed: Superba did not quit within 60 s, so the update \
             to 0.2.0 was not installed. Every step is in /data/logs/selfupdate.log"
        ),
        "{stdout}"
    );
}
