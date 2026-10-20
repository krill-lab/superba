//! Router member order, expandable line details, service status and events.

use egui::{Align, Layout, Sense, Stroke, Ui, vec2};

use super::model::{self, LineHealth, Live, MemberKind, MemberView, Mode};
use super::{Op, State, mode};
use crate::design::icons::Icon;
use crate::design::widgets::{
    Badge, Button, Dropdown, FormRow, IconButton, Status, StatusDot, Text, link_button,
};
use crate::pages::{Cx, error_text, kv, note};

/// A row of controls in the value column of the `kv` rows above it (the
/// service's buttons, the live counters), wrapping when narrow. The label
/// column is `kv`'s (user bug 25: a hard-coded 128 px indent put 安装并启动
/// 6 px left of the value column).
fn value_row(ui: &mut Ui, id: &str, gap: f32, add: impl FnOnce(&mut Ui)) {
    let label_w = (ui.available_width() * 0.3).clamp(90.0, 128.0);
    FormRow::default()
        .label_width(label_w)
        .gap(6.0)
        .height(24.0)
        .wrap()
        .id_salt(id)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            add(ui);
        });
}

fn divider(ui: &mut Ui, cx: &Cx<'_>) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, cx.pal.border));
}

fn heading(ui: &mut Ui, cx: &Cx<'_>, key: &str) {
    ui.add_space(14.0);
    ui.label(Text::title(cx.tr.t(key), cx.pal));
    ui.add_space(2.0);
}

/// Errors remain visible above the settings, even while Advanced is closed.
pub(super) fn notices(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    if state.backend.is_none() && state.backend_checked {
        note(ui, cx, Status::Warn, cx.tr.t("router.unavailable"));
        return;
    }
    if let Some(error) = state.op_error.clone() {
        note(ui, cx, Status::Error, error);
        // The concrete reason (service manager, log, port) is one click away.
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            if link_button(ui, &cx.tr.t("router.doctor.why"), false).clicked() {
                super::doctor::open(state, cx);
            }
        });
    }
    let Some(snapshot) = state.snapshot.clone() else {
        match &state.error {
            Some(e) => note(ui, cx, Status::Error, error_text(cx.tr, e)),
            None => {
                ui.label(Text::muted(cx.tr.t("router.loading"), cx.pal));
            }
        }
        return;
    };

    if let Ok(live) = &snapshot.live {
        local_shortage(ui, cx, &live.local);
    } else if mode(cx).is_router() {
        note(ui, cx, Status::Error, cx.tr.t("router.live.down_on"));
    }
    if let Some(path) = &snapshot.network_exe {
        note(
            ui,
            cx,
            Status::Warn,
            cx.tr.f("router.service.network_exe", &[("path", path)]),
        );
    }
}

/// The most frequent task comes first, with the member order always visible.
pub(super) fn members_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(snapshot) = state.snapshot.clone() else {
        ui.label(Text::section(cx.tr.t("router.members.title"), cx.pal));
        return;
    };
    members(ui, cx, state, &snapshot);
    if let Some(claude) = &snapshot.claude {
        claude_members(ui, cx, claude);
    }
}

/// Maintenance controls keep their names and behavior in Advanced.
pub(super) fn service_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    ui.label(Text::section(cx.tr.t("router.service_label"), cx.pal));
    if let Some(snapshot) = state.snapshot.clone() {
        service_and_status(ui, cx, state, &snapshot, &mode(cx));
        if let Ok(live) = &snapshot.live
            && !live.events.is_empty()
        {
            events(ui, cx, state, live, &snapshot);
        }
    }
}

