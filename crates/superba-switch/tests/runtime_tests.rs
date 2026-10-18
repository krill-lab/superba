//! GH#4 rules: which configuration each running Codex consumer runs, which of
//! them a running turn belongs to, and how each picks the new configuration up.

use superba_core::codex::{ORIGINATOR_DESKTOP, ORIGINATOR_TUI};
use superba_core::*;
use superba_switch::runtime::{
    RuntimeFacts, attribute_busy, build, busy_of, effect, generation_at,
};

const MIN: i64 = 60_000;
const T: i64 = 1_760_000_000_000;

fn p(pid: u32, ppid: u32, kind: CodexProcessKind, started: Option<i64>) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: Some(ppid),
        kind,
        exe: None,
        cmdline: vec!["codex".into()],
        started_at: started.map(Timestamp),
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

fn generation(n: u64, at: i64, profile: &str, kind: SiteKind) -> GenerationRecord {
    GenerationRecord {
        generation: n,
        profile_id: Some(ProfileId::new(profile)),
        site_kind: Some(kind),
        applied_at: Timestamp(at),
    }
}

fn active(n: u64, at: i64) -> ActiveState {
    ActiveState {
        profile_id: Some(ProfileId::new("krill")),
        site_id: Some(SiteId::new("krill")),
        line_id: Some(LineId::new("overseas")),
        auth_mode: Some(AuthMode::Pat),
        generation: n,
        applied_at: Timestamp(at),
    }
}

fn thread(originator: Option<&str>, cwd: Option<&str>, signal: ActivitySignal) -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::local(),
        thread_id: Some("t".into()),
        title: Some("Refactor".into()),
        rollout_path: None,
        last_event_at: None,
        signal,
        originator: originator.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
    }
}

#[test]
fn a_process_runs_the_generation_that_was_current_when_it_started() {
    let gens = [
        generation(4, T - 60 * MIN, "official", SiteKind::Official),
        generation(5, T, "krill", SiteKind::Krill),
    ];
    let a = active(5, T);
    // Started before Superba's first write: the original configuration.
    let before = generation_at(&gens, Timestamp(T - 120 * MIN));
    assert_eq!(before.generation, None);
    // Started under generation 4.
    match effect(Some(Timestamp(T - 10 * MIN)), Some(&a), &gens) {
        RuntimeEffect::Previous { running } => {
            assert_eq!(running.generation, Some(4));
            assert_eq!(running.profile_id, Some(ProfileId::new("official")));
            assert_eq!(running.site_kind, Some(SiteKind::Official));
        }
        other => panic!("{other:?}"),
    }
    // Started after the write, or within the start-time slack of it (the
    // app-server the switch restarted): current.
    assert_eq!(
        effect(Some(Timestamp(T + MIN)), Some(&a), &gens),
        RuntimeEffect::Current
    );
    assert_eq!(
        effect(Some(Timestamp(T - 2_000)), Some(&a), &gens),
        RuntimeEffect::Current
    );
    // Nothing written / no start time: not known.
    assert_eq!(
        effect(Some(Timestamp(T)), None, &gens),
        RuntimeEffect::Unknown {
            reason: UnknownReason::NothingWritten
        }
    );
    assert_eq!(
        effect(None, Some(&a), &gens),
        RuntimeEffect::Unknown {
            reason: UnknownReason::NoStartTime
        }
    );
}

fn facts<'a>(
    processes: &'a [ProcessInfo],
    a: Option<&'a ActiveState>,
    gens: &'a [GenerationRecord],
    hot: Option<&'a HotSwitchStatus>,
    busy: Option<&'a [ThreadActivity]>,
) -> RuntimeFacts<'a> {
    RuntimeFacts {
        processes,
        active: a,
        generations: gens,
        hot,
        busy,
        can_stop: true,
        now: Timestamp(T + MIN),
    }
}

