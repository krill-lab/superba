//! The UI side of the plan 7.2 state machine, as a pure value driven by inputs and
//! a monotonic clock reading (`Clock::monotonic`). The GUI and `spb` both drive it:
//! they feed user actions, plan results, live turn activity and the
//! [`superba_core::SwitchEvent`]s of `execute`, and perform the returned
//! [`Effect`]s (plan, execute, discard a stored plan).
//!
//! ```text
//! Idle -> Planning -> Blocked | NeedsAnswers | ConfirmFull | Countdown | Running
//! Countdown --(busy turns)--> WaitingForTurn --(turns idle)--> Countdown
//! Countdown --(expired / Enter)--> Running(TurnCheck ... Done) -> Done | Failed
//! ConfirmFull --(confirm / write only)--> Running
//! Planning/Countdown/WaitingForTurn/ConfirmFull --(Esc)--> Idle   (zero writes)
//! Countdown/ConfirmFull --(new selection)--> Planning (latest selection wins)
//! ```
//!
//! Nothing is written before `Effect::Execute`: cancelling in any pre-execution
//! state is always free. Mid-execution questions (force, undo ...) go through the
//! `SwitchDecider`, not through this machine.

use std::time::Duration;

use superba_core::{
    BlockReason, Countdown, Error, LocalizedText, OutcomeStatus, RestartKind, SwitchEvent,
    SwitchOutcome, SwitchPhase, SwitchPlan, SwitchRequest, ThreadActivity,
};

/// Longest configurable countdown (seconds).
pub const MAX_COUNTDOWN_SECS: u8 = 30;

/// Clamp a configured countdown to `0..=30` seconds.
pub fn countdown_duration(secs: u8) -> Duration {
    Duration::from_secs(u64::from(secs.min(MAX_COUNTDOWN_SECS)))
}

/// What the machine needs to know about a computed plan (the UI keeps the plan
/// itself, keyed by `plan_id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanTicket {
    /// Plan id.
    pub plan_id: String,
    /// Local restart decision.
    pub primary: RestartKind,
    /// Hot plan without questions.
    pub countdown_allowed: bool,
    /// Needs the explicit confirmation dialog.
    pub needs_confirmation: bool,
    /// Block reasons (non-empty = blocked).
    pub blocked: Vec<BlockReason>,
    /// Number of unanswered plan choices.
    pub choices: usize,
    /// Destination label.
    pub destination: LocalizedText,
    /// Threads busy at plan time.
    pub busy: Vec<ThreadActivity>,
}

impl PlanTicket {
    /// Summarize a plan.
    pub fn from_plan(plan: &SwitchPlan) -> Self {
        Self {
            plan_id: plan.id.clone(),
            primary: plan.primary.clone(),
            countdown_allowed: plan.countdown_allowed(),
            needs_confirmation: plan.needs_confirmation(),
            blocked: match &plan.primary {
                RestartKind::Blocked { reasons } => reasons.clone(),
                _ => Vec::new(),
            },
            choices: plan.choices.len(),
            destination: plan.to.label.clone(),
            busy: plan.active_threads.clone(),
        }
    }

    fn is_blocked(&self) -> bool {
        self.primary.is_blocked()
    }
}

/// States of the UI machine.
#[derive(Clone, Debug, PartialEq)]
pub enum MachineState {
    /// Nothing in progress.
    Idle,
    /// Waiting for the plan of selection `seq`.
    Planning {
        /// Selection sequence number.
        seq: u64,
    },
    /// The plan is blocked (show the reasons).
    Blocked {
        /// Plan.
        ticket: PlanTicket,
    },
    /// The plan asks questions; the UI re-plans with the answers (new selection).
    NeedsAnswers {
        /// Plan.
        ticket: PlanTicket,
    },
    /// Hot plan, countdown running (plan `Armed`).
    Countdown {
        /// Plan.
        ticket: PlanTicket,
        /// Timer.
        countdown: Countdown,
    },
    /// Countdown paused because turns are running (plan `WaitingTurn` before
    /// execution; amber progress bar).
    WaitingForTurn {
        /// Plan.
        ticket: PlanTicket,
        /// Paused timer.
        countdown: Countdown,
        /// Busy threads.
        busy: Vec<ThreadActivity>,
    },
    /// Full restart / migration needs an explicit confirmation (no countdown).
    ConfirmFull {
        /// Plan.
        ticket: PlanTicket,
    },
    /// `execute` is running; `phase` mirrors its `SwitchEvent::Phase` events.
    Running {
        /// Plan.
        ticket: PlanTicket,
        /// Current execution phase.
        phase: SwitchPhase,
        /// Executing with "write only".
        write_only: bool,
    },
    /// Finished (toast); `Dismiss` returns to idle.
    Done {
        /// Plan.
        ticket: PlanTicket,
        /// Final status.
        status: OutcomeStatus,
    },
    /// Planning or execution failed with an error.
    Failed {
        /// Plan, when one existed.
        ticket: Option<PlanTicket>,
        /// Error.
        error: Error,
    },
}

