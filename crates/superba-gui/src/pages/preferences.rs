//! Preferences (UX §3.11): Superba's own settings (switching, appearance,
//! network), its data folder, the setup wizard, restore, uninstall and about.
//! Codex settings live on the catalog-driven Settings page.

use egui::{Sense, Stroke, Ui, vec2};
use superba_core::{
    Background, DownloadSource, FeedSourceChoice, Locale, ProviderRestore, RestoreTarget, Settings,
    Theme,
};

use super::{Action, Cx, Page, page_header, section};
use crate::design::backdrop::{self, Backdrop};
use crate::design::icons::{self, Icon};
use crate::design::widgets::{Button, Card, FormRow, Segmented, Text, Toggle, link_button};

/// The project's repository (About).
pub const PROJECT_URL: &str = "https://github.com/krill-lab/superba";

/// One settings row: title + description on the left, control on the right,
/// both centered on the row (at least 56 px high, taller when the description
/// wraps).
fn row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    desc: &str,
    last: bool,
    control: impl FnOnce(&mut Ui),
) {
    let text_w = (ui.available_width() - 300.0).max(240.0);
    FormRow::default()
        .label_width(text_w)
        .height(56.0)
        .right()
        .show_with(
            ui,
            |ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(text_w);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(Text::strong(title, cx.pal));
                    ui.label(Text::small(desc, cx.pal));
                });
            },
            control,
        );
    if !last {
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(
            r.x_range(),
            r.center().y,
            egui::Stroke::new(1.0, cx.pal.border),
        );
    }
}

/// Side of a background swatch.
const SWATCH: f32 = 30.0;

/// The background themes as round swatches (each painted with its own gradient
/// in the current light or dark mode), the selected one ringed and named. A click
/// applies the background to the window at once (live) and saves it.
pub fn background_swatches(ui: &mut Ui, cx: &Cx<'_>, value: &mut Background) {
    let dark = cx.pal.dark;
    // Right to left: the last theme first.
    for bg in Background::ALL.into_iter().rev() {
        let b = Backdrop::new(bg, dark);
        let (rect, response) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::click());
        let name = cx.tr.t(backdrop::name_key(bg));
        let selected = *value == bg;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &name)
        });
        let response = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(&name);
        let p = ui.painter();
        let inner = rect.shrink(3.0);
        // The theme's own window gradient, clipped to a circle by painting it in
        // a rounded square with full radius.
        let mut mesh = egui::Mesh::default();
        let n = 24;
        let c = inner.center();
        let r = inner.width() / 2.0;
        mesh.colored_vertex(c, b.top.lerp_to_gamma(b.bottom, 0.5));
        for i in 0..=n {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            let pt = c + vec2(a.cos(), a.sin()) * r;
            // Top-left light, bottom-right darker, like the window.
            let t = ((pt.x - inner.min.x) + (pt.y - inner.min.y)) / (inner.width() * 2.0);
            mesh.colored_vertex(pt, b.top.lerp_to_gamma(b.bottom, t));
            if i > 0 {
                mesh.add_triangle(0, i as u32, i as u32 + 1);
            }
        }
        p.add(mesh);
        // The chart accent of the theme as a small dot.
        p.circle_filled(c + vec2(r * 0.42, r * 0.42), 3.0, b.chart[0]);
        p.circle_stroke(c, r, Stroke::new(1.0, cx.pal.border_strong));
        if selected {
            p.circle_stroke(c, r + 3.0, Stroke::new(2.0, cx.pal.text));
            icons::paint(
                p,
                egui::Rect::from_center_size(c - vec2(r * 0.15, r * 0.15), vec2(12.0, 12.0)),
                Icon::Check,
                cx.pal.text,
            );
        }
        if crate::design::widgets::show_focus(&response) {
            crate::design::widgets::focus_ring(
                p,
                rect,
                crate::design::tokens::radius::PILL,
                cx.pal,
            );
        }
        if response.clicked() {
            *value = bg;
            backdrop::set(ui.ctx(), bg);
        }
    }
    ui.add_space(6.0);
    ui.label(Text::small(cx.tr.t(backdrop::name_key(*value)), cx.pal).color(cx.pal.text));
}

/// Draw the settings page; edits `settings` in place.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, out: &mut Vec<Action>) {
    page_header(ui, cx, Page::Preferences, |_| {});
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
            &cx.tr.t("settings.takeover.title"),
            &cx.tr.t("settings.takeover.desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.takeover_app,
                    cx.tr.t("settings.takeover.title"),
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
    section(ui, cx, &cx.tr.t("settings.appearance"), None);
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("settings.language.title"),
            &cx.tr.t("settings.language.desc"),
            false,
            |ui| {
                let mut locale = settings.locale;
                let r = Segmented::new("locale", &mut locale)
                    .option(None, cx.tr.t("settings.system"))
                    .option(Some(Locale::En), "English")
                    .option(Some(Locale::ZhCn), "简体中文")
                    .small()
                    .show(ui);
                if r.changed() {
                    settings.locale = locale;
                    out.push(Action::SetLocale(locale));
                }
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("settings.theme.title"),
            &cx.tr.t("settings.theme.desc"),
            false,
            |ui| {
                Segmented::new("theme", &mut settings.theme)
                    .option(Theme::System, cx.tr.t("settings.system"))
                    .option(Theme::Dark, cx.tr.t("settings.theme.dark"))
                    .option(Theme::Light, cx.tr.t("settings.theme.light"))
                    .small()
                    .show(ui);
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("settings.background.title"),
            &cx.tr.t("settings.background.desc"),
            true,
            |ui| background_swatches(ui, cx, &mut settings.background),
        );
    });

    ui.add_space(24.0);
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

    ui.add_space(24.0);
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
        row(
            ui,
            cx,
            &cx.tr.t("prefs.updates.prerelease"),
            &cx.tr.t("prefs.updates.prerelease_desc"),
            true,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.include_prereleases,
                    cx.tr.t("prefs.updates.prerelease"),
                ));
            },
        );
    });

    ui.add_space(24.0);
    section(ui, cx, &cx.tr.t("prefs.data.title"), None);
    Card::new().flat().show(ui, |ui| {
        let dir = cx
            .data
            .paths
            .as_ref()
            .map(|p| p.data_dir.display().to_string())
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

    ui.add_space(24.0);
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
                    out.push(Action::Restore(RestoreTarget::Original {
                        provider: ProviderRestore::KeepOpenai,
                    }));
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

    ui.add_space(24.0);
    section(ui, cx, &cx.tr.t("settings.about.title"), None);
    Card::new().flat().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(Text::strong("Superba", cx.pal));
            ui.label(Text::mono_muted(
                format!("v{}", env!("CARGO_PKG_VERSION")),
                cx.pal,
            ));
        });
        ui.add(egui::Label::new(Text::small(cx.tr.t("settings.about.desc"), cx.pal)).wrap());
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if link_button(ui, &cx.tr.t("prefs.about.project"), true).clicked() {
                out.push(Action::OpenUrl(PROJECT_URL.into()));
            }
            ui.add_space(12.0);
            ui.label(Text::small(cx.tr.t("settings.about.license"), cx.pal));
            ui.add_space(12.0);
            ui.label(Text::small(cx.tr.t("settings.about.fonts"), cx.pal));
        });
    });

    if *settings != before {
        out.push(Action::SaveSettings);
    }
}
