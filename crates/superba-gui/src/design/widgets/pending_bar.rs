//! Pending bar (UX §2.4): the unsaved edits of a page in its footer —
//! "3 项更改未保存 · 查看差异 · 放弃 · 保存并应用". Shared by Settings and Models.
//! Texts come translated from the page; the shell maps Ctrl/Cmd+S to "save".

use egui::{Sense, Ui, vec2};

use super::{Button, Status, StatusDot, Text};
use crate::design::tokens::Palette;

/// Translated texts of a [`PendingBar`].
#[derive(Clone, Debug, Default)]
pub struct PendingLabels {
    /// "3 项更改未保存".
    pub count: String,
    /// "查看差异".
    pub diff: String,
    /// "放弃".
    pub discard: String,
    /// "保存并应用".
    pub save: String,
}

/// What the user chose in a [`PendingBar`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PendingAction {
    /// Nothing this frame.
    #[default]
    None,
    /// Show the diff.
    Diff,
    /// Drop the pending edits.
    Discard,
    /// Save (and apply).
    Save,
}

/// The pending-edits bar; draws nothing while `count` is 0.
#[must_use = "call .show(ui)"]
pub struct PendingBar {
    count: usize,
    labels: PendingLabels,
    saving: bool,
    enabled: bool,
    error: Option<String>,
}

impl PendingBar {
    /// A bar for `count` pending edits.
    pub fn new(count: usize, labels: PendingLabels) -> Self {
        Self {
            count,
            labels,
            saving: false,
            enabled: true,
            error: None,
        }
    }

    /// A save is running (spinner on the save button, other buttons disabled).
    pub fn saving(mut self, saving: bool) -> Self {
        self.saving = saving;
        self
    }

    /// Disable saving (read-only page, switch running).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The last save failed: shown before the count.
    pub fn error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }

    /// Draw right-aligned in the current row; returns the chosen action.
    pub fn show(self, ui: &mut Ui) -> PendingAction {
        if self.count == 0 {
            return PendingAction::None;
        }
        let pal = Palette::of(ui.ctx());
        let mut action = PendingAction::None;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let idle = self.enabled && !self.saving;
            if ui
                .add(
                    Button::primary(self.labels.save.clone())
                        .small()
                        .loading(self.saving)
                        .enabled(idle),
                )
                .clicked()
            {
                action = PendingAction::Save;
            }
            if ui
                .add(
                    Button::new(self.labels.discard.clone())
                        .small()
                        .enabled(!self.saving),
                )
                .clicked()
            {
                action = PendingAction::Discard;
            }
            if ui
                .add(Button::ghost(self.labels.diff.clone()).small())
                .clicked()
            {
                action = PendingAction::Diff;
            }
            ui.add_space(4.0);
            ui.label(Text::small(self.labels.count.clone(), pal).color(pal.text));
            let (r, _) = ui.allocate_exact_size(vec2(8.0, 16.0), Sense::hover());
            ui.painter()
                .circle_filled(r.center(), 3.0, pal.status(Status::Warn));
            if let Some(e) = &self.error {
                ui.add_space(8.0);
                ui.add(egui::Label::new(Text::small(e.clone(), pal).color(pal.red)).truncate());
                ui.add(StatusDot::new(Status::Error));
            }
        });
        action
    }
}
