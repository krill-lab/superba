//! Stopping Codex processes for the user (GH#2): who may be stopped, pid-reuse
//! safety, the forced tree, and a real stop of a child process this test starts.

use std::time::Duration;

use superba_appctl::procs::ProcEntry;
use superba_appctl::stop::{self, Identity, force_targets, identity, stoppable};
use superba_core::{CodexProcessKind, EnvObservation, ProcessInfo, StopOutcome, Timestamp};

fn listed(pid: u32, kind: CodexProcessKind, started: Option<i64>) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: None,
        kind,
        exe: None,
        cmdline: vec!["codex".into()],
        started_at: started.map(Timestamp::from_millis),
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

fn entry(pid: u32, ppid: u32, started: i64) -> ProcEntry {
    ProcEntry {
        pid,
        ppid: Some(ppid),
        started_at: Some(Timestamp::from_millis(started)),
        ..Default::default()
    }
}

#[test]
fn only_codex_consumers_are_stoppable_never_the_desktop_app() {
    for kind in [
        CodexProcessKind::CliTui,
        CodexProcessKind::CliExec,
        CodexProcessKind::CliDaemon,
        CodexProcessKind::IdeAppServer,
        CodexProcessKind::Other,
    ] {
        assert!(stoppable(&kind), "{kind:?}");
    }
    for kind in [
        CodexProcessKind::AppMain,
        CodexProcessKind::AppServer,
        CodexProcessKind::RemoteLink { host: "h".into() },
    ] {
        assert!(!stoppable(&kind), "{kind:?}");
    }
}

#[test]
fn a_reused_pid_or_another_users_process_is_left_alone() {
    let p = listed(10, CodexProcessKind::CliTui, Some(100_000));
    assert_eq!(identity(&p, Some(&entry(10, 1, 100_000))), Identity::Same);
    // Start time within the OS rounding slack: the same process.
    assert_eq!(identity(&p, Some(&entry(10, 1, 101_000))), Identity::Same);
    // Started much later: the pid now belongs to another program.
    assert_eq!(identity(&p, Some(&entry(10, 1, 200_000))), Identity::Gone);
    assert_eq!(identity(&p, None), Identity::Gone);
    let zombie = ProcEntry {
        zombie: true,
        ..entry(10, 1, 100_000)
    };
    assert_eq!(identity(&p, Some(&zombie)), Identity::Gone);
    let foreign = ProcEntry {
        foreign_user: true,
        ..entry(10, 1, 100_000)
    };
    assert_eq!(identity(&p, Some(&foreign)), Identity::ForeignUser);
}

#[test]
fn a_forced_stop_ends_the_process_first_then_its_children() {
    // npm's node wrapper (20) runs the native codex (21), which runs an MCP
    // server (22); 30 is unrelated.
    let entries = vec![
        entry(20, 1, 1),
        entry(21, 20, 2),
        entry(22, 21, 3),
        entry(30, 1, 4),
    ];
    let pids: Vec<u32> = force_targets(&entries, 20)
        .iter()
        .map(|(p, _)| *p)
        .collect();
    assert_eq!(pids[0], 20);
    assert!(pids.contains(&21) && pids.contains(&22));
    assert!(!pids.contains(&30));
}

#[test]
fn the_tui_reset_sequence_leaves_the_alternate_screen_and_shows_the_cursor() {
    assert!(stop::TUI_RESET.contains("\x1b[?1049l"));
    assert!(stop::TUI_RESET.contains("\x1b[?1000l"));
    assert!(stop::TUI_RESET.ends_with("\x1b[?25h"));
}

/// The listed process of a child this test started, as the snapshot sees it.
fn listed_child(child: &std::process::Child, kind: CodexProcessKind) -> ProcessInfo {
    let entries = superba_appctl::procs::snapshot_os(Default::default());
    let e = entries
        .iter()
        .find(|e| e.pid == child.id())
        .expect("child in the snapshot");
    ProcessInfo {
        exe: e.exe.clone(),
        started_at: e.started_at,
        ..listed(child.id(), kind, None)
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_graceful_stop_asks_the_process_to_exit() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let p = listed_child(&child, CodexProcessKind::CliExec);
    let outcome = stop::stop(&p, false, Duration::from_secs(5)).await.unwrap();
    // Reap it so the OS no longer lists a zombie.
    let status = child.wait().unwrap();
    assert_eq!(outcome, StopOutcome::Exited);
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(15), "ended by SIGTERM");
}

#[tokio::test]
async fn a_forced_stop_ends_the_process_and_a_stale_listing_touches_nothing() {
    let mut child = if cfg!(windows) {
        std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    } else {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap()
    };
    let p = listed_child(&child, CodexProcessKind::CliTui);
    // The same pid listed with another start time (a reused pid): untouched.
    let mut stale = p.clone();
    stale.started_at = p
        .started_at
        .map(|t| Timestamp::from_millis(t.as_millis() - 3_600_000));
    assert_eq!(
        stop::stop(&stale, true, Duration::from_secs(2))
            .await
            .unwrap(),
        StopOutcome::NotRunning
    );
    assert!(child.try_wait().unwrap().is_none(), "still running");
    // Without force, Windows cannot ask a console program to exit.
    if cfg!(windows) {
        assert_eq!(
            stop::stop(&p, false, Duration::from_secs(1)).await.unwrap(),
            StopOutcome::ForceOnly
        );
        assert!(child.try_wait().unwrap().is_none(), "still running");
    }
    let outcome = stop::stop(&p, true, Duration::from_secs(5)).await.unwrap();
    child.wait().unwrap();
    assert_eq!(outcome, StopOutcome::Killed);
    // The desktop app is never stopped this way.
    let app = ProcessInfo {
        kind: CodexProcessKind::AppMain,
        ..p.clone()
    };
    assert!(
        stop::stop(&app, true, Duration::from_secs(1))
            .await
            .is_err()
    );
}
