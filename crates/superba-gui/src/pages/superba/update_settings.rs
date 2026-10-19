//! Preferences shared by Superba updates.
use super::{Action, Cx, row, section};
use crate::design::widgets::{Card, Segmented, Toggle};
use egui::Ui;
use superba_core::Settings;
/// Draw this section and save changes immediately.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, out: &mut Vec<Action>) {
    let before = settings.clone();
    section(ui, cx, &cx.tr.t("prefs.updates.title"), None);
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("prefs.updates.auto"),
            &cx.tr.t("prefs.updates.auto_desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.auto_update_check,
                    cx.tr.t("prefs.updates.auto"),
                ));
            },
        );
        update_channel_row(ui, cx, settings);
    });

    if *settings != before {
        out.push(Action::SaveSettings);
    }
}
/// The Superba update channel (Stable / Preview). Until the user picks one the
/// running build decides (Preview on a pre-release build); a pick is saved.
pub fn update_channel_row(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings) {
    row(
        ui,
        cx,
        &cx.tr.t("prefs.updates.channel"),
        &cx.tr.t("prefs.updates.channel_desc"),
        true,
        |ui| {
            let running = settings.update_channel_for(env!("CARGO_PKG_VERSION"));
            let mut channel = running;
            Segmented::new("update-channel", &mut channel)
                .label(cx.tr.t("prefs.updates.channel"))
                .option(
                    superba_core::UpdateChannel::Stable,
                    cx.tr.t("updates.superba.channel_stable"),
                )
                .option(
                    superba_core::UpdateChannel::Preview,
                    cx.tr.t("updates.superba.channel_preview"),
                )
                .small()
                .show(ui);
            if channel != running {
                settings.update_channel = Some(channel);
            }
        },
    );
}
