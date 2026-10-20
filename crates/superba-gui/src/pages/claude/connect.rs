//! Claude Code · connection and models (user bug 79).
//!
//! - **Provider**: Claude Code's own provider (default: the one Codex uses);
//!   direct connections need an Anthropic endpoint; the local router can convert protocols. **Route**: direct, or the local router (off
//!   until the router serves Claude Code: [`superba_core::ClaudeView::router_base_url`]).
//! - **Endpoint**: the provider's Anthropic base URL and how the key is sent,
//!   from a verified preset or typed (saved with the provider).
//! - **Models**: the provider's Codex list and its endpoint's standard list,
//!   merged; ids the endpoint cannot serve are marked "Codex only" and not
//!   offered for slots; ids can be added by hand. A model is put into slots
//!   (Opus / Sonnet / Haiku / Fable / custom, default / subagent / background),
//!   gets its capabilities (suggested from its Codex metadata or family) and
//!   can be added to the `/model` rows.
//! - **Apply** saves the configuration, shows what changes in
//!   `settings.json` and writes only the keys Superba owns (backed up first);
//!   **Restore** puts back what they held before Superba. New Claude Code
//!   sessions use it.

use std::collections::{BTreeSet, HashMap};

use egui::{Align, Layout, RichText, TextEdit, Ui, vec2};
use superba_core::claude::{
    AnthropicPreset, anthropic_preset, anthropic_presets, capabilities_text, effective_endpoint,
    suggested_capabilities,
};
use superba_core::{
    AnthropicAuth, AnthropicEndpoint, ClaudeApplyReport, ClaudeCapability, ClaudeConfig,
    ClaudeModelRow, ClaudeModels, ClaudeModelsRequest, ClaudeModelsView, ClaudePlan,
    ClaudePlanKind, ClaudeProbe, ClaudeRoute, ClaudeSlot, ClaudeView, ModelListKind, PickerRow,
    Profile, ProfileId, SlotModel,
};

use super::State;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, text};
use crate::design::widgets::{
    Badge, Button, Card, Dropdown, PagedState, PagedTable, Segmented, Status, StatusDot, Table,
    Text, Toggle, dialog, dialog_body,
};
use crate::pages::shared::loading::{self, Latest};
use crate::pages::{Action, Cx, Page, PageStatus, error_text, page_header};
use crate::runtime::ReadTask;

/// Time limit of a model list load.
const MODELS_LIMIT: std::time::Duration = std::time::Duration::from_secs(45);
/// Width of the detail pane.
const DETAIL_W: f32 = 380.0;
/// Height of the top cards.
const CARDS_H: f32 = 148.0;
/// Gap between panes.
const GAP: f32 = 12.0;

/// The endpoint dialog's edits.
#[derive(Clone, Debug)]
pub struct EndpointEdit {
    /// The provider.
    pub provider: ProfileId,
    /// Preset id (`None` = typed URL).
    pub preset: Option<String>,
    /// Typed base URL.
    pub url: String,
    /// Key style.
    pub auth: AnthropicAuth,
    /// Why the URL is refused (i18n key).
    pub error: Option<&'static str>,
}

/// Page state.
#[derive(Default)]
pub struct ConnectState {
    /// Edited configuration (`None` = as saved).
    pub draft: Option<ClaudeConfig>,
    /// Model lists per provider (each shown only for its own provider).
    pub models: HashMap<ProfileId, ClaudeModelsView>,
    /// The load of the chosen provider's models (user bug 83: the last
    /// choice wins, a failure or timeout is shown with Retry).
    pub load: Latest<ProfileId, ClaudeModelsView>,
    /// Search text of the model list.
    pub search: String,
    /// The model shown in the detail pane.
    pub selected: Option<String>,
    /// "Add model id" field.
    pub manual: String,
    /// The endpoint dialog.
    pub endpoint: Option<EndpointEdit>,
    /// Paging of the model list.
    pub table: PagedState,
    save: Option<(ReadTask<ClaudeView>, ClaudePlanKind)>,
    plan_task: Option<ReadTask<ClaudePlan>>,
    /// The plan shown for confirmation.
    pub plan: Option<ClaudePlan>,
    /// "Overwrite keys someone changed" in the plan dialog.
    pub take_over: bool,
    apply_task: Option<ReadTask<ClaudeApplyReport>>,
    /// The last apply / restore (the "new sessions use it" banner).
    pub report: Option<ClaudeApplyReport>,
    probe_task: Option<ReadTask<ClaudeProbe>>,
    /// The last endpoint check.
    pub probe: Option<ClaudeProbe>,
    /// The last error of a save, plan or apply.
    pub error: Option<String>,
}

