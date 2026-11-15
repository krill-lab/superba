//! Switching, the ChatGPT app location, window behavior and setup.
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Action, Cx, row, section};
use crate::design::widgets::{Button, Card, Segmented, Text, Toggle};
use crate::runtime::{ReadTask, task};
use egui::{RichText, Ui};
use superba_core::paths::display_path;
use superba_core::{AppLocationCheck, CloseAction, Locale, Settings};

/// Settings → General → ChatGPT app location: the typed or picked folder, the
/// check of the saved one or of the one being chosen.
#[derive(Debug, Default)]
pub struct AppLocationState {
    /// The folder being typed or picked.
    input: String,
    /// The last check: the path, then what is there (or why the check failed).
    checked: Option<(PathBuf, Result<AppLocationCheck, String>)>,
    /// A check running in the background.
    pending: Option<PendingCheck>,
}

#[derive(Debug)]
struct PendingCheck {
    path: PathBuf,
    /// Save the location when the check finds an app (a pick, not the
    /// re-check of the saved one).
    save: bool,
    task: ReadTask<AppLocationCheck>,
}

impl AppLocationState {
    fn start(&mut self, cx: &Cx<'_>, path: PathBuf, save: bool) {
        let service = cx.service.clone();
        let target = path.clone();
        self.pending = Some(PendingCheck {
            path,
            save,
            task: task(async move { service.check_app_location(target).await }),
        });
    }

    /// Finish a check; a usable picked location is saved into `settings`.
    fn poll(&mut self, cx: &Cx<'_>, settings: &mut Settings) {
        let Some(result) = self.pending.as_ref().and_then(|p| p.task.poll()) else {
            return;
        };
        let Some(pending) = self.pending.take() else {
            return;
        };
        let result = result.map_err(|e| cx.tr.text(&e.to_text()));
        if pending.save && result.as_ref().is_ok_and(AppLocationCheck::usable) {
            settings.app_location = Some(pending.path.clone());
            self.input.clear();
        }
        self.checked = Some((pending.path, result));
    }
}

/// The text and colour of a check: the versions found, or what is wrong.
fn check_line(cx: &Cx<'_>, path: &Path, result: &Result<AppLocationCheck, String>) -> RichText {
    match result {
        Ok(check @ AppLocationCheck::Install { .. }) => {
            Text::small(cx.tr.text(&check.text(path)), cx.pal).color(cx.pal.green)
        }
        Ok(check) => Text::small(cx.tr.text(&check.text(path)), cx.pal).color(cx.pal.amber),
        Err(message) => Text::small(message.clone(), cx.pal).color(cx.pal.red),
    }
}

