//! Providers (UX §3.3): the services Codex connects to, as master/detail. The
//! list holds every provider (the applied one first, then presets, custom sites
//! and the official login), with a right-click menu per entry (rename /
//! duplicate / test / delete) and the name renamed in place (a click on the
//! selected entry's name, a double click or F2); the detail shows a provider's
//! header (kind, in use, homepage, "switch to this provider", Test and Delete,
//! which asks for confirmation) and
//! the tabs **Connection** (lines with the one in use, switching and the order
//! as the mode makes them work, latency and auth availability; [`lines`]),
//! the automatic auth decision, the key, a models and a settings summary) and
//! **Account & usage** (balance, and for sites with a usage API the daily cost
//! and per-model usage, fetched only when the tab opens without data or on
//! refresh). The official login has its own detail without tabs. "Add provider"
//! is a two-step dialog over the presets (Krill pre-selected).
//!
//! "Import from other tools" (step 1 of Add provider, the palette, the `import`
//! command) opens [`crate::pages::import`]'s dialog over the page.
//!
//! Commands: `add`, `import`, `profile:<id>` / `site:<id>` (select; `site:` also opens the
//! key editor of a key-less site), `tab:connection|account|usage`,
//! `test:<provider command>` (`test:profile:<id>`: select and test it).

pub mod account;
mod actions;
mod add;
mod compaction;
mod connection;
mod detail;
pub mod form;
mod lines;
mod list;
pub(crate) mod mixed;
pub mod ops;
mod simple;
mod transports;

use std::collections::{BTreeSet, HashMap};

use egui::Ui;
use superba_core::{
    AccountReport, AuthMode, Error, KeyCheck, LineId, ModelsView, ModelsViewRequest, Profile,
    ProfileId, SiteId,
};

use self::ops::SiteReport;
use super::shared::providers::{self, Provider, ProviderId};
use super::{Action, Cx, Page, PageStatus, page_header};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::widgets::{Button, PagedState};
use crate::runtime::ReadTask;

pub use self::add::{AddDialog, AddStep};
pub use self::simple::show as show_simple;

/// Tabs of a key-based provider's detail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    /// Lines, authentication, key, models and settings summaries.
    #[default]
    Connection,
    /// Balance and usage.
    Account,
}

/// The key editor of one provider.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeyEdit {
    /// Whose key.
    pub provider: Option<ProviderId>,
    /// Typed key.
    pub key: String,
    /// Show it.
    pub reveal: bool,
}

/// Key checks of one provider on every line.
pub(crate) type LineChecks = Vec<(LineId, AuthMode, superba_core::Result<KeyCheck>)>;

/// Account data of one provider (page-owned: each provider has its own key).
#[derive(Default)]
pub struct AccountState {
    /// Range shown.
    pub days: u32,
    /// Last report (kept while a new one loads or when the site refuses one).
    pub report: Option<AccountReport>,
    /// Running request: (days, task).
    pub task: Option<(u32, ReadTask<AccountReport>)>,
    /// Last error.
    pub error: Option<Error>,
    /// A range the site refused with 429 (the previous report stays).
    pub limited: Option<u32>,
}

/// The delete confirmation.
pub(crate) struct RemoveDialog {
    /// Provider removed.
    pub provider: Provider,
    /// Last error.
    pub error: Option<String>,
    /// Running removal.
    pub task: Option<ReadTask<()>>,
}

