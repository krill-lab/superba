//! Quit helpers: the app's process tree and waiting for its children.
//!
//! Found by the e2e harness: right after the main process exited, the Codex
//! app-server (its child) was still shutting down, so a session migration asked
//! the user to close it and cancelled the restore.

use pretty_assertions::assert_eq;
use superba_appctl::procs::ProcEntry;
use superba_appctl::quit::{QUIT_APP_JS, descendants, kill_targets};

fn entry(pid: u32, ppid: u32) -> ProcEntry {
    ProcEntry {
        pid,
        ppid: Some(ppid),
        ..Default::default()
    }
}

#[test]
fn descendants_walk_the_whole_tree_of_the_main_process() {
    let entries = vec![
        entry(10, 1),
        entry(11, 10),
        entry(12, 10),
        entry(13, 11),
        entry(14, 13),
        entry(20, 1),
        entry(21, 20),
    ];
    let mut got: Vec<u32> = descendants(&entries, 10)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    got.sort_unstable();
    assert_eq!(got, vec![11, 12, 13, 14]);
    assert!(descendants(&entries, 99).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn waiting_for_children_ends_when_they_exit_and_times_out_otherwise() {
    use std::process::Command;
    use std::time::Duration;

    use superba_appctl::quit::wait_children_exit;

    // Processes this test starts itself (and reaps), by pid.
    let mut short = Command::new("sleep").arg("0.3").spawn().unwrap();
    let short_pid = short.id();
    let reaper = std::thread::spawn(move || short.wait());
    assert!(wait_children_exit(&[(short_pid, None)], Duration::from_secs(10)).await);
    reaper.join().unwrap().unwrap();

    let mut long = Command::new("sleep").arg("30").spawn().unwrap();
    let long_pid = long.id();
    let waited = wait_children_exit(&[(long_pid, None)], Duration::from_millis(400)).await;
    long.kill().unwrap();
    long.wait().unwrap();
    assert!(
        !waited,
        "a running child keeps the wait going until the timeout"
    );

    // Nothing to wait for.
    assert!(wait_children_exit(&[], Duration::from_millis(1)).await);
}

#[test]
fn a_forced_quit_ends_the_main_process_first_then_its_live_descendants() {
    use superba_core::Timestamp;
    let t = |s: i64| Some(Timestamp::from_millis(s * 1000));
    let children = [(11, t(1)), (12, t(1)), (13, t(2)), (11, t(1))];
    // 12 is gone; 13 was reused by another process (other start time).
    let alive = |pid: u32, started: Option<Timestamp>| match pid {
        10 | 11 => true,
        13 => started == t(9),
        _ => false,
    };
    assert_eq!(kill_targets(10, t(0), &children, &alive), vec![10, 11]);
    // The main process already gone: only what is left of the tree.
    let alive = |pid: u32, _| pid == 11;
    assert_eq!(kill_targets(10, t(0), &children, &alive), vec![11]);
}

#[test]
fn the_quit_message_is_the_apps_own() {
    // The app's main process handles `{type: "quit-app"}` from the renderer
    // (`app.quit()`, unless `relaunch` is set).
    assert!(QUIT_APP_JS.contains("sendMessageFromView({type:'quit-app'})"));
    assert!(!QUIT_APP_JS.contains("relaunch"));
}
