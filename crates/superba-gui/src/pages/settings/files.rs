//! "Edit Codex files" (GitHub issue 9): `config.toml` and `auth.json` of this
//! computer, a remote host or a WSL distribution, edited by hand in a
//! monospace editor with TOML / JSON highlighting.
//!
//! The text is checked as it is typed with the same rules the service saves
//! with (`superba_switch::codex_files`): syntax errors with line and column
//! (never saved), catalog errors for the Codex versions reading that home
//! (saved only with "Save anyway"), unknown and deprecated keys, and the keys
//! Superba manages. `auth.json` secrets are shown as placeholders until
//! "Show secrets"; a placeholder changed only partly is an error. "Format"
//! tidies the file without losing a comment. "Review & save" shows the diff,
//! which managed keys the edit changes (they stay the user's: the next switch
//! asks, GH#5) and saves after an encrypted backup; afterwards the page says
//! which running Codex still reads the old file ("Codex in effect", GH#4).
//! "Backups" lists every backup of the file (the editor's, the switch
//! pre-images, the original) with a diff to the current file; restoring one
//! loads it into the editor and saves it like an edit.
//!
//! This module holds the state and its logic; [`super::files_view`] draws it.

use std::hash::{Hash, Hasher};

use superba_core::{
    CodexFileBackup, CodexFileBackupContents, CodexFileBackupQuery, CodexFileDoc, CodexFileKind,
    CodexFileSave, CodexFileSaved, FileProblem, KeyChange, SaveReason, SecretBytes, TargetId,
};
use superba_switch::codex_files::{self, CheckContext, Masker};

use crate::design::widgets::Status;
use crate::pages::{Action, Cx};
use crate::runtime::ReadTask;

/// A dialog of the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    /// The diff and the ownership summary before saving.
    Review,
    /// The backups of the file (and the diff to one of them).
    Backups,
    /// Unsaved changes would be lost by `next`.
    Discard(Next),
}

/// What a confirmed discard does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Leave the editor.
    Close,
    /// Open another file or home.
    Open(TargetId, CodexFileKind),
    /// Read the file again.
    Reload,
    /// Throw the edits away and stay.
    Revert,
}

/// The editor's state, kept while the Settings page is left.
#[derive(Default)]
pub struct Editor {
    /// The editor replaces the settings list.
    pub open: bool,
    /// The home (`local` until another is chosen).
    pub target: Option<TargetId>,
    /// The file.
    pub kind: Option<CodexFileKind>,
    /// The file as opened.
    pub doc: Option<CodexFileDoc>,
    load: Option<ReadTask<CodexFileDoc>>,
    /// Why it could not be opened.
    pub load_error: Option<String>,
    /// What the text field shows (placeholders while secrets are hidden).
    pub buffer: String,
    /// The real text when opened (or last saved).
    original: String,
    masker: Masker,
    /// `auth.json` secrets are shown.
    pub revealed: bool,
    /// The check of [`Editor::buffer`].
    pub problems: Vec<FileProblem>,
    checked: Option<u64>,
    /// The open dialog.
    pub dialog: Option<Dialog>,
    saving: Option<ReadTask<CodexFileSaved>>,
    /// The last save (its "in effect" note).
    pub saved: Option<CodexFileSaved>,
    /// The last save failed.
    pub save_error: Option<String>,
    /// "Save anyway" is ticked in the review.
    pub force: bool,
    /// Why the next save happens (an edit, or a restore of a backup).
    pub reason: Option<SaveReason>,
    backups_task: Option<ReadTask<Vec<CodexFileBackup>>>,
    /// The backups of the file.
    pub backups: Option<Vec<CodexFileBackup>>,
    /// Reading the backups failed.
    pub backups_error: Option<String>,
    backup_task: Option<ReadTask<CodexFileBackupContents>>,
    /// The backup whose diff the Backups dialog shows (its real text).
    pub preview: Option<(CodexFileBackup, String)>,
    /// Formatting failed.
    pub format_error: Option<String>,
    /// Put the cursor at this byte of the buffer on the next frame.
    pub jump: Option<usize>,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("files::Editor")
            .field("open", &self.open)
            .field("target", &self.target)
            .field("kind", &self.kind)
            .field("dirty", &self.dirty())
            .field("problems", &self.problems.len())
            .field("dialog", &self.dialog)
            .finish_non_exhaustive()
    }
}

