//! Claude Code · settings (user bug 79): the same three panes as the Codex
//! settings page — a rail (Common, Changed, the groups Connection / Models /
//! Capabilities / Output and context / Traffic and privacy / Other), a paged
//! list of Chinese-first rows and a detail pane with the official English
//! text, the Chinese explanation and an editor — for every key of Claude
//! Code's `settings.json` and every documented environment variable (saved
//! into `env`), plus keys of the file that no catalog knows. Edits are
//! pending until saved; a save writes the file through the service (backup
//! first, compare-and-swap) and keeps every key it did not change. Keys
//! Superba manages are read-only here (the connection page owns them). The
//! file editor of the Codex settings page opens `settings.json` itself.

use egui::{Align, Layout, RichText, Sense, TextEdit, Ui, vec2};
use superba_catalog::claude::{ClaudeGroup, ClaudeSource};
use superba_core::{
    CodexFileDoc, CodexFileKind, CodexFileSave, CodexFileSaved, Locale, SaveReason, SecretBytes,
    TargetId,
};

use super::State;
use super::editors::{self, Buffers};
use super::model::{Filter, Pending, Rail, Row, SettingsModel};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, text};
use crate::design::widgets::{
    Badge, Button, PagedState, PagedTable, Pager, PendingAction, PendingBar, PendingLabels, Status,
    StatusDot, Table, Text, Toggle, dialog,
};
use crate::pages::settings::files::Editor;
use crate::pages::shared::loading::{self, Latest};
use crate::pages::{
    Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem, error_text, page_header,
};
use crate::runtime::ReadTask;

/// Time limit of reading settings.json.
const LOAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);
/// Rail width.
const RAIL_W: f32 = 176.0;
/// Detail width.
const DETAIL_W: f32 = 400.0;
/// Gap between panes.
const GAP: f32 = 12.0;
/// Widest value chip of a row.
const CHIP_W: f32 = 140.0;

/// Page state.
pub struct SettingsState {
    /// The model (`None` before the first read).
    pub model: Option<SettingsModel>,
    /// The file as read.
    doc: Option<CodexFileDoc>,
    /// The read of the file (user bug 83: a loading state or the failure
    /// with Retry in place of the panes, never stale data).
    load: Latest<(), CodexFileDoc>,
    /// List filter.
    pub filter: Filter,
    shown_filter: Option<Filter>,
    /// Paging and selection.
    pub table: PagedState,
    /// Pending edits.
    pub pending: Pending,
    /// Text buffers of the editors.
    pub buffers: Buffers,
    saving: Option<ReadTask<CodexFileSaved>>,
    /// The last save failed.
    pub save_error: Option<String>,
    /// The diff dialog is open.
    pub diff_open: bool,
    /// The file editor (it replaces the panes while open).
    pub files: Editor,
    focus: Option<String>,
    focus_search: bool,
    /// The file changed elsewhere (an apply): read it again.
    stale: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            model: None,
            doc: None,
            load: Latest::default(),
            filter: Filter::default(),
            shown_filter: None,
            table: PagedState::default(),
            pending: Pending::default(),
            buffers: Buffers::default(),
            saving: None,
            save_error: None,
            diff_open: false,
            files: Editor::claude(),
            focus: None,
            focus_search: false,
            stale: false,
        }
    }
}

