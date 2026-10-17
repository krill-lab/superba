#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;
use superba_core::{
    ActivitySignal, AuthMode, BlockReason, Decision, DecisionRequest, Error, ExecuteOptions,
    FullRestartReason, LegacySessionChoice, LocalizedText, OutcomeStatus, PlanAlternative,
    PlanChoice, PlanDestination, ProviderRestore, RcLine, RestartKind, RestoreTarget, StepStatus,
    SwitchEvent, SwitchIntent, SwitchOutcome, SwitchPhase, SwitchPlan, SwitchRequest, SwitchStep,
    ThreadActivity, Timestamp, TurnActivity, Verification,
};
use superba_gui::flow::{Flow, FlowAttention, FlowNotice, Stage};
use superba_gui::runtime::{Command, PendingDecision};
use tokio::sync::oneshot;

fn hot_plan() -> SwitchPlan {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    SwitchPlan {
        id: format!("flow-plan-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)),
        created_at: Timestamp::EPOCH,
        request: SwitchRequest::profile("krill".into()),
        from: None,
        to: PlanDestination {
            profile_id: Some("krill".into()),
            site_id: Some("krill".into()),
            line_id: Some("cdn".into()),
            auth_mode: Some(AuthMode::Pat),
            label: LocalizedText::new("test.destination", "Krill CDN"),
        },
        primary: RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
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

fn full_plan() -> SwitchPlan {
    SwitchPlan {
        primary: RestartKind::FullAppRestart {
            reason: FullRestartReason::CrossAuthFamily,
        },
        ..hot_plan()
    }
}

fn busy_thread(target: &str, thread: &str) -> ThreadActivity {
    ThreadActivity {
        target_id: target.into(),
        thread_id: Some(thread.into()),
        title: Some(format!("Thread {thread}")),
        rollout_path: None,
        last_event_at: Some(Timestamp::from_millis(42)),
        signal: ActivitySignal::RolloutTail,
    }
}

fn busy_plan() -> SwitchPlan {
    SwitchPlan {
        active_threads: vec![busy_thread("local", "one")],
        ..hot_plan()
    }
}

fn wait_question() -> DecisionRequest {
    DecisionRequest::WaitForTurns {
        busy: vec![
            busy_thread("local", "one"),
            busy_thread("ssh:devbox", "two"),
            busy_thread("local", "three"),
        ],
        waited_ms: 1500,
    }
}

fn take_plan(flow: &mut Flow) -> (u64, SwitchRequest) {
    match flow.take_commands().as_slice() {
        [Command::Plan { seq, request }] => (*seq, request.clone()),
        commands => panic!("expected exactly one Plan command, got {commands:?}"),
    }
}

fn take_execute(flow: &mut Flow) -> (SwitchPlan, ExecuteOptions) {
    match flow.take_commands().as_slice() {
        [Command::Execute { plan, options }] => ((**plan).clone(), options.clone()),
        commands => panic!("expected exactly one Execute command, got {commands:?}"),
    }
}

fn planned_flow(seconds: u8, plan: SwitchPlan) -> Flow {
    let mut flow = Flow::new(seconds);
    assert!(flow.request(plan.request.clone(), Duration::ZERO));
    assert_eq!(take_plan(&mut flow), (1, plan.request.clone()));
    flow.on_planned(1, Ok(plan), Duration::ZERO);
    flow
}

fn running_flow() -> Flow {
    let mut flow = planned_flow(0, hot_plan());
    take_execute(&mut flow);
    assert!(matches!(flow.stage(), Stage::Running(_)));
    flow
}

fn outcome(plan: &SwitchPlan) -> SwitchOutcome {
    SwitchOutcome {
        plan_id: plan.id.clone(),
        status: OutcomeStatus::Switched,
        elapsed_ms: 1200,
        generation: Some(8),
        restart: plan.primary.clone(),
        verification: None,
        remotes: vec![],
        stale_processes: vec![],
        returned_to: None,
        notices: vec![],
    }
}

#[test]
fn default_flow_is_idle_and_has_no_pending_work() {
    let mut flow = Flow::default();
    assert!(matches!(flow.stage(), Stage::Idle));
    assert_eq!(flow.machine().countdown_len(), Duration::from_secs(5));
    assert!(!flow.is_running());
    assert!(!flow.is_planning());
    assert!(flow.run().is_none());
    assert!(flow.current_plan().is_none());
    assert!(flow.last_request().is_none());
    assert!(flow.destination().is_none());
    assert_eq!(flow.stored_plans(), 0);
    assert!(flow.take_commands().is_empty());
    assert!(flow.take_notices().is_empty());
    assert!(!flow.cancel(Duration::ZERO));
}

#[test]
fn request_emits_one_plan_command_and_enters_planning() {
    let mut flow = Flow::default();
    let request = SwitchRequest::profile("krill".into());
    assert!(flow.request(request.clone(), Duration::from_secs(10)));
    assert!(matches!(flow.stage(), Stage::Planning));
    assert!(flow.is_planning());
    assert_eq!(flow.last_request(), Some(&request));
    assert_eq!(take_plan(&mut flow), (1, request));
    assert!(flow.take_commands().is_empty(), "commands are drained");
    assert_eq!(flow.stored_plans(), 0);
}

#[test]
fn hot_countdown_executes_once_at_exactly_five_seconds() {
    let plan = hot_plan();
    let mut flow = planned_flow(5, plan.clone());
    let Stage::Armed { countdown, .. } = flow.stage() else {
        panic!("hot plan should arm the countdown");
    };
    assert_eq!(countdown.remaining(Duration::ZERO), Duration::from_secs(5));
    assert!(!countdown.is_paused());
    assert_eq!(flow.destination(), Some(&plan.to.label));
    assert!(flow.take_commands().is_empty());

    flow.tick(Duration::from_millis(4900));
    assert!(matches!(flow.stage(), Stage::Armed { .. }));
    assert!(flow.take_commands().is_empty());
    flow.tick(Duration::from_secs(5));
    let (executed, options) = take_execute(&mut flow);
    assert_eq!(executed.id, plan.id);
    assert_eq!(options, ExecuteOptions::default());
    assert!(matches!(flow.stage(), Stage::Running(_)));
    assert_eq!(flow.run().unwrap().started, Duration::from_secs(5));
    assert_eq!(flow.stored_plans(), 1);
    flow.tick(Duration::from_secs(5));
    flow.tick(Duration::from_secs(50));
    assert!(flow.take_commands().is_empty(), "execution must not repeat");
}

#[test]
fn cancelling_armed_plan_discards_it_without_execution() {
    let mut flow = planned_flow(5, hot_plan());
    assert!(flow.take_commands().is_empty());
    assert!(flow.cancel(Duration::from_secs(2)));
    assert!(matches!(flow.stage(), Stage::Idle));
    assert_eq!(flow.stored_plans(), 0);
    assert!(flow.run().is_none());
    flow.tick(Duration::from_secs(5));
    flow.switch_now(Duration::from_secs(60));
    flow.tick(Duration::from_secs(600));
    assert!(
        flow.take_commands().is_empty(),
        "cancel must produce zero writes"
    );
    assert!(!flow.cancel(Duration::from_secs(600)));
}

#[test]
fn reselection_discards_old_and_late_plans_and_restarts_countdown() {
    let mut flow = planned_flow(5, hot_plan());
    let next = SwitchPlan {
        request: SwitchRequest::profile("other".into()),
        ..hot_plan()
    };
    assert!(flow.request(next.request.clone(), Duration::from_secs(4)));
    assert_eq!(take_plan(&mut flow), (2, next.request.clone()));
    assert!(matches!(flow.stage(), Stage::Planning));
    assert_eq!(flow.stored_plans(), 0);
    for _ in 0..8 {
        flow.on_planned(1, Ok(hot_plan()), Duration::from_secs(4));
        assert!(matches!(flow.stage(), Stage::Planning));
        assert_eq!(flow.stored_plans(), 0);
        assert!(flow.take_commands().is_empty());
    }
    flow.on_planned(2, Ok(next.clone()), Duration::from_secs(5));
    flow.on_planned(1, Ok(hot_plan()), Duration::from_secs(6));
    assert_eq!(flow.stored_plans(), 1);
    assert_eq!(flow.current_plan().unwrap().id, next.id);
    flow.tick(Duration::from_millis(9900));
    assert!(flow.take_commands().is_empty());
    flow.tick(Duration::from_secs(10));
    assert_eq!(take_execute(&mut flow).0.id, next.id);
    assert!(flow.take_notices().is_empty());
}

#[test]
fn cancelled_planning_discards_a_late_reply() {
    let mut flow = Flow::new(0);
    let plan = hot_plan();
    flow.request(plan.request.clone(), Duration::ZERO);
    take_plan(&mut flow);
    assert!(flow.cancel(Duration::from_secs(1)));
    flow.on_planned(1, Ok(plan), Duration::from_secs(2));
    assert!(matches!(flow.stage(), Stage::Idle));
    assert_eq!(flow.stored_plans(), 0);
    assert!(flow.take_commands().is_empty());
}

#[test]
fn zero_countdown_executes_on_plan_arrival() {
    let plan = hot_plan();
    let mut flow = planned_flow(0, plan.clone());
    assert!(matches!(flow.stage(), Stage::Running(_)));
    let (executed, options) = take_execute(&mut flow);
    assert_eq!(executed.id, plan.id);
    assert_eq!(options, ExecuteOptions::default());
    assert_eq!(flow.run().unwrap().started, Duration::ZERO);
}

#[test]
fn full_restart_waits_for_confirmation_even_with_zero_countdown() {
    for seconds in [0, 5] {
        let plan = full_plan();
        let mut flow = planned_flow(seconds, plan.clone());
        assert!(matches!(flow.stage(), Stage::ConfirmFull { .. }));
        for now in [
            Duration::ZERO,
            Duration::from_secs(5),
            Duration::from_secs(3600),
        ] {
            flow.tick(now);
            assert!(matches!(flow.stage(), Stage::ConfirmFull { .. }));
            assert!(flow.take_commands().is_empty());
        }
        flow.confirm_full(false, Duration::from_secs(3601));
        let (executed, options) = take_execute(&mut flow);
        assert_eq!(executed.id, plan.id);
        assert_eq!(
            options,
            ExecuteOptions {
                allow_full_restart: true,
                ..ExecuteOptions::default()
            }
        );
        assert!(matches!(flow.stage(), Stage::Running(_)));
    }
}

#[test]
fn full_restart_can_write_only_without_permission_to_restart() {
    let plan = full_plan();
    let mut flow = planned_flow(5, plan.clone());
    assert!(flow.take_commands().is_empty());
    flow.confirm_full(true, Duration::from_secs(1));
    let (executed, options) = take_execute(&mut flow);
    assert_eq!(executed.id, plan.id);
    assert_eq!(
        options,
        ExecuteOptions {
            write_only: true,
            ..ExecuteOptions::default()
        }
    );
    assert!(matches!(flow.stage(), Stage::Running(_)));
}

#[test]
fn blocked_plan_replans_the_selected_alternative() {
    let alternative = SwitchRequest::profile("official".into());
    let plan = SwitchPlan {
        primary: RestartKind::Blocked {
            reasons: vec![BlockReason::MissingKey],
        },
        alternatives: vec![PlanAlternative {
            label: LocalizedText::new("test.alternative", "Use official login"),
            request: alternative.clone(),
        }],
        ..hot_plan()
    };
    let mut flow = planned_flow(0, plan);
    assert!(matches!(flow.stage(), Stage::Blocked { .. }));
    flow.tick(Duration::from_secs(60));
    flow.switch_now(Duration::from_secs(60));
    flow.take_alternative(1, Duration::from_secs(60));
    assert!(matches!(flow.stage(), Stage::Blocked { .. }));
    assert!(flow.take_commands().is_empty());
    flow.take_alternative(0, Duration::from_secs(61));
    assert_eq!(take_plan(&mut flow), (2, alternative));
    assert!(matches!(flow.stage(), Stage::Planning));
    assert_eq!(flow.stored_plans(), 0);
}

#[test]
fn legacy_and_model_answers_replan_and_preserve_previous_answers() {
    let model_choice = PlanChoice::ReplacementModel {
        current: "gpt-old".into(),
        suggestions: vec!["gpt-x".into()],
    };
    let plan = SwitchPlan {
        choices: vec![
            PlanChoice::LegacySessions {
                providers: BTreeMap::from([("krill".into(), 3)]),
            },
            model_choice.clone(),
        ],
        ..hot_plan()
    };
    let mut expected = plan.request.clone();
    let mut flow = planned_flow(0, plan);
    assert!(matches!(flow.stage(), Stage::Choose { .. }));
    flow.tick(Duration::from_secs(60));
    assert!(flow.take_commands().is_empty());
    flow.answer_legacy_sessions(LegacySessionChoice::Migrate, Duration::from_secs(61));
    expected.legacy_sessions = Some(LegacySessionChoice::Migrate);
    assert_eq!(take_plan(&mut flow), (2, expected.clone()));
    assert_eq!(flow.stored_plans(), 0);
    let replanned = SwitchPlan {
        request: expected.clone(),
        choices: vec![model_choice],
        ..hot_plan()
    };
    flow.on_planned(2, Ok(replanned), Duration::from_secs(62));
    assert!(matches!(flow.stage(), Stage::Choose { .. }));
    flow.answer_model("gpt-x".into(), Duration::from_secs(63));
    expected.model_override = Some("gpt-x".into());
    assert_eq!(take_plan(&mut flow), (3, expected));
    assert!(matches!(flow.stage(), Stage::Planning));
    assert_eq!(flow.stored_plans(), 0);
}

#[test]
fn restore_provider_answer_updates_the_restore_intent() {
    for provider in [
        ProviderRestore::KeepOpenai,
        ProviderRestore::Baseline {
            reverse_migrate: true,
        },
        ProviderRestore::Baseline {
            reverse_migrate: false,
        },
    ] {
        let mut plan = full_plan();
        plan.request.intent = SwitchIntent::Restore {
            target: RestoreTarget::Official,
        };
        plan.choices.push(PlanChoice::RestoreProvider {
            baseline_provider: "krill".into(),
            created_during: 2,
            migrated_in: 3,
        });
        let mut expected = plan.request.clone();
        let mut flow = planned_flow(5, plan);
        assert!(matches!(flow.stage(), Stage::Choose { .. }));
        flow.answer_restore_provider(provider, Duration::from_secs(1));
        expected.intent = SwitchIntent::Restore {
            target: RestoreTarget::Original { provider },
        };
        assert_eq!(take_plan(&mut flow), (2, expected));
    }
}

#[test]
fn accepting_risks_answers_only_the_corresponding_request_fields() {
    let foreign = PlanChoice::ForeignEnv {
        lines: vec![RcLine {
            path: "/home/user/.bashrc".into(),
            line_no: 4,
            text: "export CODEX_AUTHAPI_BASE_URL=https://relay.example.com".into(),
            value: "https://relay.example.com".into(),
        }],
    };
    let version = PlanChoice::UnknownVersion {
        consumer: "devbox".into(),
    };
    let shared = PlanChoice::SharedHome {
        targets: vec!["ssh:one".into(), "ssh:two".into()],
    };
    for (choices, take_over, unknown, shared_home) in [
        (vec![foreign.clone()], Some(true), false, false),
        (vec![version.clone()], None, true, false),
        (vec![shared.clone()], None, false, true),
        (vec![foreign, version, shared], Some(true), true, true),
    ] {
        let plan = SwitchPlan {
            choices: choices.clone(),
            ..hot_plan()
        };
        let mut expected = plan.request.clone();
        let mut flow = planned_flow(5, plan);
        assert!(matches!(flow.stage(), Stage::Choose { .. }));
        flow.accept_risks(&choices, Duration::from_secs(1));
        expected.take_over_foreign_env = take_over;
        expected.accept_unknown_versions = unknown;
        expected.accept_shared_home = shared_home;
        assert_eq!(take_plan(&mut flow), (2, expected));
    }
}

#[test]
fn plan_without_restart_executes_immediately() {
    let plan = SwitchPlan {
        primary: RestartKind::None,
        ..hot_plan()
    };
    let mut flow = planned_flow(30, plan.clone());
    assert!(matches!(flow.stage(), Stage::Running(_)));
    let (executed, options) = take_execute(&mut flow);
    assert_eq!(executed.id, plan.id);
    assert_eq!(executed.primary, RestartKind::None);
    assert_eq!(options, ExecuteOptions::default());
}

#[test]
fn busy_plan_pauses_countdown_until_wait_is_acknowledged() {
    let plan = busy_plan();
    let mut flow = planned_flow(5, plan.clone());
    let Stage::WaitingBeforeSwitch {
        countdown, busy, ..
    } = flow.stage()
    else {
        panic!("busy plan should pause before switching");
    };
    assert_eq!(busy, plan.active_threads);
    assert!(countdown.is_paused());
    assert_eq!(
        countdown.remaining(Duration::from_secs(40)),
        Duration::from_secs(5)
    );
    flow.tick(Duration::from_secs(40));
    assert!(flow.take_commands().is_empty());
    flow.acknowledge_wait(Duration::from_secs(40));
    let Stage::Armed { countdown, .. } = flow.stage() else {
        panic!("acknowledging wait should resume the countdown");
    };
    assert!(!countdown.is_paused());
    flow.tick(Duration::from_millis(44900));
    assert!(flow.take_commands().is_empty());
    flow.tick(Duration::from_secs(45));
    let (executed, options) = take_execute(&mut flow);
    assert_eq!(executed.id, plan.id);
    assert_eq!(options, ExecuteOptions::default());
    assert!(matches!(flow.stage(), Stage::Running(_)));
}

#[test]
fn switching_now_from_busy_plan_forces_turns() {
    let mut flow = planned_flow(5, busy_plan());
    assert!(matches!(flow.stage(), Stage::WaitingBeforeSwitch { .. }));
    flow.switch_now(Duration::from_secs(1));
    assert_eq!(
        take_execute(&mut flow).1,
        ExecuteOptions {
            force_turns: true,
            ..ExecuteOptions::default()
        }
    );
    assert!(matches!(flow.stage(), Stage::Running(_)));
    flow.switch_now(Duration::from_secs(2));
    assert!(flow.take_commands().is_empty());
}

#[test]
fn planning_error_returns_to_idle_with_a_drained_notice() {
    let mut flow = Flow::default();
    flow.request(SwitchRequest::profile("missing".into()), Duration::ZERO);
    take_plan(&mut flow);
    let error = Error::other("planning failed");
    flow.on_planned(1, Err(error.clone()), Duration::from_secs(1));
    assert!(matches!(flow.stage(), Stage::Idle));
    assert_eq!(flow.stored_plans(), 0);
    assert!(flow.take_commands().is_empty());
    match flow.take_notices().as_slice() {
        [FlowNotice::PlanFailed(actual)] => assert_eq!(actual, &error),
        notices => panic!("expected one planning failure, got {notices:?}"),
    }
    flow.tick(Duration::from_secs(2));
    assert!(flow.take_notices().is_empty());
}

#[test]
fn stale_planning_error_does_not_replace_latest_selection() {
    let mut flow = Flow::default();
    flow.request(SwitchRequest::profile("old".into()), Duration::ZERO);
    take_plan(&mut flow);
    let plan = hot_plan();
    flow.request(plan.request.clone(), Duration::from_secs(1));
    assert_eq!(take_plan(&mut flow).0, 2);
    flow.on_planned(1, Err(Error::other("old failure")), Duration::from_secs(2));
    assert!(matches!(flow.stage(), Stage::Planning));
    assert!(flow.take_notices().is_empty());
    flow.on_planned(2, Ok(plan), Duration::from_secs(3));
    assert!(matches!(flow.stage(), Stage::Armed { .. }));
}

#[test]
fn running_events_update_phase_steps_and_turn_activity() {
    let mut flow = running_flow();
    flow.on_event(
        &SwitchEvent::Phase {
            phase: SwitchPhase::Applying,
            at: Timestamp::from_millis(100),
        },
        Duration::from_millis(100),
    );
    assert_eq!(flow.run().unwrap().phase, SwitchPhase::Applying);
    assert_eq!(flow.machine().phase(), SwitchPhase::Applying);
    for (step, status) in [
        (SwitchStep::WriteConfig, StepStatus::Running),
        (SwitchStep::WriteEnv, StepStatus::Pending),
        (SwitchStep::WriteConfig, StepStatus::Done),
        (
            SwitchStep::WriteEnv,
            StepStatus::Failed {
                message: "read-only".into(),
            },
        ),
    ] {
        flow.on_event(
            &SwitchEvent::Step {
                step,
                status,
                at: Timestamp::EPOCH,
            },
            Duration::from_secs(1),
        );
    }
    let run = flow.run().unwrap();
    assert_eq!(
        run.steps,
        vec![
            (SwitchStep::WriteConfig, StepStatus::Done),
            (
                SwitchStep::WriteEnv,
                StepStatus::Failed {
                    message: "read-only".into()
                }
            ),
        ]
    );
    assert_eq!(run.step(&SwitchStep::WriteConfig), Some(&StepStatus::Done));
    assert_eq!(run.step(&SwitchStep::VerifyAuth), None);

    let activity = vec![TurnActivity {
        target_id: "local".into(),
        checked_at: Timestamp::from_millis(200),
        busy: vec![busy_thread("local", "one")],
        complete: true,
    }];
    flow.on_event(
        &SwitchEvent::Turns {
            activity: activity.clone(),
            waited_ms: 2500,
        },
        Duration::from_secs(3),
    );
    assert_eq!(flow.run().unwrap().turns, activity);
    assert_eq!(flow.run().unwrap().waited_ms, 2500);
    flow.on_event(
        &SwitchEvent::Turns {
            activity: vec![],
            waited_ms: 3000,
        },
        Duration::from_secs(4),
    );
    assert!(flow.run().unwrap().turns.is_empty());
    assert_eq!(flow.run().unwrap().waited_ms, 3000);
    assert!(flow.take_commands().is_empty());
}

#[test]
fn running_decision_is_stored_and_force_reaches_the_receiver() {
    let mut flow = running_flow();
    let question = wait_question();
    let (tx, mut rx) = oneshot::channel();
    flow.on_decision(PendingDecision::new(question.clone(), tx), Duration::ZERO);
    let run = flow.run().unwrap();
    assert_eq!(run.decision.as_ref().unwrap().request, question);
    assert!(run.waiting_for_turns());
    assert_eq!(run.waited_ms, 1500);
    assert_eq!(run.turns.len(), 2);
    assert_eq!(
        run.turns[0].busy,
        vec![busy_thread("local", "one"), busy_thread("local", "three")]
    );
    assert_eq!(run.turns[1].busy, vec![busy_thread("ssh:devbox", "two")]);
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    flow.acknowledge_wait(Duration::from_secs(2));
    assert!(flow.run().unwrap().wait_acknowledged);
    assert!(
        flow.run().unwrap().decision.is_some(),
        "waiting still permits force or cancel"
    );
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    flow.decide(Decision::Force);
    assert_eq!(rx.try_recv(), Ok(Decision::Force));
    assert!(flow.run().unwrap().decision.is_none());
    assert!(flow.take_commands().is_empty());
}

#[test]
fn cancelling_a_running_wait_answers_cancel_and_waits_for_completion() {
    let mut flow = running_flow();
    let (tx, mut rx) = oneshot::channel();
    flow.on_decision(PendingDecision::new(wait_question(), tx), Duration::ZERO);
    assert!(flow.cancel(Duration::from_secs(1)));
    assert_eq!(rx.try_recv(), Ok(Decision::Cancel));
    assert!(flow.run().unwrap().decision.is_none());
    assert!(matches!(flow.stage(), Stage::Running(_)));
    assert!(!flow.cancel(Duration::from_secs(2)));
    assert!(flow.take_commands().is_empty());
}

#[test]
fn cancel_preserves_a_question_that_does_not_allow_cancel() {
    let mut flow = running_flow();
    let (tx, mut rx) = oneshot::channel();
    flow.on_decision(
        PendingDecision::new(
            DecisionRequest::IpcFailed {
                error: Error::other("IPC failed"),
            },
            tx,
        ),
        Duration::ZERO,
    );
    assert!(!flow.cancel(Duration::from_secs(1)));
    assert!(flow.run().unwrap().decision.is_some());
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    flow.decide(Decision::Undo);
    assert_eq!(rx.try_recv(), Ok(Decision::Undo));
}

#[test]
fn tick_clears_a_withdrawn_question_and_wait_acknowledgement() {
    let mut flow = running_flow();
    let (tx, rx) = oneshot::channel();
    flow.on_decision(PendingDecision::new(wait_question(), tx), Duration::ZERO);
    flow.acknowledge_wait(Duration::from_secs(1));
    drop(rx);
    assert!(flow.run().unwrap().decision.as_ref().unwrap().is_stale());
    flow.tick(Duration::from_secs(2));
    assert!(flow.run().unwrap().decision.is_none());
    assert!(!flow.run().unwrap().wait_acknowledged);
    assert!(matches!(flow.stage(), Stage::Running(_)));
}

#[test]
fn decisions_outside_execution_receive_the_safe_default_immediately() {
    for (question, expected) in [
        (wait_question(), Decision::Wait),
        (
            DecisionRequest::ForceQuit { pids: vec![42] },
            Decision::Cancel,
        ),
        (
            DecisionRequest::IpcFailed {
                error: Error::other("IPC failed"),
            },
            Decision::Undo,
        ),
        (
            DecisionRequest::VerificationFailed {
                verification: Verification::default(),
            },
            Decision::Rollback,
        ),
        (
            DecisionRequest::UnknownRemoteActivity {
                targets: vec!["ssh:devbox".into()],
            },
            Decision::Cancel,
        ),
        (
            DecisionRequest::CloseConsumers { processes: vec![] },
            Decision::Cancel,
        ),
    ] {
        for mut flow in [Flow::default(), planned_flow(5, hot_plan())] {
            let (tx, mut rx) = oneshot::channel();
            flow.on_decision(PendingDecision::new(question.clone(), tx), Duration::ZERO);
            assert_eq!(rx.try_recv(), Ok(expected));
            assert!(flow.run().is_none());
            assert!(flow.take_commands().is_empty());
        }
    }
}

#[test]
fn successful_execution_emits_finished_notice_and_releases_plan() {
    let mut flow = running_flow();
    let plan = flow.run().unwrap().plan.clone();
    let result = outcome(&plan);
    flow.on_executed(Ok(result.clone()), Duration::from_secs(2));
    assert!(matches!(flow.stage(), Stage::Idle));
    assert!(flow.run().is_none());
    assert_eq!(flow.stored_plans(), 0);
    match flow.take_notices().as_slice() {
        [
            FlowNotice::Finished {
                plan: actual_plan,
                outcome,
            },
        ] => {
            assert_eq!(actual_plan.id, plan.id);
            assert_eq!(outcome.as_ref(), &result);
        }
        notices => panic!("expected one finished notice, got {notices:?}"),
    }
    assert!(flow.take_notices().is_empty());
    assert!(flow.request(
        SwitchRequest::profile("official".into()),
        Duration::from_secs(3)
    ));
    assert_eq!(take_plan(&mut flow).0, 2);
}

#[test]
fn failed_execution_emits_failure_notice_and_releases_plan() {
    let mut flow = running_flow();
    let id = flow.run().unwrap().plan.id.clone();
    let error = Error::other("execute failed");
    flow.on_executed(Err(error.clone()), Duration::from_secs(2));
    assert!(matches!(flow.stage(), Stage::Idle));
    assert!(flow.run().is_none());
    assert_eq!(flow.stored_plans(), 0);
    match flow.take_notices().as_slice() {
        [
            FlowNotice::ExecuteFailed {
                plan,
                error: actual,
            },
        ] => {
            assert_eq!(plan.id, id);
            assert_eq!(actual, &error);
        }
        notices => panic!("expected one execution failure, got {notices:?}"),
    }
    assert!(flow.take_notices().is_empty());
    assert!(flow.take_commands().is_empty());
}

#[test]
fn requests_are_rejected_while_running() {
    let mut flow = running_flow();
    let original_request = flow.last_request().cloned();
    let id = flow.run().unwrap().plan.id.clone();
    assert!(!flow.request(
        SwitchRequest::profile("official".into()),
        Duration::from_secs(1)
    ));
    assert!(flow.take_commands().is_empty());
    assert!(matches!(flow.stage(), Stage::Running(_)));
    assert_eq!(flow.run().unwrap().plan.id, id);
    assert_eq!(flow.last_request(), original_request.as_ref());
    assert_eq!(flow.stored_plans(), 1);
}

#[test]
fn attention_is_emitted_once_per_entry_for_every_pending_state() {
    for (plan, attention) in [
        (busy_plan(), FlowAttention::WaitingTurn),
        (full_plan(), FlowAttention::ConfirmFull),
    ] {
        let mut flow = planned_flow(5, plan);
        assert_eq!(flow.take_attention(), Some(attention));
        for _ in 0..3 {
            flow.tick(Duration::from_secs(1));
            assert_eq!(flow.take_attention(), None);
        }
        flow.cancel(Duration::from_secs(1));
        assert_eq!(flow.take_attention(), None);
    }
    let mut flow = running_flow();
    for (phase, attention) in [
        (SwitchPhase::WaitingTurn, FlowAttention::WaitingTurn),
        (
            SwitchPhase::FullRestartOffer,
            FlowAttention::FullRestartOffer,
        ),
        (SwitchPhase::Recovery, FlowAttention::Recovery),
    ] {
        for _ in 0..2 {
            let event = SwitchEvent::Phase {
                phase,
                at: Timestamp::EPOCH,
            };
            flow.on_event(&event, Duration::ZERO);
            assert_eq!(flow.take_attention(), Some(attention));
            flow.on_event(&event, Duration::ZERO);
            assert_eq!(flow.take_attention(), None);
            flow.on_event(
                &SwitchEvent::Phase {
                    phase: SwitchPhase::Verifying,
                    at: Timestamp::EPOCH,
                },
                Duration::ZERO,
            );
            assert_eq!(flow.take_attention(), None);
        }
    }
}

fn recovering_flow() -> (Flow, oneshot::Receiver<Decision>) {
    let mut flow = running_flow();
    let (tx, rx) = oneshot::channel();
    flow.on_event(
        &SwitchEvent::Phase {
            phase: SwitchPhase::Recovery,
            at: Timestamp::EPOCH,
        },
        Duration::from_secs(4),
    );
    flow.on_decision(
        PendingDecision::new(
            DecisionRequest::VerificationFailed {
                verification: Verification::default(),
            },
            tx,
        ),
        Duration::from_secs(4),
    );
    (flow, rx)
}

#[test]
fn recovery_rolls_back_once_after_ten_seconds() {
    let (mut flow, mut rx) = recovering_flow();
    assert_eq!(flow.take_attention(), Some(FlowAttention::Recovery));
    assert_eq!(flow.take_attention(), None);
    assert_eq!(
        flow.run()
            .unwrap()
            .rollback_remaining(Duration::from_secs(4)),
        Some(Duration::from_secs(10))
    );
    flow.tick(Duration::from_millis(13999));
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    flow.tick(Duration::from_secs(14));
    assert_eq!(rx.try_recv(), Ok(Decision::Rollback));
    assert!(flow.run().unwrap().decision.is_none());
    assert_eq!(flow.run().unwrap().phase, SwitchPhase::RollingBack);
    flow.tick(Duration::from_secs(30));
    assert!(flow.take_commands().is_empty());
}

#[test]
fn escape_stops_auto_rollback_but_enter_still_rolls_back() {
    let (mut flow, mut rx) = recovering_flow();
    assert!(flow.cancel(Duration::from_secs(8)));
    flow.tick(Duration::from_secs(100));
    assert!(flow.run().unwrap().recovering());
    assert!(flow.run().unwrap().decision.is_some());
    assert!(
        flow.run()
            .unwrap()
            .rollback_remaining(Duration::from_secs(100))
            .is_none()
    );
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(flow.rollback_now());
    assert_eq!(rx.try_recv(), Ok(Decision::Rollback));
    assert!(!flow.rollback_now());
}

#[test]
fn keep_new_configuration_and_stale_requests_cancel_auto_rollback() {
    for decision in [Decision::KeepWritten, Decision::FullRestart] {
        let (mut flow, mut rx) = recovering_flow();
        flow.decide(decision);
        assert_eq!(rx.try_recv(), Ok(decision));
        flow.tick(Duration::from_secs(30));
        assert!(
            flow.run()
                .unwrap()
                .rollback_remaining(Duration::from_secs(30))
                .is_none()
        );
        assert!(!flow.rollback_now());
    }
    let (mut flow, rx) = recovering_flow();
    drop(rx);
    flow.tick(Duration::from_secs(20));
    assert!(flow.run().unwrap().decision.is_none());
    assert!(!flow.rollback_now());
}

#[test]
fn pending_destination_tracks_reselection_and_clears_on_cancel_or_execute() {
    let plan = hot_plan();
    let mut flow = planned_flow(5, plan.clone());
    assert_eq!(flow.armed_destination(), Some(&plan.to));
    let mut next = hot_plan();
    next.to.line_id = Some("overseas".into());
    flow.request(next.request.clone(), Duration::from_secs(1));
    assert!(flow.armed_destination().is_none());
    let (seq, _) = take_plan(&mut flow);
    flow.on_planned(seq, Ok(next.clone()), Duration::from_secs(1));
    assert_eq!(flow.armed_destination(), Some(&next.to));
    flow.cancel(Duration::from_secs(2));
    assert!(flow.armed_destination().is_none());
    let mut flow = planned_flow(5, plan);
    flow.switch_now(Duration::from_secs(2));
    assert!(flow.armed_destination().is_none());
}
