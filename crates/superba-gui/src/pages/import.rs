//! "Import from other tools" (user request 37): providers that cc-switch and AI
//! Toolbox keep for Codex, shown per tool with checkboxes and what each becomes
//! in Superba (the Krill preset with its line, or a custom site with its lines
//! and model), whether its key comes along, and conflicts with providers Superba
//! already has (skip by default; rename or update when chosen). Importing saves
//! Superba providers and keys only: nothing is written to Codex and nothing
//! restarts; switching stays a separate step.
//!
//! The same dialog body serves the Providers page (Add provider → Import from
//! other tools, the command palette) and the setup wizard's provider step.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align, Layout, ScrollArea, Ui};
use superba_core::{
    ConflictAction, ImportApplyRequest, ImportCandidate, ImportKey, ImportOutcome, ImportReport,
    ImportScan, ImportScanRequest, ImportSelection, ImportSource, ProfileId, SiteKind, SourceScan,
    StoreStatus,
};

use super::{Action, Cx, error_text};
use crate::design::icons::Icon;
use crate::design::widgets::{Badge, Button, Segmented, Status, StatusDot, Text, dialog};
use crate::runtime::ReadTask;

/// What the dialog wants its host to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportOutcomeUi {
    /// Keep showing.
    Continue,
    /// Closed without importing anything.
    Close,
    /// Finished: `imported` providers were created or updated; `first` is the
    /// first of them (to select and offer a switch to).
    Done {
        /// Created or updated providers.
        imported: usize,
        /// The first one.
        first: Option<ProfileId>,
        /// Its name.
        first_name: Option<String>,
    },
}

/// State of the import dialog.
pub struct ImportDialog {
    scan_task: Option<ReadTask<ImportScan>>,
    /// The scan (`None` while it runs).
    pub scan: Option<superba_core::Result<ImportScan>>,
    /// Ticked candidates.
    pub ticked: BTreeSet<String>,
    /// Conflict choice per candidate (absent = skip).
    pub actions: BTreeMap<String, ConflictAction>,
    /// Sources whose not-importable list is expanded.
    pub expanded: BTreeSet<ImportSource>,
    apply_task: Option<ReadTask<ImportReport>>,
    /// The import's result.
    pub report: Option<ImportReport>,
    /// Why the import failed as a whole.
    pub error: Option<String>,
}

impl std::fmt::Debug for ImportDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportDialog")
            .field("scanning", &self.scan_task.is_some())
            .field("ticked", &self.ticked)
            .field("actions", &self.actions)
            .field("importing", &self.apply_task.is_some())
            .field("report", &self.report.is_some())
            .finish_non_exhaustive()
    }
}

impl ImportDialog {
    /// Open the dialog and start scanning every source.
    pub fn open(cx: &Cx<'_>) -> Self {
        let service = cx.service.clone();
        let task =
            crate::runtime::task(
                async move { service.import_scan(ImportScanRequest::default()).await },
            );
        let mut d = Self::empty();
        d.scan_task = Some(task);
        d
    }

    /// A dialog showing `scan` (tests, screenshots of fixed data).
    pub fn with_scan(scan: superba_core::Result<ImportScan>) -> Self {
        let mut d = Self::empty();
        d.set_scan(scan);
        d
    }

    fn empty() -> Self {
        Self {
            scan_task: None,
            scan: None,
            ticked: BTreeSet::new(),
            actions: BTreeMap::new(),
            expanded: BTreeSet::new(),
            apply_task: None,
            report: None,
            error: None,
        }
    }

    fn set_scan(&mut self, scan: superba_core::Result<ImportScan>) {
        if let Ok(s) = &scan {
            self.ticked = s
                .candidates()
                .filter(|c| c.preselected)
                .map(|c| c.id.clone())
                .collect();
        }
        self.scan = Some(scan);
    }

    /// Scanning or importing.
    pub fn busy(&self) -> bool {
        self.scan_task.is_some() || self.apply_task.is_some()
    }

    /// The conflict choice of `candidate` (skip unless chosen).
    pub fn action(&self, candidate: &str) -> ConflictAction {
        self.actions.get(candidate).copied().unwrap_or_default()
    }

