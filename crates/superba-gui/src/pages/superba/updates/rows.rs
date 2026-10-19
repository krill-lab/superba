//! Responsive inventory and expansion panels, with one named operation per row.
use super::{
    State,
    items::{Entry, Operation, UpdateItem, UpdateStatus},
    queue::Batch,
    tools::ToolsView,
};
use crate::design::icons::Icon;
use crate::design::widgets::{
    Button, ButtonKind, Segmented, Status, Text, UpdateGroup, UpdateRow, UpdateTile, dialog,
    link_button,
};
use crate::pages::{Action, Cx, Page, callout};
use egui::{Align, Layout, Ui};
use std::time::Duration;

/// Persistent filter; opening a deep link restores All so it cannot be hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    /// Every supported item.
    #[default]
    All,
    /// New versions and failed attempts.
    Available,
}
/// Page body (the enclosing Superba rail owns scrolling).
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.background(cx, out);
    // Leave space for the enclosing rail's floating vertical scrollbar.
    ui.set_max_width((ui.available_width() - 8.0).max(0.0));
    let tools = state.tools.view(cx);
    let entries = super::items::inventory(cx, state, &tools);
    let count = entries.iter().filter(|e| e.status.actionable()).count();
    if ui.available_width() >= 700.0 {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(cx.tr.t("superba.section.updates"))
                    .font(crate::design::fonts::font(
                        28.0,
                        crate::design::fonts::Weight::Semibold,
                    ))
                    .color(cx.pal.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                toolbar(ui, cx, state, &entries, &tools, count, out)
            });
        });
    } else {
        ui.label(
            egui::RichText::new(cx.tr.t("superba.section.updates"))
                .font(crate::design::fonts::font(
                    28.0,
                    crate::design::fonts::Weight::Semibold,
                ))
                .color(cx.pal.text),
        );
        ui.horizontal_wrapped(|ui| toolbar(ui, cx, state, &entries, &tools, count, out));
    }
    ui.add(
        egui::Label::new(Text::small(
            cx.tr
                .t(Page::Superba(crate::pages::SuperbaSection::Updates).subtitle_key()),
            cx.pal,
        ))
        .wrap(),
    );
    if let Some(checked) = cx.data.selfupdate.value.as_ref().and_then(|v| v.checked_at) {
        ui.label(Text::caption(
            cx.tr.f("updates.checked", &[("age", &cx.ago(checked))]),
            cx.pal,
        ));
    }
    ui.add_space(12.0);
    if state.queue.busy() {
        ui.horizontal_wrapped(|ui| {
            ui.label(Text::small(
                cx.tr.f(
                    "updates.list.queued",
                    &[("done", &state.queue.completed), ("n", &state.queue.total)],
                ),
                cx.pal,
            ));
            if ui
                .add(Button::ghost(cx.tr.t("updates.list.stop_queue")).small())
                .clicked()
            {
                state.cancel_queue();
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    if let Some(error) = &tools.error {
        callout(ui, cx, Status::Warn, error);
    }
    if let Some(error) = &state.view.error {
        callout(
            ui,
            cx,
            Status::Warn,
            &crate::pages::error_text(cx.tr, error),
        );
    }
    if let Some(v) = &state.view.value {
        for note in &v.notes {
            callout(
                ui,
                cx,
                if note.severity == superba_core::Severity::Info {
                    Status::Idle
                } else {
                    Status::Warn
                },
                &cx.localize(&note.text),
            );
        }
    }
    if state.focus.is_some() {
        state.filter = Filter::All;
    }
    let mut shown = 0;
    let spacing = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    for (group, key) in [
        "updates.list.superba_group",
        "updates.list.codex_group",
        "updates.list.tools_group",
    ]
    .into_iter()
    .enumerate()
    {
        let rows: Vec<_> = entries
            .iter()
            .filter(|e| {
                e.id.group() == group && (state.filter == Filter::All || e.status.actionable())
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        ui.add_space(6.0);
        UpdateGroup(&format!("{}  {}", cx.tr.t(key), rows.len())).show(ui, |ui| {
            if group == 2 && link_button(ui, &cx.tr.t("updates.list.tools_config"), false).clicked()
            {
                out.push(Action::Go(Page::Tools));
            }
        });
        for row in rows {
            show_row(ui, cx, state, row, &tools, out);
            shown += 1;
        }
    }
    ui.spacing_mut().item_spacing.y = spacing;
    if shown == 0 {
        ui.label(Text::muted(cx.tr.t("updates.list.no_updates"), cx.pal));
    }
    for note in &tools.notes {
        ui.add(egui::Label::new(Text::small(note, cx.pal)).wrap());
    }
    if tools.unavailable > 0 {
        ui.label(Text::caption(
            cx.tr
                .f("updates.list.unavailable", &[("n", &tools.unavailable)]),
            cx.pal,
        ));
    }
    confirmation(ui, cx, state, out);
}
fn show_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    entry: &Entry,
    tools: &ToolsView,
    out: &mut Vec<Action>,
) {
    let key = entry.id.key();
    let focusing = state.focus.as_ref() == Some(&entry.id);
    let mut expanded = focusing || state.expanded.contains(&entry.id);
    let versions = entry.versions();
    let status = cx.tr.t(entry.status.key());
    let label = entry.operation.as_ref().map(|op| {
        cx.tr.t(match op {
            Operation::ToolInstall(_) => "updates.list.install",
            Operation::Cancel => "updates.win.cancel",
            Operation::Change => "updates.list.change",
            Operation::Unskip => "updates.superba.unskip",
            Operation::Check => "updates.list.retry",
            Operation::DownloadPage(_) => "updates.tools.download_page",
            Operation::OpenInstaller(_) => "updates.app.open_installer",
            _ if entry.status == UpdateStatus::Failed => "updates.list.retry",
            _ if entry.status == UpdateStatus::Missing => "updates.list.install",
            _ => "updates.list.update",
        })
    });
    let tile = match entry.id {
        UpdateItem::Superba => UpdateTile::Icon(Icon::Superba),
        UpdateItem::App => UpdateTile::Icon(Icon::AppWindow),
        UpdateItem::AppPin | UpdateItem::CodexCli => UpdateTile::Icon(Icon::Terminal),
        UpdateItem::CodexRemote(_) => UpdateTile::Icon(Icon::Server),
        UpdateItem::Tool(id) => UpdateTile::Monogram(id.monogram()),
    };
    let description = entry
        .progress
        .as_ref()
        .map_or(entry.description.as_str(), |p| p.label.as_str());
    let mut row = UpdateRow::new(&key, &entry.name, &status)
        .tile(tile)
        .versions(&versions)
        .description(description)
        .status(entry.status.tone());
    if matches!(
        entry.status,
        UpdateStatus::Missing | UpdateStatus::Disconnected
    ) {
        row = row.hollow();
    }
    if let Some(label) = &label {
        let kind = match entry.operation {
            Some(Operation::Cancel) => ButtonKind::Ghost,
            Some(
                Operation::Change
                | Operation::Unskip
                | Operation::Check
                | Operation::OpenInstaller(_)
                | Operation::DownloadPage(_),
            ) => ButtonKind::Secondary,
            _ => ButtonKind::Primary,
        };
        let non_mutating = matches!(
            entry.operation,
            Some(
                Operation::Cancel
                    | Operation::Change
                    | Operation::Unskip
                    | Operation::Check
                    | Operation::OpenInstaller(_)
                    | Operation::DownloadPage(_)
            )
        );
        row = row
            .action(label)
            .action_kind(kind)
            .action_enabled(non_mutating || (!state.queue.busy() && !cx.busy));
    }
    if let Some(progress) = &entry.progress {
        row = row.progress(progress.fraction, &progress.label);
    }
    let response = row.show(ui, &mut expanded, |ui| {
        egui::Frame::new()
            .fill(cx.pal.bg_subtle)
            .inner_margin(12)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 8.0;
                // The full reason remains readable after a compact row elides it.
                if matches!(
                    entry.status,
                    UpdateStatus::Failed | UpdateStatus::Blocked | UpdateStatus::Unknown
                ) {
                    ui.add(egui::Label::new(Text::small(&entry.description, cx.pal)).wrap());
                }
                let enabled = !state.queue.busy();
                ui.add_enabled_ui(enabled, |ui| detail(ui, cx, state, entry, tools, out));
            });
        ui.add_space(6.0);
    });
    if expanded {
        state.expanded.insert(entry.id.clone());
    } else {
        state.expanded.remove(&entry.id);
    }
    if focusing {
        response.response.scroll_to_me(Some(Align::Center));
        response.response.request_focus();
        if ui.clip_rect().contains(response.response.rect.center()) {
            state.focus = None;
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
    if response.action_clicked
        && let Some(operation) = &entry.operation
    {
        if matches!(
            operation,
            Operation::Tool(_)
                | Operation::ToolInstall(_)
                | Operation::SelfUpdate
                | Operation::Codex { .. }
                | Operation::AppDownload
        ) && !entry.cautions.is_empty()
            || matches!(
                operation,
                Operation::ToolVersion { .. } | Operation::ToolRemove { .. }
            )
        {
            state.confirm = Some(Batch {
                jobs: vec![entry.clone()],
                cautions: entry
                    .cautions
                    .iter()
                    .map(|c| format!("{}: {c}", entry.name))
                    .collect(),
            });
        } else {
            state.perform(cx, &entry.id, operation, out);
        }
    }
}
fn detail(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    entry: &Entry,
    tools: &ToolsView,
    out: &mut Vec<Action>,
) {
    if entry.id == UpdateItem::Superba {
        super::self_update::show(ui, cx, &mut state.superba, out);
        return;
    }
    if let UpdateItem::Tool(id) = entry.id {
        if let Some(row) = tools
            .rows
            .iter()
            .find(|r| r.entry.id == UpdateItem::Tool(id))
            && let Some(request) = super::tools::section(
                ui,
                cx,
                state.tools.as_ref(),
                row,
                state.tool_versions.entry(id).or_default(),
                out,
            )
        {
            state.confirm = Some(Batch {
                cautions: request.cautions.clone(),
                jobs: vec![request],
            });
        }
        return;
    }
    let Some(view) = state
        .view
        .value
        .clone()
        .or_else(|| cx.data.versions.value.clone())
    else {
        return;
    };
    match &entry.id {
        UpdateItem::App => super::panels::app_section(ui, cx, state, &view, out),
        UpdateItem::AppPin => super::panels::pin_section(ui, cx, state, &view, out),
        UpdateItem::CodexCli => super::panels::cli_section(ui, cx, state, &view, out),
        UpdateItem::CodexRemote(id) => {
            let mut view = view;
            view.remote.retain(|r| &r.target_id == id);
            super::panels::remote_section(ui, cx, state, &view);
        }
        _ => {}
    }
}
fn confirmation(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(mut batch) = state.confirm.clone() else {
        return;
    };
    let removing = batch
        .jobs
        .iter()
        .any(|e| matches!(e.operation, Some(Operation::ToolRemove { .. })));
    let mut start = false;
    let mut cancel = false;
    let response = dialog(
        ui.ctx(),
        egui::Id::unique("updates-batch-confirm"),
        440.0_f32.min(ui.ctx().content_rect().width() - 80.0),
        |ui| {
            ui.label(Text::section(
                cx.tr.t(if removing {
                    "updates.list.confirm_remove"
                } else {
                    "updates.list.confirm"
                }),
                cx.pal,
            ));
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for job in &batch.jobs {
                        ui.add(
                            egui::Label::new(Text::small(
                                format!("{} · {}", job.name, job.versions()),
                                cx.pal,
                            ))
                            .wrap(),
                        );
                    }
                    for caution in &batch.cautions {
                        callout(ui, cx, Status::Warn, caution);
                    }
                });
            if removing {
                ui.add(
                    egui::Label::new(Text::small(cx.tr.t("updates.tools.kept_note"), cx.pal))
                        .wrap(),
                );
                let tools = state.tools.view(cx);
                for job in &mut batch.jobs {
                    if let Some(Operation::ToolRemove { id, remove_data }) = &mut job.operation
                        && let Some(removal) = tools
                            .rows
                            .iter()
                            .find(|r| r.entry.id == UpdateItem::Tool(*id))
                            .and_then(|r| r.removal.as_ref())
                        && !removal.data_dirs.is_empty()
                    {
                        ui.checkbox(remove_data, cx.tr.t("updates.list.remove_data"));
                        for path in &removal.data_dirs {
                            ui.add(egui::Label::new(Text::mono(path, cx.pal)).wrap());
                        }
                    }
                }
            } else {
                ui.label(Text::small(cx.tr.t("updates.list.sequential"), cx.pal));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                start = ui
                    .add(
                        Button::primary(cx.tr.t(if removing {
                            "updates.list.remove"
                        } else {
                            "updates.list.start"
                        }))
                        .enabled(!cx.busy),
                    )
                    .clicked();
                cancel = ui.add(Button::new(cx.tr.t("updates.win.cancel"))).clicked();
            });
        },
    );
    if start {
        state.confirm = None;
        state.queue.start(batch);
        state.advance_queue(cx, out);
    } else if cancel || response.should_close() {
        state.confirm = None;
    } else {
        state.confirm = Some(batch);
    }
}

#[allow(clippy::too_many_arguments)]
fn toolbar(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    entries: &[Entry],
    tools: &ToolsView,
    count: usize,
    out: &mut Vec<Action>,
) {
    if ui
        .add(
            Button::primary(cx.tr.t("updates.list.update_all"))
                .small()
                .enabled(entries.iter().any(Entry::batchable) && !state.queue.busy() && !cx.busy),
        )
        .clicked()
    {
        state.confirm = Some(Batch::from_entries(entries));
    }
    let checking = state.view.loading() || cx.data.selfupdate.loading || tools.checking;
    if ui
        .add(
            Button::new(cx.tr.t("updates.check"))
                .small()
                .icon(Icon::Refresh)
                .enabled(!checking)
                .loading(checking),
        )
        .clicked()
    {
        state.check();
        out.push(Action::CheckSelfUpdate);
    }
    Segmented::new("update-filter", &mut state.filter)
        .label(cx.tr.t("updates.list.filter"))
        .option(
            Filter::All,
            cx.tr.f("updates.list.all", &[("n", &entries.len())]),
        )
        .option(
            Filter::Available,
            cx.tr.f("updates.list.updatable", &[("n", &count)]),
        )
        .small()
        .show(ui);
}