fn service_and_status(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    snapshot: &model::Snapshot,
    mode: &Mode,
) {
    let busy = state.op.as_ref().map(|(op, _)| *op);
    // Service.
    kv(
        ui,
        cx,
        &cx.tr.t("router.service_label"),
        |ui| match &snapshot.service {
            Some(s) => {
                let (status, key) = match (s.installed, s.running) {
                    (false, _) => (Status::Idle, "router.service.not_installed"),
                    (true, Some(true)) => (Status::Ok, "router.service.running"),
                    (true, Some(false)) => (Status::Error, "router.service.stopped"),
                    (true, None) => (Status::Warn, "router.service.installed"),
                };
                ui.add(Badge::new(cx.tr.t(key)).status(status));
                let mut tip = vec![s.backend.clone()];
                if let Some(pid) = s.pid {
                    tip.push(format!("pid {pid}"));
                }
                if let Some(detail) = &s.detail {
                    tip.push(detail.clone());
                }
                ui.label(Text::mono_muted(&s.backend, cx.pal))
                    .on_hover_text(tip.join(" · "));
            }
            None => {
                ui.label(Text::small(cx.tr.t("router.service.none"), cx.pal));
            }
        },
    );
    if let Some(s) = &snapshot.service {
        value_row(ui, "router-service-actions", 4.0, |ui| {
            let label = if s.installed {
                "router.service.reinstall"
            } else {
                "router.service.install"
            };
            if ui
                .add(
                    Button::ghost(cx.tr.t(label))
                        .small()
                        .loading(busy == Some(Op::Install))
                        .enabled(busy.is_none() && !snapshot.members.is_empty()),
                )
                .on_hover_text(cx.tr.t("router.service.install_hint"))
                // egui shows no hover text on disabled widgets.
                .on_disabled_hover_text(cx.tr.t("router.needs_member"))
                .clicked()
            {
                state.start(cx, Op::Install, |b| b.install());
            }
            if s.installed
                && ui
                    .add(
                        Button::ghost(cx.tr.t("router.service.restart"))
                            .small()
                            .loading(busy == Some(Op::Restart))
                            .enabled(busy.is_none()),
                    )
                    .clicked()
            {
                state.start(cx, Op::Restart, |b| b.restart());
            }
            if ui
                .add(
                    Button::ghost(cx.tr.t("router.doctor.open"))
                        .icon(Icon::Diagnostics)
                        .small()
                        .loading(state.doctor.loading()),
                )
                .on_hover_text(cx.tr.t("router.doctor.hint"))
                .clicked()
            {
                super::doctor::open(state, cx);
            }
            if s.installed
                && ui
                    .add(
                        Button::ghost(cx.tr.t("router.service.uninstall"))
                            .small()
                            .loading(busy == Some(Op::Uninstall))
                            .enabled(busy.is_none() && !mode.is_router()),
                    )
                    .on_hover_text(cx.tr.t("router.service.uninstall_hint"))
                    .on_disabled_hover_text(cx.tr.t("router.service.uninstall_hint"))
                    .clicked()
            {
                state.start(cx, Op::Uninstall, |b| b.uninstall());
            }
        });
    }
    // Process.
    kv(ui, cx, &cx.tr.t("router.process"), |ui| {
        match &snapshot.live {
            Ok(live) => {
                ui.add(Badge::new(cx.tr.t("router.live.up")).status(Status::Ok));
                ui.label(Text::mono_muted(&live.listen, cx.pal));
                ui.label(Text::mono_muted(format!("v{}", live.version), cx.pal));
            }
            Err(reason) => {
                let status = if mode.is_router() {
                    Status::Error
                } else {
                    Status::Idle
                };
                ui.add(Badge::new(cx.tr.t("router.live.down")).status(status))
                    .on_hover_text(reason);
            }
        }
    });
    if let Ok(live) = &snapshot.live {
        value_row(ui, "router-live-stats", 6.0, |ui| {
            stat(
                ui,
                cx,
                "router.live.uptime",
                model::duration_text(live.uptime_ms),
            );
            stat(
                ui,
                cx,
                "router.live.requests",
                model::count_text(live.totals.requests),
            );
            stat(
                ui,
                cx,
                "router.live.active",
                model::count_text(live.totals.active_requests),
            );
            stat(
                ui,
                cx,
                "router.live.failovers",
                model::count_text(live.totals.failovers),
            );
            if live.totals.unavailable > 0 {
                stat(
                    ui,
                    cx,
                    "router.live.unavailable",
                    model::count_text(live.totals.unavailable),
                );
            }
            stat(
                ui,
                cx,
                "router.live.sessions",
                live.totals.sticky_sessions.to_string(),
            );
            if live.local.max_concurrent > 0 {
                stat(
                    ui,
                    cx,
                    "router.live.capacity",
                    format!("{} / {}", live.local.in_flight, live.local.max_concurrent),
                );
            }
        });
    }
}

