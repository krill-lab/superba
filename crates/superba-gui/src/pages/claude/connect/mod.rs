//! Claude Code · connection and models (user bug 79), one flow of sections
//! in a scrolling column above the apply bar:
//!
//! 1. **连接** ([`connection`]): form rows for the provider (default: the
//!    one Codex uses), the route (direct, or the local router once it serves
//!    Claude Code: [`superba_core::ClaudeView::router_base_url`]), the
//!    provider's Anthropic endpoint (copy, check, change: a verified preset
//!    or a typed URL, saved with the provider) and Claude Code itself with
//!    its `settings.json`.
//! 2. **模型槽位** ([`slots`]): Opus / Sonnet / Haiku / Fable / custom and
//!    the default, subagent and background models, each with a dropdown of
//!    the fetched models (the provider's Codex list and its endpoint's
//!    standard list, merged; ids the endpoint cannot serve are "Codex only"
//!    and not offered) and, for the `/model` slots, the capabilities written
//!    for the model (suggested from its Codex metadata or family).
//! 3. **/model 列表** ([`picker`]): which models Claude Code's `/model`
//!    lists (`modelPicker`), ids added by hand, every row in one table.
//! 4. **高级** ([`picker::advanced`]): hiding Claude Code's own rows and the
//!    vendor's recommended variables, closed by default.
//!
//! **Apply** (the bar at the bottom) saves the configuration, shows what
//! changes in `settings.json` and writes only the keys Superba owns (backed
//! up first); **Restore** puts back what they held before Superba. New
//! Claude Code sessions use it.

mod connection;
mod dialogs;
pub mod picker;
pub mod slots;

use std::collections::HashMap;

use egui::{Align, Layout, Ui};
use superba_core::claude::{effective_endpoint, suggested_capabilities};
use superba_core::{
    AnthropicAuth, AnthropicEndpoint, ClaudeApplyReport, ClaudeConfig, ClaudeModelRow,
    ClaudeModels, ClaudeModelsRequest, ClaudeModelsView, ClaudePlan, ClaudePlanKind, ClaudeProbe,
    ClaudeView, Profile, ProfileId,
};

use super::State;
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, space};
use crate::design::widgets::{Button, FormRow, Status, StatusDot, Text};
use crate::pages::shared::loading::Latest;
use crate::pages::{Action, Cx, Page, PageStatus, error_text, page_header};
use crate::runtime::ReadTask;

pub use slots::SlotKey;

/// Time limit of a model list load.
const MODELS_LIMIT: std::time::Duration = std::time::Duration::from_secs(45);
/// Height of the banner line above the sections (the last apply or error).
const BANNER_H: f32 = size::CONTROL;

/// Below this width a setting row puts its controls under its text.
pub const STACK_BELOW: f32 = 560.0;

/// Narrowest text column of a setting row.
pub const TEXT_MIN: f32 = 200.0;

/// Width of the text column of a setting row `width` points wide whose
/// controls need `controls` points: the rest, so its help wraps as late as
/// possible, but never below [`TEXT_MIN`] (the controls shrink instead: an
/// address or a path is shortened in the middle).
pub fn text_width(width: f32, controls: f32) -> f32 {
    (width - controls - crate::design::widgets::form_row::LABEL_GAP).max(TEXT_MIN)
}

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
    /// Search text of the `/model` table.
    pub search: String,
    /// "Add model id" field.
    pub manual: String,
    /// The endpoint dialog.
    pub endpoint: Option<EndpointEdit>,
    /// The slot whose name, description and capabilities are edited.
    pub slot_edit: Option<SlotKey>,
    /// The `/model` row (model id) whose text is edited.
    pub picker_edit: Option<String>,
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
            .field("slot_edit", &self.slot_edit)
            .field("picker_edit", &self.picker_edit)
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

/// The endpoint of a provider (its own, else the suggested preset's).
fn endpoint_of(cx: &Cx<'_>, provider: &ProfileId) -> Option<AnthropicEndpoint> {
    let p = cx.data.profile(provider)?;
    let site = cx.data.site(&p.site_id)?;
    effective_endpoint(&site, p)
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

/// What the model sections draw from: the provider, its models as edited and
/// the rows of its list.
struct Models<'a> {
    view: &'a ClaudeView,
    provider: ProfileId,
    models: ClaudeModels,
    rows: Vec<ClaudeModelRow>,
    /// The list is loading for this provider.
    loading: bool,
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

    let models = provider.as_ref().map(|p| {
        let models = config.models_of(p);
        Models {
            view: &view,
            provider: p.clone(),
            rows: rows(state.connect.models.get(p), &models),
            models,
            loading: state.connect.load.is_chosen(p) && state.connect.load.loading(),
        }
    });

    let body = ui.available_rect_before_wrap();
    let banner = state.connect.report.is_some() || state.connect.error.is_some();
    let mut tracks = Vec::new();
    if banner {
        tracks.push(Track::Fixed(BANNER_H));
    }
    tracks.push(Track::Fill);
    tracks.push(Track::Fixed(size::FOOTER));
    let rects = layout::row_rects(body, &tracks, space::S);
    let mut i = 0;
    if banner {
        layout::fixed(ui, rects[0], |ui| banner_line(ui, cx, &mut state.connect));
        i = 1;
    }
    layout::fixed(ui, rects[i], |ui| {
        sections(ui, cx, state, &view, &config, models.as_ref(), out);
    });
    layout::fixed(ui, rects[i + 1], |ui| footer(ui, cx, state, &view, &config));
    if state.connect.endpoint.is_some() {
        dialogs::endpoint(ui.ctx(), cx, state, out);
    }
    if state.connect.plan.is_some() {
        dialogs::plan(ui.ctx(), cx, state);
    }
    if let Some(m) = &models {
        if state.connect.slot_edit.is_some() {
            slots::dialog(ui.ctx(), cx, state, m);
        }
        if state.connect.picker_edit.is_some() {
            picker::dialog(ui.ctx(), cx, state, m);
        }
    }
}

