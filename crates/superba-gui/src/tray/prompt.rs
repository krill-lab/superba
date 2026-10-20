//! The first-close question (Windows, Linux): keep Superba running in the
//! tray, or quit? Asked once; the answer is saved as `close_action` and can
//! be changed in Preferences.

use superba_core::CloseAction;

use crate::design::tokens::Palette;
use crate::design::widgets::{Button, Text, dialog, dialog_body};
use crate::i18n::Tr;

/// What the user answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// "Keep running in the tray" / "Quit Superba".
    Chose(CloseAction),
    /// "Cancel" or Escape: the window stays open, nothing is saved.
    Cancel,
}

impl Answer {
    /// The choice, `None` when cancelled.
    pub fn choice(self) -> Option<CloseAction> {
        match self {
            Answer::Chose(a) => Some(a),
            Answer::Cancel => None,
        }
    }
}

/// Draw the question; `Some` once answered.
pub fn show(ctx: &egui::Context, tr: &Tr) -> Option<Answer> {
    let pal = Palette::of(ctx);
    let mut answer = None;
    let response = dialog(ctx, egui::Id::unique("tray-first-close"), 520.0, |ui| {
        ui.label(Text::h2(tr.t("tray.prompt.title"), pal));
        dialog_body(ui, |ui| {
            ui.add(egui::Label::new(Text::body(tr.t("tray.prompt.body"), pal)).wrap());
            ui.add(egui::Label::new(Text::muted(tr.t("tray.prompt.later"), pal)).wrap());
        });
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::primary(tr.t("tray.prompt.keep"))).clicked() {
                answer = Some(Answer::Chose(CloseAction::Tray));
            }
            if ui.add(Button::new(tr.t("tray.prompt.quit"))).clicked() {
                answer = Some(Answer::Chose(CloseAction::Quit));
            }
            if ui
                .add(Button::ghost(tr.t("tray.prompt.cancel")).kbd("Esc"))
                .clicked()
            {
                answer = Some(Answer::Cancel);
            }
        });
    });
    if answer.is_none() && response.should_close() {
        answer = Some(Answer::Cancel);
    }
    answer
}
