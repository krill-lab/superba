//! `spb self-update --channel stable|preview` and `install --version` (user
//! bug 73): the channel is saved like the Preferences choice, the check says
//! what each channel has, an older version is installed only when named.

mod common;

use std::sync::Arc;

use clap::Parser;
use common::{FakeService, invoke_runtime, one_json, runtime};
use superba_cli::Cli;
use superba_core::{
    SelfInstallOutcome, SelfRelease, SelfUpdateReport, SelfUpdateStatus, SelfUpdateView, Timestamp,
    UpdateChannel,
};

fn release(version: &str) -> SelfRelease {
    SelfRelease {
        tag: format!("v{version}"),
        version: version.into(),
        prerelease: version.contains('-'),
        notes: String::new(),
        published_at: None,
        url: String::new(),
        asset: Some(format!("superba-{version}-linux-x86_64.tar.gz")),
        asset_size: Some(10),
    }
}

/// The reported case: a 0.1.1-alpha.3 build.
fn preview_view(status: SelfUpdateStatus, channel: UpdateChannel) -> SelfUpdateView {
    let preview = release("0.1.1-alpha.4");
    let stable = release("0.1.0");
    SelfUpdateView {
        current: "0.1.1-alpha.3".into(),
        status,
        latest: Some(match channel {
            UpdateChannel::Stable => stable.clone(),
            UpdateChannel::Preview => preview.clone(),
        }),
        channel,
        latest_stable: Some(stable),
        latest_preview: (channel == UpdateChannel::Preview).then_some(preview),
        checked_at: Some(Timestamp::from_millis(1_800_000_000_000)),
        ..SelfUpdateView::default()
    }
}

fn set(service: &Arc<FakeService>, v: SelfUpdateView) {
    service.self_update.lock().unwrap().view = v;
}

async fn run(service: &Arc<FakeService>, args: &[&str]) -> (u8, String, String) {
    invoke_runtime(&runtime(service.clone()), args).await
}

#[tokio::test]
async fn check_names_the_channel_and_what_each_channel_has() {
    for (locale, want) in [
        (
            "en",
            [
                "Update channel: preview (the default of a preview build)",
                "You're on preview 0.1.1-alpha.3 · latest preview 0.1.1-alpha.4 · latest stable 0.1.0",
                "Superba 0.1.1-alpha.4 is available",
            ],
        ),
        (
            "zh-CN",
            [
                "更新通道：预览版（预览版构建的默认值）",
                "当前为预览版 0.1.1-alpha.3 · 最新预览版 0.1.1-alpha.4 · 最新稳定版 0.1.0",
                "Superba 0.1.1-alpha.4 可更新",
            ],
        ),
    ] {
        let service = FakeService::new();
        set(
            &service,
            preview_view(SelfUpdateStatus::Available, UpdateChannel::Preview),
        );
        let (code, stdout, stderr) =
            run(&service, &["--locale", locale, "self-update", "check"]).await;
        assert_eq!(code, 0, "{stderr}");
        for w in want {
            assert!(stdout.contains(w), "{locale}: {w:?} in\n{stdout}");
        }
    }
}

