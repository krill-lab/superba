//! "Codex in effect" (GH#4) and the process lists of "stop them for me"
//! (GH#2).
//!
//! The dialog puts, for every running Codex consumer of this computer, the
//! configuration it runs next to the one Superba wrote last: the desktop app's
//! Codex core, each codex CLI session in a terminal, `codex exec`, the CLI's
//! background server and the app-server of each editor's Codex extension. A
//! consumer still on an older configuration says what it runs, whether a turn
//! is running in it (restarting it now interrupts that turn), and how it picks
//! the new configuration up: Superba restarts the app's Codex core (or reopens
//! ChatGPT), guides the user for terminals and editors (VS Code: "Developer:
//! Restart Extension Host"; Superba never closes an editor), and can stop a
//! terminal session or an extension's app-server after an explicit
//! confirmation, graceful first, then by force only after a second one.
//!
//! Opened from the Overview's 待办, from the toast after a switch and from the
//! command palette; it reads [`superba_core::SwitchService::runtime`] when
//! opened and every few seconds while open.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Layout, ScrollArea, Ui};
use superba_core::{
    BusyState, ConsumerRole, ConsumerRuntime, Error, ProcessInfo, RestartAdvice, RunningGeneration,
    RuntimeEffect, RuntimeView, SiteKind, StopOutcome, StopReport, SwitchService, ThreadActivity,
    UnknownReason,
};

use crate::design::widgets::{Button, Status, StatusDot, Text, dialog};
use crate::pages::{Action, Cx};
use crate::runtime::ReadTask;

/// How often the open dialog reads again.
const RELOAD: Duration = Duration::from_secs(4);

/// A confirmation shown under one row.
#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    /// "Stop this process?" (graceful).
    Stop(ConsumerRuntime),
    /// It did not exit: "End it by force?".
    Force(ConsumerRuntime, StopOutcome),
}

/// Dialog state (owned by the app).
#[derive(Default)]
pub struct InEffect {
    open: bool,
    task: Option<ReadTask<RuntimeView>>,
    /// The last view read.
    pub view: Option<RuntimeView>,
    error: Option<Error>,
    loaded_at: Option<Instant>,
    stop: Option<(ReadTask<Vec<StopReport>>, ConsumerRuntime, bool)>,
    confirm: Option<Confirm>,
    /// Results of stops in this dialog session (pid, outcome or error).
    results: Vec<(u32, Result<StopOutcome, String>)>,
    copied: Option<String>,
}

impl std::fmt::Debug for InEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InEffect")
            .field("open", &self.open)
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

impl InEffect {
    /// Open the dialog (and read the view at once).
    pub fn open(&mut self, service: &Arc<dyn SwitchService>) {
        self.open = true;
        self.confirm = None;
        self.results.clear();
        self.copied = None;
        self.reload(service);
    }

    /// Open with a view already in hand (the switch's report; tests).
    pub fn open_with(&mut self, view: RuntimeView) {
        self.open = true;
        self.view = Some(view);
        self.loaded_at = Some(Instant::now());
    }

