//! What one Superba process records about the WSL distributions reaches the
//! others (user bug 68): `spb switch` next to the open GUI leaves a stopped
//! distribution pending in `<data dir>/cache/hosts.json`, and the GUI's host
//! list shows it (it read the file once before); a write of the GUI keeps what
//! `spb` recorded; `hosts_now` asks Windows which distributions run, `hosts`
//! never does (the GUI lists hosts on a timer).

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_core::{
    ExecuteOptions, NoEvents, ProfileId, SafeDefaults, SwitchRequest, SwitchService, TargetId,
    WslNetworking,
};
use superba_switch::Switcher;

const UBUNTU: &str = "WSL: Ubuntu";

fn target() -> TargetId {
    superba_core::wsl::target_id("Ubuntu")
}

fn world() -> World {
    let w = World::new();
    let mut host = wsl_host("Ubuntu", 1, WslNetworking::Nat);
    host.wsl.as_mut().unwrap().set_running(true);
    w.remote.hosts.lock().unwrap().push(host);
    w.remote.written.lock().unwrap().push(UBUNTU.into());
    w
}

fn set_running(w: &World, running: bool) {
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        if let Some(d) = h.wsl.as_mut() {
            d.set_running(running);
        }
    }
}

async fn switch(sw: &Switcher, request: SwitchRequest) {
    let plan = sw.plan(request).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
}

async fn pending(sw: &Switcher) -> bool {
    sw.hosts()
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.host.target_id == target())
        .is_some_and(|h| h.pending.is_some())
}

#[tokio::test]
async fn the_gui_shows_what_spb_left_pending_and_keeps_it_when_it_writes() {
    let w = world();
    // The GUI process has listed its hosts already (it read the cache file).
    let gui = w.switcher();
    assert!(!pending(&gui).await);

    // `spb switch`, another process, while the distribution is stopped.
    set_running(&w, false);
    let spb = w.switcher();
    switch(&spb, SwitchRequest::profile(ProfileId::new("work"))).await;
    assert!(pending(&spb).await);
    assert!(w.remote.probes.lock().unwrap().is_empty());

    // The GUI's next list shows it, the "Check again" one too.
    assert!(pending(&gui).await);
    assert!(
        gui.hosts_now()
            .await
            .unwrap()
            .iter()
            .any(|h| h.host.target_id == target() && h.pending.is_some())
    );

    // A write of the GUI (a check of the distribution) keeps spb's record.
    set_running(&w, true);
    gui.host_probe(&target()).await.unwrap();
    assert!(pending(&w.switcher()).await, "the GUI's write dropped it");

    // `spb wsl update` (the pending request) clears it; the GUI follows.
    let request = gui
        .hosts()
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.host.target_id == target())
        .and_then(|h| h.pending)
        .unwrap()
        .request(&target());
    let spb = w.switcher();
    switch(&spb, request).await;
    assert!(!pending(&spb).await);
    assert!(!pending(&gui).await);
}

#[tokio::test]
async fn only_hosts_now_asks_windows_which_distributions_run() {
    let w = world();
    let sw = w.switcher();
    sw.hosts().await.unwrap();
    assert_eq!(*w.remote.running_calls.lock().unwrap(), 0);
    let hosts = sw.hosts_now().await.unwrap();
    assert_eq!(*w.remote.running_calls.lock().unwrap(), 1);
    assert!(hosts.iter().any(|h| h.host.target_id == target()));
    // Neither checks (starts) the distribution.
    assert!(w.remote.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn merging_another_process_record_keeps_the_newer_probe() {
    use superba_core::{HostPending, RemoteHosts, SwitchIntent, Timestamp};
    use superba_switch::hosts::{CachedHost, merge_cached};

    let w = world();
    let host = wsl_host("Ubuntu", 1, WslNetworking::Nat);
    let base = w.remote.probe(&host).await.unwrap();
    let probe = |at: i64, url: &str| {
        let mut s = base.clone();
        s.probed_at = Timestamp(at);
        s.openai_base_url = Some(url.to_owned());
        s
    };
    let url = |c: &CachedHost| c.state.as_ref().unwrap().openai_base_url.clone().unwrap();
    let pending = HostPending {
        at: Timestamp(5),
        intent: SwitchIntent::Profile {
            profile_id: ProfileId::new("work"),
            line_id: None,
            auth_mode: None,
        },
    };
    // Newer on disk: taken, with the pending record.
    let mut mem = CachedHost {
        state: Some(probe(1, "https://old")),
        ..Default::default()
    };
    merge_cached(
        &mut mem,
        CachedHost {
            state: Some(probe(2, "https://new")),
            pending: Some(pending),
            ..Default::default()
        },
    );
    assert_eq!(url(&mem), "https://new");
    assert!(mem.pending.is_some());
    // Older on disk: this process' probe stays; a cleared record is cleared.
    merge_cached(
        &mut mem,
        CachedHost {
            state: Some(probe(1, "https://old")),
            ..Default::default()
        },
    );
    assert_eq!(url(&mem), "https://new");
    assert!(mem.pending.is_none());
}