/// A recent local shortage: requests got 503 `local_router_busy` because this
/// computer ran out of open files or router places; no line was blamed.
fn local_shortage(ui: &mut Ui, cx: &Cx<'_>, local: &superba_core::RouterLocalView) {
    let now_ms = u64::try_from(superba_core::Timestamp::now().as_millis()).unwrap_or(0);
    if !model::recent_shortage(local, now_ms) {
        return;
    }
    let ago = model::duration_text(now_ms.saturating_sub(local.last_problem_ms.unwrap_or(now_ms)));
    let limit = local
        .open_files_limit
        .map_or_else(|| cx.tr.t("router.local.unlimited"), |n| n.to_string());
    let text = cx.tr.f(
        "router.local.ran_short",
        &[
            ("time", &ago),
            ("files", &local.file_limit_errors),
            ("busy", &local.busy),
            ("limit", &limit),
        ],
    );
    note(ui, cx, Status::Warn, text);
    if let Some(last) = &local.last_file_limit {
        ui.label(Text::mono_muted(last, cx.pal));
    }
}

fn stat(ui: &mut Ui, cx: &Cx<'_>, key: &str, value: String) {
    ui.label(Text::caption(cx.tr.t(key), cx.pal));
    ui.label(Text::mono(value, cx.pal));
}

fn kind_text(cx: &Cx<'_>, kind: MemberKind) -> String {
    cx.tr.t(match kind {
        MemberKind::Krill => "router.kind.krill",
        MemberKind::Compatible => "router.kind.compatible",
    })
}

fn members(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, snapshot: &model::Snapshot) {
    ui.spacing_mut().item_spacing.y = 4.0;
    ui.label(Text::section(cx.tr.t("router.members.title"), cx.pal));
    ui.add(egui::Label::new(Text::small(cx.tr.t("router.members.order_hint"), cx.pal)).wrap());
    egui::CollapsingHeader::new(cx.tr.t("router.members.rules"))
        .id_salt("router-member-rules")
        .show(ui, |ui| {
            ui.add(egui::Label::new(Text::small(cx.tr.t("router.members.help"), cx.pal)).wrap());
            ui.add(
                egui::Label::new(Text::small(cx.tr.t("router.preference.recovery"), cx.pal)).wrap(),
            );
        });
    divider(ui, cx);
    let busy = state.op.is_some();
    let n = snapshot.members.len();
    if n == 0 {
        ui.add_space(10.0);
        ui.label(Text::muted(cx.tr.t("router.members.empty"), cx.pal));
        ui.add_space(10.0);
        divider(ui, cx);
    }
    for (i, m) in snapshot.members.iter().enumerate() {
        ui.add_space(4.0);
        ui.push_id(&m.id, |ui| member_row(ui, cx, state, m, i, n, busy));
        ui.add_space(4.0);
        divider(ui, cx);
    }
    // Add a member.
    if !snapshot.candidates.is_empty() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let selected = state
                .pick
                .and_then(|i| snapshot.candidates.get(i))
                .map(|c| candidate_label(cx, c))
                .unwrap_or_else(|| cx.tr.t("router.members.pick"));
            Dropdown::new("router-add-member")
                .label(cx.tr.t("router.members.candidate"))
                .selected_text(selected)
                .fixed_width((ui.available_width() - 130.0).clamp(120.0, 320.0))
                .show_ui(ui, |ui| {
                    for (i, c) in snapshot.candidates.iter().enumerate() {
                        ui.selectable_value(&mut state.pick, Some(i), candidate_label(cx, c));
                    }
                });
            let candidate = state.pick.and_then(|i| snapshot.candidates.get(i)).cloned();
            if ui
                .add(
                    Button::ghost(cx.tr.t("router.members.add"))
                        .icon(Icon::Plus)
                        .small()
                        .loading(state.op.as_ref().is_some_and(|(op, _)| *op == Op::Add))
                        .enabled(!busy && candidate.is_some()),
                )
                .clicked()
                && let Some(c) = candidate
            {
                state.pick = None;
                state.add(cx, c);
            }
        });
        ui.label(Text::caption(cx.tr.t("router.members.key_note"), cx.pal));
    }
    if !snapshot.keyless.is_empty() {
        ui.add_space(4.0);
        ui.label(Text::caption(
            cx.tr.f(
                "router.members.keyless",
                &[("sites", &snapshot.keyless.join(", "))],
            ),
            cx.pal,
        ));
    }
}

