//! Plan 7.2 UI transitions, driven exclusively by explicit monotonic readings.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use pretty_assertions::assert_eq;
use superba_core::{
    ActivitySignal, AuthMode, BlockReason, Countdown, Error, FullRestartReason, LineId,
    LocalizedText, MigrationPlan, MigrationRequest, OutcomeStatus, PlanChoice, PlanDestination,
    ProfileId, RestartKind, Severity, SiteId, StepStatus, SwitchEvent, SwitchOutcome, SwitchPhase,
    SwitchPlan, SwitchRequest, SwitchStep, TargetId, ThreadActivity, Timestamp,
};
use superba_switch::{Effect, Input, MachineState, PlanTicket, SwitchMachine, machine, messages};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn request(name: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(name))
}

fn hot_ticket(id: &str) -> PlanTicket {
    PlanTicket {
        plan_id: id.into(),
        primary: RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
        countdown_allowed: true,
        needs_confirmation: false,
        blocked: vec![],
        choices: 0,
        destination: LocalizedText::new("test.destination", "Overseas"),
        busy: vec![],
    }
}

fn full_ticket(id: &str) -> PlanTicket {
    PlanTicket {
        primary: RestartKind::FullAppRestart {
            reason: FullRestartReason::SessionMigration,
        },
        countdown_allowed: false,
        needs_confirmation: true,
        ..hot_ticket(id)
    }
}

fn blocked_ticket(id: &str) -> PlanTicket {
    let reasons = vec![BlockReason::MissingKey, BlockReason::JournalPending];
    PlanTicket {
        primary: RestartKind::Blocked {
            reasons: reasons.clone(),
        },
        blocked: reasons,
        countdown_allowed: false,
        ..hot_ticket(id)
    }
}

fn busy_thread(id: &str) -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::local(),
        thread_id: Some(id.into()),
        title: Some(format!("Thread {id}")),
        rollout_path: None,
        last_event_at: None,
        signal: ActivitySignal::RolloutTail,
    }
}

fn execute(
    ticket: &PlanTicket,
    write_only: bool,
    allow_full_restart: bool,
    force_turns: bool,
) -> Effect {
    Effect::Execute {
        plan_id: ticket.plan_id.clone(),
        write_only,
        allow_full_restart,
        force_turns,
    }
}

fn discard(ticket: &PlanTicket) -> Effect {
    Effect::Discard {
        plan_id: ticket.plan_id.clone(),
    }
}

fn assert_state(machine: &SwitchMachine, expected: MachineState, phase: SwitchPhase) {
    assert_eq!(machine.state(), &expected);
    assert_eq!(machine.phase(), phase);
    assert_eq!(
        machine.cancel_is_free(),
        !matches!(expected, MachineState::Running { .. })
    );
    let ticket = match &expected {
        MachineState::Idle | MachineState::Planning { .. } => None,
        MachineState::Failed { ticket, .. } => ticket.as_ref(),
        MachineState::Blocked { ticket }
        | MachineState::NeedsAnswers { ticket }
        | MachineState::Countdown { ticket, .. }
        | MachineState::WaitingForTurn { ticket, .. }
        | MachineState::ConfirmFull { ticket }
        | MachineState::Running { ticket, .. }
        | MachineState::Done { ticket, .. } => Some(ticket),
    };
    assert_eq!(machine.ticket(), ticket);
    if !matches!(
        expected,
        MachineState::Countdown { .. } | MachineState::WaitingForTurn { .. }
    ) {
        assert_eq!(machine.seconds_left(ms(100_000)), None);
        assert_eq!(machine.progress(ms(100_000)), None);
    }
}

fn select(machine: &mut SwitchMachine, name: &str, seq: u64, now: Duration) {
    let request = request(name);
    assert_eq!(
        machine.handle(Input::Select(request.clone()), now),
        vec![Effect::Plan { seq, request }]
    );
    assert_state(
        machine,
        MachineState::Planning { seq },
        SwitchPhase::Planning,
    );
}

fn first_plan(machine: &mut SwitchMachine, ticket: &PlanTicket, now: Duration) -> Vec<Effect> {
    select(machine, "profile", 1, now);
    machine.handle(
        Input::Planned {
            seq: 1,
            ticket: ticket.clone(),
        },
        now,
    )
}

fn armed(secs: u8, now: Duration) -> (SwitchMachine, PlanTicket) {
    assert!(secs > 0);
    let mut machine = SwitchMachine::new(secs);
    let ticket = hot_ticket("hot");
    assert_eq!(first_plan(&mut machine, &ticket, now), vec![]);
    assert_state(
        &machine,
        MachineState::Countdown {
            ticket: ticket.clone(),
            countdown: Countdown::start(machine::countdown_duration(secs), now),
        },
        SwitchPhase::Armed,
    );
    (machine, ticket)
}

fn confirming() -> (SwitchMachine, PlanTicket) {
    let mut machine = SwitchMachine::new(5);
    let ticket = full_ticket("full");
    assert_eq!(first_plan(&mut machine, &ticket, ms(0)), vec![]);
    assert_state(
        &machine,
        MachineState::ConfirmFull {
            ticket: ticket.clone(),
        },
        SwitchPhase::ConfirmFull,
    );
    (machine, ticket)
}

