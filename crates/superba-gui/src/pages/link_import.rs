//! "Import from the Krill website" (feature 140): the confirmation dialog of
//! a `superba://import/v1` link.
//!
//! A link only carries an API key (and a display name); it always means the
//! Krill provider. Before anything is written the dialog shows the masked
//! key, the Krill line the provider will use and what changes: the existing
//! Krill provider's key is updated (its lines stay), or a new Krill provider
//! is added on the line the setup wizard would pick (the fastest reachable
//! line probed, else the default line). Cancel writes nothing; confirming
//! imports through [`superba_core::SwitchService::import_link`] and, when
//! ticked, switches Codex to the provider through the normal switch flow.
//!
//! The key never leaves this dialog's memory except into that call: it is not
//! logged, not put in a toast or the activity log, and dropped with the
//! dialog.

use egui::{Align, Layout, Ui};
use superba_core::catalog::KRILL_SITE_ID;
use superba_core::{
    ConflictAction, ImportLink, ImportOutcome, LineId, LinkError, LinkImportReport,
    LinkImportRequest, ProfileId, SiteId,
};

use super::{Action, Cx, error_text};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Segmented, Status, StatusDot, Text, dialog};
use crate::runtime::ReadTask;

/// What the dialog wants its host to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkImportOutcome {
    /// Keep showing.
    Continue,
    /// Closed; nothing was written.
    Close,
    /// Imported `profile`; `switch` asks to switch Codex to it now.
    Done {
        /// The new or updated provider.
        profile: ProfileId,
        /// Its name.
        name: String,
        /// Switch to it now.
        switch: bool,
    },
}

/// State of the dialog.
pub struct LinkImportDialog {
    /// The link, or why it was refused.
    pub link: Result<ImportLink, LinkError>,
    preview_task: Option<ReadTask<LinkImportReport>>,
    preview_for: ConflictAction,
    /// The dry run for the current choice.
    pub preview: Option<superba_core::Result<LinkImportReport>>,
    /// Update the existing Krill provider (default) or add a new one.
    pub choice: ConflictAction,
    /// Switch Codex to the provider once imported.
    pub switch_now: bool,
    /// A new provider's line, once the user or the probe picked one.
    pub line: Option<LineId>,
    probe_requested: bool,
    apply_task: Option<ReadTask<LinkImportReport>>,
    /// Why the import failed.
    pub error: Option<String>,
}

impl std::fmt::Debug for LinkImportDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkImportDialog")
            .field("link", &self.link)
            .field("choice", &self.choice)
            .field("previewing", &self.preview_task.is_some())
            .field("importing", &self.apply_task.is_some())
            .finish_non_exhaustive()
    }
}

fn krill() -> SiteId {
    SiteId::new(KRILL_SITE_ID)
}

impl LinkImportDialog {
    /// Open the dialog for `link` and start its preview (a dry run).
    pub fn open(cx: &Cx<'_>, link: Result<ImportLink, LinkError>) -> Self {
        let mut d = Self {
            link,
            preview_task: None,
            preview_for: ConflictAction::Update,
            preview: None,
            choice: ConflictAction::Update,
            switch_now: true,
            line: None,
            probe_requested: false,
            apply_task: None,
            error: None,
        };
        d.start_preview(cx);
        d
    }

    /// Show `report` as the preview (tests, screenshots of fixed data).
    pub fn set_preview(&mut self, report: superba_core::Result<LinkImportReport>) {
        self.preview_task = None;
        self.preview = Some(report);
    }

    fn request(&self, dry_run: bool) -> Option<LinkImportRequest> {
        let link = self.link.as_ref().ok()?.clone();
        Some(LinkImportRequest {
            link,
            line: self.line.clone(),
            on_conflict: self.choice,
            dry_run,
        })
    }

    fn start_preview(&mut self, cx: &Cx<'_>) {
        let Some(request) = self.request(true) else {
            return;
        };
        self.preview_for = self.choice;
        let service = cx.service.clone();
        self.preview_task = Some(crate::runtime::task(async move {
            service.import_link(request).await
        }));
    }

    /// Import (confirm).
    pub fn start_import(&mut self, cx: &Cx<'_>) {
        if self.apply_task.is_some() {
            return;
        }
        let Some(request) = self.request(false) else {
            return;
        };
        self.error = None;
        let service = cx.service.clone();
        self.apply_task = Some(crate::runtime::task(async move {
            service.import_link(request).await
        }));
    }

    /// The import is running (the dialog cannot be replaced or closed).
    pub fn importing(&self) -> bool {
        self.apply_task.is_some()
    }

