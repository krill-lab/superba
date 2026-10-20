//! 最近请求 (UX §3.5): the Surge-style log. A source list on the left (按会话 /
//! 按供应商 with each provider's lines indented under it, "all", counts,
//! remote-host footnote), filters above the table,
//! the 13 columns with a pinned totals row, the toolbar (清空 / 重新载入 /
//! 暂停记录 / 收起详情; clearing and pausing act on the router's records only)
//! and the detail pane of the selected row (通用 / 计时).
//!
//! Rows come from the router's log and from Codex's session files (rollouts),
//! so the log works without the router; a rollout row shows "—" with a tooltip
//! in the columns only the router measures.
//!
//! Live while visible: the query re-runs every 2 s while the router is on (5 s
//! otherwise); new rows appear at the top only while the table is scrolled to
//! the top, otherwise a "↑ n new requests" pill offers them (Surge behaviour).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align, Layout, Ui};
use superba_core::{
    ProfileId, RequestClient, RequestDetail, RequestGroup, RequestGrouping, RequestHarness,
    RequestPage, RequestQuery, RequestRow, RequestSort, RequestSource, RequestStatusFilter,
    RequestTotals, SessionTitle, StatsBucket, StatsGroupBy, StatsQuery, Timestamp,
};

use super::db::Loader;
use super::requests::{self, Col, Range};
use super::{Dialog, State, detail, mode};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::widgets::detail_pane::{self, DetailPane};
use crate::design::widgets::log_table::{self, LogTable};
use crate::design::widgets::source_list::{COLLAPSED_WIDTH, SourceList};
use crate::design::widgets::toolbar::{self, Toolbar, tool_button};
use crate::design::widgets::{Button, Dropdown, Segmented, Status, Text};
use crate::pages::{Action, Cx, error_text};
use crate::runtime::ReadTask;

/// Live refresh interval while the router is on.
const LIVE: Duration = Duration::from_secs(2);
/// Refresh interval while the router is off (rows come from Codex's session
/// files only, read incrementally).
const ROLLOUT_LIVE: Duration = Duration::from_secs(5);
/// Search typing debounce.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Rows per query.
const LIMIT: u32 = 500;
/// Gap between the two filter rows.
const FILTER_GAP: f32 = 6.0;

/// Filter menus: models and members seen in the range.
#[derive(Clone, Debug, Default)]
pub struct Facets {
    /// Models (key = slug).
    pub models: Vec<StatsBucket>,
    /// Members (key = profile id, label = name).
    pub members: Vec<StatsBucket>,
    /// Hosts in the selected range.
    pub hosts: Vec<StatsBucket>,
}

/// State of 最近请求.
#[derive(Debug)]
pub struct Recent {
    /// Range.
    pub range: Range,
    /// Only this member.
    pub member: Option<ProfileId>,
    /// Only this model.
    pub model: Option<String>,
    /// Tools selected (empty means all tools).
    pub harnesses: Vec<RequestHarness>,
    /// Selected host; `local` means this computer.
    pub host: Option<String>,
    /// Status filter.
    pub status: RequestStatusFilter,
    /// Only this client.
    pub client: Option<RequestClient>,
    /// Only rows from this source (router, rollouts, an import).
    pub source: Option<RequestSource>,
    /// Source list grouping.
    pub grouping: RequestGrouping,
    /// Selected source list entry.
    pub group: Option<String>,
    /// Sort order.
    pub sort: RequestSort,
    /// Source list collapsed.
    pub collapsed: bool,
    /// The query.
    pub page: Loader<RequestPage>,
    /// Rows shown.
    pub shown: Vec<RequestRow>,
    /// Totals of the filters.
    pub totals: RequestTotals,
    /// Source list entries.
    pub groups: Vec<RequestGroup>,
    /// Count of the "all" entry.
    pub all_count: u64,
    /// More rows match than are shown.
    pub truncated: bool,
    /// Rows newer than the shown ones (pill).
    pub new_rows: usize,
    at_top: bool,
    scroll_top: bool,
    apply_next: bool,
    /// Selected row id.
    pub selected: Option<i64>,
    reveal: bool,
    /// The detail pane is collapsed.
    pub detail_hidden: bool,
    /// Detail tab.
    pub detail_tab: usize,
    /// Height the detail pane was dragged to (`None`: sized to its content).
    /// Kept while the pane is collapsed, so 展开详情 brings it back as it was.
    pub detail_height: Option<f32>,
    /// Detail of the selected row.
    pub detail: Loader<RequestDetail>,
    detail_for: Option<i64>,
    facets: Loader<Facets>,
    facets_range: Option<Range>,
    search_seen: String,
    search_at: Option<Instant>,
    export: Option<ReadTask<(PathBuf, usize)>>,
    export_done: Option<superba_core::Result<(PathBuf, usize)>>,
    /// What the provider and line names of the rows were resolved with.
    names_sig: u64,
}

