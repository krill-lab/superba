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

fn at(s: i64) -> Option<superba_core::Timestamp> {
    Some(superba_core::Timestamp::from_millis(s * 1000))
}

fn proc_with(pid: u32, ppid: u32, argv: &[&str]) -> ProcEntry {
    ProcEntry {
        pid,
        ppid: Some(ppid),
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        exe: argv.first().map(std::path::PathBuf::from),
        started_at: at(i64::from(pid)),
        ..Default::default()
    }
}

fn session(main: u32) -> superba_core::AppSession {
    superba_core::AppSession {
        install_id: None,
        main_pid: main,
        started_at: at(i64::from(main)),
        executable: None,
        cdp: None,
        launched_by_us: false,
        authapi_env: superba_core::EnvObservation::Unknown,
        home_override: None,
        codex_home: None,
        app_server_exe: None,
    }
}

/// User bug 53: a report says what each left-over process is.
#[test]
fn roles_name_the_main_process_electron_helpers_and_codex() {
    use superba_appctl::quit::role_of;
    let app = "/opt/ChatGPT/ChatGPT";
    assert_eq!(role_of(&proc_with(10, 1, &[app]), 10), "main");
    assert_eq!(
        role_of(
            &proc_with(11, 10, &[app, "--type=renderer", "--lang=en"]),
            10
        ),
        "renderer"
    );
    // Chromium's rewritten Linux command line: one word.
    assert_eq!(
        role_of(
            &proc_with(12, 10, &[&format!("{app} --type=gpu-process --x")]),
            10
        ),
        "gpu-process"
    );
    assert_eq!(
        role_of(
            &proc_with(13, 10, &["/home/u/.local/bin/codex", "app-server"]),
            10
        ),
        "codex app-server"
    );
    assert_eq!(
        role_of(&proc_with(14, 13, &["/usr/bin/conhost.exe"]), 10),
        "conhost.exe"
    );
}

/// The left-over processes: the main process, the tree recorded when the
/// quit began (orphans keep their parent pid on Windows) and helpers started
/// since, only while they are still the same process.
#[test]
fn lingering_lists_the_live_part_of_the_tree() {
    use superba_appctl::quit::{lingering, merge_tree};
    let entries = vec![
        proc_with(10, 1, &["/app/ChatGPT"]),
        proc_with(11, 10, &["/app/ChatGPT", "--type=renderer"]),
        proc_with(12, 10, &["/app/ChatGPT", "--type=gpu-process"]),
        proc_with(13, 11, &["/app/helper"]),
        proc_with(20, 1, &["/usr/bin/other"]),
    ];
    // Recorded at the start: 11 (alive), 12 (alive), 15 (gone since).
    let recorded = vec![(11, at(11)), (12, at(12)), (15, at(15))];
    let mut tree: Vec<u32> = merge_tree(&recorded, &entries, 10, at(10))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    tree.sort_unstable();
    assert_eq!(tree, vec![11, 12, 13, 15]);
    // 12's pid was reused by another program (another start time).
    let alive = |pid: u32, started: Option<superba_core::Timestamp>| match pid {
        10 | 11 | 13 => true,
        12 => started == at(99),
        _ => false,
    };
    let left: Vec<u32> = lingering(&session(10), &entries, &recorded, &alive)
        .into_iter()
        .map(|(e, _)| e.pid)
        .collect();
    assert_eq!(left, vec![10, 11, 13]);
    // The main process gone: what is left of its tree.
    let alive = |pid: u32, _| pid == 13;
    let left: Vec<u32> = lingering(&session(10), &entries, &recorded, &alive)
        .into_iter()
        .map(|(e, _)| e.pid)
        .collect();
    assert_eq!(left, vec![13]);
}