#[tokio::test]
async fn the_channel_flag_is_saved_like_the_preferences_choice() {
    let service = FakeService::new();
    set(
        &service,
        preview_view(SelfUpdateStatus::UpToDate, UpdateChannel::Stable),
    );
    assert_eq!(
        service.profiles.lock().unwrap().settings.update_channel,
        None
    );
    let (code, stdout, stderr) =
        run(&service, &["self-update", "check", "--channel", "preview"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("Update channel: preview (saved)"),
        "{stdout}{stderr}"
    );
    assert_eq!(
        service.profiles.lock().unwrap().settings.update_channel,
        Some(UpdateChannel::Preview)
    );
    assert_eq!(service.self_update.lock().unwrap().checks.len(), 1);
    // `install --channel stable` saves it too before looking.
    let (code, _, _) = run(
        &service,
        &["self-update", "install", "--channel", "stable", "--yes"],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(
        service.profiles.lock().unwrap().settings.update_channel,
        Some(UpdateChannel::Stable)
    );
    // An unknown channel is a usage error from the parser.
    assert!(Cli::try_parse_from(["spb", "self-update", "check", "--channel", "beta"]).is_err());
}

#[tokio::test]
async fn a_preview_build_on_stable_is_told_why_nothing_is_offered() {
    let service = FakeService::new();
    let mut v = preview_view(SelfUpdateStatus::UpToDate, UpdateChannel::Stable);
    v.channel_chosen = true;
    set(&service, v);
    let (code, stdout, _) = run(&service, &["self-update", "install", "--yes"]).await;
    assert_eq!(code, 0);
    for w in [
        "Update channel: stable (your choice)",
        "You're on preview 0.1.1-alpha.3 · latest stable 0.1.0",
        "No stable release is newer than this preview build 0.1.1-alpha.3",
        "never installs an older version on its own",
        "spb self-update install --version 0.1.0",
    ] {
        assert!(stdout.contains(w), "{w:?} in\n{stdout}");
    }
    assert!(!stdout.contains("up to date"), "{stdout}");
    assert!(
        service.self_update.lock().unwrap().installs.is_empty(),
        "never a downgrade on its own"
    );
}

#[tokio::test]
async fn the_rate_limit_is_explained_with_the_time_it_ends() {
    let service = FakeService::new();
    let mut v = preview_view(SelfUpdateStatus::UpToDate, UpdateChannel::Stable);
    v.detail = Some("rate limited".into());
    v.limited_until = Some(Timestamp::from_millis(1_800_000_600_000));
    set(&service, v);
    let (code, stdout, _) = run(&service, &["self-update", "check"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("GitHub's limit for checks without an account (60 an hour) is reached"),
        "{stdout}"
    );
    let (_, stdout, _) = run(&service, &["self-update", "check", "--json"]).await;
    let v = one_json(&stdout);
    assert_eq!(v["limited_until"], 1_800_000_600_000_i64);
    assert_eq!(v["channel"], "stable");
    assert_eq!(v["latest_stable"]["version"], "0.1.0");
}

#[tokio::test]
async fn an_older_version_is_installed_only_by_name_and_with_a_warning() {
    let service = FakeService::new();
    set(
        &service,
        preview_view(SelfUpdateStatus::UpToDate, UpdateChannel::Stable),
    );
    service.self_update.lock().unwrap().report = Some(SelfUpdateReport {
        version: "0.1.0".into(),
        outcome: SelfInstallOutcome::Installed,
        package: None,
        steps: Vec::new(),
        router_restarted: false,
        relaunching: false,
        replaced: vec!["spb".into()],
    });
    // Without --yes and without a terminal: refused, nothing installed.
    let (code, _, stderr) = run(&service, &["self-update", "install", "--version", "0.1.0"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(service.self_update.lock().unwrap().installs.is_empty());
    let (code, stdout, stderr) = run(
        &service,
        &["self-update", "install", "--version", "v0.1.0", "--yes"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Superba 0.1.0 is older than the running 0.1.1-alpha.3"),
        "{stdout}"
    );
    assert!(stdout.contains("Superba 0.1.0 is installed"), "{stdout}");
    let installs = service.self_update.lock().unwrap().installs.clone();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].version.as_deref(), Some("0.1.0"));
    assert!(!installs[0].relaunch);
    // The running version is not "installed again".
    let (code, _, stderr) = run(
        &service,
        &[
            "self-update",
            "install",
            "--version",
            "0.1.1-alpha.3",
            "--yes",
        ],
    )
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("is the running version"), "{stderr}");
    // A named version and a channel do not mix.
    assert!(
        Cli::try_parse_from([
            "spb",
            "self-update",
            "install",
            "--version",
            "0.1.0",
            "--channel",
            "stable"
        ])
        .is_err()
    );
}