/// Inputs to the machine.
#[derive(Clone, Debug)]
pub enum Input {
    /// The user selected a profile / line / toggle (also re-selection while armed).
    Select(SwitchRequest),
    /// The plan for selection `seq` is ready.
    Planned {
        /// Selection sequence number from [`Effect::Plan`].
        seq: u64,
        /// Summary.
        ticket: PlanTicket,
    },
    /// Planning for selection `seq` failed.
    PlanFailed {
        /// Selection sequence number.
        seq: u64,
        /// Error.
        error: Error,
    },
    /// Clock tick (UI frame / CLI loop).
    Tick,
    /// "Switch now" / Enter (in `WaitingForTurn`: force).
    SwitchNow,
    /// "Full restart" in the confirmation dialog.
    Confirm,
    /// "Only write the configuration, I will restart later".
    WriteOnly,
    /// Cancel / Esc.
    Cancel,
    /// Live turn activity while the countdown runs (busy threads; empty = idle).
    Turns(Vec<ThreadActivity>),
    /// An event emitted by `execute`.
    Event(SwitchEvent),
    /// `execute` returned.
    Finished(SwitchOutcome),
    /// `execute` failed.
    ExecFailed(Error),
    /// Close the result toast / error.
    Dismiss,
}

/// Side effects requested by the machine.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Call `SwitchService::plan(request)` and feed back `Planned { seq, .. }`.
    Plan {
        /// Selection sequence number.
        seq: u64,
        /// Request.
        request: SwitchRequest,
    },
    /// Call `SwitchService::execute` with the stored plan.
    Execute {
        /// Plan id.
        plan_id: String,
        /// "Write only" (no restart, no migration).
        write_only: bool,
        /// The user confirmed a full restart.
        allow_full_restart: bool,
        /// Do not wait for running turns (user forced).
        force_turns: bool,
    },
    /// The stored plan is no longer needed.
    Discard {
        /// Plan id.
        plan_id: String,
    },
}

/// The UI state machine.
#[derive(Clone, Debug)]
pub struct SwitchMachine {
    state: MachineState,
    countdown: Duration,
    seq: u64,
}

impl SwitchMachine {
    /// New idle machine with a countdown of `countdown_secs` (clamped to 0..=30).
    pub fn new(countdown_secs: u8) -> Self {
        Self {
            state: MachineState::Idle,
            countdown: countdown_duration(countdown_secs),
            seq: 0,
        }
    }

    /// Current state.
    pub fn state(&self) -> &MachineState {
        &self.state
    }

    /// Change the countdown length (applies to the next countdown).
    pub fn set_countdown_secs(&mut self, secs: u8) {
        self.countdown = countdown_duration(secs);
    }

    /// Configured countdown length.
    pub fn countdown_len(&self) -> Duration {
        self.countdown
    }

    /// The plan 7.2 phase the UI is in.
    pub fn phase(&self) -> SwitchPhase {
        match &self.state {
            MachineState::Idle => SwitchPhase::Idle,
            MachineState::Planning { .. } | MachineState::NeedsAnswers { .. } => {
                SwitchPhase::Planning
            }
            MachineState::Blocked { .. } => SwitchPhase::Blocked,
            MachineState::Countdown { .. } => SwitchPhase::Armed,
            MachineState::WaitingForTurn { .. } => SwitchPhase::WaitingTurn,
            MachineState::ConfirmFull { .. } => SwitchPhase::ConfirmFull,
            MachineState::Running { phase, .. } => *phase,
            MachineState::Done { .. } | MachineState::Failed { .. } => SwitchPhase::Done,
        }
    }

