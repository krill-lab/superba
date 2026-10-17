//! Flag-only decisions and nonblocking progress delivery.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use superba_core::{
    Clock, Decision, DecisionRequest, Result, StepStatus, SwitchDecider, SwitchEvent, SwitchEvents,
};
use superba_switch::messages;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{Output, text::text};

/// Messages queued by callbacks; the command loop owns and writes the output.
pub enum Progress {
    Event(SwitchEvent),
    CloseConsumers(DecisionRequest),
    WaitHint,
}

impl Progress {
    /// Render a queued event outside the service callback.
    pub fn render(self, out: &mut Output) -> Result<()> {
        match self {
            Self::WaitHint => out.progress(text("cli.hint.turns")),
            Self::CloseConsumers(mut request) => {
                if let DecisionRequest::CloseConsumers { processes } = &mut request {
                    for process in processes {
                        process.cmdline = superba_core::app::redact_argv(&process.cmdline);
                    }
                }
                out.progress(messages::question(&request))
            }
            Self::Event(event) => match event {
                SwitchEvent::Phase { phase, .. } => {
                    out.progress(text("cli.format.phase").arg_text("phase", messages::phase(phase)))
                }
                SwitchEvent::Step { step, status, .. } => {
                    if matches!(status, StepStatus::Pending) {
                        return Ok(());
                    }
                    out.progress(
                        text("cli.format.step")
                            .arg_text("step", messages::step(&step))
                            .arg_text("status", messages::step_status(&status)),
                    )
                }
                SwitchEvent::Turns {
                    activity,
                    waited_ms,
                } => {
                    for target in activity {
                        for thread in target.busy {
                            out.progress(messages::busy_thread(&thread))?;
                        }
                    }
                    out.progress(
                        messages::text("switch.waiting.waited")
                            .arg("seconds", messages::seconds_1dp(waited_ms)),
                    )
                }
                SwitchEvent::Log { level, text: value } => out.progress(
                    text("cli.format.notice")
                        .arg("severity", out.severity(level))
                        .arg_text("text", value),
                ),
                _ => Ok(()),
            },
        }
    }
}

/// An unbounded channel keeps callbacks free of writer locks and blocking I/O.
pub struct CliEvents {
    sender: Option<UnboundedSender<Progress>>,
    clock: Arc<dyn Clock>,
    next_turns_ms: AtomicU64,
}

impl CliEvents {
    /// Build a progress sink and its receiver. JSON mode queues nothing.
    pub fn channel(json: bool, clock: Arc<dyn Clock>) -> (Arc<Self>, UnboundedReceiver<Progress>) {
        let (sender, receiver) = unbounded_channel();
        (
            Arc::new(Self {
                sender: (!json).then_some(sender),
                clock,
                next_turns_ms: AtomicU64::new(0),
            }),
            receiver,
        )
    }

    fn send(&self, message: Progress) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(message);
        }
    }
}

impl SwitchEvents for CliEvents {
    fn emit(&self, event: SwitchEvent) {
        if self.sender.is_none()
            || matches!(
                event,
                SwitchEvent::Step {
                    status: StepStatus::Pending,
                    ..
                }
            )
        {
            return;
        }
        if matches!(event, SwitchEvent::Turns { .. }) {
            let now = u64::try_from(self.clock.monotonic().as_millis()).unwrap_or(u64::MAX);
            let next = self.next_turns_ms.load(Ordering::Relaxed);
            if now < next
                || self
                    .next_turns_ms
                    .compare_exchange(
                        next,
                        now.saturating_add(5_000),
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_err()
            {
                return;
            }
        }
        self.send(Progress::Event(event));
    }
}

/// Mid-execution answers. No interactive stdin reads occur while executing.
pub struct FlagDecider {
    pub force: bool,
    pub wait_turn: bool,
    pub yes: bool,
    events: Option<Arc<CliEvents>>,
}

impl FlagDecider {
    /// Construct a decider without a progress sink (useful for embedding).
    pub fn new(force: bool, wait_turn: bool, yes: bool) -> Self {
        Self {
            force,
            wait_turn,
            yes,
            events: None,
        }
    }

    /// Attach the command's progress sink for cancellation and close-process hints.
    pub fn with_events(mut self, events: Arc<CliEvents>) -> Self {
        self.events = Some(events);
        self
    }
}

#[async_trait]
impl SwitchDecider for FlagDecider {
    async fn decide(&self, request: DecisionRequest) -> Decision {
        match &request {
            DecisionRequest::WaitForTurns { .. } => {
                if self.force {
                    Decision::Force
                } else if self.wait_turn {
                    Decision::Wait
                } else {
                    if let Some(events) = &self.events {
                        events.send(Progress::WaitHint);
                    }
                    Decision::Cancel
                }
            }
            DecisionRequest::ForceQuit { .. } => {
                if self.force {
                    Decision::Force
                } else {
                    Decision::Cancel
                }
            }
            DecisionRequest::IpcFailed { .. } => {
                if self.yes {
                    Decision::FullRestart
                } else {
                    Decision::Undo
                }
            }
            DecisionRequest::VerificationFailed { .. } => Decision::Rollback,
            DecisionRequest::UnknownRemoteActivity { .. } => {
                if self.yes || self.force {
                    Decision::Continue
                } else {
                    Decision::Cancel
                }
            }
            DecisionRequest::CloseConsumers { .. } => {
                if let Some(events) = &self.events {
                    events.send(Progress::CloseConsumers(request));
                }
                if self.wait_turn {
                    Decision::Continue
                } else {
                    Decision::Cancel
                }
            }
            _ => request.safe_default(),
        }
    }
}
