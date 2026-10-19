//! Superba's maintenance and preferences: a fixed rail and in-place detail.
pub mod appearance;
pub mod backups;
pub mod data;
pub mod diagnostics;
pub mod general;
pub mod home;
mod maintenance;
pub mod network;
pub mod search;
pub mod update_settings;
pub mod updates;

use super::{Action, Cx, Page, page_header, section};
use crate::design::widgets::{SubNav, SubNavItem};
use crate::design::{icons::Icon, layout, tokens::size};
use egui::{Key, Modifiers, Ui};
pub(crate) use search::row;
use superba_core::Settings;

/// Independently addressable sections; the first is the identity/home row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SuperbaSection {
    /// Version, attention, local facts, recent activity and about.
    #[default]
    Home,
    /// All installation and updates.
    Updates,
    /// Checks and activity log.
    Diagnostics,
    /// Data, backups and restore.
    Data,
    /// Switching and window behavior.
    General,
    /// Language, theme and background.
    Appearance,
    /// Online checks and download sources.
    Network,
    /// Channels, events and hooks.
    Notify,
}
impl SuperbaSection {
    /// Rail and keyboard order.
    pub const ALL: [Self; 8] = [
        Self::Home,
        Self::Updates,
        Self::Diagnostics,
        Self::Data,
        Self::General,
        Self::Appearance,
        Self::Network,
        Self::Notify,
    ];
    /// Stable command-line/rail identity.
    pub fn id(self) -> &'static str {
        match self {
            Self::Home => "superba",
            Self::Updates => "updates",
            Self::Diagnostics => "diagnostics",
            Self::Data => "data",
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Network => "network",
            Self::Notify => "notify",
        }
    }
    /// Localized section name.
    pub fn title_key(self) -> &'static str {
        match self {
            Self::Home => "superba.title",
            Self::Updates => "superba.section.updates",
            Self::Diagnostics => "superba.section.diagnostics",
            Self::Data => "superba.section.data",
            Self::General => "superba.section.general",
            Self::Appearance => "superba.section.appearance",
            Self::Network => "superba.section.network",
            Self::Notify => "superba.section.notify",
        }
    }
    /// Localized header explanation.
    pub fn subtitle_key(self) -> &'static str {
        match self {
            Self::Home => "superba.home.subtitle",
            Self::Updates => "updates.list.subtitle",
            Self::Diagnostics => "superba.diagnostics.subtitle",
            Self::Data => "superba.data.subtitle",
            Self::General => "superba.general.subtitle",
            Self::Appearance => "superba.appearance.subtitle",
            Self::Network => "superba.network.subtitle",
            Self::Notify => "superba.notify.subtitle",
        }
    }
    /// Rail icon.
    pub fn icon(self) -> Icon {
        match self {
            Self::Home => Icon::Superba,
            Self::Updates => Icon::Download,
            Self::Diagnostics => Icon::Probe,
            Self::Data => Icon::Archive,
            Self::General => Icon::Gear,
            Self::Appearance => Icon::Settings,
            Self::Network => Icon::Lines,
            Self::Notify => Icon::Bell,
        }
    }
}

/// State is kept when leaving the page, including the last visited section.
#[derive(Debug, Default)]
pub struct State {
    /// Where the sidebar returns during this run.
    pub last: SuperbaSection,
    /// Palette row waiting to be scrolled into view and focused.
    pub focus: Option<String>,
    /// Background reads for the home summary.
    pub home: home::State,
    /// Coding-tool checks and expansion state.
    pub diagnostics: diagnostics::State,
    /// All configuration backup stores.
    pub backups: backups::State,
    /// Data location, disk usage and retention controls.
    pub storage: super::data::State,
}

