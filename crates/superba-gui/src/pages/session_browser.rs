//! The Sessions page's session browser (sessions2): every thread the desktop
//! app knows on every host (this computer's `state_*.sqlite` and the app's
//! thread catalog, [`superba_core::SwitchService::session_list`]), with fuzzy
//! search over title, directory, ids and host, host and provider filters, a
//! paged list with Ctrl/⌘-click and Shift-click multi-selection, and a context
//! menu: copy the app's `codex://threads/<id>?hostId=…` link, open it in
//! ChatGPT, copy the id or directory, show the rollout file, copy as Markdown,
//! export as a Claude Code or pi session. Session migration is a tool behind
//! the header's 「会话迁移…」 button (`sessions.rs`).
//!
//! Reads run in the background ([`crate::runtime::task`]); the list is read
//! once when the page opens and again on Rescan. Exports never write into a
//! Codex store (Markdown goes to the clipboard, sessions to new files).

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align, Layout, Ui};
use superba_core::{
    Error, HostKind, RequestHarness, RequestStats, RequestTotals, SessionEntry, SessionExport,
    SessionExportFormat, SessionList, StatsGroupBy, StatsQuery, fuzzy_fields, thread_link,
};

use super::shared::format::compact;
use super::shared::loading;
use super::{Action, Cx, empty_state, error_text};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, space};
use crate::design::widgets::{
    Button, Dropdown, PagedState, PagedTable, Status, StatusDot, Table, Text,
};
use crate::runtime::ReadTask;

/// Width of the search field.
const SEARCH_W: f32 = 280.0;
/// Width of a filter dropdown.
const FILTER_W: f32 = 150.0;

/// A thread's identity: (host id, thread id).
pub type Key = (String, String);

/// A running export and what it was for.
struct ExportTask {
    format: SessionExportFormat,
    count: usize,
    task: ReadTask<SessionExport>,
}

/// State of the browser, kept across navigation.
#[derive(Default)]
pub struct Browser {
    /// Last list read.
    pub list: Option<SessionList>,
    /// Last read error.
    pub error: Option<Error>,
    task: Option<ReadTask<SessionList>>,
    /// Router totals per Codex thread id.
    pub costs: BTreeMap<String, RequestTotals>,
    cost_task: Option<ReadTask<RequestStats>>,
    /// Search text.
    pub search: String,
    /// Host filter (`None` = every host).
    pub host: Option<String>,
    /// Provider filter (`None` = every provider).
    pub provider: Option<String>,
    /// Paging and the focused row (index into the filtered list).
    pub paging: PagedState,
    /// Rows picked for bulk actions.
    pub checked: BTreeSet<Key>,
    /// Filtered rows (indices into the list) and the inputs they came from.
    shown: Vec<usize>,
    shown_for: Option<(String, Option<String>, Option<String>, usize)>,
    export: Option<ExportTask>,
    /// Result of the last export: status and text.
    pub notice: Option<(Status, String)>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("rows", &self.list.as_ref().map(|l| l.entries.len()))
            .field("search", &self.search)
            .field("checked", &self.checked.len())
            .finish_non_exhaustive()
    }
}

impl Browser {
    /// Read the list (and the router's per-session totals) unless a read runs.
    pub fn load(&mut self, cx: &Cx<'_>) {
        if self.task.is_none() {
            let service = cx.service.clone();
            self.task = Some(crate::runtime::task(
                async move { service.session_list().await },
            ));
        }
        if self.cost_task.is_none() {
            let service = cx.service.clone();
            let query = StatsQuery {
                by: StatsGroupBy::Session,
                harnesses: vec![RequestHarness::Codex],
                ..StatsQuery::default()
            };
            self.cost_task = Some(crate::runtime::task(async move {
                service.router_stats(query).await
            }));
        }
    }

