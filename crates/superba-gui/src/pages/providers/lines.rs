//! The lines of a preset provider on the Connection tab (UX §3.3, user bug
//! 105): rank, name and host, latency, WebSocket / zstd support
//! ([`super::transports`]), auth availability, which line is used now, the
//! row action and the order arrows. What the controls do depends on how Codex
//! reaches the provider, and the section title and caption say which:
//!
//! - **Direct** (no local router): Codex talks to exactly one line, the
//!   applied one, marked 使用中 / In use. 切换到此线路 / Use this line asks for
//!   the same switch as the Overview's line popover
//!   ([`crate::pages::shared::switch_action`]): the hot-switch countdown and
//!   confirmation follow. Moving another line to #1 of the provider in use is
//!   that same switch ([`crate::pages::shared::reorder_action`]); other moves
//!   save the order (what a switch to the provider tries first, and the
//!   router's failover order). For a provider not in use the row action
//!   switches to the provider on that line.
//! - **Router**: the order is the router's failover order. #1 is 首选 /
//!   Preferred; 设为首选 / Set as preferred and the arrows save the profile,
//!   and a running router reloads a member's order at once (rline). The latest
//!   observed upstream (当前：…, with the lines that failed before it) comes
//!   from `router_status`, read again every [`ROUTER_RELOAD`].
//!
//! Each row of a provider with several lines has a switch that enables or
//! disables the line (user bug 108). A disabled line keeps its place in the
//! order, is muted and says 已禁用 / Disabled instead of a latency and the row
//! action; the router never selects it and a switch never uses it. The last
//! enabled line cannot be disabled. Disabling the line Codex uses in direct
//! mode asks first ([`disable_dialog`]), then switches to the first enabled
//! line through the usual switch.

use std::time::{Duration, Instant};

use egui::{Align, Layout, Sense, Stroke, Ui, vec2};
use superba_core::{LineId, ProfileId, RouterLineRoute, RouterView};

use super::State;
use crate::design::a11y;
use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::{size, text};
use crate::design::widgets::{
    Badge, Button, IconButton, Latency, LatencyBadge, Status, StatusDot, Text, Toggle, dialog,
    dialog_body,
};
use crate::pages::shared::providers::Provider;
use crate::pages::shared::{
    LineToggle, applied_line, disable_and_switch, last_enabled, ordered_lines, reorder_action,
    route_text, switch_action, toggle_action,
};
use crate::pages::{Action, Cx, divider, elided, error_text, note, pat_status};
use crate::runtime::ReadTask;

/// Height of a line row on one line (wide) and on two (narrow).
const LINE_H: f32 = 46.0;
const LINE_H_NARROW: f32 = 56.0;
/// Detail width from which a line row keeps everything on one line: rank,
/// name, latency, the transport badges, the auth note, the row action and the
/// arrows (measured at 1280×800; 960×640 gets the two-line row).
const WIDE: f32 = 640.0;
/// Width of the latency badge.
const LATENCY_W: f32 = 80.0;
/// Side of an order arrow.
const ARROW: f32 = 24.0;
/// How often the router's latest upstream is read while it is shown.
const ROUTER_RELOAD: Duration = Duration::from_secs(10);

/// How Codex reaches the provider whose lines are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// No local router: Codex talks to one line (`applied`: of this provider).
    Direct { applied: bool },
    /// Router mode (`member`: the router serves this provider, so a change of
    /// the order reaches it at once).
    Router { member: bool },
}

impl Mode {
    fn of(cx: &Cx<'_>, p: &Provider) -> Self {
        if cx.data.router_enabled() {
            let member = p.profile_id().is_some_and(|id| {
                cx.data
                    .profiles
                    .value
                    .as_ref()
                    .is_some_and(|s| s.settings.router.members.contains(id))
            });
            Mode::Router { member }
        } else {
            Mode::Direct { applied: p.applied }
        }
    }

    fn title_key(self) -> &'static str {
        match self {
            Mode::Direct { .. } => "providers.lines.title_direct",
            Mode::Router { .. } => "providers.lines.title_router",
        }
    }

    fn caption_key(self) -> &'static str {
        match self {
            Mode::Direct { applied: true } => "providers.lines.caption_direct_applied",
            Mode::Direct { applied: false } => "providers.lines.caption_direct",
            Mode::Router { member: true } => "providers.lines.caption_router",
            Mode::Router { member: false } => "providers.lines.caption_router_idle",
        }
    }
}

