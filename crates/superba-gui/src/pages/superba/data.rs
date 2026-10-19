//! Data location, restore and uninstall.
use super::{Action, Cx, row, section};
use crate::design::widgets::{Button, Card};
use egui::Ui;
use superba_core::{RestoreTarget, Settings};
/// Draw this section and save changes immediately.
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    backups: &mut super::backups::State,
    storage: &mut crate::pages::data::State,
    out: &mut Vec<Action>,
) {
    let before = settings.clone();
    data_location_section(ui, cx, settings, storage, out);
    ui.add_space(24.0);
    super::backups::show(ui, cx, backups, out);

    ui.add_space(24.0);
    section(ui, cx, &cx.tr.t("prefs.restore.title"), None);
    Card::new().flat().show(ui, |ui| {
        let baseline = cx
            .data
            .overview
            .value
            .as_ref()
            .is_some_and(|o| o.baseline_captured);
        row(
            ui,
            cx,
            &cx.tr.t("prefs.restore.original"),
            &cx.tr.t("prefs.restore.original_desc"),
            false,
            |ui| {
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.restore.original_button"))
                            .small()
                            .enabled(baseline && !cx.busy),
                    )
                    .clicked()
                {
                    out.push(Action::restore_original());
                }
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.restore.official"),
            &cx.tr.t("prefs.restore.official_desc"),
            true,
            |ui| {
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.restore.official_button"))
                            .small()
                            .enabled(!cx.busy && !cx.data.official_active()),
                    )
                    .clicked()
                {
                    out.push(Action::Restore(RestoreTarget::Official));
                }
            },
        );
    });

    ui.add_space(24.0);
    section(ui, cx, &cx.tr.t("settings.danger"), None);
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("settings.uninstall.title"),
            &cx.tr.t("settings.uninstall.desc"),
            true,
            |ui| {
                if ui
                    .add(
                        Button::danger(cx.tr.t("settings.uninstall.button"))
                            .small()
                            .enabled(!cx.busy),
                    )
                    .clicked()
                {
                    out.push(Action::Uninstall);
                }
            },
        );
    });

    if *settings != before {
        out.push(Action::SaveSettings);
    }
}

/// Independent data-location controls, storage use, retention and cleanup.
pub fn data_location_section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    storage: &mut crate::pages::data::State,
    out: &mut Vec<Action>,
) {
    crate::pages::data::section_ui(ui, cx, settings, storage, out);
}