/// Page-local state, kept across navigation.
#[derive(Default)]
pub struct State {
    /// Full provider settings were explicitly opened from simple mode.
    pub simple_advanced: bool,
    /// Selected provider (`None` = the first of the list, the applied one).
    pub selected: Option<ProviderId>,
    /// Tab of the detail.
    pub tab: Tab,
    /// Paging of the list.
    pub list: PagedState,
    /// Manual authentication controls are shown.
    pub(crate) advanced: bool,
    /// The key editor.
    pub(crate) key_edit: Option<KeyEdit>,
    /// Edited base URL of a custom site: (site, text).
    pub(crate) base_url: Option<(SiteId, String)>,
    /// Saving the base URL.
    pub(crate) base_url_task: Option<ReadTask<Profile>>,
    /// Why the base URL was not saved.
    pub(crate) base_url_error: Option<String>,
    /// Key checks per provider.
    pub(crate) checks: HashMap<ProviderId, LineChecks>,
    /// Running key check: (provider, task).
    pub(crate) check_task: Option<(ProviderId, ReadTask<LineChecks>)>,
    /// Sites whose lines the page asked to probe (once each).
    pub(crate) probed: BTreeSet<SiteId>,
    /// The router's latest upstream per member, while router-mode lines show.
    pub(crate) router: lines::RouterLive,
    /// Models summaries per profile.
    pub(crate) models: HashMap<ProfileId, superba_core::Result<ModelsView>>,
    /// Running models load.
    pub(crate) models_task: Option<(ProfileId, ReadTask<ModelsView>)>,
    /// Account data per profile.
    pub accounts: HashMap<ProfileId, AccountState>,
    /// Simple mode always requests seven days, independently of the advanced tab.
    pub simple_accounts: HashMap<ProfileId, AccountState>,
    /// Paging of the per-model usage table.
    pub(crate) usage_table: PagedState,
    /// The inline rename in the list.
    pub(crate) rename: Option<actions::Rename>,
    /// A click on the selected entry's name, waiting out the double-click delay
    /// before the rename starts: (entry, input time of the click).
    pub(crate) rename_armed: Option<(ProviderId, f64)>,
    /// Site tests ("Test connection" on a custom site).
    pub(crate) tests: HashMap<SiteId, ReadTask<SiteReport>>,
    /// Results of site tests.
    pub(crate) reports: HashMap<SiteId, SiteReport>,
    /// The Add provider dialog.
    pub add: Option<AddDialog>,
    /// The import dialog (providers from cc-switch, AI Toolbox).
    pub import: Option<crate::pages::import::ImportDialog>,
    /// The delete confirmation.
    pub(crate) remove: Option<RemoveDialog>,
    /// Confirmation to disable the line Codex uses in direct mode (user bug
    /// 108): (provider, the line to disable).
    pub(crate) disable_line: Option<(ProfileId, LineId)>,
    /// Mixed login on the official login's detail.
    pub(crate) mixed: mixed::MixedLogin,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("providers::State")
            .field("selected", &self.selected)
            .field("tab", &self.tab)
            .field("adding", &self.add.as_ref().map(|a| a.step))
            .finish_non_exhaustive()
    }
}

impl State {
    /// A dialog of this page is open (the shell leaves Enter / Escape to it).
    pub fn is_modal_open(&self) -> bool {
        self.add.is_some()
            || self.remove.is_some()
            || self.import.is_some()
            || self.disable_line.is_some()
    }

    /// The entry being renamed in the list and the typed name.
    pub fn renaming(&self) -> Option<(&ProviderId, &str)> {
        self.rename.as_ref().map(|r| (&r.provider, r.text.as_str()))
    }

    /// The entry whose delete confirmation is open.
    pub fn removing(&self) -> Option<&ProviderId> {
        self.remove.as_ref().map(|r| &r.provider.id)
    }

    /// Open the Add provider dialog (step 1, Krill pre-selected).
    pub fn open_add(&mut self) {
        if !self.is_modal_open() {
            self.add = Some(AddDialog::new());
        }
    }

    /// Open the import dialog (and start scanning the other tools' stores).
    pub fn open_import(&mut self, cx: &Cx<'_>) {
        if self.add.is_none() && self.remove.is_none() && self.import.is_none() {
            self.import = Some(crate::pages::import::ImportDialog::open(cx));
        }
    }