    /// Seconds left on the countdown, rounded up (for "Switching in 3 s").
    pub fn seconds_left(&self, now: Duration) -> Option<u64> {
        match &self.state {
            MachineState::Countdown { countdown, .. }
            | MachineState::WaitingForTurn { countdown, .. } => {
                let r = countdown.remaining(now);
                Some(r.as_millis().div_ceil(1000) as u64)
            }
            _ => None,
        }
    }

    /// Countdown progress `0.0..=1.0`.
    pub fn progress(&self, now: Duration) -> Option<f32> {
        match &self.state {
            MachineState::Countdown { countdown, .. }
            | MachineState::WaitingForTurn { countdown, .. } => Some(countdown.progress(now)),
            _ => None,
        }
    }

    /// True while nothing has been written and cancelling is free.
    pub fn cancel_is_free(&self) -> bool {
        !matches!(self.state, MachineState::Running { .. })
    }

    /// The ticket of the current plan, if any.
    pub fn ticket(&self) -> Option<&PlanTicket> {
        match &self.state {
            MachineState::Blocked { ticket }
            | MachineState::NeedsAnswers { ticket }
            | MachineState::Countdown { ticket, .. }
            | MachineState::WaitingForTurn { ticket, .. }
            | MachineState::ConfirmFull { ticket }
            | MachineState::Running { ticket, .. }
            | MachineState::Done { ticket, .. } => Some(ticket),
            MachineState::Failed { ticket, .. } => ticket.as_ref(),
            MachineState::Idle | MachineState::Planning { .. } => None,
        }
    }

