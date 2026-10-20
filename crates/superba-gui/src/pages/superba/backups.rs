//! Configuration backup stores, including tools no longer installed.
use egui::Ui;
use std::collections::BTreeSet;
use superba_core::harness::{HarnessApplyReport, HarnessBackup, HarnessId};
use superba_core::{CodexFileBackup, CodexFileBackupQuery, CodexFileKind, Result, Timestamp};

use super::maintenance::{self, Read};
use crate::design::widgets::{Button, Card, Status, Table, Text, dialog, dialog_body};
use crate::pages::{Action, Cx, Page, error_text, section_with};
use crate::runtime::ReadTask;

/// Backup storage owner; Codex includes every home, Claude Code is local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Store {
    /// Codex config.toml and auth.json, including remote homes.
    Codex,
    /// Claude Code settings.json.
    Claude,
    /// One of the other coding tools (all editions).
    Harness(HarnessId),
}
impl Store {
    /// Human-readable tool name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Harness(h) => h.display_name(),
        }
    }
}

#[derive(Clone, Debug)]
enum Backup {
    Codex(CodexFileBackup),
    Harness(HarnessBackup),
}
impl Backup {
    fn at(&self) -> Timestamp {
        match self {
            Self::Codex(b) => b.at,
            Self::Harness(b) => b.at,
        }
    }
    fn size(&self) -> u64 {
        match self {
            Self::Codex(b) => b.size,
            Self::Harness(b) => b.size,
        }
    }
    fn id(&self) -> &str {
        match self {
            Self::Codex(b) => &b.id,
            Self::Harness(b) => &b.id,
        }
    }
    fn description(&self, cx: &Cx<'_>) -> String {
        let (file, source) = match self {
            Self::Codex(b) => (
                format!(
                    "{} · {}",
                    crate::pages::settings::files_view::target_label(cx, &b.target),
                    b.kind.file_name()
                ),
                &b.source,
            ),
            Self::Harness(b) => (superba_core::paths::display_path(&b.path), &b.source),
        };
        format!(
            "{file}\n{} · {} · {}",
            crate::pages::clock_time(self.at(), true),
            maintenance::bytes(self.size()),
            cx.tr.text(&source.text())
        )
    }
}

#[derive(Debug)]
struct StoreView {
    store: Store,
    backups: Result<Vec<Backup>>,
}
impl StoreView {
    fn bytes(&self) -> Option<u64> {
        self.backups.as_ref().ok().map(|rows| {
            rows.iter()
                .fold(0_u64, |sum, b| sum.saturating_add(b.size()))
        })
    }
}

/// The summary, expanded backup lists and an explicit whole-file restore prompt.
#[derive(Debug, Default)]
pub struct State {
    read: Read<Vec<StoreView>>,
    expanded: BTreeSet<Store>,
    confirm: Option<HarnessBackup>,
    restoring: Option<ReadTask<HarnessApplyReport>>,
    restore_error: Option<String>,
    restored: Option<String>,
}
impl State {
    /// Read all stores again after a refresh or when returning to this section.
    pub fn invalidate(&mut self) {
        self.read.invalidate();
    }
    /// A whole-file restore is being reviewed or executed.
    pub fn modal(&self) -> bool {
        self.confirm.is_some()
    }
    fn poll(&mut self, ui: &Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
        if let Some(task) = &self.restoring {
            if let Some(result) = task.poll() {
                self.restoring = None;
                match result {
                    Ok(report) => {
                        self.confirm = None;
                        self.restored = Some(report.harness.display_name().into());
                        self.read.invalidate();
                        out.push(Action::Refresh);
                    }
                    Err(e) => self.restore_error = Some(error_text(cx.tr, &e)),
                }
            } else {
                crate::pages::shared::loading::repaint_soon(ui);
            }
        }
        let service = cx.service.clone();
        self.read.poll(ui, move || async move {
            let all = service
                .codex_file_backups(CodexFileBackupQuery::default())
                .await;
            let mut stores = codex_stores(all);
            for h in HarnessId::ALL {
                let backups = service
                    .harness_backups(h)
                    .await
                    .map(|b| b.into_iter().map(Backup::Harness).collect());
                stores.push(StoreView {
                    store: Store::Harness(h),
                    backups,
                });
            }
            for store in &mut stores {
                if let Ok(backups) = &mut store.backups {
                    backups.sort_by_key(|b| (std::cmp::Reverse(b.at()), b.id().to_owned()));
                }
            }
            Ok(stores)
        });
    }
}