    /// The dialog is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn reload(&mut self, service: &Arc<dyn SwitchService>) {
        if self.task.is_some() {
            return;
        }
        let service = service.clone();
        self.task = Some(crate::runtime::task(async move { service.runtime().await }));
    }

    fn poll(&mut self, cx: &Cx<'_>) {
        if let Some(task) = &self.task
            && let Some(result) = task.poll()
        {
            self.task = None;
            self.loaded_at = Some(Instant::now());
            match result {
                Ok(v) => {
                    self.view = Some(v);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some((task, row, force)) = &self.stop
            && let Some(result) = task.poll()
        {
            let (row, force) = (row.clone(), *force);
            self.stop = None;
            match result {
                Ok(reports) => {
                    let outcome = reports
                        .iter()
                        .find(|r| r.process.pid == row.pid)
                        .map(|r| match &r.error {
                            Some(e) => Err(e.clone()),
                            None => Ok(r.outcome),
                        })
                        .unwrap_or(Ok(StopOutcome::NotRunning));
                    if let Ok(o @ (StopOutcome::StillRunning | StopOutcome::ForceOnly)) = outcome
                        && !force
                    {
                        self.confirm = Some(Confirm::Force(row.clone(), o));
                    }
                    self.results.push((row.pid, outcome));
                }
                Err(e) => self.results.push((row.pid, Err(cx.tr.text(&e.to_text())))),
            }
            self.loaded_at = None;
        }
        if self.loaded_at.is_none_or(|t| t.elapsed() >= RELOAD) && self.stop.is_none() {
            self.reload(cx.service);
        }
    }

    fn start_stop(&mut self, cx: &Cx<'_>, row: &ConsumerRuntime, force: bool) {
        let Some(process) = row_process(row) else {
            return;
        };
        let service = cx.service.clone();
        self.confirm = None;
        self.stop = Some((
            crate::runtime::task(async move { service.stop_processes(vec![process], force).await }),
            row.clone(),
            force,
        ));
    }
}

/// The process a row stands for, as the service lists it (pid, kind, start
/// time), so a stop never reaches a reused pid.
fn row_process(row: &ConsumerRuntime) -> Option<ProcessInfo> {
    use superba_core::CodexProcessKind as K;
    let kind = match &row.role {
        ConsumerRole::Terminal => K::CliTui,
        ConsumerRole::Exec => K::CliExec,
        ConsumerRole::Daemon => K::CliDaemon,
        ConsumerRole::IdeExtension { .. } => K::IdeAppServer,
        ConsumerRole::Other => K::Other,
        _ => return None,
    };
    Some(ProcessInfo {
        pid: row.pid,
        ppid: None,
        kind,
        exe: row.exe.clone(),
        cmdline: row.cmdline.clone(),
        started_at: row.started_at,
        authapi_env: Default::default(),
        codex_home: row.codex_home.clone(),
        stale: false,
        ide: match &row.role {
            ConsumerRole::IdeExtension { ide } => *ide,
            _ => None,
        },
        cwd: row.cwd.clone(),
    })
}

/// The hand edit (GitHub issue 9) a consumer on `running` started before:
/// it runs the configuration Superba wrote last, but not the edit saved
/// after it.
pub fn before_edit(
    view: &RuntimeView,
    running: &RunningGeneration,
) -> Option<superba_core::ManualEdit> {
    let e = view.edited?;
    (running.generation == view.written.as_ref().map(|w| w.generation)).then_some(e)
}

/// "Runs: generation 4 · Official ChatGPT login" for a consumer on an older
/// configuration.
pub fn running_text(cx: &Cx<'_>, running: &RunningGeneration) -> String {
    let Some(g) = running.generation else {
        return cx.tr.t("in_effect.runs_original");
    };
    let name = running
        .profile_id
        .as_ref()
        .and_then(|p| cx.data.profile(p))
        .map(|p| p.name.clone())
        .or_else(|| {
            (running.site_kind == Some(SiteKind::Official))
                .then(|| cx.tr.t("overview.official_chip"))
        });
    match name {
        Some(n) => cx
            .tr
            .f("in_effect.runs_named", &[("generation", &g), ("name", &n)]),
        None => cx.tr.f("in_effect.runs", &[("generation", &g)]),
    }
}

/// Status dot and label of a row's effect.
pub fn effect_label(cx: &Cx<'_>, effect: &RuntimeEffect) -> (Status, String) {
    match effect {
        RuntimeEffect::Current => (Status::Ok, cx.tr.t("in_effect.current")),
        RuntimeEffect::Previous { .. } => (Status::Warn, cx.tr.t("in_effect.previous")),
        RuntimeEffect::Unknown { reason } => (
            Status::Idle,
            cx.tr.t(match reason {
                UnknownReason::NothingWritten => "in_effect.unknown_nothing",
                UnknownReason::NoCore => "in_effect.unknown_core",
                _ => "in_effect.unknown_start",
            }),
        ),
        _ => (Status::Idle, cx.tr.t("in_effect.unknown_start")),
    }
}

/// "pid 1234 · started 10:21 (5 min ago) · ~/work/project".
pub fn identity_line(
    cx: &Cx<'_>,
    pid: u32,
    started: Option<superba_core::Timestamp>,
    cwd: Option<&std::path::Path>,
) -> String {
    let mut parts = vec![format!("pid {pid}")];
    if let Some(t) = started {
        parts.push(cx.tr.f(
            "in_effect.started",
            &[
                ("time", &crate::pages::clock_time(t, false)),
                ("ago", &cx.ago(t)),
            ],
        ));
    }
    if let Some(c) = cwd {
        parts.push(superba_core::paths::display_path(c));
    }
    parts.join(" · ")
}

/// The list of a close / force-stop question: each process (what it is, its
/// pid, start and folder, what stopping it does) and the turns running in
/// them.
pub fn process_list(ui: &mut Ui, cx: &Cx<'_>, processes: &[ProcessInfo], busy: &[ThreadActivity]) {
    ScrollArea::vertical()
        .max_height(260.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for p in processes {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(StatusDot::new(Status::Idle).size(7.0));
                    ui.label(Text::strong(
                        cx.tr.text(&superba_core::process_role(p).text()),
                        cx.pal,
                    ));
                });
                ui.label(Text::mono_muted(
                    identity_line(cx, p.pid, p.started_at, p.cwd.as_deref()),
                    cx.pal,
                ));
                ui.add(
                    egui::Label::new(Text::small(
                        cx.tr.text(&superba_core::stop_effect(p)),
                        cx.pal,
                    ))
                    .wrap(),
                );
            }
        });
    if !busy.is_empty() {
        ui.add_space(6.0);
        for t in busy {
            crate::shell::busy_line(ui, cx, t);
        }
        ui.add(egui::Label::new(Text::small(cx.tr.t("in_effect.busy_warning"), cx.pal)).wrap());
    }
}

