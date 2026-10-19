//! Network and download preferences.
use super::{Action, Cx, row, section};
use crate::design::widgets::{Card, Segmented, Text, Toggle};
use egui::Ui;
use superba_core::{DownloadSource, FeedSourceChoice, Settings, WindowsAppSource};
/// Windows: where the ChatGPT app installer package comes from (shown on
/// Windows only; public for tests).
pub fn windows_app_source_row(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings) {
    row(
        ui,
        cx,
        &cx.tr.t("prefs.network.app_source"),
        &cx.tr.t("prefs.network.app_source_desc"),
        false,
        |ui| {
            Segmented::new("app-source", &mut settings.network.windows_app_source)
                .option(
                    WindowsAppSource::OfficialFirst,
                    cx.tr.t("prefs.network.app_source_official_first"),
                )
                .option(
                    WindowsAppSource::OfficialOnly,
                    cx.tr.t("prefs.network.app_source_official_only"),
                )
                .option(
                    WindowsAppSource::StoreMirrorOnly,
                    cx.tr.t("prefs.network.app_source_store_only"),
                )
                .small()
                .show(ui);
        },
    );
}

/// Draw this section and save changes immediately.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, out: &mut Vec<Action>) {
    let before = settings.clone();
    section(ui, cx, &cx.tr.t("prefs.network.title"), None);
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("prefs.network.online"),
            &cx.tr.t("prefs.network.online_desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.network.online_checks,
                    cx.tr.t("prefs.network.online"),
                ));
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.network.source"),
            &cx.tr.t("prefs.network.source_desc"),
            false,
            |ui| {
                Segmented::new("download-source", &mut settings.network.download_source)
                    .option(DownloadSource::Auto, cx.tr.t("prefs.network.source_auto"))
                    .option(DownloadSource::GitHub, "GitHub")
                    .option(DownloadSource::Npm, "npm")
                    .option(DownloadSource::NpmMirror, "npmmirror")
                    .small()
                    .show(ui);
            },
        );
        if cfg!(windows) {
            windows_app_source_row(ui, cx, settings);
        }
        row(
            ui,
            cx,
            &cx.tr.t("prefs.network.models_source"),
            &cx.tr.t("prefs.network.models_source_desc"),
            false,
            |ui| {
                Segmented::new("feed-source", &mut settings.network.feed_source)
                    .option(FeedSourceChoice::Auto, cx.tr.t("prefs.network.auto"))
                    .option(FeedSourceChoice::GitHubRaw, "GitHub raw")
                    .option(FeedSourceChoice::JsDelivr, "jsDelivr")
                    .small()
                    .show(ui);
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.network.proxies"),
            &cx.tr.t("prefs.network.proxies_desc"),
            true,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.network.third_party_proxies,
                    cx.tr.t("prefs.network.proxies"),
                ));
            },
        );
    });
    ui.add_space(6.0);
    ui.label(Text::small(cx.tr.t("prefs.network.note"), cx.pal));

    if *settings != before {
        out.push(Action::SaveSettings);
    }
}