impl std::fmt::Debug for ConnectState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectState")
            .field("dirty", &self.draft.is_some())
            .field("selected", &self.selected)
            .field("plan", &self.plan.is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectState {
    /// A new view arrived: drop a draft that equals it.
    pub fn on_view(&mut self, view: &ClaudeView) {
        if self.draft.as_ref() == Some(&view.config) {
            self.draft = None;
        }
    }

    /// The configuration as edited.
    pub fn config(&self, view: &ClaudeView) -> ClaudeConfig {
        self.draft.clone().unwrap_or_else(|| view.config.clone())
    }

    /// Unsaved edits.
    pub fn dirty(&self, view: &ClaudeView) -> bool {
        self.draft.as_ref().is_some_and(|d| d != &view.config)
    }

    fn edit(&mut self, view: &ClaudeView, f: impl FnOnce(&mut ClaudeConfig)) {
        let mut c = self.config(view);
        f(&mut c);
        self.draft = (c != view.config).then_some(c);
    }

    /// Change the models of `provider`.
    pub fn edit_models(
        &mut self,
        view: &ClaudeView,
        provider: &ProfileId,
        f: impl FnOnce(&mut ClaudeModels),
    ) {
        let provider = provider.clone();
        self.edit(view, move |c| {
            let mut m = c.models_of(&provider);
            f(&mut m);
            if m.is_empty() {
                c.models.remove(&provider);
            } else {
                c.models.insert(provider, m);
            }
        });
    }

    /// Something runs.
    pub fn busy(&self) -> bool {
        self.save.is_some() || self.plan_task.is_some() || self.apply_task.is_some()
    }

    /// Read the models of `provider` (`refresh` = ask the provider again):
    /// it becomes the chosen provider; a load of another one is dropped.
    pub fn load_models(&mut self, cx: &Cx<'_>, provider: &ProfileId, refresh: bool) {
        if !refresh && self.load.is_chosen(provider) && self.load.loading() {
            return;
        }
        let service = cx.service.clone();
        let request = ClaudeModelsRequest {
            provider: Some(provider.clone()),
            refresh,
        };
        let task = crate::runtime::task(async move { service.claude_models(request).await });
        self.load.start(provider.clone(), task, MODELS_LIMIT);
    }

    /// Save the draft, then plan.
    fn save_then(&mut self, cx: &Cx<'_>, view: &ClaudeView, then: ClaudePlanKind) {
        if self.busy() {
            return;
        }
        self.error = None;
        if !self.dirty(view) {
            self.start_plan(cx, then);
            return;
        }
        let config = self.config(view);
        let service = cx.service.clone();
        self.save = Some((
            crate::runtime::task(async move { service.save_claude_config(config).await }),
            then,
        ));
    }

    fn start_plan(&mut self, cx: &Cx<'_>, kind: ClaudePlanKind) {
        let service = cx.service.clone();
        self.take_over = false;
        self.plan_task = Some(crate::runtime::task(async move {
            service.claude_plan(kind).await
        }));
    }

    fn apply(&mut self, cx: &Cx<'_>) {
        let Some(plan) = self.plan.take() else {
            return;
        };
        let service = cx.service.clone();
        let take_over = self.take_over;
        self.apply_task = Some(crate::runtime::task(async move {
            service.claude_apply(plan, take_over).await
        }));
    }

    fn probe(&mut self, cx: &Cx<'_>, model: Option<String>) {
        if self.probe_task.is_some() {
            return;
        }
        let service = cx.service.clone();
        self.probe = None;
        self.probe_task = Some(crate::runtime::task(async move {
            service.claude_probe(model).await
        }));
    }
}

/// Poll background work; true while any runs.
fn poll(state: &mut State, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
    let mut busy = false;
    let s = &mut state.connect;
    if let Some((provider, list)) = s.load.poll() {
        s.models.insert(provider, list);
    }
    busy |= s.load.loading();
    if let Some((t, then)) = &s.save {
        let then = *then;
        match t.poll() {
            Some(Ok(view)) => {
                s.save = None;
                s.draft = None;
                state.set_view(view);
                state.connect.start_plan(cx, then);
            }
            Some(Err(e)) => {
                s.save = None;
                s.error = Some(error_text(cx.tr, &e));
            }
            None => busy = true,
        }
    }
    let s = &mut state.connect;
    if let Some(t) = &s.plan_task {
        match t.poll() {
            Some(Ok(plan)) => {
                s.plan_task = None;
                if plan.is_empty() {
                    out.push(Action::Toast {
                        status: Status::Ok,
                        text: cx.tr.t("claude.apply.nothing"),
                    });
                } else {
                    s.plan = Some(plan);
                }
            }
            Some(Err(e)) => {
                s.plan_task = None;
                s.error = Some(error_text(cx.tr, &e));
            }
            None => busy = true,
        }
    }
    if let Some(t) = &s.apply_task {
        match t.poll() {
            Some(Ok(report)) => {
                s.apply_task = None;
                out.push(Action::Toast {
                    status: Status::Ok,
                    text: cx.tr.f(
                        if report.kind == ClaudePlanKind::Restore {
                            "claude.apply.restored"
                        } else {
                            "claude.apply.done"
                        },
                        &[("n", &report.changed.len())],
                    ),
                });
                s.report = Some(report);
                state.reload(cx);
                state.settings.file_changed();
            }
            Some(Err(e)) => {
                s.apply_task = None;
                s.error = Some(error_text(cx.tr, &e));
            }
            None => busy = true,
        }
    }
    let s = &mut state.connect;
    if let Some(t) = &s.probe_task {
        match t.poll() {
            Some(Ok(p)) => {
                s.probe_task = None;
                s.probe = Some(p);
            }
            Some(Err(e)) => {
                s.probe_task = None;
                s.probe = Some(ClaudeProbe {
                    model: String::new(),
                    url: String::new(),
                    status: None,
                    latency_ms: 0,
                    ok: false,
                    message: Some(error_text(cx.tr, &e)),
                });
            }
            None => busy = true,
        }
    }
    busy
}

/// Providers with a key, with their Anthropic endpoint (if any).
fn providers<'a>(cx: &'a Cx<'_>) -> Vec<(&'a Profile, Option<AnthropicEndpoint>)> {
    cx.data
        .providers()
        .into_iter()
        .filter(|p| p.key_ref.is_some())
        .map(|p| {
            let e = cx
                .data
                .site(&p.site_id)
                .and_then(|s| effective_endpoint(&s, p));
            (p, e)
        })
        .collect()
}

fn provider_label(cx: &Cx<'_>, p: &Profile) -> String {
    match cx.data.site(&p.site_id) {
        Some(site) => crate::app::provider_name(cx.tr, p, &site),
        None => p.name.clone(),
    }
}

/// The provider Claude Code uses after the draft: the configured one, else
/// the one the view resolved.
fn current_provider(view: &ClaudeView, config: &ClaudeConfig) -> Option<ProfileId> {
    config.provider.clone().or_else(|| {
        if view.config.provider.is_none() {
            view.provider.clone()
        } else {
            None
        }
    })
}

/// Model rows with the ids added by hand in the draft.
pub fn rows(list: Option<&ClaudeModelsView>, models: &ClaudeModels) -> Vec<ClaudeModelRow> {
    let mut out: Vec<ClaudeModelRow> = list.map(|l| l.rows.clone()).unwrap_or_default();
    out.retain(|r| !r.manual || models.extra_models.contains(&r.id));
    for id in models
        .extra_models
        .iter()
        .chain(models.named_models().iter())
    {
        if !out.iter().any(|r| &r.id == id) {
            out.push(ClaudeModelRow {
                id: id.clone(),
                display_name: None,
                lists: Vec::new(),
                manual: true,
                usable: true,
                context_window: None,
                reasoning_levels: Vec::new(),
                suggested: suggested_capabilities(id, None),
            });
        }
    }
    out
}

/// Draw the page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if poll(state, cx, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    let loading = state.loading();
    page_header(ui, cx, Page::ClaudeCode, |ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.reload"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(loading)
                    .enabled(!loading),
            )
            .clicked()
        {
            state.reload(cx);
        }
    });
    let Some(view) = state.view.clone() else {
        match &state.view_error {
            Some(e) => crate::pages::error_state(ui, cx, &cx.tr.t("claude.load_failed"), e),
            None => {
                ui.label(Text::muted(cx.tr.t("claude.loading"), cx.pal));
            }
        }
        return;
    };
    let config = state.connect.config(&view);
    let provider = current_provider(&view, &config);
    if let Some(p) = &provider
        && !state.connect.models.contains_key(p)
        && !state.connect.load.is_chosen(p)
    {
        state.connect.load_models(cx, p, false);
    }

    let body = ui.available_rect_before_wrap();
    let mut tracks = Vec::new();
    let banner = state.connect.report.is_some() || state.connect.error.is_some();
    if banner {
        tracks.push(Track::Fixed(30.0));
    }
    tracks.push(Track::Fixed(CARDS_H));
    tracks.push(Track::Fill);
    tracks.push(Track::Fixed(size::FOOTER));
    let rects = layout::row_rects(body, &tracks, 8.0);
    let mut i = 0;
    if banner {
        layout::fixed(ui, rects[0], |ui| banner_line(ui, cx, &mut state.connect));
        i = 1;
    }
    layout::fixed(ui, rects[i], |ui| cards(ui, cx, state, &view, &config, out));
    let panes = layout::column_rects(rects[i + 1], &[Track::Fill, Track::Fixed(DETAIL_W)], GAP);
    layout::fixed(ui, panes[0], |ui| model_list(ui, cx, state, &view, &config));
    ui.painter().vline(
        panes[1].min.x - GAP / 2.0,
        panes[1].y_range(),
        egui::Stroke::new(1.0, cx.pal.border),
    );
    layout::fixed(ui, panes[1], |ui| detail(ui, cx, state, &view, &config));
    layout::fixed(ui, rects[i + 2], |ui| footer(ui, cx, state, &view, &config));
    if state.connect.endpoint.is_some() {
        endpoint_dialog(ui.ctx(), cx, state, out);
    }
    if state.connect.plan.is_some() {
        plan_dialog(ui.ctx(), cx, state);
    }
}

