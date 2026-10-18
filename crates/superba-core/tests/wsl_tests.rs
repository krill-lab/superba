//! WSL facts (user bug 45): `wsl.exe` output, host ids, paths, networking.

use std::net::Ipv4Addr;

use pretty_assertions::assert_eq;
use superba_core::wsl::{
    RouterPath, WslNetworking, decode_output, default_gateway, distro_of_host_id,
    distro_of_router_source, distro_of_target, drvfs_path, encode_uri_component, host_id,
    is_utility_distro, parse_list_quiet, parse_list_verbose, parse_wslconfig_networking,
    router_path, router_source, target_id, unc_path,
};
use superba_core::{
    Catalog, HostKind, HostSettings, HostSource, Locale, Origin, RemoteHost, SshEndpoint, TargetId,
    WslDistro, target_title,
};

fn utf16(text: &str, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend([0xFF, 0xFE]);
    }
    for u in text.encode_utf16() {
        out.extend(u.to_le_bytes());
    }
    out
}

const LIST: &str = "  NAME                   STATE           VERSION\r\n\
* Ubuntu-24.04           Running         2\r\n\
  Debian                 Stopped         1\r\n\
  docker-desktop         Stopped         2\r\n\
  kali-linux             Stopped         2\r\n";

#[test]
fn wsl_exe_output_is_decoded_like_the_app_does() {
    // UTF-16LE with and without a BOM, UTF-8 with a BOM, plain UTF-8 (WSL_UTF8=1).
    let plain = "  NAME   STATE   VERSION\n* Ubuntu   Running   2";
    for bytes in [
        utf16(LIST, true),
        utf16(LIST, false),
        [vec![0xEF, 0xBB, 0xBF], LIST.as_bytes().to_vec()].concat(),
        LIST.as_bytes().to_vec(),
    ] {
        let text = decode_output(&bytes);
        assert!(text.starts_with("NAME"), "{text:?}");
        assert!(!text.contains('\r') && !text.contains('\0'));
    }
    assert_eq!(decode_output(plain.as_bytes()), plain.trim());
    // A localized message (Chinese) survives the round trip.
    let msg = "适用于 Linux 的 Windows 子系统没有已安装的分发。";
    assert_eq!(decode_output(&utf16(msg, false)), msg);
    assert_eq!(decode_output(b""), "");
}

#[test]
fn the_verbose_list_gives_name_version_state_and_default_first() {
    let distros = parse_list_verbose(&decode_output(&utf16(LIST, true)));
    let rows: Vec<(&str, Option<u8>, bool, Option<bool>)> = distros
        .iter()
        .map(|d| (d.name.as_str(), d.version, d.default, d.running()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Ubuntu-24.04", Some(2), true, Some(true)),
            ("Debian", Some(1), false, Some(false)),
            ("kali-linux", Some(2), false, Some(false)),
        ]
    );
    // Localized headers and states, tabs, a default that is not first.
    let de = "NAME\tSTATUS\tVERSION\n  Alpine\tBeendet\t1\n* Arch\tWird ausgeführt\t2\n";
    let d = parse_list_verbose(de);
    assert_eq!(d[0].name, "Arch");
    assert!(d[0].default);
    assert_eq!(d[0].running(), None);
    assert_eq!(d[1].version, Some(1));
    // No distributions: a message, never a name.
    let none = "Windows Subsystem for Linux has no installed distributions.\n\
                You can resolve this by installing a distribution with the instructions below:";
    assert!(parse_list_verbose(none).is_empty());
    assert_eq!(
        parse_list_quiet("Ubuntu\r\ndocker-desktop-data\r\nDebian\r\nUbuntu\r\n"),
        vec!["Ubuntu", "Debian"]
    );
    assert!(is_utility_distro("Docker-Desktop"));
}

#[test]
fn host_ids_match_the_apps_encode_uri_component() {
    assert_eq!(host_id("Ubuntu-24.04"), "remote-wsl:Ubuntu-24.04");
    assert_eq!(encode_uri_component("a b/ü!~*'()"), "a%20b%2F%C3%BC!~*'()");
    assert_eq!(
        distro_of_host_id("remote-wsl:Ubuntu-24.04").as_deref(),
        Some("Ubuntu-24.04")
    );
    assert_eq!(
        distro_of_host_id("remote-wsl:a%2Fb"),
        None,
        "a separator is no name"
    );
    assert_eq!(distro_of_host_id("remote-ssh-discovered:x"), None);
    let t = target_id("Debian");
    assert_eq!(t, TargetId::remote("remote-wsl:Debian"));
    assert_eq!(distro_of_target(&t).as_deref(), Some("Debian"));
    assert_eq!(HostKind::of_target(&t), HostKind::Wsl);
    assert_eq!(
        HostKind::of_target(&TargetId::remote("devbox")),
        HostKind::Ssh
    );
    assert_eq!(HostKind::of_target(&TargetId::local()), HostKind::Unknown);
    let en = Catalog::new(Locale::En);
    let zh = Catalog::new(Locale::ZhCn);
    assert_eq!(en.text(&target_title(&t)), "WSL: Debian");
    assert_eq!(zh.text(&target_title(&t)), "WSL：Debian");
    assert_eq!(router_source("Debian"), "wsl:Debian");
    assert_eq!(distro_of_router_source("wsl:Debian"), Some("Debian"));
    assert_eq!(distro_of_router_source("local"), None);
}