/// General → ChatGPT app: where the desktop app is installed when Superba does
/// not find it in the default locations (the setting comes after
/// `SUPERBA_APP_PATH`, before the default locations). A picked folder is
/// checked first and saved only when the app is there.
pub fn app_location_section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    state: &mut AppLocationState,
) {
    state.poll(cx, settings);
    // The saved location's check (versions, or why it no longer works).
    if let Some(saved) = settings.app_location.clone()
        && state.pending.is_none()
        && state.checked.as_ref().is_none_or(|(p, _)| *p != saved)
    {
        state.start(cx, saved, false);
    }
    if state.pending.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let title = cx.tr.t("prefs.app_location.title");
    let desc = cx.tr.t("prefs.app_location.desc");
    if !super::search::visible(ui.ctx(), &[&title, &desc, "prefs.app_location.title"]) {
        return;
    }
    section(ui, cx, &cx.tr.t("prefs.app_location.section"), None);
    Card::new().flat().show(ui, |ui| {
        let checking = state.pending.is_some();
        row(ui, cx, &title, &desc, true, |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("prefs.app_location.reset"))
                        .small()
                        .enabled(settings.app_location.is_some() && !checking),
                )
                .clicked()
            {
                settings.app_location = None;
                state.checked = None;
            }
            #[cfg(feature = "file-dialogs")]
            if ui
                .add(
                    Button::new(cx.tr.t("prefs.app_location.browse"))
                        .small()
                        .enabled(!checking),
                )
                .clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                state.input = display_path(&dir);
                state.start(cx, dir, true);
            }
        });
        // What Superba uses now.
        match (&settings.app_location, &state.checked) {
            (Some(saved), Some((path, result))) if path == saved => {
                ui.add(egui::Label::new(check_line(cx, path, result)).wrap());
            }
            (Some(saved), _) => {
                ui.label(Text::small(
                    cx.tr.f(
                        "prefs.app_location.checking",
                        &[("path", &display_path(saved))],
                    ),
                    cx.pal,
                ));
            }
            (None, _) => {
                let found = cx
                    .data
                    .overview
                    .value
                    .as_ref()
                    .and_then(|o| o.app.installs.first());
                let line = match found {
                    Some(install) => Text::small(
                        cx.tr.f(
                            "prefs.app_location.auto",
                            &[("path", &display_path(&install.path))],
                        ),
                        cx.pal,
                    ),
                    None if cx.data.overview.value.is_some() => {
                        Text::small(cx.tr.t("prefs.app_location.none_found"), cx.pal)
                            .color(cx.pal.amber)
                    }
                    None => Text::small(cx.tr.t("prefs.app_location.not_set"), cx.pal),
                };
                ui.add(egui::Label::new(line).wrap());
            }
        }
        // Type a folder (or a program file) and check it.
        ui.horizontal(|ui| {
            let edit = ui
                .add(
                    egui::TextEdit::singleline(&mut state.input)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(cx.tr.t("prefs.app_location.hint"))
                        .desired_width((ui.available_width() - 110.0).max(120.0)),
                )
                .accessible_name(cx.tr.t("prefs.app_location.title"));
            // "Copy as path" in Explorer wraps the path in quotes.
            let typed = state.input.trim().trim_matches('"').trim().to_owned();
            let submit = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui
                .add(
                    Button::new(cx.tr.t("prefs.app_location.use"))
                        .small()
                        .enabled(!typed.is_empty() && !checking),
                )
                .clicked()
                || submit)
                && !typed.is_empty()
                && !checking
            {
                state.start(cx, PathBuf::from(typed), true);
            }
        });
        // A picked or typed folder that was not saved: why.
        if let Some(pending) = &state.pending
            && pending.save
        {
            ui.label(Text::small(
                cx.tr.f(
                    "prefs.app_location.checking",
                    &[("path", &display_path(&pending.path))],
                ),
                cx.pal,
            ));
        } else if let Some((path, result)) = &state.checked
            && settings.app_location.as_ref() != Some(path)
            && !result.as_ref().is_ok_and(AppLocationCheck::usable)
        {
            ui.add(egui::Label::new(check_line(cx, path, result)).wrap());
        }
        ui.add(egui::Label::new(Text::caption(cx.tr.t("prefs.app_location.note"), cx.pal)).wrap());
    });
    ui.add_space(24.0);
}
/// Preferences → Window: what closing the window does. Without a tray it is disabled and
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
    });
}

/// General → startup. Kept independent for the future Superba settings page.
pub fn autostart_section(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, out: &mut Vec<Action>) {
    let mac = cfg!(target_os = "macos");
    let key = |plain: &'static str, on_mac: &'static str| if mac { on_mac } else { plain };
    let unavailable = !matches!(cx.data.tray, crate::tray::TrayStatus::Shown(_));
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("prefs.autostart.title"),
            &cx.tr.t("prefs.autostart.desc"),
            false,
            |ui| {
                if ui
                    .add(Toggle::new(
                        &mut settings.autostart,
                        cx.tr.t("prefs.autostart.title"),
                    ))
                    .changed()
                {
                    out.push(Action::SetAutostart(settings.autostart));
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
                    .enabled(!unavailable),
                );
            },
        );
    });
}
/// Draw this section and save changes immediately.
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    app_location: &mut AppLocationState,
    out: &mut Vec<Action>,
) {
    let before = settings.clone();
    autostart_section(ui, cx, settings, out);
    ui.add_space(24.0);
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
            &cx.tr.t("settings.speed_menu.title"),
            &cx.tr.t("settings.speed_menu.desc"),
            false,
            |ui| {
                ui.add(
                    Toggle::new(
                        &mut settings.app_speed_menu,
                        cx.tr.t("settings.speed_menu.title"),
                    )
                    .enabled(!settings.safe_mode),
                );
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("settings.predictions.title"),
            &cx.tr.t("settings.predictions.desc"),
            false,
            |ui| {
                ui.add(
                    Toggle::new(
                        &mut settings.app_predictions,
                        cx.tr.t("settings.predictions.title"),
                    )
                    .enabled(!settings.safe_mode),
                );
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
    app_location_section(ui, cx, settings, app_location);
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