    /// Select `id` (the detail resets its per-provider editors).
    pub fn select(&mut self, id: ProviderId) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            self.key_edit = None;
            self.rename = None;
            self.rename_armed = None;
            self.base_url = None;
            self.base_url_error = None;
            self.advanced = false;
        }
    }

    /// The selected provider in `list` (the first one when nothing or a vanished
    /// provider is selected).
    pub fn current<'a>(&self, list: &'a [Provider]) -> Option<&'a Provider> {
        self.selected
            .as_ref()
            .and_then(|id| list.iter().find(|p| &p.id == id))
            .or_else(|| list.first())
    }

    /// Collect finished background work; true while something runs.
    fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        let mut busy = false;
        if let Some((id, task)) = &self.check_task {
            match task.poll() {
                Some(Ok(checks)) => {
                    self.checks.insert(id.clone(), checks);
                    self.check_task = None;
                }
                Some(Err(e)) => {
                    out.push(Action::Toast {
                        status: crate::design::widgets::Status::Error,
                        text: super::error_text(cx.tr, &e),
                    });
                    self.check_task = None;
                }
                None => busy = true,
            }
        }
        if let Some((id, task)) = &self.models_task {
            match task.poll() {
                Some(r) => {
                    self.models.insert(id.clone(), r);
                    self.models_task = None;
                }
                None => busy = true,
            }
        }
        if let Some(task) = &self.base_url_task {
            match task.poll() {
                Some(Ok(_)) => {
                    self.base_url_task = None;
                    self.base_url = None;
                    self.base_url_error = None;
                    out.push(Action::Refresh);
                }
                Some(Err(e)) => {
                    self.base_url_task = None;
                    self.base_url_error = Some(super::error_text(cx.tr, &e));
                }
                None => busy = true,
            }
        }
        let done: Vec<SiteId> = self
            .tests
            .iter()
            .filter_map(|(id, task)| task.poll().map(|r| (id.clone(), r)))
            .map(|(id, r)| {
                if let Ok(report) = r {
                    self.reports.insert(id.clone(), report);
                }
                id
            })
            .collect();
        for id in done {
            self.tests.remove(&id);
        }
        busy |= !self.tests.is_empty();
        busy |= account::poll(self);
        busy |= self.mixed.poll(cx, out);
        if let Some(d) = &mut self.add {
            let (running, finished) = d.poll(cx, out);
            busy |= running;
            if let Some(selected) = finished {
                self.add = None;
                if let Some(id) = selected {
                    self.select(id);
                }
            }
        }
        if let Some(r) = &mut self.remove
            && let Some(task) = &r.task
        {
            match task.poll() {
                Some(Ok(())) => {
                    let name = list::name(cx, &r.provider);
                    out.push(Action::Toast {
                        status: crate::design::widgets::Status::Ok,
                        text: cx.tr.f("providers.delete.done", &[("name", &name)]),
                    });
                    out.push(Action::Refresh);
                    self.remove = None;
                    self.selected = None;
                }
                Some(Err(e)) => {
                    r.task = None;
                    r.error = Some(super::error_text(cx.tr, &e));
                }
                None => busy = true,
            }
        }
        busy
    }

    /// Load the models summary of `profile` once.
    pub(crate) fn want_models(&mut self, cx: &Cx<'_>, profile: &ProfileId) {
        if self.models.contains_key(profile) || self.models_task.is_some() {
            return;
        }
        let service = cx.service.clone();
        let request = ModelsViewRequest {
            profile_id: Some(profile.clone()),
            refresh: false,
        };
        self.models_task = Some((
            profile.clone(),
            crate::runtime::task(async move { service.models_view(request).await }),
        ));
    }
}