/// "Work · Krill", or just "Krill" when the profile is named after its kind.
/// Claude Code's providers on the router (its own first), each with its
/// Anthropic endpoint or why it cannot serve Claude Code.
fn claude_members(ui: &mut Ui, cx: &Cx<'_>, claude: &superba_core::RouterClaudeView) {
    ui.add_space(12.0);
    heading(ui, cx, "router.claude.title");
    ui.add(egui::Label::new(Text::small(cx.tr.t("router.claude.help"), cx.pal)).wrap());
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let (status, key) = if claude.routed {
            (Status::Ok, "router.claude.routed")
        } else {
            (Status::Warn, "router.claude.pending")
        };
        ui.add(Badge::new(cx.tr.t(key)).status(status));
        if let Some(url) = &claude.base_url {
            ui.add(egui::Label::new(Text::mono_muted(url.clone(), cx.pal)).truncate());
        }
    });
    divider(ui, cx);
    for m in &claude.members {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let ok = m.ineligible.is_none();
            ui.add(StatusDot::new(if ok { Status::Ok } else { Status::Idle }));
            ui.label(Text::strong(&m.name, cx.pal));
            if m.preferred {
                ui.add(Badge::new(cx.tr.t("router.claude.preferred")).outline());
            }
            if m.claude_only {
                ui.add(Badge::new(cx.tr.t("router.claude.claude_only")).outline());
            }
        });
        let detail = if m.ineligible.is_none() && !m.translated {
            Text::mono_muted(m.endpoint.clone().unwrap_or_default(), cx.pal)
        } else {
            Text::caption(cx.tr.text(&m.connection_text()), cx.pal)
        };
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            ui.add(egui::Label::new(detail).wrap());
        });
        ui.add_space(6.0);
        divider(ui, cx);
    }
}

fn candidate_label(cx: &Cx<'_>, c: &model::Candidate) -> String {
    let kind = kind_text(cx, c.kind);
    if c.name.trim().eq_ignore_ascii_case(kind.trim()) {
        c.name.clone()
    } else {
        format!("{} · {kind}", c.name)
    }
}