fn running_machine() -> (SwitchMachine, PlanTicket) {
    let (mut machine, ticket) = armed(5, ms(0));
    assert_eq!(
        machine.handle(Input::SwitchNow, ms(1)),
        vec![execute(&ticket, false, false, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket: ticket.clone(),
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
    (machine, ticket)
}

fn outcome(ticket: &PlanTicket, status: OutcomeStatus) -> SwitchOutcome {
    SwitchOutcome {
        plan_id: ticket.plan_id.clone(),
        status,
        elapsed_ms: 1234,
        generation: Some(7),
        restart: ticket.primary.clone(),
        verification: None,
        remotes: vec![],
        stale_processes: vec![],
        returned_to: None,
        notices: vec![],
    }
}

fn switch_plan(primary: RestartKind) -> SwitchPlan {
    SwitchPlan {
        id: "hand-built-plan".into(),
        created_at: Timestamp(1234),
        request: request("profile"),
        from: None,
        to: PlanDestination {
            profile_id: Some(ProfileId::new("profile")),
            site_id: Some(SiteId::new("krill")),
            line_id: Some(LineId::new("overseas")),
            auth_mode: Some(AuthMode::Pat),
            label: LocalizedText::new("test.destination", "Krill overseas"),
        },
        primary,
        restarts: vec![],
        config: None,
        env: None,
        migration: None,
        remotes: vec![],
        active_threads: vec![],
        model_check: None,
        choices: vec![],
        alternatives: vec![],
        warnings: vec![],
        cleanup: vec![],
    }
}

#[test]
fn new_machine_is_idle_with_no_ticket_timer_or_effects() {
    // Plan 7.2 starts in Idle; no work or writes are implicit in construction.
    let machine = SwitchMachine::new(5);
    assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
    assert_eq!(machine.countdown_len(), ms(5000));
}

#[test]
fn selections_increment_sequence_and_preserve_the_complete_request() {
    // Plan 7.2 Idle/Planning -> Planning: each selection plans once with its own request.
    let mut machine = SwitchMachine::new(5);
    select(&mut machine, "first", 1, ms(10));
    let mut second = request("second");
    second.targets.remotes = vec![TargetId::new("ssh:devbox")];
    second.model_override = Some("model-override".into());
    second.accept_unknown_versions = true;
    assert_eq!(
        machine.handle(Input::Select(second.clone()), ms(20)),
        vec![Effect::Plan {
            seq: 2,
            request: second
        }]
    );
    assert_state(
        &machine,
        MachineState::Planning { seq: 2 },
        SwitchPhase::Planning,
    );
}

#[test]
fn stale_plan_is_discarded_without_replacing_the_latest_selection() {
    // Plan 7.2 Planning -> Planning: delayed results cannot arm or execute an old selection.
    let mut machine = SwitchMachine::new(0);
    select(&mut machine, "first", 1, ms(0));
    select(&mut machine, "second", 2, ms(1));
    let stale = hot_ticket("stale");
    assert_eq!(
        machine.handle(
            Input::Planned {
                seq: 1,
                ticket: stale.clone()
            },
            ms(2)
        ),
        vec![discard(&stale)]
    );
    assert_state(
        &machine,
        MachineState::Planning { seq: 2 },
        SwitchPhase::Planning,
    );
    let current = hot_ticket("current");
    assert_eq!(
        machine.handle(
            Input::Planned {
                seq: 2,
                ticket: current.clone()
            },
            ms(3)
        ),
        vec![execute(&current, false, false, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket: current,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
}

#[test]
fn only_the_current_planning_failure_enters_failed() {
    // Plan 7.2 Planning -> Failed: stale errors are ignored, current errors carry no ticket.
    let mut machine = SwitchMachine::new(5);
    select(&mut machine, "first", 1, ms(0));
    select(&mut machine, "second", 2, ms(1));
    let error = Error::other("planning failed");
    assert_eq!(
        machine.handle(
            Input::PlanFailed {
                seq: 1,
                error: error.clone()
            },
            ms(2)
        ),
        vec![]
    );
    assert_state(
        &machine,
        MachineState::Planning { seq: 2 },
        SwitchPhase::Planning,
    );
    assert_eq!(
        machine.handle(
            Input::PlanFailed {
                seq: 2,
                error: error.clone()
            },
            ms(3)
        ),
        vec![]
    );
    assert_state(
        &machine,
        MachineState::Failed {
            ticket: None,
            error,
        },
        SwitchPhase::Done,
    );
    assert_eq!(machine.handle(Input::Dismiss, ms(4)), vec![]);
    assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
}

#[test]
fn blocked_plan_takes_precedence_over_choices_confirmation_and_countdown() {
    // Plan 7.2 Planning -> Blocked (plan 7.1 rule 1) must never produce Execute.
    let mut machine = SwitchMachine::new(0);
    let ticket = PlanTicket {
        choices: 2,
        needs_confirmation: true,
        countdown_allowed: true,
        ..blocked_ticket("blocked")
    };
    assert_eq!(first_plan(&mut machine, &ticket, ms(10)), vec![]);
    assert_state(
        &machine,
        MachineState::Blocked { ticket },
        SwitchPhase::Blocked,
    );
}

#[test]
fn unanswered_choices_take_precedence_over_confirmation_and_countdown() {
    // Plan 7.2 Planning -> NeedsAnswers: answers must be replanned before any execution.
    let mut machine = SwitchMachine::new(0);
    let ticket = PlanTicket {
        choices: 2,
        needs_confirmation: true,
        countdown_allowed: true,
        ..full_ticket("questions")
    };
    assert_eq!(first_plan(&mut machine, &ticket, ms(10)), vec![]);
    assert_state(
        &machine,
        MachineState::NeedsAnswers { ticket },
        SwitchPhase::Planning,
    );
}

#[test]
fn confirmation_takes_precedence_over_even_a_zero_countdown() {
    // Plan 7.2 Planning -> ConfirmFull: full restart/migration always needs an explicit action.
    let mut machine = SwitchMachine::new(0);
    let ticket = PlanTicket {
        countdown_allowed: true,
        ..full_ticket("confirm")
    };
    assert_eq!(first_plan(&mut machine, &ticket, ms(10)), vec![]);
    assert_state(
        &machine,
        MachineState::ConfirmFull { ticket },
        SwitchPhase::ConfirmFull,
    );
}

#[test]
fn zero_countdown_enters_turn_check_without_forcing_or_confirming() {
    // Plan 7.2 Planning -> Running(TurnCheck): execution still checks busy turns for a zero timer.
    for busy in [vec![], vec![busy_thread("busy")]] {
        let mut machine = SwitchMachine::new(0);
        let ticket = PlanTicket {
            busy,
            ..hot_ticket("immediate")
        };
        assert_eq!(
            first_plan(&mut machine, &ticket, ms(100)),
            vec![execute(&ticket, false, false, false)]
        );
        assert_state(
            &machine,
            MachineState::Running {
                ticket,
                phase: SwitchPhase::TurnCheck,
                write_only: false,
            },
            SwitchPhase::TurnCheck,
        );
    }
}

#[test]
fn nonhot_plan_without_confirmation_executes_immediately() {
    // Plan 7.2 Planning -> Applying via TurnCheck (plan 7.1 rule 4: app stopped).
    for primary in [RestartKind::None, RestartKind::NextLaunch] {
        let mut machine = SwitchMachine::new(30);
        let ticket = PlanTicket {
            primary,
            countdown_allowed: false,
            ..hot_ticket("not-running")
        };
        assert_eq!(
            first_plan(&mut machine, &ticket, ms(200)),
            vec![execute(&ticket, false, false, false)]
        );
        assert_state(
            &machine,
            MachineState::Running {
                ticket,
                phase: SwitchPhase::TurnCheck,
                write_only: false,
            },
            SwitchPhase::TurnCheck,
        );
    }
}

#[test]
fn positive_countdown_starts_at_plan_delivery_time() {
    // Plan 7.2 Planning -> Armed: time spent planning is not deducted from the countdown.
    let mut machine = SwitchMachine::new(5);
    select(&mut machine, "profile", 1, ms(100));
    let ticket = hot_ticket("delayed-plan");
    assert_eq!(
        machine.handle(
            Input::Planned {
                seq: 1,
                ticket: ticket.clone()
            },
            ms(90_000)
        ),
        vec![]
    );
    assert_state(
        &machine,
        MachineState::Countdown {
            ticket,
            countdown: Countdown::start(ms(5000), ms(90_000)),
        },
        SwitchPhase::Armed,
    );
    assert_eq!(machine.seconds_left(ms(90_000)), Some(5));
}

#[test]
fn plan_time_busy_threads_start_a_paused_full_countdown() {
    // Plan 7.2 Planning -> WaitingTurn: busy plans preserve the full countdown indefinitely.
    let mut machine = SwitchMachine::new(5);
    let ticket = PlanTicket {
        busy: vec![busy_thread("one"), busy_thread("two")],
        ..hot_ticket("busy-plan")
    };
    assert_eq!(first_plan(&mut machine, &ticket, ms(100)), vec![]);
    let mut countdown = Countdown::start(ms(5000), ms(100));
    countdown.pause(ms(100));
    let expected = MachineState::WaitingForTurn {
        ticket: ticket.clone(),
        countdown,
        busy: ticket.busy.clone(),
    };
    assert_state(&machine, expected.clone(), SwitchPhase::WaitingTurn);
    assert_eq!(machine.handle(Input::Tick, ms(1_000_000)), vec![]);
    assert_state(&machine, expected, SwitchPhase::WaitingTurn);
    assert_eq!(machine.seconds_left(ms(1_000_000)), Some(5));
    assert_eq!(machine.progress(ms(1_000_000)), Some(0.0));
}

#[test]
fn plan_ticket_copies_snapshot_fields_and_derives_routing_for_every_restart_kind() {
    // Plan 7.2: from_plan is the bridge from plan 7.1 decisions to UI routing flags.
    let cases = [
        (RestartKind::None, false, false),
        (hot_ticket("local").primary, true, false),
        (
            RestartKind::AppServerHot {
                host_id: "devbox".into(),
                kill_remote: true,
            },
            true,
            false,
        ),
        (full_ticket("full").primary, false, true),
        (RestartKind::NextLaunch, false, false),
        (RestartKind::DaemonRestart, false, true),
        (blocked_ticket("blocked").primary, false, false),
    ];
    for (primary, countdown_allowed, needs_confirmation) in cases {
        let mut plan = switch_plan(primary.clone());
        plan.active_threads = vec![busy_thread("one")];
        let blocked = if let RestartKind::Blocked { reasons } = &primary {
            reasons.clone()
        } else {
            vec![]
        };
        let expected = PlanTicket {
            plan_id: "hand-built-plan".into(),
            primary,
            countdown_allowed,
            needs_confirmation,
            blocked,
            choices: 0,
            destination: plan.to.label.clone(),
            busy: plan.active_threads.clone(),
        };
        let ticket = PlanTicket::from_plan(&plan);
        assert_eq!(ticket, expected);
        plan.to.label.fallback = "changed after snapshot".into();
        plan.active_threads.clear();
        assert_eq!(ticket, expected);
    }
}

#[test]
fn plan_ticket_counts_questions_and_machine_routes_to_needs_answers() {
    // Plan 7.2 Planning -> NeedsAnswers: a real SwitchPlan with choices cannot auto-execute.
    let mut plan = switch_plan(hot_ticket("hot").primary);
    plan.choices = vec![
        PlanChoice::UnknownVersion {
            consumer: "app".into(),
        },
        PlanChoice::SharedHome {
            targets: vec![TargetId::new("ssh:devbox")],
        },
    ];
    let ticket = PlanTicket::from_plan(&plan);
    assert_eq!(
        ticket,
        PlanTicket {
            plan_id: plan.id.clone(),
            destination: plan.to.label.clone(),
            choices: 2,
            countdown_allowed: false,
            ..hot_ticket("unused")
        }
    );
    let mut machine = SwitchMachine::new(0);
    assert_eq!(first_plan(&mut machine, &ticket, ms(0)), vec![]);
    assert_state(
        &machine,
        MachineState::NeedsAnswers { ticket },
        SwitchPhase::Planning,
    );
}

#[test]
fn migration_in_a_real_plan_disables_countdown_and_requires_confirmation() {
    // Plan 7.2 Planning -> ConfirmFull: migration independently forces confirmation.
    let mut plan = switch_plan(hot_ticket("hot").primary);
    plan.migration = Some(MigrationPlan {
        id: "migration".into(),
        codex_home: "/virtual/codex".into(),
        request: MigrationRequest {
            to_provider: "openai".into(),
            selectors: vec![],
            include_archived: true,
            exclude: vec![],
        },
        thread_count: 0,
        rollout_count: 0,
        by_source_provider: BTreeMap::new(),
        backup_dir: "/virtual/backups".into(),
        backup_bytes: 0,
        items: vec![],
    });
    let ticket = PlanTicket::from_plan(&plan);
    assert_eq!(
        ticket,
        PlanTicket {
            plan_id: plan.id.clone(),
            destination: plan.to.label.clone(),
            countdown_allowed: false,
            needs_confirmation: true,
            ..hot_ticket("unused")
        }
    );
    let mut machine = SwitchMachine::new(0);
    assert_eq!(first_plan(&mut machine, &ticket, ms(0)), vec![]);
    assert_state(
        &machine,
        MachineState::ConfirmFull { ticket },
        SwitchPhase::ConfirmFull,
    );
}

#[test]
fn countdown_ticks_execute_once_at_or_after_the_deadline() {
    // Plan 7.2 Armed -> TurnCheck: expiry is inclusive and cannot emit Execute twice.
    for expiry in [6000, 6001, 100_000] {
        let (mut machine, ticket) = armed(5, ms(1000));
        let before = machine.state().clone();
        assert_eq!(machine.handle(Input::Tick, ms(5999)), vec![]);
        assert_state(&machine, before, SwitchPhase::Armed);
        assert_eq!(
            machine.handle(Input::Tick, ms(expiry)),
            vec![execute(&ticket, false, false, false)]
        );
        let running = MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        };
        assert_state(&machine, running.clone(), SwitchPhase::TurnCheck);
        assert_eq!(machine.handle(Input::Tick, ms(expiry + 1)), vec![]);
        assert_state(&machine, running, SwitchPhase::TurnCheck);
    }
}

#[test]
fn countdown_seconds_round_up_and_progress_stays_in_range() {
    // Plan 7.2 Armed display: round remaining milliseconds up, clamp elapsed progress.
    let (machine, _) = armed(5, ms(10_000));
    for (elapsed, seconds) in [(0, 5), (999, 5), (1000, 4), (4999, 1), (5000, 0), (9000, 0)] {
        assert_eq!(machine.seconds_left(ms(10_000 + elapsed)), Some(seconds));
        let progress = machine.progress(ms(10_000 + elapsed)).unwrap();
        assert!((0.0..=1.0).contains(&progress));
        let expected = (elapsed as f32 / 5000.0).min(1.0);
        assert!((progress - expected).abs() < 0.000_001, "elapsed={elapsed}");
    }
    assert_eq!(machine.seconds_left(ms(9000)), Some(5));
    assert_eq!(machine.progress(ms(9000)), Some(0.0));
}

#[test]
fn switch_now_skips_the_remaining_countdown_without_forcing_turns() {
    // Plan 7.2 Armed -> TurnCheck on Enter; normal execution still checks live turns.
    let (mut machine, ticket) = armed(30, ms(100));
    assert_eq!(
        machine.handle(Input::SwitchNow, ms(101)),
        vec![execute(&ticket, false, false, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
}

#[test]
fn busy_turns_pause_update_and_resume_without_consuming_paused_time() {
    // Plan 7.2 Armed -> WaitingTurn -> Armed: busy updates never reset the saved remainder.
    let (mut machine, ticket) = armed(5, ms(100));
    let first_busy = vec![busy_thread("one")];
    assert_eq!(
        machine.handle(Input::Turns(first_busy.clone()), ms(1334)),
        vec![]
    );
    let mut countdown = Countdown::start(ms(5000), ms(100));
    countdown.pause(ms(1334));
    assert_state(
        &machine,
        MachineState::WaitingForTurn {
            ticket: ticket.clone(),
            countdown,
            busy: first_busy,
        },
        SwitchPhase::WaitingTurn,
    );
    assert_eq!(machine.seconds_left(ms(90_000)), Some(4));
    let saved_progress = machine.progress(ms(1334));
    assert_eq!(machine.handle(Input::Tick, ms(90_000)), vec![]);
    let updated_busy = vec![busy_thread("two"), busy_thread("three")];
    assert_eq!(
        machine.handle(Input::Turns(updated_busy.clone()), ms(100_000)),
        vec![]
    );
    assert_state(
        &machine,
        MachineState::WaitingForTurn {
            ticket: ticket.clone(),
            countdown,
            busy: updated_busy,
        },
        SwitchPhase::WaitingTurn,
    );
    assert_eq!(machine.progress(ms(100_000)), saved_progress);
    assert_eq!(machine.handle(Input::Turns(vec![]), ms(200_000)), vec![]);
    countdown.resume(ms(200_000));
    let resumed = MachineState::Countdown {
        ticket: ticket.clone(),
        countdown,
    };
    assert_state(&machine, resumed.clone(), SwitchPhase::Armed);
    assert_eq!(machine.progress(ms(200_000)), saved_progress);
    assert_eq!(machine.handle(Input::Tick, ms(203_765)), vec![]);
    assert_state(&machine, resumed, SwitchPhase::Armed);
    assert_eq!(
        machine.handle(Input::Tick, ms(203_766)),
        vec![execute(&ticket, false, false, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
}

#[test]
fn multiple_pause_cycles_preserve_total_active_countdown_time() {
    // Plan 7.2 repeated Armed <-> WaitingTurn: only active time counts toward expiry.
    let (mut machine, ticket) = armed(5, ms(0));
    for (pause_at, resume_at) in [(1000, 10_000), (11_000, 20_000)] {
        assert_eq!(
            machine.handle(Input::Turns(vec![busy_thread("busy")]), ms(pause_at)),
            vec![]
        );
        assert_eq!(machine.handle(Input::Turns(vec![]), ms(resume_at)), vec![]);
    }
    assert_eq!(machine.seconds_left(ms(20_000)), Some(3));
    assert_eq!(machine.handle(Input::Tick, ms(22_999)), vec![]);
    assert_eq!(
        machine.handle(Input::Tick, ms(23_000)),
        vec![execute(&ticket, false, false, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
}

#[test]
fn switch_now_while_waiting_explicitly_forces_busy_turns() {
    // Plan 7.2 WaitingTurn -> TurnCheck: only this SwitchNow path sets force_turns.
    let (mut machine, ticket) = armed(5, ms(0));
    assert_eq!(
        machine.handle(Input::Turns(vec![busy_thread("busy")]), ms(10)),
        vec![]
    );
    assert_eq!(
        machine.handle(Input::SwitchNow, ms(20)),
        vec![execute(&ticket, false, false, true)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
}

fn reachable_states() -> Vec<SwitchMachine> {
    let mut states = vec![SwitchMachine::new(5)];
    let mut planning = SwitchMachine::new(5);
    select(&mut planning, "profile", 1, ms(0));
    states.push(planning.clone());
    let error = Error::other("planning failed");
    assert_eq!(
        planning.handle(
            Input::PlanFailed {
                seq: 1,
                error: error.clone()
            },
            ms(0)
        ),
        vec![]
    );
    assert_state(
        &planning,
        MachineState::Failed {
            ticket: None,
            error,
        },
        SwitchPhase::Done,
    );
    states.push(planning);

    for ticket in [
        blocked_ticket("blocked"),
        PlanTicket {
            choices: 1,
            ..hot_ticket("questions")
        },
        hot_ticket("countdown"),
        PlanTicket {
            busy: vec![busy_thread("busy")],
            ..hot_ticket("waiting")
        },
        full_ticket("confirmation"),
    ] {
        let mut machine = SwitchMachine::new(5);
        assert_eq!(first_plan(&mut machine, &ticket, ms(0)), vec![]);
        states.push(machine);
    }
    let (running, ticket) = running_machine();
    states.push(running.clone());
    let mut done = running.clone();
    assert_eq!(
        done.handle(
            Input::Finished(outcome(&ticket, OutcomeStatus::Switched)),
            ms(2)
        ),
        vec![]
    );
    assert_state(
        &done,
        MachineState::Done {
            ticket: ticket.clone(),
            status: OutcomeStatus::Switched,
        },
        SwitchPhase::Done,
    );
    states.push(done);
    let mut failed = running;
    let error = Error::other("execution failed");
    assert_eq!(
        failed.handle(Input::ExecFailed(error.clone()), ms(2)),
        vec![]
    );
    assert_state(
        &failed,
        MachineState::Failed {
            ticket: Some(ticket),
            error,
        },
        SwitchPhase::Done,
    );
    states.push(failed);
    states
}

#[test]
fn cancel_is_free_in_every_state_except_running_and_never_executes() {
    // Plan 7.2 Esc -> Idle with zero writes: discard tickets, including from result states.
    for mut machine in reachable_states() {
        let before = machine.state().clone();
        let phase = machine.phase();
        let is_running = matches!(before, MachineState::Running { .. });
        assert_eq!(machine.cancel_is_free(), !is_running);
        let expected = if is_running {
            vec![]
        } else {
            machine.ticket().map(discard).into_iter().collect()
        };
        assert_eq!(
            machine.handle(Input::Cancel, ms(5000)),
            expected,
            "before={before:?}"
        );
        if is_running {
            assert_state(&machine, before, phase);
        } else {
            assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
            assert_eq!(machine.handle(Input::Tick, ms(1_000_000)), vec![]);
            assert_eq!(machine.handle(Input::Cancel, ms(1_000_001)), vec![]);
            assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
        }
    }
}

#[test]
fn cancelled_planning_discards_a_late_plan_and_keeps_sequence_monotonic() {
    // Plan 7.2 Planning -> Idle: a late result after Esc must never resurrect its plan.
    let mut machine = SwitchMachine::new(0);
    select(&mut machine, "cancelled", 1, ms(0));
    assert_eq!(machine.handle(Input::Cancel, ms(1)), vec![]);
    let late = hot_ticket("late");
    assert_eq!(
        machine.handle(
            Input::Planned {
                seq: 1,
                ticket: late.clone()
            },
            ms(2)
        ),
        vec![discard(&late)]
    );
    assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
    assert_eq!(
        machine.handle(
            Input::PlanFailed {
                seq: 1,
                error: Error::other("late error")
            },
            ms(3)
        ),
        vec![]
    );
    assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
    select(&mut machine, "new", 2, ms(4));
}

#[test]
fn full_confirmation_explicitly_allows_the_full_restart() {
    // Plan 7.2 ConfirmFull -> TurnCheck: Confirm grants restart permission, not force-turns.
    let (mut machine, ticket) = confirming();
    assert_eq!(
        machine.handle(Input::Confirm, ms(10)),
        vec![execute(&ticket, false, true, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only: false,
        },
        SwitchPhase::TurnCheck,
    );
}

#[test]
fn write_only_confirmation_sets_only_the_write_only_flag() {
    // Plan 7.2 ConfirmFull -> TurnCheck: WriteOnly neither confirms a restart nor forces turns.
    let (mut machine, ticket) = confirming();
    assert_eq!(
        machine.handle(Input::WriteOnly, ms(10)),
        vec![execute(&ticket, true, false, false)]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket: ticket.clone(),
            phase: SwitchPhase::TurnCheck,
            write_only: true,
        },
        SwitchPhase::TurnCheck,
    );
    assert_eq!(
        machine.handle(
            Input::Event(SwitchEvent::Phase {
                phase: SwitchPhase::Applying,
                at: Timestamp(123)
            }),
            ms(11)
        ),
        vec![]
    );
    assert_state(
        &machine,
        MachineState::Running {
            ticket,
            phase: SwitchPhase::Applying,
            write_only: true,
        },
        SwitchPhase::Applying,
    );
}

#[test]
fn full_confirmation_cannot_be_bypassed_by_ticks_or_switch_now() {
    // Plan 7.2 ConfirmFull has no countdown; Enter/SwitchNow cannot authorize a full restart.
    let (mut machine, ticket) = confirming();
    for input in [
        Input::Tick,
        Input::SwitchNow,
        Input::Turns(vec![busy_thread("busy")]),
        Input::Turns(vec![]),
    ] {
        assert_eq!(machine.handle(input, ms(1_000_000)), vec![]);
        assert_state(
            &machine,
            MachineState::ConfirmFull {
                ticket: ticket.clone(),
            },
            SwitchPhase::ConfirmFull,
        );
    }
}

#[test]
fn reselection_discards_the_old_ticket_before_planning_and_restarts_the_full_timer() {
    // Plan 7.2 Armed/ConfirmFull/WaitingTurn -> Planning: latest wins with a new full countdown.
    let (countdown, _) = armed(5, ms(0));
    let (confirmation, _) = confirming();
    let mut waiting = countdown.clone();
    assert_eq!(
        waiting.handle(Input::Turns(vec![busy_thread("busy")]), ms(100)),
        vec![]
    );
    for mut machine in [countdown, confirmation, waiting] {
        let old = machine.ticket().unwrap().clone();
        let next = request("replacement");
        assert_eq!(
            machine.handle(Input::Select(next.clone()), ms(4999)),
            vec![
                discard(&old),
                Effect::Plan {
                    seq: 2,
                    request: next
                }
            ]
        );
        assert_state(
            &machine,
            MachineState::Planning { seq: 2 },
            SwitchPhase::Planning,
        );
        assert_eq!(
            machine.handle(
                Input::Planned {
                    seq: 1,
                    ticket: old.clone()
                },
                ms(5000)
            ),
            vec![discard(&old)]
        );
        assert_state(
            &machine,
            MachineState::Planning { seq: 2 },
            SwitchPhase::Planning,
        );
        let new = hot_ticket("replacement-plan");
        assert_eq!(
            machine.handle(
                Input::Planned {
                    seq: 2,
                    ticket: new.clone()
                },
                ms(90_000)
            ),
            vec![]
        );
        let expected = MachineState::Countdown {
            ticket: new.clone(),
            countdown: Countdown::start(ms(5000), ms(90_000)),
        };
        assert_state(&machine, expected.clone(), SwitchPhase::Armed);
        assert_eq!(machine.seconds_left(ms(90_000)), Some(5));
        assert_eq!(machine.handle(Input::Tick, ms(94_999)), vec![]);
        assert_state(&machine, expected, SwitchPhase::Armed);
        assert_eq!(
            machine.handle(Input::Tick, ms(95_000)),
            vec![execute(&new, false, false, false)]
        );
        assert_state(
            &machine,
            MachineState::Running {
                ticket: new,
                phase: SwitchPhase::TurnCheck,
                write_only: false,
            },
            SwitchPhase::TurnCheck,
        );
    }
}

#[test]
fn blocked_questions_and_results_can_be_reselected() {
    // Plan 7.2 Blocked/NeedsAnswers/Done/Failed -> Planning: re-plan answers or retry a failure.
    for mut machine in reachable_states().into_iter().filter(|m| {
        matches!(
            m.state(),
            MachineState::Blocked { .. }
                | MachineState::NeedsAnswers { .. }
                | MachineState::Done { .. }
                | MachineState::Failed { .. }
        )
    }) {
        let mut expected: Vec<_> = machine.ticket().map(discard).into_iter().collect();
        let next = request("retry");
        expected.push(Effect::Plan {
            seq: 2,
            request: next.clone(),
        });
        assert_eq!(machine.handle(Input::Select(next), ms(10)), expected);
        assert_state(
            &machine,
            MachineState::Planning { seq: 2 },
            SwitchPhase::Planning,
        );
    }
}

#[test]
fn running_mirrors_each_phase_event_without_emitting_effects() {
    // Plan 7.2 Running mirrors executor progress, including Recovery/RollingBack/Done phases.
    let (mut machine, ticket) = running_machine();
    for phase in messages::ALL_PHASES {
        assert_eq!(
            machine.handle(
                Input::Event(SwitchEvent::Phase {
                    phase,
                    at: Timestamp(100)
                }),
                ms(100)
            ),
            vec![]
        );
        assert_state(
            &machine,
            MachineState::Running {
                ticket: ticket.clone(),
                phase,
                write_only: false,
            },
            phase,
        );
    }
}

#[test]
fn nonphase_events_never_change_ui_state() {
    // Plan 7.2: executor Step/Turns/Log notifications do not drive UI transitions.
    let events = [
        SwitchEvent::Step {
            step: SwitchStep::WriteConfig,
            status: StepStatus::Done,
            at: Timestamp(0),
        },
        SwitchEvent::Turns {
            activity: vec![],
            waited_ms: 1500,
        },
        SwitchEvent::Log {
            level: Severity::Warning,
            text: LocalizedText::new("test.log", "A warning"),
        },
    ];
    for mut machine in reachable_states() {
        let before = machine.state().clone();
        let phase = machine.phase();
        for event in &events {
            assert_eq!(
                machine.handle(Input::Event(event.clone()), ms(10_000)),
                vec![]
            );
            assert_state(&machine, before.clone(), phase);
        }
    }
}

#[test]
fn select_and_cancel_are_ignored_until_running_finishes() {
    // Plan 7.2 Running cannot be replaced or cancelled by the pre-execution controls.
    let (mut machine, ticket) = running_machine();
    for input in [
        Input::Select(request("ignored")),
        Input::Cancel,
        Input::Confirm,
        Input::WriteOnly,
        Input::SwitchNow,
        Input::Turns(vec![busy_thread("busy")]),
    ] {
        assert_eq!(machine.handle(input, ms(10)), vec![]);
        assert_state(
            &machine,
            MachineState::Running {
                ticket: ticket.clone(),
                phase: SwitchPhase::TurnCheck,
                write_only: false,
            },
            SwitchPhase::TurnCheck,
        );
    }
    assert_eq!(
        machine.handle(
            Input::Finished(outcome(&ticket, OutcomeStatus::Switched)),
            ms(11)
        ),
        vec![]
    );
    assert_eq!(
        machine.handle(Input::Dismiss, ms(12)),
        vec![discard(&ticket)]
    );
    select(&mut machine, "next", 2, ms(13));
}

#[test]
fn finished_copies_every_outcome_status_and_keeps_the_ticket_until_dismissed() {
    // Plan 7.2 Running -> Done -> Idle: the outcome status is preserved, then the ticket is freed.
    for status in [
        OutcomeStatus::Switched,
        OutcomeStatus::WrittenOnly,
        OutcomeStatus::Cancelled,
        OutcomeStatus::RolledBack,
        OutcomeStatus::Partial,
        OutcomeStatus::Failed,
    ] {
        let (mut machine, ticket) = running_machine();
        assert_eq!(
            machine.handle(Input::Finished(outcome(&ticket, status)), ms(10)),
            vec![]
        );
        assert_state(
            &machine,
            MachineState::Done {
                ticket: ticket.clone(),
                status,
            },
            SwitchPhase::Done,
        );
        assert_eq!(
            machine.handle(Input::Dismiss, ms(11)),
            vec![discard(&ticket)]
        );
        assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
        assert_eq!(machine.handle(Input::Dismiss, ms(12)), vec![]);
    }
}

#[test]
fn execution_failure_retains_error_and_ticket_until_dismissed() {
    // Plan 7.2 Running -> Failed -> Idle: show the exact execution error, then discard once.
    let (mut machine, ticket) = running_machine();
    let error = Error::other("apply failed");
    assert_eq!(
        machine.handle(Input::ExecFailed(error.clone()), ms(10)),
        vec![]
    );
    assert_state(
        &machine,
        MachineState::Failed {
            ticket: Some(ticket.clone()),
            error,
        },
        SwitchPhase::Done,
    );
    assert_eq!(
        machine.handle(Input::Dismiss, ms(11)),
        vec![discard(&ticket)]
    );
    assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
}

#[test]
fn dismiss_only_leaves_blocked_done_or_failed() {
    // Plan 7.2 Dismiss is a result-dialog action, never a substitute for Cancel while armed.
    for mut machine in reachable_states() {
        let before = machine.state().clone();
        let phase = machine.phase();
        let dismissible = matches!(
            before,
            MachineState::Blocked { .. } | MachineState::Done { .. } | MachineState::Failed { .. }
        );
        let effects = if dismissible {
            machine.ticket().map(discard).into_iter().collect()
        } else {
            vec![]
        };
        assert_eq!(machine.handle(Input::Dismiss, ms(10_000)), effects);
        if dismissible {
            assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
        } else {
            assert_state(&machine, before, phase);
        }
    }
}

#[test]
fn execution_notifications_are_ignored_outside_running() {
    // Plan 7.2: delayed executor notifications cannot turn an idle/planning/result UI into Done.
    for mut machine in reachable_states()
        .into_iter()
        .filter(|m| !matches!(m.state(), MachineState::Running { .. }))
    {
        let before = machine.state().clone();
        let phase = machine.phase();
        for input in [
            Input::Event(SwitchEvent::Phase {
                phase: SwitchPhase::Applying,
                at: Timestamp(0),
            }),
            Input::Finished(outcome(&hot_ticket("old"), OutcomeStatus::Switched)),
            Input::ExecFailed(Error::other("late failure")),
        ] {
            assert_eq!(machine.handle(input, ms(10_000)), vec![]);
            assert_state(&machine, before.clone(), phase);
        }
    }
}

#[test]
fn stale_plan_results_and_failures_do_not_mutate_any_reachable_state() {
    // Plan 7.2: only matching Planning accepts results; other states discard late plans.
    for mut machine in reachable_states() {
        let before = machine.state().clone();
        let phase = machine.phase();
        let stale = full_ticket("stale");
        assert_eq!(
            machine.handle(
                Input::Planned {
                    seq: 0,
                    ticket: stale.clone()
                },
                ms(10_000)
            ),
            vec![discard(&stale)]
        );
        assert_state(&machine, before.clone(), phase);
        assert_eq!(
            machine.handle(
                Input::PlanFailed {
                    seq: 0,
                    error: Error::other("stale")
                },
                ms(10_001)
            ),
            vec![]
        );
        assert_state(&machine, before, phase);
    }
}

#[test]
fn controls_without_a_transition_are_noops_in_every_reachable_state() {
    // Plan 7.2 transition guards: each execution control works only in its designated state.
    for machine in reachable_states() {
        for input in [
            Input::Tick,
            Input::SwitchNow,
            Input::Confirm,
            Input::WriteOnly,
            Input::Turns(vec![]),
            Input::Turns(vec![busy_thread("busy")]),
        ] {
            let applicable = matches!(
                (&input, machine.state()),
                (
                    Input::Tick | Input::SwitchNow,
                    MachineState::Countdown { .. }
                ) | (Input::SwitchNow, MachineState::WaitingForTurn { .. })
                    | (
                        Input::Confirm | Input::WriteOnly,
                        MachineState::ConfirmFull { .. }
                    )
                    | (
                        Input::Turns(_),
                        MachineState::Countdown { .. } | MachineState::WaitingForTurn { .. }
                    )
            );
            if !applicable {
                let mut probe = machine.clone();
                assert_eq!(probe.handle(input, ms(100_000)), vec![]);
                assert_state(&probe, machine.state().clone(), machine.phase());
            }
        }
    }
    let (mut machine, _) = armed(5, ms(0));
    let before = machine.state().clone();
    assert_eq!(machine.handle(Input::Turns(vec![]), ms(100)), vec![]);
    assert_state(&machine, before, SwitchPhase::Armed);
}

#[test]
fn every_configured_countdown_is_clamped_to_thirty_seconds() {
    // Plan 7.2 Armed configuration: 0 is immediate and all values above 30 saturate at 30.
    assert_eq!(machine::MAX_COUNTDOWN_SECS, 30);
    for secs in 0..=u8::MAX {
        let expected = Duration::from_secs(u64::from(secs).min(30));
        assert_eq!(machine::countdown_duration(secs), expected);
        assert_eq!(SwitchMachine::new(secs).countdown_len(), expected);
    }
}

#[test]
fn changing_countdown_length_affects_only_the_next_active_or_paused_timer() {
    // Plan 7.2 Armed/WaitingTurn: settings changes cannot shorten a countdown already shown.
    for paused in [false, true] {
        for configured in [0, 2, u8::MAX] {
            let (mut machine, old) = armed(5, ms(100));
            if paused {
                assert_eq!(
                    machine.handle(Input::Turns(vec![busy_thread("busy")]), ms(1100)),
                    vec![]
                );
            }
            let before = machine.state().clone();
            let phase = machine.phase();
            machine.set_countdown_secs(configured);
            assert_eq!(
                machine.countdown_len(),
                Duration::from_secs(u64::from(configured).min(30))
            );
            assert_state(&machine, before, phase);
            assert_eq!(machine.seconds_left(ms(1100)), Some(4));
            let next = request("new-duration");
            assert_eq!(
                machine.handle(Input::Select(next.clone()), ms(1101)),
                vec![
                    discard(&old),
                    Effect::Plan {
                        seq: 2,
                        request: next
                    }
                ]
            );
            let new = hot_ticket("new-duration");
            let effects = machine.handle(
                Input::Planned {
                    seq: 2,
                    ticket: new.clone(),
                },
                ms(2000),
            );
            if configured == 0 {
                assert_eq!(effects, vec![execute(&new, false, false, false)]);
                assert_state(
                    &machine,
                    MachineState::Running {
                        ticket: new,
                        phase: SwitchPhase::TurnCheck,
                        write_only: false,
                    },
                    SwitchPhase::TurnCheck,
                );
            } else {
                assert_eq!(effects, vec![]);
                assert_state(
                    &machine,
                    MachineState::Countdown {
                        ticket: new,
                        countdown: Countdown::start(
                            Duration::from_secs(u64::from(configured).min(30)),
                            ms(2000),
                        ),
                    },
                    SwitchPhase::Armed,
                );
                assert_eq!(
                    machine.seconds_left(ms(2000)),
                    Some(u64::from(configured).min(30))
                );
            }
        }
    }
}

struct Rng(u64);

impl Rng {
    fn pick(&mut self, upper: u64) -> u64 {
        // Fixed xorshift64 sequence: reproducible tests without random-device IO or dependencies.
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % upper
    }

    fn ticket(&mut self, id: &str) -> PlanTicket {
        match self.pick(6) {
            0 => blocked_ticket(id),
            1 => PlanTicket {
                choices: 1,
                ..hot_ticket(id)
            },
            2 => full_ticket(id),
            3 => PlanTicket {
                busy: vec![busy_thread("busy")],
                ..hot_ticket(id)
            },
            4 => PlanTicket {
                primary: RestartKind::None,
                countdown_allowed: false,
                ..hot_ticket(id)
            },
            _ => hot_ticket(id),
        }
    }
}

fn permitted_execution(
    state: &MachineState,
    input: &Input,
    now: Duration,
    countdown_len: Duration,
) -> Option<(&'static str, Effect)> {
    // Independent specification of the only five inputs allowed to request execution.
    match (state, input) {
        (MachineState::Countdown { ticket, countdown }, Input::Tick)
            if countdown.remaining(now).is_zero() =>
        {
            Some(("tick", execute(ticket, false, false, false)))
        }
        (MachineState::Countdown { ticket, .. }, Input::SwitchNow) => {
            Some(("switch_now", execute(ticket, false, false, false)))
        }
        (MachineState::WaitingForTurn { ticket, .. }, Input::SwitchNow) => {
            Some(("force_turns", execute(ticket, false, false, true)))
        }
        (MachineState::ConfirmFull { ticket }, Input::Confirm) => {
            Some(("confirm", execute(ticket, false, true, false)))
        }
        (MachineState::ConfirmFull { ticket }, Input::WriteOnly) => {
            Some(("write_only", execute(ticket, true, false, false)))
        }
        (MachineState::Planning { seq: current }, Input::Planned { seq, ticket })
            if seq == current
                && !matches!(ticket.primary, RestartKind::Blocked { .. })
                && ticket.choices == 0
                && !ticket.needs_confirmation
                && (!ticket.countdown_allowed || countdown_len.is_zero()) =>
        {
            Some(("immediate_plan", execute(ticket, false, false, false)))
        }
        _ => None,
    }
}

#[test]
fn deterministic_input_sequences_preserve_execution_and_cancellation_invariants() {
    // Plan 7.2: arbitrary interleavings can execute only at a documented transition; Esc is free.
    let mut execution_paths = BTreeSet::new();
    let mut cancellations = 0;
    for seed in 1..=64 {
        let mut rng = Rng(seed);
        let mut machine = SwitchMachine::new((seed % 8) as u8);
        let mut now = Duration::ZERO;
        for index in 0..512 {
            now += ms(rng.pick(10_000));
            let seq = match machine.state() {
                MachineState::Planning { seq } => *seq,
                _ => 0,
            };
            let input = match rng.pick(20) {
                0 | 1 => Input::Select(request(&format!("profile-{index}"))),
                2..=4 => Input::Planned {
                    seq,
                    ticket: rng.ticket(&format!("plan-{seed}-{index}")),
                },
                5 => Input::Planned {
                    seq: seq.saturating_sub(1),
                    ticket: rng.ticket("stale"),
                },
                6 | 7 => Input::Tick,
                8 => Input::SwitchNow,
                9 => Input::Confirm,
                10 => Input::WriteOnly,
                11 => Input::Cancel,
                12 => Input::Turns(vec![busy_thread("busy")]),
                13 => Input::Turns(vec![]),
                14 => Input::Event(SwitchEvent::Phase {
                    phase: SwitchPhase::Verifying,
                    at: Timestamp(42),
                }),
                15 => Input::Finished(outcome(&hot_ticket("result"), OutcomeStatus::Switched)),
                16 => Input::ExecFailed(Error::other("execution failure")),
                17 => Input::PlanFailed {
                    seq,
                    error: Error::other("planning failure"),
                },
                18 => Input::Dismiss,
                _ => Input::Event(SwitchEvent::Step {
                    step: SwitchStep::WriteConfig,
                    status: StepStatus::Done,
                    at: Timestamp(42),
                }),
            };
            let before = machine.state().clone();
            let expected = permitted_execution(&before, &input, now, machine.countdown_len());
            let cancellation =
                matches!(input, Input::Cancel) && !matches!(before, MachineState::Running { .. });
            let discard_on_cancel: Vec<_> = machine.ticket().map(discard).into_iter().collect();
            let effects = machine.handle(input.clone(), now);
            let actual: Vec<_> = effects
                .iter()
                .filter(|e| matches!(e, Effect::Execute { .. }))
                .cloned()
                .collect();
            assert_eq!(
                actual,
                expected
                    .as_ref()
                    .map(|(_, e)| e.clone())
                    .into_iter()
                    .collect::<Vec<_>>(),
                "seed={seed}, index={index}, before={before:?}, input={input:?}, now={now:?}"
            );
            if let Some((path, effect)) = expected {
                execution_paths.insert(path);
                assert_eq!(effects, vec![effect.clone()]);
                let Effect::Execute {
                    plan_id,
                    write_only,
                    ..
                } = effect
                else {
                    unreachable!()
                };
                assert!(
                    matches!(machine.state(), MachineState::Running { ticket, phase: SwitchPhase::TurnCheck, write_only: actual_write_only } if ticket.plan_id == plan_id && *actual_write_only == write_only)
                );
                assert_eq!(machine.phase(), SwitchPhase::TurnCheck);
                assert!(!machine.cancel_is_free());
            }
            if cancellation {
                cancellations += 1;
                assert_eq!(effects, discard_on_cancel);
                assert_state(&machine, MachineState::Idle, SwitchPhase::Idle);
            }
        }
    }
    assert_eq!(
        execution_paths,
        BTreeSet::from([
            "tick",
            "switch_now",
            "force_turns",
            "confirm",
            "write_only",
            "immediate_plan"
        ])
    );
    assert!(
        cancellations > 100,
        "the generated sequences must exercise cancellation"
    );
}
