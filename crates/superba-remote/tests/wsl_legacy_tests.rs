//! The desktop app's "Run Codex in WSL" mode (user bug 54 (b)) inside a
//! distribution: that app-server runs the app's own Linux Codex from the
//! Windows Codex home (`/mnt/c/Users/<u>/.codex/bin/wsl/codex`) and reads that
//! home, so a probe of the distribution does not count it as the
//! distribution's app-server (the app's WSL connection, which reads the
//! distribution's own Codex home, still is).
#![cfg(unix)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::engine;
use superba_core::wsl::{host_id, target_id};
use superba_core::{
    DataDir, HostKind, HostSource, RemoteHost, RemoteHosts, SshEndpoint, WslDistro,
};
use superba_remote::wsl::FixedWsl;
use superba_remote::{LocalShell, SshRemoteHosts};

fn distro(name: &str) -> RemoteHost {
    RemoteHost {
        target_id: target_id(name),
        label: String::new(),
        host_id: Some(host_id(name)),
        kind: HostKind::Wsl,
        ssh: SshEndpoint::default(),
        sources: vec![HostSource::Wsl],
        connected: None,
        wsl: Some(WslDistro {
            version: Some(2),
            ..WslDistro::named(name)
        }),
    }
}

/// A process whose command line is `command` (bash's `exec -a`).
fn fake_process(command: &str) -> std::process::Child {
    std::process::Command::new("/usr/bin/bash")
        .args(["-c", &format!("exec -a '{command}' sleep 30")])
        .spawn()
        .unwrap()
}

#[tokio::test]
async fn the_run_codex_in_wsl_app_server_is_not_the_distributions_own() {
    if !Path::new("/usr/bin/bash").is_file() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_shell(Arc::new(LocalShell::new(&home).with_login_shell("/bin/sh")))
        .with_wsl_lister(Arc::new(FixedWsl::default()));
    // As the app runs it (app 26.1013 in the test VM): a hash directory of the
    // copied binary, under a bash wrapper that waits for it.
    let mut legacy = fake_process(
        "/mnt/c/Users/u/.codex/bin/wsl/f19bbb73214bc62a/codex -c features.code_mode_host=true app-server --analytics-default-enabled",
    );
    let mut wrapper = fake_process(
        "/usr/bin/bash -lc set -m; /bin/bash -c PATH=x CODEX_SQLITE_HOME=\"$HOME/.codex/sqlite\" exec codex -c features.code_mode_host=true app-server",
    );
    let mut connection = fake_process(
        "codex -c features.code_mode_host=true app-server --analytics-default-enabled",
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let state = hosts.probe(&distro("Ubuntu")).await;
    for child in [&mut legacy, &mut wrapper, &mut connection] {
        let _ = child.kill();
        let _ = child.wait();
    }
    let pids = state.unwrap().app_server_pids;
    assert!(pids.contains(&connection.id()), "{pids:?}");
    assert!(!pids.contains(&legacy.id()), "{pids:?}");
    assert!(!pids.contains(&wrapper.id()), "{pids:?}");
}
