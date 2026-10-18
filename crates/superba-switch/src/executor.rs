//! Execution of a [`SwitchPlan`] (plan 7.2 states after the UI's countdown or
//! confirmation, plan 7.6 failure rules).
//!
//! Order: TurnCheck / WaitingTurn -> (full restart) Quitting [+ ForceQuitConfirm,
//! CloseConsumers] -> Applying under `state.lock` with a switch journal entry
//! (baseline, config, env, session migration; any failure rolls back what was
//! written) -> Restarting (turns re-checked right before the IPC) or Launching ->
//! Verifying (process, hashes, whoami, line, renderer) -> NavRestore / Recovery /
//! RollbackRestart -> remote hosts in parallel -> Done.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use superba_core::{
    AppSession, ApplyReport, AuthApiHealth, AuthMode, CodexFileKind, CodexHome, CodexProcessKind,
    ConsumersStopped, Decision, DecisionRequest, EnvApplied, Error, ExecuteOptions,
    HotRestartRequest, LaunchOptions, LocalizedText, MigrationReceipt, Notice, OutcomeStatus,
    ProcessInfo, QuitOptions, QuitOutcome, RemoteApplyReport, RemoteHost, RemoteOutcome,
    RemotePlan, RemoteVerifyRequest, RestartKind, Result, Severity, StepStatus, SwitchDecider,
    SwitchEvent, SwitchEvents, SwitchIntent, SwitchOutcome, SwitchPhase, SwitchPlan, SwitchStep,
    TargetId, ThreadActivity, ThreadRef, Timestamp, TurnActivity, Verification,
    codex::ENV_AUTHAPI_BASE_URL, plan::CheckResult,
};
use tokio::time::Instant;

use crate::Switcher;
use crate::journal::{JournalEntry, JournalStatus, RemoteReceipt, StateLock, SwitchJournal};
use crate::messages::{self, text};
use crate::planner::{PlanContext, effective_root, launch_cdp_mode};
use crate::rules;
use crate::session_events::SessionEvents;

pub use crate::rules::PROCESS_START_SLACK_MS;

/// A relaunched renderer can need several seconds to leave its startup screen.
/// Bound the wait for navigation readiness so a switch can still finish.
const NAV_RESTORE_WAIT: Duration = Duration::from_secs(15);

/// Sends events with timestamps.
struct Emitter<'a> {
    events: &'a dyn SwitchEvents,
    clock: &'a dyn superba_core::Clock,
}

impl Emitter<'_> {
    fn phase(&self, phase: SwitchPhase) {
        tracing::info!(phase = messages::phase_id(phase), "switch phase");
        self.events.emit(SwitchEvent::Phase {
            phase,
            at: self.clock.now(),
        });
    }

    fn step(&self, step: SwitchStep, status: StepStatus) {
        self.events.emit(SwitchEvent::Step {
            step,
            status,
            at: self.clock.now(),
        });
    }

    fn log(&self, level: Severity, t: LocalizedText) {
        self.events.emit(SwitchEvent::Log { level, text: t });
    }

    fn turns(&self, activity: Vec<TurnActivity>, waited: Duration) {
        self.events.emit(SwitchEvent::Turns {
            activity,
            waited_ms: waited.as_millis() as u64,
        });
    }
}

/// Ask the decider; answers outside `allowed()` count as `Cancel`.
async fn ask(decider: &dyn SwitchDecider, request: DecisionRequest) -> Decision {
    let allowed = request.allowed();
    let d = decider.decide(request).await;
    if allowed.contains(&d) {
        d
    } else {
        Decision::Cancel
    }
}

/// Result of waiting for turns.
enum TurnWait {
    Proceed,
    Cancel,
}

/// Receipts of the local writes of this execution.
#[derive(Default)]
struct Written {
    config: Option<ApplyReport>,
    env: Option<EnvApplied>,
    migration: Option<MigrationReceipt>,
    /// A model catalog file this execution created (removed on rollback).
    catalog: Option<std::path::PathBuf>,
}

impl Written {
    fn any(&self) -> bool {
        self.config.is_some() || self.env.is_some() || self.migration.is_some()
    }
}

/// Mutable state of one execution.
struct Run<'a> {
    sw: &'a Switcher,
    plan: &'a SwitchPlan,
    pc: PlanContext,
    opts: ExecuteOptions,
    decider: Arc<dyn SwitchDecider>,
    ev: Emitter<'a>,
    /// The execution's events, for the session migrator's progress reports.
    events: Arc<dyn SwitchEvents>,
    journal: SwitchJournal,
    entry: JournalEntry,
    written: Written,
    notices: Vec<Notice>,
    /// The app was quit by this execution and must be reopened.
    app_was_quit: bool,
    /// The thread open before quitting a session with CDP.
    thread_before_quit: Option<ThreadRef>,
    /// The session to use for IPCs (replaced after a relaunch).
    session: Option<AppSession>,
    /// The relaunched app-server runs the pinned Codex (`None` = not checked).
    pin_verified: Option<bool>,
}

/// How the local part ended.
struct LocalResult {
    status: OutcomeStatus,
    restart: RestartKind,
    verification: Option<Verification>,
    returned_to: Option<ThreadRef>,
}

impl Switcher {
    pub(crate) async fn execute_impl(
        &self,
        plan: SwitchPlan,
        opts: ExecuteOptions,
        decider: Arc<dyn SwitchDecider>,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome> {
        if let RestartKind::Blocked { reasons } = &plan.primary {
            return Err(Error::Blocked {
                reasons: reasons.clone(),
            });
        }
        if !plan.choices.is_empty() {
            return Err(Error::invalid(
                "the plan has unanswered questions; plan again with the answers",
            ));
        }
        let full =
            matches!(plan.primary, RestartKind::FullAppRestart { .. }) || plan.migration.is_some();
        if full && !opts.allow_full_restart && !opts.write_only {
            return Err(Error::Blocked {
                reasons: Vec::new(),
            });
        }
        let pc = self.take_plan(&plan.id).ok_or_else(|| Error::NotFound {
            what: format!(
                "plan {} (plans are executed once, by the service that made them)",
                plan.id
            ),
        })?;
        let _lock = StateLock::acquire(&self.deps.ctx.data_dir)?;
        let started = self.deps.clock.monotonic();
        let now = self.deps.clock.now();
        let journal = SwitchJournal::new(&self.deps.ctx.data_dir);
        let entry = JournalEntry {
            id: plan.id.clone(),
            started_at: now,
            updated_at: now,
            status: JournalStatus::Applying,
            codex_home: pc.home.root().to_path_buf(),
            summary: plan.to.label.render_fallback(),
            config: None,
            env: None,
            migration: None,
            remotes: Vec::new(),
            rollback: Default::default(),
        };
        let session = pc.session.clone();
        let mut run = Run {
            sw: self,
            plan: &plan,
            pc,
            opts,
            decider,
            ev: Emitter {
                events: events.as_ref(),
                clock: self.deps.clock.as_ref(),
            },
            events: events.clone(),
            journal,
            entry,
            written: Written::default(),
            notices: plan
                .warnings
                .iter()
                .filter(|w| w.severity >= Severity::Warning)
                .cloned()
                .collect(),
            app_was_quit: false,
            thread_before_quit: None,
            session,
            pin_verified: None,
        };
        let local = match run.local().await {
            Ok(l) => l,
            // Failed after writing (e.g. quitting for a full restart offered after a
            // failed IPC): the new configuration stays, the restart is left to the user.
            Err(e) if run.written.any() => {
                run.notices.push(Notice::danger(e.to_text()));
                LocalResult {
                    status: OutcomeStatus::WrittenOnly,
                    restart: RestartKind::None,
                    verification: None,
                    returned_to: None,
                }
            }
            Err(e) => {
                run.finish_journal(JournalStatus::RolledBack);
                return Err(e);
            }
        };
        let remotes = if matches!(
            local.status,
            OutcomeStatus::Cancelled | OutcomeStatus::RolledBack | OutcomeStatus::Failed
        ) {
            Vec::new()
        } else {
            run.remotes().await
        };
        let mut status = local.status;
        if remotes.iter().any(|r| r.status == OutcomeStatus::Failed)
            && matches!(status, OutcomeStatus::Switched | OutcomeStatus::WrittenOnly)
        {
            // Without a local part, every host failing is a failure (a settings
            // sync, a remote-only switch).
            status = if !plan.request.targets.local
                && remotes.iter().all(|r| r.status == OutcomeStatus::Failed)
            {
                OutcomeStatus::Failed
            } else {
                OutcomeStatus::Partial
            };
        }
        let final_journal = match status {
            OutcomeStatus::Failed if run.entry.status == JournalStatus::RollingBack => {
                JournalStatus::RollingBack
            }
            OutcomeStatus::RolledBack | OutcomeStatus::Cancelled => JournalStatus::RolledBack,
            _ if run.written.any() || !run.entry.remotes.is_empty() => JournalStatus::Done,
            _ => JournalStatus::RolledBack,
        };
        run.finish_journal(final_journal);
        if matches!(
            status,
            OutcomeStatus::Switched | OutcomeStatus::WrittenOnly | OutcomeStatus::Partial
        ) {
            run.after_success(&remotes).await;
        }
        let elapsed = self.deps.clock.monotonic().saturating_sub(started);
        let stale = run.stale_processes().await;
        run.summary_notices(
            status,
            elapsed,
            &remotes,
            &stale,
            local.returned_to.as_ref(),
        );
        run.ev.phase(SwitchPhase::Done);
        Ok(SwitchOutcome {
            plan_id: plan.id.clone(),
            status,
            elapsed_ms: elapsed.as_millis() as u64,
            generation: run.written.config.as_ref().map(|r| r.generation),
            restart: local.restart,
            verification: local.verification,
            remotes,
            stale_processes: stale,
            returned_to: local.returned_to,
            notices: run.notices,
        })
    }
}

impl Run<'_> {
    fn deps(&self) -> &crate::SwitcherDeps {
        &self.sw.deps
    }