fn hash(s: &str, revealed: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    revealed.hash(&mut h);
    h.finish()
}

impl Editor {
    /// The home shown.
    pub fn target(&self) -> TargetId {
        self.target.clone().unwrap_or_else(TargetId::local)
    }

    /// The file shown.
    pub fn kind(&self) -> CodexFileKind {
        self.kind.unwrap_or(CodexFileKind::ConfigToml)
    }

    /// Secrets are shown as placeholders now.
    pub fn masked(&self) -> bool {
        self.kind() == CodexFileKind::AuthJson && !self.revealed
    }

    /// The real text of the buffer (placeholders put back).
    pub fn real_text(&self) -> String {
        if self.masked() {
            self.masker.unmask(&self.buffer)
        } else {
            self.buffer.clone()
        }
    }

    /// `text` as the buffer shows it.
    fn view_of(&mut self, text: &str) -> String {
        if self.masked() {
            self.masker.mask(text)
        } else {
            text.to_owned()
        }
    }

    /// The text when opened, as the buffer would show it.
    pub fn original_view(&mut self) -> String {
        let o = self.original.clone();
        self.view_of(&o)
    }

    /// The buffer differs from the file.
    pub fn dirty(&self) -> bool {
        self.doc.is_some() && self.real_text() != self.original
    }

    /// A read or a save runs.
    pub fn busy(&self) -> bool {
        self.load.is_some() || self.saving.is_some()
    }

    /// Saving is running.
    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    /// Open the editor on `target` / `kind` (asks first when edits would be
    /// lost).
    pub fn open_file(&mut self, cx: &Cx<'_>, target: TargetId, kind: CodexFileKind) {
        self.open = true;
        let same = self.target() == target && self.kind() == kind && self.doc.is_some();
        if same {
            return;
        }
        if self.dirty() {
            self.dialog = Some(Dialog::Discard(Next::Open(target, kind)));
            return;
        }
        self.target = Some(target);
        self.kind = Some(kind);
        self.reload(cx);
    }

    /// Read the file again (discarding the buffer).
    pub fn reload(&mut self, cx: &Cx<'_>) {
        let (service, target, kind) = (cx.service.clone(), self.target(), self.kind());
        self.load_error = None;
        self.load = Some(crate::runtime::task(async move {
            service.codex_file(target, kind).await
        }));
    }

    /// Leave the editor (asks first when edits would be lost).
    pub fn close(&mut self) {
        if self.dirty() {
            self.dialog = Some(Dialog::Discard(Next::Close));
        } else {
            self.open = false;
            self.dialog = None;
        }
    }

    /// Do what a confirmed discard was asked for.
    pub fn discard_and(&mut self, cx: &Cx<'_>, next: Next) {
        self.dialog = None;
        self.reason = None;
        let o = self.original_view();
        self.buffer = o;
        match next {
            Next::Close => self.open = false,
            Next::Open(target, kind) => {
                self.target = Some(target);
                self.kind = Some(kind);
                self.doc = None;
                self.reload(cx);
            }
            Next::Reload => self.reload(cx),
            Next::Revert => {}
        }
    }

    /// A freshly read file replaces the buffer.
    pub fn loaded(&mut self, doc: CodexFileDoc) {
        self.original = doc.text().unwrap_or_default();
        self.masker = Masker::new();
        self.revealed = false;
        self.kind = Some(doc.kind);
        self.target = Some(doc.target.clone());
        self.doc = Some(doc);
        let o = self.original.clone();
        self.buffer = self.view_of(&o);
        self.checked = None;
        self.reason = None;
        self.force = false;
        self.format_error = None;
        self.recheck();
    }