impl std::fmt::Debug for SettingsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsState")
            .field("rows", &self.model.as_ref().map(|m| m.rows.len()))
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl SettingsState {
    /// Read the file again (after an apply wrote it).
    pub fn file_changed(&mut self) {
        self.stale = true;
    }

    /// Replace the model (tests and the loader); pending edits the file now
    /// holds are dropped.
    pub fn set_doc(&mut self, doc: CodexFileDoc) {
        let model = SettingsModel::build(&doc);
        self.pending
            .0
            .retain(|p, v| model.row(p).is_none_or(|r| r.value.as_ref() != v.as_ref()));
        if self.filter.rail == Rail::Common
            && self.model.is_none()
            && model.rows.iter().all(|r| r.common.is_none())
        {
            self.filter.rail = Rail::Group(ClaudeGroup::Connection);
        }
        self.model = Some(model);
        self.doc = Some(doc);
    }

    fn load(&mut self, cx: &Cx<'_>) {
        let service = cx.service.clone();
        let task = crate::runtime::task(async move {
            service
                .codex_file(TargetId::local(), CodexFileKind::ClaudeSettings)
                .await
        });
        self.load.start((), task, LOAD_LIMIT);
    }

    /// The file is being read.
    pub fn loading(&self) -> bool {
        self.load.loading()
    }

    /// Rows listed now.
    pub fn rows(&self) -> Vec<Row> {
        match &self.model {
            Some(m) => m
                .listed(&self.filter, &self.pending)
                .into_iter()
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Select `path` (widening the filter when needed).
    pub fn focus(&mut self, path: &str) {
        self.focus = Some(path.to_owned());
        if let Some(m) = &self.model
            && let Some(r) = m.row(path)
        {
            self.filter.query.clear();
            if !m
                .listed(&self.filter, &self.pending)
                .iter()
                .any(|x| x.path() == path)
            {
                self.filter.rail = Rail::Group(r.entry.group);
            }
        }
    }

    fn apply_focus(&mut self) {
        let Some(path) = self.focus.clone() else {
            return;
        };
        let rows = self.rows();
        if let Some(i) = rows.iter().position(|r| r.path() == path) {
            if self.table.per_page == 0 {
                self.table.per_page = 1;
            }
            self.table.select(i, rows.len());
            self.shown_filter = Some(self.filter.clone());
            self.focus = None;
        } else if self.model.is_some() {
            self.focus = None;
        }
    }

    /// Record an edit (dropped when it equals the file).
    pub fn edit(&mut self, path: &str, value: Option<serde_json::Value>) {
        let Some(m) = &self.model else {
            return;
        };
        let now = m.row(path).and_then(|r| r.value.clone());
        if now == value {
            self.pending.0.remove(path);
        } else {
            self.pending.0.insert(path.to_owned(), value);
        }
    }

    fn save(&mut self, cx: &Cx<'_>) {
        let (Some(model), Some(doc)) = (&self.model, &self.doc) else {
            return;
        };
        if self.saving.is_some() || self.pending.is_empty() {
            return;
        }
        let text = match model.render(&self.pending) {
            Ok(t) => t,
            Err(e) => {
                self.save_error = Some(e);
                return;
            }
        };
        let save = CodexFileSave {
            target: TargetId::local(),
            kind: CodexFileKind::ClaudeSettings,
            contents: SecretBytes::new(text.into_bytes()),
            base_sha256: doc.sha256.clone(),
            reason: SaveReason::Edit,
            force: false,
        };
        let service = cx.service.clone();
        self.save_error = None;
        self.saving = Some(crate::runtime::task(async move {
            service.save_codex_file(save).await
        }));
    }

    fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        let first = self.model.is_none() && !self.load.loading() && self.load.failure().is_none();
        if first || self.stale {
            self.stale = false;
            self.load(cx);
        }
        if let Some(t) = &self.saving
            && let Some(r) = t.poll()
        {
            self.saving = None;
            match r {
                Ok(saved) => {
                    self.pending.0.clear();
                    self.buffers.clear();
                    self.diff_open = false;
                    out.push(Action::Toast {
                        status: Status::Ok,
                        text: cx.tr.t("claude.settings.saved"),
                    });
                    let _ = saved;
                    self.load(cx);
                }
                Err(e) => self.save_error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(((), doc)) = self.load.poll() {
            self.set_doc(doc);
            self.apply_focus();
        }
        self.load.loading() || self.saving.is_some()
    }
}

/// Draw the page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let view_path = state_view_path(state);
    let s = &mut state.settings;
    if s.files.open {
        crate::pages::settings::files_view::show(ui, cx, &mut s.files, out);
        if !s.files.open {
            s.stale = true;
        }
        return;
    }
    let loading = s.poll(cx, out);
    if loading {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    page_header(ui, cx, Page::ClaudeSettings, |ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.settings.edit_file"))
                    .icon(Icon::Terminal)
                    .small(),
            )
            .on_hover_text(cx.tr.t("claude.settings.edit_file_tip"))
            .clicked()
        {
            s.files
                .open_file(cx, TargetId::local(), CodexFileKind::ClaudeSettings);
        }
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
            s.load(cx);
        }
    });
    let Some(model) = s.model.clone() else {
        match s.load.failure() {
            Some(f) => {
                let reason = f.text(cx);
                if loading::failed_card(ui, cx, &cx.tr.t("claude.settings.load_failed"), &reason) {
                    s.load(cx);
                }
            }
            None => {
                loading::spinner_row(ui, cx, &cx.tr.t("claude.settings.loading"));
            }
        }
        return;
    };
    if let Some(f) = s.load.failure() {
        let reason = f.text(cx);
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Warn));
            ui.label(Text::small(
                cx.tr
                    .f("claude.settings.reload_failed", &[("error", &reason)]),
                cx.pal,
            ));
        });
    }
    let body = ui.available_rect_before_wrap();
    let notice = model.error.is_some();
    let mut tracks = vec![Track::Fixed(size::CONTROL)];
    if notice {
        tracks.push(Track::Fixed(20.0));
    }
    tracks.push(Track::Fill);
    tracks.push(Track::Fixed(size::FOOTER));
    let rects = layout::row_rects(body, &tracks, 6.0);
    layout::fixed(ui, rects[0], |ui| strip(ui, cx, s, view_path));
    let s = &mut state.settings;
    let mut i = 1;
    if notice {
        layout::fixed(ui, rects[1], |ui| {
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Error));
                ui.label(Text::small(
                    cx.tr.f(
                        "claude.settings.invalid_file",
                        &[("error", &model.error.as_deref().unwrap_or(""))],
                    ),
                    cx.pal,
                ));
            });
        });
        i = 2;
    }
    if s.shown_filter.as_ref() != Some(&s.filter) {
        s.shown_filter = Some(s.filter.clone());
        s.table.page = 0;
        s.table.selected = (!s.rows().is_empty()).then_some(0);
    }
    let cols = layout::column_rects(
        rects[i],
        &[Track::Fixed(RAIL_W), Track::Fill, Track::Fixed(DETAIL_W)],
        GAP,
    );
    layout::fixed(ui, cols[0], |ui| rail(ui, cx, s, &model));
    layout::fixed(ui, cols[1], |ui| list(ui, cx, s, &model));
    ui.painter().vline(
        cols[2].min.x - GAP / 2.0,
        cols[2].y_range(),
        egui::Stroke::new(1.0, cx.pal.border),
    );
    layout::fixed(ui, cols[2], |ui| detail(ui, cx, s, &model, out));
    layout::fixed(ui, rects[i + 1], |ui| footer(ui, cx, s, cols[1].x_range()));
    if s.diff_open {
        diff_dialog(ui.ctx(), cx, s, &model);
    }
}

