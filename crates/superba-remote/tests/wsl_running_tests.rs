//! Which WSL distributions run (user bug 54 (a)): discovery never starts a
//! process; `wsl_running` asks Windows (`wsl.exe --list --running`, which starts
//! no distribution) and later discoveries carry the answer; a successful probe
//! marks the distribution running; `written` says whether Superba changed a host
//! (its baseline is kept locally).
#![cfg(unix)]

mod common;

use std::sync::Arc;

use common::{engine, token_profile};
use superba_core::{
    AuthMode, CodexHome, DataDir, HostKind, HostSource, RemoteHost, RemoteHosts, SshEndpoint,
    WslDistro, WslNetworking,
};
use superba_remote::wsl::{FixedWsl, WslInventory, WslLister};
use superba_remote::{LocalShell, SshRemoteHosts};

fn distro(name: &str, running: Option<bool>) -> WslDistro {
    let mut d = WslDistro {
        version: Some(2),
        networking: WslNetworking::Nat,
        ..WslDistro::named(name)
    };
    if let Some(r) = running {
        d.set_running(r);
    }
    d
}

fn inventory(distros: Vec<WslDistro>) -> WslInventory {
    WslInventory {
        distros,
        networking: WslNetworking::Nat,
        error: None,
    }
}

fn states(hosts: &[RemoteHost]) -> Vec<(String, Option<bool>)> {
    hosts
        .iter()
        .filter(|h| h.kind == HostKind::Wsl)
        .map(|h| {
            (
                h.wsl_distro().unwrap(),
                h.wsl.as_ref().and_then(WslDistro::running),
            )
        })
        .collect()
}

#[tokio::test]
async fn fixed_listers_answer_running_only_when_a_state_is_known() {
    let unknown = FixedWsl(inventory(vec![distro("Ubuntu", None)]));
    assert_eq!(unknown.running().await, None);
    let known = FixedWsl(inventory(vec![
        distro("Ubuntu", Some(true)),
        distro("Debian", Some(false)),
    ]));
    assert_eq!(known.running().await, Some(vec!["Ubuntu".to_owned()]));
}

#[tokio::test]
async fn discovery_carries_the_last_running_answer_and_starts_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    // The registry knows no running state.
    let listed = vec![distro("Ubuntu", None), distro("Debian", None)];
    #[derive(Debug)]
    struct Lister(Vec<WslDistro>, std::sync::Mutex<u32>);
    #[async_trait::async_trait]
    impl WslLister for Lister {
        async fn list(&self) -> WslInventory {
            inventory(self.0.clone())
        }
        async fn running(&self) -> Option<Vec<String>> {
            *self.1.lock().unwrap() += 1;
            Some(vec!["Debian".into()])
        }
    }
    let lister = Arc::new(Lister(listed, std::sync::Mutex::new(0)));
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_home_dir(tmp.path())
        .with_wsl_lister(lister.clone());
    let before = hosts.discover(&home, &[], &[]).await.unwrap();
    assert_eq!(
        states(&before),
        vec![("Ubuntu".into(), None), ("Debian".into(), None)]
    );
    assert_eq!(*lister.1.lock().unwrap(), 0, "discovery asked wsl.exe");
    assert_eq!(
        hosts.wsl_running().await.unwrap(),
        Some(vec!["Debian".to_owned()])
    );
    let after = hosts.discover(&home, &[], &[]).await.unwrap();
    assert_eq!(
        states(&after),
        vec![
            ("Ubuntu".into(), Some(false)),
            ("Debian".into(), Some(true))
        ]
    );
    // Clones share the answer (the service hands clones around).
    let again = hosts.clone().discover(&home, &[], &[]).await.unwrap();
    assert_eq!(states(&again), states(&after));
    assert_eq!(*lister.1.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_probe_marks_the_distribution_running_and_an_apply_makes_it_written() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/config.toml"), "model = \"gpt-5\"\n").unwrap();
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_home_dir(tmp.path())
        .with_shell(Arc::new(LocalShell::new(&home).with_login_shell("/bin/sh")))
        .with_wsl_lister(Arc::new(FixedWsl(inventory(vec![distro(
            "Ubuntu",
            Some(false),
        )]))));
    hosts.wsl_running().await.unwrap();
    let codex_home = CodexHome::new(tmp.path().join(".codex-local"));
    let found = hosts.discover(&codex_home, &[], &[]).await.unwrap();
    assert_eq!(states(&found), vec![("Ubuntu".into(), Some(false))]);
    let host = RemoteHost {
        sources: vec![HostSource::Wsl],
        ssh: SshEndpoint::default(),
        ..found[0].clone()
    };
    assert!(!hosts.written(&host).await.unwrap());
    // The user asked (a probe): it runs now.
    hosts.probe(&host).await.unwrap();
    let found = hosts.discover(&codex_home, &[], &[]).await.unwrap();
    assert_eq!(states(&found), vec![("Ubuntu".into(), Some(true))]);
    let plan = hosts
        .plan_apply(
            &host,
            &token_profile(
                "https://api.example.org",
                AuthMode::ApiKey,
                "sk-test-0123456789",
            ),
        )
        .await
        .unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    hosts.apply(&host, &plan).await.unwrap();
    assert!(hosts.written(&host).await.unwrap());
}
