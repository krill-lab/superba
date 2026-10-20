//! Sync settings dialog (UX §3.8): this computer's Codex settings of the active
//! provider, filtered by the host's Codex version. Rows are grouped as 将写入
//! (writes and conflicts), 跳过 (the host's Codex does not know the key) and
//! 将恢复主机原值 (keys Superba wrote there earlier that the local provider no
//! longer sets: the host gets its own earlier value back).
//! Rows can be unticked; conflicts (changed on the host since the last sync)
//! start unticked ("keep the host's value"). Confirming plans
//! `SwitchIntent::HostSync`, which writes settings only (never the connection or
//! model overrides) and hot-restarts the host's Codex core.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align, Layout, Ui};
use superba_core::{
    BlockReason, CodexVersion, HostSyncAction, HostSyncPlan, HostSyncRow, HostSyncSelection,
    ProfileId, SwitchIntent, SwitchRequest, TargetId,
};

use super::super::router::db::Loader;
use super::State;
use crate::design::widgets::value_chip::tooltip_text;
use crate::design::widgets::{Button, Status, Text, dialog, dialog_body};
use crate::pages::shared::values;
use crate::pages::{Action, Cx, divider, error_text, note};

/// Width of the dialog.
const WIDTH: f32 = 720.0;

/// Rows a sync applies when nothing is unticked: the rows selected by default
/// (writes and removals; never skips or conflicts).
pub fn default_ticked(plan: &HostSyncPlan) -> BTreeSet<String> {
    plan.rows
        .iter()
        .filter(|r| r.selected && tickable(r))
        .map(|r| r.path.clone())
        .collect()
}

/// Whether a row can be ticked (skipped rows cannot).
pub fn tickable(row: &HostSyncRow) -> bool {
    !matches!(row.action, HostSyncAction::Skip { .. })
}

/// Rows grouped as (将写入 incl. conflicts, 跳过, 将恢复主机原值).
pub fn groups(plan: &HostSyncPlan) -> (Vec<&HostSyncRow>, Vec<&HostSyncRow>, Vec<&HostSyncRow>) {
    let mut writes = Vec::new();
    let mut skips = Vec::new();
    let mut removes = Vec::new();
    for r in &plan.rows {
        match r.action {
            HostSyncAction::Skip { .. } => skips.push(r),
            HostSyncAction::Remove => removes.push(r),
            _ => writes.push(r),
        }
    }
    (writes, skips, removes)
}

/// The switch request that executes a sync of `paths` to `target`.
pub fn request(target: &TargetId, paths: &BTreeSet<String>) -> SwitchRequest {
    let mut r = SwitchRequest::profile(ProfileId::new(""));
    r.intent = SwitchIntent::HostSync {
        target_id: target.clone(),
        selection: HostSyncSelection {
            paths: Some(paths.iter().cloned().collect()),
        },
    };
    r
}

/// Chinese (or official English) names of settings, from the built-in catalog.
pub fn setting_names(paths: &[String], zh: bool) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(store) = superba_catalog::CatalogStore::builtin() else {
        return out;
    };
    let Ok(catalog) = store.latest() else {
        return out;
    };
    for p in paths {
        let ex = match p.strip_prefix("features.") {
            Some(f) if !f.contains('.') => store.explain_feature(&catalog, f),
            _ => store.explain_key(&catalog, p),
        };
        let name = if zh {
            ex.zh_name.or(ex.official_name)
        } else {
            ex.official_name
        };
        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
            out.insert(p.clone(), n);
        }
    }
    out
}

/// Why a preview cannot be used: the host could not be reached (the service
/// reports a failed probe as a blocked plan without rows), with the detail the
/// probe gave, or other blocking reasons as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    /// The probe failed: "cannot connect to {host}" and the ssh message.
    Unreachable {
        /// ssh's own message, without the "ssh error on host:" wrapper.
        detail: String,
    },
    /// Other reasons, localized texts.
    Blocked(Vec<superba_core::LocalizedText>),
}