/// Draw the page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if state.poll(cx, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    let mut add = false;
    page_header(ui, cx, Page::Providers, |ui| {
        if state.simple_advanced
            && ui
                .add(
                    Button::ghost(cx.tr.t("providers.simple.back"))
                        .small()
                        .enabled(!state.is_modal_open()),
                )
                .clicked()
        {
            state.simple_advanced = false;
        }
        add = ui
            .add(
                Button::primary(cx.tr.t("providers.add.button"))
                    .icon(Icon::Plus)
                    .small()
                    .enabled(!state.is_modal_open() && cx.data.profiles.value.is_some()),
            )
            .clicked();
    });
    if add {
        state.open_add();
    }
    let all = providers::list(cx.data);
    let body = ui.available_rect_before_wrap();
    let cols = layout::column_rects(body, &[Track::Fixed(260.0), Track::Fill], 24.0);
    layout::fixed(ui, cols[0], |ui| list::show(ui, cx, state, &all, out));
    ui.painter().vline(
        cols[1].min.x - 12.0,
        cols[1].y_range(),
        egui::Stroke::new(1.0, cx.pal.border),
    );
    let current = state.current(&all).cloned();
    layout::fixed(ui, cols[1], |ui| match &current {
        Some(p) => detail::show(ui, cx, state, p, out),
        None => super::empty_state(
            ui,
            cx,
            Icon::Plug,
            &cx.tr.t("providers.empty.title"),
            &cx.tr.t("providers.empty.body"),
        ),
    });
    add::show(ui.ctx(), cx, state, out);
    detail::remove_dialog(ui.ctx(), cx, state);
    lines::disable_dialog(ui.ctx(), cx, state, out);
    show_import(ui.ctx(), cx, state, out);
}

/// The import dialog, when open; when it finishes with imports the page reloads,
/// selects the first imported provider and offers a switch to it.
fn show_import(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    use crate::pages::import::{self, ImportOutcomeUi};
    let Some(d) = state.import.as_mut() else {
        return;
    };
    let outcome = import::show_dialog(ctx, cx, d, out);
    match &outcome {
        ImportOutcomeUi::Continue => {}
        ImportOutcomeUi::Close => state.import = None,
        ImportOutcomeUi::Done { first, .. } => {
            import::finished_actions(cx, &outcome, out);
            state.import = None;
            if let Some(id) = first {
                state.select(ProviderId::Profile(id.clone()));
            }
        }
    }
}

/// Run a command addressed to this page (see the module docs).
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    if command == "add" {
        state.open_add();
        return;
    }
    if command == "import" {
        state.open_import(cx);
        return;
    }
    if let Some(tab) = command.strip_prefix("tab:") {
        state.tab = match tab {
            "account" | "usage" => Tab::Account,
            _ => Tab::Connection,
        };
        return;
    }
    if let Some(id) = command.strip_prefix("profile:") {
        state.select(ProviderId::Profile(ProfileId::new(id)));
        return;
    }
    // `key:<profile>`: the profile's key field (the Router page sends here a
    // provider without an API key).
    if let Some(id) = command.strip_prefix("key:") {
        let target = ProviderId::Profile(ProfileId::new(id));
        state.select(target.clone());
        state.tab = Tab::Connection;
        state.key_edit = Some(KeyEdit {
            provider: Some(target),
            ..KeyEdit::default()
        });
        return;
    }
    if let Some(id) = command.strip_prefix("site:") {
        let site = SiteId::new(id);
        let all = providers::list(cx.data);
        let target = all
            .iter()
            .find(|p| p.site.id == site && p.profile.is_none())
            .or_else(|| all.iter().find(|p| p.site.id == site))
            .map(|p| p.id.clone())
            .unwrap_or(ProviderId::Site(site));
        state.select(target.clone());
        state.tab = Tab::Connection;
        state.key_edit = Some(KeyEdit {
            provider: Some(target),
            ..KeyEdit::default()
        });
        return;
    }
    // `test:` takes the provider command form (`profile:<id>`, `site:<id>`).
    if let Some(rest) = command.strip_prefix("test:") {
        let all = providers::list(cx.data);
        let target = all.iter().find(|p| p.id.command() == rest).cloned();
        if let Some(p) = target {
            state.select(p.id.clone());
            state.tab = Tab::Connection;
            detail::test_connection(state, cx, &p, out);
        }
    }
}

/// Dialogs of the page.
pub fn status(state: &State) -> PageStatus {
    PageStatus {
        modal: state.is_modal_open(),
        ..PageStatus::default()
    }
}
