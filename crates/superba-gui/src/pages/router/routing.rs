//! 路由 / Routing (Usage page, user bug 116): how each request through the
//! local router went. One entry per request, newest first: when, which tool
//! and model, the outcome, the member and line that answered, and every hop
//! before it (multi-level failover: member A line 1 → line 2 → member B), each
//! with its status and time; the protocol conversion (the tool's protocol →
//! the upstream's) and the upstream fallback. Above the list: what the router
//! answered itself since it started (ChatGPT backend side calls, `x-superba-
//! route: local`; counted by the router, never logged per call).
//!
//! Sources: the router's request log rows (`router_requests`, source = router)
//! with their attempts (`RequestRow::attempts`, each with the upstream
//! protocol since this build); older rows without attempts show the answering
//! line from the row itself. Local answers come from the live admin status.
//! Filters: all, failed over, failed; the page's search (Esc or 清除 clears).

use std::time::Duration;

use egui::{Align, Layout, Ui};
use superba_core::{
    RequestAttempt, RequestPage, RequestProtocol, RequestQuery, RequestRow, RequestSource,
    RequestStatusFilter,
};

use super::db::Loader;
use super::requests::{self, Range};
use super::{State, mode};
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{space, text};
use crate::design::widgets::{Badge, Segmented, Status, StatusDot, Text};
use crate::pages::{Action, Cx, divider};

/// Rows read at most.
const LIMIT: u32 = 300;
/// Where the hops start: past the status dot (8 pt and its margin) and the
/// gap after it.
const INDENT: f32 = 10.0 + space::S;
/// Reload period while the router is on.
const LIVE: Duration = Duration::from_secs(5);

/// Which requests are listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Filter {
    /// Every routed request.
    #[default]
    All,
    /// Requests that failed over at least once.
    Failover,
    /// Requests that did not end well.
    Failed,
}

impl Filter {
    fn status(self) -> RequestStatusFilter {
        match self {
            Filter::All => RequestStatusFilter::All,
            Filter::Failover => RequestStatusFilter::Failover,
            Filter::Failed => RequestStatusFilter::Errors,
        }
    }
}

/// State of the tab.
#[derive(Debug, Default)]
pub struct Routing {
    /// Listed requests.
    pub filter: Filter,
    /// Time range.
    pub range: Range,
    /// The rows.
    pub page: Loader<RequestPage>,
    /// Filter, range and search of the rows shown (reload when they change).
    shown_for: Option<(Filter, Range, String)>,
}

impl Routing {
    /// A read is running.
    pub fn loading(&self) -> bool {
        self.page.loading()
    }
}

/// One hop of a request: a line tried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    /// Member (provider) name.
    pub member: String,
    /// Line name.
    pub line: Option<String>,
    /// HTTP status, when headers arrived.
    pub status: Option<u16>,
    /// What happened (`answered`, `connect error`, `HTTP 502`).
    pub outcome: String,
    /// Time spent.
    pub elapsed_ms: Option<u64>,
    /// This line answered.
    pub answered: bool,
    /// Protocol sent upstream, when recorded.
    pub protocol: Option<RequestProtocol>,
}

/// The hops of a routed request in order, the answering one last. A row
/// recorded before attempts were kept on it yields the answering line alone.
pub fn hops(row: &RequestRow, name_of: &dyn Fn(&str) -> Option<String>) -> Vec<Hop> {
    let member = |a: &RequestAttempt| {
        if a.member_id == row.member_id && !row.member_name.is_empty() {
            row.member_name.clone()
        } else {
            name_of(a.member_id.as_str()).unwrap_or_else(|| a.member_id.as_str().to_owned())
        }
    };
    if row.attempts.is_empty() {
        return vec![Hop {
            member: if row.member_name.is_empty() {
                row.member_id.as_str().to_owned()
            } else {
                row.member_name.clone()
            },
            line: row.line_name.clone().or_else(|| row.line_id.clone()),
            status: row.http_status,
            outcome: String::new(),
            elapsed_ms: row.ttfb_ms,
            answered: row.http_status.is_some(),
            protocol: None,
        }];
    }
    row.attempts
        .iter()
        .map(|a| Hop {
            member: member(a),
            line: a.line_name.clone(),
            status: a.status,
            outcome: a.outcome.clone(),
            elapsed_ms: a.elapsed_ms,
            answered: a.outcome == "answered",
            protocol: a.upstream_protocol,
        })
        .collect()
}