    fn home(&self) -> CodexHome {
        self.pc.home.clone()
    }

    fn save_journal(&mut self) {
        self.entry.updated_at = self.deps().clock.now();
        if let Err(e) = self.journal.write(&self.entry) {
            tracing::warn!(error = %e, "cannot write the switch journal");
        }
    }

    fn finish_journal(&mut self, status: JournalStatus) {
        let recorded = self.entry.config.is_some()
            || self.entry.env.is_some()
            || self.entry.migration.is_some()
            || !self.entry.remotes.is_empty();
        if !recorded {
            // Nothing was written: the entry has no value for recovery or history.
            let path = self.journal.dir().join(format!("{}.json", self.entry.id));
            let _ = std::fs::remove_file(path);
            return;
        }
        self.entry.status = status;
        self.save_journal();
        let _ = self.journal.prune();
    }

    fn cancelled(&self, restart: RestartKind) -> LocalResult {
        LocalResult {
            status: OutcomeStatus::Cancelled,
            restart,
            verification: None,
            returned_to: None,
        }
    }

    // -----------------------------------------------------------------------
    // Local target
    // -----------------------------------------------------------------------

    async fn local(&mut self) -> Result<LocalResult> {
        let plan = self.plan;
        let primary = plan.primary.clone();
        let write_only = self.opts.write_only;

        if !plan.request.targets.local {
            // Remote hosts only (settings sync, remote switch): their app-servers
            // restart, so wait for their running turns first.
            if !write_only
                && !self.remote_hosts_to_apply().is_empty()
                && let TurnWait::Cancel = self.wait_for_turns(true).await?
            {
                return Ok(self.cancelled(RestartKind::None));
            }
            return Ok(LocalResult {
                status: OutcomeStatus::Switched,
                restart: RestartKind::None,
                verification: None,
                returned_to: None,
            });
        }

        // A pinned store version is downloaded and verified before anything else
        // (nothing is written yet, so a failure ends here).
        if let Some(version) = self.pc.pin.as_ref().and_then(|p| p.ensure.clone()) {
            let set = self.deps().profiles.load().await?;
            let stored = (self.deps().versions)(&set.settings.network)
                .ensure(&version)
                .await?;
            if let Some(pin) = self.pc.pin.as_mut() {
                pin.exe = Some(stored.executable);
                pin.ensure = None;
            }
        }

        // A pin change for a home whose config.toml names a model catalog file:
        // the new Codex must accept the file before the app restarts with it, else
        // the file is rebuilt for it (or left out). Nothing is written yet, so a
        // configuration that cannot be repaired blocks the pin here.
        if let Some(pin) = self.pc.pin.clone().filter(|p| p.check_catalog) {
            match self
                .sw
                .pin_catalog_rewrite(&pin, self.pc.install_id.as_deref())
                .await
            {
                Ok(None) => {}
                Ok(Some(r)) => {
                    self.notices.extend(r.notes);
                    self.pc.catalog = r.catalog;
                    self.pc.catalog_record = r.record;
                    self.pc.pin_config = Some(r.config);
                }
                Err(reason) => {
                    return Err(Error::Blocked {
                        reasons: vec![superba_core::BlockReason::ModelCatalog { reason }],
                    });
                }
            }
        }

        // TurnCheck / WaitingTurn (nothing written yet: cancel is free).
        if !write_only && primary.interrupts_turns() {
            self.ev.phase(SwitchPhase::TurnCheck);
            if let TurnWait::Cancel = self.wait_for_turns(true).await? {
                return Ok(self.cancelled(RestartKind::None));
            }
        }

        let full = !write_only
            && (matches!(primary, RestartKind::FullAppRestart { .. })
                || plan.migration.is_some()
                || self.pc.rollback_migration.is_some());
        if full && let Some(result) = self.quit_for_full_restart().await? {
            return Ok(result);
        }

        // Applying.
        self.ev.phase(SwitchPhase::Applying);
        if let Err(e) = self.apply_local(full).await {
            if matches!(e, Error::Cancelled) {
                // The user stopped the session migration: it undid itself.
                return Ok(self.roll_back_after_stop().await);
            }
            self.ev.log(Severity::Danger, e.to_text());
            return Ok(self.roll_back_after_failure(&e).await);
        }
        self.entry.status = JournalStatus::Committed;
        self.save_journal();

        if write_only {
            if plan.migration.is_some() {
                self.notices
                    .push(Notice::warning(text("switch.warn.write_only_migration")));
            }
            let verification = self.verify(None, None, false).await;
            return Ok(LocalResult {
                status: OutcomeStatus::WrittenOnly,
                restart: RestartKind::None,
                verification: Some(verification),
                returned_to: None,
            });
        }

        match &primary {
            RestartKind::AppServerHot { .. } => self.hot_path().await,
            RestartKind::FullAppRestart { .. } => self.launch_path(primary.clone()).await,
            _ if full || self.pc.restart_app => self.launch_path(primary.clone()).await,
            _ => {
                // Nothing to restart (app not running): plan 7.3 "takes effect next time".
                let verification = self.verify(None, None, false).await;
                if self.session.is_none() && !self.pc.restart_app {
                    self.notices
                        .push(Notice::info(text("switch.done.not_running")));
                }
                Ok(LocalResult {
                    status: OutcomeStatus::Switched,
                    restart: RestartKind::None,
                    verification: Some(verification),
                    returned_to: None,
                })
            }
        }
    }

    /// Current turn activity of the local app and the plan's remote hosts.
    async fn activity(&self, include_remotes: bool) -> Vec<TurnActivity> {
        let d = self.deps();
        let mut out = Vec::new();
        if self.plan.request.targets.local {
            match d
                .app
                .turn_activity(&self.pc.home, self.session.as_ref())
                .await
            {
                Ok(a) => out.push(a),
                Err(e) => tracing::debug!(error = %e, "local turn activity unavailable"),
            }
        }
        if include_remotes {
            let futs = self
                .remote_hosts_to_apply()
                .into_iter()
                .map(|(host, _)| async move { d.remote.turn_activity(&host).await });
            for r in join_all(futs).await {
                match r {
                    Ok(a) => out.push(a),
                    Err(e) => tracing::debug!(error = %e, "remote turn activity unavailable"),
                }
            }
        }
        out
    }

    /// Wait until no selected target has a turn in flight (plan 7.2 WaitingTurn).
    /// The decider's `WaitForTurns` question stays pending while polling continues;
    /// it is dropped when the turns end on their own.
    async fn wait_for_turns(&mut self, include_remotes: bool) -> Result<TurnWait> {
        if self.opts.force_turns {
            return Ok(TurnWait::Proceed);
        }
        let poll = self.sw.tuning.poll;
        let quiet = Duration::from_millis(self.opts.turn_quiet_ms);
        let reask = self.sw.tuning.reask_after;
        let started = Instant::now();
        let mut waited_once = false;
        let mut asked_unknown = false;
        loop {
            let activity = self.activity(include_remotes).await;
            let unknown: Vec<TargetId> = activity
                .iter()
                .filter(|a| !a.complete && !a.target_id.is_local())
                .map(|a| a.target_id.clone())
                .collect();
            if !unknown.is_empty() && !asked_unknown {
                asked_unknown = true;
                let d = ask(
                    self.decider.as_ref(),
                    DecisionRequest::UnknownRemoteActivity { targets: unknown },
                )
                .await;
                if d != Decision::Continue {
                    return Ok(TurnWait::Cancel);
                }
            }
            let busy: Vec<ThreadActivity> = activity.iter().flat_map(|a| a.busy.clone()).collect();
            if busy.is_empty() {
                if !waited_once || quiet.is_zero() {
                    return Ok(TurnWait::Proceed);
                }
                // Quiet period after the last turn ended (plan 7.2: "all finished
                // and quiet for 2 s").
                tokio::time::sleep(quiet).await;
                let again = self.activity(include_remotes).await;
                if again.iter().all(|a| !a.is_busy()) {
                    return Ok(TurnWait::Proceed);
                }
                continue;
            }
            if !waited_once {
                waited_once = true;
                self.ev.phase(SwitchPhase::WaitingTurn);
            }
            self.ev.turns(activity.clone(), started.elapsed());

            // Ask, and keep polling while the question is open.
            let question = DecisionRequest::WaitForTurns {
                busy: busy.clone(),
                waited_ms: started.elapsed().as_millis() as u64,
            };
            let decider = Arc::clone(&self.decider);
            let mut answer = Box::pin(async move { ask(decider.as_ref(), question).await });
            let asked_at = Instant::now();
            let decision = loop {
                tokio::select! {
                    d = &mut answer => break Some(d),
                    _ = tokio::time::sleep(poll) => {
                        let now = self.activity(include_remotes).await;
                        if now.iter().all(|a| !a.is_busy()) {
                            break None;
                        }
                        self.ev.turns(now, started.elapsed());
                    }
                }
            };
            match decision {
                // Turns ended by themselves: drop the question, re-check (quiet period).
                None => continue,
                Some(Decision::Force) => return Ok(TurnWait::Proceed),
                Some(Decision::Wait) => {
                    // Keep waiting quietly; ask again after `reask`.
                    let deadline = asked_at + reask;
                    loop {
                        tokio::time::sleep(poll).await;
                        let now = self.activity(include_remotes).await;
                        if now.iter().all(|a| !a.is_busy()) || Instant::now() >= deadline {
                            break;
                        }
                        self.ev.turns(now, started.elapsed());
                    }
                }
                Some(_) => return Ok(TurnWait::Cancel),
            }
        }
    }

