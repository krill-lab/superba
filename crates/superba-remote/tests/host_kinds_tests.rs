//! Host kinds in discovery and the SSH paths (user bug 36): the desktop app
//! records SSH connections in its global state, and its renderer also reports
//! ChatGPT remote-control devices, WSL distributions, ChatGPT-hosted and unknown
//! hosts. Every discovered host keeps the kind its app id says; only SSH hosts
//! get an endpoint, and no other kind ever reaches the SSH transport.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{engine, original_profile};
use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{
    CodexHome, CodexProcessKind, CodexVersion, DataDir, EnvObservation, Error, HostKind,
    HostSource, ProcessInfo, RemoteApplyReport, RemoteApplyScope, RemoteHost, RemoteHosts,
    RemoteVerifyRequest, SshEndpoint, TargetId, Timestamp,
};
use superba_remote::discover::{
    KEY_AUTO_CONNECT, KEY_CONNECTIONS, KEY_PROJECTS, discovered_alias, host_from_app, merge_hosts,
    parse_global_state, saved_connection_kind,
};
use superba_remote::ssh::{RemoteShell, require_ssh};
use superba_remote::{ScriptOutput, SshRemoteHosts};

const RC: &str = "remote-control:env_e_6a1b8b37df48832d9f0";

fn from_app(id: &str, label: &str, kind: HostKind) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(id),
        label: label.into(),
        host_id: Some(id.into()),
        kind,
        ssh: SshEndpoint::default(),
        sources: vec![HostSource::Cdp],
        connected: None,
        wsl: None,
    }
}