impl Default for Recent {
    fn default() -> Self {
        Self {
            range: Range::Day,
            member: None,
            model: None,
            harnesses: Vec::new(),
            host: None,
            status: RequestStatusFilter::All,
            client: None,
            source: None,
            grouping: RequestGrouping::Session,
            group: None,
            sort: RequestSort::default(),
            collapsed: false,
            page: Loader::default(),
            shown: Vec::new(),
            totals: RequestTotals::default(),
            groups: Vec::new(),
            all_count: 0,
            truncated: false,
            new_rows: 0,
            at_top: true,
            scroll_top: false,
            apply_next: true,
            selected: None,
            reveal: false,
            detail_hidden: false,
            detail_tab: 0,
            detail_height: None,
            detail: Loader::default(),
            detail_for: None,
            facets: Loader::default(),
            facets_range: None,
            search_seen: String::new(),
            search_at: None,
            export: None,
            export_done: None,
            names_sig: 0,
        }
    }
}

impl Recent {
    /// The query for the current filters.
    pub fn query(&self, now: Timestamp, search: &str) -> RequestQuery {
        let search = search.trim();
        RequestQuery {
            source: self.source,
            from: Some(self.range.from(now)),
            to: None,
            member: self.member.clone(),
            line: None,
            model: self.model.clone(),
            harnesses: self.harnesses.clone(),
            host: self.host.clone(),
            protocol: None,
            status: self.status,
            client: self.client,
            group_by: self.grouping,
            group: self.group.clone(),
            search: (!search.is_empty()).then(|| search.to_owned()),
            after_id: None,
            limit: LIMIT,
            sort: self.sort,
        }
    }

    /// Any filter besides the range is set.
    pub fn filtered(&self, search: &str) -> bool {
        self.member.is_some()
            || self.model.is_some()
            || !self.harnesses.is_empty()
            || self.host.is_some()
            || self.status != RequestStatusFilter::All
            || self.client.is_some()
            || self.source.is_some()
            || self.group.is_some()
            || !search.trim().is_empty()
    }

    /// The source filter is shown: rows imported from another tool exist in
    /// the range (their providers are among the facets), or it is set.
    pub fn shows_sources(&self) -> bool {
        self.source.is_some()
            || self
                .facets
                .value
                .as_ref()
                .is_some_and(|f| f.members.iter().any(|b| b.key.starts_with("import:")))
    }

    /// Filters changed: reload and show the result at once.
    pub fn changed(&mut self) {
        self.page.invalidate();
        self.apply_next = true;
        self.scroll_top = true;
    }

    fn clear_filters(&mut self) {
        self.member = None;
        self.model = None;
        self.harnesses.clear();
        self.host = None;
        self.status = RequestStatusFilter::All;
        self.client = None;
        self.source = None;
        self.group = None;
        self.changed();
    }

    /// Show the requests of one session (the Overview's recent sessions): the
    /// other filters cleared, the source list grouped by session with `key`
    /// selected, at least the last seven days.
    pub fn show_session(&mut self, key: &str) {
        self.clear_filters();
        self.grouping = RequestGrouping::Session;
        self.group = Some(superba_core::harness_group_key(RequestHarness::Codex, key));
        if self.range == Range::Day {
            self.range = Range::Week;
        }
        self.selected = None;
    }

    /// Something is loading.
    pub fn loading(&self) -> bool {
        self.page.loading()
            || self.detail.loading()
            || self.facets.loading()
            || self.export.is_some()
    }

    /// A CSV export is running.
    pub fn exporting(&self) -> bool {
        self.export.is_some()
    }

    /// The history was deleted.
    pub fn after_clear(&mut self) {
        self.shown.clear();
        self.selected = None;
        self.detail.reset();
        self.detail_for = None;
        self.changed();
        self.facets.invalidate();
    }

    /// New session names arrived (the sidebar may re-sort; nothing to reload).
    pub fn titles_changed(&mut self) {}