fn state_view_path(state: &State) -> Option<String> {
    state
        .view
        .as_ref()
        .map(|v| superba_core::paths::display_path(&v.settings_path))
}

fn strip(ui: &mut Ui, cx: &Cx<'_>, s: &mut SettingsState, path: Option<String>) {
    ui.horizontal(|ui| {
        ui.set_min_height(size::CONTROL);
        ui.spacing_mut().item_spacing.x = 8.0;
        let search = ui
            .add(
                TextEdit::singleline(&mut s.filter.query)
                    .id(egui::Id::unique("claude-settings-search"))
                    .hint_text(cx.tr.t("claude.settings.search"))
                    .desired_width(240.0)
                    .margin(vec2(10.0, 6.0)),
            )
            .accessible_name(cx.tr.t("claude.settings.search"));
        if s.focus_search {
            search.request_focus();
            s.focus_search = false;
        }
        if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            s.filter.query.clear();
        }
        ui.add(Toggle::new(
            &mut s.filter.deprecated,
            cx.tr.t("claude.settings.show_deprecated"),
        ));
        ui.label(Text::small(
            cx.tr.t("claude.settings.show_deprecated"),
            cx.pal,
        ));
        if let Some(p) = path {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::Label::new(Text::mono_muted(p.clone(), cx.pal)).truncate())
                    .on_hover_text(p);
            });
        }
    });
}