/// Fixed rail, independently scrolling detail, and immediate preference saves.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    selected: SuperbaSection,
    state: &mut State,
    settings: &mut Settings,
    notify: &mut super::notify::State,
    updates: &mut updates::State,
    out: &mut Vec<Action>,
) {
    if state.last != selected {
        match selected {
            SuperbaSection::Diagnostics => state.diagnostics.invalidate(),
            SuperbaSection::Data => state.backups.invalidate(),
            _ => {}
        }
    }
    state.last = selected;
    let before = settings.clone();
    let width = if ui.available_width() < 760.0 {
        size::RAIL_NARROW
    } else {
        size::RAIL
    };
    let labels: Vec<_> = SuperbaSection::ALL
        .iter()
        .map(|s| cx.tr.t(s.title_key()))
        .collect();
    let maintenance = cx.tr.t("superba.group.maintenance");
    let preferences = cx.tr.t("superba.group.preferences");
    let count = cx.data.update_count();
    let badge = count.to_string();
    let hint = format!(
        "{}\n{}",
        crate::shell::palette_shortcut_label(),
        cx.tr.t("superba.search")
    );
    let mut items: Vec<_> = SuperbaSection::ALL
        .iter()
        .zip(&labels)
        .map(|(s, label)| {
            let item = SubNavItem::new(s.id(), label, s.icon());
            match s {
                SuperbaSection::Home => item.identity(),
                SuperbaSection::Updates => {
                    let item = item.group(&maintenance);
                    if count > 0 { item.badge(&badge) } else { item }
                }
                SuperbaSection::General => item.group(&preferences),
                _ => item,
            }
        })
        .collect();
    let warning = cx.tr.t("superba.diagnostics_attention");
    if cx.data.doctor.value.as_ref().is_some_and(|rows| {
        rows.iter()
            .any(|r| r.severity != superba_core::Severity::Info)
    }) {
        items[2] = SubNavItem::new("diagnostics", &labels[2], Icon::Probe).dot(&warning);
    }
    let mut index = SuperbaSection::ALL
        .iter()
        .position(|s| *s == selected)
        .unwrap_or(0);
    // The legacy update body grows after its first async read. Focusing a row
    // below its loading placeholder would leave it outside the final viewport.
    let focus_ready = selected != SuperbaSection::Updates
        || (!updates.view.loading()
            && (updates.view.value.is_some() || updates.view.error.is_some()));
    if focus_ready && let Some(key) = &state.focus {
        search::request(ui.ctx(), key);
    }
    let mut detail_entry = None;
    let (rail, ()) = layout::rail_split_with_id(
        ui,
        width,
        ("superba-detail", selected),
        |ui| SubNav::new(&items).footer(&hint).show(ui, &mut index),
        |ui| {
            let entry = ui.interact(
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 1.0)),
                ui.make_persistent_id("detail-entry"),
                egui::Sense::focusable_noninteractive(),
            );
            entry.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Group, true, cx.tr.t(selected.title_key()))
            });
            detail_entry = Some(entry.id);
            // The detail's identity also keeps its scroll position per section.
            ui.push_id(selected, |ui| match selected {
                SuperbaSection::Home => home::show(ui, cx, &mut state.home, settings, notify, out),
                SuperbaSection::Updates => {
                    updates::show(ui, cx, updates, out);
                    ui.add_space(24.0);
                    update_settings::show(ui, cx, settings, out);
                }
                SuperbaSection::Diagnostics => {
                    egui::ScrollArea::horizontal()
                        .id_salt("diagnostics-content")
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            diagnostics::show(ui, cx, &mut state.diagnostics, out);
                        });
                }
                SuperbaSection::General => {
                    page_header(ui, cx, Page::Superba(selected), |_| {});
                    general::show(ui, cx, settings, out);
                }
                SuperbaSection::Appearance => {
                    page_header(ui, cx, Page::Superba(selected), |_| {});
                    appearance::show(ui, cx, settings, out);
                }
                SuperbaSection::Network => {
                    page_header(ui, cx, Page::Superba(selected), |_| {});
                    network::show(ui, cx, settings, out);
                }
                SuperbaSection::Data => {
                    page_header(ui, cx, Page::Superba(selected), |_| {});
                    data::show(
                        ui,
                        cx,
                        settings,
                        &mut state.backups,
                        &mut state.storage,
                        out,
                    );
                }
                SuperbaSection::Notify => {
                    page_header(ui, cx, Page::Superba(selected), |_| {});
                    super::notify::body_ui(ui, cx, settings, notify, out);
                }
            });
        },
    );
    if focus_ready && !search::pending(ui.ctx()) {
        state.focus = None;
    }
    search::clear(ui.ctx());
    if let Some(rail) = rail {
        if rail.has_focus()
            && ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::ArrowRight)
                    || i.consume_key(Modifiers::NONE, Key::Enter)
            })
        {
            if let Some(id) = detail_entry {
                ui.memory_mut(|m| {
                    m.move_focus(egui::FocusDirection::None);
                    m.request_focus(id);
                });
            }
        } else if !state.storage.busy()
            && !state.backups.modal()
            && !notify.modal()
            && !updates::status(updates).modal
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            rail.request_focus();
        }
    }
    let next = SuperbaSection::ALL[index];
    if next != selected {
        out.push(Action::Go(Page::Superba(next)));
    }
    if *settings != before && !out.iter().any(|a| matches!(a, Action::SaveSettings)) {
        out.push(Action::SaveSettings);
    }
}