fn banner_line(ui: &mut Ui, cx: &Cx<'_>, s: &mut ConnectState) {
    ui.horizontal(|ui| {
        let (status, msg) = match (&s.error, &s.report) {
            (Some(e), _) => (Status::Error, e.clone()),
            (None, Some(r)) if r.kind == ClaudePlanKind::Restore => {
                (Status::Ok, cx.tr.t("claude.banner.restored"))
            }
            _ => (Status::Ok, cx.tr.t("claude.banner.new_sessions")),
        };
        ui.add(StatusDot::new(status));
        ui.add(egui::Label::new(Text::small(msg, cx.pal)).truncate());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::ghost(cx.tr.t("claude.banner.dismiss")).small())
                .clicked()
            {
                s.report = None;
                s.error = None;
            }
        });
    });
}

fn card_title(ui: &mut Ui, cx: &Cx<'_>, title: &str) {
    ui.label(
        RichText::new(title)
            .font(font(text::CAPTION, Weight::Medium))
            .color(cx.pal.text_subtle),
    );
}

/// Provider + route, endpoint, status.
fn cards(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
    out: &mut Vec<Action>,
) {
    let cols = layout::column_rects(ui.max_rect(), &[Track::Fill, Track::Fill, Track::Fill], GAP);
    let provider = current_provider(view, config);
    layout::fixed(ui, cols[0], |ui| {
        Card::new().padding(12).fill_height().show(ui, |ui| {
            ui.set_width(ui.available_width());
            card_title(ui, cx, &cx.tr.t("claude.provider.title"));
            provider_picker(ui, cx, state, view, config);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(Text::small(cx.tr.t("claude.route.title"), cx.pal));
                let mut route = config.route;
                let router_ready = view.router_base_url.is_some();
                let mut seg = Segmented::new("claude-route", &mut route)
                    .label(cx.tr.t("claude.route.title"))
                    .small()
                    .option(ClaudeRoute::Direct, cx.tr.t("claude.route.direct"));
                seg = if router_ready {
                    seg.option(ClaudeRoute::Router, cx.tr.t("claude.route.router"))
                } else {
                    seg.disabled_option(
                        ClaudeRoute::Router,
                        cx.tr.t("claude.route.router"),
                        cx.tr.t("claude.route.router_soon"),
                    )
                };
                seg.show(ui);
                if route != config.route {
                    state.connect.edit(view, |c| c.route = route);
                }
            });
            if config.route == ClaudeRoute::Router
                && let Some(url) = &view.router_base_url
            {
                ui.add(
                    egui::Label::new(Text::caption(
                        cx.tr.f("claude.route.via", &[("url", &url.as_str())]),
                        cx.pal,
                    ))
                    .truncate(),
                );
            }
        });
    });
    layout::fixed(ui, cols[1], |ui| {
        Card::new().padding(12).fill_height().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                card_title(ui, cx, &cx.tr.t("claude.endpoint.title"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(p) = &provider
                        && ui
                            .add(Button::ghost(cx.tr.t("claude.endpoint.edit")).small())
                            .clicked()
                    {
                        let endpoint = endpoint_of(cx, p);
                        state.connect.endpoint = Some(EndpointEdit {
                            provider: p.clone(),
                            preset: endpoint.as_ref().and_then(|e| e.preset.clone()),
                            url: endpoint.as_ref().map(|e| e.base_text()).unwrap_or_default(),
                            auth: endpoint.map(|e| e.auth).unwrap_or_default(),
                            error: None,
                        });
                    }
                });
            });
            match provider.as_ref().and_then(|p| endpoint_of(cx, p)) {
                Some(e) => {
                    ui.add(egui::Label::new(Text::mono(e.base_text(), cx.pal)).truncate());
                    ui.horizontal(|ui| {
                        let preset = e.preset.as_deref().and_then(anthropic_preset);
                        if let Some(p) = &preset {
                            ui.add(Badge::new(cx.tr.text(&p.label())).outline());
                        }
                        ui.add(Badge::new(auth_label(cx, e.auth)).outline());
                        if view.endpoint_suggested && e.preset.is_some() {
                            ui.label(Text::caption(cx.tr.t("claude.endpoint.suggested"), cx.pal));
                        }
                    });
                    probe_line(ui, cx, state, config, provider.as_ref());
                }
                None => {
                    ui.add(
                        egui::Label::new(Text::small(cx.tr.t("claude.endpoint.none"), cx.pal))
                            .wrap(),
                    );
                }
            }
        });
    });
    layout::fixed(ui, cols[2], |ui| {
        Card::new().padding(12).fill_height().show(ui, |ui| {
            ui.set_width(ui.available_width());
            card_title(ui, cx, &cx.tr.t("claude.status.title"));
            status_lines(ui, cx, view, out);
        });
    });
}

fn auth_label(cx: &Cx<'_>, a: AnthropicAuth) -> String {
    cx.tr.t(match a {
        AnthropicAuth::Bearer => "claude.endpoint.auth_bearer",
        AnthropicAuth::ApiKey => "claude.endpoint.auth_key",
    })
}

/// The endpoint of a provider (its own, else the suggested preset's).
fn endpoint_of(cx: &Cx<'_>, provider: &ProfileId) -> Option<AnthropicEndpoint> {
    let p = cx.data.profile(provider)?;
    let site = cx.data.site(&p.site_id)?;
    effective_endpoint(&site, p)
}

fn probe_line(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    config: &ClaudeConfig,
    provider: Option<&ProfileId>,
) {
    let s = &mut state.connect;
    ui.horizontal(|ui| {
        let model = provider.and_then(|p| {
            let m = config.models_of(p);
            m.main
                .clone()
                .or_else(|| m.slots.values().next().map(|s| s.model.clone()))
        });
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.endpoint.check"))
                    .small()
                    .loading(s.probe_task.is_some())
                    .enabled(model.is_some() && s.probe_task.is_none()),
            )
            .on_hover_text(cx.tr.t("claude.endpoint.check_tip"))
            .clicked()
        {
            s.probe(cx, model);
        }
        if let Some(p) = &s.probe {
            let (status, msg) = if p.ok {
                (
                    Status::Ok,
                    cx.tr.f(
                        "claude.endpoint.check_ok",
                        &[("ms", &p.latency_ms), ("model", &p.model)],
                    ),
                )
            } else {
                (
                    Status::Error,
                    cx.tr.f(
                        "claude.endpoint.check_failed",
                        &[(
                            "error",
                            &p.message.clone().unwrap_or_else(|| {
                                p.status.map(|s| format!("HTTP {s}")).unwrap_or_default()
                            }),
                        )],
                    ),
                )
            };
            ui.add(StatusDot::new(status));
            ui.add(egui::Label::new(Text::caption(msg.clone(), cx.pal)).truncate())
                .on_hover_text(msg);
        }
    });
}

