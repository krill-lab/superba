//! Two simple tabs: application information and the shared settings panel.
use super::{Action, Cx, Page, SuperbaSection, SuperbaTab, header, home, preferences, updates};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Card, StatusDot, Text, UnderlineTab, UnderlineTabs};
use crate::pages::overview::todo::{self, TodoKind};
use crate::pages::router::db::Loader;
use egui::Ui;
use superba_core::Settings;

#[derive(Debug, Default)]
pub(super) struct State {
    storage: Loader<superba_core::data::DataUsage>,
}

/// All information stays in one tab; settings reuse the complete-mode controls.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    selected: SuperbaSection,
    state: &mut super::State,
    settings: &mut Settings,
    notify: &mut crate::pages::notify::State,
    updates: &mut updates::State,
    out: &mut Vec<Action>,
) {
    if state.rendered != Some(selected) {
        state.enter(selected);
        state.rendered = Some(selected);
    }
    let before = settings.clone();
    header::show(ui, cx, settings);
    let settings_tab = selected.tab() == SuperbaTab::Settings;
    let mut index = usize::from(settings_tab);
    let info = cx.tr.t("simple.superba.info");
    let settings_label = cx.tr.t("superba.tab.settings");
    UnderlineTabs {
        items: &[
            UnderlineTab {
                id: "simple-info",
                label: &info,
                count: None,
                attention: false,
                description: None,
            },
            UnderlineTab {
                id: "simple-settings",
                label: &settings_label,
                count: None,
                attention: false,
                description: None,
            },
        ],
        label: "Superba",
    }
    .show(ui, &mut index);
    ui.add_space(8.0);
    if settings_tab {
        preferences::toolbar(ui, cx, &mut state.preferences);
        if let Some(key) = &state.focus {
            super::search::request(ui.ctx(), key);
        }
    }
    egui::ScrollArea::vertical()
        .id_salt(("simple-superba-tab", settings_tab))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if settings_tab {
                preferences::show(ui, cx, &mut state.preferences, settings, notify, out);
            } else {
                attention(ui, cx, out);
                ui.add_space(20.0);
                updates::rows::show_simple(ui, cx, updates, out);
                ui.add_space(20.0);
                storage(ui, cx, &mut state.simple);
                ui.add_space(20.0);
                home::about(ui, cx, out);
            }
            ui.add_space(24.0);
        });
    if settings_tab {
        state.last = state.preferences.active;
        if !super::search::pending(ui.ctx()) {
            state.focus = None;
        }
        super::search::clear(ui.ctx());
    }
    if index != usize::from(settings_tab) {
        out.push(Action::Go(Page::Superba(if index == 1 {
            state.preferences.active
        } else {
            SuperbaSection::Home
        })));
    }
    if *settings != before && !out.iter().any(|a| matches!(a, Action::SaveSettings)) {
        out.push(Action::SaveSettings);
    }
}

fn attention(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    let checking = cx.data.overview.loading
        || cx.data.doctor.loading
        || cx.data.versions.loading
        || cx.data.selfupdate.loading;
    crate::pages::section_with(ui, cx, &cx.tr.t("superba.tab.home"), None, |ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("diag.run"))
                    .icon(Icon::Refresh)
                    .small()
                    .enabled(!cx.busy && !checking)
                    .loading(checking),
            )
            .clicked()
        {
            out.extend([
                Action::Refresh,
                Action::RunDoctor,
                Action::CheckUpdates,
                Action::CheckSelfUpdate,
            ]);
        }
    });
    let todos: Vec<_> = todo::todos(cx, None)
        .into_iter()
        .filter(|t| !matches!(t.kind, TodoKind::ToolUpdate(_)))
        .collect();
    Card::new().padding(12).show(ui, |ui| {
        if todos.is_empty() {
            ui.label(Text::body(cx.tr.t("superba.home.all_good"), cx.pal));
        }
        for todo in todos {
            ui.horizontal_wrapped(|ui| {
                ui.add(StatusDot::new(todo.status));
                ui.label(Text::body(&todo.title, cx.pal));
            });
            if let Some(detail) = todo.detail {
                ui.add(egui::Label::new(Text::small(detail, cx.pal)).wrap());
            }
        }
    });
}

fn storage(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    state.storage.poll();
    if state.storage.due(None) {
        let service = cx.service.clone();
        state
            .storage
            .start(async move { service.data_usage().await });
    }
    crate::pages::section_with(ui, cx, &cx.tr.t("prefs.data.storage"), None, |ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("action.refresh"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(state.storage.loading()),
            )
            .clicked()
        {
            state.storage.invalidate();
        }
    });
    Card::new().padding(12).show(ui, |ui| {
        if let Some(usage) = &state.storage.value {
            let bytes = crate::pages::data::bytes;
            egui::Grid::new("simple-storage-use")
                .num_columns(2)
                .spacing([20.0, 6.0])
                .show(ui, |ui| {
                    for category in &usage.categories {
                        ui.label(Text::body(
                            cx.tr.t(&format!("data.category.{}", category.category)),
                            cx.pal,
                        ));
                        ui.label(Text::body(bytes(category.bytes), cx.pal));
                        ui.end_row();
                    }
                    ui.label(Text::strong(cx.tr.t("prefs.data.total"), cx.pal));
                    ui.label(Text::strong(bytes(usage.total_bytes), cx.pal));
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(Text::small(
                    cx.tr.f(
                        "prefs.data.codex",
                        &[
                            (
                                "path",
                                &superba_core::paths::display_path(&usage.location.codex_home),
                            ),
                            ("total", &bytes(usage.codex_home_bytes)),
                            ("sessions", &bytes(usage.codex_sessions_bytes)),
                        ],
                    ),
                    cx.pal,
                ))
                .wrap(),
            );
            if usage.skipped > 0 {
                ui.label(Text::small(cx.tr.t("prefs.data.partial"), cx.pal));
            }
        } else if state.storage.loading() {
            ui.spinner();
        }
        if let Some(error) = &state.storage.error {
            ui.add(
                egui::Label::new(Text::small(crate::pages::error_text(cx.tr, error), cx.pal))
                    .wrap(),
            );
        }
    });
    if state.storage.loading() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}