    /// The selections an import sends: ticked importable candidates with their
    /// conflict choice.
    pub fn selections(&self) -> Vec<ImportSelection> {
        let Some(Ok(scan)) = &self.scan else {
            return Vec::new();
        };
        scan.candidates()
            .filter(|c| c.importable() && self.ticked.contains(&c.id))
            .map(|c| ImportSelection::new(c.id.clone(), self.action(&c.id)))
            .collect()
    }

    /// Selections that will create or update a provider (a conflict left on
    /// Skip imports nothing).
    pub fn effective_count(&self) -> usize {
        let Some(Ok(scan)) = &self.scan else {
            return 0;
        };
        self.selections()
            .iter()
            .filter(|s| {
                scan.candidate(&s.candidate)
                    .is_some_and(|c| c.conflict.is_none() || s.on_conflict != ConflictAction::Skip)
            })
            .count()
    }

    /// Start the import of the selections.
    pub fn start_import(&mut self, cx: &Cx<'_>) {
        if self.busy() {
            return;
        }
        let request = ImportApplyRequest {
            selections: self.selections(),
            dry_run: false,
        };
        if request.selections.is_empty() {
            return;
        }
        self.error = None;
        let service = cx.service.clone();
        self.apply_task = Some(crate::runtime::task(async move {
            service.import_apply(request).await
        }));
    }

    /// Set the import result (tests).
    pub fn set_report(&mut self, report: ImportReport) {
        self.report = Some(report);
    }

    /// Collect finished work; true while something runs.
    pub fn poll(&mut self, cx: &Cx<'_>) -> bool {
        if let Some(task) = &self.scan_task {
            match task.poll() {
                Some(r) => {
                    self.scan_task = None;
                    self.set_scan(r);
                }
                None => return true,
            }
        }
        if let Some(task) = &self.apply_task {
            match task.poll() {
                Some(Ok(report)) => {
                    self.apply_task = None;
                    self.report = Some(report);
                }
                Some(Err(e)) => {
                    self.apply_task = None;
                    self.error = Some(error_text(cx.tr, &e));
                }
                None => return true,
            }
        }
        false
    }

    fn outcome_done(&self) -> ImportOutcomeUi {
        let Some(report) = &self.report else {
            return ImportOutcomeUi::Close;
        };
        let first = report.items.iter().find_map(|i| match &i.outcome {
            ImportOutcome::Created {
                profile,
                profile_name,
                ..
            }
            | ImportOutcome::Updated {
                profile,
                profile_name,
                ..
            } => Some((profile.clone(), profile_name.clone())),
            _ => None,
        });
        ImportOutcomeUi::Done {
            imported: report.imported(),
            first_name: first.as_ref().map(|f| f.1.clone()),
            first: first.map(|f| f.0),
        }
    }
}

/// The actions a host pushes when the dialog finished with imports: reload,
/// and a toast that says nothing was written to Codex and offers a switch to the
/// first imported provider.
pub fn finished_actions(cx: &Cx<'_>, outcome: &ImportOutcomeUi, out: &mut Vec<Action>) {
    let ImportOutcomeUi::Done {
        imported,
        first,
        first_name,
    } = outcome
    else {
        return;
    };
    out.push(Action::Refresh);
    if *imported == 0 {
        return;
    }
    let text = cx.tr.f("import_dialog.toast", &[("n", imported)]);
    match (first, first_name) {
        (Some(profile), Some(_)) => out.push(Action::OfferSwitch {
            text,
            profile: profile.clone(),
        }),
        _ => out.push(Action::Toast {
            status: Status::Ok,
            text,
        }),
    }
}

/// The dialog as a modal window (Providers page).
pub fn show_dialog(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    d: &mut ImportDialog,
    out: &mut Vec<Action>,
) -> ImportOutcomeUi {
    let mut outcome = ImportOutcomeUi::Continue;
    // The list takes what the window leaves besides the title, intro and footer.
    let list_height = (ctx.content_rect().height() - 300.0).max(200.0);
    let response = dialog(ctx, egui::Id::unique("providers-import"), 720.0, |ui| {
        outcome = body(ui, cx, d, out, list_height);
    });
    if outcome == ImportOutcomeUi::Continue && response.should_close() && !d.busy() {
        outcome = if d.report.is_some() {
            d.outcome_done()
        } else {
            ImportOutcomeUi::Close
        };
    }
    outcome
}

