//! Searchable preference rows and their focus/scroll anchors.
use super::{Cx, SuperbaSection};
use crate::design::widgets::{FormRow, Text};
use egui::{Id, Rect, Sense, Ui};

/// Every preference row the inline filter and `focus:` deep links know.
pub const ROWS: &[(SuperbaSection, &str)] = &[
    (SuperbaSection::General, "settings.countdown.title"),
    (SuperbaSection::General, "settings.safe_mode.title"),
    (SuperbaSection::General, "settings.open_on_start.title"),
    (SuperbaSection::General, "settings.auto_probe.title"),
    (SuperbaSection::General, "tray.prefs.close.title"),
    (SuperbaSection::General, "tray.prefs.start.title"),
    (SuperbaSection::General, "prefs.wizard.run"),
    (SuperbaSection::Appearance, "settings.language.title"),
    (SuperbaSection::Appearance, "settings.theme.title"),
    (SuperbaSection::Appearance, "settings.background.title"),
    (SuperbaSection::Appearance, "prefs.motion.title"),
    (SuperbaSection::Network, "prefs.network.online"),
    (SuperbaSection::Network, "prefs.network.source"),
    (SuperbaSection::Network, "prefs.network.models_source"),
    (SuperbaSection::Network, "prefs.network.proxies"),
    (SuperbaSection::Network, "prefs.network.app_source"),
    (SuperbaSection::Updates, "prefs.updates.auto"),
    (SuperbaSection::Updates, "prefs.updates.channel"),
    (SuperbaSection::Data, "prefs.data.dir"),
    (SuperbaSection::Data, "prefs.data.requests"),
    (SuperbaSection::Data, "superba.backups.title"),
    (SuperbaSection::Data, "prefs.restore.original"),
    (SuperbaSection::Data, "prefs.restore.official"),
    (SuperbaSection::Data, "settings.uninstall.title"),
    (SuperbaSection::Notify, "prefs.notify.enabled.title"),
    (SuperbaSection::Notify, "prefs.notify.channels.title"),
    (SuperbaSection::Notify, "prefs.notify.events.turn_complete"),
    (SuperbaSection::Notify, "prefs.notify.events.needs_approval"),
    (SuperbaSection::Notify, "prefs.notify.events.needs_input"),
    (SuperbaSection::Notify, "prefs.notify.events.turn_failed"),
    (SuperbaSection::Notify, "prefs.notify.events.switch_done"),
    (SuperbaSection::Notify, "prefs.notify.events.switch_failed"),
    (
        SuperbaSection::Notify,
        "prefs.notify.events.router_member_down",
    ),
    (
        SuperbaSection::Notify,
        "prefs.notify.events.update_available",
    ),
    (SuperbaSection::Notify, "prefs.notify.events.min_title"),
    (SuperbaSection::Notify, "prefs.notify.events.harness_title"),
    (SuperbaSection::Notify, "prefs.notify.content.project"),
    (SuperbaSection::Notify, "prefs.notify.content.usage"),
    (SuperbaSection::Notify, "prefs.notify.content.host"),
    (SuperbaSection::Notify, "prefs.notify.content.summary"),
    (SuperbaSection::Notify, "prefs.notify.quiet.title"),
    (SuperbaSection::Notify, "prefs.notify.quiet.urgent"),
    (SuperbaSection::Notify, "prefs.notify.rate.title"),
    (SuperbaSection::Notify, "prefs.notify.hooks.title"),
    (SuperbaSection::Notify, "prefs.notify.hooks.codex_notify"),
    (SuperbaSection::Notify, "prefs.notify.hooks.codex_approvals"),
    (SuperbaSection::Notify, "prefs.notify.hooks.errors_title"),
    (SuperbaSection::Notify, "prefs.notify.hooks.claude"),
];

/// Platform-specific window terminology keeps the same row identity.
pub fn label_key(key: &str) -> &str {
    match key {
        "tray.prefs.start.title" if cfg!(target_os = "macos") => "tray.prefs.start.title_mac",
        _ => key,
    }
}

fn filter_key() -> Id {
    Id::unique("superba-settings-filter-query")
}
fn section_all_key() -> Id {
    Id::unique("superba-settings-filter-section")
}
/// The inline filter's query while the settings sections are drawn ("" = none).
pub(super) fn set_filter(ctx: &egui::Context, query: &str) {
    ctx.data_mut(|d| d.insert_temp(filter_key(), query.to_owned()));
}
/// The section being drawn matched as a whole (every row is shown).
pub(super) fn set_section_all(ctx: &egui::Context, all: bool) {
    ctx.data_mut(|d| d.insert_temp(section_all_key(), all));
}
/// The inline settings filter has a query (folded cards open so their rows
/// can match).
pub(crate) fn filtering(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<String>(filter_key()))
        .is_some_and(|q| !q.is_empty())
}