    /// Index of the selected row in [`Recent::shown`].
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.shown.iter().position(|r| r.id == id)
    }

    /// Apply a finished query.
    pub fn apply(&mut self, page: &RequestPage) {
        self.totals = page.totals.clone();
        self.groups = page.groups.clone();
        self.truncated = page.truncated;
        if self.group.is_none() {
            self.all_count = page.totals.requests;
        }
        if self.apply_next || self.at_top || self.shown.is_empty() {
            self.shown = page.rows.clone();
            self.new_rows = 0;
            self.apply_next = false;
        } else {
            self.new_rows = requests::new_rows(&self.shown, &page.rows);
        }
    }

    /// Show the rows waiting behind the pill.
    pub fn take_new_rows(&mut self) {
        if let Some(p) = &self.page.value {
            self.shown = p.rows.clone();
        }
        self.new_rows = 0;
        self.scroll_top = true;
    }

    /// Write the shown rows as CSV into the Downloads folder (background).
    pub fn export(&mut self, cx: &Cx<'_>, titles: &BTreeMap<String, SessionTitle>) {
        if self.export.is_some() || self.shown.is_empty() {
            return;
        }
        let text = requests::csv(cx.tr, titles, &self.shown);
        let n = self.shown.len();
        let name = format!(
            "superba-requests-{}.csv",
            requests::full_time(Timestamp::now()).replace([' ', ':'], "-")
        );
        self.export = Some(crate::runtime::task(async move {
            let dir = downloads_dir()
                .ok_or_else(|| superba_core::Error::other("no home directory to export into"))?;
            let path = dir.join(name);
            std::fs::write(&path, text.as_bytes()).map_err(|e| {
                superba_core::Error::other(format!(
                    "{}: {e}",
                    superba_core::paths::display_path(&path)
                ))
            })?;
            Ok((path, n))
        }));
    }

    fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) {
        // Rows and providers show the current profile and line names. When
        // those change (the profiles arrive after the first rows, a provider is
        // renamed, the language changes) the rows are read again.
        let (set, sites) = (cx.data.profiles.value.as_ref(), &cx.data.sites);
        let sig = requests::ProviderNames::signature(cx.tr, set, sites);
        if sig != self.names_sig {
            self.names_sig = sig;
            self.changed();
            self.facets.invalidate();
        }
        let names = || requests::ProviderNames::new(cx.tr, set, sites);
        if self.page.poll()
            && let Some(p) = self.page.value.as_mut()
        {
            names().relabel_page(p, self.grouping == RequestGrouping::Provider);
            let p = p.clone();
            self.apply(&p);
        }
        self.detail.poll();
        if let Some(task) = &self.export
            && let Some(result) = task.poll()
        {
            self.export = None;
            self.export_done = Some(result);
        }
        if let Some(done) = self.export_done.take() {
            out.push(match done {
                Ok((path, n)) => Action::Toast {
                    status: Status::Ok,
                    text: cx.tr.f(
                        "requests.exported",
                        &[
                            ("n", &n),
                            ("path", &superba_core::paths::display_path(&path)),
                        ],
                    ),
                },
                Err(e) => Action::Toast {
                    status: Status::Error,
                    text: error_text(cx.tr, &e),
                },
            });
        }
    }
}

/// `~/Downloads` when it exists, else the home directory.
fn downloads_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)?;
    let d = home.join("Downloads");
    Some(if d.is_dir() { d } else { home })
}

/// Load what is due: the page (live every 2 s while routed), the filter menus,
/// the selected row's detail and the session names of what is shown.
pub(super) fn load(cx: &Cx<'_>, state: &mut State) {
    let now = cx.now;
    let live = mode(cx).is_router();
    let r = &mut state.recent;
    // Debounced search.
    if state.search != r.search_seen {
        let t = *r.search_at.get_or_insert_with(Instant::now);
        if t.elapsed() >= DEBOUNCE {
            r.search_seen = state.search.clone();
            r.search_at = None;
            r.changed();
        }
    }
    if r.page.due(Some(if live { LIVE } else { ROLLOUT_LIVE })) {
        let needle = r.search_seen.trim().to_lowercase();
        let keys = requests::keys_matching(state.titles.map(), &needle);
        let query = r.query(now, &r.search_seen);
        let service = cx.service.clone();
        r.page
            .start(async move { super::db::load_page(service, query, keys).await });
    }
    load_facets(cx, r, r.range);
    if r.selected != r.detail_for
        && let Some(id) = r.selected
        && !r.detail.loading()
    {
        r.detail_for = Some(id);
        r.detail.value = None;
        r.detail.error = None;
        let service = cx.service.clone();
        r.detail
            .start(async move { service.router_request(id).await });
    }
    let mut keys: Vec<&str> = r
        .shown
        .iter()
        .filter(|r| r.harness == RequestHarness::Codex)
        .filter_map(requests::session_key)
        .collect();
    if r.grouping == RequestGrouping::Session {
        keys.extend(
            r.groups
                .iter()
                .filter_map(|g| superba_core::split_harness_key(&g.key))
                .filter(|(h, _)| *h == RequestHarness::Codex)
                .map(|(_, id)| id),
        );
    }
    state.titles.want(cx.service, keys);
}