fn group_label(cx: &Cx<'_>, g: ClaudeGroup) -> String {
    cx.tr.t(&format!("claude.settings.group.{}", g.id()))
}

fn rail(ui: &mut Ui, cx: &Cx<'_>, s: &mut SettingsState, model: &SettingsModel) {
    let counts = model.counts(&s.filter, &s.pending);
    let width = ui.available_width();
    ui.spacing_mut().item_spacing.y = 4.0;
    let mut pick = None;
    let count = |k: &str| counts.get(k).copied().unwrap_or(0);
    for (rail, label, n) in [
        (
            Rail::Common,
            cx.tr.t("claude.settings.common"),
            count("common"),
        ),
        (
            Rail::Changed,
            cx.tr.t("claude.settings.changed"),
            count("changed"),
        ),
    ] {
        if crate::pages::settings::rail_item(ui, cx, width, &label, n, s.filter.rail == rail) {
            pick = Some(rail);
        }
    }
    let (r, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
    ui.painter().hline(
        r.x_range().shrink(4.0),
        r.center().y,
        egui::Stroke::new(1.0, cx.pal.border),
    );
    for g in ClaudeGroup::ALL {
        let on = s.filter.rail == Rail::Group(g);
        if crate::pages::settings::rail_item(ui, cx, width, &group_label(cx, g), count(g.id()), on)
        {
            pick = Some(Rail::Group(g));
        }
    }
    if let Some(r) = pick {
        s.filter.rail = r;
        s.filter.query.clear();
    }
}

fn list(ui: &mut Ui, cx: &Cx<'_>, s: &mut SettingsState, model: &SettingsModel) {
    let rows = s.rows();
    if rows.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(Text::strong(cx.tr.t("claude.settings.empty_title"), cx.pal));
            ui.label(Text::small(cx.tr.t("claude.settings.empty_body"), cx.pal));
        });
        return;
    }
    let zh = cx.tr.locale() == Locale::ZhCn;
    let columns = Table::new(&[1.0, 0.0]).min_widths(&[0.0, CHIP_W]);
    let pending = s.pending.clone();
    let total = rows.len();
    PagedTable::new("claude-settings-list", columns, total, &mut s.table)
        .label(cx.tr.t(Page::ClaudeSettings.title_key()))
        .row_height(size::ROW_LIST2)
        .without_pager()
        .keys_when_idle(true)
        .row_name(|i| rows[i].path().to_owned())
        .show(ui, |ui, row, col| {
            let r = &rows[row];
            if col == 0 {
                let (marker, _) = ui.allocate_exact_size(vec2(8.0, 40.0), Sense::hover());
                if pending.get(r.path()).is_some() {
                    ui.painter()
                        .circle_filled(marker.center(), 3.0, cx.pal.status(Status::Warn));
                } else if r.owned {
                    ui.painter()
                        .circle_filled(marker.center(), 3.0, cx.pal.text);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let title = r.title(zh);
                        let t = if r.entry.name_zh.is_some() && zh {
                            RichText::new(title)
                                .font(font(text::BODY, Weight::Medium))
                                .color(cx.pal.text)
                        } else {
                            RichText::new(title)
                                .font(mono_font(text::SMALL, Weight::Medium))
                                .color(cx.pal.text)
                        };
                        ui.add(egui::Label::new(t).truncate());
                        if r.entry.env {
                            ui.add(Badge::new(cx.tr.t("claude.settings.badge.env")).outline());
                        }
                        if r.entry.deprecated {
                            ui.add(
                                Badge::new(cx.tr.t("claude.settings.badge.deprecated")).outline(),
                            );
                        }
                        if r.entry.source == ClaudeSource::File {
                            ui.add(
                                Badge::new(cx.tr.t("claude.settings.badge.file"))
                                    .status(Status::Warn),
                            );
                        }
                    });
                    let sub = if r.entry.name_zh.is_some() && zh {
                        r.path().to_owned()
                    } else {
                        r.entry
                            .en
                            .as_deref()
                            .map(|e| e.lines().next().unwrap_or_default().to_owned())
                            .unwrap_or_else(|| r.path().to_owned())
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(sub)
                                .font(mono_font(text::CAPTION, Weight::Regular))
                                .color(cx.pal.text_subtle),
                        )
                        .truncate(),
                    );
                });
            } else {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let v = model.value_with(r, &pending);
                    let (t, muted) = match v {
                        Some(v) => (editors::shown(cx, v), false),
                        None => match &r.entry.default {
                            Some(d) => (editors::shown(cx, d), true),
                            None => (cx.tr.t("claude.settings.value.unset"), true),
                        },
                    };
                    let shown = crate::pages::shared::values::Shown::label(t);
                    crate::pages::shared::values::chip(ui, cx, &shown, CHIP_W, muted);
                });
            }
        });
}