/// The dialog's content (title, sources, footer) in `ui`; `list_height` caps the
/// scrolling list.
pub fn body(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &mut ImportDialog,
    out: &mut Vec<Action>,
    list_height: f32,
) -> ImportOutcomeUi {
    let _ = out;
    // Keep painting while work runs, and once more after it finished so the
    // list lays out at its full size.
    let was_busy = d.busy();
    if d.poll(cx) || was_busy {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    ui.label(Text::h2(cx.tr.t("import_dialog.title"), cx.pal));
    ui.label(Text::small(cx.tr.t("import_dialog.intro"), cx.pal));
    ui.add_space(6.0);
    if d.report.is_some() {
        return report_view(ui, cx, d, list_height);
    }
    let mut outcome = ImportOutcomeUi::Continue;
    match &d.scan {
        None => {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Busy).pulse(true));
                ui.label(Text::muted(cx.tr.t("import_dialog.scanning"), cx.pal));
            });
            ui.add_space(16.0);
        }
        Some(Err(e)) => {
            ui.label(Text::body(error_text(cx.tr, e), cx.pal).color(cx.pal.red));
        }
        Some(Ok(scan)) => {
            let scan = scan.clone();
            ScrollArea::vertical()
                .id_salt("import-list")
                .max_height(list_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, source) in scan.sources.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(10.0);
                        }
                        source_section(ui, cx, d, source);
                    }
                });
        }
    }
    if let Some(e) = &d.error {
        ui.label(Text::small(e.clone(), cx.pal).color(cx.pal.red));
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let n = d.effective_count();
        let selected = d.selections().len();
        if d.scan.as_ref().is_some_and(|s| s.is_ok()) {
            ui.label(Text::small(
                cx.tr.f("import_dialog.selected", &[("n", &selected)]),
                cx.pal,
            ));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let importing = d.apply_task.is_some();
            if ui
                .add(
                    Button::primary(cx.tr.f("import_dialog.button", &[("n", &n)]))
                        .icon(Icon::Download)
                        .loading(importing)
                        .enabled(n > 0 && !d.busy()),
                )
                .clicked()
            {
                d.start_import(cx);
            }
            if ui
                .add(
                    Button::ghost(cx.tr.t("action.cancel"))
                        .kbd("Esc")
                        .enabled(!importing),
                )
                .clicked()
            {
                outcome = ImportOutcomeUi::Close;
            }
        });
    });
    outcome
}

fn store_line(cx: &Cx<'_>, source: &SourceScan) -> String {
    let read: Vec<_> = source
        .stores
        .iter()
        .filter(|s| s.status == StoreStatus::Read)
        .collect();
    if !read.is_empty() {
        return read
            .iter()
            .map(|s| format!("{} · {}", s.display_path, s.format))
            .collect::<Vec<_>>()
            .join("; ");
    }
    if let Some(skipped) = source
        .stores
        .iter()
        .find(|s| s.status == StoreStatus::Skipped)
    {
        let why = skipped
            .note
            .as_ref()
            .map(|r| cx.localize(r))
            .unwrap_or_default();
        return cx.tr.f(
            "import_dialog.source.skipped",
            &[("path", &skipped.display_path), ("reason", &why)],
        );
    }
    cx.tr.t("import_dialog.source.missing")
}

fn source_section(ui: &mut Ui, cx: &Cx<'_>, d: &mut ImportDialog, source: &SourceScan) {
    let importable: Vec<&ImportCandidate> = source
        .candidates
        .iter()
        .filter(|c| c.importable())
        .collect();
    let others: Vec<&ImportCandidate> = source
        .candidates
        .iter()
        .filter(|c| !c.importable())
        .collect();
    ui.horizontal(|ui| {
        ui.label(Text::title(source.source.name(), cx.pal));
        if source.found() {
            ui.add(Badge::new(
                cx.tr
                    .f("import_dialog.source.count", &[("n", &importable.len())]),
            ));
        }
    });
    ui.add(egui::Label::new(Text::caption(store_line(cx, source), cx.pal)).truncate());
    // Stores that were found but not read, besides the one that was.
    for store in source.stores.iter().filter(|s| {
        s.status == StoreStatus::Superseded
            || (s.status == StoreStatus::Skipped
                && source.stores.iter().any(|x| x.status == StoreStatus::Read))
    }) {
        let why = store
            .note
            .as_ref()
            .map(|r| cx.localize(r))
            .unwrap_or_default();
        ui.add(
            egui::Label::new(Text::caption(
                cx.tr.f(
                    "import_dialog.source.other_store",
                    &[("path", &store.display_path), ("reason", &why)],
                ),
                cx.pal,
            ))
            .truncate(),
        );
    }
    if source.found() && importable.is_empty() {
        ui.label(Text::small(cx.tr.t("import_dialog.source.none"), cx.pal));
    }
    for c in importable {
        ui.add_space(6.0);
        candidate_row(ui, cx, d, c);
    }
    if !others.is_empty() {
        ui.add_space(6.0);
        let open = d.expanded.contains(&source.source);
        let label = cx.tr.f("import_dialog.others", &[("n", &others.len())]);
        let icon = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        if ui.add(Button::ghost(label).icon(icon).small()).clicked() {
            if open {
                d.expanded.remove(&source.source);
            } else {
                d.expanded.insert(source.source);
            }
        }
        if open {
            for c in others {
                let reason = c
                    .not_importable_reason()
                    .map(|r| cx.localize(r))
                    .unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(26.0);
                    ui.label(Text::small(c.name.clone(), cx.pal).color(cx.pal.text_muted));
                    ui.label(Text::caption(
                        format!("{} · {reason}", c.app.label()),
                        cx.pal,
                    ));
                });
            }
        }
    }
}