/// What a row offers: Superba's own restart, or the command to copy.
fn copy_text(row: &ConsumerRuntime) -> Option<&'static str> {
    match &row.advice {
        RestartAdvice::Reopen => Some("codex resume --last"),
        RestartAdvice::ReloadIde { ide } => Some(
            ide.unwrap_or(superba_core::IdeKind::Other)
                .restart_command(),
        ),
        RestartAdvice::RestartDaemon => Some("codex app-server-control restart"),
        _ => None,
    }
}

/// Draw the dialog (no-op while closed).
pub fn show(ctx: &egui::Context, cx: &Cx<'_>, state: &mut InEffect, out: &mut Vec<Action>) {
    if !state.open {
        return;
    }
    state.poll(cx);
    ctx.request_repaint_after(Duration::from_millis(250));
    let mut close = false;
    let mut actions: Vec<RowAction> = Vec::new();
    let response = dialog(ctx, egui::Id::unique("in_effect"), 640.0, |ui| {
        ui.horizontal(|ui| {
            ui.label(Text::h2(cx.tr.t("in_effect.title"), cx.pal));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(Button::ghost(cx.tr.t("action.close")).small())
                    .clicked()
                {
                    close = true;
                }
                if ui
                    .add(
                        Button::ghost(cx.tr.t("action.refresh"))
                            .small()
                            .loading(state.task.is_some()),
                    )
                    .clicked()
                {
                    state.loaded_at = None;
                }
            });
        });
        ui.add(egui::Label::new(Text::muted(cx.tr.t("in_effect.body"), cx.pal)).wrap());
        let Some(view) = state.view.clone() else {
            match &state.error {
                Some(e) => ui.label(Text::small(cx.tr.text(&e.to_text()), cx.pal)),
                None => ui.label(Text::small(cx.tr.t("in_effect.loading"), cx.pal)),
            };
            return;
        };
        ui.add_space(4.0);
        ui.label(Text::body(written_line(cx, &view), cx.pal));
        ui.separator();
        if view.consumers.is_empty() {
            ui.label(Text::small(cx.tr.t("in_effect.none"), cx.pal));
            return;
        }
        // The list takes what the window leaves around the header.
        let room = (ui.ctx().content_rect().height() - 240.0).max(240.0);
        ScrollArea::vertical()
            .max_height(room)
            .min_scrolled_height(room)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &view.consumers {
                    row_ui(ui, cx, state, row, &mut actions);
                    ui.add_space(6.0);
                }
            });
    });
    if response.should_close() {
        close = true;
    }
    for a in actions {
        match a {
            RowAction::Out(a) => {
                out.push(a);
                close = true;
            }
            RowAction::Copy(text) => {
                out.push(Action::Copy(text.clone()));
                state.copied = Some(text);
            }
            RowAction::AskStop(row) => state.confirm = Some(Confirm::Stop(row)),
            RowAction::Stop(row, force) => state.start_stop(cx, &row, force),
            RowAction::Dismiss => state.confirm = None,
        }
    }
    if close {
        state.open = false;
        state.confirm = None;
    }
}