fn detail(
    ui: &mut Ui,
    cx: &Cx<'_>,
    s: &mut SettingsState,
    model: &SettingsModel,
    out: &mut Vec<Action>,
) {
    let rows = s.rows();
    let Some(row) = s.table.selected.and_then(|i| rows.get(i)).cloned() else {
        ui.label(Text::muted(cx.tr.t("claude.settings.pick"), cx.pal));
        return;
    };
    let zh = cx.tr.locale() == Locale::ZhCn;
    egui::ScrollArea::vertical()
        .id_salt(("claude-settings-detail", row.path().to_owned()))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if zh && let Some(n) = &row.entry.name_zh {
                ui.label(
                    RichText::new(n)
                        .font(font(text::TITLE, Weight::Semibold))
                        .color(cx.pal.text),
                );
            }
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(row.path())
                        .font(mono_font(text::SMALL, Weight::Medium))
                        .color(cx.pal.text),
                );
                if ui
                    .add(crate::design::widgets::IconButton::new(
                        Icon::Copy,
                        cx.tr.t("claude.settings.copy_key"),
                    ))
                    .clicked()
                {
                    out.push(Action::Copy(row.path().to_owned()));
                }
            });
            ui.horizontal_wrapped(|ui| {
                let source = cx.tr.t(match row.entry.source {
                    ClaudeSource::Schemastore => "claude.settings.badge.official",
                    ClaudeSource::Binary => "claude.settings.badge.binary",
                    ClaudeSource::File => "claude.settings.badge.file",
                });
                ui.add(Badge::new(source).outline());
                ui.add(Badge::new(group_label(cx, row.entry.group)).outline());
                if row.entry.env {
                    ui.add(Badge::new(cx.tr.t("claude.settings.badge.env")).outline());
                }
                if row.entry.deprecated {
                    ui.add(
                        Badge::new(cx.tr.t("claude.settings.badge.deprecated"))
                            .status(Status::Warn),
                    );
                }
                if row.owned {
                    ui.add(Badge::new(cx.tr.t("claude.settings.badge.owned")).status(Status::Ok));
                }
            });
            if let Some(z) = row.entry.zh.as_deref().filter(|_| zh) {
                ui.add(egui::Label::new(Text::small(z, cx.pal)).wrap());
            }
            if let Some(en) = &row.entry.en {
                ui.add(egui::Label::new(Text::caption(en, cx.pal)).wrap());
            }
            if row.entry.source == ClaudeSource::File {
                ui.add(
                    egui::Label::new(Text::caption(cx.tr.t("claude.settings.file_only"), cx.pal))
                        .wrap(),
                );
            }
            ui.add_space(6.0);
            let current = model.value_with(&row, &s.pending).cloned();
            if row.owned {
                crate::pages::note(ui, cx, Status::Idle, cx.tr.t("claude.settings.owned_note"));
                if let Some(v) = &current {
                    let shown = if superba_core::claude::is_secret_path(row.path()) {
                        superba_core::claude::display_value(row.path(), v)
                    } else {
                        editors::shown(cx, v)
                    };
                    ui.label(RichText::new(shown).font(mono_font(text::SMALL, Weight::Regular)));
                }
                if ui
                    .add(Button::ghost(cx.tr.t("claude.settings.go_connect")).small())
                    .clicked()
                {
                    out.push(Action::Go(Page::ClaudeCode));
                }
                return;
            }
            if superba_core::claude::is_secret_path(row.path()) && current.is_some() {
                ui.label(Text::caption(
                    cx.tr.t("claude.settings.secret_note"),
                    cx.pal,
                ));
            }
            if model.error.is_some() {
                ui.label(Text::caption(
                    cx.tr.t("claude.settings.fix_file_first"),
                    cx.pal,
                ));
                return;
            }
            let change = editors::editor(ui, cx, &row.entry, current.as_ref(), &mut s.buffers);
            if let Some(v) = change {
                s.edit(row.path(), v);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if let Some(d) = &row.entry.default {
                    ui.label(Text::caption(
                        cx.tr.f(
                            "claude.settings.default",
                            &[("value", &editors::shown(cx, d))],
                        ),
                        cx.pal,
                    ));
                }
                ui.add_space(8.0);
                {
                    if current.is_some()
                        && ui
                            .add(Button::ghost(cx.tr.t("claude.settings.remove_key")).small())
                            .on_hover_text(cx.tr.t("claude.settings.remove_key_tip"))
                            .clicked()
                    {
                        s.edit(row.path(), None);
                        s.buffers.retain(|k, _| !k.starts_with(row.path()));
                    }
                    if s.pending.get(row.path()).is_some()
                        && ui
                            .add(Button::ghost(cx.tr.t("claude.settings.undo")).small())
                            .clicked()
                    {
                        s.pending.0.remove(row.path());
                        s.buffers.retain(|k, _| !k.starts_with(row.path()));
                    }
                }
            });
            if !row.entry.examples.is_empty() {
                let ex: Vec<String> = row
                    .entry
                    .examples
                    .iter()
                    .take(4)
                    .map(|e| e.to_string())
                    .collect();
                ui.label(Text::caption(
                    cx.tr
                        .f("claude.settings.examples", &[("list", &ex.join("  "))]),
                    cx.pal,
                ));
            }
        });
}

