//! Remote hosts read from the renderer (user bug 36): the app's connection
//! shared objects give each host its kind and display name; host ids seen only on
//! sidebar rows get the kind their id implies; a raw id is never a label.

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_appctl::renderer::{DomHooks, parse_probe, probe_js, remote_host_ids, remote_hosts};
use superba_core::{HostKind, HostSource, SshEndpoint};

const RC: &str = "remote-control:env_e_6a1b8b37df48832d9f0";

#[test]
fn the_probe_reads_the_connection_shared_objects_read_only() {
    let js = probe_js(&DomHooks::default());
    assert!(js.contains("getSharedObjectSnapshotValue"), "{js}");
    for key in [
        "remote_ssh_connections",
        "remote_wsl_connections",
        "remote_control_connections",
    ] {
        assert!(js.contains(key), "{key}");
    }
    // Nothing is sent to the app (no shared-object-set, no IPC).
    assert!(!js.contains("sendMessageFromView"));
    // Old hook sets without the field get the default keys.
    let mut v = serde_json::to_value(DomHooks::default()).unwrap();
    v.as_object_mut().unwrap().remove("connection_objects");
    let hooks: DomHooks = serde_json::from_value(v).unwrap();
    assert_eq!(hooks, DomHooks::default());
}

#[test]
fn connections_give_kinds_names_and_ssh_endpoints() {
    let p = parse_probe(&json!({
        "url": "app://-/index.html", "composer": true,
        "rows": [
            {"id": "a:1", "host": RC, "title": "on the device"},
            {"id": "b:2", "host": "remote-wsl:Ubuntu"},
            {"id": "c:3", "host": "durable"},
            {"id": "d:4", "host": "remote-control:env_unnamed"},
            {"id": "e:5", "host": "remote-ssh-discovered:devbox"},
            {"id": "f:6", "host": "local"}
        ],
        "connections": [
            {"hostId": "remote-ssh-discovered:devbox", "displayName": "devbox",
             "source": "discovered", "sshAlias": "devbox", "sshHost": "10.0.0.5",
             "sshPort": null, "identity": null},
            {"hostId": "remote-ssh-codex-managed:5a0c", "displayName": "GPU",
             "source": "codex-managed", "sshAlias": null, "sshHost": "me@203.0.113.7",
             "sshPort": 2222, "identity": "/k/id"},
            {"hostId": "remote-wsl:Ubuntu", "displayName": "WSL: Ubuntu", "source": "wsl"},
            {"hostId": RC, "displayName": "Studio Mac", "source": "remote-control"},
            {"hostId": "remote-control:env_raw", "displayName": "remote-control:env_raw",
             "source": "remote-control"}
        ]
    }))
    .unwrap();
    let hosts = remote_hosts(&p);
    let rows: Vec<(&str, HostKind, &str, &SshEndpoint)> = hosts
        .iter()
        .map(|h| {
            (
                h.host_id.as_deref().unwrap(),
                h.kind,
                h.label.as_str(),
                &h.ssh,
            )
        })
        .collect();
    let none = SshEndpoint::default();
    let devbox = SshEndpoint {
        alias: Some("devbox".into()),
        ..SshEndpoint::default()
    };
    let gpu = SshEndpoint {
        host: Some("me@203.0.113.7".into()),
        port: Some(2222),
        identity: Some("/k/id".into()),
        ..SshEndpoint::default()
    };
    assert_eq!(
        rows,
        vec![
            (
                "remote-ssh-discovered:devbox",
                HostKind::Ssh,
                "devbox",
                &devbox
            ),
            ("remote-ssh-codex-managed:5a0c", HostKind::Ssh, "GPU", &gpu),
            ("remote-wsl:Ubuntu", HostKind::Wsl, "WSL: Ubuntu", &none),
            (RC, HostKind::RemoteControl, "Studio Mac", &none),
            ("remote-control:env_raw", HostKind::RemoteControl, "", &none),
            ("durable", HostKind::Cloud, "", &none),
            (
                "remote-control:env_unnamed",
                HostKind::RemoteControl,
                "",
                &none
            ),
        ]
    );
    assert!(hosts.iter().all(|h| h.sources == vec![HostSource::Cdp]));
    assert!(
        hosts
            .iter()
            .all(|h| h.target_id.as_str().starts_with("ssh:"))
    );
}

#[test]
fn sidebar_ids_alone_never_become_labels() {
    // What 0.1.0 turned into an "SSH host" called by its raw id.
    let p = parse_probe(&json!({
        "url": "app://-/index.html",
        "rows": [{"id": "x:1", "host": RC}, {"id": "y:2", "host": "gpu-01"}]
    }))
    .unwrap();
    assert_eq!(
        remote_host_ids(&p),
        vec!["gpu-01".to_owned(), RC.to_owned()]
    );
    let hosts = remote_hosts(&p);
    let kinds: Vec<(HostKind, &str)> = hosts.iter().map(|h| (h.kind, h.label.as_str())).collect();
    assert_eq!(
        kinds,
        vec![(HostKind::Unknown, ""), (HostKind::RemoteControl, "")]
    );
}

#[test]
fn malformed_connections_never_break_the_probe() {
    let p = parse_probe(&json!({
        "url": "app://-/index.html", "composer": true, "conversation": "c1",
        "connections": [
            null, 3, "x", {}, {"hostId": 7}, {"hostId": "  "},
            {"hostId": "remote-ssh-codex-managed:1", "sshPort": 70000},
            {"hostId": "remote-control:ok", "displayName": ["not", "a", "name"]},
            {"hostId": RC, "displayName": "Studio Mac", "source": "remote-control", "online": true}
        ]
    }))
    .expect("the probe still parses");
    assert_eq!(p.conversation.as_deref(), Some("c1"));
    let ids: Vec<&str> = p.connections.iter().map(|c| c.host_id.as_str()).collect();
    assert_eq!(ids, vec![RC]);
    // No connections at all (an older app): sidebar ids only.
    let p = parse_probe(&json!({"url": "u", "connections": null})).unwrap();
    assert!(p.connections.is_empty());
}