fn member_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    m: &MemberView,
    index: usize,
    count: usize,
    busy: bool,
) {
    // Reserve the action column before laying out names or translated badges.
    let label_w = (ui.available_width() - 112.0).max(0.0);
    FormRow::default().label_width(label_w).right().show_with(
        ui,
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(Text::mono_muted(format!("{}", index + 1), cx.pal));
                let healthy = m.healthy_lines();
                let status = if m.lines.iter().all(|l| l.health == LineHealth::Unknown) {
                    Status::Idle
                } else if healthy == m.lines.len() {
                    Status::Ok
                } else if healthy > 0 {
                    Status::Warn
                } else {
                    Status::Error
                };
                ui.add(StatusDot::new(status));
                ui.add(egui::Label::new(Text::strong(&m.name, cx.pal)).wrap());
                let priority = if index == 0 {
                    cx.tr.t("router.members.primary")
                } else {
                    cx.tr.f("router.members.fallback", &[("n", &index)])
                };
                ui.add(Badge::new(priority).outline());
                if !m.has_key {
                    ui.add(Badge::new(cx.tr.t("router.members.no_key")).status(Status::Error));
                }
            });
        },
        |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        IconButton::new(Icon::Close, cx.tr.t("router.members.remove"))
                            .enabled(!busy),
                    )
                    .clicked()
                {
                    state.remove(cx, m.id.clone());
                }
                if ui
                    .add(
                        IconButton::new(Icon::ArrowDown, cx.tr.t("router.members.down"))
                            .enabled(!busy && index + 1 < count),
                    )
                    .clicked()
                {
                    state.move_member(cx, index, false);
                }
                if ui
                    .add(
                        IconButton::new(Icon::ArrowUp, cx.tr.t("router.members.up"))
                            .enabled(!busy && index > 0),
                    )
                    .clicked()
                {
                    state.move_member(cx, index, true);
                }
            });
        },
    );
    let line_name = |id: &str| {
        let site = cx
            .data
            .profiles
            .value
            .as_ref()
            .and_then(|p| p.profile(&superba_core::ProfileId::new(&m.id)))
            .and_then(|p| cx.data.site(&p.site_id));
        site.and_then(|s| {
            s.line(&superba_core::LineId::new(id))
                .map(|l| cx.tr.text(&l.name))
        })
        .or_else(|| m.lines.iter().find(|l| l.id == id).map(|l| l.name.clone()))
        .unwrap_or_else(|| id.to_owned())
    };
    ui.add(
        egui::Label::new(Text::strong(
            crate::pages::shared::priority_text(cx.tr, m.lines.iter().map(|l| line_name(&l.id))),
            cx.pal,
        ))
        .wrap(),
    );
    if let Some(route) = &m.last_route {
        ui.add(
            egui::Label::new(Text::small(
                crate::pages::shared::route_text(cx.tr, route, line_name),
                cx.pal,
            ))
            .wrap(),
        )
        .on_hover_text(cx.tr.t("router.preference.observed"));
    }
    egui::CollapsingHeader::new(cx.tr.t("router.members.details"))
        .id_salt("router-member-details")
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(Badge::new(kind_text(cx, m.kind)).outline());
                if !m.has_key {
                    ui.add(Badge::new(cx.tr.t("router.members.no_key")).status(Status::Error));
                }
                if m.websocket {
                    ui.add(Badge::new("WebSocket").outline());
                }
                if m.model_mappings > 0 {
                    ui.add(Badge::new(
                        cx.tr
                            .f("router.members.mappings", &[("n", &m.model_mappings)]),
                    ));
                }
            });
            for route in &m.protocols {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(26.0);
                    ui.label(Text::caption(route.ingress(), cx.pal));
                    ui.label(Text::caption(cx.tr.text(&route.connection_text()), cx.pal));
                });
            }
            for line in &m.lines {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_space(26.0);
                    let (status, key) = match line.health {
                        LineHealth::Up => (Status::Ok, "router.line.up"),
                        LineHealth::Probing => (Status::Warn, "router.line.probing"),
                        LineHealth::Down => (Status::Error, "router.line.down"),
                        LineHealth::Unknown => (Status::Idle, "router.line.unknown"),
                    };
                    ui.add(StatusDot::new(status));
                    ui.label(Text::small(line_name(&line.id), cx.pal).color(cx.pal.text));
                    ui.label(Text::mono_muted(&line.base_url, cx.pal));
                    let mut parts = vec![cx.tr.t(key)];
                    if let Some(metrics) = &line.metrics {
                        if let Some(ms) = metrics.reopens_in_ms
                            && line.health == LineHealth::Down
                        {
                            parts.push(cx.tr.f(
                                "router.line.retry_in",
                                &[("time", &model::duration_text(ms))],
                            ));
                        }
                        if let Some(ttfb) = metrics.ttfb_ms {
                            parts.push(format!("{ttfb} ms"));
                        }
                        parts.push(cx.tr.f(
                            "router.line.requests",
                            &[("n", &model::count_text(metrics.requests))],
                        ));
                        if metrics.failovers_away > 0 {
                            parts.push(cx.tr.f(
                                "router.line.failovers",
                                &[("n", &model::count_text(metrics.failovers_away))],
                            ));
                        }
                        if metrics.active_websockets > 0 {
                            parts.push(cx.tr.f(
                                "router.line.websockets",
                                &[("n", &metrics.active_websockets)],
                            ));
                        }
                    }
                    ui.label(Text::caption(parts.join(" · "), cx.pal));
                    if let Some(err) = line.metrics.as_ref().and_then(|m| m.last_error.as_ref()) {
                        ui.label(Text::caption(err, cx.pal).color(cx.pal.amber))
                            .on_hover_text(cx.tr.t("router.line.last_error"));
                    }
                });
            }
        });
}

fn events(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, live: &Live, snapshot: &model::Snapshot) {
    let _ = snapshot;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(Text::title(cx.tr.t("router.events.title"), cx.pal));
        let key = if state.show_events {
            "router.events.hide"
        } else {
            "router.events.show"
        };
        if link_button(ui, &cx.tr.t(key), false).clicked() {
            state.show_events = !state.show_events;
        }
    });
    if !state.show_events {
        return;
    }
    divider(ui, cx);
    let now_ms = superba_core::Timestamp::now().as_millis() as u64;
    for e in live.events.iter().rev().take(30) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add(StatusDot::new(if e.severe {
                Status::Warn
            } else {
                Status::Idle
            }));
            ui.label(Text::mono_muted(
                cx.tr.f(
                    "router.events.ago",
                    &[(
                        "time",
                        &model::duration_text(now_ms.saturating_sub(e.at_ms)),
                    )],
                ),
                cx.pal,
            ));
            ui.label(Text::small(cx.tr.t(e.kind_key), cx.pal).color(cx.pal.text));
            let target: Vec<&str> = [e.member.as_deref(), e.line.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            if !target.is_empty() {
                ui.label(Text::small(target.join(" / "), cx.pal));
            }
            if let Some(d) = &e.detail {
                ui.label(Text::caption(d, cx.pal));
            }
        });
    }
}
