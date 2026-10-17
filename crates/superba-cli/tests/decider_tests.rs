mod common;

use common::*;
use std::sync::Arc;
use std::time::Duration;
use superba_cli::{CliEvents, FlagDecider, Output};
use superba_core::*;
use superba_switch::messages;

#[tokio::test]
async fn all_requests_and_all_flag_combinations_follow_the_decision_table() {
    for force in [false, true] {
        for wait in [false, true] {
            for yes in [false, true] {
                let decider = FlagDecider::new(force, wait, yes);
                for (request, expected) in [
                    (
                        DecisionRequest::WaitForTurns {
                            busy: vec![thread()],
                            waited_ms: 20,
                        },
                        if force {
                            Decision::Force
                        } else if wait {
                            Decision::Wait
                        } else {
                            Decision::Cancel
                        },
                    ),
                    (
                        DecisionRequest::ForceQuit {
                            pids: vec![424_242],
                        },
                        if force {
                            Decision::Force
                        } else {
                            Decision::Cancel
                        },
                    ),
                    (
                        DecisionRequest::IpcFailed {
                            error: Error::other("test IPC failure"),
                        },
                        if yes {
                            Decision::FullRestart
                        } else {
                            Decision::Undo
                        },
                    ),
                    (
                        DecisionRequest::VerificationFailed {
                            verification: Verification::default(),
                        },
                        Decision::Rollback,
                    ),
                    (
                        DecisionRequest::UnknownRemoteActivity {
                            targets: vec![TargetId::new("ssh:a")],
                        },
                        if yes || force {
                            Decision::Continue
                        } else {
                            Decision::Cancel
                        },
                    ),
                    (
                        DecisionRequest::CloseConsumers {
                            processes: vec![process()],
                        },
                        if wait {
                            Decision::Continue
                        } else {
                            Decision::Cancel
                        },
                    ),
                ] {
                    assert!(request.allowed().contains(&expected));
                    assert_eq!(
                        decider.decide(request).await,
                        expected,
                        "force={force} wait={wait} yes={yes}"
                    );
                }
            }
        }
    }
}

fn turns(waited_ms: u64) -> SwitchEvent {
    SwitchEvent::Turns {
        activity: vec![TurnActivity {
            target_id: TargetId::local(),
            checked_at: Timestamp::EPOCH,
            busy: vec![thread()],
            complete: true,
        }],
        waited_ms,
    }
}

#[test]
fn turns_are_throttled_by_injected_clock_even_if_reported_wait_time_changes() {
    let clock = Arc::new(ManualClock::new(Timestamp::EPOCH));
    let (events, mut receiver) = CliEvents::channel(false, clock.clone());
    events.emit(turns(0));
    events.emit(turns(60_000));
    clock.advance(Duration::from_millis(4_999));
    events.emit(turns(120_000));
    clock.advance(Duration::from_millis(1));
    events.emit(turns(5_000));
    let (mut out, stdout, stderr) = Output::capture();
    while let Ok(event) = receiver.try_recv() {
        event.render(&mut out).unwrap();
    }
    assert!(stdout.contents().is_empty());
    assert_eq!(stderr.contents().matches("Example thread").count(), 2);
    assert!(stderr.contents().contains("Waiting for 5.0 s"));
}

#[test]
fn progress_has_phase_step_and_severity_but_no_pending_steps() {
    let (events, mut receiver) =
        CliEvents::channel(false, Arc::new(ManualClock::new(Timestamp::EPOCH)));
    events.emit(SwitchEvent::Phase {
        phase: SwitchPhase::Applying,
        at: Timestamp::EPOCH,
    });
    events.emit(SwitchEvent::Step {
        step: SwitchStep::WriteConfig,
        status: StepStatus::Pending,
        at: Timestamp::EPOCH,
    });
    events.emit(SwitchEvent::Step {
        step: SwitchStep::WriteConfig,
        status: StepStatus::Done,
        at: Timestamp::EPOCH,
    });
    events.emit(SwitchEvent::Log {
        level: Severity::Warning,
        text: messages::text("switch.doctor.ok"),
    });
    let (mut out, stdout, stderr) = Output::capture();
    while let Ok(event) = receiver.try_recv() {
        event.render(&mut out).unwrap();
    }
    assert!(stdout.contents().is_empty());
    assert_eq!(
        stderr.contents(),
        "→ Writing configuration\n  Write configuration: done\n[warning] No problems found\n"
    );
}

#[tokio::test]
async fn json_callbacks_emit_nothing_including_decision_hints() {
    let (events, mut receiver) =
        CliEvents::channel(true, Arc::new(ManualClock::new(Timestamp::EPOCH)));
    events.emit(SwitchEvent::Phase {
        phase: SwitchPhase::Applying,
        at: Timestamp::EPOCH,
    });
    events.emit(turns(0));
    let decider = FlagDecider::new(false, false, false).with_events(events);
    assert_eq!(
        decider
            .decide(DecisionRequest::WaitForTurns {
                busy: vec![],
                waited_ms: 0
            })
            .await,
        Decision::Cancel
    );
    assert_eq!(
        decider
            .decide(DecisionRequest::CloseConsumers {
                processes: vec![process()]
            })
            .await,
        Decision::Cancel
    );
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn callbacks_do_not_wait_for_the_output_consumer() {
    let (events, mut receiver) =
        CliEvents::channel(false, Arc::new(ManualClock::new(Timestamp::EPOCH)));
    let decider = FlagDecider::new(false, false, false).with_events(events.clone());
    // No receiver is polled until every callback has completed.
    tokio::time::timeout(Duration::from_secs(1), async {
        for _ in 0..1000 {
            events.emit(SwitchEvent::Phase {
                phase: SwitchPhase::Applying,
                at: Timestamp::EPOCH,
            });
        }
        decider
            .decide(DecisionRequest::CloseConsumers {
                processes: vec![process()],
            })
            .await
    })
    .await
    .unwrap();
    let mut count = 0;
    while receiver.try_recv().is_ok() {
        count += 1;
    }
    assert_eq!(count, 1001);
}