/// The request changed member at least once (a fallback to another provider,
/// not only another line of the same one).
pub fn changed_member(row: &RequestRow) -> bool {
    row.attempts.iter().any(|a| a.member_id != row.member_id)
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, _out: &mut Vec<Action>) {
    load(cx, state);
    toolbar(ui, cx, state);
    local_answers(ui, cx, state);
    ui.add_space(space::XS);
    let rows: Vec<RequestRow> = state
        .routing
        .page
        .value
        .as_ref()
        .map(|p| p.rows.clone())
        .unwrap_or_default();
    super::settings::scroll_column(ui, "usage-routing", |ui| {
        ui.spacing_mut().item_spacing.y = space::XS;
        if rows.is_empty() {
            let key = if state.routing.page.value.is_none() {
                "usage.routing.loading"
            } else if state.routing.filter != Filter::All || !state.search.trim().is_empty() {
                "usage.routing.none_matching"
            } else {
                "usage.routing.empty"
            };
            ui.add_space(space::L);
            ui.add(egui::Label::new(Text::small(cx.tr.t(key), cx.pal)).wrap());
            return;
        }
        let names = provider_names(cx);
        for row in &rows {
            ui.push_id(row.id, |ui| entry(ui, cx, row, &names));
            divider(ui, cx);
        }
    });
}

/// Provider names by id, for hops on another member than the answering one.
fn provider_names(cx: &Cx<'_>) -> Vec<(String, String)> {
    cx.data
        .profiles
        .value
        .as_ref()
        .map(|p| {
            p.profiles
                .iter()
                .map(|p| (p.id.as_str().to_owned(), p.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn load(cx: &Cx<'_>, state: &mut State) {
    let r = &mut state.routing;
    r.page.poll();
    let search = state.search.trim().to_owned();
    let key = (r.filter, r.range, search.clone());
    if r.shown_for.as_ref() != Some(&key) {
        r.shown_for = Some(key);
        r.page.invalidate();
    }
    let live = mode(cx).is_router();
    if r.page.due(live.then_some(LIVE)) {
        let query = RequestQuery {
            source: Some(RequestSource::Router),
            from: Some(r.range.from(cx.now)),
            status: r.filter.status(),
            search: (!search.is_empty()).then_some(search),
            limit: LIMIT,
            ..RequestQuery::default()
        };
        let service = cx.service.clone();
        r.page
            .start(async move { service.router_requests(query).await });
    }
}

/// The filter, the range and how many requests are listed.
fn toolbar(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        let r = &mut state.routing;
        let mut filter = r.filter;
        if Segmented::new("usage-routing-filter", &mut filter)
            .small()
            .label(cx.tr.t("usage.routing.filter_label"))
            .option(Filter::All, cx.tr.t("usage.routing.filter.all"))
            .option(Filter::Failover, cx.tr.t("usage.routing.filter.failover"))
            .option(Filter::Failed, cx.tr.t("usage.routing.filter.failed"))
            .show(ui)
            .changed()
        {
            r.filter = filter;
        }
        let mut range = r.range;
        let mut seg = Segmented::new("usage-routing-range", &mut range)
            .small()
            .label(cx.tr.t("requests.range.name"));
        for x in Range::ALL {
            seg = seg.option(x, cx.tr.t(x.key()));
        }
        if seg.show(ui).changed() {
            r.range = range;
        }
        if let Some(page) = &r.page.value {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let failovers = page.rows.iter().filter(|r| r.failovers > 0).count();
                let converted = page
                    .rows
                    .iter()
                    .filter(|r| r.converted_to().is_some())
                    .count();
                ui.label(Text::caption(
                    cx.tr.f(
                        "usage.routing.count",
                        &[
                            ("n", &page.rows.len()),
                            ("failovers", &failovers),
                            ("converted", &converted),
                        ],
                    ),
                    cx.pal,
                ));
            });
        }
    });
}

/// What the router answered itself since it started, by route.
fn local_answers(ui: &mut Ui, cx: &Cx<'_>, state: &State) {
    let Some(live) = state.snapshot.as_ref().and_then(|s| s.live.as_ref().ok()) else {
        return;
    };
    if live.local_answers.is_empty() {
        return;
    }
    let total: u64 = live.local_answers.values().sum();
    let mut routes: Vec<(&String, &u64)> = live.local_answers.iter().collect();
    routes.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let top: Vec<String> = routes
        .iter()
        .take(4)
        .map(|(route, n)| format!("{route} {n}"))
        .collect();
    let all: Vec<String> = routes
        .iter()
        .map(|(route, n)| format!("{route}: {n}"))
        .collect();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        ui.add(Badge::new(cx.tr.t("usage.routing.local")).outline());
        ui.label(
            Text::small(
                cx.tr.f(
                    "usage.routing.local_count",
                    &[("n", &total), ("routes", &top.join(" · "))],
                ),
                cx.pal,
            )
            .color(cx.pal.text),
        )
        .on_hover_text(format!(
            "{}\n{}",
            cx.tr.t("usage.routing.local_tip"),
            all.join("\n")
        ));
    });
}

