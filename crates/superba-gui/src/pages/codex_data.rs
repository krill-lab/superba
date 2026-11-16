//! Codex data location (GitHub issue 15): where Codex keeps its conversations
//! and a guarded move to another disk (preview first, then copy, verify and
//! link). Mounted in Superba → Data below the storage use.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Ui;
use superba_core::Error;
use superba_core::data::{
    CodexDataLocation, CodexDataMoveReport, CodexDataPlan, CodexDirPlacement, DataMoveProgress,
    DataMoveProgressSink,
};
use superba_core::paths::display_path;

use super::Cx;
use super::data::bytes;
use crate::design::widgets::{Button, Card, Text};
use crate::runtime::{ReadTask, task};

/// Controls, background tasks and feedback of the Codex data card.
#[derive(Debug, Default)]
pub struct State {
    /// New folder entered by the user or the folder picker.
    pub destination: String,
    /// Latest placement.
    pub location: Option<CodexDataLocation>,
    /// The preview the user is looking at.
    pub plan: Option<CodexDataPlan>,
    /// Last completed move.
    pub report: Option<CodexDataMoveReport>,
    /// Last error.
    pub error: Option<Error>,
    reading: Option<ReadTask<CodexDataLocation>>,
    planning: Option<ReadTask<CodexDataPlan>>,
    moving: Option<ReadTask<CodexDataMoveReport>>,
    progress: Arc<Mutex<DataMoveProgress>>,
    requested: bool,
}

impl State {
    /// A move is running (other data operations wait).
    pub fn busy(&self) -> bool {
        self.moving.is_some()
    }

    fn read(&mut self, cx: &Cx<'_>) {
        if self.reading.is_none() {
            let service = cx.service.clone();
            self.reading = Some(task(async move { service.codex_data_where().await }));
            self.requested = true;
        }
    }

