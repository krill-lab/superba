//! Harness partition of the GH#9 editor: protected contents, shared diff view,
//! encrypted backups and compare-and-swap saves through the harness service.
use crate::design::widgets::{Button, Text, dialog};
use crate::pages::shared::loading::Latest;
use crate::pages::{Cx, error_text};
use crate::runtime::{ReadTask, task};
use std::time::Duration;
use superba_core::harness::*;

/// Editor state. Debug deliberately omits configuration contents.
#[derive(Default)]
pub struct Editor {
    /// Replaces the tool detail while open.
    pub open: bool,
    /// Explicit save review.
    pub review: bool,
    /// Backups section is expanded.
    pub backups_open: bool,
    doc: Option<HarnessFileDoc>,
    load: Latest<(HarnessId, String), HarnessFileDoc>,
    buffer: String,
    original: String,
    revealed: bool,
    discard: bool,
    backups: Vec<HarnessBackup>,
    backups_task: Option<ReadTask<Vec<HarnessBackup>>>,
    backup_load: Latest<String, superba_core::SecretBytes>,
    backup_preview: Option<(String, String)>,
    saving: Option<ReadTask<HarnessFileDoc>>,
    restoring: Option<ReadTask<HarnessApplyReport>>,
    error: Option<String>,
}
impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessEditor")
            .field("open", &self.open)
            .field("dirty", &self.dirty())
            .finish_non_exhaustive()
    }
}
impl Editor {
    /// Open only a file recognized by the writer; a late response cannot replace another choice.
    pub fn open(&mut self, cx: &Cx<'_>, h: HarnessId, file: String, backups: bool) {
        *self = Self {
            open: true,
            backups_open: backups,
            ..Default::default()
        };
        self.read(cx, h, file);
        let svc = cx.service.clone();
        self.backups_task = Some(task(async move { svc.harness_backups(h).await }));
    }
    fn read(&mut self, cx: &Cx<'_>, h: HarnessId, file: String) {
        let svc = cx.service.clone();
        let key = (h, file.clone());
        self.load.start(
            key,
            task(async move { svc.harness_file(h, file).await }),
            Duration::from_secs(45),
        );
    }
    /// Unsaved bytes, independently of whether the secret text is revealed.
    pub fn dirty(&self) -> bool {
        self.doc.is_some() && self.buffer != self.original
    }
    /// Whether the editor owns a modal.
    pub fn modal(&self) -> bool {
        self.review || self.discard
    }
    fn set_doc(&mut self, doc: HarnessFileDoc) {
        self.original = doc
            .contents
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.expose()).into_owned())
            .unwrap_or_default();
        self.buffer = self.original.clone();
        self.doc = Some(doc);
        self.error = None;
    }
    fn poll(&mut self, cx: &Cx<'_>) {
        if let Some((_, doc)) = self.load.poll() {
            self.set_doc(doc);
        }
        if let Some((id, bytes)) = self.backup_load.poll() {
            self.backup_preview = Some((id, String::from_utf8_lossy(bytes.expose()).into_owned()));
        }
        if let Some(answer) = self.backups_task.as_ref().and_then(ReadTask::poll) {
            self.backups_task = None;
            match answer {
                Ok(v) => self.backups = v,
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(answer) = self.saving.as_ref().and_then(ReadTask::poll) {
            self.saving = None;
            match answer {
                Ok(doc) => {
                    let h = doc.harness;
                    self.set_doc(doc);
                    let svc = cx.service.clone();
                    self.backups_task = Some(task(async move { svc.harness_backups(h).await }));
                }
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(answer) = self.restoring.as_ref().and_then(ReadTask::poll) {
            self.restoring = None;
            match answer {
                Ok(_) => {
                    self.backup_preview = None;
                    if let Some(doc) = self.doc.clone() {
                        self.open(cx, doc.harness, doc.file.key, true);
                    }
                }
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
    }
}
/// Shared diff and explicit backup restore, with file status errors shown in place.
pub fn show(ui: &mut egui::Ui, cx: &Cx<'_>, ed: &mut Editor) {
    ed.poll(cx);
    let busy = ed.saving.is_some() || ed.restoring.is_some();
    if busy || ed.load.loading() || ed.backups_task.is_some() || ed.backup_load.loading() {
        ui.ctx().request_repaint_after(Duration::from_millis(60));
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add(Button::new(cx.tr.t("tools.back")).enabled(!busy))
            .clicked()
        {
            if ed.dirty() {
                ed.discard = true;
            } else {
                ed.open = false;
            }
        }
        if ui.add(Button::ghost(cx.tr.t("tools.backups"))).clicked() {
            ed.backups_open = !ed.backups_open;
        }
        ui.checkbox(&mut ed.revealed, cx.tr.t("tools.reveal"));
        if ui
            .add(Button::primary(cx.tr.t("tools.review_save")).enabled(ed.dirty() && !busy))
            .clicked()
        {
            ed.review = true;
        }
    });
    if let Some(f) = ed.load.failure() {
        ui.label(Text::body(f.text(cx), cx.pal).color(cx.pal.red));
    }
    if let Some(e) = &ed.error {
        ui.add(egui::Label::new(Text::body(e, cx.pal).color(cx.pal.red)).wrap());
    }
    let Some(doc) = ed.doc.clone() else {
        ui.label(cx.tr.t("tools.loading"));
        return;
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(superba_core::paths::display_path(&doc.file.path)).monospace(),
        )
        .wrap(),
    );
    if !doc.file.status.writable() {
        ui.add(
            egui::Label::new(
                Text::small(format!("{:?}", doc.file.status), cx.pal).color(cx.pal.red),
            )
            .wrap(),
        );
    }
    let protected = doc.file.holds_key && !ed.revealed;
    egui::ScrollArea::vertical()
        .id_salt("harness-editor-body")
        .show(ui, |ui| {
            if protected {
                ui.label(cx.tr.t("tools.protected_file"));
            } else {
                egui::ScrollArea::both()
                    .id_salt("harness-editor-text")
                    .max_height(340.0)
                    .show(ui, |ui| {
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut ed.buffer)
                                .code_editor()
                                .desired_width(ui.available_width())
                                .desired_rows(14),
                        );
                        r.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::TextInput,
                                ui.is_enabled(),
                                cx.tr.t("tools.file_contents"),
                            )
                        });
                    });
            }
            if ed.backups_open {
                ui.label(Text::strong(cx.tr.t("tools.backups"), cx.pal));
                let backups: Vec<_> = ed
                    .backups
                    .iter()
                    .filter(|b| b.file == doc.file.key)
                    .cloned()
                    .collect();
                if backups.is_empty() {
                    ui.label(cx.tr.t("tools.no_backups"));
                }
                for backup in backups {
                    if ui
                        .add(
                            Button::ghost(format!(
                                "{} · {}",
                                backup.id,
                                cx.tr.text(&backup.source.text())
                            ))
                            .small(),
                        )
                        .clicked()
                    {
                        let svc = cx.service.clone();
                        let id = backup.id.clone();
                        ed.backup_preview = None;
                        ed.backup_load.start(
                            backup.id,
                            task(async move { svc.harness_backup_contents(id).await }),
                            Duration::from_secs(45),
                        );
                    }
                }
                if let Some(f) = ed.backup_load.failure() {
                    ui.label(Text::body(f.text(cx), cx.pal).color(cx.pal.red));
                }
                if let Some((id, text)) = &ed.backup_preview {
                    if !protected {
                        crate::pages::settings::files_view::diff_view(
                            ui,
                            cx,
                            "harness-backup",
                            &ed.original,
                            text,
                            220.0,
                        );
                    }
                    if ui
                        .add(
                            Button::primary(cx.tr.t("tools.restore_backup"))
                                .enabled(!busy && !ed.dirty()),
                        )
                        .clicked()
                    {
                        let svc = cx.service.clone();
                        let id = id.clone();
                        let h = doc.harness;
                        let sha = doc.sha256.clone();
                        ed.restoring = Some(task(async move {
                            svc.harness_restore_reviewed_backup(h, id, sha).await
                        }));
                    }
                }
            }
        });
    if ed.review {
        let mut save = false;
        let mut cancel = false;
        let response = dialog(
            ui.ctx(),
            egui::Id::unique("harness-file-review"),
            560.0_f32.min(ui.ctx().content_rect().width() - 64.0),
            |ui| {
                ui.heading(cx.tr.t("tools.review_save"));
                if protected {
                    ui.label(cx.tr.t("tools.protected_file"));
                } else {
                    crate::pages::settings::files_view::diff_view(
                        ui,
                        cx,
                        "harness-edit",
                        &ed.original,
                        &ed.buffer,
                        300.0,
                    );
                }
                ui.horizontal(|ui| {
                    cancel = ui.add(Button::new(cx.tr.t("tools.cancel"))).clicked();
                    save = ui.add(Button::primary(cx.tr.t("tools.confirm"))).clicked();
                });
            },
        );
        if cancel || response.should_close() {
            ed.review = false;
        } else if save {
            ed.review = false;
            let svc = cx.service.clone();
            let save = HarnessFileSave {
                harness: doc.harness,
                file: doc.file.key,
                base_sha256: doc.sha256,
                contents: superba_core::SecretBytes::new(ed.buffer.as_bytes().to_vec()),
            };
            ed.saving = Some(task(async move { svc.save_harness_file(save).await }));
        }
    }
    if ed.discard {
        let mut yes = false;
        let mut no = false;
        let response = dialog(
            ui.ctx(),
            egui::Id::unique("harness-file-discard"),
            380.0,
            |ui| {
                ui.heading(cx.tr.t("tools.discard_question"));
                ui.horizontal(|ui| {
                    no = ui.add(Button::new(cx.tr.t("tools.cancel"))).clicked();
                    yes = ui.add(Button::primary(cx.tr.t("tools.discard"))).clicked();
                });
            },
        );
        if no || response.should_close() {
            ed.discard = false;
        } else if yes {
            *ed = Editor::default();
        }
    }
}