fn status_lines(ui: &mut Ui, cx: &Cx<'_>, view: &ClaudeView, out: &mut Vec<Action>) {
    let line = |ui: &mut Ui, status: Status, text: String| {
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(status));
            ui.add(egui::Label::new(Text::small(text.clone(), cx.pal)).truncate())
                .on_hover_text(text);
        });
    };
    match &view.install {
        Some(i) => line(
            ui,
            Status::Ok,
            cx.tr.f(
                "claude.status.found",
                &[("version", &i.version.clone().unwrap_or_else(|| "?".into()))],
            ),
        ),
        None => line(ui, Status::Warn, cx.tr.t("claude.status.not_found")),
    }
    let path = superba_core::paths::display_path(&view.settings_path);
    match &view.file {
        superba_core::ClaudeFileStatus::Absent => line(
            ui,
            Status::Idle,
            cx.tr.f("claude.status.file_absent", &[("path", &path)]),
        ),
        superba_core::ClaudeFileStatus::Valid => line(
            ui,
            Status::Ok,
            cx.tr.f("claude.status.file_ok", &[("path", &path)]),
        ),
        _ => line(
            ui,
            Status::Error,
            cx.tr.f("claude.status.file_invalid", &[("path", &path)]),
        ),
    }
    match &view.applied {
        Some(a) if !a.drift.is_empty() => line(
            ui,
            Status::Warn,
            cx.tr.f("claude.status.drift", &[("n", &a.drift.len())]),
        ),
        Some(_) if !view.pending.is_empty() => line(
            ui,
            Status::Warn,
            cx.tr
                .f("claude.status.pending", &[("n", &view.pending.len())]),
        ),
        Some(a) => line(
            ui,
            Status::Ok,
            cx.tr.f(
                "claude.status.applied",
                &[("n", &a.owned.len()), ("at", &cx.ago(a.at))],
            ),
        ),
        None => line(ui, Status::Idle, cx.tr.t("claude.status.not_applied")),
    }
    let _ = out;
}

fn provider_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
) {
    let list = providers(cx);
    let follows = config.provider.is_none();
    let current = current_provider(view, config);
    let selected = match (&current, follows) {
        (Some(id), true) => cx.tr.f(
            "claude.provider.follows_named",
            &[(
                "name",
                &cx.data
                    .profile(id)
                    .map(|p| provider_label(cx, p))
                    .unwrap_or_else(|| id.to_string()),
            )],
        ),
        (Some(id), false) => cx
            .data
            .profile(id)
            .map(|p| provider_label(cx, p))
            .unwrap_or_else(|| id.to_string()),
        (None, _) => cx.tr.t("claude.provider.none"),
    };
    let mut pick: Option<Option<ProfileId>> = None;
    Dropdown::new("claude-provider")
        .label(cx.tr.t("claude.provider.title"))
        .selected_text(selected)
        .loading(state.connect.load.loading())
        .width(ui.available_width().min(320.0))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(follows, cx.tr.t("claude.provider.follow"))
                .clicked()
            {
                pick = Some(None);
            }
            for (p, endpoint) in &list {
                let label = provider_label(cx, p);
                let on = !follows && config.provider.as_ref() == Some(&p.id);
                if endpoint.is_some() || config.route == ClaudeRoute::Router {
                    if ui.selectable_label(on, label).clicked() {
                        pick = Some(Some(p.id.clone()));
                    }
                } else {
                    ui.add_enabled(false, egui::Button::selectable(false, label))
                        .on_disabled_hover_text(
                            cx.tr
                                .text(&superba_core::ClaudeIneligible::NoAnthropicEndpoint.text()),
                        );
                }
            }
        });
    if let Some(p) = pick {
        state.connect.edit(view, |c| c.provider = p.clone());
        if let Some(id) = p.or_else(|| view.provider.clone()) {
            state.connect.load_models(cx, &id, false);
        }
        state.connect.selected = None;
    }
    if let Some(id) = &current
        && endpoint_of(cx, id).is_none()
    {
        ui.add(
            egui::Label::new(Text::caption(
                if config.route == ClaudeRoute::Router {
                    cx.tr.t("claude_routing.translated")
                } else {
                    cx.tr
                        .text(&superba_core::ClaudeIneligible::NoAnthropicEndpoint.text())
                },
                cx.pal,
            ))
            .wrap(),
        );
    }
}

/// Rows of the list for the search text.
fn filtered(rows: &[ClaudeModelRow], query: &str) -> Vec<ClaudeModelRow> {
    let q = query.trim().to_lowercase();
    rows.iter()
        .filter(|r| {
            q.is_empty()
                || r.id.to_lowercase().contains(&q)
                || r.display_name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
        })
        .cloned()
        .collect()
}

fn slot_label(cx: &Cx<'_>, slot: ClaudeSlot) -> String {
    cx.tr.t(match slot {
        ClaudeSlot::Opus => "claude.slot.opus",
        ClaudeSlot::Sonnet => "claude.slot.sonnet",
        ClaudeSlot::Haiku => "claude.slot.haiku",
        ClaudeSlot::Fable => "claude.slot.fable",
        ClaudeSlot::Custom => "claude.slot.custom",
    })
}

