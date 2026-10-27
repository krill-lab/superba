//! Common problems, a separate diagnostic workbench and user-verified results.

mod artwork;
mod guidance;
mod view;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::Ui;
use superba_core::*;

use super::{Action, Cx, PageStatus};
use crate::runtime::ReadTask;

/// A recovery operation, sent to the application's global command/Flow owner.
#[derive(Clone, Debug)]
pub enum Repair {
    /// Remove manually reviewed obsolete config entries with an editor backup.
    Cleanup(TroubleshootingCleanup),
    /// Reapply the current managed configuration.
    Reapply,
    /// Restart Codex once.
    Restart,
    /// Back up and rebuild the whole config.toml.
    Reinitialize,
    /// Migrate all non-openai conversations.
    Migrate,
    /// Restore the official ChatGPT login.
    Official,
    /// Reveal and synchronize a user-selected upstream model.
    Model {
        /// The provider that returned it.
        profile: ProfileId,
        /// Exact upstream model id.
        model: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Screen {
    #[default]
    Select,
    Workbench,
    Result,
    Next,
    Complete,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Answer {
    #[default]
    Unanswered,
    Yes,
    No,
    Unsure,
}

#[derive(Clone, Debug)]
struct Run {
    id: u64,
    issue: TroubleshootingIssue,
    report: Option<TroubleshootingReport>,
    checks: Vec<TroubleshootingCheck>,
    repairs: Vec<LocalizedText>,
    error: Option<LocalizedText>,
    resolved: Option<bool>,
    model: Option<String>,
    model_provider: Option<ProfileId>,
    model_missing: bool,
    speed: SpeedTier,
    new_model: Answer,
    added_today: Answer,
    restarted: bool,
    reapplied: bool,
    migrated: bool,
    rebuilt: bool,
    model_synced: bool,
    sync_model_requested: bool,
    isolated_server: bool,
    server_unauthorized: Answer,
}

impl Run {
    fn request(&self, refresh_models: bool) -> TroubleshootingRequest {
        TroubleshootingRequest {
            issue: if self.issue == TroubleshootingIssue::Server
                || (self.issue == TroubleshootingIssue::Other && self.rebuilt)
                || (self.issue == TroubleshootingIssue::Models && self.model_missing)
            {
                TroubleshootingIssue::Request
            } else {
                self.issue
            },
            model: self.model.clone(),
            speed: Some(self.speed),
            refresh_models: refresh_models
                || (self.issue == TroubleshootingIssue::Models && self.model.is_none()),
            retry: None,
        }
    }
}

#[derive(Debug)]
struct Inspection {
    task: ReadTask<TroubleshootingReport>,
    cancelled: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    automatic: bool,
    retry_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Progress {
    backend: bool,
    checks: Vec<TroubleshootingCheck>,
}

/// Page state persists across navigation, but diagnostic history is not stored
/// on disk. Global writes are correlated by the run id.
#[derive(Debug, Default)]
pub struct State {
    screen: Screen,
    run: Option<Run>,
    inspection: Option<Inspection>,
    history: Vec<Run>,
    next_id: u64,
    pending: Option<(u64, Repair)>,
    confirming_reinitialize: bool,
    confirming_cleanup: bool,
    verification: bool,
    filter: usize,
    model_query: String,
    error: Option<LocalizedText>,
    drawer_stage: Option<(u64, usize)>,
    result_revision: u64,
    completed_at: Option<f64>,
    verification_automatic: bool,
    simple_mode: bool,
    reduce_motion: bool,
    art: crate::movers::render::SceneCache,
    celebration: crate::movers::render::SceneCache,
    advice_art: crate::movers::render::SceneCache,
}

impl State {
    /// Match the shell's navigation and animation preferences.
    pub fn display_options(&mut self, simple_mode: bool, reduce_motion: bool) {
        self.simple_mode = simple_mode;
        self.reduce_motion = reduce_motion;
    }
    /// True while checks or a recovery operation are pending.
    pub fn busy(&self) -> bool {
        self.inspection.is_some() || self.pending.is_some()
    }

    /// Identity of the recovery currently owned by this page.
    pub fn pending_run(&self) -> Option<u64> {
        self.pending.as_ref().map(|(id, _)| *id)
    }

    fn start(&mut self, issue: TroubleshootingIssue, cx: &Cx<'_>, out: &mut Vec<Action>) {
        if self.busy() || cx.busy {
            return;
        }
        self.next_id += 1;
        self.run = Some(Run {
            id: self.next_id,
            issue,
            report: None,
            checks: Vec::new(),
            repairs: Vec::new(),
            error: None,
            resolved: None,
            model: None,
            model_provider: None,
            model_missing: false,
            speed: SpeedTier::Fast,
            new_model: Answer::Unanswered,
            added_today: Answer::Unanswered,
            restarted: false,
            reapplied: false,
            migrated: false,
            rebuilt: false,
            model_synced: false,
            sync_model_requested: false,
            isolated_server: false,
            server_unauthorized: Answer::Unanswered,
        });
        self.error = None;
        self.model_query.clear();
        if matches!(
            issue,
            TroubleshootingIssue::Policy
                | TroubleshootingIssue::Server
                | TroubleshootingIssue::Other
        ) {
            self.screen = Screen::Next;
            self.remember();
        } else {
            self.inspect(cx, true, issue == TroubleshootingIssue::Models, out);
        }
    }

    fn inspect(
        &mut self,
        cx: &Cx<'_>,
        automatic: bool,
        refresh_models: bool,
        _out: &mut Vec<Action>,
    ) {
        let Some(run) = &self.run else {
            return;
        };
        self.begin_inspection(cx.service.clone(), run.request(refresh_models), automatic);
    }

    fn retry(&mut self, id: &str, cx: &Cx<'_>) {
        if self.busy() || cx.busy {
            return;
        }
        let Some(run) = &self.run else {
            return;
        };
        let Some(retry) = run
            .report
            .as_ref()
            .and_then(|r| r.checks.iter().find(|c| c.id == id))
            .and_then(|c| c.retry.clone())
        else {
            return;
        };
        let mut request = run.request(true);
        request.retry = Some(retry);
        self.begin_inspection(cx.service.clone(), request, false);
        if let Some(inspection) = &mut self.inspection {
            inspection.retry_id = Some(id.to_owned());
        }
        self.screen = Screen::Result;
    }

    fn begin_inspection(
        &mut self,
        service: Arc<dyn SwitchService>,
        request: TroubleshootingRequest,
        automatic: bool,
    ) {
        if self.inspection.is_some() || self.pending.is_some() {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(Progress::default()));
        let stop = cancelled.clone();
        let steps = progress.clone();
        let sink: TroubleshootingProgressSink = Arc::new(move |p| {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            if let Ok(mut steps) = steps.lock() {
                match p {
                    TroubleshootingProgress::Configuration => steps.backend = false,
                    TroubleshootingProgress::Backend => steps.backend = true,
                    TroubleshootingProgress::Check(check) => steps.checks.push(check),
                }
            }
            true
        });
        let task =
            crate::runtime::task(async move { service.troubleshooting_check(request, sink).await });
        self.inspection = Some(Inspection {
            task,
            cancelled,
            progress,
            automatic,
            retry_id: None,
        });
        self.drawer_stage = None;
        self.screen = Screen::Workbench;
        self.error = None;
    }

    fn queue(&mut self, repair: Repair, out: &mut Vec<Action>) {
        if self.busy() {
            return;
        }
        let Some(run) = self.run.as_mut() else {
            return;
        };
        match repair {
            Repair::Cleanup(_) => {}
            Repair::Reapply => run.reapplied = true,
            Repair::Restart => run.restarted = true,
            Repair::Migrate => run.migrated = true,
            Repair::Reinitialize => run.rebuilt = true,
            Repair::Model { .. } => run.model_synced = true,
            Repair::Official => {}
        }
        let id = run.id;
        run.resolved = None;
        self.pending = Some((id, repair.clone()));
        self.screen = Screen::Workbench;
        out.push(Action::TroubleshootingRepair { run: id, repair });
    }

    fn finish_inspection(
        &mut self,
        report: TroubleshootingReport,
        automatic: bool,
        out: &mut Vec<Action>,
    ) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        // Preserve the user's problem identity (500 uses the request checks).
        let reapply = report.reapply && !run.reapplied && run.issue != TroubleshootingIssue::All;
        let restart = report.restart_recommended && !run.restarted;
        let synchronize = (run.sync_model_requested && report.config_ok)
            .then(|| run.model_provider.clone().zip(run.model.clone()))
            .flatten();
        if synchronize.is_some() {
            run.sync_model_requested = false;
        }
        run.checks.extend(report.checks.clone());
        run.report = Some(report);
        self.result_revision += 1;
        self.screen = Screen::Result;
        self.remember();
        if automatic && reapply {
            self.queue(Repair::Reapply, out);
        } else if let Some((profile, model)) = synchronize {
            self.queue(Repair::Model { profile, model }, out);
        } else if automatic && restart {
            self.queue(Repair::Restart, out);
        }
    }

    fn remember(&mut self) {
        let Some(run) = &mut self.run else {
            return;
        };
        run.error = self.error.clone();
        self.history.retain(|r| r.id != run.id);
        self.history.insert(0, run.clone());
        self.history.truncate(5);
    }

    fn finish_retry(&mut self, id: &str, report: TroubleshootingReport) {
        let Some(run) = &mut self.run else {
            return;
        };
        let Some(current) = &mut run.report else {
            return;
        };
        // Replace only the retried observation; keep other results and recovery state.
        if !report.checks.is_empty() {
            let position = current
                .checks
                .iter()
                .position(|c| c.id == id)
                .unwrap_or(current.checks.len());
            current
                .checks
                .retain(|c| c.id != id && !report.checks.iter().any(|new| new.id == c.id));
            let position = position.min(current.checks.len());
            current
                .checks
                .splice(position..position, report.checks.clone());
            run.checks.extend(report.checks);
            current.checked_at = report.checked_at;
        }
        for models in report.models {
            if let Some(old) = current
                .models
                .iter_mut()
                .find(|m| m.profile_id == models.profile_id)
            {
                *old = models;
            } else {
                current.models.push(models);
            }
        }
        run.resolved = None;
        self.result_revision += 1;
        self.screen = Screen::Result;
        self.remember();
    }

    /// Called once each frame, including when another page is visible.
    pub fn poll(&mut self, ui: &Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
        if self.verification && self.pending.is_none() && self.inspection.is_none() {
            self.verification = false;
            self.inspect(cx, self.verification_automatic, false, out);
        }
        let result = self.inspection.as_ref().and_then(|r| r.task.poll());
        if let Some(result) = result {
            let inspection = self.inspection.take().expect("inspection was polled");
            if inspection.cancelled.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok(report) => {
                    if let Some(id) = inspection.retry_id {
                        self.finish_retry(&id, report);
                    } else {
                        self.finish_inspection(report, inspection.automatic, out);
                    }
                }
                Err(e) => {
                    self.error = Some(e.to_text());
                    self.screen = Screen::Result;
                    self.remember();
                }
            }
        }
        if self.busy() {
            crate::pages::shared::loading::repaint_soon(ui);
        }
    }

    /// Return a global write/restart result to its originating diagnostic run.
    pub fn repair_finished(&mut self, id: u64, result: Result<LocalizedText>) {
        if self.pending_run() != Some(id) {
            return;
        }
        let (_, repair) = self.pending.take().expect("pending id matched");
        let Some(run) = self.run.as_mut().filter(|r| r.id == id) else {
            return;
        };
        match result {
            Ok(message) => {
                let (key, fallback) = match &repair {
                    Repair::Reapply => ("reapplied", "Reapplied the provider configuration."),
                    Repair::Cleanup(_) => (
                        "cleaned",
                        "Removed the reviewed configuration entries after creating a backup.",
                    ),
                    Repair::Restart => ("restarted", "Restarted Codex."),
                    Repair::Reinitialize => ("rebuilt", "Backed up and rebuilt config.toml."),
                    Repair::Migrate => ("migrated", "Migrated conversations to openai."),
                    Repair::Official => (
                        "restored_official",
                        "Switched to the official ChatGPT provider.",
                    ),
                    Repair::Model { .. } => (
                        "model_synced",
                        "Synchronized the selected model with Codex.",
                    ),
                };
                run.repairs.push(if matches!(repair, Repair::Cleanup(_)) {
                    message
                } else {
                    LocalizedText::new(format!("troubleshooting.{key}"), fallback)
                });
                if matches!(repair, Repair::Official) {
                    self.screen = Screen::Result;
                } else {
                    self.verification_automatic = !matches!(repair, Repair::Cleanup(_));
                    self.verification = true;
                }
            }
            Err(e) => {
                if matches!(repair, Repair::Model { .. }) {
                    run.model_synced = false;
                }
                if matches!(repair, Repair::Migrate) {
                    run.migrated = false;
                }
                self.error = Some(e.to_text());
                self.screen = Screen::Result;
            }
        }
        self.remember();
    }

    /// Count a restart performed as part of another recovery, so a feature
    /// diagnosis never schedules a second automatic restart.
    pub fn repair_restarted(&mut self, id: u64) {
        if let Some(run) = self.run.as_mut().filter(|r| r.id == id) {
            run.restarted = true;
        }
    }

    fn cancel(&mut self) {
        if self.pending.is_some() {
            return;
        }
        if let Some(inspection) = self.inspection.take() {
            inspection.cancelled.store(true, Ordering::Relaxed);
        }
        self.verification = false;
        self.completed_at = None;
        self.confirming_cleanup = false;
        self.screen = Screen::Select;
    }
}

/// One implementation for both navigation modes.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.poll(ui, cx, out);
    view::show(ui, cx, state, out);
}

/// Open a problem from the shell's page-command interface.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    if let Some(id) = command.strip_prefix("check:")
        && let Some(issue) = TroubleshootingIssue::CARDS
            .into_iter()
            .chain([TroubleshootingIssue::All])
            .find(|i| i.id() == id)
    {
        state.start(issue, cx, out);
    }
}

/// Report page-owned modal state to the shell.
pub fn status(state: &State) -> PageStatus {
    PageStatus {
        modal: state.confirming_reinitialize || state.confirming_cleanup,
        ..Default::default()
    }
}
