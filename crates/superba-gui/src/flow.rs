//! The GUI's driver of the plan 7.2 state machine.
//!
//! The states before execution (Idle, Planning, Blocked, questions, Armed with its
//! countdown, waiting for turns, ConfirmFull) are owned by
//! [`superba_switch::SwitchMachine`], the same pure machine `spb` drives. This
//! module feeds it inputs (selections, plans, ticks, clicks, events), performs its
//! [`Effect`]s as backend [`Command`]s, keeps the full [`SwitchPlan`]s the machine
//! refers to by id, and tracks what the machine does not: execution steps, live turn
//! activity and the open [`PendingDecision`] of `SwitchService::execute`.
//!
//! Nothing is written before an `Effect::Execute`, so cancelling while armed or
//! confirming writes nothing. Time is passed in as monotonic `Duration`s, so every
//! transition is unit tested without a window.

use std::collections::HashMap;
use std::time::Duration;

use superba_core::{
    Countdown, Decision, DecisionRequest, Error, ExecuteOptions, LegacySessionChoice,
    LocalizedText, PlanChoice, PlanDestination, ProviderRestore, RestoreTarget, SessionProgress,
    SessionStage, StepStatus, SwitchEvent, SwitchIntent, SwitchOutcome, SwitchPhase, SwitchPlan,
    SwitchRequest, SwitchStep, ThreadActivity, TurnActivity,
};
use superba_switch::{Effect, Input, MachineState, PlanTicket, SwitchMachine};

use crate::runtime::{Command, PendingDecision};

/// A switch in progress (after `execute` was called).
#[derive(Debug)]
pub struct Run {
    /// The plan being executed.
    pub plan: Box<SwitchPlan>,
    /// Latest phase reported by `execute`.
    pub phase: SwitchPhase,
    /// Step statuses in first-seen order.
    pub steps: Vec<(SwitchStep, StepStatus)>,
    /// Busy threads while waiting.
    pub turns: Vec<TurnActivity>,
    /// Time waited for turns.
    pub waited_ms: u64,
    /// Open question from `execute`.
    pub decision: Option<PendingDecision>,
    /// The user chose "switch when they finish" (the question stays open so they
    /// can still force or cancel).
    pub wait_acknowledged: bool,
    /// Monotonic start time.
    pub started: Duration,
    /// Sessions being migrated or put back (the migration dialog).
    pub sessions: Option<SessionsRun>,
    /// Automatic rollback deadline, present only while a verification decision is open.
    recovery_deadline: Option<Duration>,
}

/// Sessions moving during a switch, from its [`SwitchEvent::Sessions`] reports.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionsRun {
    /// The latest report.
    pub progress: SessionProgress,
    /// Monotonic time of the first report.
    pub started: Duration,
    /// The user asked to stop (and undo) the migration.
    pub stop_requested: bool,
    /// The sessions are still moving (the dialog is shown).
    pub active: bool,
}

impl SessionsRun {
    /// Whether the dialog may offer to stop now.
    pub fn can_stop(&self) -> bool {
        self.active && self.progress.can_stop && !self.stop_requested
    }
}

impl Run {
    fn new(plan: Box<SwitchPlan>, now: Duration) -> Self {
        Self {
            plan,
            phase: SwitchPhase::TurnCheck,
            steps: Vec::new(),
            turns: Vec::new(),
            waited_ms: 0,
            decision: None,
            wait_acknowledged: false,
            started: now,
            sessions: None,
            recovery_deadline: None,
        }
    }

    /// Sessions are moving now (migration dialog).
    pub fn sessions_active(&self) -> Option<&SessionsRun> {
        self.sessions.as_ref().filter(|s| s.active)
    }

    /// Status of a step, if reported.
    pub fn step(&self, step: &SwitchStep) -> Option<&StepStatus> {
        self.steps.iter().find(|(s, _)| s == step).map(|(_, st)| st)
    }

    /// True while `execute` waits for running turns.
    pub fn waiting_for_turns(&self) -> bool {
        self.phase == SwitchPhase::WaitingTurn
            || matches!(
                self.decision.as_ref().map(|d| &d.request),
                Some(DecisionRequest::WaitForTurns { .. })
            )
    }