/// The router's status while router-mode lines are shown (page state; the
/// Overview keeps its own copy in its snapshot).
#[derive(Default)]
pub(crate) struct RouterLive {
    task: Option<ReadTask<RouterView>>,
    view: Option<RouterView>,
    at: Option<Instant>,
}

impl RouterLive {
    /// Forget the last read: the next frame reads `router_status` again
    /// (router mode changed outside this window; alpha.9 D3).
    pub(crate) fn invalidate(&mut self) {
        self.view = None;
        self.at = None;
    }

    /// Poll a running read and start one when none ran for [`ROUTER_RELOAD`].
    pub(super) fn tick(&mut self, ui: &Ui, cx: &Cx<'_>) {
        if let Some(task) = &self.task {
            match task.poll() {
                Some(result) => {
                    self.task = None;
                    // A failed read keeps the last view; the next one retries.
                    if let Ok(view) = result {
                        self.view = Some(view);
                    }
                }
                None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
            }
        }
        if self.task.is_none() && self.at.is_none_or(|t| t.elapsed() >= ROUTER_RELOAD) {
            let service = cx.service.clone();
            self.task = Some(crate::runtime::task(async move {
                service.router_status().await
            }));
            self.at = Some(Instant::now());
        }
        ui.ctx().request_repaint_after(ROUTER_RELOAD);
    }

    /// The latest upstream the router used for `profile`.
    pub(super) fn route(&self, profile: &ProfileId) -> Option<&RouterLineRoute> {
        self.view
            .as_ref()?
            .members
            .iter()
            .find(|m| &m.profile_id == profile)?
            .last_route
            .as_ref()
    }
}

/// What the right end of a line row shows.
enum Slot {
    /// A state: in use (direct), preferred (router), disabled; `name` for
    /// screen readers.
    Badge {
        text: String,
        name: String,
        status: Status,
    },
    /// The row action ([`switch_action`] to the row's line).
    Action {
        text: String,
        name: String,
        tip: String,
        enabled: bool,
    },
    /// Nothing (a single line).
    Empty,
}

