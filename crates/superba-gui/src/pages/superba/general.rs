//! Switching, window behavior and setup.
use super::{Action, Cx, row, section};
use crate::design::widgets::{Button, Card, Segmented, Toggle};
use egui::Ui;
use superba_core::{CloseAction, Locale, Settings};
/// Preferences → Window (item 70): what closing the window does and whether
/// Superba starts hidden in the tray. Without a tray both are disabled and
/// the reason replaces the description (closing then always quits).
pub fn window_section(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings) {
    use crate::tray::TrayStatus;
    use crate::tray::close::{CloseContext, Unset};
    let mac = cfg!(target_os = "macos");
    let key = |plain: &'static str, on_mac: &'static str| if mac { on_mac } else { plain };
    let close = CloseContext {
        setting: settings.close_action,
        hide: cx.data.tray.hide_support(),
        unset: Unset::platform(),
    };
    let unavailable = match &cx.data.tray {
        TrayStatus::Unavailable(why) => Some(cx.tr.t(why.reason_key())),
        TrayStatus::Shown(_) => None,
    };
    section(ui, cx, &cx.tr.t("tray.prefs.section"), None);
    Card::new().flat().show(ui, |ui| {
        let desc = match (&unavailable, close.effective()) {
            (Some(why), _) => why.clone(),
            (None, None) => format!(
                "{}{}{}",
                cx.tr
                    .t(key("tray.prefs.close.desc", "tray.prefs.close.desc_mac")),
                // Chinese sentences follow each other without a space.
                if cx.tr.locale() == Locale::ZhCn {
                    ""
                } else {
                    " "
                },
                cx.tr.t("tray.prefs.close.ask")
            ),
            (None, Some(_)) => cx
                .tr
                .t(key("tray.prefs.close.desc", "tray.prefs.close.desc_mac")),
        };
        row(
            ui,
            cx,
            &cx.tr.t("tray.prefs.close.title"),
            &desc,
            false,
            |ui| {
                let mut value = close.effective();
                let quit = cx.tr.t("tray.prefs.close.quit");
                let tray = cx
                    .tr
                    .t(key("tray.prefs.close.tray", "tray.prefs.close.tray_mac"));
                let control = Segmented::new("close-action", &mut value);
                let control = match &unavailable {
                    Some(why) => control
                        .disabled_option(Some(CloseAction::Quit), quit, why.clone())
                        .disabled_option(Some(CloseAction::Tray), tray, why.clone()),
                    None => control
                        .option(Some(CloseAction::Quit), quit)
                        .option(Some(CloseAction::Tray), tray),
                };
                if control.small().show(ui).changed() && unavailable.is_none() {
                    settings.close_action = value;
                }
            },
        );
        row(
            ui,
            cx,
            &cx.tr
                .t(key("tray.prefs.start.title", "tray.prefs.start.title_mac")),
            // Without a tray the row above says why; the toggle is disabled.
            &cx.tr
                .t(key("tray.prefs.start.desc", "tray.prefs.start.desc_mac")),
            true,
            |ui| {
                ui.add(
                    Toggle::new(
                        &mut settings.start_in_tray,
                        cx.tr
                            .t(key("tray.prefs.start.title", "tray.prefs.start.title_mac")),
                    )
                    .enabled(unavailable.is_none()),
                );
            },
        );
    });
}

/// Draw this section and save changes immediately.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, out: &mut Vec<Action>) {
    let before = settings.clone();
    section(ui, cx, &cx.tr.t("settings.switching"), None);
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("settings.countdown.title"),
            &cx.tr.t("settings.countdown.desc"),
            false,
            |ui| {
                Segmented::new("countdown", &mut settings.countdown_secs)
                    .option(0, cx.tr.t("settings.countdown.off"))
                    .option(3, "3 s")
                    .option(5, "5 s")
                    .option(10, "10 s")
                    .small()
                    .show(ui);
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("settings.safe_mode.title"),
            &cx.tr.t("settings.safe_mode.desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.safe_mode,
                    cx.tr.t("settings.safe_mode.title"),
                ));
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("settings.open_on_start.title"),
            &cx.tr.t("settings.open_on_start.desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.open_app_on_start,
                    cx.tr.t("settings.open_on_start.title"),
                ));
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("settings.auto_probe.title"),
            &cx.tr.t("settings.auto_probe.desc"),
            true,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.auto_probe,
                    cx.tr.t("settings.auto_probe.title"),
                ));
            },
        );
    });

    ui.add_space(24.0);
    window_section(ui, cx, settings);

    section(ui, cx, &cx.tr.t("prefs.wizard.title"), None);
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("prefs.wizard.run"),
            &cx.tr.t("prefs.wizard.body"),
            true,
            |ui| {
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.wizard.run"))
                            .small()
                            .enabled(!cx.busy),
                    )
                    .clicked()
                {
                    out.push(Action::StartWizard);
                }
            },
        );
    });

    if *settings != before {
        out.push(Action::SaveSettings);
    }
}