    /// Verification failed and the user is choosing how to recover.
    pub fn recovering(&self) -> bool {
        self.phase == SwitchPhase::Recovery
            || self
                .decision
                .as_ref()
                .is_some_and(|d| matches!(d.request, DecisionRequest::VerificationFailed { .. }))
    }

    /// Time left before rolling back; `None` after Esc or when no decision is open.
    pub fn rollback_remaining(&self, now: Duration) -> Option<Duration> {
        self.decision.as_ref().filter(|d| {
            !d.is_stale() && matches!(d.request, DecisionRequest::VerificationFailed { .. })
        })?;
        self.recovery_deadline
            .map(|deadline| deadline.saturating_sub(now))
    }
}

/// A newly entered state that needs the user's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowAttention {
    /// Running turns need a decision.
    WaitingTurn,
    /// A planned full restart needs confirmation.
    ConfirmFull,
    /// Failed IPC needs a decision about a full restart.
    FullRestartOffer,
    /// Verification failed; bring the recovery dialog to the foreground too.
    Recovery,
}

/// What the UI shows, derived from the machine state and the stored plans.
#[derive(Debug)]
pub enum Stage<'a> {
    /// Nothing in progress.
    Idle,
    /// Waiting for a plan.
    Planning,
    /// The plan is blocked; reasons are shown.
    Blocked {
        /// Plan.
        plan: &'a SwitchPlan,
    },
    /// The plan raised questions (legacy sessions, replacement model, ...).
    Choose {
        /// Plan.
        plan: &'a SwitchPlan,
    },
    /// Hot switch armed: the countdown runs.
    Armed {
        /// Plan.
        plan: &'a SwitchPlan,
        /// Countdown.
        countdown: &'a Countdown,
    },
    /// Turns were running at plan time: the countdown is paused (amber).
    WaitingBeforeSwitch {
        /// Plan.
        plan: &'a SwitchPlan,
        /// Paused countdown.
        countdown: &'a Countdown,
        /// Busy threads.
        busy: &'a [ThreadActivity],
    },
    /// A full restart / migration needs explicit confirmation (no countdown).
    ConfirmFull {
        /// Plan.
        plan: &'a SwitchPlan,
    },
    /// Executing.
    Running(&'a Run),
}

/// Something the app shows after a transition.
#[derive(Debug)]
pub enum FlowNotice {
    /// Planning failed.
    PlanFailed(Error),
    /// Execution returned an outcome.
    Finished {
        /// The executed plan.
        plan: Box<SwitchPlan>,
        /// Outcome.
        outcome: Box<SwitchOutcome>,
    },
    /// Execution returned an error.
    ExecuteFailed {
        /// The plan.
        plan: Box<SwitchPlan>,
        /// Error.
        error: Error,
    },
}

/// The GUI flow: [`SwitchMachine`] plus plans, execution details and decisions.
#[derive(Debug)]
pub struct Flow {
    machine: SwitchMachine,
    plans: HashMap<String, SwitchPlan>,
    run: Option<Run>,
    last_request: Option<SwitchRequest>,
    commands: Vec<Command>,
    notices: Vec<FlowNotice>,
    attention: Option<FlowAttention>,
}

impl Default for Flow {
    fn default() -> Self {
        Self::new(5)
    }
}

impl Flow {
    /// New idle flow with a countdown of `countdown_secs` (0 = switch at once).
    pub fn new(countdown_secs: u8) -> Self {
        Self {
            machine: SwitchMachine::new(countdown_secs),
            plans: HashMap::new(),
            run: None,
            last_request: None,
            commands: Vec::new(),
            notices: Vec::new(),
            attention: None,
        }
    }

    /// Change the countdown length (applies to the next arming).
    pub fn set_countdown_secs(&mut self, secs: u8) {
        self.machine.set_countdown_secs(secs);
    }

    /// The underlying machine (read-only).
    pub fn machine(&self) -> &SwitchMachine {
        &self.machine
    }