    /// Feed one input at monotonic time `now`; returns the effects to perform.
    pub fn handle(&mut self, input: Input, now: Duration) -> Vec<Effect> {
        let mut fx = Vec::new();
        match input {
            Input::Select(request) => {
                if matches!(self.state, MachineState::Running { .. }) {
                    return fx;
                }
                self.discard_current(&mut fx);
                self.seq += 1;
                self.state = MachineState::Planning { seq: self.seq };
                fx.push(Effect::Plan {
                    seq: self.seq,
                    request,
                });
            }
            Input::Planned { seq, ticket } => {
                if !matches!(self.state, MachineState::Planning { seq: s } if s == seq) {
                    // A newer selection superseded this plan.
                    fx.push(Effect::Discard {
                        plan_id: ticket.plan_id,
                    });
                    return fx;
                }
                self.enter_planned(ticket, now, &mut fx);
            }
            Input::PlanFailed { seq, error } => {
                if matches!(self.state, MachineState::Planning { seq: s } if s == seq) {
                    self.state = MachineState::Failed {
                        ticket: None,
                        error,
                    };
                }
            }
            Input::Tick => {
                if let MachineState::Countdown { countdown, ticket } = &self.state
                    && countdown.is_expired(now)
                {
                    let ticket = ticket.clone();
                    self.start(ticket, false, false, false, &mut fx);
                }
            }
            Input::SwitchNow => match &self.state {
                MachineState::Countdown { ticket, .. } => {
                    let ticket = ticket.clone();
                    self.start(ticket, false, false, false, &mut fx);
                }
                MachineState::WaitingForTurn { ticket, .. } => {
                    // "Switch anyway": the user accepts losing in-flight output.
                    let ticket = ticket.clone();
                    self.start(ticket, false, false, true, &mut fx);
                }
                _ => {}
            },
            Input::Confirm => {
                if let MachineState::ConfirmFull { ticket } = &self.state {
                    let ticket = ticket.clone();
                    self.start(ticket, false, true, false, &mut fx);
                }
            }
            Input::WriteOnly => {
                if let MachineState::ConfirmFull { ticket } = &self.state {
                    let ticket = ticket.clone();
                    self.start(ticket, true, false, false, &mut fx);
                }
            }
            Input::Cancel => {
                if !matches!(self.state, MachineState::Running { .. }) {
                    self.discard_current(&mut fx);
                    self.state = MachineState::Idle;
                }
            }
            Input::Turns(busy) => match &mut self.state {
                MachineState::Countdown { ticket, countdown } if !busy.is_empty() => {
                    let mut countdown = *countdown;
                    countdown.pause(now);
                    let ticket = ticket.clone();
                    self.state = MachineState::WaitingForTurn {
                        ticket,
                        countdown,
                        busy,
                    };
                }
                MachineState::WaitingForTurn {
                    ticket, countdown, ..
                } if busy.is_empty() => {
                    let mut countdown = *countdown;
                    countdown.resume(now);
                    let ticket = ticket.clone();
                    self.state = MachineState::Countdown { ticket, countdown };
                }
                MachineState::WaitingForTurn { busy: b, .. } => *b = busy,
                _ => {}
            },
            Input::Event(SwitchEvent::Phase { phase, .. }) => {
                if let MachineState::Running { phase: p, .. } = &mut self.state {
                    *p = phase;
                }
            }
            Input::Event(_) => {}
            Input::Finished(outcome) => {
                if let MachineState::Running { ticket, .. } = &self.state {
                    let ticket = ticket.clone();
                    self.state = MachineState::Done {
                        ticket,
                        status: outcome.status,
                    };
                }
            }
            Input::ExecFailed(error) => {
                if let MachineState::Running { ticket, .. } = &self.state {
                    let ticket = Some(ticket.clone());
                    self.state = MachineState::Failed { ticket, error };
                }
            }
            Input::Dismiss => match &self.state {
                MachineState::Done { ticket, .. } | MachineState::Blocked { ticket } => {
                    fx.push(Effect::Discard {
                        plan_id: ticket.plan_id.clone(),
                    });
                    self.state = MachineState::Idle;
                }
                MachineState::Failed { ticket, .. } => {
                    if let Some(t) = ticket {
                        fx.push(Effect::Discard {
                            plan_id: t.plan_id.clone(),
                        });
                    }
                    self.state = MachineState::Idle;
                }
                _ => {}
            },
        }
        fx
    }

    fn enter_planned(&mut self, ticket: PlanTicket, now: Duration, fx: &mut Vec<Effect>) {
        if ticket.is_blocked() {
            self.state = MachineState::Blocked { ticket };
        } else if ticket.choices > 0 {
            self.state = MachineState::NeedsAnswers { ticket };
        } else if ticket.needs_confirmation {
            self.state = MachineState::ConfirmFull { ticket };
        } else if ticket.countdown_allowed {
            if self.countdown.is_zero() {
                self.start(ticket, false, false, false, fx);
            } else {
                let countdown = Countdown::start(self.countdown, now);
                self.state = if ticket.busy.is_empty() {
                    MachineState::Countdown { ticket, countdown }
                } else {
                    let mut countdown = countdown;
                    countdown.pause(now);
                    let busy = ticket.busy.clone();
                    MachineState::WaitingForTurn {
                        ticket,
                        countdown,
                        busy,
                    }
                };
            }
        } else {
            // Nothing to restart (app not running): plan 7.2 "Planning -> Applying".
            self.start(ticket, false, false, false, fx);
        }
    }

    fn start(
        &mut self,
        ticket: PlanTicket,
        write_only: bool,
        allow_full_restart: bool,
        force_turns: bool,
        fx: &mut Vec<Effect>,
    ) {
        fx.push(Effect::Execute {
            plan_id: ticket.plan_id.clone(),
            write_only,
            allow_full_restart,
            force_turns,
        });
        self.state = MachineState::Running {
            ticket,
            phase: SwitchPhase::TurnCheck,
            write_only,
        };
    }

    fn discard_current(&mut self, fx: &mut Vec<Effect>) {
        if let Some(t) = self.ticket()
            && !matches!(self.state, MachineState::Running { .. })
        {
            fx.push(Effect::Discard {
                plan_id: t.plan_id.clone(),
            });
        }
    }
}