/// One request: a header line and its hops.
fn entry(ui: &mut Ui, cx: &Cx<'_>, row: &RequestRow, names: &[(String, String)]) {
    let name_of = |id: &str| names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone());
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        // Each fact is one unit: the line wraps between them.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        let (status, text) = requests::status_cell(cx.tr, row);
        ui.add(StatusDot::new(status));
        ui.label(
            Text::small(requests::date_cell(row.started_at, cx.now), cx.pal)
                .color(cx.pal.text_muted),
        );
        crate::pages::shared::tool_label::tool_label(
            ui,
            crate::pages::shared::tool_label::monogram(row.harness),
            Text::small(cx.tr.text(&row.client_text()), cx.pal).color(cx.pal.text),
        );
        ui.label(Text::small(requests::model_text(row), cx.pal).color(cx.pal.text));
        ui.add(Badge::new(text).status(status));
        if let Some(to) = row.converted_to() {
            ui.add(
                Badge::new(cx.tr.f(
                    "usage.routing.converted",
                    &[
                        ("from", &cx.tr.text(&row.protocol.text())),
                        ("to", &cx.tr.text(&to.text())),
                    ],
                ))
                .outline(),
            );
        }
        if row.failovers > 0 {
            let key = if changed_member(row) {
                "usage.routing.fallback"
            } else {
                "usage.routing.failovers"
            };
            ui.add(Badge::new(cx.tr.f(key, &[("n", &row.failovers)])).status(Status::Warn));
        }
        if let Some(ms) = row.duration_ms {
            ui.label(Text::small(requests::seconds(Some(ms)), cx.pal));
        }
    });
    // The hops: member · line, status and time, joined by arrows.
    let hops = hops(row, &name_of);
    let mut job = egui::text::LayoutJob::default();
    let small = |w| font(text::SMALL, w);
    for (i, h) in hops.iter().enumerate() {
        if i > 0 {
            job.append(
                "  →  ",
                0.0,
                egui::TextFormat::simple(small(Weight::Regular), cx.pal.text_subtle),
            );
        }
        let place = match &h.line {
            Some(line) => format!("{} · {line}", h.member),
            None => h.member.clone(),
        };
        job.append(
            &place,
            0.0,
            egui::TextFormat::simple(
                small(Weight::Medium),
                if h.answered {
                    cx.pal.text
                } else {
                    cx.pal.text_muted
                },
            ),
        );
        let mut facts = Vec::new();
        match (h.status, h.answered) {
            (Some(s), _) => facts.push(s.to_string()),
            (None, false) if !h.outcome.is_empty() => facts.push(h.outcome.clone()),
            _ => {}
        }
        if let Some(ms) = h.elapsed_ms {
            facts.push(format!("{ms} ms"));
        }
        if let Some(p) = h.protocol.filter(|p| *p != row.protocol) {
            facts.push(cx.tr.text(&p.text()));
        }
        if !facts.is_empty() {
            let color = if h.answered { cx.pal.green } else { cx.pal.red };
            job.append(
                &facts.join(" · "),
                space::XS,
                egui::TextFormat::simple(small(Weight::Regular), color),
            );
        }
    }
    // Indented under the status dot, also on the lines a narrow window wraps.
    indented(ui, |ui| {
        ui.add(egui::Label::new(job).wrap());
    });
    if let Some(reason) = requests::status_reason(cx.tr, row).filter(|_| row.status.is_error()) {
        indented(ui, |ui| {
            ui.add(egui::Label::new(Text::caption(reason, cx.pal)).wrap());
        });
    }
    ui.add_space(space::XS);
}

/// `add` in a column starting at [`INDENT`], wrapped text included.
fn indented(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: INDENT as i8,
            ..egui::Margin::ZERO
        })
        .show(ui, add);
}