/// Whether a row passes the inline filter.
pub(crate) fn visible(ctx: &egui::Context, fields: &[&str]) -> bool {
    let query: String = ctx.data(|d| d.get_temp(filter_key())).unwrap_or_default();
    query.is_empty()
        || ctx.data(|d| d.get_temp::<bool>(section_all_key()).unwrap_or(false))
        || super::preferences::filter_matches(&query, fields)
}

fn focus_key() -> Id {
    Id::unique("superba-setting-focus")
}
pub(super) fn request(ctx: &egui::Context, key: &str) {
    ctx.data_mut(|d| d.insert_temp(focus_key(), key.to_owned()));
}
pub(crate) fn pending(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<String>(focus_key()).is_some())
}
pub(super) fn clear(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<String>(focus_key()));
}

/// A named, keyboard-focusable row anchor. Tab continues to its controls.
pub(crate) fn anchor(ui: &mut Ui, key: &str, name: &str, rect: Rect) {
    let id = ui.make_persistent_id(("setting-row", key));
    if !ui.is_enabled() {
        // A disabled control remains read-only; its containing row can still
        // receive deep-link focus. Register the row from the enabled parent.
        ui.ctx().data_mut(|d| {
            d.get_temp_mut_or_default::<Vec<DeferredAnchor>>(Id::unique("superba-disabled-rows"))
                .push(DeferredAnchor {
                    id,
                    key: key.into(),
                    name: name.into(),
                    rect,
                })
        });
        return;
    }
    anchor_at(ui, id, key, name, rect);
}

#[derive(Clone)]
struct DeferredAnchor {
    id: Id,
    key: String,
    name: String,
    rect: Rect,
}

pub(crate) fn flush_disabled(ui: &mut Ui) {
    let rows = ui
        .ctx()
        .data_mut(|d| d.remove_temp::<Vec<DeferredAnchor>>(Id::unique("superba-disabled-rows")))
        .unwrap_or_default();
    for r in rows {
        anchor_at(ui, r.id, &r.key, &r.name, r.rect);
    }
}

fn anchor_at(ui: &mut Ui, id: Id, key: &str, name: &str, rect: Rect) {
    let response = ui.interact(rect, id, Sense::focusable_noninteractive());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, ui.is_enabled(), name));
    let wanted = ui.ctx().data(|d| d.get_temp::<String>(focus_key()));
    if wanted.as_deref() == Some(key) {
        response.scroll_to_me(Some(egui::Align::Center));
        response.request_focus();
        // FormRow and async sections can discard their first layout pass.
        // Keep the request until the anchor actually enters the viewport.
        if ui.clip_rect().contains(rect.center()) {
            clear(ui.ctx());
        } else {
            ui.ctx().request_repaint();
        }
    }
    if crate::design::widgets::show_focus(&response) {
        crate::design::widgets::focus_ring(
            ui.painter(),
            rect,
            4,
            crate::design::Palette::of(ui.ctx()),
        );
    }
}

/// Label and explanation with a control; wide controls stack at narrow widths.
pub(crate) fn row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    desc: &str,
    last: bool,
    control: impl FnOnce(&mut Ui),
) {
    let key = ROWS
        .iter()
        .find(|(_, key)| cx.tr.t(label_key(key)) == title)
        .map(|(_, key)| *key);
    if !visible(ui.ctx(), &[title, desc, key.unwrap_or("")]) {
        return;
    }
    // The installer-source explanation is a paragraph, and its three-way
    // selector exceeds the ordinary control column. Keep both on their own row.
    let compact = ui.available_width() < 620.0
        || key == Some("prefs.network.app_source")
        || (key == Some("prefs.network.source") && ui.available_width() < 860.0);
    let toggle = key.is_some_and(|key| {
        matches!(
            key,
            "settings.safe_mode.title"
                | "settings.open_on_start.title"
                | "settings.auto_probe.title"
                | "tray.prefs.start.title"
                | "prefs.motion.title"
                | "prefs.network.online"
                | "prefs.network.proxies"
                | "prefs.updates.auto"
                | "prefs.notify.enabled.title"
                | "prefs.notify.content.summary"
                | "prefs.notify.quiet.urgent"
        )
    });
    let rect = if compact && !toggle {
        ui.vertical(|ui| {
            ui.add_space(6.0);
            let label = ui.label(Text::strong(title, cx.pal));
            ui.add(egui::Label::new(Text::small(desc, cx.pal)).wrap());
            crate::design::a11y::label_unnamed_by(ui, label.id, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
                });
            });
            ui.add_space(6.0);
        })
        .response
        .rect
    } else {
        let text_w = (ui.available_width() - if compact { 64.0 } else { 300.0 }).max(140.0);
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
                        ui.add(egui::Label::new(Text::strong(title, cx.pal)).wrap());
                        ui.add(egui::Label::new(Text::small(desc, cx.pal)).wrap());
                    });
                },
                control,
            )
            .rect
    };
    if let Some(key) = key {
        anchor(ui, key, title, rect);
    }
    if !last {
        let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            r.x_range(),
            r.center().y,
            egui::Stroke::new(1.0, cx.pal.border),
        );
    }
}