    /// Check the buffer again when it changed.
    pub fn recheck(&mut self) {
        let h = hash(&self.buffer, self.revealed);
        if self.checked == Some(h) {
            return;
        }
        self.checked = Some(h);
        let Some(doc) = &self.doc else {
            self.problems.clear();
            return;
        };
        let owner = doc.profile.as_ref().map(ToString::to_string);
        self.problems = codex_files::validate(
            doc.kind,
            &self.buffer,
            &CheckContext {
                schema: Some(&doc.schema),
                owned: &doc.owned,
                owner: owner.as_deref(),
                masker: self.masked().then_some(&self.masker),
            },
        );
    }

    /// Show or hide the secrets of `auth.json`.
    pub fn toggle_secrets(&mut self) {
        if self.kind() != CodexFileKind::AuthJson {
            return;
        }
        if self.revealed {
            self.revealed = false;
            let b = self.buffer.clone();
            self.buffer = self.masker.mask(&b);
        } else {
            self.buffer = self.masker.unmask(&self.buffer);
            self.revealed = true;
        }
        self.recheck();
    }

    /// Tidy the buffer (comments kept); refused with a syntax error.
    pub fn format(&mut self) {
        match codex_files::format(self.kind(), &self.buffer) {
            Ok(t) => {
                self.buffer = t;
                self.format_error = None;
                self.recheck();
            }
            Err(p) => {
                self.format_error = Some(format!("{}:{}", p.line, p.column));
                self.jump = Some(p.start);
            }
        }
    }

    /// Errors that keep the file from being saved.
    pub fn blocking(&self) -> usize {
        self.problems.iter().filter(|p| p.blocks_save()).count()
    }

    /// Errors that need "Save anyway".
    pub fn forced(&self) -> usize {
        self.problems.iter().filter(|p| p.needs_force()).count()
    }

