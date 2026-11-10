//! Project folders ChatGPT dropped (user bugs 135/136 follow-up). The
//! overview's to-do names them; this dialog lists them with where each was
//! found and when it was last used, ticked when the app's own project list
//! had it, and the answers: bring the ticked ones back (written at the next
//! launch through Superba; the unticked ones are not offered again, which the
//! dialog says), don't (never offered again), or not now (the to-do stays).
//! It never opens by itself.

use std::collections::BTreeSet;
use std::path::PathBuf;

use egui::{Align, Layout, Ui};
use superba_core::{LostProject, LostProjectSource, LostProjects, LostProjectsAnswer};

use crate::design::widgets::{Button, Text, dialog, dialog_body};
use crate::pages::{Action, Cx};

/// Dialog state between frames.
#[derive(Debug, Default)]
pub struct LostProjectsDialog {
    /// Fingerprint of the list being shown (`None` = closed).
    open: Option<String>,
    /// Ticked folders.
    ticked: BTreeSet<PathBuf>,
}

impl LostProjectsDialog {
    /// Open for `lost` (the to-do's button): the folders from the app's
    /// project list start ticked.
    pub fn open(&mut self, lost: &LostProjects) {
        self.open = Some(lost.fingerprint.clone());
        self.ticked = lost
            .candidates
            .iter()
            .filter(|c| c.recommended())
            .map(|c| c.path.clone())
            .collect();
    }

    /// Shown now.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The ticked folders, in the list's order.
    pub fn ticked(&self, lost: &LostProjects) -> Vec<PathBuf> {
        lost.candidates
            .iter()
            .filter(|c| self.ticked.contains(&c.path))
            .map(|c| c.path.clone())
            .collect()
    }

    /// Tick or untick `path`.
    pub fn set(&mut self, path: &PathBuf, on: bool) {
        if on {
            self.ticked.insert(path.clone());
        } else {
            self.ticked.remove(path);
        }
    }

    /// The action for "bring back" (`None` when nothing is ticked).
    pub fn restore_action(&self, lost: &LostProjects) -> Option<Action> {
        let paths = self.ticked(lost);
        (!paths.is_empty()).then(|| Action::AnswerLostProjects {
            fingerprint: lost.fingerprint.clone(),
            answer: LostProjectsAnswer::Restore { paths },
        })
    }

    /// The action for "don't bring back".
    pub fn dismiss_action(lost: &LostProjects) -> Action {
        Action::AnswerLostProjects {
            fingerprint: lost.fingerprint.clone(),
            answer: LostProjectsAnswer::Dismiss,
        }
    }

    fn close(&mut self) {
        self.open = None;
        self.ticked.clear();
    }
}

/// "From ChatGPT's project list · 3 chats" (translated).
pub fn sources_text(cx: &Cx<'_>, p: &LostProject) -> String {
    p.sources
        .iter()
        .map(|s| match s {
            LostProjectSource::ProjectList => cx.tr.t("lost_projects.source.list"),
            LostProjectSource::Chats { count } => {
                cx.tr.f("lost_projects.source.chats", &[("n", count)])
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn row(ui: &mut Ui, cx: &Cx<'_>, state: &mut LostProjectsDialog, p: &LostProject) {
    let mut on = state.ticked.contains(&p.path);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut on, "").changed() {
            state.set(&p.path, on);
        }
        ui.label(Text::strong(p.name.clone(), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(t) = p.last_used {
                ui.label(Text::caption(
                    cx.tr.f("lost_projects.last_used", &[("ago", &cx.ago(t))]),
                    cx.pal,
                ));
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.add(
            egui::Label::new(Text::mono_muted(p.path.display().to_string(), cx.pal).size(11.5))
                .truncate(),
        );
    });
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.label(Text::small(sources_text(cx, p), cx.pal).color(cx.pal.text_muted));
    });
    ui.add_space(4.0);
}

/// Draw the dialog when it is open.
pub fn show(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    state: &mut LostProjectsDialog,
    out: &mut Vec<Action>,
) {
    let Some(open) = state.open.clone() else {
        return;
    };
    let Some(lost) = cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.lost_projects.as_ref())
        .filter(|l| l.fingerprint == open)
        .cloned()
    else {
        // Answered, or the list changed: the to-do offers the new one.
        state.close();
        return;
    };
    let pal = cx.pal;
    let tr = cx.tr;
    let mut close = false;
    let response = dialog(ctx, egui::Id::unique("lost-projects"), 620.0, |ui| {
        ui.label(Text::h2(tr.t("lost_projects.title"), pal));
        dialog_body(ui, |ui| {
            ui.add(egui::Label::new(Text::body(tr.t("lost_projects.body"), pal)).wrap());
            ui.add_space(8.0);
            for p in &lost.candidates {
                row(ui, cx, state, p);
            }
            ui.add_space(4.0);
            ui.add(egui::Label::new(Text::muted(tr.t("lost_projects.promise"), pal)).wrap());
        });
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            let n = state.ticked(&lost).len();
            let restore = state.restore_action(&lost);
            if ui
                .add(
                    Button::primary(tr.f("lost_projects.restore", &[("n", &n)]))
                        .enabled(restore.is_some()),
                )
                .clicked()
                && let Some(a) = restore
            {
                out.push(a);
                close = true;
            }
            if ui
                .add(Button::new(tr.t("lost_projects.dismiss")))
                .on_hover_text(tr.t("lost_projects.dismiss_tip"))
                .clicked()
            {
                out.push(LostProjectsDialog::dismiss_action(&lost));
                close = true;
            }
            if ui
                .add(Button::ghost(tr.t("lost_projects.later")).kbd("Esc"))
                .clicked()
            {
                close = true;
            }
        });
    });
    if close || response.should_close() {
        state.close();
    }
}
