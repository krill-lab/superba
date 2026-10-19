//! Self-contained data location, storage usage and retention controls.
//! Mounted by Superba → Data without coupling its tasks to the page layout.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Ui;
use superba_core::data::{
    DataCleanupReport, DataLocation, DataMoveProgress, DataMoveProgressSink, DataMoveReport,
    DataUsage,
};
use superba_core::paths::display_path;
use superba_core::{Error, RouterStatsSettings, RouterView, Settings};

use super::{Action, Cx, section};
use crate::design::widgets::{Button, Card, Text};
use crate::runtime::{ReadTask, task};

#[derive(Debug, Default)]
/// Persistent controls, background tasks and feedback for the data block.
pub struct State {
    /// New directory entered by the user or the folder picker.
    pub destination: String,
    /// Most recent disk usage scan.
    pub usage: Option<DataUsage>,
    /// Last operation's error.
    pub error: Option<Error>,
    /// Last completed relocation.
    pub report: Option<DataMoveReport>,
    scan: Option<ReadTask<DataUsage>>,
    location: Option<ReadTask<DataLocation>>,
    moving: Option<ReadTask<DataMoveReport>>,
    cleaning: Option<ReadTask<DataCleanupReport>>,
    saving: Option<ReadTask<RouterView>>,
    desired_stats: Option<RouterStatsSettings>,
    progress: Arc<Mutex<DataMoveProgress>>,
    reclaimed: Option<u64>,
    requested: bool,
}

impl State {
    /// Whether relocating or cleaning currently excludes other GUI mutations.
    pub fn busy(&self) -> bool {
        self.moving.is_some() || self.cleaning.is_some()
    }

    fn scan(&mut self, cx: &Cx<'_>) {
        if self.scan.is_some() {
            return;
        }
        let service = cx.service.clone();
        self.scan = Some(task(async move { service.data_usage().await }));
        self.requested = true;
    }

    pub(crate) fn check_location(&mut self, service: Arc<dyn superba_core::SwitchService>) {
        if self.location.is_none() && !self.busy() {
            self.location = Some(task(async move { service.data_where().await }));
        }
    }

    pub(crate) fn relocated(&mut self) {
        self.usage = None;
        self.scan = None;
        self.requested = false;
    }