fn link(host: &str, argv: &[&str]) -> ProcessInfo {
    ProcessInfo {
        pid: 7,
        ppid: None,
        kind: CodexProcessKind::RemoteLink { host: host.into() },
        exe: None,
        cmdline: argv.iter().map(|s| (*s).to_owned()).collect(),
        started_at: None,
        authapi_env: EnvObservation::default(),
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

/// The global state of an app with two SSH connections (one legacy id) whose
/// projects and auto-connect preferences also mention a remote-control device.
fn global_state() -> Vec<u8> {
    serde_json::to_vec(&json!({
        KEY_CONNECTIONS: [
            {"hostId": "remote-ssh-discovered:devbox", "displayName": "devbox",
             "source": "discovered", "alias": "devbox", "hostname": null,
             "sshPort": null, "identity": null},
            {"hostId": "remote-ssh-codex-managed:5a0c", "displayName": "gpu-01",
             "source": "codex-managed", "alias": null, "hostname": "me@203.0.113.7",
             "sshPort": 2222, "identity": "/k/id"},
            {"hostId": "legacy-7", "sshAlias": "old-box"}
        ],
        KEY_PROJECTS: [
            {"id": "p1", "hostId": RC, "label": "site", "remotePath": "/Users/me/site"},
            {"id": "p2", "hostId": "remote-ssh-discovered:devbox", "remotePath": "/srv/app"}
        ],
        KEY_AUTO_CONNECT: {RC: true, "remote-ssh-discovered:devbox": true}
    }))
    .unwrap()
}

#[test]
fn global_state_holds_ssh_connections_only() {
    let global = parse_global_state(&global_state()).unwrap();
    let ids: Vec<&str> = global.iter().map(|c| c.host_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "remote-ssh-discovered:devbox",
            "remote-ssh-codex-managed:5a0c",
            "legacy-7"
        ]
    );
    let hosts = merge_hosts(&global, &[], &[], &[], &[]);
    assert!(
        hosts
            .iter()
            .all(|h| h.kind == HostKind::Ssh && h.is_managed())
    );
    assert_eq!(hosts[2].ssh.alias.as_deref(), Some("old-box"));
    // Legacy ids are SSH; an id that names another kind is not (defensive).
    assert_eq!(saved_connection_kind("legacy-7"), HostKind::Ssh);
    assert_eq!(saved_connection_kind(RC), HostKind::RemoteControl);
}

#[test]
fn mixed_kinds_from_the_renderer_keep_their_kind_and_never_get_an_endpoint() {
    let global = parse_global_state(&global_state()).unwrap();
    let app = vec![
        // The SSH connection the global state has, with the app's display name.
        from_app("remote-ssh-codex-managed:5a0c", "GPU box", HostKind::Ssh),
        // A device with the app's display name.
        from_app(RC, "Studio Mac", HostKind::RemoteControl),
        // A device seen only on a sidebar row: 0.1.0 labelled it with its id.
        from_app(
            "remote-control:env_other",
            "remote-control:env_other",
            HostKind::Ssh,
        ),
        from_app("remote-wsl:Ubuntu", "WSL: Ubuntu", HostKind::Wsl),
        from_app("durable", "", HostKind::Cloud),
        from_app("chatgpt:acct:user", "", HostKind::Unknown),
        from_app("mystery:1", "", HostKind::Unknown),
        // An SSH alias the app discovered, known only from its id.
        from_app("remote-ssh-discovered:lab%20box", "", HostKind::Ssh),
    ];
    // A live link to the address of the device's host name must not merge into it.
    let live = [link(
        "studio.local",
        &["ssh", "studio.local", "codex", "app-server"],
    )];
    let hosts = merge_hosts(&global, &app, &[], &live, &[]);
    let rows: Vec<(String, HostKind, String, bool)> = hosts
        .iter()
        .map(|h| {
            (
                h.target_id.to_string(),
                h.kind,
                h.label.clone(),
                h.ssh != SshEndpoint::default(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "ssh:remote-ssh-discovered:devbox".into(),
                HostKind::Ssh,
                "devbox".into(),
                true
            ),
            (
                "ssh:remote-ssh-codex-managed:5a0c".into(),
                HostKind::Ssh,
                "gpu-01".into(),
                true
            ),
            ("ssh:legacy-7".into(), HostKind::Ssh, "old-box".into(), true),
            (
                format!("ssh:{RC}"),
                HostKind::RemoteControl,
                "Studio Mac".into(),
                false
            ),
            (
                "ssh:remote-control:env_other".into(),
                HostKind::RemoteControl,
                String::new(),
                false
            ),
            (
                "ssh:remote-wsl:Ubuntu".into(),
                HostKind::Wsl,
                "WSL: Ubuntu".into(),
                false
            ),
            ("ssh:durable".into(), HostKind::Cloud, String::new(), false),
            (
                "ssh:chatgpt:acct:user".into(),
                HostKind::Cloud,
                String::new(),
                false
            ),
            (
                "ssh:mystery:1".into(),
                HostKind::Unknown,
                String::new(),
                false
            ),
            (
                "ssh:remote-ssh-discovered:lab%20box".into(),
                HostKind::Ssh,
                "lab box".into(),
                true
            ),
            (
                "ssh:studio.local".into(),
                HostKind::Ssh,
                "studio.local".into(),
                true
            ),
        ]
    );
    let managed: Vec<&str> = hosts
        .iter()
        .filter(|h| h.is_managed())
        .map(|h| h.label.as_str())
        .collect();
    // User bug 45: a WSL distribution is managed (through wsl.exe), without an
    // SSH endpoint, and knows its distribution from the app's id.
    assert_eq!(
        managed,
        vec![
            "devbox",
            "gpu-01",
            "old-box",
            "WSL: Ubuntu",
            "lab box",
            "studio.local"
        ]
    );
    let wsl = hosts.iter().find(|h| h.kind == HostKind::Wsl).unwrap();
    assert_eq!(wsl.wsl_distro().as_deref(), Some("Ubuntu"));
    let listed_read_only: Vec<&str> = hosts
        .iter()
        .filter(|h| h.is_listed() && !h.is_managed())
        .map(|h| h.host_id.as_deref().unwrap())
        .collect();
    assert_eq!(listed_read_only, vec![RC, "remote-control:env_other"]);
    // The device on the projects / auto-connect keys only came from the renderer.
    assert_eq!(hosts[3].sources, vec![HostSource::Cdp]);
    assert_eq!(
        hosts[9].ssh.alias.as_deref(),
        Some("lab box"),
        "the alias of a discovered id"
    );
}

#[test]
fn a_renderer_host_is_normalised() {
    // A raw id is no label, and an SSH endpoint never sticks to another kind.
    let mut rc = from_app(RC, RC, HostKind::RemoteControl);
    rc.ssh.host = Some("studio.local".into());
    let rc = host_from_app(&rc);
    assert_eq!(
        (rc.label.as_str(), rc.ssh.clone()),
        ("", SshEndpoint::default())
    );
    let mut wsl = from_app("remote-wsl:Debian", "remote-wsl:Debian", HostKind::Ssh);
    wsl.ssh.alias = Some("Debian".into());
    let wsl = host_from_app(&wsl);
    assert_eq!(wsl.kind, HostKind::Wsl);
    assert_eq!(wsl.ssh, SshEndpoint::default());
    let ssh = host_from_app(&from_app(
        "remote-ssh-discovered:devbox",
        "remote-ssh-discovered:devbox",
        HostKind::Ssh,
    ));
    assert_eq!(
        (ssh.label.as_str(), ssh.ssh.alias.as_deref()),
        ("devbox", Some("devbox"))
    );
    assert_eq!(
        discovered_alias("remote-ssh-discovered:a%2Fb").as_deref(),
        Some("a/b")
    );
}

/// A transport that records every script it is asked to run.
#[derive(Debug, Default)]
struct Recording {
    runs: Mutex<Vec<String>>,
}

#[async_trait]
impl RemoteShell for Recording {
    async fn run(
        &self,
        host: &RemoteHost,
        _script: &str,
        _timeout: Duration,
    ) -> superba_core::Result<ScriptOutput> {
        self.runs.lock().unwrap().push(host.target_id.to_string());
        Err(Error::other("no ssh in this test"))
    }

    async fn run_with_payload(
        &self,
        host: &RemoteHost,
        _script: &str,
        _payload: &std::path::Path,
        _timeout: Duration,
    ) -> superba_core::Result<ScriptOutput> {
        self.runs.lock().unwrap().push(host.target_id.to_string());
        Err(Error::other("no ssh in this test"))
    }
}

fn unsupported(r: superba_core::Result<impl std::fmt::Debug>, what: &str) {
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
async fn no_ssh_operation_reaches_the_transport_for_a_remote_control_device() {
    let tmp = tempfile::tempdir().unwrap();
    let shell = Arc::new(Recording::default());
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_shell(shell.clone());
    for kind in [HostKind::RemoteControl, HostKind::Cloud, HostKind::Unknown] {
        let mut h = from_app(RC, "Studio Mac", kind);
        // Even an endpoint (never set by discovery) is not used.
        h.ssh.alias = Some("studio".into());
        assert!(require_ssh(&h).is_err());
        unsupported(hosts.probe(&h).await, "probe");
        unsupported(
            hosts.plan_apply(&h, &original_profile()).await,
            "plan_apply",
        );
        unsupported(
            hosts
                .plan_apply_scoped(&h, &original_profile(), RemoteApplyScope::Settings)
                .await,
            "plan_apply_scoped",
        );
        unsupported(hosts.scan_sessions(&h).await, "scan_sessions");
        unsupported(hosts.turn_activity(&h).await, "turn_activity");
        unsupported(hosts.rollback_codex(&h).await, "rollback_codex");
        let package = tmp.path().join("codex.tar.gz");
        std::fs::write(&package, b"not a package").unwrap();
        unsupported(
            hosts
                .install_codex(&h, &package, &CodexVersion::parse("0.162.0").unwrap())
                .await,
            "install_codex",
        );
        let report = RemoteApplyReport {
            target_id: h.target_id.clone(),
            applied_at: Timestamp(0),
            files: Vec::new(),
            env: None,
        };
        unsupported(
            hosts
                .verify(
                    &h,
                    &RemoteVerifyRequest {
                        report: report.clone(),
                        previous_app_server_pids: Vec::new(),
                        expected_authapi: None,
                        timeout_ms: 10,
                    },
                )
                .await,
            "verify",
        );
    }
    assert_eq!(shell.runs.lock().unwrap().clone(), Vec::<String>::new());
    // An SSH host does reach the transport.
    let ssh = common::host("devbox", Some("remote-ssh-discovered:devbox"));
    assert!(hosts.probe(&ssh).await.is_err());
    assert_eq!(
        shell.runs.lock().unwrap().clone(),
        vec!["ssh:remote-ssh-discovered:devbox".to_owned()]
    );
    // The local discovery has nothing to classify.
    let found = hosts
        .discover(&CodexHome::new(tmp.path().join("none")), &[], &[])
        .await
        .unwrap();
    assert!(found.is_empty());
}