fn model_list(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
) {
    let Some(provider) = current_provider(view, config) else {
        crate::pages::empty_state(
            ui,
            cx,
            Icon::Model,
            &cx.tr.t("claude.models.no_provider_title"),
            &cx.tr.t("claude.models.no_provider_body"),
        );
        return;
    };
    let models = config.models_of(&provider);
    let value = state.connect.models.get(&provider).cloned();
    let value = value.as_ref();
    let chosen = state.connect.load.is_chosen(&provider);
    let loading = chosen && state.connect.load.loading();
    let failure = if chosen {
        state.connect.load.failure().map(|f| f.text(cx))
    } else {
        None
    };
    let all = rows(value, &models);
    let sources = value.map(|v| v.sources.clone()).unwrap_or_default();
    let name = cx
        .data
        .profile(&provider)
        .map(|p| provider_label(cx, p))
        .unwrap_or_else(|| provider.to_string());
    // Toolbar: search, pull, list status.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.add(
            TextEdit::singleline(&mut state.connect.search)
                .id(egui::Id::unique("claude-models-search"))
                .hint_text(cx.tr.t("claude.models.search"))
                .desired_width(200.0)
                .margin(vec2(10.0, 6.0)),
        )
        .accessible_name(cx.tr.t("claude.models.search"));
        if ui
            .add(
                Button::new(cx.tr.t("claude.models.pull"))
                    .icon(Icon::Download)
                    .small()
                    .loading(loading)
                    .enabled(!loading),
            )
            .on_hover_text(cx.tr.t("claude.models.pull_tip"))
            .clicked()
        {
            state.connect.load_models(cx, &provider, true);
        }
        for s in &sources {
            let name = cx.tr.t(match s.kind {
                ModelListKind::Codex => "claude.models.list_codex",
                ModelListKind::Standard => "claude.models.list_standard",
            });
            match &s.error {
                None => {
                    ui.add(Badge::new(format!("{name} {}", s.count)).outline())
                        .on_hover_text(s.path.clone().unwrap_or_default());
                }
                Some(e) => {
                    ui.add(Badge::new(name).status(Status::Warn))
                        .on_hover_text(error_text(cx.tr, e));
                }
            }
        }
    });
    // Never another provider's list: the loading state or the failure (with
    // Retry) in its place until this provider's list is there.
    if value.is_none() {
        if let Some(f) = &failure {
            let title = cx.tr.f("claude.models.load_failed", &[("provider", &name)]);
            if loading::failed_card(ui, cx, &title, f) {
                state.connect.load_models(cx, &provider, true);
            }
            return;
        }
        if loading {
            loading::spinner_row(
                ui,
                cx,
                &cx.tr.f("claude.models.loading", &[("provider", &name)]),
            );
            return;
        }
    } else if let Some(f) = &failure {
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Warn));
            ui.add(
                egui::Label::new(Text::caption(
                    cx.tr.f("claude.models.refresh_failed", &[("error", f)]),
                    cx.pal,
                ))
                .truncate(),
            )
            .on_hover_text(f.clone());
        });
    }
    let shown = filtered(&all, &state.connect.search);
    // Room for the manual entry under the list.
    let list_rect = {
        let r = ui.available_rect_before_wrap();
        egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, r.max.y - size::CONTROL - 8.0))
    };
    let selected_id = state.connect.selected.clone();
    if let Some(id) = &selected_id
        && let Some(i) = shown.iter().position(|r| &r.id == id)
        && state.connect.table.selected != Some(i)
    {
        state.connect.table.selected = Some(i);
    }
    layout::fixed(ui, list_rect, |ui| {
        if shown.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label(Text::strong(cx.tr.t("claude.models.empty_title"), cx.pal));
                ui.label(Text::small(cx.tr.t("claude.models.empty_body"), cx.pal));
            });
            return;
        }
        let columns = Table::new(&[1.0, 0.0]).min_widths(&[0.0, 150.0]);
        let total = shown.len();
        let r = PagedTable::new("claude-models", columns, total, &mut state.connect.table)
            .label(cx.tr.t("claude.models.title"))
            .row_height(size::ROW_LIST2)
            .row_name(|i| shown[i].id.clone())
            .show(ui, |ui, row, col| {
                let m = &shown[row];
                if col == 0 {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let color = if m.usable {
                                cx.pal.text
                            } else {
                                cx.pal.text_disabled
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&m.id)
                                        .font(mono_font(text::SMALL, Weight::Medium))
                                        .color(color),
                                )
                                .truncate(),
                            );
                            for slot in models.slots_of(&m.id) {
                                ui.add(Badge::new(slot_label(cx, slot)).status(Status::Ok));
                            }
                            if models.main.as_deref() == Some(m.id.as_str()) {
                                ui.add(Badge::new(cx.tr.t("claude.role.main")).status(Status::Ok));
                            }
                        });
                        let mut second = Vec::new();
                        if let Some(n) = &m.display_name {
                            second.push(n.clone());
                        }
                        second.push(if m.manual {
                            cx.tr.t("claude.models.manual")
                        } else {
                            m.lists
                                .iter()
                                .map(|k| {
                                    cx.tr.t(match k {
                                        ModelListKind::Codex => "claude.models.list_codex",
                                        ModelListKind::Standard => "claude.models.list_standard",
                                    })
                                })
                                .collect::<Vec<_>>()
                                .join(" · ")
                        });
                        if !m.usable {
                            second.push(cx.tr.t("claude.models.codex_only"));
                        }
                        ui.add(
                            egui::Label::new(Text::caption(second.join(" · "), cx.pal)).truncate(),
                        );
                    });
                } else {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !m.usable {
                            ui.add(Badge::new(cx.tr.t("claude.models.codex_only")).outline())
                                .on_hover_text(cx.tr.t("claude.models.codex_only_tip"));
                        } else if models.picker.iter().any(|p| p.model == m.id) {
                            ui.add(Badge::new(cx.tr.t("claude.models.in_picker")).outline());
                        }
                    });
                }
            });
        if r.selection_changed
            && let Some(i) = state.connect.table.selected
        {
            state.connect.selected = shown.get(i).map(|r| r.id.clone());
        }
    });
    // Add a model id by hand.
    let manual_rect = egui::Rect::from_min_max(
        egui::pos2(list_rect.min.x, list_rect.max.y + 8.0),
        egui::pos2(list_rect.max.x, list_rect.max.y + 8.0 + size::CONTROL),
    );
    layout::fixed(ui, manual_rect, |ui| {
        ui.horizontal(|ui| {
            let field = ui
                .add(
                    TextEdit::singleline(&mut state.connect.manual)
                        .hint_text(cx.tr.t("claude.models.add_hint"))
                        .desired_width(240.0)
                        .font(mono_font(text::SMALL, Weight::Regular))
                        .margin(vec2(10.0, 6.0)),
                )
                .accessible_name(cx.tr.t("claude.models.add"));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let id = state.connect.manual.trim().to_owned();
            if (ui
                .add(
                    Button::ghost(cx.tr.t("claude.models.add"))
                        .icon(Icon::Plus)
                        .small()
                        .enabled(!id.is_empty()),
                )
                .clicked()
                || enter)
                && !id.is_empty()
            {
                state.connect.edit_models(view, &provider, |m| {
                    if !m.extra_models.contains(&id) {
                        m.extra_models.push(id.clone());
                    }
                });
                state.connect.selected = Some(id);
                state.connect.manual.clear();
            }
        });
    });
}