    /// Quitting (+ ForceQuitConfirm, CloseConsumers). Returns `Some(result)` when the
    /// user cancelled (nothing written).
    async fn quit_for_full_restart(&mut self) -> Result<Option<LocalResult>> {
        let d = self.deps().clone();
        let needs_migration = self.plan.migration.is_some() || self.pc.rollback_migration.is_some();
        if let Some(session) = self.session.clone() {
            self.ev.phase(SwitchPhase::Quitting);
            self.ev.step(SwitchStep::QuitApp, StepStatus::Running);
            if session.cdp.is_some() {
                self.thread_before_quit = d.app.current_thread(&session).await.ok().flatten();
            }
            let graceful = QuitOptions {
                graceful_timeout_ms: self.opts.quit_timeout_ms,
                force: false,
            };
            let mut outcome = d.app.quit(&session, &graceful).await;
            if let Ok(QuitOutcome::TimedOut) = outcome {
                self.ev.phase(SwitchPhase::ForceQuitConfirm);
                let decision = if self.opts.force_turns {
                    Decision::Force
                } else {
                    ask(
                        self.decider.as_ref(),
                        DecisionRequest::ForceQuit {
                            pids: vec![session.main_pid],
                        },
                    )
                    .await
                };
                if decision != Decision::Force {
                    self.ev.step(SwitchStep::QuitApp, StepStatus::Skipped);
                    return Ok(Some(self.cancelled(self.plan.primary.clone())));
                }
                outcome = d
                    .app
                    .quit(
                        &session,
                        &QuitOptions {
                            graceful_timeout_ms: self.opts.quit_timeout_ms,
                            force: true,
                        },
                    )
                    .await;
            }
            match outcome {
                Ok(QuitOutcome::TimedOut) => {
                    let e = Error::Timeout {
                        what: "app quit".into(),
                    };
                    self.ev.step(
                        SwitchStep::QuitApp,
                        StepStatus::Failed {
                            message: e.to_string(),
                        },
                    );
                    return Err(e);
                }
                Ok(_) => {
                    self.app_was_quit = true;
                    self.ev.step(SwitchStep::QuitApp, StepStatus::Done);
                }
                Err(e) => {
                    self.ev.step(
                        SwitchStep::QuitApp,
                        StepStatus::Failed {
                            message: e.to_string(),
                        },
                    );
                    return Err(e);
                }
            }
        }
        if needs_migration {
            // Session migration needs every consumer of the home stopped (plan 10.2):
            // stop the daemon, ask the user to close TUI / IDE processes.
            let home = self.home();
            let processes = d.app.processes(&home).await.unwrap_or_default();
            let mut daemon_left = Vec::new();
            if processes
                .iter()
                .any(|p| matches!(p.kind, CodexProcessKind::CliDaemon))
                && let Err(e) = d.app.stop_daemon(&home).await
            {
                tracing::warn!(error = %e, "cannot stop the Codex daemon");
                daemon_left = processes
                    .iter()
                    .filter(|p| matches!(p.kind, CodexProcessKind::CliDaemon))
                    .cloned()
                    .collect();
            }
            loop {
                let processes = d.app.processes(&home).await.unwrap_or_default();
                let mut blockers: Vec<ProcessInfo> =
                    rules::migration_blockers(&processes, home.root())
                        .into_iter()
                        .cloned()
                        .collect();
                blockers.extend(
                    daemon_left
                        .iter()
                        .filter(|p| processes.iter().any(|q| q.pid == p.pid))
                        .cloned(),
                );
                if blockers.is_empty() {
                    break;
                }
                let decision = ask(
                    self.decider.as_ref(),
                    DecisionRequest::CloseConsumers {
                        processes: blockers,
                    },
                )
                .await;
                if decision != Decision::Continue {
                    self.reopen_after_cancel().await;
                    return Ok(Some(self.cancelled(self.plan.primary.clone())));
                }
                tokio::time::sleep(self.sw.tuning.poll).await;
            }
        }
        Ok(None)
    }

    /// Reopen the app the way it was after a cancelled full restart (plan 7.2
    /// ForceQuitConfirm -> Idle "reopen as it was").
    async fn reopen_after_cancel(&mut self) {
        if !self.app_was_quit {
            return;
        }
        if let Err(e) = self.launch_app(true).await {
            let failed = text("switch.failed.launch").arg_text("detail", e.to_text());
            self.ev.log(Severity::Danger, failed.clone());
            self.notices.push(Notice::danger(failed));
        }
    }

