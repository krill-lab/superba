//! Host kinds (user bug 36): the desktop app's host ids classified as the app
//! does, only SSH hosts managed, and titles that never show a raw host id.

#![allow(missing_docs)]

use pretty_assertions::assert_eq;
use superba_core::i18n::{Catalog, Locale};
use superba_core::{
    HostKind, HostSource, RemoteHost, SshEndpoint, TargetId, discovered_ssh_alias,
    looks_like_host_id, target_title,
};

fn host(id: Option<&str>, label: &str, kind: HostKind) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(id.unwrap_or(label)),
        label: label.into(),
        host_id: id.map(str::to_owned),
        kind,
        ssh: SshEndpoint::default(),
        sources: vec![HostSource::Cdp],
        connected: None,
        wsl: None,
    }
}

#[test]
fn host_ids_are_classified_like_the_app_does() {
    for (id, kind) in [
        ("remote-ssh-discovered:devbox", HostKind::Ssh),
        (
            "remote-ssh-codex-managed:0d4c4c0e-3a7e-4c39-9d43-0b6c8c1c9d2e",
            HostKind::Ssh,
        ),
        (
            "remote-control:env_e_6a1b8b37df48832d9f0",
            HostKind::RemoteControl,
        ),
        ("remote-wsl:Ubuntu-24.04", HostKind::Wsl),
        ("durable", HostKind::Cloud),
        ("chatgpt:acct-1:user-2", HostKind::Cloud),
        ("local", HostKind::Unknown),
        ("environment-git:hosted:x", HostKind::Unknown),
        ("something-new:abc", HostKind::Unknown),
        (" remote-control:env ", HostKind::RemoteControl),
    ] {
        assert_eq!(HostKind::of_host_id(id), kind, "{id}");
    }
}

#[test]
fn ssh_and_wsl_are_managed_and_cloud_or_unknown_hosts_are_not_listed() {
    let rows: Vec<(HostKind, bool, bool, bool, &str)> = [
        HostKind::Ssh,
        HostKind::RemoteControl,
        HostKind::Wsl,
        HostKind::Cloud,
        HostKind::Unknown,
    ]
    .into_iter()
    .map(|k| (k, k.is_managed(), k.is_ssh(), k.is_listed(), k.as_str()))
    .collect();
    assert_eq!(
        rows,
        vec![
            (HostKind::Ssh, true, true, true, "ssh"),
            (
                HostKind::RemoteControl,
                false,
                false,
                true,
                "remote_control"
            ),
            // User bug 45: WSL distributions are managed (through wsl.exe).
            (HostKind::Wsl, true, false, true, "wsl"),
            (HostKind::Cloud, false, false, false, "cloud"),
            (HostKind::Unknown, false, false, false, "unknown"),
        ]
    );
    assert_eq!(HostKind::default(), HostKind::Ssh);
}

#[test]
fn a_raw_host_id_is_never_a_title() {
    let zh = Catalog::new(Locale::ZhCn);
    let en = Catalog::new(Locale::En);
    let id = "remote-control:env_e_6a1b8b37df48832d9f0";
    // 0.1.0 used the id as the label.
    for label in [id, "", "  ", "remote-ssh-codex-managed:1"] {
        let h = host(Some(id), label, HostKind::RemoteControl);
        assert_eq!(h.display_name(), None, "{label:?}");
        assert_eq!(en.text(&h.title()), "Remote-control device");
        assert_eq!(zh.text(&h.title()), "远程控制设备");
    }
    let named = host(Some(id), "Studio Mac", HostKind::RemoteControl);
    assert_eq!(named.display_name(), Some("Studio Mac"));
    assert_eq!(zh.text(&named.title()), "Studio Mac");
    // Braces in a name are not placeholders.
    let braces = host(Some("remote-wsl:x"), "WSL: {n}", HostKind::Wsl);
    assert_eq!(en.text(&braces.title()), "WSL: {n}");
    // SSH names stay as they are.
    let ssh = host(
        Some("remote-ssh-discovered:devbox"),
        "devbox",
        HostKind::Ssh,
    );
    assert_eq!(ssh.display_name(), Some("devbox"));
    assert!(ssh.is_managed() && ssh.is_listed());
    assert!(!named.is_managed() && named.is_listed());
}

#[test]
fn every_kind_has_a_name_in_both_catalogs() {
    for locale in [Locale::En, Locale::ZhCn] {
        let c = Catalog::new(locale);
        for kind in [
            HostKind::Ssh,
            HostKind::RemoteControl,
            HostKind::Wsl,
            HostKind::Cloud,
            HostKind::Unknown,
        ] {
            let t = kind.text();
            assert!(c.get(&t.key).is_some(), "{locale:?} {}", t.key);
        }
    }
    let zh = Catalog::new(Locale::ZhCn);
    assert_eq!(zh.text(&HostKind::Wsl.text()), "WSL 发行版");
}

#[test]
fn discovered_aliases_decode_like_decode_uri_component() {
    assert_eq!(
        discovered_ssh_alias("remote-ssh-discovered:devbox").as_deref(),
        Some("devbox")
    );
    assert_eq!(
        discovered_ssh_alias("remote-ssh-discovered:my%20box%2Bgpu").as_deref(),
        Some("my box+gpu")
    );
    assert_eq!(
        discovered_ssh_alias("remote-ssh-discovered:%E4%B8%BB%E6%9C%BA").as_deref(),
        Some("主机")
    );
    for bad in [
        "remote-ssh-discovered:",
        "remote-ssh-discovered:%2",
        "remote-ssh-discovered:%zz",
        "remote-ssh-discovered:%FF",
        "remote-ssh-codex-managed:abc",
        "devbox",
    ] {
        assert_eq!(discovered_ssh_alias(bad), None, "{bad}");
    }
}

#[test]
fn target_titles_name_the_alias_or_the_kind_never_the_raw_id() {
    let en = Catalog::new(Locale::En);
    let title = |id: &str| en.text(&target_title(&TargetId::new(id)));
    assert_eq!(title("ssh:devbox"), "devbox");
    assert_eq!(title("ssh:user@10.0.0.9"), "user@10.0.0.9");
    assert_eq!(title("ssh:remote-ssh-discovered:gpu-01"), "gpu-01");
    assert_eq!(title("ssh:remote-ssh-codex-managed:42"), "SSH host");
    assert_eq!(
        title("ssh:remote-control:env_e_6a1b8b37df48832d9f0"),
        "Remote-control device"
    );
    assert_eq!(title("ssh:durable"), "ChatGPT cloud");
    assert!(looks_like_host_id("remote-wsl:Ubuntu"));
    assert!(looks_like_host_id("local"));
    assert!(!looks_like_host_id("Studio Mac"));
}

#[test]
fn kind_serializes_snake_case_and_defaults_to_ssh_when_absent() {
    let h = host(
        Some("remote-control:env"),
        "Studio Mac",
        HostKind::RemoteControl,
    );
    let v = serde_json::to_value(&h).unwrap();
    assert_eq!(v["kind"], "remote_control");
    let mut old = v.clone();
    old.as_object_mut().unwrap().remove("kind");
    let back: RemoteHost = serde_json::from_value(old).unwrap();
    assert_eq!(back.kind, HostKind::Ssh);
    let same: RemoteHost = serde_json::from_value(v).unwrap();
    assert_eq!(same, h);
}