fn footer(ui: &mut Ui, cx: &Cx<'_>, s: &mut SettingsState, list_x: egui::Rangef) {
    let rect = ui.max_rect();
    let total = s.rows().len();
    let per = s.table.per_page.max(1);
    let pager_rect = egui::Rect::from_x_y_ranges(list_x.min..=list_x.max, rect.y_range());
    let mut page = s.table.page;
    let changed = layout::fixed(ui, pager_rect, |ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            Pager::new("claude-settings-pager", &mut page, total, per)
                .labels(
                    cx.tr.t("claude.settings.prev"),
                    cx.tr.t("claude.settings.next"),
                )
                .show(ui)
                .changed()
        })
        .inner
    });
    if changed {
        s.table.page = page;
        let visible = s.table.visible(total);
        if s.table.selected.is_some_and(|x| !visible.contains(&x)) {
            s.table.selected = (total > 0).then_some(visible.start);
        }
    }
    let count = s.pending.len();
    if count == 0 {
        return;
    }
    let labels = PendingLabels {
        count: cx.tr.f("claude.settings.pending", &[("n", &count)]),
        diff: cx.tr.t("claude.settings.diff"),
        discard: cx.tr.t("claude.settings.discard"),
        save: cx.tr.t("claude.settings.save"),
    };
    let bar = egui::Rect::from_x_y_ranges(
        (list_x.max.min(rect.max.x - 360.0)).max(rect.min.x)..=rect.max.x,
        rect.y_range(),
    );
    let action = layout::fixed(ui, bar, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            PendingBar::new(count, labels)
                .saving(s.saving.is_some())
                .error(s.save_error.clone())
                .show(ui)
        })
        .inner
    });
    match action {
        PendingAction::Save => s.save(cx),
        PendingAction::Discard => {
            s.pending.0.clear();
            s.buffers.clear();
            s.save_error = None;
        }
        PendingAction::Diff => s.diff_open = true,
        PendingAction::None => {}
    }
}

