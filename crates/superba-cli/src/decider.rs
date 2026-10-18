//! Flag-only decisions and nonblocking progress delivery.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use superba_core::{
    Clock, Decision, DecisionRequest, LocalizedText, ProcessInfo, Result, SessionStage, StepStatus,
    SwitchDecider, SwitchEvent, SwitchEvents, ThreadActivity,
};
use superba_switch::messages;
use superba_switch::session_events::session_progress_line;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{Output, text::text};

/// Messages queued by callbacks; the command loop owns and writes the output.
pub enum Progress {
    Event(SwitchEvent),
    /// The close / force-stop question with every process and running turn.
    CloseConsumers(DecisionRequest),
    WaitHint,
    /// A hint line (which flag answers the question).
    Hint(LocalizedText),
    /// An interactive question; the decider reads the answer from stdin.
    Prompt(LocalizedText),
}

/// One line per listed process: what it is, its pid, when it started, where it
/// runs, and what stopping it does.
fn render_processes(
    processes: &[ProcessInfo],
    busy: &[ThreadActivity],
    out: &mut Output,
) -> Result<()> {
    for p in processes {
        let mut line = text("cli.stop.process")
            .arg_text("role", superba_core::process_role(p).text())
            .arg("pid", p.pid)
            .arg("started", out.optional_time(p.started_at));
        line = match &p.cwd {
            Some(cwd) => line.arg("where", superba_core::paths::display_path(cwd)),
            None => line.arg("where", out.none()),
        };
        out.progress(line)?;
        out.progress(text("cli.stop.effect").arg_text("effect", superba_core::stop_effect(p)))?;
    }
    for thread in busy {
        out.progress(messages::busy_thread(thread))?;
    }
    if !busy.is_empty() {
        out.progress(text("cli.stop.busy"))?;
    }
    Ok(())
}

impl Progress {
    /// Render a queued event outside the service callback.
    pub fn render(self, out: &mut Output) -> Result<()> {
        match self {
            Self::WaitHint => out.progress(text("cli.hint.turns")),
            Self::Hint(value) => out.progress(value),
            Self::Prompt(value) => out.progress(value),
            Self::CloseConsumers(mut request) => {
                if let DecisionRequest::CloseConsumers { processes, .. }
                | DecisionRequest::ForceStop { processes, .. } = &mut request
                {
                    for process in processes {
                        process.cmdline = superba_core::app::redact_argv(&process.cmdline);
                    }
                }
                out.progress(messages::question(&request))?;
                match &request {
                    DecisionRequest::CloseConsumers { processes, busy }
                    | DecisionRequest::ForceStop { processes, busy } => {
                        render_processes(processes, busy, out)
                    }
                    _ => Ok(()),
                }
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
                SwitchEvent::Sessions { progress } => {
                    let done = progress.stage == SessionStage::Done;
                    out.live_progress(session_progress_line(&progress), done)?;
                    if done {
                        out.finish_progress()?;
                    }
                    Ok(())
                }
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
    /// Session progress throttle: (stage of the last line, earliest next line in ms).
    sessions: Mutex<Option<(SessionStage, u64)>>,
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
                sessions: Mutex::new(None),
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
        if let SwitchEvent::Sessions { progress } = &event
            && !self.session_line_due(progress.stage)
        {
            return;
        }
        self.send(Progress::Event(event));
    }
}

/// Shortest time between two session progress lines (a new stage and the end
/// always print).
const SESSION_LINE_MS: u64 = 250;

impl CliEvents {
    fn session_line_due(&self, stage: SessionStage) -> bool {
        let now = u64::try_from(self.clock.monotonic().as_millis()).unwrap_or(u64::MAX);
        let mut last = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let due = match *last {
            Some((seen, next)) => seen != stage || stage == SessionStage::Done || now >= next,
            None => true,
        };
        if due {
            *last = Some((stage, now.saturating_add(SESSION_LINE_MS)));
        }
        due
    }
}

/// Reads one answer line (stdin in production, injected in tests).
pub type PromptReader = Arc<dyn Fn() -> std::io::Result<String> + Send + Sync>;

/// Mid-execution answers from flags. The only stdin read while executing is the
/// "stop the Codex processes for you?" question (and its force follow-up) when
/// a prompt reader is attached (an interactive terminal without `--json`).
pub struct FlagDecider {
    pub force: bool,
    pub wait_turn: bool,
    pub yes: bool,
    /// `--stop-codex`: answer "stop them for me" without asking.
    pub stop_codex: bool,
    events: Option<Arc<CliEvents>>,
    prompt: Option<PromptReader>,
}

impl FlagDecider {
    /// Construct a decider without a progress sink (useful for embedding).
    pub fn new(force: bool, wait_turn: bool, yes: bool) -> Self {
        Self {
            force,
            wait_turn,
            yes,
            stop_codex: false,
            events: None,
            prompt: None,
        }
    }

    /// `--stop-codex`.
    pub fn with_stop_codex(mut self, stop_codex: bool) -> Self {
        self.stop_codex = stop_codex;
        self
    }

    /// Ask the stop questions interactively, reading answers with `reader`.
    pub fn with_prompt(mut self, reader: PromptReader) -> Self {
        self.prompt = Some(reader);
        self
    }

    fn send(&self, message: Progress) {
        if let Some(events) = &self.events {
            events.send(message);
        }
    }

    /// Ask `question` and read one answer line (lower-cased, trimmed); `None`
    /// without a prompt reader or when reading fails.
    async fn ask(&self, question: LocalizedText) -> Option<String> {
        let reader = self.prompt.clone()?;
        self.send(Progress::Prompt(question));
        let line = tokio::task::spawn_blocking(move || reader())
            .await
            .ok()?
            .ok()?;
        Some(line.trim().to_lowercase())
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
            // --force: end exactly the processes the report names (user bug 53).
            DecisionRequest::ForceQuit { .. } | DecisionRequest::AppStillRunning { .. } => {
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
                self.send(Progress::CloseConsumers(request));
                if self.stop_codex {
                    return Decision::StopProcesses;
                }
                if let Some(answer) = self.ask(text("cli.stop.prompt")).await {
                    return match answer.as_str() {
                        "s" | "stop" | "y" | "yes" => Decision::StopProcesses,
                        "c" | "continue" => Decision::Continue,
                        _ => Decision::Cancel,
                    };
                }
                if self.wait_turn {
                    Decision::Continue
                } else {
                    self.send(Progress::Hint(text("cli.hint.stop_codex")));
                    Decision::Cancel
                }
            }
            DecisionRequest::ForceStop { .. } => {
                self.send(Progress::CloseConsumers(request));
                if self.force {
                    return Decision::Force;
                }
                if let Some(answer) = self.ask(text("cli.stop.prompt_force")).await {
                    return match answer.as_str() {
                        "f" | "force" => Decision::Force,
                        "c" | "continue" => Decision::Continue,
                        _ => Decision::Cancel,
                    };
                }
                self.send(Progress::Hint(text("cli.hint.force_stop")));
                Decision::Cancel
            }
            _ => request.safe_default(),
        }
    }
}