enum RowAction {
    Out(Action),
    Copy(String),
    AskStop(ConsumerRuntime),
    Stop(ConsumerRuntime, bool),
    Dismiss,
}

/// "Superba wrote: Krill · Overseas · PAT at 10:21 (generation 5)".
pub fn written_line(cx: &Cx<'_>, view: &RuntimeView) -> String {
    let Some(w) = &view.written else {
        return cx.tr.t("in_effect.written_none");
    };
    let name = w
        .profile_id
        .as_ref()
        .and_then(|p| cx.data.profile(p))
        .map(|p| p.name.clone())
        .or_else(|| w.profile_id.as_ref().map(|p| p.as_str().to_owned()))
        .unwrap_or_else(|| cx.tr.t("in_effect.written_unnamed"));
    let line = w.line_id.as_ref().map(|l| cx.line_name(l));
    let auth = w.auth_mode.map(|m| cx.auth_short(m));
    let what = [Some(name), line, auth]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    cx.tr.f(
        "in_effect.written",
        &[
            ("what", &what),
            ("time", &crate::pages::clock_time(w.applied_at, false)),
            ("generation", &w.generation),
        ],
    )
}

fn row_ui(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &InEffect,
    row: &ConsumerRuntime,
    actions: &mut Vec<RowAction>,
) {
    let (status, label) = effect_label(cx, &row.effect);
    ui.horizontal(|ui| {
        ui.add(StatusDot::new(status));
        ui.label(Text::strong(cx.tr.text(&row.role.text()), cx.pal));
        ui.label(Text::small(label, cx.pal));
    });
    let pid = row.core_pid.unwrap_or(row.pid);
    ui.label(Text::mono_muted(
        identity_line(cx, pid, row.started_at, row.cwd.as_deref()),
        cx.pal,
    ));
    if let RuntimeEffect::Previous { running } = &row.effect {
        let text = match state.view.as_ref().and_then(|v| before_edit(v, running)) {
            Some(e) => cx.tr.f(
                "in_effect.before_edit",
                &[
                    ("file", &e.kind.file_name()),
                    ("time", &crate::pages::clock_time(e.at, false)),
                ],
            ),
            None => running_text(cx, running),
        };
        ui.label(Text::small(text, cx.pal));
    }
    match &row.busy {
        BusyState::Running { threads } => {
            for t in threads {
                crate::shell::busy_line(ui, cx, t);
            }
            if row.is_previous() {
                ui.add(
                    egui::Label::new(Text::small(cx.tr.t("in_effect.interrupts"), cx.pal)).wrap(),
                );
            }
        }
        BusyState::Unknown if row.is_previous() => {
            ui.label(Text::small(cx.tr.t("in_effect.busy_unknown"), cx.pal));
        }
        _ => {}
    }
    if !row.is_previous() {
        return;
    }
    ui.add(egui::Label::new(Text::small(cx.tr.text(&row.advice.text()), cx.pal)).wrap());
    let stopping = state
        .stop
        .as_ref()
        .is_some_and(|(_, r, _)| r.pid == row.pid);
    ui.horizontal_wrapped(|ui| {
        match &row.advice {
            RestartAdvice::HotRestart => {
                if let Some(a) = cx.data.active_state().and_then(|a| a.profile_id.clone()) {
                    let line = cx.data.active_state().and_then(|a| a.line_id.clone());
                    if ui
                        .add(
                            Button::primary(cx.tr.t("in_effect.restart_core"))
                                .small()
                                .enabled(!cx.busy),
                        )
                        .clicked()
                    {
                        actions.push(RowAction::Out(Action::Switch {
                            profile: a,
                            line,
                            auth: None,
                        }));
                    }
                }
            }
            RestartAdvice::FullRestart
                if ui
                    .add(
                        Button::primary(cx.tr.t("in_effect.reopen_app"))
                            .small()
                            .enabled(!cx.busy),
                    )
                    .clicked() =>
            {
                actions.push(RowAction::Out(Action::RestartApp));
            }
            _ => {}
        }
        if let Some(text) = copy_text(row) {
            let copied = state.copied.as_deref() == Some(text);
            let label = if copied {
                cx.tr.t("in_effect.copied")
            } else {
                cx.tr.f("in_effect.copy", &[("text", &text)])
            };
            if ui.add(Button::new(label).small()).clicked() {
                actions.push(RowAction::Copy(text.to_owned()));
            }
        }
        if row.can_stop
            && state.confirm.is_none()
            && ui
                .add(
                    Button::new(cx.tr.t("in_effect.stop"))
                        .small()
                        .loading(stopping)
                        .enabled(!stopping),
                )
                .clicked()
        {
            actions.push(RowAction::AskStop(row.clone()));
        }
    });
    if let Some((_, result)) = state.results.iter().rev().find(|(p, _)| *p == row.pid) {
        let text = match result {
            Ok(o) => cx.tr.text(&o.text()),
            Err(e) => e.clone(),
        };
        ui.label(Text::small(
            cx.tr.f("in_effect.stop_result", &[("result", &text)]),
            cx.pal,
        ));
    }
    match &state.confirm {
        Some(Confirm::Stop(r)) if r.pid == row.pid => {
            confirm_box(ui, cx, row, false, None, actions);
        }
        Some(Confirm::Force(r, why)) if r.pid == row.pid => {
            confirm_box(ui, cx, row, true, Some(*why), actions);
        }
        _ => {}
    }
}

