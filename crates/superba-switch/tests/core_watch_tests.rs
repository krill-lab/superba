//! The "Verify core" check after a hot restart or a relaunch (user bug 62(a))
//! and the full restart chosen after a failed hot restart (user bug 60).
//!
//! Seen on Windows (e2e/windows W10, ChatGPT 26.1013): after a launch the app
//! runs a short-lived `codex app-server` (~1 s), then the real one 2–3 s
//! later; the check used to follow only the first one it saw and failed with
//! "no new Codex core process started" while the real one was running.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    CodexProcessKind, Decision, ExecuteOptions, OutcomeStatus, ProcessInfo, ProfileId, QuitOutcome,
    RestartKind, SwitchOutcome, SwitchRequest, SwitchService, Timestamp, plan::CheckResult,
};
use superba_switch::core_watch::{CoreState, CoreWatch};
use superba_switch::{Switcher, Tuning};

const S: Duration = Duration::from_secs(1);

fn server(pid: u32, started: i64) -> ProcessInfo {
    app_server(pid, 100, started)
}

fn other(pid: u32) -> ProcessInfo {
    ProcessInfo {
        kind: CodexProcessKind::CliTui,
        ..server(pid, T0)
    }
}

// --- the pure watch --------------------------------------------------------------

#[test]
fn a_short_lived_first_app_server_does_not_fail_the_check() {
    let mut w = CoreWatch::new(Timestamp(T0), &[], 5 * S);
    assert_eq!(w.observe(Duration::ZERO, &[]), CoreState::Waiting);
    // The app's first app-server, gone a second later.
    assert_eq!(w.observe(S, &[server(601, T0)]), CoreState::Settling);
    assert_eq!(w.observe(2 * S, &[]), CoreState::Waiting);
    // The real one: it has to run for the stability period itself.
    assert_eq!(w.observe(4 * S, &[server(602, T0)]), CoreState::Settling);
    assert_eq!(w.observe(8 * S, &[server(602, T0)]), CoreState::Settling);
    assert_eq!(
        w.observe(9 * S, &[server(602, T0), other(7)]),
        CoreState::Passed { pid: 602 }
    );
    assert_eq!(w.exited_new(), vec![601]);
    assert_eq!(w.alive_new(), vec![602]);
}

#[test]
fn the_previous_app_server_must_be_gone_and_old_ones_are_not_new() {
    let before = [150];
    let mut w = CoreWatch::new(Timestamp(T0), &before, Duration::ZERO);
    // Still the previous one, and one started long before the restart.
    assert_eq!(
        w.observe(S, &[server(150, T0 - 60_000), server(151, T0 - 60_000)]),
        CoreState::Waiting
    );
    assert_eq!(
        w.observe(2 * S, &[server(150, T0 - 60_000), server(700, T0)]),
        CoreState::Waiting
    );
    assert_eq!(
        w.observe(3 * S, &[server(700, T0)]),
        CoreState::Passed { pid: 700 }
    );
    // Start times within the slack of the request count as new.
    let mut w = CoreWatch::new(Timestamp(T0), &before, Duration::ZERO);
    assert_eq!(
        w.observe(S, &[server(701, T0 - 2_000)]),
        CoreState::Passed { pid: 701 }
    );
}

#[test]
fn failures_say_what_was_checked_and_what_was_seen() {
    let render = |t: superba_core::LocalizedText| t.render_fallback();
    // Nothing new, the previous one still running.
    let mut w = CoreWatch::new(Timestamp(T0), &[150], 5 * S);
    w.observe(15 * S, &[server(150, T0 - 60_000)]);
    assert_eq!(
        render(w.failure(15 * S, 100)),
        "no new Codex core (codex app-server of ChatGPT, pid 100) started within 15 s; \
         the previous Codex core (pid 150) is still running"
    );
    // Nothing new, the previous one gone.
    let mut w = CoreWatch::new(Timestamp(T0), &[150], 5 * S);
    w.observe(15 * S, &[]);
    assert_eq!(
        render(w.failure(15 * S, 100)),
        "no new Codex core (codex app-server of ChatGPT, pid 100) started within 15 s; \
         the previous Codex core (pid 150) exited"
    );
    // New ones came and went (a crash loop).
    let mut w = CoreWatch::new(Timestamp(T0), &[], 5 * S);
    w.observe(S, &[server(601, T0)]);
    w.observe(3 * S, &[server(602, T0)]);
    w.observe(6 * S, &[]);
    assert_eq!(
        render(w.failure(60 * S, 200)),
        "new Codex core processes of ChatGPT (pid 200) started but exited again \
         (pid 601, 602); none was running after 60 s"
    );
    // A new one runs, the previous one too.
    let mut w = CoreWatch::new(Timestamp(T0), &[150], 5 * S);
    w.observe(S, &[server(150, T0 - 60_000), server(700, T0)]);
    assert_eq!(
        render(w.failure(15 * S, 100)),
        "a new Codex core runs (pid 700), but the previous one (pid 150) was still \
         running after 15 s"
    );
    // A launch whose app never started an app-server.
    let w = CoreWatch::new(Timestamp(T0), &[], 5 * S);
    assert!(!w.seen_any());
}