    /// A list read is running.
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }

    /// Set the list (tests).
    pub fn set_list(&mut self, list: SessionList) {
        self.list = Some(list);
        self.error = None;
        self.task = None;
        self.shown_for = None;
    }

    /// Poll the background reads; true while one runs.
    fn poll(&mut self) -> bool {
        let mut busy = false;
        if let Some(task) = &self.task {
            match task.poll() {
                Some(Ok(list)) => {
                    self.list = Some(list);
                    self.error = None;
                    self.task = None;
                    self.shown_for = None;
                }
                Some(Err(e)) => {
                    self.error = Some(e);
                    self.task = None;
                }
                None => busy = true,
            }
        }
        if let Some(task) = &self.cost_task {
            match task.poll() {
                Some(Ok(stats)) => {
                    self.costs = stats
                        .buckets
                        .into_iter()
                        .filter_map(|b| {
                            let (tool, id) = superba_core::split_harness_key(&b.key)?;
                            (tool == RequestHarness::Codex).then(|| (id.to_owned(), b.totals))
                        })
                        .collect();
                    self.cost_task = None;
                }
                // The router log is optional: no totals without it.
                Some(Err(_)) => self.cost_task = None,
                None => busy = true,
            }
        }
        busy
    }

    /// Recompute the filtered rows when the list or a filter changed.
    fn refresh_shown(&mut self, cx: &Cx<'_>) {
        let Some(list) = &self.list else {
            self.shown.clear();
            return;
        };
        let key = (
            self.search.clone(),
            self.host.clone(),
            self.provider.clone(),
            list.entries.len(),
        );
        if self.shown_for.as_ref() == Some(&key) {
            return;
        }
        self.shown = filter(cx, &list.entries, &self.search, &self.host, &self.provider);
        self.shown_for = Some(key);
        self.paging.clamp(self.shown.len());
        if self.paging.selected.is_none() || !self.search.is_empty() {
            self.paging.page = 0;
        }
    }

    fn entry(&self, row: usize) -> Option<&SessionEntry> {
        let i = *self.shown.get(row)?;
        self.list.as_ref()?.entries.get(i)
    }

    /// The checked entries, in list order.
    fn checked_entries(&self) -> Vec<&SessionEntry> {
        self.list
            .as_ref()
            .map(|l| {
                l.entries
                    .iter()
                    .filter(|e| self.checked.contains(&key_of(e)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Export `entries` (this computer's only) as `format` in the background.
    fn start_export(
        &mut self,
        cx: &Cx<'_>,
        entries: &[&SessionEntry],
        format: SessionExportFormat,
    ) {
        let ids: Vec<String> = entries
            .iter()
            .filter(|e| e.is_local())
            .map(|e| e.thread_id.clone())
            .collect();
        if ids.is_empty() || self.export.is_some() {
            return;
        }
        let service = cx.service.clone();
        let count = ids.len();
        let task = crate::runtime::task(async move {
            let mut parts = Vec::new();
            let mut last = SessionExport::default();
            for id in ids {
                last = service.session_export(id, format).await?;
                parts.push(last.text.clone());
            }
            if format == SessionExportFormat::Markdown {
                last.text = parts.join("\n\n---\n\n");
            }
            Ok(last)
        });
        self.notice = None;
        self.export = Some(ExportTask {
            format,
            count,
            task,
        });
    }

    fn poll_export(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        let Some(e) = &self.export else {
            return false;
        };
        match e.task.poll() {
            None => true,
            Some(result) => {
                let (format, count) = (e.format, e.count);
                self.export = None;
                match result {
                    Ok(x) => {
                        out.push(Action::Copy(x.text.clone()));
                        let text = match (format, &x.path) {
                            (SessionExportFormat::Markdown, _) => {
                                cx.tr.f("sessions.browse.exported_md", &[("n", &count)])
                            }
                            (_, Some(path)) => cx.tr.f(
                                "sessions.browse.exported_session",
                                &[("path", &path.display().to_string())],
                            ),
                            (_, None) => x.text.clone(),
                        };
                        self.notice = Some((Status::Ok, text));
                    }
                    Err(err) => {
                        self.notice = Some((
                            Status::Error,
                            cx.tr.f(
                                "sessions.browse.export_failed",
                                &[("error", &error_text(cx.tr, &err))],
                            ),
                        ));
                    }
                }
                false
            }
        }
    }
}

/// The identity of an entry.
pub fn key_of(e: &SessionEntry) -> Key {
    (e.host_id.clone(), e.thread_id.clone())
}

/// What to call a host id: this computer, an SSH alias, a WSL distribution, a
/// host the app lists under a name, else the id itself.
pub fn host_label(cx: &Cx<'_>, host_id: &str) -> String {
    if host_id == superba_core::LOCAL_HOST_ID {
        return cx.tr.t("sessions.host.local");
    }
    let known = cx
        .data
        .hosts
        .value
        .iter()
        .flatten()
        .map(|h| &h.host)
        .chain(
            cx.data
                .overview
                .value
                .iter()
                .flat_map(|o| o.remotes.iter().map(|r| &r.host)),
        )
        .find(|h| h.host_id.as_deref() == Some(host_id));
    if let Some(h) = known {
        return super::shared::hosts::title(cx.tr, h);
    }
    if let Some(alias) = superba_core::discovered_ssh_alias(host_id) {
        return alias;
    }
    if let Some(distro) = host_id.strip_prefix("remote-wsl:") {
        return format!("{distro} (WSL)");
    }
    host_id.to_owned()
}

/// Rows of `entries` that pass the filters and match `search`, best match
/// first (newest first without a search).
pub fn filter(
    cx: &Cx<'_>,
    entries: &[SessionEntry],
    search: &str,
    host: &Option<String>,
    provider: &Option<String>,
) -> Vec<usize> {
    let mut rows: Vec<(i32, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| host.as_ref().is_none_or(|h| &e.host_id == h))
        .filter(|(_, e)| provider.as_ref().is_none_or(|p| &e.provider == p))
        .filter_map(|(i, e)| {
            if search.trim().is_empty() {
                return Some((0, i));
            }
            let host = host_label(cx, &e.host_id);
            fuzzy_fields(
                search,
                &[
                    (&e.title, 3),
                    (&e.cwd, 2),
                    (&e.thread_id, 1),
                    (&host, 1),
                    (e.model.as_deref().unwrap_or(""), 1),
                ],
            )
            .map(|s| (s, i))
        })
        .collect();
    // Stable: equal scores keep the list's newest-first order.
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    rows.into_iter().map(|(_, i)| i).collect()
}

/// The last two parts of a directory (`~/w/superba` → `w/superba`).
fn project(cwd: &str) -> String {
    let parts: Vec<&str> = cwd.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0 => cwd.to_owned(),
        1 => parts[0].to_owned(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// The title to show (`—` style fallback: the short id).
fn title_of(e: &SessionEntry) -> String {
    if e.title.is_empty() {
        e.thread_id.chars().take(8).collect()
    } else {
        e.title.clone()
    }
}

/// Draw the browser below the page header.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, b: &mut Browser, out: &mut Vec<Action>) {
    if b.list.is_none() && b.error.is_none() && b.task.is_none() {
        b.load(cx);
    }
    let mut busy = b.poll();
    busy |= b.poll_export(cx, out);
    if busy {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    b.refresh_shown(cx);
    let body = ui.available_rect_before_wrap();
    let bulk = !b.checked.is_empty() || b.notice.is_some() || b.export.is_some();
    let rows = layout::row_rects(
        body,
        &[
            Track::Fixed(size::CONTROL),
            Track::Fill,
            Track::Fixed(if bulk { size::CONTROL } else { 0.0 }),
        ],
        space::M,
    );
    layout::fixed(ui, rows[0], |ui| filters(ui, cx, b));
    layout::fixed(ui, rows[1], |ui| match (&b.list, &b.error) {
        (None, Some(e)) => {
            if loading::failed_card(
                ui,
                cx,
                &cx.tr.t("sessions.browse.load_failed"),
                &error_text(cx.tr, e),
            ) {
                b.error = None;
                b.load(cx);
            }
        }
        (None, None) => loading::loading_card(
            ui,
            cx,
            &cx.tr.t("sessions.browse.loading"),
            &cx.tr.t("sessions.browse.loading_body"),
        ),
        (Some(list), _) if list.entries.is_empty() => empty_state(
            ui,
            cx,
            Icon::Sessions,
            &cx.tr.t("sessions.browse.empty"),
            &cx.tr.t("sessions.browse.empty_body"),
        ),
        (Some(_), _) if b.shown.is_empty() => empty_state(
            ui,
            cx,
            Icon::Search,
            &cx.tr.t("sessions.browse.no_match"),
            &cx.tr.t("sessions.browse.no_match_body"),
        ),
        (Some(_), _) => table(ui, cx, b, out),
    });
    if bulk {
        layout::fixed(ui, rows[2], |ui| bulk_bar(ui, cx, b, out));
    }
}

/// Search, host and provider filters, and the count.
fn filters(ui: &mut Ui, cx: &Cx<'_>, b: &mut Browser) {
    let entries: &[SessionEntry] = b.list.as_ref().map_or(&[], |l| &l.entries);
    let hosts: BTreeSet<String> = entries.iter().map(|e| e.host_id.clone()).collect();
    let providers: BTreeSet<String> = entries
        .iter()
        .map(|e| e.provider.clone())
        .filter(|p| !p.is_empty())
        .collect();
    let total = entries.len();
    let shown = b.shown.len();
    let truncated = b.list.as_ref().is_some_and(|l| l.truncated);
    let mut host = b.host.clone();
    let mut provider = b.provider.clone();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        let name = cx.tr.t("sessions.browse.search");
        let width = SEARCH_W.min(ui.available_width() * 0.4);
        let r = ui.add_sized(
            [width, size::CONTROL_SM],
            egui::TextEdit::singleline(&mut b.search)
                .hint_text(cx.tr.t("sessions.browse.search_hint"))
                .align(egui::Align2::LEFT_CENTER),
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, &name));
        if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            b.search.clear();
        }
        if !b.search.is_empty()
            && ui
                .add(Button::ghost(cx.tr.t("sessions.browse.clear_search")).small())
                .clicked()
        {
            b.search.clear();
        }
        let label = |h: &Option<String>| match h {
            None => cx.tr.t("sessions.browse.all_hosts"),
            Some(h) => host_label(cx, h),
        };
        Dropdown::new("sessions-browse-host")
            .label(cx.tr.t("sessions.browse.host"))
            .width(FILTER_W)
            .show_options(
                ui,
                &mut host,
                &std::iter::once(None)
                    .chain(hosts.iter().cloned().map(Some))
                    .map(|h| {
                        let l = label(&h);
                        (h, l)
                    })
                    .collect::<Vec<_>>(),
            );
        let plabel = |p: &Option<String>| match p {
            None => cx.tr.t("sessions.browse.all_providers"),
            Some(p) => p.clone(),
        };
        Dropdown::new("sessions-browse-provider")
            .label(cx.tr.t("sessions.browse.provider"))
            .width(FILTER_W)
            .show_options(
                ui,
                &mut provider,
                &std::iter::once(None)
                    .chain(providers.iter().cloned().map(Some))
                    .map(|p| {
                        let l = plabel(&p);
                        (p, l)
                    })
                    .collect::<Vec<_>>(),
            );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let count = if shown == total {
                cx.tr.f("sessions.browse.count", &[("n", &total)])
            } else {
                cx.tr.f(
                    "sessions.browse.count_of",
                    &[("n", &shown), ("total", &total)],
                )
            };
            let r = ui.add(egui::Label::new(Text::caption(count, cx.pal)).truncate());
            if truncated {
                r.on_hover_text(cx.tr.t("sessions.browse.truncated"));
            }
        });
    });
    if host != b.host || provider != b.provider {
        b.host = host;
        b.provider = provider;
        b.paging = PagedState::default();
    }
}

/// The paged list with selection and the context menu.
fn table(ui: &mut Ui, cx: &Cx<'_>, b: &mut Browser, out: &mut Vec<Action>) {
    let headers = vec![
        cx.tr.t("sessions.browse.col.title"),
        cx.tr.t("sessions.browse.col.project"),
        cx.tr.t("sessions.browse.col.host"),
        cx.tr.t("sessions.browse.col.model"),
        cx.tr.t("sessions.browse.col.updated"),
        cx.tr.t("sessions.browse.col.tokens"),
    ];
    let table = Table::new(&[3.2, 2.0, 1.2, 1.1, 1.1, 0.8])
        .min_widths(&[160.0, 90.0, 70.0, 60.0, 70.0, 56.0])
        .right_aligned(&[5]);
    let total = b.shown.len();
    let entries: Vec<SessionEntry> = b
        .shown
        .iter()
        .filter_map(|i| b.list.as_ref()?.entries.get(*i).cloned())
        .collect();
    let checked = b.checked.clone();
    let costs = &b.costs;
    let name = |row: usize| {
        entries.get(row).map_or_else(String::new, |e| {
            format!(
                "{}, {}, {}",
                title_of(e),
                project(&e.cwd),
                host_label(cx, &e.host_id)
            )
        })
    };
    let before = b.paging.selected;
    let resp = PagedTable::new("sessions-browse", table, total, &mut b.paging)
        .label(cx.tr.t("sessions.browse.a11y.list"))
        .header(headers)
        .right_aligned(&[5])
        .truncate_headers()
        .keys_when_idle(true)
        .row_name(name)
        .show(ui, |ui, row, col| {
            let Some(e) = entries.get(row) else { return };
            match col {
                0 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS;
                        if checked.contains(&key_of(e)) {
                            ui.add(StatusDot::new(Status::Ok));
                        }
                        let mut t = Text::small(title_of(e), cx.pal);
                        if e.archived {
                            t = t.color(cx.pal.text_muted);
                        }
                        let r = ui.add(egui::Label::new(t).truncate());
                        r.on_hover_text(if e.archived {
                            format!("{}\n{}", e.title, cx.tr.t("sessions.browse.archived"))
                        } else {
                            e.title.clone()
                        });
                    });
                }
                1 => {
                    ui.add(egui::Label::new(Text::mono_muted(project(&e.cwd), cx.pal)).truncate())
                        .on_hover_text(&e.cwd);
                }
                2 => {
                    let kind = HostKind::of_host_id(&e.host_id);
                    let label = host_label(cx, &e.host_id);
                    ui.add(egui::Label::new(Text::small(label, cx.pal)).truncate())
                        .on_hover_text(match kind {
                            HostKind::Unknown if e.is_local() => cx.tr.t("sessions.host.local"),
                            _ => e.host_id.clone(),
                        });
                }
                3 => {
                    ui.add(
                        egui::Label::new(Text::small(
                            e.model.clone().unwrap_or_else(|| "—".into()),
                            cx.pal,
                        ))
                        .truncate(),
                    );
                }
                4 => {
                    let text = e.updated_at.map_or_else(|| "—".into(), |t| cx.ago(t));
                    let r = ui.add(egui::Label::new(Text::small(text, cx.pal)).truncate());
                    if let Some(t) = e.updated_at {
                        r.on_hover_text(superba_switch::localtime::local(t).to_string());
                    }
                }
                _ => {
                    let router = costs.get(&e.thread_id);
                    let tokens = e
                        .tokens_used
                        .or_else(|| router.map(|t| t.input_tokens + t.output_tokens));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let r = ui.label(Text::small(
                            tokens.map_or_else(|| "—".into(), compact),
                            cx.pal,
                        ));
                        if let Some(t) = router {
                            r.on_hover_text(cx.tr.f(
                                "sessions.browse.router_totals",
                                &[
                                    ("n", &t.requests),
                                    ("cost", &super::router::requests::cost_text(Some(t.cost))),
                                ],
                            ));
                        }
                    });
                }
            }
        });
    // Ctrl/⌘-click toggles a row into the bulk selection; Shift-click
    // extends it from the focused row; a plain click picks one row.
    let mods = ui.input(|i| i.modifiers);
    for (row, r) in &resp.rows {
        if r.clicked() {
            if mods.command {
                if let Some(e) = entries.get(*row) {
                    let k = key_of(e);
                    if !b.checked.remove(&k) {
                        if b.checked.is_empty()
                            && let Some(prev) = before.and_then(|p| entries.get(p))
                        {
                            b.checked.insert(key_of(prev));
                        }
                        b.checked.insert(k);
                    }
                }
            } else if mods.shift {
                let from = before.unwrap_or(*row);
                let (lo, hi) = (from.min(*row), from.max(*row));
                for e in entries.iter().take(hi + 1).skip(lo) {
                    b.checked.insert(key_of(e));
                }
            } else {
                b.checked.clear();
            }
        }
        let entry = entries.get(*row).cloned();
        r.context_menu(|ui| {
            if let Some(e) = &entry {
                context_menu(ui, cx, b, e, out);
            }
        });
    }
    if let Some(row) = resp.activated
        && let Some(e) = entries.get(row)
    {
        out.push(Action::OpenUrl(thread_link(&e.thread_id, &e.host_id)));
    }
}