fn confirm_box(
    ui: &mut Ui,
    cx: &Cx<'_>,
    row: &ConsumerRuntime,
    force: bool,
    why: Option<StopOutcome>,
    actions: &mut Vec<RowAction>,
) {
    let Some(process) = row_process(row) else {
        return;
    };
    crate::design::widgets::Card::new().flat().show(ui, |ui| {
        let title = if force {
            match why {
                Some(StopOutcome::ForceOnly) => cx.tr.t("in_effect.force_only"),
                _ => cx.tr.t("in_effect.force_title"),
            }
        } else {
            cx.tr.t("in_effect.stop_title")
        };
        ui.label(Text::strong(title, cx.pal));
        ui.add(
            egui::Label::new(Text::small(
                cx.tr.text(&superba_core::stop_effect(&process)),
                cx.pal,
            ))
            .wrap(),
        );
        if row.busy.is_running() {
            ui.label(Text::small(cx.tr.t("in_effect.busy_warning"), cx.pal));
        }
        ui.horizontal(|ui| {
            let go = if force {
                Button::danger(cx.tr.t("in_effect.force"))
            } else {
                Button::primary(cx.tr.t("in_effect.stop_confirm"))
            };
            if ui.add(go.small()).clicked() {
                actions.push(RowAction::Stop(row.clone(), force));
            }
            if ui
                .add(Button::new(cx.tr.t("action.cancel")).small())
                .clicked()
            {
                actions.push(RowAction::Dismiss);
            }
        });
    });
}