/// "Krill · Overseas" or "Custom site · api.example.com".
pub fn target_text(cx: &Cx<'_>, c: &ImportCandidate) -> String {
    let Some(t) = &c.target else {
        return String::new();
    };
    match t.kind {
        SiteKind::Krill => {
            let lines: Vec<String> = t.lines.iter().map(|l| cx.tr.text(&l.name)).collect();
            cx.tr.f(
                "import_dialog.target.krill",
                &[("lines", &lines.join(", "))],
            )
        }
        _ => {
            let host = t
                .base_url
                .as_ref()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            let mut s = cx.tr.f("import_dialog.target.site", &[("host", &host)]);
            if t.lines.len() > 1 {
                s.push_str(" · ");
                s.push_str(
                    &cx.tr
                        .f("import_dialog.target.lines", &[("n", &t.lines.len())]),
                );
            }
            if !t.model_map.is_empty() {
                s.push_str(" · ");
                s.push_str(
                    &cx.tr
                        .f("import_dialog.target.map", &[("n", &t.model_map.len())]),
                );
            }
            s
        }
    }
}

/// "Key ••••abcd" / "Key in the keychain (not imported)" / ...
pub fn key_text(cx: &Cx<'_>, key: &ImportKey) -> (String, bool) {
    match key {
        ImportKey::Present { masked } => (
            cx.tr.f("import_dialog.key.present", &[("masked", masked)]),
            true,
        ),
        ImportKey::Keychain => (cx.tr.t("import_dialog.key.keychain"), false),
        ImportKey::EnvVar { name } => (cx.tr.f("import_dialog.key.env", &[("name", name)]), false),
        _ => (cx.tr.t("import_dialog.key.missing"), false),
    }
}

