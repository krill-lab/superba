//! 诊断 / Diagnostics on the Router page: the same report as `spb router
//! doctor` (program, background service, configuration, providers, port, log),
//! shown as one block with a Copy button so a user can paste it into a support
//! request. Opened from the service row, from "查看原因" under a failed start,
//! and from the command palette.

use egui::{Align, Layout, ScrollArea, Ui};
use superba_core::{Error, RouterDoctorReport};

use super::{Dialog, State};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Status, Text};
use crate::pages::{Cx, error_text, note};
use crate::runtime::ReadTask;

/// Diagnostics state of the page.
#[derive(Default)]
pub struct DoctorState {
    task: Option<ReadTask<RouterDoctorReport>>,
    result: Option<Result<RouterDoctorReport, Error>>,
    copied: bool,
}

impl DoctorState {
    /// A report is being collected.
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }

    /// The last report (tests).
    pub fn report(&self) -> Option<&RouterDoctorReport> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    pub(super) fn poll(&mut self) {
        if let Some(task) = &self.task
            && let Some(result) = task.poll()
        {
            self.task = None;
            self.result = Some(result);
        }
    }
}

/// Open the dialog and collect a fresh report.
pub fn open(state: &mut State, cx: &Cx<'_>) {
    state.dialog = Some(Dialog::Doctor);
    if state.doctor.task.is_none() {
        let service = cx.service.clone();
        state.doctor.copied = false;
        state.doctor.task = Some(crate::runtime::task(async move {
            service.router_doctor().await
        }));
    }
}

/// The report as text in the UI language.
pub fn text(cx: &Cx<'_>, report: &RouterDoctorReport) -> String {
    report.render_text(&|t| cx.tr.text(t))
}

/// The dialog's content.
pub fn dialog(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, close: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(Text::title(cx.tr.t("router.doctor.title"), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::ghost(cx.tr.t("router.doctor.close")).small())
                .clicked()
            {
                *close = true;
            }
            if ui
                .add(
                    Button::ghost(cx.tr.t("router.doctor.refresh"))
                        .icon(Icon::Refresh)
                        .small()
                        .loading(state.doctor.loading())
                        .enabled(!state.doctor.loading()),
                )
                .clicked()
            {
                open(state, cx);
            }
            let report = state.doctor.report().cloned();
            let label = if state.doctor.copied {
                cx.tr.t("router.doctor.copied")
            } else {
                cx.tr.t("router.doctor.copy")
            };
            if ui
                .add(
                    Button::primary(label)
                        .icon(Icon::Copy)
                        .small()
                        .enabled(report.is_some()),
                )
                .clicked()
                && let Some(report) = report
            {
                ui.ctx().copy_text(text(cx, &report));
                state.doctor.copied = true;
            }
        });
    });
    ui.add(egui::Label::new(Text::small(cx.tr.t("router.doctor.hint"), cx.pal)).wrap());
    match (&state.doctor.result, state.doctor.loading()) {
        (_, true) => {
            ui.label(Text::muted(cx.tr.t("router.doctor.running"), cx.pal));
        }
        (Some(Ok(report)), false) => {
            let body = text(cx, report);
            // A fixed height: the dialog is sized while the report loads.
            ScrollArea::vertical()
                .max_height(440.0)
                .min_scrolled_height(440.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(Text::mono(body, cx.pal).size(11.5))
                            .wrap()
                            .selectable(true),
                    );
                });
        }
        (Some(Err(e)), false) => note(ui, cx, Status::Error, error_text(cx.tr, e)),
        (None, false) => {}
    }
}