    /// Current stage for drawing.
    pub fn stage(&self) -> Stage<'_> {
        let plan_of = |t: &PlanTicket| self.plans.get(&t.plan_id);
        match self.machine.state() {
            MachineState::Idle | MachineState::Done { .. } | MachineState::Failed { .. } => {
                Stage::Idle
            }
            MachineState::Planning { .. } => Stage::Planning,
            MachineState::Blocked { ticket } => {
                plan_of(ticket).map_or(Stage::Idle, |plan| Stage::Blocked { plan })
            }
            MachineState::NeedsAnswers { ticket } => {
                plan_of(ticket).map_or(Stage::Idle, |plan| Stage::Choose { plan })
            }
            MachineState::Countdown { ticket, countdown } => {
                plan_of(ticket).map_or(Stage::Idle, |plan| Stage::Armed { plan, countdown })
            }
            MachineState::WaitingForTurn {
                ticket,
                countdown,
                busy,
            } => plan_of(ticket).map_or(Stage::Idle, |plan| Stage::WaitingBeforeSwitch {
                plan,
                countdown,
                busy,
            }),
            MachineState::ConfirmFull { ticket } => {
                plan_of(ticket).map_or(Stage::Idle, |plan| Stage::ConfirmFull { plan })
            }
            MachineState::Running { .. } => self.run.as_ref().map_or(Stage::Idle, Stage::Running),
        }
    }

    /// Mutable access to the running switch.
    pub fn run_mut(&mut self) -> Option<&mut Run> {
        self.run.as_mut()
    }

    /// The running switch.
    pub fn run(&self) -> Option<&Run> {
        self.run.as_ref()
    }

    /// The request of the latest plan (for re-planning with answers).
    pub fn last_request(&self) -> Option<&SwitchRequest> {
        self.last_request.as_ref()
    }

    /// True while executing (selections are disabled).
    pub fn is_running(&self) -> bool {
        matches!(self.machine.state(), MachineState::Running { .. })
    }

    /// True while a plan is being computed.
    pub fn is_planning(&self) -> bool {
        matches!(self.machine.state(), MachineState::Planning { .. })
    }

    /// Commands to send to the backend (drained).
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    /// Notices for the app (drained).
    pub fn take_notices(&mut self) -> Vec<FlowNotice> {
        std::mem::take(&mut self.notices)
    }

    /// Return attention once per state entry, even when called every frame.
    pub fn take_attention(&mut self) -> Option<FlowAttention> {
        let next = match self.stage() {
            Stage::WaitingBeforeSwitch { .. } => Some(FlowAttention::WaitingTurn),
            Stage::ConfirmFull { .. } => Some(FlowAttention::ConfirmFull),
            Stage::Running(run) if run.recovering() => Some(FlowAttention::Recovery),
            Stage::Running(run) if run.waiting_for_turns() => Some(FlowAttention::WaitingTurn),
            Stage::Running(run)
                if run.phase == SwitchPhase::FullRestartOffer
                    || run.decision.as_ref().is_some_and(|d| {
                        matches!(d.request, DecisionRequest::IpcFailed { .. })
                    }) =>
            {
                Some(FlowAttention::FullRestartOffer)
            }
            _ => None,
        };
        if self.attention == next {
            None
        } else {
            self.attention = next;
            next
        }
    }

    /// Profile, site, line and auth mode of a selection whose countdown is armed
    /// (including a countdown paused for running turns).
    pub fn armed_destination(&self) -> Option<&PlanDestination> {
        match self.stage() {
            Stage::Armed { plan, .. } | Stage::WaitingBeforeSwitch { plan, .. } => Some(&plan.to),
            _ => None,
        }
    }

    fn feed(&mut self, input: Input, now: Duration) {
        let effects = self.machine.handle(input, now);
        for effect in effects {
            match effect {
                Effect::Plan { seq, request } => {
                    self.last_request = Some(request.clone());
                    self.commands.push(Command::Plan { seq, request });
                }
                Effect::Execute {
                    plan_id,
                    write_only,
                    allow_full_restart,
                    force_turns,
                } => {
                    let Some(plan) = self.plans.get(&plan_id).cloned() else {
                        // The machine only executes plans we stored; if one is missing,
                        // report it instead of executing anything.
                        self.notices
                            .push(FlowNotice::PlanFailed(Error::other(format!(
                                "plan {plan_id} is no longer available"
                            ))));
                        continue;
                    };
                    // The confirmation said up front that ChatGPT is ended by
                    // force (Windows, not opened by Superba): confirming it is
                    // the answer, no second question (user bug 76).
                    let force_quit = allow_full_restart && !write_only && plan.quits_by_force();
                    let plan = Box::new(plan);
                    let options = ExecuteOptions {
                        write_only,
                        allow_full_restart,
                        force_turns,
                        force_quit,
                        ..ExecuteOptions::default()
                    };
                    self.commands.push(Command::Execute {
                        plan: plan.clone(),
                        options,
                    });
                    self.run = Some(Run::new(plan, now));
                }
                Effect::Discard { plan_id } => {
                    if self.run.as_ref().is_none_or(|r| r.plan.id != plan_id) {
                        self.plans.remove(&plan_id);
                    }
                }
            }
        }
        // Planning errors become notices (toasts) and the machine returns to idle.
        let failed = match self.machine.state() {
            MachineState::Failed {
                ticket: None,
                error,
            } => Some(error.clone()),
            _ => None,
        };
        if let Some(e) = failed {
            self.notices.push(FlowNotice::PlanFailed(e));
            self.feed(Input::Dismiss, now);
        }
    }

    /// The user selected something: plan it. While armed this restarts the cycle
    /// (only the last selection executes). Ignored while executing.
    pub fn request(&mut self, request: SwitchRequest, now: Duration) -> bool {
        if self.is_running() {
            return false;
        }
        self.feed(Input::Select(request), now);
        true
    }

    /// A plan arrived.
    pub fn on_planned(
        &mut self,
        seq: u64,
        result: superba_core::Result<SwitchPlan>,
        now: Duration,
    ) {
        match result {
            Ok(plan) => {
                let ticket = PlanTicket::from_plan(&plan);
                self.plans.insert(plan.id.clone(), plan);
                self.feed(Input::Planned { seq, ticket }, now);
            }
            Err(error) => self.feed(Input::PlanFailed { seq, error }, now),
        }
    }

    /// Advance timers: an expired countdown executes the plan; questions withdrawn
    /// by `execute` are dropped.
    pub fn tick(&mut self, now: Duration) {
        self.feed(Input::Tick, now);
        if let Some(run) = &mut self.run
            && run.decision.as_ref().is_some_and(PendingDecision::is_stale)
        {
            run.decision = None;
            run.wait_acknowledged = false;
            run.recovery_deadline = None;
        }
        if self.run.as_ref().and_then(|r| r.rollback_remaining(now)) == Some(Duration::ZERO) {
            self.decide(Decision::Rollback);
        }
    }

    /// "Switch now" / Enter while armed; "switch anyway" while turns ran at plan time.
    pub fn switch_now(&mut self, now: Duration) {
        self.feed(Input::SwitchNow, now);
    }

    /// Cancel / Esc. Before execution this writes nothing; while `execute` waits for
    /// turns it answers `Cancel`. Returns true if something was cancelled.
    pub fn cancel(&mut self, now: Duration) -> bool {
        if let Some(run) = &mut self.run {
            if run
                .decision
                .as_ref()
                .is_some_and(|d| matches!(d.request, DecisionRequest::VerificationFailed { .. }))
            {
                // Escape stops the timer, but leaves recovery and its choices open.
                return run.recovery_deadline.take().is_some();
            }
            return match run.decision.take() {
                Some(d) if d.request.allowed().contains(&Decision::Cancel) => {
                    d.answer(Decision::Cancel);
                    true
                }
                other => {
                    run.decision = other;
                    false
                }
            };
        }
        let was_idle = matches!(self.machine.state(), MachineState::Idle);
        self.feed(Input::Cancel, now);
        !was_idle
    }

    /// Confirm a full restart (`write_only`: "write the config only, I will restart
    /// myself").
    pub fn confirm_full(&mut self, write_only: bool, now: Duration) {
        let input = if write_only {
            Input::WriteOnly
        } else {
            Input::Confirm
        };
        self.feed(input, now);
    }

    /// Re-plan with one of the plan's alternatives ("hot switch with an API key").
    pub fn take_alternative(&mut self, index: usize, now: Duration) {
        let request = self
            .current_plan()
            .and_then(|p| p.alternatives.get(index))
            .map(|a| a.request.clone());
        if let Some(r) = request {
            self.request(r, now);
        }
    }

    /// Answer the legacy-sessions question (plan 10.3) by re-planning.
    pub fn answer_legacy_sessions(&mut self, choice: LegacySessionChoice, now: Duration) {
        self.replan(|r| r.legacy_sessions = Some(choice), now);
    }

    /// Answer the replacement-model question by re-planning.
    pub fn answer_model(&mut self, model: String, now: Duration) {
        self.replan(|r| r.model_override = Some(model), now);
    }

    /// Answer the restore-provider question (plan 10.4) by re-planning.
    pub fn answer_restore_provider(&mut self, provider: ProviderRestore, now: Duration) {
        self.replan(
            |r| {
                if let SwitchIntent::Restore { target } = &mut r.intent {
                    *target = RestoreTarget::Original { provider };
                } else if let SwitchIntent::Uninstall { provider: p, .. } = &mut r.intent {
                    *p = Some(provider);
                }
            },
            now,
        );
    }

    /// Accept the remaining yes/no questions (foreign env, unknown versions, shared
    /// home) by re-planning with them answered.
    pub fn accept_risks(&mut self, plan_choices: &[PlanChoice], now: Duration) {
        let choices = plan_choices.to_vec();
        self.replan(
            move |r| {
                for c in &choices {
                    match c {
                        PlanChoice::ForeignEnv { .. } => r.take_over_foreign_env = Some(true),
                        PlanChoice::UnknownVersion { .. } => r.accept_unknown_versions = true,
                        PlanChoice::SharedHome { .. } => r.accept_shared_home = true,
                        _ => {}
                    }
                }
            },
            now,
        );
    }

    /// Plan shown in the current stage (not while executing).
    pub fn current_plan(&self) -> Option<&SwitchPlan> {
        match self.stage() {
            Stage::Blocked { plan }
            | Stage::Choose { plan }
            | Stage::Armed { plan, .. }
            | Stage::WaitingBeforeSwitch { plan, .. }
            | Stage::ConfirmFull { plan } => Some(plan),
            _ => None,
        }
    }

    fn replan(&mut self, edit: impl FnOnce(&mut SwitchRequest), now: Duration) {
        let base = self
            .current_plan()
            .map(|p| p.request.clone())
            .or_else(|| self.last_request.clone());
        if let Some(mut request) = base {
            edit(&mut request);
            self.request(request, now);
        }
    }

    /// A progress event from `execute`.
    pub fn on_event(&mut self, event: &SwitchEvent, now: Duration) {
        if let Some(run) = &mut self.run {
            match event {
                SwitchEvent::Phase { phase, .. } => run.phase = *phase,
                SwitchEvent::Step { step, status, .. } => {
                    if let Some(slot) = run.steps.iter_mut().find(|(s, _)| s == step) {
                        slot.1 = status.clone();
                    } else {
                        run.steps.push((step.clone(), status.clone()));
                    }
                    // The migration step ended (also when a last report was lost).
                    if *step == SwitchStep::MigrateSessions
                        && !matches!(status, StepStatus::Running | StepStatus::Pending)
                        && let Some(s) = &mut run.sessions
                    {
                        s.active = false;
                    }
                }
                SwitchEvent::Sessions { progress } => {
                    let active = progress.stage != SessionStage::Done;
                    match &mut run.sessions {
                        // A new operation (the undo after a stop, a restore after
                        // a later failure) keeps the clock of the dialog it follows
                        // while that one is still open.
                        Some(s) if s.active || active => {
                            if !s.active {
                                s.started = now;
                                s.stop_requested = false;
                            }
                            s.progress = progress.clone();
                            s.active = active;
                        }
                        _ => {
                            run.sessions = active.then(|| SessionsRun {
                                progress: progress.clone(),
                                started: now,
                                stop_requested: false,
                                active,
                            });
                        }
                    }
                }
                SwitchEvent::Turns {
                    activity,
                    waited_ms,
                } => {
                    run.turns = activity.clone();
                    run.waited_ms = *waited_ms;
                }
                _ => {}
            }
        }
        self.feed(Input::Event(event.clone()), now);
    }

    /// A question from `execute`. Without a running switch the safe default is
    /// answered at once.
    pub fn on_decision(&mut self, decision: PendingDecision, now: Duration) {
        match &mut self.run {
            Some(run) => {
                run.recovery_deadline =
                    matches!(decision.request, DecisionRequest::VerificationFailed { .. })
                        .then(|| now.saturating_add(Duration::from_secs(10)));
                if let DecisionRequest::WaitForTurns { busy, waited_ms } = &decision.request {
                    run.waited_ms = *waited_ms;
                    if run.turns.is_empty() {
                        run.turns = group_turns(busy);
                    }
                }
                run.wait_acknowledged = false;
                run.decision = Some(decision);
            }
            None => {
                let d = decision.request.safe_default();
                decision.answer(d);
            }
        }
    }

    /// Answer the open question.
    pub fn decide(&mut self, decision: Decision) {
        if let Some(run) = &mut self.run
            && run
                .decision
                .as_ref()
                .is_some_and(|d| !d.is_stale() && d.request.allowed().contains(&decision))
            && let Some(d) = run.decision.take()
        {
            run.recovery_deadline = None;
            if decision == Decision::Rollback {
                run.phase = SwitchPhase::RollingBack;
            }
            d.answer(decision);
        }
    }

    /// "Stop and undo" in the migration dialog: honoured only while the latest
    /// report says the migration can stop (it then puts every session back and
    /// the switch ends as cancelled). Returns whether a stop was requested.
    pub fn stop_sessions(&mut self) -> bool {
        let Some(s) = self.run.as_mut().and_then(|r| r.sessions.as_mut()) else {
            return false;
        };
        if !s.can_stop() {
            return false;
        }
        s.stop_requested = true;
        self.commands.push(Command::StopSessions);
        true
    }

    /// Enter in the recovery dialog rolls back immediately, including after Esc.
    pub fn rollback_now(&mut self) -> bool {
        let recovering = self.run.as_ref().is_some_and(|r| {
            r.decision.as_ref().is_some_and(|d| {
                !d.is_stale() && matches!(d.request, DecisionRequest::VerificationFailed { .. })
            })
        });
        if recovering {
            self.decide(Decision::Rollback);
        }
        recovering
    }

    /// "Switch when they finish". Before execution (turns found at plan time) this
    /// resumes the countdown, and `execute` then waits for the turns itself; during
    /// execution the open question stays open (so force / cancel remain possible).
    pub fn acknowledge_wait(&mut self, now: Duration) {
        match &mut self.run {
            Some(run) => run.wait_acknowledged = true,
            None => self.feed(Input::Turns(Vec::new()), now),
        }
    }

    /// `execute` returned.
    pub fn on_executed(&mut self, result: superba_core::Result<SwitchOutcome>, now: Duration) {
        let Some(run) = self.run.take() else {
            return;
        };
        let plan = run.plan;
        match result {
            Ok(outcome) => {
                self.feed(Input::Finished(outcome.clone()), now);
                self.notices.push(FlowNotice::Finished {
                    plan: plan.clone(),
                    outcome: Box::new(outcome),
                });
            }
            Err(error) => {
                self.feed(Input::ExecFailed(error.clone()), now);
                self.notices.push(FlowNotice::ExecuteFailed {
                    plan: plan.clone(),
                    error,
                });
            }
        }
        self.feed(Input::Dismiss, now);
        self.plans.remove(&plan.id);
    }

    /// Destination label of the plan in the current stage.
    pub fn destination(&self) -> Option<&LocalizedText> {
        match self.stage() {
            Stage::Running(run) => Some(&run.plan.to.label),
            _ => self.current_plan().map(|p| &p.to.label),
        }
    }

    /// Number of stored plans (tests: discarded plans do not leak).
    pub fn stored_plans(&self) -> usize {
        self.plans.len()
    }
}

/// Busy threads grouped per target (for the waiting banner before the first
/// `Turns` event).
fn group_turns(busy: &[ThreadActivity]) -> Vec<TurnActivity> {
    let mut out: Vec<TurnActivity> = Vec::new();
    for t in busy {
        if let Some(a) = out.iter_mut().find(|a| a.target_id == t.target_id) {
            a.busy.push(t.clone());
        } else {
            out.push(TurnActivity {
                target_id: t.target_id.clone(),
                checked_at: t.last_event_at.unwrap_or_default(),
                busy: vec![t.clone()],
                complete: true,
            });
        }
    }
    out
}