fn candidate_row(ui: &mut Ui, cx: &Cx<'_>, d: &mut ImportDialog, c: &ImportCandidate) {
    let mut on = d.ticked.contains(&c.id);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut on, "").changed() {
            if on {
                d.ticked.insert(c.id.clone());
            } else {
                d.ticked.remove(&c.id);
            }
        }
        ui.label(Text::strong(c.name.clone(), cx.pal));
        if c.current {
            ui.add(Badge::new(cx.tr.t("import_dialog.current")).outline());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(Text::small(target_text(cx, c), cx.pal)).truncate());
            ui.label(Text::caption(cx.tr.t("import_dialog.target.to"), cx.pal));
        });
    });
    let indent = 26.0;
    let mut detail: Vec<String> = Vec::new();
    if let Some(url) = &c.base_url {
        detail.push(url.clone());
    }
    if let Some(model) = &c.model {
        detail.push(cx.tr.f("import_dialog.model", &[("model", model)]));
    }
    let (key, has_key) = key_text(cx, &c.key);
    ui.horizontal(|ui| {
        ui.add_space(indent);
        if !detail.is_empty() {
            ui.add(
                egui::Label::new(Text::mono_muted(detail.join(" · "), cx.pal).size(11.5))
                    .truncate(),
            );
            ui.label(Text::caption("·", cx.pal));
        }
        let color = if has_key {
            cx.pal.text_muted
        } else {
            cx.pal.amber
        };
        ui.label(Text::small(key, cx.pal).color(color));
    });
    if let Some(conflict) = &c.conflict {
        ui.horizontal(|ui| {
            ui.add_space(indent);
            let what = match (conflict.same_name, conflict.same_base_url) {
                (true, true) => "import_dialog.conflict.both",
                (true, false) => "import_dialog.conflict.name",
                _ => "import_dialog.conflict.url",
            };
            ui.label(
                Text::small(cx.tr.f(what, &[("name", &conflict.profile_name)]), cx.pal)
                    .color(cx.pal.amber),
            );
            let mut action = d.action(&c.id);
            let rename_tip = c
                .target
                .as_ref()
                .map(|t| {
                    cx.tr.f(
                        "import_dialog.conflict.rename_tip",
                        &[("name", &t.profile_name)],
                    )
                })
                .unwrap_or_default();
            let before = action;
            Segmented::new(("import-conflict", c.id.clone()), &mut action)
                .option_with_tooltip(
                    ConflictAction::Skip,
                    cx.tr.t("import_dialog.conflict.skip"),
                    cx.tr.t("import_dialog.conflict.skip_tip"),
                )
                .option_with_tooltip(
                    ConflictAction::Rename,
                    cx.tr.t("import_dialog.conflict.rename"),
                    rename_tip,
                )
                .option_with_tooltip(
                    ConflictAction::Update,
                    cx.tr.t("import_dialog.conflict.update"),
                    cx.tr.t("import_dialog.conflict.update_tip"),
                )
                .small()
                .show(ui);
            if action != before {
                d.actions.insert(c.id.clone(), action);
                // Choosing what to do with a conflict means importing it.
                if action != ConflictAction::Skip {
                    d.ticked.insert(c.id.clone());
                }
            }
        });
    }
    for note in &c.notes {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(indent);
            ui.label(Text::caption(cx.localize(note), cx.pal));
        });
    }
}

fn outcome_text(cx: &Cx<'_>, o: &ImportOutcome) -> (Status, String) {
    match o {
        ImportOutcome::Created {
            profile_name,
            key_stored,
            ..
        } => (
            Status::Ok,
            cx.tr.f(
                if *key_stored {
                    "import_dialog.result.created"
                } else {
                    "import_dialog.result.created_no_key"
                },
                &[("name", profile_name)],
            ),
        ),
        ImportOutcome::Updated {
            profile_name,
            key_stored,
            ..
        } => (
            Status::Ok,
            cx.tr.f(
                if *key_stored {
                    "import_dialog.result.updated"
                } else {
                    "import_dialog.result.updated_no_key"
                },
                &[("name", profile_name)],
            ),
        ),
        ImportOutcome::Skipped { reason } => (
            Status::Idle,
            cx.tr.f(
                "import_dialog.result.skipped",
                &[("reason", &cx.localize(reason))],
            ),
        ),
        ImportOutcome::Failed { error } => (
            Status::Error,
            cx.tr.f("import_dialog.result.failed", &[("error", error)]),
        ),
        _ => (Status::Idle, String::new()),
    }
}

fn report_view(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &mut ImportDialog,
    list_height: f32,
) -> ImportOutcomeUi {
    let mut outcome = ImportOutcomeUi::Continue;
    let Some(report) = d.report.clone() else {
        return outcome;
    };
    ui.label(Text::body(
        cx.tr
            .f("import_dialog.result.summary", &[("n", &report.imported())]),
        cx.pal,
    ));
    ui.label(Text::small(
        cx.tr.t("import_dialog.result.not_switched"),
        cx.pal,
    ));
    ui.add_space(6.0);
    ScrollArea::vertical()
        .id_salt("import-report")
        .max_height(list_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for item in &report.items {
                let (status, text) = outcome_text(cx, &item.outcome);
                ui.horizontal(|ui| {
                    ui.add(StatusDot::new(status));
                    ui.label(Text::strong(item.name.clone(), cx.pal));
                    ui.label(Text::caption(item.source.name(), cx.pal));
                });
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(16.0);
                    ui.label(Text::small(text, cx.pal));
                });
            }
        });
    ui.add_space(10.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui
            .add(Button::primary(cx.tr.t("import_dialog.finish")).kbd("Enter"))
            .clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Enter))
        {
            outcome = d.outcome_done();
        }
    });
    outcome
}