/// Lines in their order with latency, auth availability, the line in use, the
/// row action and the order arrows; per-line notices and the transport footer.
pub(super) fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    p: &Provider,
    out: &mut Vec<Action>,
) {
    let site = &p.site;
    let profile = p.profile.as_ref();
    let lines = ordered_lines(site, profile);
    let many = lines.len() > 1;
    let probing = cx.data.probing_site(&site.id);
    let mode = Mode::of(cx, p);
    let applied = profile.and_then(|pr| applied_line(cx.data, pr));
    let route = match (mode, profile) {
        (Mode::Router { member: true }, Some(pr)) => {
            state.router.tick(ui, cx);
            state.router.route(&pr.id).cloned()
        }
        _ => None,
    };
    header(ui, cx, p, mode, route.as_ref(), probing, out);
    if let Some(e) = cx.data.caps.get(&site.id).and_then(|l| l.error.as_ref()) {
        note(ui, cx, Status::Error, error_text(cx.tr, e));
    }
    let ids: Vec<LineId> = lines.iter().map(|l| l.id.clone()).collect();
    let n = lines.len();
    let wide = ui.available_width() >= WIDE;
    let disabled: Vec<bool> = lines
        .iter()
        .map(|l| profile.is_some_and(|pr| pr.is_line_disabled(&l.id)))
        .collect();
    // The router's preferred line: the first enabled one.
    let preferred = disabled.iter().position(|d| !d);
    let slots: Vec<Slot> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            slot(
                cx,
                p,
                mode,
                many,
                (disabled[i], preferred == Some(i)),
                line,
                applied.as_ref(),
            )
        })
        .collect();
    let slot_w = slots.iter().map(|s| slot_width(ui, s)).fold(0.0, f32::max);
    let mut reorder = None;
    let mut toggled: Option<(LineId, bool)> = None;
    for (i, (line, slot)) in lines.iter().zip(slots).enumerate() {
        let off = disabled[i];
        if i > 0 {
            divider(ui, cx);
        }
        let caps = cx.data.site_caps(&site.id, &line.id);
        let latency = match caps {
            None if probing => Latency::Probing,
            None => Latency::Unknown,
            Some(c) if !c.reachable => Latency::Unreachable,
            Some(c) => c.latency_ms.map_or(Latency::Unknown, Latency::Ms),
        };
        // The router's latest upstream is marked on its host line (in amber
        // when it is not the preferred line: a failover).
        let current = route
            .as_ref()
            .is_some_and(|r| r.line_id == line.id.as_str());
        let failover = current && Some(i) != preferred;
        let host = if current {
            format!(
                "{} · {}",
                line.origin.host(),
                cx.tr.t("connection.lines.current")
            )
        } else {
            line.origin.host().to_owned()
        };
        let name = cx.tr.text(&line.name);
        let mut arrows = |ui: &mut Ui| {
            if !many {
                return;
            }
            let can = profile.is_some() && !cx.busy;
            if ui
                .add(
                    IconButton::new(Icon::ArrowDown, cx.tr.t("connection.lines.down"))
                        .size(ARROW)
                        .enabled(can && i + 1 < n),
                )
                .clicked()
            {
                reorder = Some((i, i + 1));
            }
            if ui
                .add(
                    IconButton::new(Icon::ArrowUp, cx.tr.t("connection.lines.up"))
                        .size(ARROW)
                        .enabled(can && i > 0),
                )
                .clicked()
            {
                reorder = Some((i, i - 1));
            }
        };
        let mut chosen = false;
        let mut right = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            arrows(ui);
            ui.add_space(6.0);
            chosen |= slot_ui(ui, cx, slot_w, &slot);
            if many && let Some(pr) = profile {
                ui.add_space(8.0);
                if let Some(disable) = enable_switch(ui, cx, pr, site, line, &name, off) {
                    toggled = Some((line.id.clone(), disable));
                }
            }
        };
        let name_text = if off {
            Text::strong(name.clone(), cx.pal).color(cx.pal.text_muted)
        } else {
            Text::strong(name.clone(), cx.pal)
        };
        let latency_ui = |ui: &mut Ui| {
            if off {
                // A disabled line is not probed for Codex: no latency.
                ui.add_space(LATENCY_W);
            } else {
                ui.add_sized(
                    [LATENCY_W, 22.0],
                    LatencyBadge::new(latency)
                        .name(crate::pages::overview::latency_text(cx, latency)),
                );
            }
        };
        if wide {
            ui.horizontal(|ui| {
                ui.set_min_height(LINE_H);
                ui.spacing_mut().item_spacing.x = 8.0;
                rank(ui, cx, i, off);
                let name_w = (ui.available_width() * 0.26).clamp(110.0, 200.0);
                ui.allocate_ui_with_layout(
                    vec2(name_w, 40.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(name_w);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        elided(ui, name_text.clone());
                        host_text(ui, cx, &host, current, failover);
                    },
                );
                latency_ui(ui);
                super::transports::badges(ui, cx, p, &line.id);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    right(ui);
                    ui.add_space(6.0);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        super::connection::availability(ui, cx, state, p, line, caps);
                    });
                });
            });
        } else {
            // Narrow (960×640): name, latency, action and arrows on the first
            // line; host, transports and the auth note under the name.
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), LINE_H_NARROW),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_min_height(LINE_H_NARROW);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.set_min_height(size::CONTROL_SM);
                        ui.spacing_mut().item_spacing.x = 8.0;
                        rank(ui, cx, i, off);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            right(ui);
                            ui.add_space(6.0);
                            latency_ui(ui);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                elided(ui, name_text.clone());
                            });
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        // Under the name: past the rank disc and its gap.
                        ui.add_space(22.0 + 8.0 - 6.0);
                        host_text(ui, cx, &host, current, failover);
                        super::transports::badges(ui, cx, p, &line.id);
                        super::connection::availability(ui, cx, state, p, line, caps);
                    });
                },
            );
        }
        if chosen && let Some(profile) = profile {
            out.push(switch_action(cx.data, profile, &line.id));
        }
    }
    if let Some((line, disable)) = toggled
        && let Some(profile) = profile
    {
        match toggle_action(cx.data, profile, site, &line, disable) {
            Some(LineToggle::Save(action)) => out.push(action),
            Some(LineToggle::Confirm { .. }) => {
                state.disable_line = Some((profile.id.clone(), line));
            }
            None => {}
        }
    }
    if let Some((from, to)) = reorder
        && let Some(profile) = profile
        && let Some(action) = reorder_action(cx.data, profile, ids, from, to)
    {
        out.push(action);
    }
    notices(ui, cx, p, &lines);
    super::transports::footer(ui, cx, p);
}