pub(super) fn load_facets(cx: &Cx<'_>, r: &mut Recent, range: Range) {
    r.facets.poll();
    if let Some(f) = r.facets.value.as_mut() {
        requests::ProviderNames::new(cx.tr, cx.data.profiles.value.as_ref(), &cx.data.sites)
            .relabel_buckets(&mut f.members);
    }
    let now = cx.now;
    if r.facets_range != Some(range) {
        r.facets.invalidate();
        r.facets_range = Some(range);
    }
    if r.facets.due(Some(Duration::from_secs(60))) {
        let service = cx.service.clone();
        let from = Some(range.from(now));
        r.facets.start(async move {
            let q = |by| StatsQuery {
                source: None,
                from,
                to: None,
                by,
                member: None,
                utc_offset_minutes: 0,
                ..StatsQuery::default()
            };
            let models = service.router_stats(q(StatsGroupBy::Model)).await?.buckets;
            let members = service
                .router_stats(q(StatsGroupBy::Provider))
                .await?
                .buckets;
            let hosts = service.router_stats(q(StatsGroupBy::Host)).await?.buckets;
            Ok(Facets {
                models,
                members,
                hosts,
            })
        });
    }
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.recent.poll(cx, out);
    load(cx, state);
    filters(ui, cx, state);
    ui.add_space(6.0);
    let rect = ui.available_rect_before_wrap();
    let side = if state.recent.collapsed {
        COLLAPSED_WIDTH
    } else if rect.width() >= 1100.0 {
        180.0
    } else {
        150.0
    };
    layout::columns(ui, &[Track::Fixed(side), Track::Fill], 12.0, |ui, i| {
        if i == 0 {
            sidebar(ui, cx, state);
        } else {
            main(ui, cx, state, out);
        }
    });
}

fn sidebar(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let titles = state.titles.map();
    let r = &mut state.recent;
    let entries = requests::source_entries(cx.tr, r.grouping, &r.groups, titles);
    let mut grouping = r.grouping;
    let resp = SourceList::new("router-sources", &mut r.group, &mut r.collapsed, &entries)
        .grouping(
            &mut grouping,
            vec![
                (RequestGrouping::Harness, cx.tr.t("requests.group.harness")),
                (
                    RequestGrouping::HarnessModel,
                    cx.tr.t("requests.group.harness_model"),
                ),
                (
                    RequestGrouping::Session,
                    cx.tr.t("requests.group.session_short"),
                ),
                (
                    RequestGrouping::Provider,
                    cx.tr.t("requests.group.provider_short"),
                ),
            ],
        )
        .grouping_label(cx.tr.t("requests.stats.group"))
        .all(cx.tr.t("requests.group.all"), r.all_count)
        .footnote(cx.tr.t("requests.remote_note"))
        .collapse_tips(
            cx.tr.t("requests.sources.collapse"),
            cx.tr.t("requests.sources.expand"),
        )
        .show(ui);
    if resp.grouping_changed {
        r.grouping = grouping;
        r.group = None;
    }
    if resp.selection_changed || resp.grouping_changed {
        r.changed();
    }
}

/// Gap between the rows of the log column.
const ROW_GAP: f32 = 6.0;
/// The log keeps at least its header, three rows and the totals row when the
/// detail pane grows.
const MIN_LOG: f32 =
    crate::design::tokens::size::TABLE_HEADER + 4.0 * crate::design::widgets::log_table::ROW_HEIGHT;

