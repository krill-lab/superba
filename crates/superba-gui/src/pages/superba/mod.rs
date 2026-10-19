//! Superba: shared brand header, five tabs and anchored, scrolling preferences.
pub mod appearance;
pub mod backups;
pub mod data;
pub mod diagnostics;
pub mod general;
mod header;
pub mod home;
mod maintenance;
pub mod network;
mod preferences;
pub mod search;
pub mod update_settings;
pub mod updates;

use super::{Action, Cx, Page, page_header, section};
use crate::design::icons::Icon;
use crate::design::widgets::{UnderlineTab, UnderlineTabs};
use egui::{Key, Modifiers, Ui};
pub(crate) use search::row;
use superba_core::Settings;

/// Independently addressable sections; settings sections share one tab.
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
    /// Deep-link, palette and tray order.
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
    /// Stable command-line identity.
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
    /// Section icon.
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

/// The five visible tabs. Deep links continue to use [`SuperbaSection`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuperbaTab {
    /// Local facts, attention, recent activity and about.
    Home,
    /// Unified update inventory.
    Updates,
    /// General, appearance, network and notifications on one page.
    Settings,
    /// Diagnostics and logs.
    Diagnostics,
    /// Data and backups.
    Data,
}
impl SuperbaTab {
    /// Visual and keyboard order.
    pub const ALL: [Self; 5] = [
        Self::Home,
        Self::Updates,
        Self::Settings,
        Self::Diagnostics,
        Self::Data,
    ];
    /// Localized visible and accessible tab name.
    pub fn title_key(self) -> &'static str {
        match self {
            Self::Home => "superba.tab.home",
            Self::Updates => "superba.section.updates",
            Self::Settings => "superba.tab.settings",
            Self::Diagnostics => "superba.section.diagnostics",
            Self::Data => "superba.tab.data",
        }
    }
    /// Default destination when selecting the tab.
    pub fn section(self) -> SuperbaSection {
        match self {
            Self::Home => SuperbaSection::Home,
            Self::Updates => SuperbaSection::Updates,
            Self::Settings => SuperbaSection::General,
            Self::Diagnostics => SuperbaSection::Diagnostics,
            Self::Data => SuperbaSection::Data,
        }
    }
}
impl SuperbaSection {
    /// The tab containing this deep link.
    pub fn tab(self) -> SuperbaTab {
        match self {
            Self::Home => SuperbaTab::Home,
            Self::Updates => SuperbaTab::Updates,
            Self::Diagnostics => SuperbaTab::Diagnostics,
            Self::Data => SuperbaTab::Data,
            Self::General | Self::Appearance | Self::Network | Self::Notify => SuperbaTab::Settings,
        }
    }
}

/// State survives leaving the page, including the last visible settings section.
#[derive(Debug, Default)]
pub struct State {
    /// Where the main sidebar returns during this run.
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
    rendered: Option<SuperbaSection>,
    preferences: preferences::State,
}
impl State {
    /// Explicit navigation also repositions an anchor in the already open tab.
    pub fn enter(&mut self, section: SuperbaSection) {
        self.last = section;
        if section.tab() == SuperbaTab::Settings {
            self.preferences.jump = Some(section);
        }
    }
}