fn detail(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &ClaudeView, config: &ClaudeConfig) {
    let Some(provider) = current_provider(view, config) else {
        return;
    };
    let models = config.models_of(&provider);
    let all = rows(state.connect.models.get(&provider), &models);
    let Some(id) = state.connect.selected.clone() else {
        summary(ui, cx, state, view, &provider, &models);
        return;
    };
    let Some(row) = all.iter().find(|r| r.id == id).cloned() else {
        state.connect.selected = None;
        return;
    };
    egui::ScrollArea::vertical()
        .id_salt("claude-model-detail")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&row.id)
                        .font(mono_font(text::BODY, Weight::Semibold))
                        .color(cx.pal.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(crate::design::widgets::IconButton::new(
                            Icon::Close,
                            cx.tr.t("claude.detail.close"),
                        ))
                        .clicked()
                    {
                        state.connect.selected = None;
                    }
                });
            });
            if let Some(n) = &row.display_name {
                ui.label(Text::small(n, cx.pal));
            }
            let mut facts = Vec::new();
            if let Some(w) = row.context_window {
                facts.push(cx.tr.f("claude.detail.context", &[("n", &w)]));
            }
            if !row.reasoning_levels.is_empty() {
                facts.push(cx.tr.f(
                    "claude.detail.levels",
                    &[("levels", &row.reasoning_levels.join(", "))],
                ));
            }
            if !facts.is_empty() {
                ui.label(Text::caption(facts.join(" · "), cx.pal));
            }
            if !row.usable {
                crate::pages::note(
                    ui,
                    cx,
                    Status::Warn,
                    cx.tr.t("claude.models.codex_only_tip"),
                );
                return;
            }
            ui.add_space(6.0);
            ui.label(Text::strong(cx.tr.t("claude.detail.use_as"), cx.pal));
            let mut changed: Option<ClaudeModels> = None;
            let mut m = models.clone();
            ui.horizontal_wrapped(|ui| {
                for slot in ClaudeSlot::ALL {
                    let mut on = m.slots.get(&slot).is_some_and(|s| s.model == row.id);
                    let before = on;
                    ui.add(Toggle::new(&mut on, slot_label(cx, slot)));
                    ui.label(Text::small(slot_label(cx, slot), cx.pal));
                    if on != before {
                        if on {
                            m.slots.insert(slot, SlotModel::new(row.id.clone()));
                        } else {
                            m.slots.remove(&slot);
                        }
                        changed = Some(m.clone());
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (key, field) in [
                    ("claude.role.main", 0),
                    ("claude.role.subagent", 1),
                    ("claude.role.background", 2),
                ] {
                    let slot = match field {
                        0 => &mut m.main,
                        1 => &mut m.subagent,
                        _ => &mut m.small_fast,
                    };
                    let mut on = slot.as_deref() == Some(row.id.as_str());
                    let before = on;
                    ui.add(Toggle::new(&mut on, cx.tr.t(key)));
                    ui.label(Text::small(cx.tr.t(key), cx.pal))
                        .on_hover_text(cx.tr.t(&format!("{key}_tip")));
                    if on != before {
                        *slot = on.then(|| row.id.clone());
                        changed = Some(m.clone());
                    }
                }
            });
            // Row text of the slots this model is in.
            for slot in m.slots_of(&row.id) {
                let s = m.slots.get_mut(&slot).expect("slot listed");
                ui.add_space(4.0);
                ui.label(Text::caption(
                    cx.tr
                        .f("claude.detail.row_text", &[("slot", &slot_label(cx, slot))]),
                    cx.pal,
                ));
                let mut name = s.name.clone().unwrap_or_default();
                let mut desc = s.description.clone().unwrap_or_default();
                let r1 = ui
                    .add(
                        TextEdit::singleline(&mut name)
                            .hint_text(cx.tr.t("claude.detail.name_hint"))
                            .desired_width(f32::INFINITY),
                    )
                    .accessible_name(cx.tr.t("claude.detail.name_hint"));
                let r2 = ui
                    .add(
                        TextEdit::singleline(&mut desc)
                            .hint_text(cx.tr.t("claude.detail.description_hint"))
                            .desired_width(f32::INFINITY),
                    )
                    .accessible_name(cx.tr.t("claude.detail.description_hint"));
                if r1.changed() || r2.changed() {
                    s.name = (!name.trim().is_empty()).then_some(name);
                    s.description = (!desc.trim().is_empty()).then_some(desc);
                    changed = Some(m.clone());
                }
            }
            // Capabilities.
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(Text::strong(cx.tr.t("claude.caps.title"), cx.pal));
                let own = m
                    .capabilities
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case(&row.id));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if own
                        && ui
                            .add(Button::ghost(cx.tr.t("claude.caps.suggested_reset")).small())
                            .clicked()
                    {
                        m.capabilities
                            .retain(|k, _| !k.eq_ignore_ascii_case(&row.id));
                        changed = Some(m.clone());
                    } else if !own {
                        ui.label(Text::caption(cx.tr.t("claude.caps.suggested"), cx.pal));
                    }
                });
            });
            let caps = m
                .capabilities_of(&row.id, None)
                .union(&BTreeSet::new())
                .copied()
                .collect::<BTreeSet<_>>();
            let caps = if m
                .capabilities
                .keys()
                .any(|k| k.eq_ignore_ascii_case(&row.id))
            {
                caps
            } else {
                row.suggested.clone()
            };
            let mut next = caps.clone();
            egui::Grid::new("claude-caps")
                .num_columns(2)
                .spacing(vec2(12.0, 4.0))
                .show(ui, |ui| {
                    for (i, c) in ClaudeCapability::ALL.into_iter().enumerate() {
                        let mut on = next.contains(&c);
                        let r = ui.checkbox(&mut on, c.as_str());
                        r.on_hover_text(cx.tr.t(&format!("claude.caps.{}", c.as_str())));
                        if on {
                            next.insert(c);
                        } else {
                            next.remove(&c);
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
            if next != caps {
                m.capabilities
                    .retain(|k, _| !k.eq_ignore_ascii_case(&row.id));
                m.capabilities.insert(row.id.clone(), next.clone());
                changed = Some(m.clone());
            }
            ui.label(Text::caption(
                cx.tr.f(
                    "claude.caps.written",
                    &[("list", &capabilities_text(&next))],
                ),
                cx.pal,
            ));
            // /model rows.
            ui.add_space(8.0);
            let mut in_picker = m.picker.iter().any(|p| p.model == row.id);
            let before = in_picker;
            ui.horizontal(|ui| {
                ui.add(Toggle::new(&mut in_picker, cx.tr.t("claude.picker.add")));
                ui.label(Text::strong(cx.tr.t("claude.picker.add"), cx.pal))
                    .on_hover_text(cx.tr.t("claude.picker.add_tip"));
            });
            if in_picker != before {
                if in_picker {
                    m.add_to_picker(&row.id);
                } else {
                    m.picker.retain(|p| p.model != row.id);
                }
                changed = Some(m.clone());
            }
            let in_slot = !m.slots_of(&row.id).is_empty();
            if let Some(p) = m.picker.iter_mut().find(|p| p.model == row.id) {
                if in_slot {
                    ui.add(
                        egui::Label::new(Text::caption(cx.tr.t("claude.picker.slot_row"), cx.pal))
                            .wrap(),
                    );
                }
                if picker_fields(ui, cx, p) {
                    changed = Some(m.clone());
                }
            }
            if let Some(next) = changed {
                state.connect.edit_models(view, &provider, |mm| *mm = next);
            }
        });
}

/// Label, description and "behaves as" of a picker row; true when changed.
fn picker_fields(ui: &mut Ui, cx: &Cx<'_>, p: &mut PickerRow) -> bool {
    let mut changed = false;
    let mut label = p.label.clone().unwrap_or_default();
    let mut desc = p.description.clone().unwrap_or_default();
    if ui
        .add(
            TextEdit::singleline(&mut label)
                .hint_text(cx.tr.t("claude.picker.label_hint"))
                .desired_width(f32::INFINITY),
        )
        .accessible_name(cx.tr.t("claude.picker.label_hint"))
        .changed()
    {
        p.label = (!label.trim().is_empty()).then_some(label);
        changed = true;
    }
    if ui
        .add(
            TextEdit::singleline(&mut desc)
                .hint_text(cx.tr.t("claude.picker.description_hint"))
                .desired_width(f32::INFINITY),
        )
        .accessible_name(cx.tr.t("claude.picker.description_hint"))
        .changed()
    {
        p.description = (!desc.trim().is_empty()).then_some(desc);
        changed = true;
    }
    ui.horizontal(|ui| {
        ui.label(Text::small(cx.tr.t("claude.picker.behaves_as"), cx.pal))
            .on_hover_text(cx.tr.t("claude.picker.behaves_as_tip"));
        let current = p.behaves_as.clone().unwrap_or_default();
        let mut pick: Option<Option<String>> = None;
        Dropdown::new(("claude-behaves", p.model.clone()))
            .label(cx.tr.t("claude.picker.behaves_as"))
            .selected_text(if current.is_empty() {
                cx.tr.t("claude.picker.behaves_none")
            } else {
                current.clone()
            })
            .width(180.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(current.is_empty(), cx.tr.t("claude.picker.behaves_none"))
                    .clicked()
                {
                    pick = Some(None);
                }
                for slot in ClaudeSlot::ALL {
                    if let Some(k) = slot.known_model()
                        && ui.selectable_label(current == k, k).clicked()
                    {
                        pick = Some(Some(k.to_owned()));
                    }
                }
            });
        if let Some(v) = pick {
            p.behaves_as = v;
            changed = true;
        }
    });
    changed
}