/// Height of the detail pane when the log and the pane share `room` points: what
/// the user dragged it to, else its content (title, tabs and the two rows of 通用
/// cards), always leaving the log [`MIN_LOG`].
fn detail_height(cx: &Cx<'_>, state: &State, room: f32) -> f32 {
    let r = &state.recent;
    let desired = r.detail_height.unwrap_or_else(|| {
        r.selected_index().map_or(detail_pane::MIN_HEIGHT, |i| {
            let row = &r.shown[i];
            let d = r.detail.value.as_ref().filter(|d| d.row.id == row.id);
            detail::pane_height(cx.tr, state.titles.map(), row, d)
        })
    });
    detail_pane::clamp_height(desired, room, MIN_LOG)
}

fn main(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let column = ui.available_rect_before_wrap();
    let show_detail = !state.recent.detail_hidden && state.recent.selected_index().is_some();
    let mut tracks = vec![Track::Fill, Track::Fixed(toolbar::HEIGHT)];
    // The log and the detail pane share what the fixed rows and the gaps leave.
    let fixed: f32 = tracks
        .iter()
        .map(|t| match t {
            Track::Fixed(h) => *h,
            Track::Fill => 0.0,
        })
        .sum();
    let room = column.height() - fixed - ROW_GAP * tracks.len() as f32;
    if show_detail {
        tracks.push(Track::Fixed(detail_height(cx, state, room)));
    }
    let pane_rect = layout::row_rects(column, &tracks, ROW_GAP)
        .get(2)
        .copied()
        .filter(|_| show_detail);
    layout::rows(ui, &tracks, ROW_GAP, |ui, i| match i {
        0 => table(ui, cx, state, out),
        1 => tools(ui, cx, state),
        _ => pane(ui, cx, state, out),
    });
    // The pane's top edge is the split: drag it to resize, double click for the
    // content height again.
    if let Some(pane) = pane_rect {
        let split =
            detail_pane::split_handle(ui, egui::Id::unique("router-detail-split"), pane, ROW_GAP);
        let r = &mut state.recent;
        if split.reset {
            r.detail_height = None;
        } else if split.delta != 0.0 {
            r.detail_height = Some(detail_pane::clamp_height(
                pane.height() - split.delta,
                room,
                MIN_LOG,
            ));
        }
    }
}

fn filters(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let r = &mut state.recent;
    ui.horizontal(|ui| {
        range_filter(ui, cx, r);
        extra_filters(ui, cx, r);
    });
    ui.add_space(FILTER_GAP);
    tool_filters(ui, cx, r);
}

/// Shared Usage filters. The whole page width is shared by four fields.
pub(super) fn tool_filters(ui: &mut Ui, cx: &Cx<'_>, r: &mut Recent) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let w = ((ui.available_width() - 18.0) / 4.0).floor();
        let label = if r.harnesses.is_empty() {
            cx.tr.t("requests.filter.all_tools")
        } else {
            r.harnesses
                .iter()
                .map(|h| cx.tr.text(&h.text()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut changed = false;
        Dropdown::new("usage-tools")
            .keep_open()
            .label(cx.tr.t("requests.col.harness"))
            .selected_text(label)
            .fixed_width(w)
            .menu_height(300.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(r.harnesses.is_empty(), cx.tr.t("requests.filter.all_tools"))
                    .clicked()
                {
                    r.harnesses.clear();
                    changed = true;
                }
                for h in RequestHarness::ALL {
                    let mut on = r.harnesses.contains(&h);
                    let changed_here = ui
                        .horizontal(|ui| {
                            let c = ui.checkbox(&mut on, cx.tr.text(&h.text())).changed();
                            if let Some(m) = crate::pages::shared::tool_label::monogram(h) {
                                let (r, _) = ui.allocate_exact_size(
                                    egui::vec2(
                                        crate::pages::shared::tool_label::TILE,
                                        crate::pages::shared::tool_label::TILE,
                                    ),
                                    egui::Sense::hover(),
                                );
                                crate::pages::shared::tool_label::paint(ui, m, r.center());
                            }
                            c
                        })
                        .inner;
                    if changed_here {
                        r.harnesses.retain(|v| *v != h);
                        if on {
                            r.harnesses.push(h);
                        }
                        r.harnesses.sort();
                        changed = true;
                    }
                }
            });
        if changed {
            r.changed();
        }
        provider_model_filters(ui, cx, r, w);
        let mut hosts = vec![
            (None, cx.tr.t("requests.filter.all_hosts")),
            (Some("local".to_owned()), cx.tr.t("requests.host.local")),
        ];
        if let Some(f) = &r.facets.value {
            hosts.extend(
                f.hosts
                    .iter()
                    .filter(|b| b.key != "local" && !b.key.is_empty())
                    .map(|b| (Some(b.key.clone()), requests::host_label(cx.tr, &b.key))),
            );
        }
        if let Some(h) = &r.host
            && !hosts.iter().any(|(v, _)| v.as_ref() == Some(h))
        {
            hosts.push((Some(h.clone()), requests::host_label(cx.tr, h)));
        }
        if Dropdown::new("usage-host")
            .label(cx.tr.t("requests.col.host"))
            .fixed_width(w)
            .show_options(ui, &mut r.host, &hosts)
            .changed()
        {
            r.changed();
        }
    });
}

