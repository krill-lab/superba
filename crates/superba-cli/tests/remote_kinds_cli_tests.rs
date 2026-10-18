//! `spb remote list` with hosts of several kinds (user bug 36): a Kind column,
//! remote-control devices and WSL distributions named by the app's display name
//! or their kind, never by the raw app host id; the JSON carries the kind.

mod common;

use common::*;
use superba_core::*;

const DEVICE: &str = "remote-control:env_e_6a1b8b37df48832d9f0";

fn device(id: &str, label: &str, kind: HostKind) -> HostView {
    HostView {
        host: RemoteHost {
            target_id: TargetId::remote(id),
            label: label.into(),
            host_id: Some(id.into()),
            kind,
            ssh: SshEndpoint::default(),
            sources: vec![HostSource::Cdp],
            connected: None,
            wsl: None,
        },
        state: None,
        probe_error: None,
        settings: HostSettings::default(),
        pending_sync: None,
        hot_restart: false,
    }
}

fn service() -> std::sync::Arc<FakeService> {
    let s = FakeService::new();
    s.v3.lock().unwrap().hosts.extend([
        device(DEVICE, "Studio Mac", HostKind::RemoteControl),
        device("remote-control:env_other", "", HostKind::RemoteControl),
        device("remote-wsl:Ubuntu", "WSL: Ubuntu", HostKind::Wsl),
    ]);
    s
}

/// The table row whose first column is `target`.
fn row<'a>(stdout: &'a str, target: &str) -> Vec<&'a str> {
    stdout
        .lines()
        .find(|l| l.starts_with(&format!("{target} |")))
        .unwrap_or_else(|| panic!("no row {target}:\n{stdout}"))
        .split(" | ")
        .collect()
}

#[tokio::test]
async fn remote_list_has_a_kind_column_and_never_names_a_host_by_its_id() {
    for (locale, header, ssh, device_kind, wsl) in [
        (
            "en",
            "Target | Name | Kind | Host ID",
            "SSH host",
            "Remote-control device",
            "WSL distribution",
        ),
        (
            "zh-CN",
            "目标 | 名称 | 类型 | 主机标识",
            "SSH 主机",
            "远程控制设备",
            "WSL 发行版",
        ),
    ] {
        let (code, stdout, stderr) =
            invoke(service(), &["--locale", locale, "remote", "list"]).await;
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.contains(header), "{stdout}");
        assert_eq!(&row(&stdout, "ssh:dev")[1..3], ["dev", ssh]);
        assert_eq!(
            &row(&stdout, &format!("ssh:{DEVICE}"))[1..4],
            ["Studio Mac", device_kind, DEVICE]
        );
        // No display name: the kind is the name; the id stays in its column.
        assert_eq!(
            &row(&stdout, "ssh:remote-control:env_other")[1..3],
            [device_kind, device_kind]
        );
        assert_eq!(
            &row(&stdout, "ssh:remote-wsl:Ubuntu")[1..3],
            ["WSL: Ubuntu", wsl]
        );
    }
    let (code, stdout, _) = invoke(service(), &["remote", "list", "--json"]).await;
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let kinds: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["host"]["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["ssh", "remote_control", "remote_control", "wsl"]);
}
