//! Codex dashboard: route, lines, provider switcher and usage.

use egui::{Align, Layout, Rect, RichText, ScrollArea, Sense, Ui, vec2};
use superba_core::RestoreTarget;

use super::{Action, Cx, Page, State, cards, hero, lines, today};
use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Button, ListRow, Text};

/// Draw the four-card Codex dashboard for simple mode.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.simple_usage.tick_for(ui, cx, true);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), super::TITLE_ROW),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_height(super::TITLE_ROW);
            ui.label(
                RichText::new(cx.tr.t(Page::Overview.title_key()))
                    .font(font(text::TITLE_XL, Weight::Semibold))
                    .color(cx.pal.text)
                    .extra_letter_spacing(-0.8),
            );
        },
    );
    ui.add_space(8.0);
    let Some(overview) = cx.data.overview.value.as_ref() else {
        if let Some(error) = &cx.data.overview.error {
            crate::pages::error_state(ui, cx, &cx.tr.t("overview.load_failed"), error);
        } else {
            ui.spinner();
            ui.label(Text::muted(cx.tr.t("overview.loading"), cx.pal));
        }
        return;
    };

    let snap = state.simple_usage.value.as_ref();
    let route = hero::model(cx, overview, &[], snap);
    let area = ui.available_rect_before_wrap();
    let grid = cards::layout(area);
    let usage = grid.today.union(grid.sessions);
    let used = usage.max.y.max(grid.todo.max.y) - area.min.y;
    ui.allocate_rect(
        Rect::from_min_size(area.min, vec2(area.width(), used)),
        Sense::hover(),
    );
    hero::show_simple(ui, cx, &route, grid.hero, out);
    cards::card(ui, grid.lines, |ui| lines::show_simple(ui, cx, snap, out));
    cards::card(ui, grid.todo, |ui| quick_switch(ui, cx, out));
    cards::card(ui, usage, |ui| {
        if snap.is_none()
            && let Some(error) = &state.simple_usage.error
        {
            crate::pages::error_state(ui, cx, &cx.tr.t("overview.load_failed"), error);
        } else {
            today::show_codex(ui, cx, snap, out);
        }
    });
}

fn quick_switch(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    cards::header(ui, cx, &cx.tr.t("overview.simple.switch"), None, |ui| {
        if ui
            .add(Button::ghost(cx.tr.t("overview.simple.manage")).small())
            .clicked()
        {
            out.push(Action::Go(Page::Providers));
        }
    });
    ui.add_space(4.0);
    ScrollArea::vertical()
        .id_salt("simple-provider-switch")
        .auto_shrink([false, false])
        .max_height(ui.available_height())
        .show(ui, |ui| {
            provider_row(
                ui,
                cx,
                &cx.tr.t("overview.official_chip"),
                cx.data.official_active(),
                None,
                out,
            );
            for profile in cx.data.providers() {
                let name = cx.data.site(&profile.site_id).as_ref().map_or_else(
                    || profile.name.clone(),
                    |site| crate::app::provider_name(cx.tr, profile, site),
                );
                let selected = !cx.data.official_active()
                    && cx
                        .data
                        .active_profile()
                        .is_some_and(|active| active.id == profile.id);
                provider_row(ui, cx, &name, selected, Some(profile), out);
            }
            if cx.data.providers().is_empty()
                && ui
                    .add(
                        Button::new(cx.tr.t("overview.add_provider"))
                            .min_width(ui.available_width()),
                    )
                    .clicked()
            {
                out.push(Action::Go(Page::Providers));
            }
        });
}

fn provider_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    name: &str,
    selected: bool,
    profile: Option<&superba_core::Profile>,
    out: &mut Vec<Action>,
) {
    let needs_key = profile.is_some_and(|p| p.auth_mode.uses_key() && p.key_ref.is_none());
    let status = if selected {
        cx.tr.t("overview.lines.in_use")
    } else if needs_key {
        cx.tr.t("overview.simple.set_key")
    } else {
        String::new()
    };
    let icon = match profile.and_then(|p| cx.data.site(&p.site_id)) {
        None if profile.is_none() => Icon::Key,
        Some(site) if site.kind == superba_core::SiteKind::Krill => Icon::Superba,
        _ => Icon::Cloud,
    };
    let response = ui.add_enabled(
        !cx.busy,
        ListRow::new(icon, name)
            .selected(selected)
            .selected_tint(cx.pal.link)
            .value(status)
            .height(40.0),
    );
    if response.clicked() && !selected {
        match profile {
            Some(p) if needs_key => out.push(Action::PageCommand {
                page: Page::Providers,
                command: format!("key:{}", p.id),
            }),
            Some(p) => out.push(Action::Switch {
                profile: p.id.clone(),
                line: None,
                auth: None,
            }),
            None => out.push(Action::Restore(RestoreTarget::Official)),
        }
    }
}
