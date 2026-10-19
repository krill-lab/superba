//! Data location, restore and uninstall.
use super::{Action, Cx, Page, row, section};
use crate::design::widgets::{Button, Card};
use egui::Ui;
use superba_core::{RestoreTarget, Settings};
/// Draw this section and save changes immediately.
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    backups: &mut super::backups::State,
    out: &mut Vec<Action>,
) {
    let before = settings.clone();
    section(ui, cx, &cx.tr.t("prefs.data.title"), None);
    Card::new().flat().show(ui, |ui| {
        let dir = cx
            .data
            .paths
            .as_ref()
            .map(|p| superba_core::paths::display_path(&p.data_dir))
            .unwrap_or_default();
        row(ui, cx, &cx.tr.t("prefs.data.dir"), &dir, false, |ui| {
            if ui
                .add(
                    Button::new(cx.tr.t("prefs.data.open_dir"))
                        .small()
                        .enabled(cx.data.paths.is_some()),
                )
                .clicked()
            {
                out.push(Action::OpenDataDir);
            }
        });
        row(
            ui,
            cx,
            &cx.tr.t("prefs.data.requests"),
            &cx.tr.t("prefs.data.retention_moved"),
            true,
            |ui| {
                if ui
                    .add(Button::ghost(cx.tr.t("prefs.data.open_router")).small())
                    .clicked()
                {
                    out.push(Action::Go(Page::Router));
                }
            },
        );
    });

    data_location_section(ui, cx, settings, out);
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

/// Mount point for the separate data-location feature (GH#15).
/// Keep its state and controls in this section when that feature is integrated.
pub fn data_location_section(
    _ui: &mut Ui,
    _cx: &Cx<'_>,
    _settings: &mut Settings,
    _out: &mut Vec<Action>,
) {
}