/// A forced quit after a graceful one keeps both in the record.
#[test]
fn attempts_merge_in_order() {
    use superba_appctl::quit::QuitAttempt;
    use superba_core::{QuitOutcome, QuitStep};
    let mut first = QuitAttempt::new(
        QuitOutcome::TimedOut,
        vec![QuitStep::ClosedWindows, QuitStep::Waited { ms: 5000 }],
    );
    first.tree = vec![(11, at(11))];
    let mut second = QuitAttempt::new(
        QuitOutcome::TimedOut,
        vec![QuitStep::Ended {
            ended: 2,
            failed: 1,
        }],
    );
    second.tree = vec![(11, at(11)), (12, at(12))];
    second
        .end_errors
        .insert(10, "terminate: Access is denied.".into());
    second.terminated = vec![11, 12];
    let merged = first.merge(second);
    assert_eq!(merged.steps.len(), 3);
    assert_eq!(merged.steps[0], QuitStep::ClosedWindows);
    assert_eq!(merged.tree, vec![(11, at(11)), (12, at(12))]);
    assert_eq!(merged.terminated, vec![11, 12]);
    assert_eq!(merged.end_errors.len(), 1);
}

/// Ending one process: only while it is the process recorded, and the
/// result says what happened (Unix: SIGKILL).
#[cfg(unix)]
#[test]
fn ending_a_process_checks_its_start_and_reports_the_result() {
    use std::process::Command;
    use std::time::Duration;

    use superba_appctl::osproc::{self, EndResult};
    use superba_appctl::procs;

    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let started = procs::snapshot_os(procs::ReadOpts::default())
        .into_iter()
        .find(|e| e.pid == pid)
        .and_then(|e| e.started_at);
    assert!(started.is_some());
    let facts = osproc::facts(pid);
    assert_eq!(facts.running, Some(true));
    // Another start time: a reused pid is left alone.
    let other = started.map(|t| superba_core::Timestamp::from_millis(t.as_millis() - 3_600_000));
    assert_eq!(
        osproc::end(pid, other, Duration::from_secs(1)),
        EndResult::Reused
    );
    assert!(child.try_wait().unwrap().is_none(), "still running");
    // Reap it from another thread so the pid is really gone after the kill.
    let reaper = std::thread::spawn(move || child.wait());
    let r = osproc::end(pid, started, Duration::from_secs(5));
    assert_eq!(r, EndResult::Ended, "{}", r.describe());
    reaper.join().unwrap().unwrap();
    assert_eq!(
        osproc::end(pid, started, Duration::from_millis(100)),
        EndResult::AlreadyGone
    );
}

#[test]
fn descendants_exclude_foreign_users_and_children_of_a_reused_parent_pid() {
    let root = proc_with(10, 1, &["app"]);
    let mut foreign = proc_with(11, 10, &["helper"]);
    foreign.foreign_user = true;
    let mut stale = proc_with(12, 10, &["unrelated"]);
    stale.started_at = at(2);
    let entries = [
        root,
        foreign,
        stale,
        proc_with(13, 10, &["renderer"]),
        proc_with(14, 11, &["other-user-child"]),
    ];
    assert_eq!(descendants(&entries, 10), vec![(13, at(13))]);
    assert!(superba_appctl::quit::merge_tree(&[], &entries, 10, at(50)).is_empty());
}

#[test]
fn a_reused_or_missing_root_does_not_add_unrelated_children_to_the_recorded_tree() {
    use superba_appctl::quit::merge_tree;
    let recorded = [(12, at(12))];
    let entries = [
        proc_with(10, 1, &["other-app"]),
        proc_with(11, 10, &["helper"]),
    ];
    assert_eq!(merge_tree(&recorded, &entries, 10, at(50)), recorded);
    assert_eq!(merge_tree(&recorded, &entries[1..], 10, at(10)), recorded);
    let mut foreign_root = entries[0].clone();
    foreign_root.foreign_user = true;
    assert_eq!(
        merge_tree(&recorded, &[foreign_root, entries[1].clone()], 10, at(10)),
        recorded
    );
}
