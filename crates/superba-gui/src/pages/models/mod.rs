//! Models (UX §3.4, plan §11): the model list of a provider with its source,
//! the default model / reasoning effort / profile speed (and where the speed
//! applies), per-model context window and auto-compact overrides, the tiers each
//! model offers (read-only), price overrides for the request log's cost
//! estimates, the upstream model name and picker mode of custom sites, and the
//! pending bar ("save & apply" = `save_models`, then a hot-restart switch when the
//! provider is applied). The official login is shown read-only.
//!
//! Context window and auto-compact (user bug 7): every row shows the value Codex
//! uses (custom, all models or the model's default, tagged as such; the exact
//! count on hover), validated with the bounds `spb models` and the service use
//! (`superba_core::check_*`). After a save the row shows the saved value at once
//! (the view is patched from the saved profile while it reloads) with its state
//! against what Codex reads now: not applied yet, applied, next switch (another
//! provider), or not used (profiles without catalog support). Only
//! a save that changes what the applied provider's Codex reads restarts it, and
//! the page says so before and after saving.
//!
//! Commands: `profile:<id>` (show a provider), `context:<slug>` (focus a model's
//! context window), `speed:standard|fast|ultrafast` (save the speed of the shown
//! provider and apply it), `refresh`, `search`, `save`.

mod catalog;
pub mod model;
mod view;

pub use view::speed_tooltip;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use egui::Ui;
use superba_core::{
    Error, ModelsEdit, ModelsEditReport, ModelsView, ModelsViewRequest, Notice, PriceEntry,
    PriceTable, ProfileId, ProfileSet, Result, SiteKind, SpeedTier, SwitchService, Timestamp,
};

use self::model::{Field, Pending, Saved};
use super::shared::providers;
use super::{Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem};
use crate::design::widgets::{PagedState, Status};
use crate::runtime::ReadTask;

/// The price editor of one model.
#[derive(Clone, Debug, Default)]
pub struct PriceDialog {
    /// Model slug.
    pub slug: String,
    /// Input price (USD per 1M tokens).
    pub input: String,
    /// Cached input price.
    pub cached: String,
    /// Output price.
    pub output: String,
    /// Validation message.
    pub error: Option<String>,
}

/// What a finished save did.
#[derive(Debug)]
struct SaveDone {
    /// The provider the edit was saved for.
    profile: ProfileId,
    report: Option<ModelsEditReport>,
    models_changed: bool,
    /// The save changes what the applied provider's Codex reads: restart it.
    restart: bool,
}