/// The range segments, and "showing the newest n" on the right.
fn range_filter(ui: &mut Ui, cx: &Cx<'_>, r: &mut Recent) {
    ui.spacing_mut().item_spacing.x = 6.0;
    let mut range = r.range;
    let mut seg = Segmented::new("router-range", &mut range)
        .small()
        .label(cx.tr.t("requests.range.name"));
    for x in Range::ALL {
        seg = seg.option(x, cx.tr.t(x.key()));
    }
    if seg.show(ui).changed() {
        r.range = range;
        r.changed();
    }
    if r.truncated {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::Label::new(Text::caption(
                    cx.tr.f("requests.truncated", &[("n", &r.shown.len())]),
                    cx.pal,
                ))
                .truncate(),
            )
            .on_hover_text(cx.tr.t("requests.truncated_tip"));
        });
    }
}

/// Provider, model, status, client and (with imported rows) source
/// dropdowns, `widths` wide.
fn provider_model_filters(ui: &mut Ui, cx: &Cx<'_>, r: &mut Recent, width: f32) {
    ui.spacing_mut().item_spacing.x = 6.0;
    let facets = r.facets.value.clone().unwrap_or_default();
    let mut members: Vec<(Option<ProfileId>, String)> =
        vec![(None, cx.tr.t("requests.filter.all_providers"))];
    members.extend(
        facets
            .members
            .iter()
            .map(|b| (Some(ProfileId::new(&b.key)), b.label.clone())),
    );
    if let Some(m) = &r.member
        && !members.iter().any(|(v, _)| v.as_ref() == Some(m))
    {
        members.push((Some(m.clone()), m.to_string()));
    }
    if Dropdown::new("router-f-member")
        .label(cx.tr.t("requests.col.provider"))
        .fixed_width(width)
        .show_options(ui, &mut r.member, &members)
        .changed()
    {
        r.changed();
    }
    let mut models: Vec<(Option<String>, String)> =
        vec![(None, cx.tr.t("requests.filter.all_models"))];
    models.extend(
        facets
            .models
            .iter()
            .map(|b| (Some(b.key.clone()), b.key.clone())),
    );
    if let Some(m) = &r.model
        && !models.iter().any(|(v, _)| v.as_ref() == Some(m))
    {
        models.push((Some(m.clone()), m.clone()));
    }
    if Dropdown::new("router-f-model")
        .label(cx.tr.t("requests.col.model"))
        .fixed_width(width)
        .show_options(ui, &mut r.model, &models)
        .changed()
    {
        r.changed();
    }
}

fn extra_filters(ui: &mut Ui, cx: &Cx<'_>, r: &mut Recent) {
    let statuses = [
        (
            RequestStatusFilter::All,
            cx.tr.t("requests.filter.status.all"),
        ),
        (
            RequestStatusFilter::Ok,
            cx.tr.t("requests.filter.status.ok"),
        ),
        (
            RequestStatusFilter::Errors,
            cx.tr.t("requests.filter.status.failed"),
        ),
        (
            RequestStatusFilter::Failover,
            cx.tr.t("requests.filter.status.failover"),
        ),
    ];
    if Dropdown::new("router-f-status")
        .label(cx.tr.t("requests.col.status"))
        .width(100.0)
        .show_options(ui, &mut r.status, &statuses)
        .changed()
    {
        r.changed();
    }
    let clients = [
        (None, cx.tr.t("requests.filter.client.all")),
        (
            Some(RequestClient::Desktop),
            cx.tr.text(&RequestClient::Desktop.text()),
        ),
        (
            Some(RequestClient::Tui),
            cx.tr.text(&RequestClient::Tui.text()),
        ),
        (
            Some(RequestClient::Exec),
            cx.tr.text(&RequestClient::Exec.text()),
        ),
    ];
    if Dropdown::new("router-f-client")
        .label(cx.tr.t("requests.col.client"))
        .width(108.0)
        .show_options(ui, &mut r.client, &clients)
        .changed()
    {
        r.changed();
    }
    if r.shows_sources() {
        let mut sources: Vec<(Option<RequestSource>, String)> =
            vec![(None, cx.tr.t("requests.filter.source.all"))];
        sources.extend(
            RequestSource::ALL
                .iter()
                .map(|s| (Some(*s), cx.tr.text(&s.text()))),
        );
        if Dropdown::new("router-f-source")
            .label(cx.tr.t("requests.filter.source.name"))
            .width(140.0)
            .show_options(ui, &mut r.source, &sources)
            .changed()
        {
            r.changed();
        }
    }
}