/// Brand header and tabs stay visible while the selected panel scrolls.
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
    if state.rendered != Some(selected) {
        match selected {
            SuperbaSection::Diagnostics => state.diagnostics.invalidate(),
            SuperbaSection::Data => state.backups.invalidate(),
            _ => {}
        }
        state.enter(selected);
        state.rendered = Some(selected);
    }
    let before = settings.clone();
    header::show(ui, cx, settings, out);
    let tab = selected.tab();
    let mut index = SuperbaTab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
    let labels: Vec<_> = SuperbaTab::ALL
        .iter()
        .map(|t| cx.tr.t(t.title_key()))
        .collect();
    let count = cx.data.update_count();
    let updates_description = cx
        .tr
        .f("superba.updates_count", &[("n", &count.to_string())]);
    let warning = cx.tr.t("superba.diagnostics_attention");
    let attention = cx.data.doctor.value.as_ref().is_some_and(|rows| {
        rows.iter()
            .any(|r| r.severity != superba_core::Severity::Info)
    });
    let items: Vec<_> = SuperbaTab::ALL
        .iter()
        .zip(&labels)
        .map(|(tab, label)| UnderlineTab {
            id: tab.section().id(),
            label,
            count: (*tab == SuperbaTab::Updates && count > 0).then_some(count),
            attention: *tab == SuperbaTab::Diagnostics && attention,
            description: if *tab == SuperbaTab::Updates && count > 0 {
                Some(updates_description.as_str())
            } else if *tab == SuperbaTab::Diagnostics && attention {
                Some(warning.as_str())
            } else {
                None
            },
        })
        .collect();
    let tab_response = UnderlineTabs {
        items: &items,
        label: &cx.tr.t("superba.title"),
    }
    .show(ui, &mut index);
    ui.add_space(8.0);
    if tab == SuperbaTab::Settings {
        preferences::toolbar(ui, cx, &mut state.preferences, out);
    }
    let focus_ready = tab != SuperbaTab::Updates
        || (!updates.view.loading()
            && (updates.view.value.is_some() || updates.view.error.is_some()));
    if focus_ready && let Some(key) = &state.focus {
        search::request(ui.ctx(), key);
    }
    let mut entry = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("superba-panel", tab))
            .accessibility_role(egui::Role::TabPanel)
            .accessibility_label(cx.tr.t(tab.title_key())),
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("superba-content", tab))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let response = ui.interact(
                        egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(ui.available_width(), 1.0),
                        ),
                        ui.make_persistent_id("panel-entry"),
                        egui::Sense::focusable_noninteractive(),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Group, true, cx.tr.t(tab.title_key()))
                    });
                    entry = Some(response.id);
                    match tab {
                        SuperbaTab::Home => {
                            home::show(ui, cx, &mut state.home, settings, notify, out)
                        }
                        SuperbaTab::Updates => {
                            ui.scope(|ui| {
                                // The existing CLI detail uses fixed columns above
                                // 800 px, including a copy-command button. Keep its
                                // compact layout until those columns have room;
                                // section internals are shared with the update work.
                                if ui.available_width() < 1040.0 {
                                    ui.set_max_width(ui.available_width().min(820.0));
                                }
                                updates::show(ui, cx, updates, out);
                            });
                            ui.add_space(24.0);
                            update_settings::show(ui, cx, settings, out);
                        }
                        SuperbaTab::Settings => {
                            preferences::show(ui, cx, &mut state.preferences, settings, notify, out)
                        }
                        SuperbaTab::Diagnostics => {
                            egui::ScrollArea::horizontal()
                                .id_salt("diagnostics-content")
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    diagnostics::show(ui, cx, &mut state.diagnostics, out)
                                });
                        }
                        SuperbaTab::Data => {
                            page_header(ui, cx, Page::Superba(SuperbaSection::Data), |_| {});
                            data::show(ui, cx, settings, &mut state.backups, &mut state.storage, out);
                        }
                    }
                    ui.add_space(40.0);
                });
        },
    );
    if tab == SuperbaTab::Settings {
        state.last = state.preferences.active;
    }
    if focus_ready && !search::pending(ui.ctx()) {
        state.focus = None;
    }
    search::clear(ui.ctx());
    if let Some(response) = tab_response {
        if response.has_focus()
            && ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::ArrowDown)
                    || i.consume_key(Modifiers::NONE, Key::Enter)
            })
        {
            if let Some(id) = entry {
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
            response.request_focus();
        }
    }
    let next = SuperbaTab::ALL[index];
    if next != tab {
        out.push(Action::Go(Page::Superba(if next == SuperbaTab::Settings {
            state.preferences.active
        } else {
            next.section()
        })));
    }
    if *settings != before && !out.iter().any(|a| matches!(a, Action::SaveSettings)) {
        out.push(Action::SaveSettings);
    }
}
