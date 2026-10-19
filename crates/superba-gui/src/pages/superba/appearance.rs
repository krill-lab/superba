//! Interface appearance, applied immediately.
use super::{Action, Cx, row, section};
use crate::design::backdrop::{self, Backdrop};
use crate::design::icons::{self, Icon};
use crate::design::widgets::{Card, Segmented, Text, Toggle};
use egui::{Sense, Stroke, Ui, vec2};
use superba_core::{Background, Locale, Settings, Theme};
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

/// Draw this section and save changes immediately.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, out: &mut Vec<Action>) {
    let before = settings.clone();
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
            false,
            |ui| background_swatches(ui, cx, &mut settings.background),
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.motion.title"),
            &cx.tr.t("prefs.motion.desc"),
            true,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.reduce_motion,
                    cx.tr.t("prefs.motion.title"),
                ));
            },
        );
    });

    if *settings != before {
        out.push(Action::SaveSettings);
    }
}