fn table(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let columns = requests::columns(cx.tr);
    let titles = state.titles.map();
    let r = &mut state.recent;
    let sort_col = Col::of_sort_key(r.sort.key)
        .and_then(|c| Col::ALL.iter().position(|x| *x == c))
        .unwrap_or(1);
    let selected = r.selected_index();
    let pill = (r.new_rows > 0).then(|| cx.tr.f("requests.new_rows", &[("n", &r.new_rows)]));
    let mut totals = requests::totals_cells(cx.tr, &r.totals, &r.shown);
    // A narrow table hides 缓存键, where 合计 sits.
    requests::fold_hidden_totals(
        &mut totals,
        &log_table::column_widths(&columns, ui.available_width()),
    );
    let reveal = std::mem::take(&mut r.reveal).then_some(selected).flatten();
    let shown = &r.shown;
    let resp = LogTable::new("router-requests", &columns, shown.len())
        .label(cx.tr.t("usage.tab.recent"))
        .row_name(|row| requests::row_name(cx.tr, titles, &shown[row], cx.now))
        .selected(selected)
        .sort(sort_col, r.sort.descending)
        .totals(totals)
        .pill(pill)
        .scroll_to_top(std::mem::take(&mut r.scroll_top))
        .reveal(reveal)
        .show(ui, |row, col| {
            requests::cell(cx.tr, titles, &shown[row], Col::ALL[col], cx.now)
        });
    r.at_top = resp.at_top;
    if let Some(row) = resp.clicked.or(resp.key_selected) {
        r.selected = r.shown.get(row).map(|x| x.id);
        if resp.key_selected.is_some() {
            r.reveal = true;
        }
    }
    if resp.activated.is_some() {
        r.detail_hidden = false;
    }
    if resp.pill_clicked {
        r.take_new_rows();
    }
    if let Some(c) = resp.sort_clicked {
        let key = Col::ALL[c].sort_key();
        if r.sort.key == key {
            r.sort.descending = !r.sort.descending;
        } else {
            r.sort = RequestSort {
                key,
                descending: true,
            };
        }
        r.changed();
    }
    if r.shown.is_empty() {
        let body = resp.body_rect.shrink(16.0);
        let filtered = r.filtered(&state.search);
        let configured = cx
            .data
            .profiles
            .value
            .as_ref()
            .is_some_and(|p| !p.settings.router.members.is_empty());
        layout::fixed(ui, body, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((body.height() / 2.0 - 40.0).max(0.0));
                if let Some(e) = &r.page.error {
                    ui.label(Text::small(error_text(cx.tr, e), cx.pal).color(cx.pal.red));
                } else if r.page.value.is_none() {
                    ui.label(Text::small(cx.tr.t("requests.loading"), cx.pal));
                } else if filtered {
                    ui.label(Text::small(cx.tr.t("requests.empty.filtered"), cx.pal));
                    if ui
                        .add(Button::ghost(cx.tr.t("requests.empty.clear_filters")).small())
                        .clicked()
                    {
                        state.search.clear();
                        r.clear_filters();
                    }
                } else if !mode(cx).is_router() && !configured {
                    // No request in Codex's session files either.
                    ui.add(
                        egui::Label::new(Text::small(cx.tr.t("requests.empty.off"), cx.pal)).wrap(),
                    );
                    if ui
                        .add(Button::ghost(cx.tr.t("requests.empty.off_action")).small())
                        .clicked()
                    {
                        out.push(Action::Go(crate::pages::Page::Router));
                    }
                } else {
                    ui.label(Text::small(cx.tr.t("requests.empty.none"), cx.pal));
                }
            });
        });
    }
}

/// What a toolbar click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolAct {
    Clear,
    Reload,
    Record,
    Detail,
}