fn codex_stores(all: Result<Vec<CodexFileBackup>>) -> Vec<StoreView> {
    let mut stores = Vec::new();
    for store in [Store::Codex, Store::Claude] {
        let backups = all
            .as_ref()
            .map(|rows| {
                rows.iter()
                    .filter(|b| {
                        (b.kind == CodexFileKind::ClaudeSettings) == (store == Store::Claude)
                    })
                    .cloned()
                    .map(Backup::Codex)
                    .collect()
            })
            .map_err(Clone::clone);
        stores.push(StoreView { store, backups });
    }
    stores
}

/// A target-specific editor command; it never restores or writes by itself.
pub fn editor_action(backup: &CodexFileBackup) -> Action {
    if backup.kind == CodexFileKind::ClaudeSettings {
        Action::PageCommand {
            page: Page::ClaudeSettings,
            command: "files-backups".into(),
        }
    } else {
        Action::PageCommand {
            page: Page::Settings,
            command: format!(
                "files-backups:{}@{}",
                backup.kind.file_name(),
                backup.target
            ),
        }
    }
}

/// Summary counts and restore entry points; every store is listed even if empty.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.poll(ui, cx, out);
    section_with(ui, cx, &cx.tr.t("superba.backups.title"), None, |ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("superba.backups.refresh"))
                    .small()
                    .loading(state.read.load.loading()),
            )
            .clicked()
        {
            state.invalidate();
        }
    });
    state.read.message(ui, cx);
    if let Some(tool) = &state.restored {
        crate::pages::note(
            ui,
            cx,
            Status::Ok,
            cx.tr.f("superba.backups.restored", &[("tool", tool)]),
        );
    }
    if let Some(stores) = &state.read.value {
        Card::new().flat().show(ui, |ui| {
            let narrow = ui.available_width() < 650.0;
            let table = if narrow {
                Table::new(&[1.6, 1.8, 0.0]).min_widths(&[55.0, 65.0, 142.0])
            } else {
                Table::new(&[1.5, 0.8, 1.0, 1.9, 0.0]).min_widths(&[55.0, 42.0, 55.0, 45.0, 146.0])
            };
            let headers = if narrow {
                vec!["tool", "summary", "actions"]
            } else {
                vec!["tool", "count", "size", "latest", "actions"]
            };
            table.header(
                ui,
                &headers
                    .into_iter()
                    .map(|k| cx.tr.t(&format!("superba.backups.col.{k}")))
                    .collect::<Vec<_>>(),
            );
            for (i, store) in stores.iter().enumerate() {
                ui.push_id(store.store, |ui| {
                    let mut open = state.expanded.contains(&store.store);
                    let rows = store.backups.as_ref().ok();
                    let count = rows.map_or_else(|| "—".into(), |b| b.len().to_string());
                    let bytes = store.bytes().map_or_else(|| "—".into(), maintenance::bytes);
                    let latest = rows
                        .and_then(|b| b.first())
                        .map(|b| cx.ago(b.at()))
                        .unwrap_or_else(|| "—".into());
                    table.row(
                        ui,
                        if narrow { 54.0 } else { 38.0 },
                        i + 1 == stores.len(),
                        |ui, col| {
                            if col == 0 {
                                ui.add(
                                    egui::Label::new(Text::body(store.store.name(), cx.pal))
                                        .truncate(),
                                );
                            } else if col == if narrow { 2 } else { 4 } {
                                let view_label = cx.tr.f(
                                    "superba.backups.view_tool",
                                    &[("tool", &store.store.name())],
                                );
                                if ui
                                    .add(Button::ghost(cx.tr.t("superba.backups.view")).small())
                                    .accessible_name(view_label)
                                    .clicked()
                                {
                                    open = !open;
                                }
                                let restore_label = cx.tr.f(
                                    "superba.backups.restore_tool",
                                    &[("tool", &store.store.name())],
                                );
                                if ui
                                    .add(
                                        Button::ghost(cx.tr.t("superba.backups.restore"))
                                            .small()
                                            .enabled(
                                                rows.is_some_and(|b| !b.is_empty()) && !cx.busy,
                                            ),
                                    )
                                    .accessible_name(restore_label)
                                    .clicked()
                                {
                                    open = true;
                                }
                            } else if narrow {
                                ui.vertical(|ui| {
                                    ui.label(Text::small(
                                        cx.tr.f(
                                            "superba.backups.summary",
                                            &[("n", &count), ("size", &bytes)],
                                        ),
                                        cx.pal,
                                    ));
                                    ui.label(Text::small(&latest, cx.pal));
                                });
                            } else {
                                ui.label(Text::small(
                                    match col {
                                        1 => &count,
                                        2 => &bytes,
                                        _ => &latest,
                                    },
                                    cx.pal,
                                ));
                            }
                        },
                    );
                    if let Err(error) = &store.backups {
                        crate::pages::note(
                            ui,
                            cx,
                            Status::Error,
                            format!("{}: {}", store.store.name(), error_text(cx.tr, error)),
                        );
                    }
                    if open {
                        state.expanded.insert(store.store);
                        egui::Frame::new().inner_margin(12).show(ui, |ui| {
                            ui.label(Text::small(cx.tr.t("superba.backups.choose"), cx.pal));
                            if let Some(rows) = rows {
                                if rows.is_empty() {
                                    ui.label(Text::small(cx.tr.t("superba.backups.empty"), cx.pal));
                                }
                                egui::ScrollArea::vertical()
                                    .id_salt("backup-list")
                                    .max_height(280.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        for backup in rows {
                                            ui.push_id(backup.id(), |ui| {
                                                ui.add(
                                                    egui::Label::new(Text::small(
                                                        backup.description(cx),
                                                        cx.pal,
                                                    ))
                                                    .wrap(),
                                                );
                                                match backup {
                                                    Backup::Codex(b) => {
                                                        if ui
                                                            .add(
                                                                Button::ghost(
                                                                    cx.tr.t(
                                                                        "superba.backups.review",
                                                                    ),
                                                                )
                                                                .small(),
                                                            )
                                                            .accessible_name(format!(
                                                                "{} · {} · {}",
                                                                cx.tr.t("superba.backups.review"),
                                                                store.store.name(),
                                                                b.id
                                                            ))
                                                            .clicked()
                                                        {
                                                            out.push(editor_action(b));
                                                        }
                                                    }
                                                    Backup::Harness(b) => {
                                                        if ui
                                                            .add(
                                                                Button::new(
                                                                    cx.tr.t(
                                                                        "superba.backups.restore",
                                                                    ),
                                                                )
                                                                .small()
                                                                .enabled(!cx.busy),
                                                            )
                                                            .accessible_name(format!(
                                                                "{} · {} · {}",
                                                                cx.tr.t("superba.backups.restore"),
                                                                store.store.name(),
                                                                b.id
                                                            ))
                                                            .clicked()
                                                        {
                                                            state.confirm = Some(b.clone());
                                                            state.restore_error = None;
                                                        }
                                                    }
                                                }
                                                ui.add_space(8.0);
                                            });
                                        }
                                    });
                            }
                        });
                    } else {
                        state.expanded.remove(&store.store);
                    }
                });
            }
        });
    }
    ui.add(egui::Label::new(Text::small(cx.tr.t("superba.backups.hint"), cx.pal)).wrap());
    restore_dialog(ui, cx, state);
}