/// Title (the mode), the caption (what the controls do), the router's latest
/// upstream and Refresh.
fn header(
    ui: &mut Ui,
    cx: &Cx<'_>,
    p: &Provider,
    mode: Mode,
    route: Option<&RouterLineRoute>,
    probing: bool,
    out: &mut Vec<Action>,
) {
    let site = &p.site;
    ui.horizontal(|ui| {
        ui.set_min_height(size::CONTROL_SM);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("action.probe"))
                        .icon(Icon::Refresh)
                        .small()
                        .loading(probing)
                        .enabled(!probing),
                )
                .clicked()
            {
                out.push(Action::Probe(site.id.clone()));
                super::transports::refresh(ui, cx, p);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.label(Text::section(cx.tr.t(mode.title_key()), cx.pal));
                if mode == (Mode::Router { member: true }) {
                    current_route(ui, cx, site, route);
                }
            });
        });
    });
    let caption = cx.tr.t(mode.caption_key());
    let hint = match mode {
        Mode::Direct { .. } => cx.tr.t("providers.lines.order_hint"),
        Mode::Router { .. } => format!(
            "{}\n{}",
            crate::pages::router::recovery_text(cx.tr, cx.data.router_failover.as_ref()),
            cx.tr.t("router.preference.observed")
        ),
    };
    ui.add(egui::Label::new(Text::small(caption, cx.pal)).wrap())
        .on_hover_text(hint);
    ui.add_space(2.0);
}

/// 当前：海外线路（国内极速失败后切换）, or that the router waits for a request.
fn current_route(
    ui: &mut Ui,
    cx: &Cx<'_>,
    site: &superba_core::Site,
    route: Option<&RouterLineRoute>,
) {
    let (status, text) = match route {
        Some(r) => (
            if r.failed_lines.is_empty() {
                Status::Ok
            } else {
                Status::Warn
            },
            route_text(cx.tr, r, |id| {
                site.line(&LineId::new(id))
                    .map(|l| cx.tr.text(&l.name))
                    .unwrap_or_else(|| id.to_owned())
            }),
        ),
        None => (Status::Idle, cx.tr.t("router.preference.waiting")),
    };
    ui.spacing_mut().item_spacing.x = 6.0;
    ui.add(StatusDot::new(status));
    elided(ui, Text::small(text, cx.pal).color(cx.pal.text))
        .on_hover_text(cx.tr.t("router.preference.observed"));
}

/// The host under a line's name; with the router's latest upstream marked.
fn host_text(ui: &mut Ui, cx: &Cx<'_>, host: &str, current: bool, failover: bool) {
    let t = Text::mono_muted(host, cx.pal);
    let t = if failover { t.color(cx.pal.amber) } else { t };
    let r = elided(ui, t);
    if current {
        r.on_hover_text(cx.tr.t("router.preference.observed"));
    }
}