    pub(crate) fn poll(&mut self, settings: &mut Settings, out: &mut Vec<Action>) {
        if let Some(result) = self.location.as_ref().and_then(ReadTask::poll) {
            self.location = None;
            if let Ok(location) = result {
                out.push(Action::DataLocationChanged(location));
            }
        }
        if let Some(result) = self.scan.as_ref().and_then(ReadTask::poll) {
            self.scan = None;
            match result {
                Ok(usage) => {
                    out.push(Action::DataLocationChanged(usage.location.clone()));
                    self.usage = Some(usage);
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(result) = self.moving.as_ref().and_then(ReadTask::poll) {
            self.moving = None;
            match result {
                Ok(report) => {
                    out.push(Action::DataLocationChanged(report.location.clone()));
                    self.report = Some(report);
                    self.usage = None;
                    self.requested = false;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(result) = self.cleaning.as_ref().and_then(ReadTask::poll) {
            self.cleaning = None;
            match result {
                Ok(report) => {
                    self.reclaimed = Some(report.reclaimed_bytes);
                    self.usage = Some(report.usage);
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(result) = self.saving.as_ref().and_then(ReadTask::poll) {
            self.saving = None;
            match result {
                Ok(_) => {
                    if let Some(stats) = self.desired_stats.take() {
                        settings.router.stats = stats;
                    }
                }
                Err(error) => {
                    self.error = Some(error);
                    self.desired_stats = None;
                }
            }
        }
    }
}

/// Data block with its section heading.
pub fn section_ui(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    state: &mut State,
    out: &mut Vec<Action>,
) {
    section(ui, cx, &cx.tr.t("prefs.data.title"), None);
    body_ui(ui, cx, settings, state, out);
}

/// Standalone data block, also embeddable in Superba → Data and backups.
pub fn body_ui(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    state: &mut State,
    out: &mut Vec<Action>,
) {
    state.poll(settings, out);
    if !state.requested && !state.busy() {
        state.scan(cx);
    }
    if state.busy() || state.scan.is_some() || state.saving.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let available = !state.busy() && !cx.busy && state.saving.is_none();
    Card::new().flat().show(ui, |ui| {
        let current = state
            .usage
            .as_ref()
            .map(|u| &u.location.directory)
            .or_else(|| cx.data.paths.as_ref().map(|p| &p.data_dir));
        super::superba::row(
            ui,
            cx,
            &cx.tr.t("prefs.data.dir"),
            &current.map(|p| display_path(p)).unwrap_or_default(),
            false,
            |ui| {
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.data.open_dir"))
                            .small()
                            .enabled(current.is_some()),
                    )
                    .clicked()
                {
                    out.push(Action::OpenDataDir);
                }
            },
        );
        ui.label(Text::small(cx.tr.t("prefs.data.move_note"), cx.pal));
        ui.add_enabled_ui(available, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.destination)
                        .desired_width((ui.available_width() - 110.0).max(100.0))
                        .hint_text(cx.tr.t("prefs.data.destination")),
                );
                #[cfg(feature = "file-dialogs")]
                if ui
                    .add(Button::new(cx.tr.t("prefs.data.choose")).small())
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    state.destination = display_path(&path);
                }
            });
            if ui
                .add(
                    Button::new(cx.tr.t("prefs.data.move"))
                        .enabled(!state.destination.trim().is_empty()),
                )
                .clicked()
            {
                let directory = PathBuf::from(state.destination.trim());
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
                state.report = None;
                state.moving = Some(task(
                    async move { service.data_move(directory, progress).await },
                ));
            }
        });
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
            ui.label(Text::small(
                format!(
                    "{} / {}",
                    bytes(progress.copied_bytes),
                    bytes(progress.total_bytes)
                ),
                cx.pal,
            ));
        }
        if let Some(report) = &state.report {
            ui.label(Text::small(
                cx.tr.f(
                    "prefs.data.moved",
                    &[("path", &display_path(&report.location.directory))],
                ),
                cx.pal,
            ));
            if report.source_cleanup_pending {
                ui.label(Text::small(
                    cx.tr.f(
                        "prefs.data.leftovers",
                        &[("path", &display_path(&report.previous))],
                    ),
                    cx.pal,
                ));
            }
        }
        if let Some(error) = &state.error {
            ui.label(Text::small(cx.tr.text(&error.to_text()), cx.pal));
        }
    });

    ui.add_space(12.0);
    Card::new().flat().show(ui, |ui| {
        ui.label(Text::strong(cx.tr.t("prefs.data.storage"), cx.pal));
        if let Some(usage) = &state.usage {
            egui::Grid::new("data-usage")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    for category in &usage.categories {
                        ui.label(cx.tr.t(&format!("data.category.{}", category.category)));
                        ui.label(bytes(category.bytes));
                        ui.end_row();
                    }
                    ui.label(Text::strong(cx.tr.t("prefs.data.total"), cx.pal));
                    ui.label(bytes(usage.total_bytes));
                    ui.end_row();
                });
            ui.separator();
            ui.label(Text::small(
                cx.tr.f(
                    "prefs.data.codex",
                    &[
                        ("path", &display_path(&usage.location.codex_home)),
                        ("total", &bytes(usage.codex_home_bytes)),
                        ("sessions", &bytes(usage.codex_sessions_bytes)),
                    ],
                ),
                cx.pal,
            ));
            if usage.codex_sqlite_bytes > 0 {
                ui.label(Text::small(
                    cx.tr.f(
                        "prefs.data.sqlite",
                        &[("size", &bytes(usage.codex_sqlite_bytes))],
                    ),
                    cx.pal,
                ));
            }
            if usage.skipped > 0 {
                ui.label(Text::small(cx.tr.t("prefs.data.partial"), cx.pal));
            }
        } else {
            ui.label(Text::small(cx.tr.t("prefs.data.counting"), cx.pal));
        }
        if ui
            .add(
                Button::ghost(cx.tr.t("prefs.data.refresh"))
                    .small()
                    .enabled(available && state.scan.is_none()),
            )
            .clicked()
        {
            state.scan(cx);
        }
    });

    ui.add_space(12.0);
    Card::new().flat().show(ui, |ui| {
        let mut stats = state
            .desired_stats
            .clone()
            .unwrap_or_else(|| settings.router.stats.clone());
        let before = stats.clone();
        ui.add_enabled_ui(available, |ui| {
            egui::Grid::new("data-retention")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(cx.tr.t("recording.usage_days"));
                    ui.add(egui::DragValue::new(&mut stats.usage_days).range(1..=3650));
                    ui.end_row();
                    ui.label(cx.tr.t("prefs.data.max_rows"));
                    ui.add(egui::DragValue::new(&mut stats.max_rows).range(1..=10_000_000));
                    ui.end_row();
                });
            if stats != before {
                let service = cx.service.clone();
                state.desired_stats = Some(stats.clone());
                state.saving = Some(task(async move { service.router_set_stats(stats).await }));
            }
            ui.label(Text::small(cx.tr.t("prefs.data.cleanup_note"), cx.pal));
            if ui.add(Button::new(cx.tr.t("prefs.data.clean"))).clicked() {
                state.error = None;
                state.reclaimed = None;
                let service = cx.service.clone();
                state.cleaning = Some(task(async move { service.data_cleanup().await }));
            }
        });
        if state.cleaning.is_some() {
            ui.spinner();
        }
        if let Some(reclaimed) = state.reclaimed {
            ui.label(Text::small(
                cx.tr
                    .f("prefs.data.cleaned", &[("size", &bytes(reclaimed))]),
                cx.pal,
            ));
        }
    });
}

/// Compact binary units for logical file sizes.
pub fn bytes(size: u64) -> String {
    if size >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if size >= 1024 * 1024 {
        format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}