// --- in the executor -------------------------------------------------------------

fn tuning(stable: Duration) -> Tuning {
    Tuning {
        poll: Duration::from_millis(250),
        verify_timeout: Duration::from_secs(15),
        launch_verify_timeout: Duration::from_secs(60),
        stable,
        reask_after: Duration::from_secs(60),
        plan_cache: 16,
    }
}

async fn run_with(
    w: &World,
    sw: &Switcher,
    opts: ExecuteOptions,
    decider: Arc<ScriptedDecider>,
) -> SwitchOutcome {
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .expect("plan");
    let _ = w;
    sw.execute(plan, opts, decider, Arc::new(RecordingEvents::default()))
        .await
        .expect("execute")
}

fn full_restart_world() -> World {
    let w = World::new();
    w.set_session(Some(session(Some(OVERSEAS), false)));
    w.app
        .processes
        .lock()
        .unwrap()
        .push(app_server(150, 100, T0 - 10));
    w
}

#[tokio::test(start_paused = true)]
async fn a_relaunch_passes_when_the_app_replaces_its_first_app_server() {
    let w = full_restart_world();
    // ~15 s to the first app-server (living 0.7 s), the real one 2 s later:
    // the timings of the Windows VM (procmon of e2e run 20261018-154832).
    *w.app.spawn_script.lock().unwrap() = vec![
        (
            Duration::from_millis(14_500),
            Some(Duration::from_millis(700)),
        ),
        (Duration::from_millis(16_600), None),
    ];
    let sw = Switcher::with_tuning(w.deps(), tuning(5 * S));
    let opts = ExecuteOptions {
        allow_full_restart: true,
        ..ExecuteOptions::default()
    };
    let o = run_with(&w, &sw, opts, ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(o.verification.unwrap().process, CheckResult::Passed);
}

#[tokio::test(start_paused = true)]
async fn a_relaunch_waits_for_a_slow_app_and_then_names_what_it_saw() {
    // The app started a single app-server 46 s after the launch (first start
    // after the setup wizard on the VM): still within the launch window.
    let w = full_restart_world();
    *w.app.spawn_script.lock().unwrap() = vec![(Duration::from_secs(46), None)];
    let sw = Switcher::with_tuning(w.deps(), tuning(5 * S));
    let opts = ExecuteOptions {
        allow_full_restart: true,
        ..ExecuteOptions::default()
    };
    let o = run_with(&w, &sw, opts.clone(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);

    // Every app-server dies within a second: a crash loop fails with the pids.
    let w = full_restart_world();
    *w.app.spawn_script.lock().unwrap() = vec![
        (Duration::from_secs(2), Some(S)),
        (Duration::from_secs(5), Some(S)),
    ];
    let sw = Switcher::with_tuning(w.deps(), tuning(5 * S));
    let o = run_with(&w, &sw, opts, ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly, "{:?}", o.notices);
    let CheckResult::Failed { detail } = o.verification.unwrap().process else {
        panic!("process check should fail");
    };
    assert!(
        detail.starts_with(
            "new Codex core processes of ChatGPT (pid 200) started but exited again (pid 501, 502)"
        ),
        "{detail}"
    );
    // Superba has just reopened ChatGPT: the summary does not tell the user to
    // restart it themselves (user bug 69b).
    let keys: Vec<&str> = o.notices.iter().map(|n| n.text.key.as_str()).collect();
    assert!(
        keys.contains(&"switch.done.reopened_unverified"),
        "{keys:?}"
    );
    assert!(!keys.contains(&"switch.done.written_only"), "{keys:?}");
}

#[tokio::test(start_paused = true)]
async fn a_relaunched_app_running_codex_in_wsl_gets_a_longer_window() {
    // User bug 69b (e2e run 20261018-191254, WSL 1): with "Run Codex in WSL"
    // the app's first core (a wsl.exe) exited and its replacement came 60-80 s
    // after the launch.
    let script = || {
        vec![
            (Duration::from_secs(20), Some(Duration::from_secs(2))),
            (Duration::from_secs(72), None),
        ]
    };
    let opts = ExecuteOptions {
        allow_full_restart: true,
        ..ExecuteOptions::default()
    };
    let w = full_restart_world();
    let mut host = wsl_host("Ubuntu", 1, superba_core::WslNetworking::Nat);
    host.wsl.as_mut().unwrap().default = true;
    w.remote.hosts.lock().unwrap().push(host);
    let home = w.ctx.codex_home();
    std::fs::create_dir_all(home.root()).unwrap();
    std::fs::write(
        home.config_toml(),
        "[desktop]\nrunCodexInWindowsSubsystemForLinux = true\n",
    )
    .unwrap();
    *w.app.spawn_script.lock().unwrap() = script();
    let sw = Switcher::with_tuning(w.deps(), tuning(5 * S));
    let o = run_with(&w, &sw, opts.clone(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(o.verification.unwrap().process, CheckResult::Passed);

    // The app's own Windows core keeps the 60 s window.
    let w = full_restart_world();
    *w.app.spawn_script.lock().unwrap() = script();
    let sw = Switcher::with_tuning(w.deps(), tuning(5 * S));
    let o = run_with(&w, &sw, opts, ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly, "{:?}", o.notices);
}

#[tokio::test(start_paused = true)]
async fn a_relaunched_app_without_a_core_yet_is_named_as_such() {
    // The first start after an install took 93 s to its first app-server on
    // the Windows VM: past the window the check is skipped and says why.
    let w = full_restart_world();
    *w.app.spawn_script.lock().unwrap() = vec![(Duration::from_secs(93), None)];
    let sw = Switcher::with_tuning(w.deps(), tuning(5 * S));
    let opts = ExecuteOptions {
        allow_full_restart: true,
        ..ExecuteOptions::default()
    };
    let o = run_with(&w, &sw, opts, ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    let CheckResult::Skipped { reason } = o.verification.unwrap().process else {
        panic!("process check should be skipped");
    };
    assert_eq!(
        reason,
        "ChatGPT had not started its Codex core 60 s after it was opened (it may still be starting)"
    );
}

#[tokio::test(start_paused = true)]
async fn a_full_restart_that_is_not_forced_after_a_failed_hot_restart_reports_the_written_config() {
    // User bug 60: the hot restart failed, the user chose a full restart, the
    // app did not close and was not forced: the result must not say
    // "nothing was changed" — the configuration is written.
    let w = World::new();
    *w.app.fail_hot_restart.lock().unwrap() = 1;
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .push_back(QuitOutcome::TimedOut);
    let d = ScriptedDecider::with(&[
        ("ipc_failed", Decision::FullRestart),
        ("force_quit", Decision::Cancel),
    ]);
    let sw = w.switcher();
    let o = run_with(&w, &sw, ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly, "{:?}", o.notices);
    assert_eq!(o.restart, RestartKind::None);
    let keys: Vec<String> = o.notices.iter().map(|n| n.text.key.clone()).collect();
    assert!(
        keys.contains(&"switch.failed.full_restart_stopped".to_owned()),
        "{keys:?}"
    );
    assert!(w.calls().contains(&"config.apply".to_owned()));
}

/// Declines the forced quit; the app closes on its own meanwhile.
#[derive(Debug)]
struct ClosesLate {
    app: Arc<FakeApp>,
}

#[async_trait::async_trait]
impl superba_core::SwitchDecider for ClosesLate {
    async fn decide(&self, request: superba_core::DecisionRequest) -> Decision {
        match request {
            superba_core::DecisionRequest::IpcFailed { .. } => Decision::FullRestart,
            superba_core::DecisionRequest::ForceQuit { .. } => {
                *self.app.session.lock().unwrap() = None;
                Decision::Cancel
            }
            other => other.safe_default(),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_app_that_closed_late_is_opened_again_after_a_failed_hot_restart() {
    // The graceful quit timed out and the user declined to force it, but the
    // app closed afterwards (it vanished from the taskbar in user bug 60): the
    // full restart is finished instead of leaving ChatGPT closed.
    let w = World::new();
    *w.app.fail_hot_restart.lock().unwrap() = 1;
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .push_back(QuitOutcome::TimedOut);
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let o = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            Arc::new(ClosesLate { app: w.app.clone() }),
            Arc::new(RecordingEvents::default()),
        )
        .await
        .unwrap();
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert!(
        w.calls().iter().any(|c| c.starts_with("app.launch")),
        "{:?}",
        w.calls()
    );
}
