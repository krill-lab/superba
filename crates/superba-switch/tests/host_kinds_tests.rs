//! Host kinds in the switch service (user bug 36): the remote-control devices
//! and WSL distributions the app reports are listed read-only and refused by
//! every SSH operation; ChatGPT-hosted and unknown hosts are not listed; a
//! switch never plans, probes or writes a host Superba does not manage.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    CodexVersion, Error, HostKind, HostSettings, HostSource, InstallRequest, InstallTarget,
    ProfileId, RemoteHost, SwitchRequest, SwitchService, TargetId,
};

const RC: &str = "remote-control:env_e_6a1b8b37df48832d9f0";

fn app_host(id: &str, label: &str, kind: HostKind) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(id),
        label: label.into(),
        host_id: Some(id.into()),
        kind,
        ssh: superba_core::SshEndpoint::default(),
        sources: vec![HostSource::Cdp],
        connected: None,
    }
}

/// An SSH host `dev`, a remote-control device, a WSL distribution, the app's
/// cloud host and an unknown id.
fn world() -> World {
    let w = World::new();
    w.remote.hosts.lock().unwrap().extend([
        remote_host("dev", Some("h-dev")),
        app_host(RC, "Studio Mac", HostKind::RemoteControl),
        app_host("remote-wsl:Ubuntu", "WSL: Ubuntu", HostKind::Wsl),
        app_host("durable", "", HostKind::Cloud),
        app_host("mystery:1", "", HostKind::Unknown),
    ]);
    w
}

fn rc() -> TargetId {
    TargetId::remote(RC)
}

fn refused<T: std::fmt::Debug>(r: superba_core::Result<T>, what: &str) {
    match r {
        Err(Error::Unsupported { what: detail }) => {
            assert!(
                detail.contains("does not reach it over SSH"),
                "{what}: {detail}"
            );
            assert!(!detail.contains(RC), "{what}: raw id in {detail}");
        }
        other => panic!("{what}: expected Unsupported, got {other:?}"),
    }
}

#[tokio::test]
async fn only_ssh_and_read_only_kinds_are_listed() {
    let w = world();
    // A 0.1.0 Superba let the user follow the device.
    w.profiles.set.lock().unwrap().settings.hosts.insert(
        rc(),
        HostSettings {
            follow_switches: true,
            last_sync: None,
        },
    );
    let sw = w.switcher();
    let hosts = sw.hosts().await.unwrap();
    let rows: Vec<(String, HostKind, bool, bool)> = hosts
        .iter()
        .map(|h| {
            (
                h.host.target_id.to_string(),
                h.host.kind,
                h.hot_restart,
                h.settings.follow_switches,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("ssh:dev".into(), HostKind::Ssh, true, false),
            (format!("ssh:{RC}"), HostKind::RemoteControl, false, false),
            ("ssh:remote-wsl:Ubuntu".into(), HostKind::Wsl, false, false),
        ]
    );
    assert!(hosts[1].state.is_none() && hosts[1].pending_sync.is_none());
    let overview = sw.overview().await.unwrap();
    let kinds: Vec<HostKind> = overview.remotes.iter().map(|r| r.host.kind).collect();
    assert_eq!(
        kinds,
        vec![HostKind::Ssh, HostKind::RemoteControl, HostKind::Wsl]
    );
    // The Updates page offers Codex installs on SSH hosts only.
    let versions = sw.versions(false).await.unwrap();
    let remote: Vec<String> = versions
        .remote
        .iter()
        .map(|r| r.target_id.to_string())
        .collect();
    assert_eq!(remote, vec!["ssh:dev".to_owned()]);
}

#[tokio::test]
async fn every_ssh_operation_refuses_a_remote_control_device() {
    let w = world();
    let sw = w.switcher();
    for id in [rc(), TargetId::remote("remote-wsl:Ubuntu")] {
        refused(sw.host_probe(&id).await, "probe");
        refused(sw.set_host_follow(&id, true).await, "follow");
        refused(sw.host_sync_plan(&id).await, "sync plan");
        refused(sw.host_sessions(&id).await, "sessions");
        refused(
            sw.install_codex(InstallRequest {
                version: CodexVersion::parse("0.162.0").unwrap(),
                target: InstallTarget::Remote {
                    target_id: id.clone(),
                },
            })
            .await,
            "install",
        );
        refused(
            sw.rollback_codex(InstallTarget::Remote {
                target_id: id.clone(),
            })
            .await,
            "rollback",
        );
    }
    // Hidden kinds are not found at all.
    assert!(matches!(
        sw.host_probe(&TargetId::remote("durable")).await,
        Err(Error::NotFound { .. })
    ));
    // Nothing was written on any host.
    assert!(w.writes().iter().all(|c| !c.starts_with("remote.")));
}

#[tokio::test]
async fn a_stale_follow_can_be_turned_off() {
    let w = world();
    w.profiles.set.lock().unwrap().settings.hosts.insert(
        rc(),
        HostSettings {
            follow_switches: true,
            last_sync: None,
        },
    );
    let sw = w.switcher();
    let view = sw.set_host_follow(&rc(), false).await.unwrap();
    assert_eq!(view.host.kind, HostKind::RemoteControl);
    assert!(w.profiles.set.lock().unwrap().settings.hosts.is_empty());
}

#[tokio::test]
async fn a_switch_skips_a_followed_device_and_blocks_one_asked_for() {
    let w = world();
    {
        let mut set = w.profiles.set.lock().unwrap();
        for id in [rc(), TargetId::remote("dev")] {
            set.settings.hosts.insert(
                id,
                HostSettings {
                    follow_switches: true,
                    last_sync: None,
                },
            );
        }
    }
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let planned: Vec<String> = plan
        .remotes
        .iter()
        .map(|p| p.target_id.to_string())
        .collect();
    assert_eq!(planned, vec!["ssh:dev".to_owned()]);
    let scoped: Vec<String> = w
        .remote
        .scoped
        .lock()
        .unwrap()
        .iter()
        .map(|(h, _, _)| h.clone())
        .collect();
    assert_eq!(scoped, vec!["dev".to_owned()]);
    let skip = plan
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.hosts.not_ssh")
        .expect("a note says why the device is skipped");
    assert_eq!(skip.severity, superba_core::Severity::Info);
    let rendered = superba_switch::messages::catalog(superba_core::Locale::ZhCn).text(&skip.text);
    assert_eq!(
        rendered,
        "Studio Mac 是远程控制设备：ChatGPT app 不通过 SSH 连接它，Superba 无法切换它，已跳过。"
    );

    // Asked for explicitly: blocked with a warning, still never probed.
    let mut r = SwitchRequest::profile(ProfileId::new("work"));
    r.targets.local = false;
    r.targets.remotes = vec![rc()];
    let plan = sw.plan(r).await.unwrap();
    assert!(plan.remotes.iter().all(|p| p.target_id != rc()));
    assert!(
        plan.warnings
            .iter()
            .any(|n| n.text.key == "switch.hosts.not_ssh"
                && n.severity == superba_core::Severity::Warning)
    );
    assert!(w.writes().iter().all(|c| !c.contains("Studio")));
}