    /// Warnings.
    pub fn warnings(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == superba_core::ProblemSeverity::Warning)
            .count()
    }

    /// The managed keys and the credential this save changes (GH#5).
    pub fn ownership_changes(&self) -> (Vec<KeyChange>, Option<superba_core::AuthDrift>) {
        let Some(doc) = &self.doc else {
            return (Vec::new(), None);
        };
        let new = self.real_text();
        match doc.kind {
            CodexFileKind::ConfigToml => (
                codex_files::owned_changes(&self.original, &new, &doc.owned),
                None,
            ),
            CodexFileKind::AuthJson => (
                Vec::new(),
                doc.owns_credential
                    .then(|| {
                        codex_files::credential_change(
                            doc.contents.as_ref().map(SecretBytes::expose),
                            Some(new.as_bytes()),
                        )
                    })
                    .flatten(),
            ),
        }
    }

    /// Open the review (only with changes and without blocking errors).
    pub fn review(&mut self) {
        if self.dirty() && self.blocking() == 0 && self.doc.is_some() {
            self.recheck();
            self.save_error = None;
            self.force = false;
            self.dialog = Some(Dialog::Review);
        }
    }

    /// Save the buffer (from the review).
    pub fn save(&mut self, cx: &Cx<'_>) {
        let Some(doc) = &self.doc else {
            return;
        };
        if self.saving.is_some() || (self.forced() > 0 && !self.force) {
            return;
        }
        let save = CodexFileSave {
            target: doc.target.clone(),
            kind: doc.kind,
            contents: SecretBytes::new(self.real_text().into_bytes()),
            base_sha256: doc.sha256.clone(),
            reason: self.reason.clone().unwrap_or(SaveReason::Edit),
            force: self.force,
        };
        let service = cx.service.clone();
        self.save_error = None;
        self.saving = Some(crate::runtime::task(async move {
            service.save_codex_file(save).await
        }));
    }

    /// Open the Backups dialog and read the list.
    pub fn open_backups(&mut self, cx: &Cx<'_>) {
        self.dialog = Some(Dialog::Backups);
        self.preview = None;
        self.backups_error = None;
        let service = cx.service.clone();
        let query = CodexFileBackupQuery {
            target: Some(self.target()),
            kind: Some(self.kind()),
        };
        self.backups_task = Some(crate::runtime::task(async move {
            service.codex_file_backups(query).await
        }));
    }

    /// Read one backup for its diff.
    pub fn preview_backup(&mut self, cx: &Cx<'_>, id: &str) {
        let service = cx.service.clone();
        let id = id.to_owned();
        self.backups_error = None;
        self.backup_task = Some(crate::runtime::task(async move {
            service.codex_file_backup(&id).await
        }));
    }

    /// Put the previewed backup into the editor and review saving it.
    pub fn restore_preview(&mut self) {
        let Some((backup, text)) = self.preview.take() else {
            return;
        };
        self.buffer = self.view_of(&text);
        self.reason = Some(SaveReason::Restore {
            backup: backup.id.clone(),
        });
        self.recheck();
        self.dialog = None;
        if self.dirty() {
            self.review();
        }
    }

    /// The previewed backup as the buffer would show it.
    pub fn preview_view(&mut self) -> Option<String> {
        let text = self.preview.as_ref()?.1.clone();
        Some(self.view_of(&text))
    }

    /// Finish background work; true while any runs.
    pub fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        if let Some(t) = &self.load
            && let Some(r) = t.poll()
        {
            self.load = None;
            match r {
                Ok(doc) => {
                    self.load_error = None;
                    self.loaded(doc);
                }
                Err(e) => self.load_error = Some(crate::pages::error_text(cx.tr, &e)),
            }
        }
        if let Some(t) = &self.saving
            && let Some(r) = t.poll()
        {
            self.saving = None;
            match r {
                Ok(saved) => {
                    self.dialog = None;
                    self.original = self.real_text();
                    self.reason = None;
                    if let Some(doc) = &mut self.doc {
                        doc.sha256 = Some(saved.sha256.clone());
                        doc.contents = Some(SecretBytes::new(self.original.clone().into_bytes()));
                    }
                    let file = saved.kind.file_name();
                    out.push(Action::Toast {
                        status: Status::Ok,
                        text: match &saved.backup {
                            Some(_) => cx
                                .tr
                                .f("codex_editor.toast.saved_backup", &[("file", &file)]),
                            None => cx.tr.f("codex_editor.toast.saved", &[("file", &file)]),
                        },
                    });
                    self.saved = Some(saved);
                    out.push(Action::Refresh);
                    // Read the file back (new hash, owned keys).
                    self.reload(cx);
                }
                Err(e) => self.save_error = Some(crate::pages::error_text(cx.tr, &e)),
            }
        }
        if let Some(t) = &self.backups_task
            && let Some(r) = t.poll()
        {
            self.backups_task = None;
            match r {
                Ok(list) => self.backups = Some(list),
                Err(e) => self.backups_error = Some(crate::pages::error_text(cx.tr, &e)),
            }
        }
        if let Some(t) = &self.backup_task
            && let Some(r) = t.poll()
        {
            self.backup_task = None;
            match r {
                Ok(b) => match superba_core::codex_files::text_of(b.contents.expose()) {
                    Some(text) => self.preview = Some((b.backup, text)),
                    None => self.backups_error = Some(cx.tr.t("codex_editor.not_utf8")),
                },
                Err(e) => self.backups_error = Some(crate::pages::error_text(cx.tr, &e)),
            }
        }
        self.recheck();
        self.load.is_some()
            || self.saving.is_some()
            || self.backups_task.is_some()
            || self.backup_task.is_some()
    }

    /// Reading the backup list or one backup runs.
    pub fn backups_loading(&self) -> bool {
        self.backups_task.is_some() || self.backup_task.is_some()
    }

    /// The file is being read.
    pub fn loading(&self) -> bool {
        self.load.is_some()
    }
}