#[test]
fn every_consumer_gets_one_row_with_its_effect_and_advice() {
    let gens = [generation(5, T, "krill", SiteKind::Krill)];
    let a = active(5, T);
    let mut ide = p(40, 30, CodexProcessKind::IdeAppServer, Some(T - 30 * MIN));
    ide.ide = Some(IdeKind::VsCode);
    let processes = vec![
        p(100, 1, CodexProcessKind::AppMain, Some(T - 120 * MIN)),
        // The app's old core is still exiting; the new one runs the new config.
        p(150, 100, CodexProcessKind::AppServer, Some(T - 100 * MIN)),
        p(151, 100, CodexProcessKind::AppServer, Some(T + 1_000)),
        p(
            120,
            100,
            CodexProcessKind::RemoteLink { host: "dev".into() },
            Some(T),
        ),
        p(200, 50, CodexProcessKind::CliTui, Some(T - 5 * MIN)),
        p(300, 50, CodexProcessKind::CliExec, Some(T + 2 * MIN)),
        p(400, 1, CodexProcessKind::CliDaemon, Some(T - 3 * MIN)),
        ide,
    ];
    let hot = HotSwitchStatus::Ready;
    let view = build(&facts(&processes, Some(&a), &gens, Some(&hot), None));
    let roles: Vec<(u32, ConsumerRole)> = view
        .consumers
        .iter()
        .map(|c| (c.pid, c.role.clone()))
        .collect();
    assert_eq!(
        roles,
        vec![
            (100, ConsumerRole::DesktopApp),
            (200, ConsumerRole::Terminal),
            (300, ConsumerRole::Exec),
            (400, ConsumerRole::Daemon),
            (
                40,
                ConsumerRole::IdeExtension {
                    ide: Some(IdeKind::VsCode)
                }
            ),
        ],
        "remote links are left out, app-servers fold into the app"
    );
    let row = |pid| view.consumers.iter().find(|c| c.pid == pid).unwrap();
    let app = row(100);
    assert_eq!(app.core_pid, Some(151), "the newest core counts");
    assert_eq!(app.effect, RuntimeEffect::Current);
    assert_eq!(app.advice, RestartAdvice::None);
    assert!(!app.can_stop, "the desktop app is never stopped this way");
    assert_eq!(row(200).advice, RestartAdvice::Reopen);
    assert!(row(200).can_stop);
    assert_eq!(row(300).effect, RuntimeEffect::Current);
    assert_eq!(row(400).advice, RestartAdvice::RestartDaemon);
    assert_eq!(
        row(40).advice,
        RestartAdvice::ReloadIde {
            ide: Some(IdeKind::VsCode)
        }
    );
    assert_eq!(view.previous_count(), 3);
    assert!(!view.busy_checked);
    assert!(
        view.consumers
            .iter()
            .all(|c| c.busy == BusyState::NotChecked)
    );
    assert_eq!(view.written, Some(a.clone()));
}

#[test]
fn an_app_core_on_the_previous_configuration_is_hot_restarted_or_reopened() {
    let gens = [generation(5, T, "krill", SiteKind::Krill)];
    let a = active(5, T);
    let processes = vec![
        p(100, 1, CodexProcessKind::AppMain, Some(T - 120 * MIN)),
        p(150, 100, CodexProcessKind::AppServer, Some(T - 100 * MIN)),
    ];
    let ready = HotSwitchStatus::Ready;
    let view = build(&facts(&processes, Some(&a), &gens, Some(&ready), None));
    assert!(view.consumers[0].is_previous());
    assert_eq!(view.consumers[0].advice, RestartAdvice::HotRestart);
    let no_cdp = HotSwitchStatus::Unavailable {
        reason: FullRestartReason::NoCdp,
    };
    let view = build(&facts(&processes, Some(&a), &gens, Some(&no_cdp), None));
    assert_eq!(view.consumers[0].advice, RestartAdvice::FullRestart);
    // The app without a visible core: nothing to say.
    let only_main = vec![p(100, 1, CodexProcessKind::AppMain, Some(T - 120 * MIN))];
    let view = build(&facts(&only_main, Some(&a), &gens, Some(&ready), None));
    assert_eq!(
        view.consumers[0].effect,
        RuntimeEffect::Unknown {
            reason: UnknownReason::NoCore
        }
    );
    assert_eq!(view.consumers[0].advice, RestartAdvice::None);
}