/// The right-click menu: the bulk menu on a checked row of several, else the
/// row's own.
fn context_menu(
    ui: &mut Ui,
    cx: &Cx<'_>,
    b: &mut Browser,
    e: &SessionEntry,
    out: &mut Vec<Action>,
) {
    ui.set_min_width(220.0);
    if b.checked.len() > 1 && b.checked.contains(&key_of(e)) {
        bulk_menu(ui, cx, b, out);
        return;
    }
    let link = thread_link(&e.thread_id, &e.host_id);
    let item = |ui: &mut Ui, key: &str| ui.button(cx.tr.t(key)).clicked();
    if item(ui, "sessions.browse.menu.copy_link") {
        out.push(Action::Copy(link.clone()));
        ui.close();
    }
    if ui
        .button(cx.tr.t("sessions.browse.menu.open"))
        .on_hover_text(&link)
        .clicked()
    {
        out.push(Action::OpenUrl(link.clone()));
        ui.close();
    }
    if item(ui, "sessions.browse.menu.copy_id") {
        out.push(Action::Copy(e.thread_id.clone()));
        ui.close();
    }
    if !e.cwd.is_empty() && item(ui, "sessions.browse.menu.copy_cwd") {
        out.push(Action::Copy(e.cwd.clone()));
        ui.close();
    }
    ui.separator();
    let local = e.is_local();
    let only_local = cx.tr.t("sessions.browse.menu.local_only");
    let folder = e
        .rollout_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.display().to_string());
    let r = ui.add_enabled(
        local && folder.is_some(),
        egui::Button::new(cx.tr.t("sessions.browse.menu.show_rollout")),
    );
    if r.clicked()
        && let Some(f) = folder
    {
        out.push(Action::OpenUrl(f));
        ui.close();
    }
    for (key, format) in [
        (
            "sessions.browse.menu.markdown",
            SessionExportFormat::Markdown,
        ),
        (
            "sessions.browse.menu.claude",
            SessionExportFormat::ClaudeCode,
        ),
        ("sessions.browse.menu.pi", SessionExportFormat::Pi),
    ] {
        let r = ui.add_enabled(local && b.export.is_none(), egui::Button::new(cx.tr.t(key)));
        let r = if local {
            r
        } else {
            r.on_disabled_hover_text(&only_local)
        };
        if r.clicked() {
            b.start_export(cx, &[e], format);
            ui.close();
        }
    }
    ui.add_enabled(
        false,
        egui::Button::new(cx.tr.t("sessions.browse.menu.dsh")),
    )
    .on_disabled_hover_text(cx.tr.t("sessions.browse.menu.dsh_later"));
}