/// What line `i` of `p` shows at the right end.
fn slot(
    cx: &Cx<'_>,
    p: &Provider,
    mode: Mode,
    many: bool,
    (disabled, preferred): (bool, bool),
    line: &superba_core::Line,
    applied: Option<&LineId>,
) -> Slot {
    let name = cx.tr.text(&line.name);
    let named = |key: &str| cx.tr.f(key, &[("line", &name)]);
    let profile = p.profile.as_ref();
    match mode {
        _ if disabled => Slot::Badge {
            text: cx.tr.t("connection.lines.disabled"),
            name: named("connection.lines.disabled_named"),
            status: Status::Idle,
        },
        Mode::Direct { .. } if applied == Some(&line.id) => Slot::Badge {
            text: cx.tr.t("connection.lines.in_use"),
            name: named("connection.lines.in_use_named"),
            status: Status::Ok,
        },
        // One line: the provider's own switch is in the header.
        _ if !many => Slot::Empty,
        Mode::Direct { .. } => {
            // A switch needs the key (the header's switch says the same).
            let keyed = profile.is_some_and(|pr| pr.key_ref.is_some());
            Slot::Action {
                text: cx.tr.t("connection.lines.use"),
                name: named("connection.lines.use_named"),
                tip: cx.tr.t(if keyed {
                    "connection.lines.use_tip"
                } else {
                    "providers.use_needs_key"
                }),
                enabled: keyed,
            }
        }
        Mode::Router { .. } if preferred => Slot::Badge {
            text: cx.tr.t("connection.lines.preferred"),
            name: named("connection.lines.preferred_named"),
            status: Status::Ok,
        },
        Mode::Router { .. } => Slot::Action {
            text: cx.tr.t("connection.lines.prefer"),
            name: named("connection.lines.prefer_named"),
            tip: cx.tr.t("router.preference.choose"),
            enabled: profile.is_some(),
        },
    }
}

/// Width a slot takes (badge or button).
fn slot_width(ui: &Ui, slot: &Slot) -> f32 {
    match slot {
        Slot::Badge { text, .. } => {
            let g = ui.painter().layout_no_wrap(
                text.clone(),
                font(text::CAPTION, Weight::Medium),
                egui::Color32::PLACEHOLDER,
            );
            // Badge: text, 8 px padding each side and the status dot.
            g.size().x + 16.0 + 12.0
        }
        Slot::Action { text, .. } => Button::new(text.clone()).small().desired_size(ui).x,
        Slot::Empty => 0.0,
    }
}

/// Draw `slot`, right-aligned in `width`; true when its action was clicked.
fn slot_ui(ui: &mut Ui, cx: &Cx<'_>, width: f32, slot: &Slot) -> bool {
    if width <= 0.0 {
        return false;
    }
    ui.allocate_ui_with_layout(
        vec2(width, size::CONTROL_SM),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.set_width(width);
            match slot {
                Slot::Badge { text, name, status } => {
                    let r = ui.add(Badge::new(text.clone()).status(*status));
                    a11y::describe(&r, egui::accesskit::Role::Label, name);
                    false
                }
                Slot::Action {
                    text,
                    name,
                    tip,
                    enabled,
                } => {
                    let r = ui.add(
                        Button::new(text.clone())
                            .small()
                            .enabled(*enabled && !cx.busy),
                    );
                    a11y::describe(&r, egui::accesskit::Role::Button, name);
                    r.on_hover_text(tip).on_disabled_hover_text(tip).clicked()
                }
                Slot::Empty => false,
            }
        },
    )
    .inner
}

/// Per-line notices (edge interception, PAT unavailable), one line each.
fn notices(ui: &mut Ui, cx: &Cx<'_>, p: &Provider, lines: &[superba_core::Line]) {
    let site = &p.site;
    let primary = lines.first().map(|l| l.id.clone());
    for caps in lines
        .iter()
        .filter_map(|l| cx.data.site_caps(&site.id, &l.id))
        .filter(|c| c.detail.as_ref().is_some_and(|d| d.key != "probe.ok"))
        .filter(|c| {
            Some(&c.line_id) != primary.as_ref()
                || !c.detail.as_ref().is_some_and(|d| is_pat_detail(&d.key))
        })
    {
        let name = site
            .line(&caps.line_id)
            .map(|l| cx.tr.text(&l.name))
            .unwrap_or_else(|| caps.line_id.to_string());
        let detail = caps
            .detail
            .as_ref()
            .map(|d| cx.localize(d))
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(pat_status(caps)));
            let text = cx.tr.f(
                "providers.lines.note",
                &[("line", &name), ("detail", &detail)],
            );
            ui.add(egui::Label::new(Text::small(text.clone(), cx.pal)).truncate())
                .on_hover_text(text);
        });
    }
}