/// The sections in one scrolling column.
fn sections(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
    models: Option<&Models<'_>>,
    out: &mut Vec<Action>,
) {
    // The scroll bar gets a gutter of its own (over the content it would sit
    // on the controls at the right edge) and stays visible at rest.
    let scroll = &mut ui.spacing_mut().scroll;
    scroll.floating_allocated_width = scroll.bar_width + space::S;
    scroll.dormant_handle_opacity = 0.45;
    egui::ScrollArea::vertical()
        .id_salt("claude-connect-body")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::S;
            connection::section(ui, cx, state, view, config, out);
            ui.add_space(space::XL);
            let Some(m) = models else {
                crate::pages::empty_state(
                    ui,
                    cx,
                    Icon::Model,
                    &cx.tr.t("claude.models.no_provider_title"),
                    &cx.tr.t("claude.models.no_provider_body"),
                );
                return;
            };
            slots::section(ui, cx, state, m);
            ui.add_space(space::XL);
            picker::section(ui, cx, state, m);
            ui.add_space(space::XL);
            picker::advanced(ui, cx, state, m);
            ui.add_space(space::L);
        });
}

/// A section's title with actions on the right, then its muted caption.
fn section_head(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    caption: Option<&str>,
    actions: impl FnOnce(&mut Ui),
) {
    FormRow::new(Text::section(title, cx.pal))
        .label_width(0.0)
        .right()
        .id_salt(("claude-section", title))
        .show(ui, actions);
    if let Some(c) = caption {
        ui.add(egui::Label::new(Text::small(c, cx.pal)).wrap());
    }
    ui.add_space(space::XS);
}

/// A setting row: its title and help on the left, the controls right-aligned
/// (added right to left: the first one is the rightmost) in about
/// `controls_w` points; a hairline under it unless it is the `last`.
/// Narrower than [`STACK_BELOW`], the controls go under the text.
fn setting_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    help: impl FnOnce(&mut Ui),
    last: bool,
    controls_w: f32,
    controls: impl FnOnce(&mut Ui),
) {
    let width = ui.available_width();
    if width < STACK_BELOW {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::XS;
            ui.add_space(space::S);
            let label = ui.add(egui::Label::new(Text::strong(title, cx.pal)).wrap());
            help(ui);
            crate::design::a11y::label_unnamed_by(ui, label.id, |ui| {
                FormRow::unlabeled()
                    .id_salt(("claude-row-stacked", title))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = space::S;
                        ui.with_layout(Layout::right_to_left(Align::Center), controls);
                    });
            });
            ui.add_space(space::S);
        });
    } else {
        let text_w = text_width(width, controls_w);
        FormRow::default()
            .label_width(text_w)
            .height(size::ROW_LIST2)
            .right()
            .id_salt(("claude-row", title))
            .show_with(
                ui,
                |ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width(text_w);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(egui::Label::new(Text::strong(title, cx.pal)).wrap());
                        help(ui);
                    });
                },
                |ui| {
                    ui.spacing_mut().item_spacing.x = space::S;
                    controls(ui);
                },
            );
    }
    if !last {
        crate::pages::divider(ui, cx);
    }
}

/// One help line of a setting row: wrapped muted text, with a status dot in
/// front when `status` is given.
fn help_line(ui: &mut Ui, cx: &Cx<'_>, status: Option<Status>, text: &str) {
    match status {
        Some(s) => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                ui.add(StatusDot::new(s));
                ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
            });
        }
        None => {
            ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
        }
    }
}

fn banner_line(ui: &mut Ui, cx: &Cx<'_>, s: &mut ConnectState) {
    let (status, msg) = match (&s.error, &s.report) {
        (Some(e), _) => (Status::Error, e.clone()),
        (None, Some(r)) if r.kind == ClaudePlanKind::Restore => {
            (Status::Ok, cx.tr.t("claude.banner.restored"))
        }
        _ => (Status::Ok, cx.tr.t("claude.banner.new_sessions")),
    };
    FormRow::unlabeled()
        .id_salt("claude-banner")
        .show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(Button::ghost(cx.tr.t("claude.banner.dismiss")).small())
                    .clicked()
                {
                    s.report = None;
                    s.error = None;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(StatusDot::new(status));
                    ui.add(egui::Label::new(Text::small(msg.clone(), cx.pal)).truncate())
                        .on_hover_text(msg);
                });
            });
        });
}

/// The apply bar: what is pending, then Discard, Restore and Apply.
fn footer(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &ClaudeView, config: &ClaudeConfig) {
    let dirty = state.connect.dirty(view);
    let pending = view.pending.len();
    let busy = state.connect.busy();
    let rect = ui.max_rect();
    ui.painter().hline(
        rect.x_range(),
        rect.min.y,
        egui::Stroke::new(1.0, cx.pal.border),
    );
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = space::S;
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
                Button::new(cx.tr.t("claude.restore.button"))
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
            ui.add(egui::Label::new(Text::small(text.clone(), cx.pal)).truncate())
                .on_hover_text(text);
        });
    });
}

/// Unsaved edits and dialogs.
pub fn status(s: &ConnectState, view: Option<&ClaudeView>) -> PageStatus {
    PageStatus {
        modal: s.plan.is_some()
            || s.endpoint.is_some()
            || s.slot_edit.is_some()
            || s.picker_edit.is_some(),
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