#[test]
fn a_wsl_host_is_titled_by_its_distribution_and_reached_by_name() {
    let host = RemoteHost {
        target_id: target_id("Ubuntu"),
        label: String::new(),
        host_id: Some(host_id("Ubuntu")),
        kind: HostKind::Wsl,
        ssh: SshEndpoint::default(),
        sources: vec![HostSource::Wsl],
        connected: None,
        wsl: None,
    };
    assert!(host.is_app_known() && host.is_managed() && !host.kind.is_ssh());
    assert_eq!(host.wsl_distro().as_deref(), Some("Ubuntu"));
    assert_eq!(Catalog::new(Locale::En).text(&host.title()), "WSL: Ubuntu");
    let named = RemoteHost {
        wsl: Some(WslDistro::named("Other")),
        ..host
    };
    assert_eq!(named.wsl_distro().as_deref(), Some("Other"));
}

#[test]
fn paths_map_between_windows_and_the_distribution() {
    assert_eq!(
        unc_path("Ubuntu", "/home/u/.codex").unwrap(),
        std::path::PathBuf::from(r"\\wsl.localhost\Ubuntu\home\u\.codex")
    );
    assert_eq!(unc_path("Ubuntu", "relative"), None);
    assert_eq!(
        drvfs_path(r"C:\Users\u\.codex\bin\wsl\codex").as_deref(),
        Some("/mnt/c/Users/u/.codex/bin/wsl/codex")
    );
    assert_eq!(drvfs_path(r"\\?\D:\x\").as_deref(), Some("/mnt/d/x"));
    assert_eq!(drvfs_path("D:").as_deref(), Some("/mnt/d"));
    assert_eq!(drvfs_path(r"\\server\share"), None);
}

#[test]
fn networking_mode_and_router_path() {
    assert_eq!(parse_wslconfig_networking(""), WslNetworking::Nat);
    assert_eq!(
        parse_wslconfig_networking("[wsl2]\r\nmemory=4GB\r\nnetworkingMode=mirrored # mine\r\n"),
        WslNetworking::Mirrored
    );
    assert_eq!(
        parse_wslconfig_networking(
            "[experimental]\nnetworkingMode=mirrored\n[wsl2]\nnetworkingmode = NAT\n"
        ),
        WslNetworking::Nat,
        "[wsl2] wins over the preview section"
    );
    assert_eq!(
        parse_wslconfig_networking("[experimental]\nnetworkingMode=mirrored\n"),
        WslNetworking::Mirrored
    );
    assert_eq!(
        parse_wslconfig_networking("[wsl2]\nnetworkingMode=bridged\n"),
        WslNetworking::Nat,
        "unknown values fall back to NAT like WSL"
    );
    assert_eq!(
        router_path(Some(1), WslNetworking::Nat),
        RouterPath::Loopback
    );
    assert_eq!(
        router_path(Some(2), WslNetworking::Mirrored),
        RouterPath::Loopback
    );
    assert_eq!(
        router_path(Some(2), WslNetworking::Nat),
        RouterPath::HostAddress
    );
    assert_eq!(
        router_path(None, WslNetworking::VirtioProxy),
        RouterPath::HostAddress
    );
    assert_eq!(
        router_path(Some(2), WslNetworking::None),
        RouterPath::Unreachable
    );
}

#[test]
fn the_default_gateway_comes_from_proc_net_route() {
    let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0150A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                 eth0\t0050A8C0\t00000000\t0001\t0\t0\t0\t00F0FFFF\t0\t0\t0\n";
    assert_eq!(default_gateway(route), Some(Ipv4Addr::new(192, 168, 80, 1)));
    let nat = "Iface Destination Gateway Flags\neth0 00000000 0150B5AC 0003\n";
    assert_eq!(default_gateway(nat), Some(Ipv4Addr::new(172, 181, 80, 1)));
    assert_eq!(default_gateway("Iface Destination Gateway Flags\n"), None);
    let no_gw_flag = "h\neth0 00000000 0150A8C0 0001\n";
    assert_eq!(default_gateway(no_gw_flag), None);
}

#[test]
fn the_router_origin_for_wsl_is_plain_http_on_a_private_address_only() {
    let o = Origin::local_router_for_wsl(Ipv4Addr::new(172, 25, 80, 1), 50947).unwrap();
    assert_eq!(o.as_str(), "http://172.25.80.1:50947");
    assert_eq!(
        o.join("/backend-api/codex").as_str(),
        "http://172.25.80.1:50947/backend-api/codex"
    );
    assert!(Origin::local_router_for_wsl(Ipv4Addr::new(8, 8, 8, 8), 1).is_err());
    // User input never gets plain http to a private address.
    assert!(Origin::parse("http://172.25.80.1:50947").is_err());
}

#[test]
fn a_wsl_distribution_follows_unless_the_user_turned_it_off() {
    let none = HostSettings::default();
    assert!(none.follows(HostKind::Wsl));
    assert!(!none.follows(HostKind::Ssh));
    let mut off = HostSettings::default();
    off.choose_follow(false);
    assert!(!off.follows(HostKind::Wsl));
    let mut on = HostSettings::default();
    on.choose_follow(true);
    assert!(on.follows(HostKind::Ssh));
    // A setting saved before the choice was recorded: on stays on.
    let legacy = HostSettings {
        follow_switches: true,
        ..HostSettings::default()
    };
    assert!(legacy.follows(HostKind::Ssh) && legacy.follows(HostKind::Wsl));
    // The choice survives serialization.
    let json = serde_json::to_string(&off).unwrap();
    let back: HostSettings = serde_json::from_str(&json).unwrap();
    assert!(!back.follows(HostKind::Wsl));
}