/// Copy links / Markdown of the checked rows, clear.
fn bulk_menu(ui: &mut Ui, cx: &Cx<'_>, b: &mut Browser, out: &mut Vec<Action>) {
    let n = b.checked.len();
    if ui
        .button(cx.tr.f("sessions.browse.bulk.copy_links", &[("n", &n)]))
        .clicked()
    {
        out.push(Action::Copy(links(b)));
        ui.close();
    }
    let entries: Vec<SessionEntry> = b.checked_entries().into_iter().cloned().collect();
    let local: Vec<&SessionEntry> = entries.iter().filter(|e| e.is_local()).collect();
    if ui
        .add_enabled(
            !local.is_empty() && b.export.is_none(),
            egui::Button::new(
                cx.tr
                    .f("sessions.browse.bulk.markdown", &[("n", &local.len())]),
            ),
        )
        .clicked()
    {
        b.start_export(cx, &local, SessionExportFormat::Markdown);
        ui.close();
    }
    if ui.button(cx.tr.t("sessions.browse.bulk.clear")).clicked() {
        b.checked.clear();
        ui.close();
    }
}

/// The links of the checked rows, one per line.
fn links(b: &Browser) -> String {
    b.checked_entries()
        .iter()
        .map(|e| thread_link(&e.thread_id, &e.host_id))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Below the list: the bulk actions of the checked rows and the last export.
fn bulk_bar(ui: &mut Ui, cx: &Cx<'_>, b: &mut Browser, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        let n = b.checked.len();
        if n > 0 {
            ui.label(Text::small(
                cx.tr.f("sessions.browse.bulk.selected", &[("n", &n)]),
                cx.pal,
            ));
            if ui
                .add(
                    Button::new(cx.tr.t("sessions.browse.bulk.copy_links_short"))
                        .icon(Icon::Copy)
                        .small(),
                )
                .clicked()
            {
                out.push(Action::Copy(links(b)));
            }
            let entries: Vec<SessionEntry> = b.checked_entries().into_iter().cloned().collect();
            let local: Vec<&SessionEntry> = entries.iter().filter(|e| e.is_local()).collect();
            if ui
                .add(
                    Button::new(cx.tr.t("sessions.browse.bulk.markdown_short"))
                        .small()
                        .enabled(!local.is_empty() && b.export.is_none()),
                )
                .clicked()
            {
                b.start_export(cx, &local, SessionExportFormat::Markdown);
            }
            if ui
                .add(Button::ghost(cx.tr.t("sessions.browse.bulk.clear")).small())
                .clicked()
            {
                b.checked.clear();
            }
        }
        if b.export.is_some() {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(Text::caption(cx.tr.t("sessions.browse.exporting"), cx.pal));
        } else if let Some((status, text)) = &b.notice {
            ui.add(StatusDot::new(*status));
            let r = ui.add(egui::Label::new(Text::caption(text.clone(), cx.pal)).truncate());
            r.on_hover_text(text.clone());
        }
    });
}

/// Selected entry (focused row), for tests.
pub fn focused(b: &Browser) -> Option<&SessionEntry> {
    b.paging.selected.and_then(|r| b.entry(r))
}