fn diff_dialog(ctx: &egui::Context, cx: &Cx<'_>, s: &mut SettingsState, model: &SettingsModel) {
    let mut close = false;
    let mut save = false;
    let old = s
        .doc
        .as_ref()
        .and_then(CodexFileDoc::text)
        .unwrap_or_default();
    let new = model.render(&s.pending).unwrap_or_default();
    let mut masker = superba_switch::codex_files::Masker::for_kind(CodexFileKind::ClaudeSettings);
    let (old, new) = (masker.mask(&old), masker.mask(&new));
    let r = dialog(ctx, egui::Id::unique("claude-settings-diff"), 720.0, |ui| {
        ui.label(Text::title(cx.tr.t("claude.settings.diff_title"), cx.pal));
        ui.label(Text::small(cx.tr.t("claude.settings.diff_body"), cx.pal));
        crate::pages::settings::files_view::diff_view(
            ui,
            cx,
            "claude-settings-diff",
            &old,
            &new,
            380.0,
        );
        if let Some(e) = &s.save_error {
            crate::pages::note(ui, cx, Status::Error, e.clone());
        }
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t("claude.settings.save")).loading(s.saving.is_some()))
                .clicked()
            {
                save = true;
            }
            if ui.add(Button::ghost(cx.tr.t("claude.cancel"))).clicked() {
                close = true;
            }
        });
    });
    if r.should_close() || close {
        s.diff_open = false;
    }
    if save {
        s.save(cx);
    }
}

/// Unsaved edits and dialogs.
pub fn status(s: &SettingsState) -> PageStatus {
    if s.files.open {
        return PageStatus {
            modal: s.files.dialog.is_some(),
            pending: usize::from(s.files.dirty()),
            ..PageStatus::default()
        };
    }
    PageStatus {
        modal: s.diff_open,
        pending: s.pending.len(),
        ..PageStatus::default()
    }
}

/// Run a page command.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = out;
    let s = &mut state.settings;
    if command == "files" {
        s.files
            .open_file(cx, TargetId::local(), CodexFileKind::ClaudeSettings);
    } else if command == "files-backups" {
        s.files
            .open_file(cx, TargetId::local(), CodexFileKind::ClaudeSettings);
        if s.files.dialog.is_none() {
            s.files.open_backups(cx);
        }
    } else if s.files.open && command == "save" {
        s.files.review();
    } else if let Some(path) = command.strip_prefix("focus:") {
        s.focus(path);
    } else if command == "search" {
        s.focus_search = true;
    } else if command == "save" {
        s.save(cx);
    }
}

/// Palette entries: every key with its Chinese name.
pub fn palette(cx: &Cx<'_>, s: &SettingsState) -> Vec<PaletteItem> {
    let Some(model) = &s.model else {
        return Vec::new();
    };
    let zh = cx.tr.locale() == Locale::ZhCn;
    model
        .rows
        .iter()
        .filter(|r| !r.entry.deprecated || r.value.is_some())
        .map(|r| PaletteItem {
            title: format!("{} · Claude Code", r.title(zh)),
            detail: Some(r.path().to_owned()),
            keywords: [Some(r.path().to_owned()), r.entry.name_zh.clone()]
                .into_iter()
                .flatten()
                .collect(),
            group: PaletteGroup::Settings,
            action: Action::PageCommand {
                page: Page::ClaudeSettings,
                command: format!("focus:{}", r.path()),
            },
        })
        .collect()
}