/// Whether `plan` can be shown as rows: `None` when it can, else why not.
pub fn unusable(plan: &HostSyncPlan, label: &str) -> Option<Unusable> {
    if plan.blocked.is_empty() {
        return None;
    }
    // A failed probe: no profile, no version, no rows, one UnsupportedTarget.
    if plan.rows.is_empty()
        && plan.profile_id.is_none()
        && let [BlockReason::UnsupportedTarget { detail }] = plan.blocked.as_slice()
    {
        let prefix = format!("ssh error on {label}: ");
        let detail = detail.strip_prefix(&prefix).unwrap_or(detail).trim();
        return Some(Unusable::Unreachable {
            detail: detail.to_owned(),
        });
    }
    Some(Unusable::Blocked(
        plan.blocked.iter().map(BlockReason::text).collect(),
    ))
}

/// State of the dialog.
#[derive(Debug)]
pub struct SyncDialog {
    /// Host.
    pub target: TargetId,
    /// Its label.
    pub label: String,
    /// The host's Codex version from its last probe (the title's version while
    /// the preview is loading or failed).
    pub cached_version: Option<CodexVersion>,
    /// The preview.
    pub plan: Loader<HostSyncPlan>,
    /// Paths ticked.
    pub ticked: BTreeSet<String>,
    initialized: bool,
    /// Setting names.
    pub names: Loader<BTreeMap<String, String>>,
    zh: bool,
}

impl SyncDialog {
    /// Open for `target` and load the preview; `cached_version` is the host's
    /// Codex version from its last probe.
    pub fn open(
        cx: &Cx<'_>,
        target: TargetId,
        label: String,
        cached_version: Option<CodexVersion>,
    ) -> Self {
        let mut d = Self::empty(target, label, cx.tr.locale() == superba_core::Locale::ZhCn);
        d.cached_version = cached_version;
        d.load(cx);
        d
    }

    /// (Re)load the preview.
    pub fn load(&mut self, cx: &Cx<'_>) {
        self.plan = Loader::default();
        self.names = Loader::default();
        self.ticked.clear();
        self.initialized = false;
        let service = cx.service.clone();
        let target = self.target.clone();
        self.plan
            .start(async move { service.host_sync_plan(&target).await });
    }

    fn empty(target: TargetId, label: String, zh: bool) -> Self {
        Self {
            target,
            label,
            cached_version: None,
            plan: Loader::default(),
            ticked: BTreeSet::new(),
            initialized: false,
            names: Loader::default(),
            zh,
        }
    }

    /// A dialog over a computed preview (tests).
    pub fn with_plan(target: TargetId, label: String, plan: HostSyncPlan) -> Self {
        let mut d = Self::empty(target, label, false);
        d.ticked = default_ticked(&plan);
        d.initialized = true;
        d.plan.value = Some(plan);
        d.names.value = Some(BTreeMap::new());
        d
    }

    /// Something is loading.
    pub fn loading(&self) -> bool {
        self.plan.loading() || self.names.loading()
    }

    /// Take finished reads; tick the defaults once the preview arrives.
    pub fn poll(&mut self) {
        if self.plan.poll()
            && let Some(p) = &self.plan.value
            && !self.initialized
        {
            self.ticked = default_ticked(p);
            self.initialized = true;
            let paths: Vec<String> = p.rows.iter().map(|r| r.path.clone()).collect();
            let zh = self.zh;
            self.names
                .start(async move { Ok(setting_names(&paths, zh)) });
        }
        self.names.poll();
    }

    /// Tick or untick a path.
    pub fn set(&mut self, path: &str, on: bool) {
        if on {
            self.ticked.insert(path.to_owned());
        } else {
            self.ticked.remove(path);
        }
    }

    /// The provider whose settings are pushed: the preview's profile, else the
    /// applied one (also while the preview loads or failed).
    pub fn provider(&self, cx: &Cx<'_>) -> Option<String> {
        let data = cx.data;
        let id = self
            .plan
            .value
            .as_ref()
            .and_then(|p| p.profile_id.clone())
            .or_else(|| data.active_state().and_then(|a| a.profile_id.clone()));
        id.and_then(|id| data.profile(&id).map(|p| p.name.clone()))
            .or_else(|| data.active_profile().map(|p| p.name.clone()))
            .filter(|n| !n.trim().is_empty())
    }