    fn poll(&mut self) {
        if let Some(result) = self.reading.as_ref().and_then(ReadTask::poll) {
            self.reading = None;
            match result {
                Ok(location) => self.location = Some(location),
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(result) = self.planning.as_ref().and_then(ReadTask::poll) {
            self.planning = None;
            match result {
                Ok(plan) => self.plan = Some(plan),
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(result) = self.moving.as_ref().and_then(ReadTask::poll) {
            self.moving = None;
            self.plan = None;
            match result {
                Ok(report) => {
                    self.location = Some(report.location.clone());
                    self.report = Some(report);
                }
                Err(e) => {
                    self.error = Some(e);
                    self.requested = false;
                }
            }
        }
    }
}

fn size_of(location: &CodexDataLocation) -> u64 {
    location.dirs.iter().map(|d| d.bytes).sum()
}

/// The Codex data card.
pub fn card_ui(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, enabled: bool) {
    state.poll();
    if !state.requested && !state.busy() {
        state.read(cx);
    }
    if state.busy() || state.reading.is_some() || state.planning.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let available = enabled && !state.busy() && state.planning.is_none();
    Card::new().flat().show(ui, |ui| {
        ui.label(Text::strong(cx.tr.t("prefs.codex_data.title"), cx.pal));
        ui.label(Text::small(cx.tr.t("prefs.codex_data.note"), cx.pal));
        match &state.location {
            None => {
                ui.label(Text::small(cx.tr.t("prefs.data.counting"), cx.pal));
            }
            Some(location) => {
                let size = bytes(size_of(location));
                let home = display_path(&location.codex_home);
                let line = match &location.directory {
                    None => cx.tr.f(
                        "prefs.codex_data.in_home",
                        &[("path", &home), ("size", &size)],
                    ),
                    Some(dir) => cx.tr.f(
                        "prefs.codex_data.moved_to",
                        &[
                            ("path", &display_path(dir)),
                            ("home", &home),
                            ("size", &size),
                        ],
                    ),
                };
                ui.label(line);
                for dir in &location.dirs {
                    match &dir.placement {
                        CodexDirPlacement::Linked {
                            target,
                            reachable: false,
                            ..
                        } => {
                            ui.label(Text::small(
                                cx.tr.f(
                                    "prefs.codex_data.unreachable",
                                    &[("path", &display_path(target))],
                                ),
                                cx.pal,
                            ));
                        }
                        CodexDirPlacement::Linked { managed: false, .. } => {
                            ui.label(Text::small(cx.tr.t("prefs.codex_data.foreign"), cx.pal));
                        }
                        _ => {}
                    }
                }
                if location.pending {
                    ui.label(Text::small(cx.tr.t("prefs.codex_data.pending"), cx.pal));
                }
                for path in &location.leftovers {
                    ui.label(Text::small(
                        cx.tr
                            .f("prefs.data.leftovers", &[("path", &display_path(path))]),
                        cx.pal,
                    ));
                }
            }
        }
        let moved = state
            .location
            .as_ref()
            .is_some_and(|l| l.directory.is_some());
        ui.add_enabled_ui(available, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.destination)
                        .desired_width((ui.available_width() - 110.0).max(100.0))
                        .hint_text(cx.tr.t("prefs.codex_data.destination")),
                )
                .accessible_name(cx.tr.t("prefs.codex_data.destination"));
                #[cfg(feature = "file-dialogs")]
                if ui
                    .add(Button::new(cx.tr.t("prefs.data.choose")).small())
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    state.destination = display_path(&path);
                }
            });
            ui.horizontal(|ui| {
                let mut wanted = None;
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.codex_data.preview"))
                            .small()
                            .enabled(!state.destination.trim().is_empty()),
                    )
                    .clicked()
                {
                    wanted = Some(Some(PathBuf::from(state.destination.trim())));
                }
                if moved
                    && ui
                        .add(Button::ghost(cx.tr.t("prefs.codex_data.back")).small())
                        .clicked()
                {
                    wanted = Some(None);
                }
                if let Some(directory) = wanted {
                    let service = cx.service.clone();
                    state.error = None;
                    state.report = None;
                    state.plan = None;
                    state.planning = Some(task(
                        async move { service.codex_data_plan(directory).await },
                    ));
                }
            });
        });
        if state.planning.is_some() {
            ui.spinner();
        }
        if let Some(plan) = &state.plan {
            plan_ui(ui, cx, plan);
            let start = ui
                .add(
                    Button::new(cx.tr.t(if plan.destination.is_some() {
                        "prefs.codex_data.move"
                    } else {
                        "prefs.codex_data.move_back"
                    }))
                    .enabled(available && plan.ready()),
                )
                .clicked();
            if start {
                let directory = plan.destination.clone();
                let service = cx.service.clone();
                let shared = state.progress.clone();
                *shared.lock().unwrap_or_else(|p| p.into_inner()) = DataMoveProgress::default();
                let repaint = ui.ctx().clone();
                let progress: DataMoveProgressSink = Arc::new(move |progress| {
                    *shared.lock().unwrap_or_else(|p| p.into_inner()) = progress;
                    repaint.request_repaint();
                    true
                });
                state.error = None;
                state.moving = Some(task(async move {
                    service.codex_data_move(directory, progress).await
                }));
            }
        }
        if state.moving.is_some() {
            let progress = state
                .progress
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let fraction = if progress.total_bytes == 0 {
                0.0
            } else {
                (progress.copied_bytes as f64 / progress.total_bytes as f64).clamp(0.0, 1.0) as f32
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .animate(true)
                    .text(cx.tr.t(superba_core::data::phase_key(progress.phase))),
            );
        }
        if let Some(report) = &state.report {
            let text = match &report.location.directory {
                Some(dir) => cx
                    .tr
                    .f("prefs.codex_data.done", &[("path", &display_path(dir))]),
                None => cx.tr.f(
                    "prefs.codex_data.done_back",
                    &[("path", &display_path(&report.location.codex_home))],
                ),
            };
            ui.label(Text::small(text, cx.pal));
        }
        if let Some(error) = &state.error {
            ui.label(Text::small(cx.tr.text(&error.to_text()), cx.pal));
        }
    });
}

fn plan_ui(ui: &mut Ui, cx: &Cx<'_>, plan: &CodexDataPlan) {
    if !plan.moves.is_empty() {
        egui::Grid::new("codex-data-plan")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for item in &plan.moves {
                    ui.label(&item.name);
                    ui.label(Text::small(
                        cx.tr.f(
                            "prefs.codex_data.item",
                            &[
                                ("from", &display_path(&item.from)),
                                ("to", &display_path(&item.to)),
                                ("size", &bytes(item.bytes)),
                            ],
                        ),
                        cx.pal,
                    ));
                    ui.end_row();
                }
            });
        let free = plan
            .available_bytes
            .map(bytes)
            .unwrap_or_else(|| "?".to_owned());
        ui.label(Text::small(
            cx.tr.f(
                "prefs.codex_data.total",
                &[("size", &bytes(plan.total_bytes)), ("free", &free)],
            ),
            cx.pal,
        ));
    }
    for note in &plan.notes {
        ui.label(Text::small(cx.tr.text(note), cx.pal));
    }
    for blocker in &plan.blockers {
        ui.label(Text::small(
            cx.tr.f(
                "prefs.codex_data.blocked",
                &[("reason", &cx.tr.text(blocker))],
            ),
            cx.pal,
        ));
    }
}