/// Page-local state, kept across navigation.
#[derive(Default)]
pub struct State {
    /// Filter the merged list by its source layer.
    pub catalog_source: Option<superba_core::CatalogLayer>,
    /// Filter the merged list by provider id.
    pub catalog_provider: Option<String>,
    catalog_slugs: BTreeMap<String, String>,
    catalog_test: Option<ReadTask<superba_core::RouterModelTest>>,
    catalog_save: Option<ReadTask<()>>,
    catalog_result: Option<superba_core::RouterModelTest>,
    catalog_error: Option<String>,
    /// Provider chosen in the picker (`None` = the applied one).
    profile: Option<ProfileId>,
    /// Loaded view.
    view: Option<ModelsView>,
    /// Last load error (the previous view stays visible).
    error: Option<Error>,
    /// Load in flight.
    task: Option<ReadTask<ModelsView>>,
    /// The running load bypasses the cache (Refresh).
    refreshing: bool,
    /// When the view was requested (reload after a switch).
    loaded_at: Option<Timestamp>,
    /// Profile set the view belongs to (reload when it changes).
    loaded_profiles: Option<ProfileSet>,
    /// Pending edits.
    pub pending: Pending,
    /// Text of the token / name fields by (slug, field); `""` = all models.
    buffers: HashMap<(String, Field), String>,
    /// Rejected input by (slug, field), translated.
    invalid: BTreeMap<(String, Field), String>,
    /// The rejected text by (slug, field), shown in its field until fixed.
    rejected: HashMap<(String, Field), String>,
    /// Paging and selection of the table.
    pub table: PagedState,
    /// Show hidden models.
    pub show_hidden: bool,
    /// Filter text.
    pub query: String,
    /// Focus the search field next frame.
    focus_search: bool,
    /// Focus this row's field next frame.
    focus: Option<(String, Field)>,
    /// Save in flight.
    saving: Option<ReadTask<SaveDone>>,
    /// Last save error.
    save_error: Option<String>,
    /// The diff dialog is open.
    pub diff_open: bool,
    /// The price editor is open.
    pub price: Option<PriceDialog>,
    /// Built-in price table (feeds), when loaded.
    prices: Option<PriceTable>,
    /// The price table load ran (or was set).
    prices_requested: bool,
    /// Price table load in flight.
    price_task: Option<ReadTask<PriceTable>>,
    /// Profile whose speed a palette command asked to apply after the load.
    speed_request: Option<SpeedTier>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("models::State")
            .field("profile", &self.view.as_ref().map(|v| &v.profile_id))
            .field("models", &self.view.as_ref().map(|v| v.list.models.len()))
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl State {
    /// The loaded view.
    pub fn view(&self) -> Option<&ModelsView> {
        self.view.as_ref()
    }

    /// Set the view (tests and the loader); pending edits it already contains are
    /// dropped.
    pub fn set_view(&mut self, view: ModelsView) {
        self.view = Some(view);
        self.error = None;
    }

    /// Set the price table (tests; otherwise loaded from the feed cache).
    pub fn set_price_table(&mut self, table: Option<PriceTable>) {
        self.prices = table;
        self.prices_requested = true;
    }

    /// The text buffer of a field (tests).
    pub fn buffer_mut(&mut self, slug: &str, field: Field) -> &mut String {
        self.buffers.entry((slug.to_owned(), field)).or_default()
    }

    /// Commit the text in the buffer of (slug, field) as if the user left the
    /// field: validated, then recorded as a pending edit or an error.
    pub fn commit_field(&mut self, cx: &Cx<'_>, slug: &str, field: Field) {
        let text = self
            .buffers
            .get(&(slug.to_owned(), field))
            .cloned()
            .unwrap_or_default();
        view::apply_text(cx, self, slug, field, text);
    }

    /// Validation messages by (slug, field).
    pub fn invalid(&self) -> &BTreeMap<(String, Field), String> {
        &self.invalid
    }

    /// Number of pending changes.
    pub fn pending_count(&self, cx: &Cx<'_>) -> usize {
        let Some(view) = &self.view else {
            return 0;
        };
        let map = model_map(cx, view);
        let prices = saved_prices(cx, &view.profile_id);
        self.pending.count(&Saved {
            view,
            model_map: &map,
            prices: &prices,
        })
    }

    fn select_profile(&mut self, cx: &Cx<'_>, id: ProfileId) {
        if self.view.as_ref().is_some_and(|v| v.profile_id == id) && self.profile.is_some() {
            return;
        }
        // Never show (and let the user edit) another provider's rows while the
        // chosen one loads: an edit would be saved to the wrong provider.
        if self.view.as_ref().is_some_and(|v| v.profile_id != id) {
            self.view = None;
            self.error = None;
        }
        self.profile = Some(id);
        self.pending = Pending::default();
        self.buffers.clear();
        self.invalid.clear();
        self.rejected.clear();
        self.table = PagedState::default();
        self.task = None;
        self.load(cx, false);
    }

    fn load(&mut self, cx: &Cx<'_>, refresh: bool) {
        if self.task.is_some() {
            return;
        }
        self.loaded_at = Some(cx.now);
        self.loaded_profiles = cx.data.profiles.value.clone();
        self.refreshing = refresh;
        let service = cx.service.clone();
        let request = ModelsViewRequest {
            profile_id: self.profile.clone(),
            refresh,
        };
        self.task = Some(crate::runtime::task(async move {
            service.models_view(request).await
        }));
    }

    fn load_prices(&mut self, cx: &Cx<'_>) {
        if self.prices_requested {
            return;
        }
        self.prices_requested = true;
        let service = cx.service.clone();
        self.price_task = Some(crate::runtime::task(
            async move { service.price_table().await },
        ));
    }

    /// Poll the background work; true while something runs.
    fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        if let Some(task) = &self.catalog_test
            && let Some(result) = task.poll()
        {
            self.catalog_test = None;
            match result {
                Ok(result) => {
                    self.catalog_result = Some(result);
                    self.load(cx, true);
                }
                Err(error) => self.catalog_error = Some(super::error_text(cx.tr, &error)),
            }
        }
        if let Some(task) = &self.catalog_save
            && let Some(result) = task.poll()
        {
            self.catalog_save = None;
            match result {
                Ok(()) => {
                    self.catalog_slugs.clear();
                    self.load(cx, true);
                }
                Err(error) => self.catalog_error = Some(super::error_text(cx.tr, &error)),
            }
        }
        let profiles_ready = cx.data.profiles.value.is_some() || cx.data.profiles.error.is_some();
        let first = self.view.is_none() && self.error.is_none() && profiles_ready;
        let switched = cx
            .data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.active.as_ref())
            .map(|a| a.applied_at);
        let after_switch = !cx.busy
            && self.view.is_some()
            && self.loaded_at.is_some()
            && switched > self.loaded_at;
        // Only against the profiles a load saw: a view set directly (tests) has
        // nothing to compare with, and a reload would race and replace it.
        let profiles_changed = self.view.is_some()
            && self.loaded_at.is_some()
            && cx.data.profiles.value.is_some()
            && self.loaded_profiles.as_ref() != cx.data.profiles.value.as_ref();
        if (first || after_switch || profiles_changed) && self.saving.is_none() {
            self.load(cx, false);
        }
        self.load_prices(cx);
        if let Some(task) = &self.price_task
            && let Some(result) = task.poll()
        {
            self.price_task = None;
            match result {
                Ok(table) => self.prices = Some(table),
                Err(e) => tracing::debug!("price table: {e}"),
            }
        }
        if let Some(task) = &self.saving
            && let Some(result) = task.poll()
        {
            self.saving = None;
            match result {
                Ok(done) => self.saved(cx, done, out),
                Err(e) => self.save_error = Some(super::error_text(cx.tr, &e)),
            }
        }
        if let Some(task) = &self.task
            && let Some(result) = task.poll()
        {
            self.task = None;
            self.refreshing = false;
            match result {
                Ok(view) => {
                    let map = model_map(cx, &view);
                    let prices = saved_prices(cx, &view.profile_id);
                    self.pending.rebase(&Saved {
                        view: &view,
                        model_map: &map,
                        prices: &prices,
                    });
                    self.set_view(view);
                    if let Some(tier) = self.speed_request.take() {
                        self.apply_speed(cx, tier);
                    }
                }
                Err(e) => self.error = Some(e),
            }
        }
        self.task.is_some()
            || self.saving.is_some()
            || self.price_task.is_some()
            || self.catalog_test.is_some()
            || self.catalog_save.is_some()
    }

    fn saved(&mut self, cx: &Cx<'_>, done: SaveDone, out: &mut Vec<Action>) {
        // Show the saved values at once: the reload (provider list, local Codex)
        // can take seconds, and the rows must not fall back to the old values
        // meanwhile.
        if let (Some(report), Some(v)) = (&done.report, self.view.as_mut())
            && report.profile.id == v.profile_id
        {
            v.defaults = report.profile.model.clone();
            if v.router_catalog.is_some() {
                for (slug, value) in &self.pending.overrides {
                    if value.is_empty() {
                        v.overrides.remove(slug);
                    } else {
                        v.overrides.insert(slug.clone(), value.clone());
                    }
                }
            } else {
                v.overrides = report.profile.model_overrides.clone();
            }
            v.picker = report.profile.model_picker;
        }
        self.pending = Pending::default();
        self.buffers.clear();
        self.invalid.clear();
        self.rejected.clear();
        self.save_error = None;
        out.push(Action::Refresh);
        if let Some(report) = &done.report {
            for n in report
                .notes
                .iter()
                .filter(|n| n.severity != superba_core::Severity::Info)
            {
                out.push(Action::Toast {
                    status: Status::Warn,
                    text: cx.localize(&n.text),
                });
            }
        }
        let applied = providers::applied_profile_id(cx.data) == Some(&done.profile);
        let name = provider_name(cx, &done.profile);
        // Say why Codex restarts (or why it does not) before the switch banner.
        let key = match (done.models_changed, applied, done.restart) {
            (true, true, true) => "models.saved_restart",
            (true, true, false) => "models.saved_no_restart",
            (true, false, _) => "models.saved_later",
            (false, _, _) => "models.saved",
        };
        out.push(Action::Toast {
            status: Status::Ok,
            text: cx.tr.f(key, &[("provider", &name)]),
        });
        if done.models_changed && applied && done.restart {
            out.push(Action::Switch {
                profile: done.profile.clone(),
                line: None,
                auth: None,
            });
        }
        self.task = None;
        self.load(cx, false);
    }

    /// Save the pending edits (then plan a switch when the provider is applied).
    fn save(&mut self, cx: &Cx<'_>) {
        if self.saving.is_some() || !self.invalid.is_empty() || cx.busy {
            return;
        }
        let Some(view) = &self.view else {
            return;
        };
        if view.read_only || self.pending.is_empty() {
            return;
        }
        let map = model_map(cx, view);
        let prices = saved_prices(cx, &view.profile_id);
        let saved = Saved {
            view,
            model_map: &map,
            prices: &prices,
        };
        if !model::limit_errors(&saved, &self.pending).is_empty() {
            return;
        }
        let edit = self.pending.edit(&saved);
        let restart = model::affects_codex(&edit, view.per_model);
        let price_changes = self.pending.prices.clone();
        self.save_error = None;
        self.saving = Some(crate::runtime::task(save(
            cx.service.clone(),
            edit,
            view.profile_id.clone(),
            price_changes,
            restart,
        )));
    }

    /// Save the speed of the shown provider and apply it (palette).
    fn apply_speed(&mut self, cx: &Cx<'_>, tier: SpeedTier) {
        let Some(view) = &self.view else {
            self.speed_request = Some(tier);
            return;
        };
        if view.read_only || cx.busy || self.saving.is_some() {
            return;
        }
        let mut defaults = view.defaults.clone();
        defaults.speed = Some(tier);
        let edit = ModelsEdit {
            profile_id: Some(view.profile_id.clone()),
            defaults: Some(defaults),
            ..ModelsEdit::default()
        };
        self.saving = Some(crate::runtime::task(save(
            cx.service.clone(),
            edit,
            view.profile_id.clone(),
            BTreeMap::new(),
            true,
        )));
    }
}

