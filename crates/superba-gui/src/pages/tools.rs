//! Shared tool configuration (the detailed editor follows in M4).
use super::{Action, Cx, Page, page_header};
use crate::design::widgets::{Button, Card, Text};

/// Configuration landing page; installation is reached through Superba.
pub fn show(ui: &mut egui::Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    page_header(ui, cx, Page::Tools, |_| {});
    Card::new().flat().show(ui, |ui| {
        ui.label(Text::strong(
            "Codex · Claude Code · dsh · opencode · pi · ZCode · WorkBuddy",
            cx.pal,
        ));
        ui.add(egui::Label::new(Text::body(cx.tr.t("superba.tools.placeholder"), cx.pal)).wrap());
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for (page, key) in [
                (Page::Settings, "nav.settings"),
                (Page::ClaudeCode, "nav.claude_code"),
            ] {
                if ui.add(Button::new(cx.tr.t(key))).clicked() {
                    out.push(Action::Go(page));
                }
            }
        });
    });
}