/// Probe details that only say why PAT is unavailable on a line.
fn is_pat_detail(key: &str) -> bool {
    matches!(
        key,
        "probe.pat_edge_blocked" | "probe.pat_unavailable" | "probe.pat_needs_https"
    )
}

/// The switch that enables or disables `line` (on = enabled); named per line
/// for screen readers. `Some(disable)` when it was flipped. The last enabled
/// line's switch is locked on.
fn enable_switch(
    ui: &mut Ui,
    cx: &Cx<'_>,
    profile: &superba_core::Profile,
    site: &superba_core::Site,
    line: &superba_core::Line,
    name: &str,
    off: bool,
) -> Option<bool> {
    let last = last_enabled(profile, site, &line.id);
    let mut on = !off;
    let r = ui.add(
        Toggle::new(
            &mut on,
            cx.tr.f("connection.lines.enable_named", &[("line", &name)]),
        )
        .enabled(!cx.busy && !last),
    );
    let tip = cx.tr.t(if last {
        "connection.lines.disable_last"
    } else if off {
        "connection.lines.enable_tip"
    } else {
        "connection.lines.disable_tip"
    });
    let r = r.on_hover_text(&tip).on_disabled_hover_text(&tip);
    (r.changed() && on == off).then_some(!off)
}

/// The confirmation to disable the line Codex uses (direct mode): Disable and
/// switch saves the profile and asks for the usual switch to the first enabled
/// line.
pub(super) fn disable_dialog(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    state: &mut State,
    out: &mut Vec<Action>,
) {
    let Some((profile_id, line_id)) = state.disable_line.clone() else {
        return;
    };
    let found = cx
        .data
        .profiles
        .value
        .as_ref()
        .and_then(|s| s.profile(&profile_id).cloned())
        .and_then(|p| cx.data.site(&p.site_id).map(|s| (p, s)));
    let Some((profile, site)) = found else {
        state.disable_line = None;
        return;
    };
    let mut after = profile.clone();
    let next = after
        .set_line_disabled(&site, &line_id, true)
        .ok()
        .and_then(|()| after.enabled_line_ids(&site).into_iter().next());
    let (Some(line), Some(next)) = (site.line(&line_id), next.and_then(|n| site.line(&n))) else {
        state.disable_line = None;
        return;
    };
    let line_name = cx.tr.text(&line.name);
    let next_name = cx.tr.text(&next.name);
    let args: [(&str, &dyn std::fmt::Display); 2] = [("line", &line_name), ("next", &next_name)];
    let mut close = false;
    let response = dialog(
        ctx,
        egui::Id::unique("providers-disable-line"),
        480.0,
        |ui| {
            ui.label(Text::h2(
                cx.tr.f("connection.lines.disable_active_title", &args),
                cx.pal,
            ));
            dialog_body(ui, |ui| {
                ui.add(
                    egui::Label::new(Text::muted(
                        cx.tr.f("connection.lines.disable_active_body", &args),
                        cx.pal,
                    ))
                    .wrap(),
                );
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        Button::primary(cx.tr.f("connection.lines.disable_active_confirm", &args))
                            .enabled(!cx.busy),
                    )
                    .clicked()
                {
                    out.extend(disable_and_switch(&profile, &site, &line_id));
                    close = true;
                }
                if ui
                    .add(Button::ghost(cx.tr.t("action.cancel")).kbd("Esc"))
                    .clicked()
                {
                    close = true;
                }
            });
        },
    );
    if close || response.should_close() {
        state.disable_line = None;
    }
}

/// The line's place in the order (1 = tried first); muted when disabled.
fn rank(ui: &mut Ui, cx: &Cx<'_>, i: usize, off: bool) {
    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let first = i == 0 && !off;
    ui.painter().circle(
        r.center(),
        10.0,
        if first {
            cx.pal.primary
        } else {
            cx.pal.surface
        },
        Stroke::new(
            1.0,
            if first {
                cx.pal.primary
            } else {
                cx.pal.border_strong
            },
        ),
    );
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        (i + 1).to_string(),
        font(text::CAPTION, Weight::Semibold),
        if first {
            cx.pal.on_primary
        } else {
            cx.pal.text_muted
        },
    );
}
