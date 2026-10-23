//! Codex settings of this computer (UX §3.6), driven by the settings catalog:
//! three fixed panes — a rail (Common, Changed, categories with counts), a paged
//! list of Chinese-first two-line rows, and a detail pane (names, badges with
//! availability per local Codex, the Chinese explanation and the official
//! English text, the editor, default / current / recommended values, what each
//! connected remote host would skip when syncing, reset) — and a pending bar that
//! saves the edits into the provider's profile and hands them to the switch flow
//! (preview, then apply with a hot restart). Remote hosts get settings only
//! through Remote hosts → Sync settings; there is no target filter here.
//!
//! Choosing another provider shows it in the picker at once and a loading state
//! instead of the previous provider's settings until its model is built (user
//! bug 83); only the latest choice's model is used, and a failure or a timeout
//! is shown in place with Retry ([`super::shared::loading`]).
//!
//! Commands: `focus:<config path>` (select a setting, widening the filter),
//! `profile:<id>` (edit another provider's settings), `migrate` (queue every
//! legacy migration), `search` (Ctrl/Cmd+F), `save` (Ctrl/Cmd+S), `files`,
//! `files:config|auth[@<target>]` ("Edit Codex files", [`files`]: Codex's own
//! `config.toml` / `auth.json` edited by hand, GitHub issue 9),
//! `files-backups:config|auth` (the same, with its Backups dialog open).

mod detail;
pub mod editor;
pub(crate) use rail::item as rail_item;
pub mod files;
pub mod files_highlight;
pub mod files_view;
pub mod model;
mod news;
mod rail;
pub mod source;
mod view;

use egui::Ui;

use self::model::{Change, Filter, Model, Pending, Rail};
use super::shared::loading::Latest;
use super::{Action, Cx, Page, PageStatus, page_header};
use crate::design::widgets::PagedState;
use crate::runtime::ReadTask;