    /// Work is running.
    pub fn busy(&self) -> bool {
        self.preview_task.is_some() || self.apply_task.is_some()
    }

    /// The existing Krill provider (from the preview).
    fn existing(&self) -> Option<&superba_core::ImportConflict> {
        self.preview
            .as_ref()
            .and_then(|p| p.as_ref().ok())
            .and_then(|p| p.existing.as_ref())
    }

    fn updating(&self) -> bool {
        self.existing().is_some() && self.choice == ConflictAction::Update
    }

    /// Poll the tasks; `Some` when the import finished.
    fn poll(&mut self, cx: &Cx<'_>) -> Option<LinkImportOutcome> {
        if let Some(task) = &self.preview_task
            && let Some(r) = task.poll()
        {
            self.preview_task = None;
            self.preview = Some(r);
            if self.preview_for != self.choice {
                self.start_preview(cx);
            }
        }
        let task = self.apply_task.as_ref()?;
        let result = task.poll()?;
        self.apply_task = None;
        match result {
            Ok(report) => match &report.item.outcome {
                ImportOutcome::Created {
                    profile,
                    profile_name,
                    ..
                }
                | ImportOutcome::Updated {
                    profile,
                    profile_name,
                    ..
                } => Some(LinkImportOutcome::Done {
                    profile: profile.clone(),
                    name: profile_name.clone(),
                    switch: self.switch_now,
                }),
                ImportOutcome::Skipped { reason } => {
                    self.error = Some(cx.localize(reason));
                    None
                }
                ImportOutcome::Failed { error } => {
                    self.error = Some(error.clone());
                    None
                }
                _ => None,
            },
            Err(e) => {
                self.error = Some(error_text(cx.tr, &e));
                None
            }
        }
    }

    /// The line the provider will use: an update's current line, else the
    /// line the setup wizard would pick (the fastest reachable line probed,
    /// else the default line). Kept in [`LinkImportDialog::line`] for a new
    /// provider so the import uses what the dialog showed.
    fn pick_line(&mut self, cx: &Cx<'_>) -> Option<LineId> {
        if self.updating() {
            let existing = self.existing()?;
            let profile = cx.data.profile(&existing.profile)?;
            return crate::pages::wizard::current_line(cx.data, profile).or_else(|| {
                cx.data
                    .site(&krill())
                    .and_then(|s| s.default_line().map(|l| l.id.clone()))
            });
        }
        let site = cx.data.site(&krill())?;
        let best = site
            .lines
            .iter()
            .filter_map(|l| {
                let caps = cx.data.site_caps(&site.id, &l.id)?;
                (caps.reachable && caps.codex_path.is_some())
                    .then_some((caps.latency_ms.unwrap_or(u32::MAX), l.id.clone()))
            })
            .min()
            .map(|(_, id)| id);
        let line = best.or_else(|| site.default_line().map(|l| l.id.clone()));
        if self.apply_task.is_none() {
            self.line = line.clone();
        }
        line
    }
}

/// The host's actions once the dialog imported: reload, then switch (when
/// ticked) through the normal flow, or offer it in a toast.
pub fn finished_actions(cx: &Cx<'_>, outcome: &LinkImportOutcome, out: &mut Vec<Action>) {
    let LinkImportOutcome::Done {
        profile,
        name,
        switch,
    } = outcome
    else {
        return;
    };
    out.push(Action::Refresh);
    if *switch {
        out.push(Action::Toast {
            status: Status::Ok,
            text: cx.tr.f("link_import.toast_switching", &[("name", name)]),
        });
        out.push(Action::Switch {
            profile: profile.clone(),
            line: None,
            auth: None,
        });
    } else {
        out.push(Action::OfferSwitch {
            text: cx.tr.f("link_import.toast_imported", &[("name", name)]),
            profile: profile.clone(),
        });
    }
}

/// The dialog as a modal window.
pub fn show_dialog(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    d: &mut LinkImportDialog,
    out: &mut Vec<Action>,
) -> LinkImportOutcome {
    let mut outcome = LinkImportOutcome::Continue;
    if let Some(done) = d.poll(cx) {
        return done;
    }
    if d.busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
    // Probe the Krill lines once, as the setup wizard does, so a new provider
    // gets the fastest line.
    if d.link.is_ok() && !d.probe_requested {
        d.probe_requested = true;
        if !cx.data.probing_site(&krill()) {
            out.push(Action::Probe(krill()));
        }
    }
    let response = dialog(ctx, egui::Id::unique("link-import"), 520.0, |ui| {
        outcome = body(ui, cx, d);
    });
    if outcome == LinkImportOutcome::Continue && response.should_close() && d.apply_task.is_none() {
        outcome = LinkImportOutcome::Close;
    }
    outcome
}