/// Save a models edit and price overrides.
async fn save(
    service: Arc<dyn SwitchService>,
    edit: ModelsEdit,
    profile: ProfileId,
    prices: BTreeMap<String, Option<PriceEntry>>,
    restart: bool,
) -> Result<SaveDone> {
    let models_changed =
        edit.defaults.is_some() || !edit.set.is_empty() || edit.picker.is_some() || edit.reset_all;
    let report = if models_changed {
        Some(service.save_models(edit).await?)
    } else {
        None
    };
    if !prices.is_empty() {
        let set = service.profile_set().await?;
        let mut stats = set.settings.router.stats.clone();
        let entry = stats.prices.overrides.entry(profile.clone()).or_default();
        for (slug, price) in prices {
            match price {
                Some(p) => {
                    entry.insert(slug, p);
                }
                None => {
                    entry.remove(&slug);
                }
            }
        }
        if entry.is_empty() {
            stats.prices.overrides.remove(&profile);
        }
        match service.router_set_stats(stats.clone()).await {
            Ok(_) => {}
            Err(e) if e.is_not_implemented() => {
                let mut settings = set.settings;
                settings.router.stats = stats;
                service.save_settings(settings).await?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(SaveDone {
        profile,
        report,
        models_changed,
        restart,
    })
}

/// `Site::model_map` of the view's site when it is a custom site.
fn model_map(cx: &Cx<'_>, view: &ModelsView) -> BTreeMap<String, String> {
    providers::site(cx.data, &view.site_id)
        .filter(|s| s.kind == SiteKind::Compatible)
        .map(|s| s.model_map)
        .unwrap_or_default()
}

/// The profile's saved price overrides.
fn saved_prices(cx: &Cx<'_>, profile: &ProfileId) -> BTreeMap<String, PriceEntry> {
    cx.data
        .profiles
        .value
        .as_ref()
        .and_then(|s| s.settings.router.stats.prices.overrides.get(profile))
        .cloned()
        .unwrap_or_default()
}

/// Display name of a provider.
fn provider_name(cx: &Cx<'_>, id: &ProfileId) -> String {
    providers::list(cx.data)
        .into_iter()
        .find(|p| p.profile_id() == Some(id))
        .map(|p| view::provider_label(cx, &p))
        .unwrap_or_else(|| id.to_string())
}

/// Draw the page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if state.poll(cx, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    view::page(ui, cx, state, out);
}

/// Command palette entries: a context-window entry per listed model (the speed
/// tiers of the applied provider are shell entries).
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let Some(view) = &state.view else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if !view.read_only {
        for m in &view.list.models {
            items.push(PaletteItem {
                title: cx.tr.f("models.palette.context", &[("model", &m.slug)]),
                detail: m.display_name.clone(),
                keywords: vec![m.slug.clone(), "context".into(), "window".into()],
                group: PaletteGroup::Commands,
                action: Action::PageCommand {
                    page: Page::Models,
                    command: format!("context:{}", m.slug),
                },
            });
        }
    }
    items
}

/// Run a command addressed to this page (see the module docs).
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = out;
    if let Some(id) = command.strip_prefix("profile:") {
        state.select_profile(cx, ProfileId::new(id));
    } else if let Some(slug) = command.strip_prefix("context:") {
        state.query.clear();
        if let Some(view) = &state.view
            && let Some(spec) = view.list.model(slug)
        {
            let map = model_map(cx, view);
            let prices = saved_prices(cx, &view.profile_id);
            let saved = Saved {
                view,
                model_map: &map,
                prices: &prices,
            };
            if model::hidden(spec, &state.pending.override_of(&saved, slug)) {
                state.show_hidden = true;
            }
            let rows = model::rows(&saved, &state.pending, state.show_hidden, "");
            if let Some(i) = rows.iter().position(|m| m.slug == slug) {
                let total = rows.len() + 1;
                state.table.select(i + 1, total);
            }
        }
        state.focus = Some((slug.to_owned(), Field::Window));
    } else if let Some(tier) = command.strip_prefix("speed:") {
        if let Some(tier) = SpeedTier::from_config_value(tier) {
            state.apply_speed(cx, tier);
        }
    } else if command == "refresh" {
        state.task = None;
        state.load(cx, true);
    } else if command == "search" {
        state.focus_search = true;
    } else if command == "save" {
        state.save(cx);
    }
}

/// Dialogs and pending edits of the page.
pub fn status(state: &State) -> PageStatus {
    let pending = state.view.as_ref().map_or(0, |v| {
        // Without the app data the site map and prices are unknown here; the
        // count only needs to be non-zero for the sidebar dot and Ctrl/Cmd+S.
        let empty = BTreeMap::new();
        let prices = BTreeMap::new();
        state.pending.count(&Saved {
            view: v,
            model_map: &empty,
            prices: &prices,
        })
    });
    PageStatus {
        modal: state.diff_open || state.price.is_some(),
        pending,
        ..PageStatus::default()
    }
}

/// Notes worth a warning dot (not plain information).
fn warning(notes: &[Notice]) -> bool {
    notes
        .iter()
        .any(|n| n.severity != superba_core::Severity::Info)
}
