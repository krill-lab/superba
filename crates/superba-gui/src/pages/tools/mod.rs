//! Tool configuration, with read-only Codex and Claude summaries. Edits remain
//! drafts until Apply; merely following Codex never writes a tool's files.
pub mod detail;
pub mod files;
pub mod list;

use super::{Action, Cx, Page, PageStatus, error_text, page_header};
use crate::design::widgets::{Button, PendingAction, PendingBar, PendingLabels, Text};
use crate::pages::shared::loading::Latest;
use crate::runtime::{ReadTask, task};
use std::collections::BTreeMap;
use std::time::Duration;
use superba_core::harness::*;
use superba_core::{ClaudeView, ModelList, ProfileId, ProfileSet};

/// Selected row; the first two lead to their dedicated pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Selection {
    /// Codex summary.
    Codex,
    /// Claude Code summary.
    Claude,
    /// An editable tool.
    Tool(HarnessId),
    /// OpenCode is the initial row.
    #[default]
    Initial,
}
impl Selection {
    /// The editable tool, if any.
    pub fn harness(self) -> Option<HarnessId> {
        match self {
            Self::Tool(h) => Some(h),
            Self::Initial => Some(HarnessId::OpenCode),
            _ => None,
        }
    }
    /// Stable display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            _ => self.harness().expect("tool").display_name(),
        }
    }
    /// Neutral identity.
    pub fn monogram(self) -> &'static str {
        match self {
            Self::Codex => "cx",
            Self::Claude => "cc",
            _ => match self.harness().expect("tool") {
                HarnessId::ClaudeCode => "cc",
                HarnessId::Gemini => "gm",
                HarnessId::Dsh => "ds",
                HarnessId::OpenCode => "oc",
                HarnessId::Pi => "pi",
                HarnessId::ZCode => "zc",
                HarnessId::WorkBuddy => "wb",
            },
        }
    }
}
/// Persistent state, including a separate draft for each tool.
#[derive(Debug, Default)]
pub struct State {
    /// Row shown.
    pub selected: Selection,
    /// Last saved views, by tool.
    pub views: BTreeMap<HarnessId, HarnessView>,
    /// Unsaved settings survive switching rows.
    pub drafts: BTreeMap<HarnessId, HarnessSettings>,
    /// List summaries.
    pub summaries: Vec<HarnessSummary>,
    /// Claude summary.
    pub claude: Option<ClaudeView>,
    /// Read-only preview of the currently edited settings.
    pub preview: Option<HarnessView>,
    preview_load: Latest<(HarnessId, HarnessSettings), HarnessView>,
    list_task: Option<ReadTask<Vec<HarnessSummary>>>,
    claude_task: Option<ReadTask<ClaudeView>>,
    view_load: Latest<HarnessId, HarnessView>,
    loaded_profiles: Option<ProfileSet>,
    loaded_active: Option<ProfileId>,
    initialized: bool,
    /// Model lists scoped to providers.
    pub models: BTreeMap<ProfileId, ModelList>,
    /// Model load (only the latest provider may answer).
    pub model_load: Latest<ProfileId, ModelList>,
    /// Model search.
    pub search: String,
    /// Model page.
    pub model_page: usize,
    /// Preview before a write, with secrets masked by the service.
    pub plan: Option<HarnessPlan>,
    plan_task: Option<ReadTask<HarnessPlan>>,
    apply_task: Option<ReadTask<HarnessApplyReport>>,
    /// Explicitly take over changed entries; false preserves them.
    pub take_over: bool,
    /// Last write report.
    pub report: Option<HarnessApplyReport>,
    /// Visible failure.
    pub error: Option<String>,
    /// Shared configuration-file editor, harness partition.
    pub editor: files::Editor,
}
impl State {
    /// Set a saved view, also used by deterministic GUI fixtures.
    pub fn set_view(&mut self, v: HarnessView) {
        if self.drafts.get(&v.harness) == Some(&v.settings) {
            self.drafts.remove(&v.harness);
        }
        self.views.insert(v.harness, v);
    }
    /// Settings as currently edited.
    pub fn settings(&self, h: HarnessId) -> HarnessSettings {
        self.drafts
            .get(&h)
            .cloned()
            .or_else(|| self.views.get(&h).map(|v| v.settings.clone()))
            .or_else(|| {
                self.summaries
                    .iter()
                    .find(|v| v.harness == h)
                    .map(|v| v.settings.clone())
            })
            .unwrap_or_default()
    }
    /// Change one tool's draft without saving it or touching tool files.
    pub fn edit(&mut self, h: HarnessId, settings: HarnessSettings) {
        if self.views.get(&h).is_some_and(|v| v.settings == settings) {
            self.drafts.remove(&h);
        } else {
            self.drafts.insert(h, settings);
        }
        self.preview = None;
        self.model_page = 0;
    }
    /// An apply or plan is running.
    pub fn busy(&self) -> bool {
        self.plan_task.is_some() || self.apply_task.is_some()
    }
    /// Refresh saved state; pending drafts are kept.
    pub fn reload(&mut self, cx: &Cx<'_>) {
        self.initialized = true;
        self.error = None;
        self.loaded_profiles = cx.data.profiles.value.clone();
        self.loaded_active = cx.data.active_profile().map(|p| p.id.clone());
        let svc = cx.service.clone();
        self.list_task = Some(task(async move { svc.harness_list().await }));
        let svc = cx.service.clone();
        self.claude_task = Some(task(async move { svc.claude_view().await }));
        if let Some(h) = self.selected.harness() {
            self.load_view(cx, h);
        }
        self.preview = None;
        self.preview_load = Default::default();
    }
    fn load_view(&mut self, cx: &Cx<'_>, h: HarnessId) {
        let svc = cx.service.clone();
        self.view_load.start(
            h,
            task(async move { svc.harness_view(h).await }),
            Duration::from_secs(60),
        );
    }
    /// Explicitly fetch the selected provider's models.
    pub fn pull_models(&mut self, cx: &Cx<'_>, id: ProfileId, refresh: bool) {
        let svc = cx.service.clone();
        let key = id.clone();
        self.model_load.start(
            key,
            task(async move { svc.harness_models(id, refresh).await }),
            Duration::from_secs(45),
        );
    }
    /// Prepare a review without saving the chosen draft or touching tool files.
    pub fn start_plan(&mut self, cx: &Cx<'_>, kind: HarnessPlanKind) {
        let Some(h) = self.selected.harness() else {
            return;
        };
        if self.busy() {
            return;
        }
        let svc = cx.service.clone();
        let settings = self.settings(h);
        self.error = None;
        self.take_over = false;
        self.plan_task = Some(task(async move {
            if kind == HarnessPlanKind::Apply {
                svc.harness_plan_preview(h, settings).await
            } else {
                svc.harness_plan(h, kind).await
            }
        }));
    }
    /// Execute the reviewed plan.
    pub fn apply(&mut self, cx: &Cx<'_>) {
        let Some(plan) = self.plan.take() else { return };
        let svc = cx.service.clone();
        let take = self.take_over;
        let settings = self.settings(plan.harness);
        self.apply_task = Some(task(async move {
            if plan.kind == HarnessPlanKind::Apply {
                svc.save_harness_settings(plan.harness, settings).await?;
            }
            svc.harness_apply(plan, take).await
        }));
    }
    fn poll(&mut self, cx: &Cx<'_>) {
        let active = cx.data.active_profile().map(|p| p.id.clone());
        if !self.initialized
            || self.loaded_profiles != cx.data.profiles.value
            || self.loaded_active != active
        {
            self.models.clear();
            self.model_load = Default::default();
            self.reload(cx);
        }
        if let Some(answer) = self.list_task.as_ref().and_then(ReadTask::poll) {
            self.list_task = None;
            match answer {
                Ok(v) => self.summaries = v,
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(answer) = self.claude_task.as_ref().and_then(ReadTask::poll) {
            self.claude_task = None;
            match answer {
                Ok(v) => self.claude = Some(v),
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some((_, v)) = self.view_load.poll() {
            self.set_view(v);
        }
        if let Some(((h, settings), v)) = self.preview_load.poll()
            && self.selected.harness() == Some(h)
            && self.settings(h) == settings
        {
            self.preview = Some(v);
        }
        if let Some((id, list)) = self.model_load.poll() {
            self.models.insert(id, list);
        }
        if let Some(result) = self.plan_task.as_ref().and_then(ReadTask::poll) {
            self.plan_task = None;
            match result {
                Ok(p) => {
                    self.plan = Some(p);
                }
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(result) = self.apply_task.as_ref().and_then(ReadTask::poll) {
            self.apply_task = None;
            match result {
                Ok(r) => {
                    self.report = Some(r);
                    self.reload(cx);
                }
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(h) = self.selected.harness() {
            if !self.view_load.is_chosen(&h) {
                self.load_view(cx, h);
            }
            let settings = self.settings(h);
            if self.drafts.contains_key(&h) && !self.preview_load.is_chosen(&(h, settings.clone()))
            {
                let svc = cx.service.clone();
                let draft = settings.clone();
                self.preview_load.start(
                    (h, settings),
                    task(async move { svc.harness_preview(h, draft).await }),
                    Duration::from_secs(45),
                );
            }
            if let Some(id) = provider(cx, &self.settings(h))
                && !self.model_load.is_chosen(&id)
            {
                if self.models.contains_key(&id) {
                    self.model_load.choose(id);
                } else {
                    self.pull_models(cx, id, false);
                }
            }
        }
    }
}
/// Provider of a draft. An explicitly removed provider is never silently replaced.
pub fn provider(cx: &Cx<'_>, settings: &HarnessSettings) -> Option<ProfileId> {
    settings
        .provider
        .clone()
        .or_else(|| cx.data.active_profile().map(|p| p.id.clone()))
}
/// Draw the tools page and its sticky pending bar.
pub fn show(ui: &mut egui::Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.poll(cx);
    if state.busy()
        || state.list_task.is_some()
        || state.claude_task.is_some()
        || state.view_load.loading()
        || state.preview_load.loading()
        || state.model_load.loading()
    {
        ui.ctx().request_repaint_after(Duration::from_millis(60));
    }
    page_header(ui, cx, Page::Tools, |ui| {
        if ui.add(Button::ghost(cx.tr.t("tools.reload"))).clicked() {
            state.reload(cx);
        }
    });
    if state.editor.open {
        files::show(ui, cx, &mut state.editor);
        if !state.editor.open {
            state.reload(cx);
        }
        return;
    }
    let rect = ui.available_rect_before_wrap();
    let body = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 56.0));
    let rail = if rect.width() >= 930.0 { 252.0 } else { 206.0 };
    let rows = list::rows(cx, state);
    let selected = state.selected;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("tools-body"));
    let mut detail_out = Vec::new();
    let (pick, _) = crate::design::layout::rail_split_with_id(
        &mut child,
        rail,
        ("tool-detail", selected),
        |ui| list::show(ui, cx, &rows, selected, out),
        |ui| detail::show(ui, cx, state, &mut detail_out),
    );
    out.extend(detail_out);
    if let Some(pick) = pick {
        state.selected = pick;
        state.preview = None;
        state.search.clear();
        state.model_page = 0;
    }
    ui.allocate_rect(body, egui::Sense::hover());
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        if let Some(h) = state.selected.harness() {
            let pending = state
                .preview
                .as_ref()
                .filter(|v| v.harness == h)
                .or_else(|| state.views.get(&h))
                .map_or(0, |v| v.pending.len())
                .max(usize::from(state.drafts.contains_key(&h)));
            if ui
                .add(
                    Button::ghost(cx.tr.t("tools.restore"))
                        .small()
                        .enabled(!state.busy()),
                )
                .clicked()
            {
                state.start_plan(cx, HarnessPlanKind::Restore);
            }
            if pending > 0 {
                match PendingBar::new(
                    pending,
                    PendingLabels {
                        count: cx.tr.f("tools.pending", &[("n", &pending)]),
                        diff: cx.tr.t("tools.diff"),
                        discard: cx.tr.t("tools.discard"),
                        save: cx.tr.t("tools.apply"),
                    },
                )
                .saving(state.busy())
                .show(ui)
                {
                    PendingAction::Save | PendingAction::Diff => {
                        state.start_plan(cx, HarnessPlanKind::Apply)
                    }
                    PendingAction::Discard => {
                        state.drafts.remove(&h);
                        state.preview = None;
                        state.preview_load = Default::default();
                    }
                    _ => {}
                }
            } else {
                ui.label(Text::small(cx.tr.t("tools.in_sync"), cx.pal));
                if ui
                    .add(
                        Button::primary(cx.tr.t("tools.apply"))
                            .small()
                            .enabled(!state.busy()),
                    )
                    .clicked()
                {
                    state.start_plan(cx, HarnessPlanKind::Apply);
                }
            }
        }
    });
    detail::review(ui, cx, state);
}
/// Shell status includes every tool's draft, including rows not shown.
pub fn status(state: &State) -> PageStatus {
    PageStatus {
        pending: state.drafts.len() + usize::from(state.editor.dirty()),
        modal: state.plan.is_some() || state.editor.modal(),
        ..Default::default()
    }
}
/// Keyboard and deep-link commands.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str) {
    if state.editor.open {
        if command == "save" && state.editor.dirty() {
            state.editor.review = true;
        }
        return;
    }
    match command {
        "save" | "apply" => state.start_plan(cx, HarnessPlanKind::Apply),
        "restore" => state.start_plan(cx, HarnessPlanKind::Restore),
        "reload" => state.reload(cx),
        "codex" => state.selected = Selection::Codex,
        _ => {
            if let Some(h) = HarnessId::parse(command) {
                state.selected = if h == HarnessId::ClaudeCode {
                    Selection::Claude
                } else {
                    Selection::Tool(h)
                };
            }
        }
    }
}