    /// The host's Codex version: the preview's, else the last probe's.
    pub fn version(&self) -> Option<CodexVersion> {
        self.plan
            .value
            .as_ref()
            .and_then(|p| p.host_version.clone())
            .or_else(|| self.cached_version.clone())
    }

    /// The request for the ticked rows (`None` while nothing can be applied).
    pub fn confirm(&self) -> Option<SwitchRequest> {
        let plan = self.plan.value.as_ref()?;
        if !plan.blocked.is_empty() || self.ticked.is_empty() {
            return None;
        }
        Some(request(&self.target, &self.ticked))
    }
}

fn group_header(ui: &mut Ui, cx: &Cx<'_>, key: &str, n: usize) {
    ui.add_space(4.0);
    ui.label(Text::caption(cx.tr.f(key, &[("n", &n)]), cx.pal));
}

/// One row, laid out in fixed columns (checkbox · name and path · this computer
/// · host now, or a reason spanning both value columns).
fn row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &mut SyncDialog,
    r: &HostSyncRow,
    names: &BTreeMap<String, String>,
    widths: [f32; 3],
) {
    use crate::design::fonts::{Weight, font, mono_font};
    use crate::design::tokens::text;
    use crate::design::widgets::log_table::elided;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
    let cy = rect.center().y;
    let mut x = rect.min.x;
    let check = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(22.0, 22.0));
    if tickable(r) {
        let mut on = d.ticked.contains(&r.path);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(check));
        if child.checkbox(&mut on, "").changed() {
            d.set(&r.path, on);
        }
    } else {
        ui.painter().text(
            check.center(),
            egui::Align2::CENTER_CENTER,
            "–",
            font(text::CAPTION, Weight::Regular),
            cx.pal.text_subtle,
        );
    }
    x += 26.0;
    let mut cut = false;
    let paint = |ui: &Ui, x: f32, s: &str, f: egui::FontId, color, max: f32| -> f32 {
        let g = elided(ui, s, f, color, max);
        let w = g.size().x;
        ui.painter()
            .galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
        w
    };
    let mut used = 0.0;
    if let Some(n) = names.get(&r.path) {
        used = paint(
            ui,
            x,
            n,
            font(text::SMALL, Weight::Regular),
            cx.pal.text,
            widths[0] - 8.0,
        ) + 6.0;
    }
    paint(
        ui,
        x + used,
        &r.path,
        mono_font(text::CAPTION, Weight::Regular),
        cx.pal.text_muted,
        (widths[0] - used - 8.0).max(24.0),
    );
    x += widths[0];
    let rest = rect.max.x - x;
    let mut tip = r.path.clone();
    match &r.action {
        HostSyncAction::Skip { reason } => {
            let why = cx.localize(&reason.text());
            paint(
                ui,
                x,
                &why,
                font(text::SMALL, Weight::Regular),
                cx.pal.text_muted,
                rest,
            );
            tip = format!("{tip} · {why}");
        }
        HostSyncAction::Remove => {
            let why = cx.tr.t("hosts.syncdlg.remove_reason");
            paint(
                ui,
                x,
                &why,
                font(text::SMALL, Weight::Regular),
                cx.pal.text_muted,
                rest,
            );
        }
        _ => {
            // Values cut by shape (middle for paths and URLs, end for prose), the
            // first line of multi-line text with "+N lines"; the chevron at the
            // end of the row opens the full change below it (user bug 15).
            let unset = cx.tr.t("hosts.syncdlg.unset");
            let shown = |v: &Option<String>| {
                v.as_deref()
                    .map(values::Shown::repr)
                    .unwrap_or_else(|| values::Shown::label(unset.clone()))
            };
            let (local, remote) = (shown(&r.local), shown(&r.remote));
            let mut value =
                |ui: &mut Ui, x: f32, v: &values::Shown, color: egui::Color32, w: f32| {
                    cut |= values::cut(ui, v, w);
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(egui::Rect::from_min_size(
                                egui::pos2(x, rect.min.y),
                                egui::vec2(w, rect.height()),
                            ))
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    values::plain(&mut child, cx, v, w, color);
                };
            value(ui, x, &local, cx.pal.text, widths[1] - 8.0);
            x += widths[1];
            value(ui, x, &remote, cx.pal.text_muted, widths[2] - 8.0);
            x += widths[2];
            tip = format!(
                "{tip}\n{} → {}",
                tooltip_text(&local.head),
                tooltip_text(&remote.head)
            );
            if matches!(r.action, HostSyncAction::Conflict) {
                let label = cx.tr.t("hosts.syncdlg.conflict_badge");
                let g = ui.painter().layout_no_wrap(
                    label,
                    font(text::CAPTION, Weight::Medium),
                    cx.pal.text,
                );
                let pill = egui::Rect::from_min_size(
                    egui::pos2(x, cy - 10.0),
                    egui::vec2(g.size().x + 22.0, 20.0),
                );
                ui.painter()
                    .rect_filled(pill, crate::design::tokens::radius::PILL, cx.pal.hover);
                ui.painter().circle_filled(
                    egui::pos2(pill.min.x + 9.0, cy),
                    3.0,
                    cx.pal.status(Status::Warn),
                );
                ui.painter().galley(
                    egui::pos2(pill.min.x + 16.0, cy - g.size().y / 2.0),
                    g,
                    cx.pal.text,
                );
                tip = format!(
                    "{tip}\n{} · {}",
                    cx.tr.t("hosts.syncdlg.conflict"),
                    cx.tr.t("hosts.syncdlg.keep_host")
                );
            }
        }
    }
    let id = egui::Id::unique(("hosts-sync-open", d.target.as_str(), &r.path));
    if cut || values::is_open(ui, id) {
        let chevron = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - 22.0, rect.min.y),
            egui::vec2(22.0, 22.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(chevron));
        values::disclosure(&mut child, cx, id);
    }
    resp.on_hover_text(tip);
    if values::is_open(ui, id) {
        let unset = cx.tr.t("hosts.syncdlg.unset");
        let local = r.local.as_deref().map(values::Shown::repr);
        let remote = r.remote.as_deref().map(values::Shown::repr);
        // What the host has now (-) and what this computer writes (+).
        values::full_diff(
            ui,
            cx,
            id.with("full"),
            remote.as_ref(),
            local.as_ref(),
            &unset,
            160.0,
        );
    }
}