    /// Applying: baseline, config, env, migration. Each receipt is journaled before
    /// the next step; any error leaves the receipts for the caller to roll back.
    async fn apply_local(&mut self, full: bool) -> Result<()> {
        let d = self.deps().clone();
        let home = self.home();
        let plan = self.plan;
        self.save_journal();

        // Router mode: reorder the members first, so the router already prefers
        // the destination when Codex restarts (nothing is written before this).
        if let Some((members, built)) = self.pc.router_update.clone() {
            d.router.update(built.config, built.keys).await?;
            match d.profiles.load().await {
                Ok(mut set) => {
                    set.settings.router.members = members;
                    if let Err(e) = d.profiles.save(&set).await {
                        tracing::warn!(error = %e, "cannot store the router member order");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot store the router member order"),
            }
        }

        // The model catalog file exists before config.toml names it
        // (content-addressed: never rewritten under a running app-server).
        // Also when config.toml already names it: a file removed by hand is put
        // back, so the key never points at nothing.
        if let Some(cw) = self.pc.catalog.clone()
            && crate::models::write_catalog_file(&cw)?
        {
            self.written.catalog = Some(cw.path.clone());
        }

        // A pin change may rewrite config.toml for its new Codex (catalog file).
        let config = plan.config.clone().or_else(|| self.pc.pin_config.clone());
        let writes_config = config.as_ref().is_some_and(|c| !c.is_noop());
        // Restoring a released original must keep using its saved baseline. It
        // does not begin another takeover of the user's current configuration.
        let releases_baseline = config.as_ref().is_some_and(|p| {
            matches!(
                p.desired.as_ref().map(|d| &d.mode),
                Some(
                    superba_core::ResolvedMode::Original { .. }
                        | superba_core::ResolvedMode::Official
                )
            )
        });
        if writes_config
            && self.pc.baseline_missing
            && !(self.pc.baseline_released && releases_baseline)
        {
            d.config
                .snapshot_baseline(&home, self.pc.baseline_extras.clone())
                .await?;
        }

        match config.as_ref().filter(|c| !c.is_noop()) {
            Some(cp) => {
                self.ev.step(SwitchStep::WriteConfig, StepStatus::Running);
                match d.config.apply(cp).await {
                    Ok(report) => {
                        self.entry.config = Some(report.clone());
                        self.written.config = Some(report);
                        self.save_journal();
                        self.ev.step(SwitchStep::WriteConfig, StepStatus::Done);
                    }
                    Err(e) => {
                        self.ev.step(
                            SwitchStep::WriteConfig,
                            StepStatus::Failed {
                                message: e.to_string(),
                            },
                        );
                        return Err(e);
                    }
                }
            }
            None => self.ev.step(SwitchStep::WriteConfig, StepStatus::Skipped),
        }

        match plan.env.as_ref().filter(|e| !e.is_noop()) {
            Some(ep) => {
                self.ev.step(SwitchStep::WriteEnv, StepStatus::Running);
                match d.env.apply(ep).await {
                    Ok(applied) => {
                        self.entry.env = Some(applied.clone());
                        self.written.env = Some(applied);
                        self.save_journal();
                        self.ev.step(SwitchStep::WriteEnv, StepStatus::Done);
                    }
                    Err(e) => {
                        self.ev.step(
                            SwitchStep::WriteEnv,
                            StepStatus::Failed {
                                message: e.to_string(),
                            },
                        );
                        return Err(e);
                    }
                }
            }
            None => self.ev.step(SwitchStep::WriteEnv, StepStatus::Skipped),
        }

        if full {
            let stopped = ConsumersStopped::new(&home, d.clock.now());
            if let Some(mp) = &plan.migration {
                self.ev
                    .step(SwitchStep::MigrateSessions, StepStatus::Running);
                let progress = SessionEvents::sink(self.events.clone());
                match d
                    .sessions
                    .migrate_stopped_with_progress(&home, mp, &stopped, progress)
                    .await
                {
                    Ok(receipt) => {
                        self.entry.migration = Some(receipt.clone());
                        self.written.migration = Some(receipt);
                        self.save_journal();
                        self.ev.step(SwitchStep::MigrateSessions, StepStatus::Done);
                    }
                    Err(e) => {
                        let status = if matches!(e, Error::Cancelled) {
                            StepStatus::Skipped
                        } else {
                            StepStatus::Failed {
                                message: e.to_string(),
                            }
                        };
                        self.ev.step(SwitchStep::MigrateSessions, status);
                        return Err(e);
                    }
                }
            } else if let Some(id) = self.pc.rollback_migration.clone() {
                self.ev
                    .step(SwitchStep::MigrateSessions, StepStatus::Running);
                let progress = SessionEvents::sink(self.events.clone());
                if let Err(e) = d
                    .sessions
                    .rollback_stopped_with_progress(&home, &id, &stopped, progress)
                    .await
                {
                    self.ev.step(
                        SwitchStep::MigrateSessions,
                        StepStatus::Failed {
                            message: e.to_string(),
                        },
                    );
                    return Err(e);
                }
                self.ev.step(SwitchStep::MigrateSessions, StepStatus::Done);
            }
        }
        Ok(())
    }

    /// Undo local writes in reverse order, stopping at the first failure so
    /// dependent config/env changes stay aligned with the session migration.
    async fn undo_local(&mut self) -> Vec<Error> {
        let d = self.deps().clone();
        let home = self.home();
        // Persist the undo intent before touching anything: a crash must not
        // leave a Committed entry that recovery would finish forwards.
        self.entry.status = JournalStatus::RollingBack;
        self.entry.updated_at = d.clock.now();
        if let Err(e) = self.journal.write(&self.entry) {
            return vec![e];
        }
        if let Some(receipt) = self.written.migration.take() {
            let stopped = ConsumersStopped::new(&home, d.clock.now());
            let progress = SessionEvents::sink(self.events.clone());
            if let Err(e) = d
                .sessions
                .rollback_stopped_with_progress(&home, &receipt.id, &stopped, progress)
                .await
            {
                self.written.migration = Some(receipt);
                return vec![e];
            }
            self.entry.rollback.migration = true;
            if let Err(e) = self.journal.write(&self.entry) {
                return vec![e];
            }
        }
        if let Some(applied) = self.written.env.take() {
            if let Err(e) = d.env.revert(&applied).await {
                self.written.env = Some(applied);
                return vec![e];
            }
            self.entry.rollback.env = true;
            if let Err(e) = self.journal.write(&self.entry) {
                return vec![e];
            }
        }
        if let Some(report) = self.written.config.take() {
            if let Err(e) = d.config.rollback(&report).await {
                self.written.config = Some(report);
                return vec![e];
            }
            self.entry.rollback.config = true;
            if let Err(e) = self.journal.write(&self.entry) {
                return vec![e];
            }
        }
        // config.toml no longer names a catalog this execution created.
        if let Some(path) = self.written.catalog.take()
            && let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(error = %e, "cannot remove an unused catalog file");
        }
        Vec::new()
    }

    /// RollingBack after a failed write (plan 7.6 #1): restore, reopen the app if
    /// it was quit, report.
    async fn roll_back_after_failure(&mut self, cause: &Error) -> LocalResult {
        self.ev.phase(SwitchPhase::RollingBack);
        let errors = self.undo_local().await;
        self.reopen_after_cancel().await;
        if errors.is_empty() {
            self.notices.push(Notice::danger(
                text("switch.failed.apply").arg_text("detail", cause.to_text()),
            ));
            LocalResult {
                status: OutcomeStatus::RolledBack,
                restart: RestartKind::None,
                verification: None,
                returned_to: None,
            }
        } else {
            let detail = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            self.notices.push(Notice::danger(
                text("switch.failed.rollback").arg("detail", detail),
            ));
            LocalResult {
                status: OutcomeStatus::Failed,
                restart: RestartKind::None,
                verification: None,
                returned_to: None,
            }
        }
    }

    /// The user stopped the session migration (it restored every session
    /// itself): undo the config / env written before it, reopen the app.
    async fn roll_back_after_stop(&mut self) -> LocalResult {
        self.ev.phase(SwitchPhase::RollingBack);
        let errors = self.undo_local().await;
        self.reopen_after_cancel().await;
        if errors.is_empty() {
            self.notices
                .push(Notice::info(text("switch.sessions.stopped")));
            return self.cancelled(RestartKind::None);
        }
        let detail = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        self.notices.push(Notice::danger(
            text("switch.failed.rollback").arg("detail", detail),
        ));
        LocalResult {
            status: OutcomeStatus::Failed,
            restart: RestartKind::None,
            verification: None,
            returned_to: None,
        }
    }

    /// Undo after the user cancelled or chose "undo" once files were written; the
    /// app-server was not restarted, so files are enough.
    async fn undo_written(&mut self, restart_again: bool) -> LocalResult {
        self.ev.phase(SwitchPhase::RollingBack);
        let errors = self.undo_local().await;
        if errors.is_empty()
            && restart_again
            && let Some(s) = self.session.clone()
        {
            let _ = self
                .deps()
                .app
                .hot_restart(&s, &HotRestartRequest::local())
                .await;
        }
        if errors.is_empty() {
            LocalResult {
                status: OutcomeStatus::RolledBack,
                restart: RestartKind::None,
                verification: None,
                returned_to: None,
            }
        } else {
            let detail = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            self.notices.push(Notice::danger(
                text("switch.failed.rollback").arg("detail", detail),
            ));
            LocalResult {
                status: OutcomeStatus::Failed,
                restart: RestartKind::None,
                verification: None,
                returned_to: None,
            }
        }
    }

    /// Restarting -> Verifying -> NavRestore / Recovery (hot path).
    async fn hot_path(&mut self) -> Result<LocalResult> {
        let d = self.deps().clone();
        let Some(session) = self.session.clone() else {
            return Err(Error::invalid("hot restart planned without a running app"));
        };
        self.ev.phase(SwitchPhase::Restarting);
        // Re-check right before the IPC (plan 7.4): a turn started during apply.
        if !self.opts.force_turns {
            let now = self.activity(false).await;
            if now.iter().any(TurnActivity::is_busy) {
                match self.wait_for_turns(false).await? {
                    TurnWait::Proceed => self.ev.phase(SwitchPhase::Restarting),
                    TurnWait::Cancel => return Ok(self.undo_written(false).await),
                }
            }
        }
        let thread = d.app.current_thread(&session).await.ok().flatten();
        let before = self.app_server_pids().await;
        let restart = RestartKind::AppServerHot {
            host_id: superba_core::codex::LOCAL_HOST_ID.into(),
            kill_remote: false,
        };
        self.ev.step(SwitchStep::RestartCore, StepStatus::Running);
        let t0 = match d
            .app
            .hot_restart(&session, &HotRestartRequest::local())
            .await
        {
            Ok(o) => {
                self.ev.step(SwitchStep::RestartCore, StepStatus::Done);
                o.sent_at
            }
            Err(e) => {
                self.ev.step(
                    SwitchStep::RestartCore,
                    StepStatus::Failed {
                        message: e.to_string(),
                    },
                );
                return self.ipc_failed(e).await;
            }
        };

        self.ev.phase(SwitchPhase::Verifying);
        let mut verification = self.verify(Some(&session), Some((t0, &before)), true).await;
        if !verification.passed() {
            self.ev.phase(SwitchPhase::Recovery);
            if rules::retry_ipc_helps(&verification)
                && let Ok(o) = d
                    .app
                    .hot_restart(&session, &HotRestartRequest::local())
                    .await
            {
                let before = self.app_server_pids().await;
                self.ev.phase(SwitchPhase::Verifying);
                verification = self
                    .verify(Some(&session), Some((o.sent_at, &before)), true)
                    .await;
            }
        }
        if !verification.passed() {
            return self.verification_failed(verification, restart).await;
        }
        let returned_to = self.nav_restore(&session, thread).await;
        Ok(LocalResult {
            status: OutcomeStatus::Switched,
            restart,
            verification: Some(verification),
            returned_to,
        })
    }

    /// FullRestartOffer (plan 7.6 #2).
    async fn ipc_failed(&mut self, error: Error) -> Result<LocalResult> {
        self.ev.phase(SwitchPhase::FullRestartOffer);
        self.notices.push(Notice::warning(
            text("switch.failed.ipc").arg_text("detail", error.to_text()),
        ));
        match ask(self.decider.as_ref(), DecisionRequest::IpcFailed { error }).await {
            Decision::FullRestart => {
                if let Some(r) = self.quit_for_full_restart().await? {
                    return Ok(r);
                }
                self.launch_path(RestartKind::FullAppRestart {
                    reason: superba_core::FullRestartReason::NoBridge,
                })
                .await
            }
            Decision::KeepWritten => Ok(LocalResult {
                status: OutcomeStatus::WrittenOnly,
                restart: RestartKind::None,
                verification: None,
                returned_to: None,
            }),
            _ => Ok(self.undo_written(false).await),
        }
    }

    /// Recovery after a failed verification (plan 7.6 #3).
    async fn verification_failed(
        &mut self,
        verification: Verification,
        restart: RestartKind,
    ) -> Result<LocalResult> {
        let summary = rules::verification_summary(&verification);
        let decision = ask(
            self.decider.as_ref(),
            DecisionRequest::VerificationFailed {
                verification: verification.clone(),
            },
        )
        .await;
        match decision {
            Decision::FullRestart => {
                if let Some(r) = self.quit_for_full_restart().await? {
                    return Ok(r);
                }
                self.launch_path(RestartKind::FullAppRestart {
                    reason: superba_core::FullRestartReason::AuthApiUnhealthy,
                })
                .await
            }
            Decision::KeepWritten => {
                self.notices.push(Notice::warning(
                    text("switch.question.verification_failed").arg("detail", &summary),
                ));
                Ok(LocalResult {
                    status: OutcomeStatus::WrittenOnly,
                    restart,
                    verification: Some(verification),
                    returned_to: None,
                })
            }
            _ => {
                // RollbackRestart: restore files and restart the core on them again.
                self.ev.phase(SwitchPhase::RollbackRestart);
                let hot = restart.is_hot();
                let mut result = self.undo_written(hot).await;
                if result.status == OutcomeStatus::RolledBack {
                    self.notices.push(Notice::danger(
                        text("switch.failed.verification").arg("detail", &summary),
                    ));
                }
                result.verification = Some(verification);
                Ok(result)
            }
        }
    }

    /// Launching -> Verifying -> NavRestore (full restart or reopening after migration).
    async fn launch_path(&mut self, restart: RestartKind) -> Result<LocalResult> {
        let was_running = self.app_was_quit || self.pc.restart_app;
        if !was_running {
            // The app was not running: nothing to relaunch (plan 7.3 "app not running").
            self.notices
                .push(Notice::info(text("switch.done.not_running")));
            let verification = self.verify(None, None, false).await;
            return Ok(LocalResult {
                status: OutcomeStatus::Switched,
                restart: RestartKind::None,
                verification: Some(verification),
                returned_to: None,
            });
        }
        let t0 = self.deps().clock.now();
        match self.launch_app(false).await {
            Ok(session) => {
                self.ev.phase(SwitchPhase::Verifying);
                let verification = self.verify(Some(&session), Some((t0, &[])), true).await;
                if session.cdp.is_none() {
                    self.notices
                        .push(Notice::warning(text("switch.done.hot_unavailable")));
                }
                self.check_pin(&session).await;
                let (status, returned_to) = if verification.passed() {
                    (
                        OutcomeStatus::Switched,
                        self.nav_restore_after_launch(&session).await,
                    )
                } else {
                    self.notices.push(Notice::warning(
                        text("switch.question.verification_failed")
                            .arg("detail", rules::verification_summary(&verification)),
                    ));
                    (OutcomeStatus::WrittenOnly, None)
                };
                Ok(LocalResult {
                    status,
                    restart,
                    verification: Some(verification),
                    returned_to,
                })
            }
            Err(e) => {
                // The new configuration is complete and journaled, so it stays;
                // but the app Superba quit is not running again. Say so at once
                // (the activity log, the CLI progress) and in the outcome, which
                // tells the user to open the app themselves.
                let failed = text("switch.failed.launch").arg_text("detail", e.to_text());
                self.ev.log(Severity::Danger, failed.clone());
                self.notices.push(Notice::danger(failed));
                Ok(LocalResult {
                    status: OutcomeStatus::WrittenOnly,
                    restart,
                    verification: None,
                    returned_to: None,
                })
            }
        }
    }

    /// Launch the app through Superba (CDP unless safe mode or root, the auth
    /// address of the current line in its environment).
    async fn launch_app(&mut self, reopen: bool) -> Result<AppSession> {
        let d = self.deps().clone();
        if !reopen {
            self.ev.phase(SwitchPhase::Launching);
        }
        self.ev.step(SwitchStep::LaunchApp, StepStatus::Running);
        let mut env = BTreeMap::new();
        let origin = match (&self.session, reopen) {
            // Reopen exactly as before: keep the auth address it had.
            (Some(s), true) => s.authapi_env.value().map(str::to_owned),
            _ => self
                .pc
                .endpoints
                .as_ref()
                .and_then(|e| e.authapi_origin.as_ref())
                .or(self.pc.launch_authapi.as_ref())
                .map(|o| o.as_str()),
        };
        if let Some(o) = origin {
            env.insert(ENV_AUTHAPI_BASE_URL.to_owned(), o);
        }
        let mut options = LaunchOptions {
            install_id: self.pc.install_id.clone(),
            cdp: launch_cdp_mode(self.pc.safe_mode, effective_root()),
            env,
            env_remove: Vec::new(),
            extra_args: Vec::new(),
            home_override: self.session.as_ref().and_then(|s| s.home_override.clone()),
            ready_timeout_ms: 0,
        };
        // The app's Codex: a pin plan's new pin (not when reopening after a
        // cancel), else the configured one.
        options = match self.pc.pin.as_ref().filter(|_| !reopen) {
            Some(pin) if crate::versions::PIN_SUPPORTED => {
                crate::versions::apply_pin(&mut options, pin.exe.as_deref());
                options
            }
            // Uninstall hands the app back to its own Codex.
            _ if self.pc.uninstall.is_some() && crate::versions::PIN_SUPPORTED => {
                crate::versions::apply_pin(&mut options, None);
                options
            }
            _ => self.sw.with_pin(options).await,
        };
        match d.app.launch(&options).await {
            Ok(s) => {
                self.ev.step(SwitchStep::LaunchApp, StepStatus::Done);
                self.session = Some(s.clone());
                self.app_was_quit = false;
                Ok(s)
            }
            Err(e) => {
                self.ev.step(
                    SwitchStep::LaunchApp,
                    StepStatus::Failed {
                        message: e.to_string(),
                    },
                );
                Err(e)
            }
        }
    }

    /// Wait for the new renderer before using the same navigation as the hot path.
    async fn nav_restore_after_launch(&mut self, session: &AppSession) -> Option<ThreadRef> {
        session.cdp.as_ref()?;
        let thread = self.thread_before_quit.take()?;
        let d = self.deps().clone();
        tokio::time::timeout(NAV_RESTORE_WAIT, async {
            loop {
                if let Ok(state) = d.app.renderer_state(session).await
                    && !state.on_login_page
                    && (state.on_home || state.current_thread.is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .ok()?;
        self.nav_restore(session, Some(thread)).await
    }

    async fn nav_restore(
        &mut self,
        session: &AppSession,
        thread: Option<ThreadRef>,
    ) -> Option<ThreadRef> {
        let thread = thread?;
        let d = self.deps().clone();
        let state = d.app.renderer_state(session).await.ok()?;
        if !state.on_home || state.current_thread.is_some() {
            return None;
        }
        self.ev.phase(SwitchPhase::NavRestore);
        match d.app.return_to_thread(session, &thread).await {
            Ok(()) => Some(thread),
            Err(e) => {
                tracing::debug!(error = %e, "cannot return to the thread");
                None
            }
        }
    }

    async fn app_server_pids(&self) -> Vec<u32> {
        self.deps()
            .app
            .processes(&self.pc.home)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|p| matches!(p.kind, CodexProcessKind::AppServer))
            .map(|p| p.pid)
            .collect()
    }

    // -----------------------------------------------------------------------
    // Verification (plan 7.2 Verifying)
    // -----------------------------------------------------------------------

    /// Run the checks. `restarted` = (T0, app-server pids before the restart).
    async fn verify(
        &self,
        session: Option<&AppSession>,
        restarted: Option<(Timestamp, &[u32])>,
        renderer: bool,
    ) -> Verification {
        let v = Verification {
            process: self.verify_process(session, restarted).await,
            files: self.verify_files().await,
            auth: self.verify_auth(session).await,
            line: self.verify_line().await,
            renderer: if renderer {
                self.verify_renderer(session).await
            } else {
                skipped(text("switch.verify.not_applicable"))
            },
        };
        for (step, check) in [
            (SwitchStep::VerifyProcess, &v.process),
            (SwitchStep::VerifyFiles, &v.files),
            (SwitchStep::VerifyAuth, &v.auth),
            (SwitchStep::VerifyLine, &v.line),
            (SwitchStep::VerifyRenderer, &v.renderer),
        ] {
            let status = match check {
                CheckResult::Passed => StepStatus::Done,
                CheckResult::Failed { detail } => StepStatus::Failed {
                    message: detail.clone(),
                },
                _ => StepStatus::Skipped,
            };
            self.ev.step(step, status);
        }
        v
    }

    async fn verify_process(
        &self,
        session: Option<&AppSession>,
        restarted: Option<(Timestamp, &[u32])>,
    ) -> CheckResult {
        let (Some(_), Some((t0, before))) = (session, restarted) else {
            return skipped(text("switch.verify.not_applicable"));
        };
        let d = self.deps();
        let deadline = Instant::now() + self.sw.tuning.verify_timeout;
        let is_new = |p: &ProcessInfo| {
            matches!(p.kind, CodexProcessKind::AppServer)
                && !before.contains(&p.pid)
                && p.started_at
                    .is_none_or(|s| s.as_millis() + PROCESS_START_SLACK_MS >= t0.as_millis())
        };
        // Whether app-server processes are observable at all on this system; if
        // none was ever seen, a missing one is not evidence of a failed restart.
        let mut seen_any = !before.is_empty();
        loop {
            let processes = match d.app.processes(&self.pc.home).await {
                Ok(p) => p,
                Err(e) if e.is_not_implemented() => {
                    return skipped(text("switch.verify.not_supported"));
                }
                // Not being able to list processes says nothing about the switch.
                Err(e) => {
                    return CheckResult::Skipped {
                        reason: e.to_string(),
                    };
                }
            };
            seen_any |= processes
                .iter()
                .any(|p| matches!(p.kind, CodexProcessKind::AppServer));
            let new: Vec<u32> = processes
                .iter()
                .filter(|p| is_new(p))
                .map(|p| p.pid)
                .collect();
            let last_old_alive = processes
                .iter()
                .any(|p| matches!(p.kind, CodexProcessKind::AppServer) && before.contains(&p.pid));
            if !new.is_empty() && !last_old_alive {
                // Stable: still alive after the grace period (no crash loop).
                if !self.sw.tuning.stable.is_zero() {
                    tokio::time::sleep(self.sw.tuning.stable).await;
                    let after = d.app.processes(&self.pc.home).await.unwrap_or_default();
                    if !after.iter().any(|p| new.contains(&p.pid)) {
                        return CheckResult::Failed {
                            detail: text("switch.verify.no_new_app_server").render_fallback(),
                        };
                    }
                }
                return CheckResult::Passed;
            }
            if Instant::now() >= deadline {
                if !seen_any {
                    return skipped(text("switch.verify.not_supported"));
                }
                let key = if last_old_alive && !new.is_empty() {
                    "switch.verify.old_app_server_alive"
                } else {
                    "switch.verify.no_new_app_server"
                };
                return CheckResult::Failed {
                    detail: text(key).render_fallback(),
                };
            }
            tokio::time::sleep(self.sw.tuning.poll.min(Duration::from_millis(250))).await;
        }
    }

    async fn verify_files(&self) -> CheckResult {
        let Some(report) = &self.written.config else {
            return skipped(text("switch.verify.not_applicable"));
        };
        let state = match self.deps().config.read_state(&self.pc.home).await {
            Ok(s) => s,
            Err(e) => {
                return CheckResult::Failed {
                    detail: e.to_string(),
                };
            }
        };
        for f in &report.files {
            let live = match f.kind {
                CodexFileKind::ConfigToml => &state.config_sha256,
                CodexFileKind::AuthJson => &state.auth_sha256,
            };
            if live == &f.after_sha256 {
                continue;
            }
            // The desktop app writes its own keys to config.toml at any time
            // (`[desktop]` settings, bundled `[mcp_servers.*]`), also right after a
            // restart (seen in the e2e runs). Superba owns only its keys there: the
            // file passes when it still parses and none of them changed since the
            // write. auth.json is owned as a whole.
            if f.kind == CodexFileKind::ConfigToml
                && live.is_some()
                && state.parse_error.is_none()
                && state.drift.is_empty()
            {
                self.ev.log(
                    Severity::Info,
                    text("switch.verify.config_changed_elsewhere"),
                );
                continue;
            }
            let file = match f.kind {
                CodexFileKind::ConfigToml => "config.toml",
                CodexFileKind::AuthJson => "auth.json",
            };
            let detail = if f.kind == CodexFileKind::ConfigToml && !state.drift.is_empty() {
                text("switch.verify.owned_keys_changed").arg(
                    "keys",
                    state
                        .drift
                        .iter()
                        .map(|k| k.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            } else {
                text("switch.verify.hash_mismatch").arg("file", file)
            };
            return CheckResult::Failed {
                detail: detail.render_fallback(),
            };
        }
        CheckResult::Passed
    }

    async fn verify_auth(&self, session: Option<&AppSession>) -> CheckResult {
        let d = self.deps();
        if !self.pc.auth.is_some_and(AuthMode::needs_authapi_env) {
            return skipped(text("switch.verify.not_applicable"));
        }
        // Without a configured site that owns the destination (for example a
        // restored legacy tutorial config) the key must not be sent anywhere to
        // check it; skip with the reason instead of failing, otherwise such a
        // restore (and uninstall) could never pass verification.
        let (Some(site), Some(key)) = (&self.pc.site, &self.pc.key) else {
            return skipped(text("switch.verify.authapi_credentials_missing"));
        };
        // The new app-server authenticates against the auth address of the app
        // process (hot path) or the one Superba launched it with.
        let origin = session
            .and_then(|s| s.authapi_env.value())
            .and_then(|v| superba_core::Origin::parse(v).ok())
            .or_else(|| {
                self.pc
                    .endpoints
                    .as_ref()
                    .and_then(|e| e.authapi_origin.clone())
            });
        let Some(origin) = origin else {
            return CheckResult::Failed {
                detail: text("switch.verify.authapi_unhealthy")
                    .arg("reason", "no auth address")
                    .render_fallback(),
            };
        };
        match d.prober.check_authapi(site, &origin, key).await {
            Ok(AuthApiHealth::Healthy) => CheckResult::Passed,
            Ok(AuthApiHealth::Unhealthy { reason }) => CheckResult::Failed {
                detail: text("switch.verify.authapi_unhealthy")
                    .arg("reason", reason)
                    .render_fallback(),
            },
            Err(e) if e.is_not_implemented() => skipped(text("switch.verify.not_supported")),
            Err(e) => CheckResult::Failed {
                detail: text("switch.verify.authapi_unhealthy")
                    .arg("reason", e.to_string())
                    .render_fallback(),
            },
        }
    }

    async fn verify_line(&self) -> CheckResult {
        let d = self.deps();
        let (url, key) = match (&self.pc.endpoints, &self.pc.key) {
            (Some(e), Some(k)) => (e.openai_base_url.clone(), k.clone()),
            _ => match d.config.active_credentials(&self.pc.home).await {
                Ok(Some(c)) => match c.openai_base_url {
                    Some(u) => (u, c.key),
                    None => return skipped(text("switch.verify.not_applicable")),
                },
                Ok(None) => return skipped(text("switch.verify.not_applicable")),
                Err(e) if e.is_not_implemented() => {
                    return skipped(text("switch.verify.not_supported"));
                }
                Err(e) => {
                    return CheckResult::Failed {
                        detail: e.to_string(),
                    };
                }
            },
        };
        match d.prober.list_models(&url, &key).await {
            Ok(_) => CheckResult::Passed,
            Err(e) if e.is_not_implemented() => skipped(text("switch.verify.not_supported")),
            Err(e) => CheckResult::Failed {
                detail: text("switch.verify.line_failed")
                    .arg("detail", e.to_string())
                    .render_fallback(),
            },
        }
    }

    async fn verify_renderer(&self, session: Option<&AppSession>) -> CheckResult {
        let Some(session) = session.filter(|s| s.cdp.is_some()) else {
            return skipped(text("switch.verify.no_session"));
        };
        match self.deps().app.renderer_state(session).await {
            Ok(state) if state.on_login_page && self.pc.auth.is_some_and(AuthMode::uses_key) => {
                CheckResult::Failed {
                    detail: text("switch.verify.login_page").render_fallback(),
                }
            }
            Ok(_) => CheckResult::Passed,
            Err(e) if e.is_not_implemented() => skipped(text("switch.verify.not_supported")),
            // A flaky CDP read is not evidence that the switch failed.
            Err(e) => CheckResult::Skipped {
                reason: e.to_string(),
            },
        }
    }

    // -----------------------------------------------------------------------
    // Remote hosts (plan 8.4 - 8.6), in parallel; one failing host never fails the
    // others or the local switch.
    // -----------------------------------------------------------------------

    fn remote_hosts_to_apply(&self) -> Vec<(RemoteHost, Option<&RemotePlan>)> {
        let mut out = Vec::new();
        for p in &self.plan.remotes {
            if !p.blocked.is_empty() {
                continue;
            }
            if let Some(h) = self.pc.hosts.iter().find(|h| h.target_id == p.target_id) {
                out.push((h.clone(), Some(p)));
            }
        }
        if let Some(t) = &self.pc.rollback_remote
            && let Some(h) = self.pc.hosts.iter().find(|h| &h.target_id == t)
        {
            out.push((h.clone(), None));
        }
        out
    }

    async fn remotes(&mut self) -> Vec<RemoteOutcome> {
        let mut outcomes: Vec<RemoteOutcome> = self
            .plan
            .restarts
            .iter()
            .filter_map(|r| match (&r.consumer, &r.restart) {
                (
                    superba_core::ConsumerKind::Remote { target_id },
                    RestartKind::Blocked { reasons },
                ) => Some(RemoteOutcome {
                    target_id: target_id.clone(),
                    status: OutcomeStatus::Failed,
                    error: Some(Error::Blocked {
                        reasons: reasons.clone(),
                    }),
                }),
                _ => None,
            })
            .collect();
        let work = self.remote_hosts_to_apply();
        if work.is_empty() {
            return outcomes;
        }
        let d = self.deps().clone();
        let session = self.session.clone();
        let expected_authapi = self
            .pc
            .endpoints
            .as_ref()
            .and_then(|e| e.authapi_origin.as_ref())
            .map(|o| o.as_str());
        let timeout_ms = self.sw.tuning.verify_timeout.as_millis() as u64;
        let journal = SwitchJournal::new(&d.ctx.data_dir);
        let rollback_target = self.pc.rollback_remote.clone();
        let ev = &self.ev;
        let futs = work.into_iter().map(|(host, plan)| {
            let d = d.clone();
            let session = session.clone();
            // A host's own plan says which env it gets (a WSL distribution may be
            // routed or direct while this computer is the other).
            let expected_authapi = plan
                .and_then(|p| p.desired.as_ref())
                .map(|desired| match &desired.mode {
                    superba_core::ResolvedMode::Token {
                        auth, endpoints, ..
                    } if auth.needs_authapi_env() => {
                        endpoints.authapi_origin.as_ref().map(|o| o.as_str())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| expected_authapi.clone());
            let journal = journal.clone();
            let rollback_target = rollback_target.clone();
            async move {
                let step = SwitchStep::Remote {
                    target_id: host.target_id.clone(),
                };
                ev.step(step.clone(), StepStatus::Running);
                let result = match plan {
                    Some(plan) => {
                        remote_apply(
                            &d,
                            &host,
                            plan,
                            session.as_ref(),
                            expected_authapi,
                            timeout_ms,
                        )
                        .await
                    }
                    None if rollback_target.as_ref() == Some(&host.target_id) => {
                        remote_rollback(&d, &journal, &host, session.as_ref()).await
                    }
                    None => Err(remote_failure(Error::invalid("nothing to do"), None, true)),
                };
                match &result {
                    Ok(_) => ev.step(step, StepStatus::Done),
                    Err(f) => ev.step(
                        step,
                        StepStatus::Failed {
                            message: f.error.to_string(),
                        },
                    ),
                }
                (host, result)
            }
        });
        let results = join_all(futs).await;
        for (host, result) in results {
            match result {
                Ok((status, report)) => {
                    if let Some(report) = report {
                        self.entry.remotes.push(RemoteReceipt {
                            target_id: host.target_id.clone(),
                            report,
                            rolled_back: false,
                        });
                    }
                    if host.kind == superba_core::HostKind::Wsl {
                        if status == OutcomeStatus::WrittenOnly {
                            self.notices.push(Notice::info(
                                text("switch.wsl.written_only").arg_text("host", host.title()),
                            ));
                        }
                    } else if host.host_id.is_none() || session.is_none() {
                        self.notices.push(Notice::info(
                            text("switch.warn.remote_no_host_id").arg("host", &host.label),
                        ));
                    }
                    outcomes.push(RemoteOutcome {
                        target_id: host.target_id.clone(),
                        status,
                        error: None,
                    });
                }
                Err(f) => {
                    let RemoteFailure {
                        error,
                        report,
                        rolled_back,
                    } = *f;
                    if let Some(report) = report {
                        self.entry.remotes.push(RemoteReceipt {
                            target_id: host.target_id.clone(),
                            report,
                            rolled_back,
                        });
                    }
                    outcomes.push(RemoteOutcome {
                        target_id: host.target_id.clone(),
                        status: OutcomeStatus::Failed,
                        error: Some(error),
                    });
                }
            }
        }
        // Mark rolled-back history entries for "roll back this host".
        if let Some(t) = &rollback_target
            && outcomes
                .iter()
                .any(|o| &o.target_id == t && o.status != OutcomeStatus::Failed)
            && let Ok(Some((mut old, _))) = journal.last_remote(t)
        {
            for r in old.remotes.iter_mut().filter(|r| &r.target_id == t) {
                r.rolled_back = true;
            }
            let _ = journal.write(&old);
        }
        self.save_journal();
        outcomes
    }

    // -----------------------------------------------------------------------
    // Finishing
    // -----------------------------------------------------------------------

    /// Compare the relaunched app-server's process image with a pin plan's
    /// executable (a zsh rc export or a bad path wins silently otherwise).
    async fn check_pin(&mut self, session: &AppSession) {
        let Some(pin) = self.pc.pin.clone() else {
            return;
        };
        let installs = self.deps().app.detect_installs().await.unwrap_or_default();
        let install = self
            .pc
            .install_id
            .as_ref()
            .and_then(|id| installs.iter().find(|i| &i.id == id))
            .or_else(|| installs.first());
        let expected = pin
            .exe
            .clone()
            .or_else(|| install.and_then(|i| i.bundled_codex.clone()));
        let actual = self.deps().app.app_server_exe(session).await;
        match (expected, actual) {
            (Some(x), Ok(Some(a))) => {
                // Compared as one Codex (a launcher and its target, links), never
                // by the literal paths.
                let set = self.deps().profiles.load().await.unwrap_or_default();
                let stored = (self.deps().versions)(&set.settings.network)
                    .stored()
                    .await
                    .unwrap_or_default();
                let mismatch = crate::versions::pin_mismatch(&crate::versions::PinCheck {
                    actual: &a,
                    expected: &x,
                    pinned: pin.exe.is_some(),
                    install,
                    stored: &stored,
                    launched_by_us: true,
                    persist_env: set.settings.app_codex.persist_env,
                    home: &self.deps().ctx.home_dir,
                });
                self.pin_verified = Some(mismatch.is_none());
                if let Some(m) = mismatch {
                    self.notices.push(Notice::danger(m));
                }
            }
            _ => self
                .notices
                .push(Notice::info(text("switch.pin.not_verified"))),
        }
    }

    /// Store the line / auth overrides into the profile and remember the selection.
    async fn after_success(&mut self, remotes: &[superba_core::RemoteOutcome]) {
        let d = self.deps().clone();
        if let Some((target, record)) = self.pc.host_sync.clone()
            && remotes
                .iter()
                .any(|r| r.target_id == target && r.status != OutcomeStatus::Failed)
        {
            match d.profiles.load().await {
                Ok(mut set) => {
                    set.settings.hosts.entry(target).or_default().last_sync = Some(record);
                    if let Err(e) = d.profiles.save(&set).await {
                        tracing::warn!(error = %e, "cannot store the settings sync");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot store the settings sync"),
            }
        }
        if let Some(pin) = self.pc.pin.clone()
            && self.pin_verified != Some(false)
        {
            match d.profiles.load().await {
                Ok(mut set) => {
                    set.settings.app_codex.pin = pin.pin;
                    if let Err(e) = d.profiles.save(&set).await {
                        tracing::warn!(error = %e, "cannot store the app's Codex pin");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot store the app's Codex pin"),
            }
        }
        if let Some(record) = self.pc.catalog_record.clone()
            && self.written.config.is_some()
            && let Err(e) = crate::models::commit_record(&d.ctx.data_dir, &self.pc.home, record)
        {
            tracing::warn!(error = %e, "cannot record the model catalog file");
        }
        if let Some(enabled) = self.pc.router_enabled_after {
            match d.profiles.load().await {
                Ok(mut set) => {
                    set.settings.router.enabled = enabled;
                    if let Err(e) = d.profiles.save(&set).await {
                        tracing::warn!(error = %e, "cannot store router mode");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot store router mode"),
            }
            self.sw.reset_router_watchdog();
        }
        if let Some(update) = self.pc.profile_update.clone()
            && let Ok(mut set) = d.profiles.load().await
        {
            if let Some(p) = set.profiles.iter_mut().find(|p| p.id == update.profile_id) {
                if let Some(l) = update.line_id {
                    // The chosen line becomes the primary; the others stay backups.
                    let mut lines = vec![l.clone()];
                    lines.extend(p.line_priority().into_iter().filter(|x| x != &l));
                    p.set_line_priority(lines);
                }
                match update.auth_mode {
                    Some(AuthMode::Pat) => p.set_auth(superba_core::AuthChoice::Pat),
                    Some(AuthMode::ApiKey) => p.set_auth(superba_core::AuthChoice::ApiKey),
                    _ => {}
                }
            }
            set.last_selected = Some(update.profile_id);
            if let Err(e) = d.profiles.save(&set).await {
                tracing::warn!(error = %e, "cannot store the profile overrides");
            }
        }
        if let Some(delete_backups) = self.pc.uninstall {
            // Plan 9.6 uninstall: launcher entries go too; backups only on request.
            if let Err(e) = d.app.set_launcher_entry(false).await {
                tracing::warn!(error = %e, "cannot remove the launcher entry");
            }
            // The local router's service and keys go too: the restore just pointed
            // Codex away from it.
            let (router, _) = d.router.overview().await;
            if router.configured || router.installed {
                if let Err(e) = d.router.uninstall().await {
                    tracing::warn!(error = %e, "cannot remove the local router");
                }
                match d.profiles.load().await {
                    Ok(mut set) if set.settings.router.enabled => {
                        set.settings.router.enabled = false;
                        if let Err(e) = d.profiles.save(&set).await {
                            tracing::warn!(error = %e, "cannot store router mode");
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "cannot store router mode"),
                }
            }
            // Catalog files go once config.toml no longer names them.
            crate::models::remove_all(&d.ctx.data_dir, &self.pc.home);
            // The app's Codex pin goes too, with a CODEX_CLI_PATH Superba persisted
            // for apps opened elsewhere (it would keep the app on a Codex in
            // Superba's data directory).
            match d
                .env
                .plan(
                    superba_core::codex::ENV_CODEX_CLI_PATH,
                    superba_core::EnvOp::Clear,
                )
                .await
            {
                Ok(p) if !p.is_noop() => {
                    if let Err(e) = d.env.apply(&p).await {
                        tracing::warn!(error = %e, "cannot remove CODEX_CLI_PATH");
                    }
                }
                Ok(_) => {}
                Err(e) if e.is_not_implemented() => {}
                Err(e) => tracing::warn!(error = %e, "cannot remove CODEX_CLI_PATH"),
            }
            match d.profiles.load().await {
                Ok(mut set) if set.settings.app_codex != Default::default() => {
                    set.settings.app_codex = Default::default();
                    if let Err(e) = d.profiles.save(&set).await {
                        tracing::warn!(error = %e, "cannot reset the app's Codex pin");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "cannot reset the app's Codex pin"),
            }
            if delete_backups {
                let dir = d.ctx.data_dir.backups_dir();
                if let Err(e) = std::fs::remove_dir_all(&dir)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(error = %e, "cannot delete backups");
                }
            }
        }
    }

    /// CLI processes still on the previous configuration (plan 7.7 banner).
    async fn stale_processes(&self) -> Vec<ProcessInfo> {
        let Some(report) = &self.written.config else {
            return Vec::new();
        };
        let mut processes = self
            .deps()
            .app
            .processes(&self.pc.home)
            .await
            .unwrap_or_default();
        rules::mark_stale(&mut processes, Some(report.applied_at));
        processes
            .into_iter()
            .filter(|p| p.stale && !matches!(p.kind, CodexProcessKind::AppServer))
            .collect()
    }

    fn summary_notices(
        &mut self,
        status: OutcomeStatus,
        elapsed: Duration,
        remotes: &[RemoteOutcome],
        stale: &[ProcessInfo],
        returned_to: Option<&ThreadRef>,
    ) {
        let secs = messages::seconds_1dp(elapsed.as_millis() as u64);
        let restore = matches!(self.plan.request.intent, SwitchIntent::Restore { .. });
        match status {
            OutcomeStatus::Switched | OutcomeStatus::Partial => {
                let key = if restore {
                    "switch.done.restored"
                } else {
                    "switch.done.switched"
                };
                self.notices.push(Notice::info(
                    text(key)
                        .arg_text("destination", self.plan.to.label.clone())
                        .arg("seconds", &secs),
                ));
            }
            OutcomeStatus::WrittenOnly => {
                // After a failed relaunch the app is not running: "restart it"
                // would be wrong.
                let launch_failed = self
                    .notices
                    .iter()
                    .any(|n| n.text.key == "switch.failed.launch");
                self.notices.push(if launch_failed {
                    Notice::warning(text("switch.done.launch_failed"))
                } else {
                    Notice::info(text("switch.done.written_only"))
                });
            }
            _ => {}
        }
        if let Some(t) = returned_to {
            self.notices.push(Notice::info(
                text("switch.done.returned").arg("title", t.title.clone().unwrap_or_default()),
            ));
        }
        for r in remotes {
            match (&r.status, &r.error) {
                (OutcomeStatus::Failed, Some(e)) => self.notices.push(Notice::warning(
                    text("switch.done.partial")
                        .arg("host", messages::target_label(r.target_id.as_str()))
                        .arg_text("error", e.to_text()),
                )),
                (OutcomeStatus::Switched, _) => self.notices.push(Notice::info(
                    text("switch.done.remote_switched")
                        .arg("host", messages::target_label(r.target_id.as_str())),
                )),
                _ => {}
            }
        }
        if !stale.is_empty() {
            let times: Vec<String> = stale
                .iter()
                .filter_map(|p| p.started_at)
                .map(|t| t.to_rfc3339())
                .collect();
            self.notices.push(Notice::info(
                text("switch.banner.stale")
                    .arg("count", stale.len())
                    .arg("times", times.join(", ")),
            ));
        }
    }
}

fn skipped(reason: LocalizedText) -> CheckResult {
    CheckResult::Skipped {
        reason: reason.render_fallback(),
    }
}

/// Why a host failed, and whether what was written there is undone.
struct RemoteFailure {
    error: Error,
    report: Option<RemoteApplyReport>,
    rolled_back: bool,
}

fn remote_failure(
    error: Error,
    report: Option<RemoteApplyReport>,
    rolled_back: bool,
) -> Box<RemoteFailure> {
    Box::new(RemoteFailure {
        error,
        report,
        rolled_back,
    })
}

type RemoteResult =
    std::result::Result<(OutcomeStatus, Option<RemoteApplyReport>), Box<RemoteFailure>>;

/// Apply one host, restart its app-server through the app, verify; roll the host
/// back when the restart or verification fails.
async fn remote_apply(
    d: &crate::SwitcherDeps,
    host: &RemoteHost,
    plan: &RemotePlan,
    session: Option<&AppSession>,
    expected_authapi: Option<String>,
    timeout_ms: u64,
) -> RemoteResult {
    let report = d
        .remote
        .apply(host, plan)
        .await
        .map_err(|e| remote_failure(e, None, true))?;
    let (Some(host_id), Some(session)) = (host.host_id.as_deref(), session) else {
        // Written; applies when the host is reconnected in the app.
        return Ok((OutcomeStatus::WrittenOnly, Some(report)));
    };
    if plan.state.app_server_pids.is_empty() {
        // No Codex runs there (the app is not connected to the host now): the
        // host's Codex reads the files when the app next starts it; a restart
        // would only wait for an app-server that never comes.
        return Ok((OutcomeStatus::WrittenOnly, Some(report)));
    }
    if let Err(e) = d
        .app
        .hot_restart(session, &HotRestartRequest::remote(host_id))
        .await
    {
        return remote_undo(d, host, host_id, session, report, e).await;
    }
    let request = RemoteVerifyRequest {
        report: report.clone(),
        previous_app_server_pids: plan.state.app_server_pids.clone(),
        expected_authapi,
        timeout_ms,
    };
    match d.remote.verify(host, &request).await {
        Ok(v) if v.new_app_server && v.files_match => Ok((OutcomeStatus::Switched, Some(report))),
        Ok(v) => {
            let detail = v.detail.unwrap_or_else(|| {
                if v.new_app_server {
                    "files changed after writing".into()
                } else {
                    "no new app-server".into()
                }
            });
            let e = Error::other(
                text("switch.verify.remote_failed")
                    .arg("host", &host.label)
                    .arg("detail", detail)
                    .render_fallback(),
            );
            remote_undo(d, host, host_id, session, report, e).await
        }
        Err(e) if e.is_not_implemented() => Ok((OutcomeStatus::Switched, Some(report))),
        Err(e) => remote_undo(d, host, host_id, session, report, e).await,
    }
}

/// Roll a failed host back and restart its app-server on the old files.
async fn remote_undo(
    d: &crate::SwitcherDeps,
    host: &RemoteHost,
    host_id: &str,
    session: &AppSession,
    report: RemoteApplyReport,
    error: Error,
) -> RemoteResult {
    if let Err(e) = d.remote.rollback(host, &report).await {
        tracing::warn!(host = %host.label, error = %e, "remote rollback failed");
        return Err(remote_failure(error, Some(report), false));
    }
    let _ = d
        .app
        .hot_restart(session, &HotRestartRequest::remote(host_id))
        .await;
    Err(remote_failure(error, Some(report), true))
}

/// Roll a host back to its state before its last recorded switch.
async fn remote_rollback(
    d: &crate::SwitcherDeps,
    journal: &SwitchJournal,
    host: &RemoteHost,
    session: Option<&AppSession>,
) -> RemoteResult {
    let Some((_, receipt)) = journal
        .last_remote(&host.target_id)
        .map_err(|e| remote_failure(e, None, true))?
    else {
        return Err(remote_failure(
            Error::NotFound {
                what: format!("recorded switch for {}", host.target_id),
            },
            None,
            true,
        ));
    };
    d.remote
        .rollback(host, &receipt.report)
        .await
        .map_err(|e| remote_failure(e, None, true))?;
    if let (Some(host_id), Some(session)) = (host.host_id.as_deref(), session) {
        let _ = d
            .app
            .hot_restart(session, &HotRestartRequest::remote(host_id))
            .await;
    }
    Ok((OutcomeStatus::Switched, None))
}