fn restore_dialog(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(backup) = state.confirm.clone() else {
        return;
    };
    let busy = state.restoring.is_some();
    let mut confirm = false;
    let mut close = false;
    let modal = dialog(
        ui.ctx(),
        ui.make_persistent_id("restore-harness-backup"),
        480.0_f32.min(ui.ctx().content_rect().width() - 80.0),
        |ui| {
            ui.label(Text::title(
                cx.tr.f(
                    "superba.backups.confirm_title",
                    &[("tool", &backup.harness.display_name())],
                ),
                cx.pal,
            ));
            dialog_body(ui, |ui| {
                ui.add(
                    egui::Label::new(Text::body(cx.tr.t("superba.backups.confirm_body"), cx.pal))
                        .wrap(),
                );
                ui.add(
                    egui::Label::new(Text::mono(
                        superba_core::paths::display_path(&backup.path),
                        cx.pal,
                    ))
                    .wrap(),
                );
                ui.label(Text::small(
                    format!(
                        "{} · {}",
                        backup.id,
                        crate::pages::clock_time(backup.at, true)
                    ),
                    cx.pal,
                ));
                if let Some(e) = &state.restore_error {
                    crate::pages::note(ui, cx, Status::Error, e.clone());
                }
            });
            ui.horizontal_wrapped(|ui| {
                confirm = ui
                    .add(
                        Button::primary(cx.tr.t("superba.backups.confirm"))
                            .loading(busy)
                            .enabled(!cx.busy),
                    )
                    .clicked();
                close = ui
                    .add(Button::ghost(cx.tr.t("superba.backups.cancel")).enabled(!busy))
                    .clicked();
            });
        },
    );
    if !busy && (close || modal.should_close()) {
        state.confirm = None;
    } else if confirm && !busy && !cx.busy {
        let service = cx.service.clone();
        state.restore_error = None;
        state.restoring = Some(crate::runtime::task(async move {
            service
                .harness_restore_backup(backup.harness, backup.id)
                .await
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superba_core::{BackupSource, TargetId};
    fn backup(kind: CodexFileKind, target: &str, size: u64) -> CodexFileBackup {
        CodexFileBackup {
            id: format!("{target}-{kind:?}"),
            target: TargetId::new(target),
            target_label: target.into(),
            kind,
            at: Timestamp::default(),
            source: BackupSource::Original,
            size,
            sha256: None,
        }
    }
    #[test]
    fn counts_sizes_and_owners_include_remote_backups_once() {
        let stores = codex_stores(Ok(vec![
            backup(CodexFileKind::ConfigToml, "local", 1024),
            backup(CodexFileKind::AuthJson, "remote", 512),
            backup(CodexFileKind::ClaudeSettings, "local", 128),
        ]));
        assert_eq!(stores[0].backups.as_ref().unwrap().len(), 2);
        assert_eq!(stores[0].bytes(), Some(1536));
        assert_eq!(stores[1].backups.as_ref().unwrap().len(), 1);
        assert_eq!(stores[1].bytes(), Some(128));
    }
    #[test]
    fn unreadable_store_is_not_an_empty_store() {
        assert_eq!(codex_stores(Ok(vec![]))[0].bytes(), Some(0));
        let failed = codex_stores(Err(superba_core::Error::other("cannot read index")));
        assert!(
            failed
                .iter()
                .all(|s| s.backups.is_err() && s.bytes().is_none())
        );
    }
    #[test]
    fn remote_and_claude_entries_use_the_correct_backup_editor() {
        assert!(
            matches!(editor_action(&backup(CodexFileKind::AuthJson, "remote", 42)), Action::PageCommand { page: Page::Settings, command } if command == "files-backups:auth.json@remote")
        );
        assert!(
            matches!(editor_action(&backup(CodexFileKind::ClaudeSettings, "local", 42)), Action::PageCommand { page: Page::ClaudeSettings, command } if command == "files-backups")
        );
    }
    #[test]
    fn sizes_saturate_instead_of_wrapping() {
        let stores = codex_stores(Ok(vec![
            backup(CodexFileKind::ConfigToml, "local", u64::MAX),
            backup(CodexFileKind::AuthJson, "local", 10),
        ]));
        assert_eq!(stores[0].bytes(), Some(u64::MAX));
    }
}