/// Draw the dialog.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(d) = state.sync.as_mut() else {
        return;
    };
    let mut close = false;
    let mut retry = false;
    let provider = d.provider(cx);
    let version = d.version();
    let resp = dialog(
        ui.ctx(),
        egui::Id::unique(("hosts-sync", d.target.as_str())),
        WIDTH,
        |ui| {
            ui.label(Text::title(
                match &version {
                    Some(v) => cx.tr.f(
                        "hosts.syncdlg.title",
                        &[("host", &d.label), ("version", &v.to_string())],
                    ),
                    None => cx.tr.f("hosts.syncdlg.title_plain", &[("host", &d.label)]),
                },
                cx.pal,
            ));
            ui.add(
                egui::Label::new(Text::small(
                    match &provider {
                        Some(p) => cx
                            .tr
                            .f("hosts.syncdlg.body", &[("provider", p), ("host", &d.label)]),
                        None => cx.tr.f("hosts.syncdlg.body_plain", &[("host", &d.label)]),
                    },
                    cx.pal,
                ))
                .wrap(),
            );
            divider(ui, cx);
            // A failed preview shows only why, with Retry (review ux-visual #5):
            // no column header, no "nothing to sync", no restart note.
            let failure = match (&d.plan.value, &d.plan.error) {
                (Some(plan), _) => unusable(plan, &d.label),
                (None, Some(_)) => Some(Unusable::Blocked(Vec::new())),
                (None, None) => None,
            };
            let Some(plan) = d.plan.value.clone().filter(|_| failure.is_none()) else {
                dialog_body(ui, |ui| match &failure {
                    Some(Unusable::Unreachable { detail }) => {
                        note(
                            ui,
                            cx,
                            Status::Error,
                            cx.tr.f("hosts.syncdlg.unreachable", &[("host", &d.label)]),
                        );
                        if !detail.is_empty() {
                            ui.add(
                                egui::Label::new(Text::mono_muted(detail.clone(), cx.pal)).wrap(),
                            );
                        }
                    }
                    Some(Unusable::Blocked(reasons)) => {
                        if let Some(e) = &d.plan.error {
                            note(ui, cx, Status::Error, error_text(cx.tr, e));
                        }
                        for r in reasons {
                            note(ui, cx, Status::Error, cx.localize(r));
                        }
                    }
                    None => {
                        ui.label(Text::small(cx.tr.t("hosts.syncdlg.loading"), cx.pal));
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if failure.is_some()
                            && ui
                                .add(Button::primary(cx.tr.t("action.retry")).small())
                                .clicked()
                        {
                            retry = true;
                        }
                        if ui
                            .add(Button::ghost(cx.tr.t("action.cancel")).small())
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                return;
            };
            if plan.rows.is_empty() {
                dialog_body(ui, |ui| {
                    crate::pages::shared::notices::notice_list(
                        ui,
                        cx,
                        egui::Id::unique(("hosts-sync-notices", d.target.as_str())),
                        &plan.notes,
                    );
                    // Nothing to compare: no column header and no restart note.
                    ui.label(Text::small(cx.tr.t("hosts.syncdlg.nothing"), cx.pal));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(Button::ghost(cx.tr.t("action.close")).small())
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                return;
            }
            dialog_body(ui, |ui| {
                crate::pages::shared::notices::notice_list(
                    ui,
                    cx,
                    egui::Id::unique(("hosts-sync-notices", d.target.as_str())),
                    &plan.notes,
                );
                let names = d.names.value.clone().unwrap_or_default();
                let (writes, skips, removes) = groups(&plan);
                let widths = [260.0, 150.0, 150.0];
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    ui.allocate_ui(egui::vec2(widths[0], 18.0), |ui| {
                        ui.label(Text::caption(cx.tr.t("hosts.syncdlg.col.setting"), cx.pal));
                    });
                    ui.allocate_ui(egui::vec2(widths[1], 18.0), |ui| {
                        ui.label(Text::caption(cx.tr.t("hosts.syncdlg.col.local"), cx.pal));
                    });
                    ui.label(Text::caption(
                        cx.tr.f("hosts.syncdlg.col.host", &[("host", &d.label)]),
                        cx.pal,
                    ));
                });
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if !writes.is_empty() {
                        group_header(ui, cx, "hosts.syncdlg.write", writes.len());
                        for r in &writes {
                            row(ui, cx, d, r, &names, widths);
                        }
                    }
                    if !skips.is_empty() {
                        group_header(ui, cx, "hosts.syncdlg.skip", skips.len());
                        for r in &skips {
                            row(ui, cx, d, r, &names, widths);
                        }
                    }
                    if !removes.is_empty() {
                        group_header(ui, cx, "hosts.syncdlg.remove", removes.len());
                        for r in &removes {
                            row(ui, cx, d, r, &names, widths);
                        }
                    }
                });
                divider(ui, cx);
                ui.add(
                    egui::Label::new(Text::small(
                        cx.tr.f("hosts.syncdlg.restart", &[("host", &d.label)]),
                        cx.pal,
                    ))
                    .wrap(),
                );
            });
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let n = d.ticked.len();
                    let request = d.confirm();
                    if ui
                        .add(
                            Button::primary(cx.tr.f("hosts.syncdlg.confirm", &[("n", &n)]))
                                .small()
                                .enabled(request.is_some() && !cx.busy),
                        )
                        .clicked()
                        && let Some(req) = request
                    {
                        out.push(Action::Plan(Box::new(req)));
                        close = true;
                    }
                    if ui
                        .add(Button::ghost(cx.tr.t("action.cancel")).small())
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        },
    );
    if retry && let Some(d) = state.sync.as_mut() {
        d.load(cx);
    }
    if resp.should_close() || close {
        state.sync = None;
        // The sync changes what is pending; check again when it is done.
        state.plans.clear();
    }
}