fn row(ui: &mut Ui, cx: &Cx<'_>, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, 18.0],
            egui::Label::new(Text::caption(label.to_owned(), cx.pal)),
        );
        ui.add(egui::Label::new(Text::body(value, cx.pal)).wrap());
    });
}

fn body(ui: &mut Ui, cx: &Cx<'_>, d: &mut LinkImportDialog) -> LinkImportOutcome {
    let mut outcome = LinkImportOutcome::Continue;
    ui.label(Text::h2(cx.tr.t("link_import.title"), cx.pal));
    let link = match &d.link {
        Ok(link) => link.clone(),
        Err(e) => {
            ui.label(Text::body(cx.localize(&e.text()), cx.pal).color(cx.pal.red));
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(Button::ghost(cx.tr.t("action.close")).kbd("Esc"))
                    .clicked()
                {
                    outcome = LinkImportOutcome::Close;
                }
            });
            return outcome;
        }
    };
    ui.label(Text::small(cx.tr.t("link_import.intro"), cx.pal));
    ui.add_space(4.0);
    row(
        ui,
        cx,
        &cx.tr.t("link_import.provider_label"),
        cx.tr.t("link_import.krill"),
    );
    row(ui, cx, &cx.tr.t("link_import.key_label"), link.key.masked());
    let line = d.pick_line(cx);
    let line_text = match &line {
        Some(id) => {
            let name = cx.line_name(id);
            if d.updating() {
                cx.tr.f("link_import.line_kept", &[("line", &name)])
            } else if cx.data.probing_site(&krill()) {
                cx.tr.f("link_import.line_probing", &[("line", &name)])
            } else {
                name
            }
        }
        None => cx.tr.t("link_import.line_default"),
    };
    row(ui, cx, &cx.tr.t("link_import.line_label"), line_text);
    match &d.preview {
        None => {
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Busy).pulse(true));
                ui.label(Text::muted(cx.tr.t("link_import.checking"), cx.pal));
            });
        }
        Some(Err(e)) => {
            ui.label(Text::small(error_text(cx.tr, e), cx.pal).color(cx.pal.red));
        }
        Some(Ok(report)) => {
            let report = report.clone();
            if let Some(existing) = &report.existing {
                ui.add_space(4.0);
                ui.label(Text::small(
                    cx.tr
                        .f("link_import.existing", &[("name", &existing.profile_name)]),
                    cx.pal,
                ));
                let before = d.choice;
                Segmented::new("link-import-choice", &mut d.choice)
                    .option(ConflictAction::Update, cx.tr.t("link_import.choice_update"))
                    .option(ConflictAction::Rename, cx.tr.t("link_import.choice_new"))
                    .small()
                    .show(ui);
                if d.choice != before && d.preview_task.is_none() {
                    d.start_preview(cx);
                }
            }
            let change = match &report.item.outcome {
                ImportOutcome::Updated { profile_name, .. } if d.updating() => cx
                    .tr
                    .f("link_import.change_update", &[("name", profile_name)]),
                ImportOutcome::Created { profile_name, .. } => {
                    cx.tr.f("link_import.change_new", &[("name", profile_name)])
                }
                ImportOutcome::Skipped { reason } => cx.localize(reason),
                ImportOutcome::Failed { error } => error.clone(),
                _ => String::new(),
            };
            row(ui, cx, &cx.tr.t("link_import.change_label"), change);
        }
    }
    ui.add_space(4.0);
    ui.checkbox(&mut d.switch_now, cx.tr.t("link_import.switch_now"));
    ui.label(Text::caption(cx.tr.t("link_import.warning"), cx.pal).color(cx.pal.amber));
    if let Some(e) = &d.error {
        ui.label(Text::small(e.clone(), cx.pal).color(cx.pal.red));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let importing = d.apply_task.is_some();
            let ready = matches!(d.preview, Some(Ok(_)))
                && d.preview_task.is_none()
                && d.preview_for == d.choice;
            // Enter imports when no control has the keyboard (a focused
            // checkbox or segment keeps Enter for itself).
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter))
                && ui.ctx().memory(|m| m.focused().is_none());
            if ui
                .add(
                    Button::primary(cx.tr.t("link_import.confirm"))
                        .icon(Icon::Key)
                        .kbd("Enter")
                        .loading(importing)
                        .enabled(ready && !importing),
                )
                .clicked()
                || (enter && ready && !importing)
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
                outcome = LinkImportOutcome::Close;
            }
        });
    });
    outcome
}