fn tools(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let stats = state.stats_settings(cx);
    let saving = state.saving_stats();
    let has_selection = state.recent.selected_index().is_some();
    let hidden = state.recent.detail_hidden;
    // Clearing and pausing act on the router's records: nothing to do without
    // a router (Codex's own session files are never changed).
    let router = cx
        .data
        .profiles
        .value
        .as_ref()
        .is_some_and(|p| !p.settings.router.members.is_empty());
    let (left, right) = Toolbar::show(
        ui,
        |ui| {
            let mut act = None;
            if tool_button(
                ui,
                Some(Icon::Close),
                &cx.tr.t("requests.toolbar.clear"),
                router,
            )
            .on_hover_text(cx.tr.t("requests.toolbar.clear_tip"))
            .on_disabled_hover_text(cx.tr.t("requests.tip.router_only"))
            .clicked()
            {
                act = Some(ToolAct::Clear);
            }
            if tool_button(
                ui,
                Some(Icon::Refresh),
                &cx.tr.t("requests.toolbar.reload"),
                true,
            )
            .clicked()
            {
                act = Some(ToolAct::Reload);
            }
            let (key, icon) = if stats.record {
                ("requests.toolbar.pause", Icon::Pause)
            } else {
                ("requests.toolbar.resume", Icon::Play)
            };
            if tool_button(ui, Some(icon), &cx.tr.t(key), router && !saving)
                .on_hover_text(cx.tr.t("requests.toolbar.pause_tip"))
                .on_disabled_hover_text(cx.tr.t("requests.tip.router_only"))
                .clicked()
            {
                act = Some(ToolAct::Record);
            }
            act
        },
        |ui| {
            let mut act = None;
            if has_selection {
                let (key, icon) = if hidden {
                    ("requests.toolbar.expand", Icon::ChevronRight)
                } else {
                    ("requests.toolbar.collapse", Icon::ChevronDown)
                };
                if tool_button(ui, Some(icon), &cx.tr.t(key), true).clicked() {
                    act = Some(ToolAct::Detail);
                }
            }
            if router && !stats.record {
                ui.label(Text::caption(cx.tr.t("requests.paused"), cx.pal).color(cx.pal.amber));
            }
            act
        },
    );
    match left.or(right) {
        Some(ToolAct::Clear) => {
            // Only the router's rows are deleted (Codex's session files stay).
            let r = &state.recent;
            let rows = if r.truncated {
                r.totals.requests
            } else {
                r.shown
                    .iter()
                    .filter(|x| x.source == superba_core::RequestSource::Router)
                    .count() as u64
            };
            state.dialog = Some(Dialog::Clear { rows });
        }
        Some(ToolAct::Reload) => {
            state.recent.changed();
            state.recent.facets.invalidate();
        }
        Some(ToolAct::Record) => {
            let mut s = stats;
            s.record = !s.record;
            state.save_stats(cx, s);
        }
        Some(ToolAct::Detail) => state.recent.detail_hidden = !state.recent.detail_hidden,
        None => {}
    }
}

fn pane(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(index) = state.recent.selected_index() else {
        return;
    };
    let row = state.recent.shown[index].clone();
    let titles = state.titles.map().clone();
    let r = &mut state.recent;
    let mut detail = r
        .detail
        .value
        .as_ref()
        .filter(|d| d.row.id == row.id)
        .cloned();
    // The attempts' lines by their current names, in the UI language.
    if let Some(d) = &mut detail {
        let names =
            requests::ProviderNames::new(cx.tr, cx.data.profiles.value.as_ref(), &cx.data.sites);
        for a in &mut d.attempts {
            if let Some(line) = &a.line_name {
                a.line_name = Some(names.line(a.member_id.as_str(), line));
            }
        }
    }
    let (status, _) = requests::status_cell(cx.tr, &row);
    let status_text = requests::status_detail(cx.tr, &row);
    let status_line = match row.http_status {
        Some(code) if !matches!(row.status, superba_core::RequestStatus::Http { .. }) => {
            format!("{status_text} · HTTP {code}")
        }
        _ => status_text,
    };
    let title = detail::title_line(cx, &titles, &row);
    let tabs = detail::TABS.iter().map(|k| cx.tr.t(k)).collect();
    DetailPane::new("router-detail", &mut r.detail_tab)
        .tabs(tabs)
        .title(title)
        .status(status, status_line)
        .show(ui, |ui, tab| {
            detail::body(
                ui,
                cx,
                &titles,
                &row,
                detail.as_ref(),
                r.detail.loading(),
                tab,
                out,
            )
        });
}