#[test]
fn running_turns_belong_to_the_client_kind_that_runs_them() {
    let mut tui_a = p(200, 50, CodexProcessKind::CliTui, Some(T - 5 * MIN));
    tui_a.cwd = Some("/work/a".into());
    let mut tui_b = p(201, 51, CodexProcessKind::CliTui, Some(T - 5 * MIN));
    tui_b.cwd = Some("/work/b".into());
    let mut ide = p(40, 30, CodexProcessKind::IdeAppServer, Some(T - 30 * MIN));
    ide.ide = Some(IdeKind::Cursor);
    let processes = vec![
        p(100, 1, CodexProcessKind::AppMain, Some(T - 120 * MIN)),
        p(150, 100, CodexProcessKind::AppServer, Some(T - 100 * MIN)),
        tui_a,
        tui_b,
        ide,
    ];
    let gens = [generation(5, T, "krill", SiteKind::Krill)];
    let a = active(5, T);
    let busy = vec![
        thread(
            Some(ORIGINATOR_TUI),
            Some("/work/b"),
            ActivitySignal::RolloutTail,
        ),
        thread(
            Some("codex_vscode"),
            Some("/x"),
            ActivitySignal::RolloutTail,
        ),
        thread(None, None, ActivitySignal::RendererDom),
    ];
    let view = build(&facts(&processes, Some(&a), &gens, None, Some(&busy)));
    let busy_of_row = |pid| match &view.consumers.iter().find(|c| c.pid == pid).unwrap().busy {
        BusyState::Running { threads } => threads.len(),
        BusyState::Idle => 0,
        other => panic!("{other:?}"),
    };
    assert!(view.busy_checked);
    assert_eq!(busy_of_row(100), 1, "the renderer's turn is the app's");
    assert_eq!(busy_of_row(200), 0, "the other terminal is idle");
    assert_eq!(busy_of_row(201), 1, "the terminal in the session's folder");
    assert_eq!(busy_of_row(40), 1, "a vscode session runs in the extension");
}

#[test]
fn a_turn_of_an_unknown_client_warns_everyone_and_a_known_one_without_a_row_nobody() {
    let mut rows = build(&facts(
        &[
            p(200, 50, CodexProcessKind::CliTui, Some(T)),
            p(40, 30, CodexProcessKind::IdeAppServer, Some(T)),
        ],
        None,
        &[],
        None,
        None,
    ))
    .consumers;
    attribute_busy(
        &mut rows,
        &[thread(None, None, ActivitySignal::RolloutMtime)],
    );
    assert!(rows.iter().all(|r| r.busy.is_running()));
    attribute_busy(
        &mut rows,
        &[thread(
            Some(ORIGINATOR_DESKTOP),
            None,
            ActivitySignal::RolloutTail,
        )],
    );
    assert!(rows.iter().all(|r| r.busy == BusyState::Idle));
}

#[test]
fn a_terminal_turn_falls_back_to_the_daemon_that_serves_the_tui() {
    let processes = [p(400, 1, CodexProcessKind::CliDaemon, Some(T))];
    let busy = [thread(
        Some(ORIGINATOR_TUI),
        None,
        ActivitySignal::RolloutTail,
    )];
    let found = busy_of(&processes, &busy);
    assert_eq!(found.len(), 1);
    // With a terminal listed too, the terminal owns it.
    let processes = [
        p(400, 1, CodexProcessKind::CliDaemon, Some(T)),
        p(200, 50, CodexProcessKind::CliTui, Some(T)),
    ];
    let rows = build(&facts(&processes, None, &[], None, Some(&busy))).consumers;
    let row = |pid| rows.iter().find(|r| r.pid == pid).unwrap();
    assert_eq!(row(400).busy, BusyState::Idle, "not the daemon");
    assert!(row(200).busy.is_running());
}

#[test]
fn nothing_is_stoppable_when_the_controller_cannot_stop() {
    let processes = [p(200, 50, CodexProcessKind::CliTui, Some(T))];
    let mut f = facts(&processes, None, &[], None, None);
    f.can_stop = false;
    assert!(!build(&f).consumers[0].can_stop);
}