/// The longest the model may take to build (catalog, `codex features list`
/// up to 10 s) before the page stops waiting.
pub const LOAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Builds the model of a profile (`None` = the applied one) in the
/// background: [`source::load_task`] unless a test or another settings page
/// gives its own.
pub type Loader = std::sync::Arc<
    dyn Fn(&Cx<'_>, Option<superba_core::ProfileId>) -> ReadTask<Model> + Send + Sync,
>;

/// Page-local state, kept across navigation.
pub struct State {
    /// Loaded model, of the chosen provider only (`None` while another
    /// provider's model is built).
    model: Option<Model>,
    /// Building the model of the chosen provider ([`State::profile`]); a
    /// failure of a reload keeps the model visible.
    load: Latest<Option<superba_core::ProfileId>, Model>,
    /// What builds the model.
    loader: Loader,
    /// Overview stamp the model was loaded for (reload after a switch).
    loaded_for: Option<superba_core::Timestamp>,
    /// List filter (rail, search, Advanced).
    pub filter: Filter,
    /// Filter of the last frame (a change selects the first row).
    shown_filter: Option<Filter>,
    /// Edits not yet handed to the switch flow.
    pub pending: Pending,
    /// Text buffers of the number / text editors, keyed by path.
    buffers: std::collections::HashMap<String, String>,
    /// Rejected input per path (i18n key of the reason).
    invalid: std::collections::HashMap<String, &'static str>,
    /// Select this path on the next frame (palette, migration notice).
    focus: Option<String>,
    /// Paging and selection of the list.
    pub table: PagedState,
    /// Profile chosen in the provider picker (`None` = the applied one).
    profile: Option<superba_core::ProfileId>,
    /// Saving the pending edits into the profile.
    saving: Option<ReadTask<source::AfterSave>>,
    /// Last save error.
    save_error: Option<String>,
    /// Profile set the model was built from (reload when it changes).
    loaded_profiles: Option<superba_core::ProfileSet>,
    /// The diff dialog is open.
    pub diff_open: bool,
    /// The detail sheet is open (narrow windows).
    pub sheet_open: bool,
    /// Path whose full explanation is shown in a dialog.
    pub expanded: Option<String>,
    /// Focus the search field on the next frame.
    focus_search: bool,
    /// The large text editor, when open.
    editor: Option<editor::TextEditor>,
    /// Row buffers of the list and map editors, keyed by path.
    rows: std::collections::HashMap<String, editor::Rows>,
    /// Path whose list / map rows are open in a dialog.
    pub rows_dialog: Option<String>,
    /// "Edit Codex files" (GitHub issue 9): when open it replaces the list.
    pub files: files::Editor,
}

impl Default for State {
    fn default() -> Self {
        Self {
            model: None,
            load: Latest::default(),
            loader: std::sync::Arc::new(source::load_task),
            loaded_for: None,
            filter: Filter {
                rail: Rail::Common,
                ..Filter::default()
            },
            shown_filter: None,
            pending: Pending::default(),
            buffers: Default::default(),
            invalid: Default::default(),
            focus: None,
            table: PagedState::default(),
            profile: None,
            saving: None,
            save_error: None,
            loaded_profiles: None,
            diff_open: false,
            sheet_open: false,
            expanded: None,
            focus_search: false,
            editor: None,
            rows: Default::default(),
            rows_dialog: None,
            files: files::Editor::default(),
        }
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("settings::State")
            .field("entries", &self.model.as_ref().map(|m| m.entries.len()))
            .field("filter", &self.filter)
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl State {
    /// The loaded model, if any.
    pub fn model(&self) -> Option<&Model> {
        self.model.as_ref()
    }

    /// Replace the model (tests and the loader); pending edits that the new model
    /// already contains are dropped.
    pub fn set_model(&mut self, model: Model) {
        self.pending.rebase(&model);
        // Without a curated Common list (an older catalog) start at the first
        // category instead of an empty list.
        if self.filter.rail == Rail::Common && model.entries.iter().all(|e| e.common.is_none()) {
            self.filter.rail = model
                .categories
                .first()
                .map(|c| Rail::Category(c.id.clone()))
                .unwrap_or_default();
        }
        self.model = Some(model);
    }

    /// Build the model with `loader` instead of [`source::load_task`].
    pub fn set_loader(&mut self, loader: Loader) {
        self.loader = loader;
    }

    /// The chosen provider (`None` = the applied one, else the first).
    pub fn chosen_profile(&self) -> Option<&superba_core::ProfileId> {
        self.profile.as_ref()
    }

    /// The chosen provider's model is being built and none is shown yet.
    pub fn switching(&self) -> bool {
        self.model.is_none() && self.load.loading()
    }

    /// Why the chosen provider's model could not be built.
    pub fn load_failure(&self) -> Option<&super::shared::loading::Failure> {
        self.load.failure()
    }

    /// Record a pending edit for `path` (no-op for unknown paths).
    pub fn edit(&mut self, path: &str, change: Change) {
        if let Some(model) = &self.model {
            self.pending.set_path(model, path, change);
        }
    }

    /// The rows the list shows now.
    pub fn rows(&self) -> Vec<&model::Entry> {
        match &self.model {
            Some(m) => m.listed(&self.filter, &self.pending),
            None => Vec::new(),
        }
    }

    /// The selected entry.
    pub fn selected(&self) -> Option<&model::Entry> {
        let rows = self.rows();
        self.table.selected.and_then(|i| rows.get(i).copied())
    }

    /// Queue the migration of every legacy key set in the configuration.
    pub fn migrate_all(&mut self) {
        let Some(model) = &self.model else {
            return;
        };
        let changes: Vec<_> = model
            .migrations()
            .flat_map(|e| model.migration_changes(e))
            .collect();
        for (path, change) in changes {
            self.pending.set_path(model, &path, change);
        }
    }

    /// Select `path` (widening the filter when it is hidden).
    pub fn focus(&mut self, path: &str) {
        if let Some(model) = &self.model
            && let Some(entry) = model.entry(path)
            && !self.filter.accepts(entry, &self.pending)
        {
            self.filter = Filter {
                advanced: self.filter.advanced || !entry.stage.in_default_view(),
                rail: Rail::Category(entry.category.clone()),
                query: String::new(),
            };
            if !self.filter.accepts(entry, &self.pending) {
                self.filter.rail = Rail::All;
                self.filter.advanced = true;
            }
        }
        self.focus = Some(path.to_owned());
        self.apply_focus();
    }

    /// Select the focused path in the current list, when it is listed.
    fn apply_focus(&mut self) {
        let Some(path) = self.focus.clone() else {
            return;
        };
        let rows = self.rows();
        if rows.is_empty() && self.model.is_none() {
            return;
        }
        let total = rows.len();
        let index = rows.iter().position(|e| e.path == path);
        if let Some(i) = index {
            if self.table.per_page == 0 {
                self.table.per_page = 1;
            }
            self.table.select(i, total);
            self.sheet_open = true;
        }
        self.shown_filter = Some(self.filter.clone());
        self.focus = None;
    }

    /// Edit the Codex settings of another profile (pending edits are dropped).
    /// The picker shows it at once; the previous profile's settings are not
    /// shown while its model is built, and a build for an earlier choice is
    /// dropped.
    pub fn select_profile(&mut self, cx: &Cx<'_>, profile: superba_core::ProfileId) {
        if self.profile.as_ref() == Some(&profile) {
            return;
        }
        self.profile = Some(profile);
        self.pending.clear();
        self.buffers.clear();
        self.invalid.clear();
        self.model = None;
        self.sheet_open = false;
        self.expanded = None;
        self.editor = None;
        self.rows_dialog = None;
        self.diff_open = false;
        self.reload(cx);
    }

    /// Save the pending edits into the profile (then apply them through the switch
    /// flow when the profile is applied).
    fn save(&mut self, cx: &Cx<'_>) {
        if self.saving.is_some() || self.pending.is_empty() || cx.busy {
            return;
        }
        let Some(model) = self.model.clone() else {
            return;
        };
        if source::blocked_reason(cx, &model).is_some() {
            return;
        }
        self.save_error = None;
        self.saving = Some(crate::runtime::task(source::save(
            cx.service.clone(),
            model,
            self.pending.clone(),
        )));
    }

    /// Start a (re)load unless one is running.
    fn load(&mut self, cx: &Cx<'_>) {
        if !self.load.loading() {
            self.reload(cx);
        }
    }

    /// Build the chosen profile's model now (a running build is dropped).
    fn reload(&mut self, cx: &Cx<'_>) {
        self.loaded_for = cx.data.overview.value.as_ref().map(|o| o.generated_at);
        self.loaded_profiles = cx.data.profiles.value.clone();
        let task = (self.loader)(cx, self.profile.clone());
        self.load.start(self.profile.clone(), task, LOAD_LIMIT);
    }

    /// Poll the load and the save; true while either runs.
    fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        let stamp = cx.data.overview.value.as_ref().map(|o| o.generated_at);
        // A switch rewrites config.toml: reload once the overview after it arrives.
        let switched = cx
            .data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.active.as_ref())
            .map(|a| a.applied_at);
        // Wait for the profile set: the page edits a profile's settings.
        let profiles_ready = cx.data.profiles.value.is_some() || cx.data.profiles.error.is_some();
        let first = self.model.is_none()
            && !self.load.loading()
            && self.load.failure().is_none()
            && profiles_ready;
        let after_switch =
            !cx.busy && stamp.is_some() && self.loaded_for.is_some_and(|l| switched > Some(l));
        let profiles_changed = self.model.is_some()
            && self.loaded_profiles.is_some()
            && cx.data.profiles.value.is_some()
            && self.loaded_profiles.as_ref() != cx.data.profiles.value.as_ref();
        if first || after_switch || profiles_changed {
            self.load(cx);
        }
        if let Some(task) = &self.saving
            && let Some(result) = task.poll()
        {
            self.saving = None;
            match result {
                Ok(after) => {
                    let name = self.model.as_ref().and_then(|m| m.profile.clone());
                    self.pending.clear();
                    self.buffers.clear();
                    self.save_error = None;
                    out.push(Action::Refresh);
                    match after {
                        source::AfterSave::Apply(profile) => out.push(Action::Switch {
                            profile,
                            line: None,
                            auth: None,
                        }),
                        source::AfterSave::Saved => out.push(Action::Toast {
                            status: crate::design::widgets::Status::Ok,
                            text: cx.tr.f(
                                "codex_settings.pending.saved_later",
                                &[("provider", &name.unwrap_or_default())],
                            ),
                        }),
                    }
                    // Show the saved values at once; the overview refresh reloads.
                    self.reload(cx);
                }
                Err(e) => self.save_error = Some(super::error_text(cx.tr, &e)),
            }
        }
        if let Some((_, model)) = self.load.poll() {
            self.set_model(model);
            self.apply_focus();
        }
        self.load.loading() || self.saving.is_some()
    }
}

/// Draw the page: header, then the strip, the three panes and the footer in the
/// fixed page rectangle (no page-level scrolling).
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if state.files.open {
        files_view::show(ui, cx, &mut state.files, out);
        return;
    }
    let loading = state.poll(cx, out);
    if loading {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    page_header(ui, cx, Page::Settings, |ui| {
        if ui
            .add(
                crate::design::widgets::Button::ghost(cx.tr.t("codex_editor.open"))
                    .icon(crate::design::icons::Icon::Terminal)
                    .small(),
            )
            .on_hover_text(cx.tr.t("codex_editor.open_tip"))
            .clicked()
        {
            let kind = state.files.kind();
            let target = state.files.target();
            state.files.open_file(cx, target, kind);
        }
        if ui
            .add(
                crate::design::widgets::Button::ghost(cx.tr.t("codex_settings.reload"))
                    .icon(crate::design::icons::Icon::Refresh)
                    .small()
                    .loading(loading)
                    .enabled(!loading),
            )
            .clicked()
        {
            state.reload(cx);
        }
    });
    if state.model.is_none() {
        // First load, or another provider chosen: never the previous one's
        // settings (user bug 83).
        let events = view::waiting(ui, cx, state);
        if events.retry {
            state.reload(cx);
        }
        if let Some(profile) = events.profile {
            state.select_profile(cx, profile);
        }
        return;
    }
    let events = view::page(ui, cx, state, out);
    if events.save {
        state.save(cx);
    }
    if let Some(profile) = events.profile {
        state.select_profile(cx, profile);
    }
}

/// Unsaved edits and dialogs of the page.
pub fn status(state: &State) -> PageStatus {
    if state.files.open {
        return PageStatus {
            modal: state.files.dialog.is_some(),
            pending: usize::from(state.files.dirty()),
            ..PageStatus::default()
        };
    }
    PageStatus {
        modal: state.diff_open
            || state.expanded.is_some()
            || state.editor.is_some()
            || state.rows_dialog.is_some(),
        pending: state.pending.len() + usize::from(state.files.dirty()),
        ..PageStatus::default()
    }
}

/// Run a command addressed to this page (see the module docs).
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = out;
    if let Some(file) = command.strip_prefix("files-backups:") {
        let (file, target) = file.split_once('@').map_or((file, None), |(f, t)| {
            (f, Some(superba_core::TargetId::new(t)))
        });
        let kind = superba_core::CodexFileKind::parse(file).unwrap_or(state.files.kind());
        let target = target.unwrap_or_else(|| state.files.target());
        state.files.open_file(cx, target, kind);
        if state.files.dialog.is_none() {
            state.files.open_backups(cx);
        }
    } else if let Some(rest) = command.strip_prefix("files") {
        // `files`, `files:config`, `files:auth`, `files:<file>@<target>`.
        let rest = rest.trim_start_matches(':');
        let (file, target) = match rest.split_once('@') {
            Some((f, t)) => (f, Some(superba_core::TargetId::new(t))),
            None => (rest, None),
        };
        let kind = superba_core::CodexFileKind::parse(file).unwrap_or(state.files.kind());
        let target = target.unwrap_or_else(|| state.files.target());
        state.files.open_file(cx, target, kind);
    } else if state.files.open && command == "save" {
        state.files.review();
    } else if let Some(path) = command.strip_prefix("focus:") {
        state.focus(path);
    } else if let Some(id) = command.strip_prefix("profile:") {
        state.select_profile(cx, superba_core::ProfileId::new(id));
    } else if command == "migrate" {
        state.migrate_all();
        state.diff_open = true;
    } else if command == "search" {
        state.focus_search = true;
    } else if command == "save" && !cx.busy {
        // Ctrl/Cmd+S with pending edits (the shell sends it only then).
        state.save(cx);
    }
}