/// Without a selected model: the slots and the picker rows in order.
fn summary(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    provider: &ProfileId,
    models: &ClaudeModels,
) {
    ui.label(Text::strong(cx.tr.t("claude.summary.title"), cx.pal));
    ui.label(Text::caption(cx.tr.t("claude.summary.hint"), cx.pal));
    ui.add_space(6.0);
    let none = cx.tr.t("claude.summary.unset");
    egui::Grid::new("claude-summary")
        .num_columns(3)
        .spacing(vec2(10.0, 6.0))
        .show(ui, |ui| {
            for slot in ClaudeSlot::ALL {
                ui.label(Text::small(slot_label(cx, slot), cx.pal));
                match models.slots.get(&slot) {
                    Some(s) => {
                        if ui
                            .link(
                                RichText::new(&s.model)
                                    .font(mono_font(text::SMALL, Weight::Regular)),
                            )
                            .clicked()
                        {
                            state.connect.selected = Some(s.model.clone());
                        }
                        let caps = models.capabilities_of(&s.model, None);
                        ui.label(Text::caption(capabilities_text(&caps), cx.pal));
                    }
                    None => {
                        ui.label(Text::caption(&none, cx.pal));
                        ui.label("");
                    }
                }
                ui.end_row();
            }
            for (key, v) in [
                ("claude.role.main", &models.main),
                ("claude.role.subagent", &models.subagent),
                ("claude.role.background", &models.small_fast),
            ] {
                ui.label(Text::small(cx.tr.t(key), cx.pal));
                match v {
                    Some(m) => {
                        if ui
                            .link(RichText::new(m).font(mono_font(text::SMALL, Weight::Regular)))
                            .clicked()
                        {
                            state.connect.selected = Some(m.clone());
                        }
                    }
                    None => {
                        ui.label(Text::caption(&none, cx.pal));
                    }
                }
                ui.label("");
                ui.end_row();
            }
        });
    ui.add_space(10.0);
    ui.label(Text::strong(cx.tr.t("claude.picker.title"), cx.pal));
    if models.picker.is_empty() {
        ui.label(Text::caption(cx.tr.t("claude.picker.empty"), cx.pal));
    }
    let mut next = models.clone();
    let mut changed = false;
    let n = next.picker.len();
    let mut swap: Option<(usize, usize)> = None;
    let mut remove: Option<usize> = None;
    for (i, row) in next.picker.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(row.label.clone().unwrap_or_else(|| row.model.clone()))
                    .font(mono_font(text::SMALL, Weight::Regular)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(crate::design::widgets::IconButton::new(
                        Icon::Close,
                        cx.tr.t("claude.picker.remove"),
                    ))
                    .clicked()
                {
                    remove = Some(i);
                }
                if ui
                    .add(
                        crate::design::widgets::IconButton::new(
                            Icon::ArrowDown,
                            cx.tr.t("claude.picker.down"),
                        )
                        .enabled(i + 1 < n),
                    )
                    .clicked()
                {
                    swap = Some((i, i + 1));
                }
                if ui
                    .add(
                        crate::design::widgets::IconButton::new(
                            Icon::ArrowUp,
                            cx.tr.t("claude.picker.up"),
                        )
                        .enabled(i > 0),
                    )
                    .clicked()
                {
                    swap = Some((i, i - 1));
                }
            });
        });
    }
    if let Some((a, b)) = swap {
        next.picker.swap(a, b);
        changed = true;
    }
    if let Some(i) = remove {
        next.picker.remove(i);
        changed = true;
    }
    let mut replace = next.replace_builtin;
    ui.horizontal(|ui| {
        ui.add(Toggle::new(&mut replace, cx.tr.t("claude.picker.replace")));
        ui.label(Text::small(cx.tr.t("claude.picker.replace"), cx.pal))
            .on_hover_text(cx.tr.t("claude.picker.replace_tip"));
    });
    if replace != next.replace_builtin {
        next.replace_builtin = replace;
        changed = true;
    }
    // The vendor's recommended settings, as hints.
    if let Some(e) = endpoint_of(cx, provider)
        && let Some(p) = e.preset.as_deref().and_then(anthropic_preset)
        && !p.env_hints.is_empty()
    {
        ui.add_space(10.0);
        ui.label(Text::strong(cx.tr.t("claude.hints.title"), cx.pal));
        ui.label(Text::caption(
            cx.tr.f("claude.hints.body", &[("vendor", &p.vendor)]),
            cx.pal,
        ));
        for (k, v) in p.env_hints {
            ui.label(
                RichText::new(format!("{k}={v}")).font(mono_font(text::CAPTION, Weight::Regular)),
            );
        }
    }
    if changed {
        state.connect.edit_models(view, provider, |m| *m = next);
    }
}

fn footer(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &ClaudeView, config: &ClaudeConfig) {
    let dirty = state.connect.dirty(view);
    let pending = view.pending.len();
    let busy = state.connect.busy();
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let can_apply = current_provider(view, config).is_some() && view.file.writable();
        if ui
            .add(
                Button::primary(cx.tr.t("claude.apply.button"))
                    .small()
                    .loading(busy)
                    .enabled(can_apply && !busy && !cx.busy),
            )
            .on_hover_text(cx.tr.t("claude.apply.tip"))
            .clicked()
        {
            state.connect.save_then(cx, view, ClaudePlanKind::Apply);
        }
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.restore.button"))
                    .icon(Icon::Restore)
                    .small()
                    .enabled(view.applied.is_some() && !busy),
            )
            .on_hover_text(cx.tr.t("claude.restore.tip"))
            .clicked()
        {
            state.connect.start_plan(cx, ClaudePlanKind::Restore);
        }
        if dirty
            && ui
                .add(
                    Button::ghost(cx.tr.t("claude.discard"))
                        .small()
                        .enabled(!busy),
                )
                .clicked()
        {
            state.connect.draft = None;
        }
        let text = if dirty {
            cx.tr.t("claude.footer.unsaved")
        } else if pending > 0 {
            cx.tr.f("claude.footer.pending", &[("n", &pending)])
        } else if view.applied.is_some() {
            cx.tr.t("claude.footer.in_sync")
        } else {
            cx.tr.t("claude.footer.not_applied")
        };
        let status = if dirty || pending > 0 {
            Status::Warn
        } else {
            Status::Ok
        };
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(StatusDot::new(status));
            ui.add(egui::Label::new(Text::small(text, cx.pal)).truncate());
        });
    });
}

fn endpoint_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(edit) = state.connect.endpoint.as_mut() else {
        return;
    };
    let profile = cx.data.profile(&edit.provider).cloned();
    let site = profile.as_ref().and_then(|p| cx.data.site(&p.site_id));
    let origin = match (&site, &profile) {
        (Some(s), Some(p)) => superba_core::claude::profile_origin(s, p),
        _ => None,
    };
    let mut close = false;
    let mut save: Option<Option<AnthropicEndpoint>> = None;
    let presets = anthropic_presets();
    let r = dialog(ctx, egui::Id::unique("claude-endpoint"), 520.0, |ui| {
        ui.label(Text::title(cx.tr.t("claude.endpoint.dialog_title"), cx.pal));
        dialog_body(ui, |ui| {
            ui.label(Text::small(cx.tr.t("claude.endpoint.dialog_body"), cx.pal));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(Text::small(cx.tr.t("claude.endpoint.preset"), cx.pal));
                let current = edit
                    .preset
                    .as_deref()
                    .and_then(|id| presets.iter().find(|p| p.id == id))
                    .map(|p| cx.tr.text(&p.label()))
                    .unwrap_or_else(|| cx.tr.t("claude.endpoint.custom"));
                let mut pick: Option<Option<&AnthropicPreset>> = None;
                Dropdown::new("claude-endpoint-preset")
                    .label(cx.tr.t("claude.endpoint.preset"))
                    .selected_text(current)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                edit.preset.is_none(),
                                cx.tr.t("claude.endpoint.custom"),
                            )
                            .clicked()
                        {
                            pick = Some(None);
                        }
                        for p in &presets {
                            let usable = p.base_url.is_some() || origin.is_some();
                            let on = edit.preset.as_deref() == Some(p.id);
                            let r = ui.add_enabled(
                                usable,
                                egui::Button::selectable(on, cx.tr.text(&p.label())),
                            );
                            if r.clicked() {
                                pick = Some(Some(p));
                            }
                        }
                    });
                if let Some(p) = pick {
                    match p {
                        Some(p) => {
                            edit.preset = Some(p.id.to_owned());
                            if let Some(e) = p.endpoint(origin.as_ref()) {
                                edit.url = e.base_text();
                                edit.auth = e.auth;
                            }
                        }
                        None => edit.preset = None,
                    }
                    edit.error = None;
                }
            });
            let field = ui
                .add(
                    TextEdit::singleline(&mut edit.url)
                        .hint_text("https://api.example.com/anthropic")
                        .font(mono_font(text::SMALL, Weight::Regular))
                        .desired_width(f32::INFINITY)
                        .margin(vec2(10.0, 6.0)),
                )
                .accessible_name(cx.tr.t("claude.endpoint.title"));
            if field.changed() {
                edit.preset = None;
                edit.error = None;
            }
            ui.horizontal(|ui| {
                ui.label(Text::small(cx.tr.t("claude.endpoint.auth"), cx.pal));
                Segmented::new("claude-endpoint-auth", &mut edit.auth)
                    .label(cx.tr.t("claude.endpoint.auth"))
                    .small()
                    .option(
                        AnthropicAuth::Bearer,
                        cx.tr.t("claude.endpoint.auth_bearer"),
                    )
                    .option(AnthropicAuth::ApiKey, cx.tr.t("claude.endpoint.auth_key"))
                    .show(ui);
            });
            if let Some(p) = edit.preset.as_deref().and_then(anthropic_preset) {
                ui.horizontal(|ui| {
                    if ui.link(cx.tr.t("claude.endpoint.docs")).clicked() {
                        out.push(Action::OpenUrl(p.doc_url.to_owned()));
                    }
                });
                if p.models == superba_core::claude::ModelsListing::No {
                    ui.label(Text::caption(cx.tr.t("claude.endpoint.no_list"), cx.pal));
                }
            }
            if let Some(e) = edit.error {
                crate::pages::note(ui, cx, Status::Error, cx.tr.t(e));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t("claude.endpoint.save")))
                .clicked()
            {
                match AnthropicEndpoint::parse_base(&edit.url) {
                    Ok(url) => {
                        save = Some(Some(AnthropicEndpoint {
                            base_url: url,
                            auth: edit.auth,
                            preset: edit.preset.clone(),
                        }));
                    }
                    Err(key) => edit.error = Some(key),
                }
            }
            if ui
                .add(Button::ghost(cx.tr.t("claude.endpoint.reset")))
                .on_hover_text(cx.tr.t("claude.endpoint.reset_tip"))
                .clicked()
            {
                save = Some(None);
            }
            if ui.add(Button::ghost(cx.tr.t("claude.cancel"))).clicked() {
                close = true;
            }
        });
    });
    if r.should_close() {
        close = true;
    }
    if let (Some(e), Some(mut p)) = (save, profile) {
        p.anthropic = e;
        out.push(Action::SaveProfile {
            profile: Box::new(p),
            key: None,
        });
        close = true;
    }
    if close {
        state.connect.endpoint = None;
    }
}

fn plan_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State) {
    let Some(plan) = state.connect.plan.clone() else {
        return;
    };
    let mut close = false;
    let mut write = false;
    let r = dialog(ctx, egui::Id::unique("claude-plan"), 640.0, |ui| {
        ui.label(Text::title(
            cx.tr.t(if plan.kind == ClaudePlanKind::Restore {
                "claude.plan.title_restore"
            } else {
                "claude.plan.title"
            }),
            cx.pal,
        ));
        dialog_body(ui, |ui| {
            ui.label(Text::small(
                cx.tr.f(
                    "claude.plan.body",
                    &[(
                        "path",
                        &superba_core::paths::display_path(
                            &state.view.as_ref().expect("loaded view").settings_path,
                        ),
                    )],
                ),
                cx.pal,
            ));
            egui::Grid::new("claude-plan-rows")
                .num_columns(3)
                .striped(true)
                .spacing(vec2(10.0, 4.0))
                .show(ui, |ui| {
                    let none = cx.tr.t("claude.plan.absent");
                    for c in &plan.changes {
                        ui.label(
                            RichText::new(&c.slot).font(mono_font(text::CAPTION, Weight::Medium)),
                        );
                        let before = c.before.clone().unwrap_or_else(|| none.clone());
                        let after = c.after.clone().unwrap_or_else(|| none.clone());
                        ui.add(
                            egui::Label::new(Text::caption(
                                format!("{} → {}", elide(&before), elide(&after)),
                                cx.pal,
                            ))
                            .truncate(),
                        )
                        .on_hover_text(format!("{before}\n→ {after}"));
                        if c.drift {
                            ui.add(Badge::new(cx.tr.t("claude.plan.drift")).status(Status::Warn));
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            crate::pages::shared::notices::notice_list(
                ui,
                cx,
                egui::Id::unique("claude-plan-notices"),
                &plan.notes,
            );
            if plan.drift().count() > 0 {
                ui.horizontal(|ui| {
                    ui.add(Toggle::new(
                        &mut state.connect.take_over,
                        cx.tr.t("claude.plan.take_over"),
                    ));
                    ui.label(Text::small(cx.tr.t("claude.plan.take_over"), cx.pal));
                });
            }
            ui.label(Text::caption(cx.tr.t("claude.plan.backup"), cx.pal));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t(
                    if plan.kind == ClaudePlanKind::Restore {
                        "claude.plan.restore"
                    } else {
                        "claude.plan.write"
                    },
                )))
                .clicked()
            {
                write = true;
            }
            if ui.add(Button::ghost(cx.tr.t("claude.cancel"))).clicked() {
                close = true;
            }
        });
    });
    if r.should_close() {
        close = true;
    }
    if write {
        state.connect.apply(cx);
    } else if close {
        state.connect.plan = None;
    }
}

fn elide(s: &str) -> String {
    crate::design::elide::elide_cols(s, 36, crate::design::elide::Ellipsis::End)
}

/// Unsaved edits and dialogs.
pub fn status(s: &ConnectState, view: Option<&ClaudeView>) -> PageStatus {
    PageStatus {
        modal: s.plan.is_some() || s.endpoint.is_some(),
        pending: view.map_or(0, |v| usize::from(s.dirty(v))),
        ..PageStatus::default()
    }
}

/// Run a page command.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = out;
    let Some(view) = state.view.clone() else {
        state.reload(cx);
        return;
    };
    match command {
        "apply" | "save" => state.connect.save_then(cx, &view, ClaudePlanKind::Apply),
        "restore" => state.connect.start_plan(cx, ClaudePlanKind::Restore),
        "pull" => {
            let config = state.connect.config(&view);
            if let Some(p) = current_provider(&view, &config) {
                state.connect.load_models(cx, &p, true);
            }
        }
        _ => {}
    }
}
