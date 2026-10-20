//! The router's members in priority order with their line order and line
//! details, the fixes when no member can carry traffic, Claude Code's members,
//! and (in Advanced) the service, the router process and its recent events.

use egui::{Align, Layout, Ui};

use super::model::{self, LineHealth, Live, MemberKind, MemberView, Mode};
use super::{Op, State, mode};
use crate::design::elide::Shape;
use crate::design::icons::Icon;
use crate::design::tokens::{size, space};
use crate::design::widgets::value_chip::ValueChip;
use crate::design::widgets::{
    Badge, Button, Disclosure, Dropdown, FormRow, IconButton, Status, StatusDot, Table, Text,
    link_button,
};
use crate::pages::{Action, Cx, divider, error_text, kv, note, section, section_with};

/// A row of controls in the value column of the `kv` rows above it (the
/// service's buttons, the live counters), wrapping when narrow. The label
/// column is `kv`'s (user bug 25: a hard-coded 128 px indent put 安装并启动
/// 6 px left of the value column).
fn value_row(ui: &mut Ui, id: &str, gap: f32, add: impl FnOnce(&mut Ui)) {
    let label_w = (ui.available_width() * 0.3).clamp(90.0, 128.0);
    FormRow::default()
        .label_width(label_w)
        .gap(6.0)
        .height(size::CONTROL_SM)
        .wrap()
        .id_salt(id)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            add(ui);
        });
}

/// Width of `text` in `font`, for table columns sized to their content.
fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, egui::Color32::PLACEHOLDER)
        .size()
        .x
}

/// Errors remain visible above the settings, even while Advanced is closed.
pub(super) fn notices(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    if state.backend.is_none() && state.backend_checked {
        ui.add_space(space::S);
        note(ui, cx, Status::Warn, cx.tr.t("router.unavailable"));
        return;
    }
    if let Some(error) = state.op_error.clone() {
        ui.add_space(space::S);
        note(ui, cx, Status::Error, error);
        // The concrete reason (service manager, log, port) is one click away.
        ui.horizontal(|ui| {
            ui.add_space(space::L);
            if link_button(ui, &cx.tr.t("router.doctor.why"), false).clicked() {
                super::doctor::open(state, cx);
            }
        });
    }
    let Some(snapshot) = state.snapshot.clone() else {
        ui.add_space(space::S);
        match &state.error {
            Some(e) => note(ui, cx, Status::Error, error_text(cx.tr, e)),
            None => {
                ui.label(Text::small(cx.tr.t("router.loading"), cx.pal));
            }
        }
        return;
    };

    if let Ok(live) = &snapshot.live {
        local_shortage(ui, cx, &live.local);
    } else if mode(cx).is_router() {
        ui.add_space(space::S);
        note(ui, cx, Status::Error, cx.tr.t("router.live.down_on"));
    }
    if let Some(path) = &snapshot.network_exe {
        ui.add_space(space::S);
        note(
            ui,
            cx,
            Status::Warn,
            cx.tr.f("router.service.network_exe", &[("path", path)]),
        );
    }
}

/// The most frequent task comes first: the members in priority order with
/// their line order, how to add one (or, while none can carry traffic, the
/// fixes), the failover rules; then Claude Code's members when it is routed.
pub(super) fn members_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    section(
        ui,
        cx,
        &cx.tr.t("router.members.title"),
        Some(&cx.tr.t("router.members.order_hint")),
    );
    let snapshot = state.snapshot.clone();
    if let Some(snapshot) = &snapshot {
        members(ui, cx, state, snapshot);
    }
    if super::eligibility::has_eligible_member(cx, state) {
        if let Some(snapshot) = &snapshot {
            add_member(ui, cx, state, snapshot);
        }
    } else {
        super::eligibility::panel(ui, cx, state, out);
    }
    Disclosure::new("router-member-rules", cx.tr.t("router.members.rules")).show(ui, |ui| {
        ui.add(egui::Label::new(Text::small(cx.tr.t("router.members.help"), cx.pal)).wrap());
        ui.add(egui::Label::new(Text::small(cx.tr.t("router.preference.recovery"), cx.pal)).wrap());
        ui.add_space(space::XS);
    });
    if let Some(claude) = snapshot.as_ref().and_then(|s| s.claude.as_ref()) {
        ui.add_space(space::XL);
        claude_members(ui, cx, claude);
    }
}

/// Maintenance controls keep their names and behavior in Advanced.
pub(super) fn service_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    section(ui, cx, &cx.tr.t("router.service_label"), None);
    if let Some(snapshot) = state.snapshot.clone() {
        service_and_status(ui, cx, state, &snapshot, &mode(cx));
        if let Ok(live) = &snapshot.live
            && !live.events.is_empty()
        {
            ui.add_space(space::S);
            events(ui, cx, live);
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
        &cx.tr.t("router.service.state"),
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
                ui.label(Text::small(&s.backend, cx.pal))
                    .on_hover_text(tip.join(" · "));
            }
            None => {
                ui.label(Text::small(cx.tr.t("router.service.none"), cx.pal));
            }
        },
    );
    if let Some(s) = &snapshot.service {
        value_row(ui, "router-service-actions", space::XS, |ui| {
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
                // An address: monospace.
                ui.label(Text::mono_muted(&live.listen, cx.pal));
                ui.label(Text::small(format!("v{}", live.version), cx.pal));
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
        let mut stats = vec![
            ("router.live.uptime", model::duration_text(live.uptime_ms)),
            (
                "router.live.requests",
                model::count_text(live.totals.requests),
            ),
            (
                "router.live.active",
                model::count_text(live.totals.active_requests),
            ),
            (
                "router.live.failovers",
                model::count_text(live.totals.failovers),
            ),
        ];
        if live.totals.unavailable > 0 {
            stats.push((
                "router.live.unavailable",
                model::count_text(live.totals.unavailable),
            ));
        }
        stats.push((
            "router.live.sessions",
            live.totals.sticky_sessions.to_string(),
        ));
        if live.local.max_concurrent > 0 {
            stats.push((
                "router.live.capacity",
                format!("{} / {}", live.local.in_flight, live.local.max_concurrent),
            ));
        }
        value_row(ui, "router-live-stats", space::S, |ui| {
            // Each counter is one unit: a narrow column wraps between them,
            // never between a counter's name and its value.
            for (key, value) in stats {
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &cx.tr.t(key),
                    0.0,
                    egui::TextFormat::simple(
                        crate::design::fonts::font(
                            crate::design::tokens::text::CAPTION,
                            crate::design::fonts::Weight::Medium,
                        ),
                        cx.pal.text_subtle,
                    ),
                );
                job.append(
                    &value,
                    space::XS,
                    egui::TextFormat::simple(
                        crate::design::fonts::font(
                            crate::design::tokens::text::SMALL,
                            crate::design::fonts::Weight::Regular,
                        ),
                        cx.pal.text,
                    ),
                );
                ui.add(egui::Label::new(job).extend());
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
    ui.add_space(space::S);
    note(ui, cx, Status::Warn, text);
    if let Some(last) = &local.last_file_limit {
        // The system's error message, with the path it names.
        ui.label(Text::mono_muted(last, cx.pal));
    }
}

fn kind_text(cx: &Cx<'_>, kind: MemberKind) -> String {
    cx.tr.t(match kind {
        MemberKind::Krill => "router.kind.krill",
        MemberKind::Compatible => "router.kind.compatible",
    })
}

fn members(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, snapshot: &model::Snapshot) {
    let busy = state.op.is_some();
    let n = snapshot.members.len();
    if n == 0 {
        return;
    }
    divider(ui, cx);
    for (i, m) in snapshot.members.iter().enumerate() {
        ui.push_id(&m.id, |ui| member_row(ui, cx, state, m, i, n, busy));
        divider(ui, cx);
    }
}

/// Add another provider as a member: the candidates in a dropdown and 添加成员,
/// the key note, and which providers cannot join yet (no key).
fn add_member(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, snapshot: &model::Snapshot) {
    let busy = state.op.is_some();
    if !snapshot.candidates.is_empty() {
        ui.add_space(space::S);
        FormRow::unlabeled().show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            let selected = state
                .pick
                .and_then(|i| snapshot.candidates.get(i))
                .map(|c| candidate_label(cx, c))
                .unwrap_or_else(|| cx.tr.t("router.members.pick"));
            let add = Button::new(cx.tr.t("router.members.add"))
                .icon(Icon::Plus)
                .small();
            let add_w = add.desired_size(ui).x + space::S;
            Dropdown::new("router-add-member")
                .label(cx.tr.t("router.members.candidate"))
                .selected_text(selected)
                .fixed_width((ui.available_width() - add_w).clamp(0.0, 320.0))
                .show_ui(ui, |ui| {
                    for (i, c) in snapshot.candidates.iter().enumerate() {
                        ui.selectable_value(&mut state.pick, Some(i), candidate_label(cx, c));
                    }
                });
            let candidate = state.pick.and_then(|i| snapshot.candidates.get(i)).cloned();
            if ui
                .add(
                    add.loading(state.op.as_ref().is_some_and(|(op, _)| *op == Op::Add))
                        .enabled(!busy && candidate.is_some()),
                )
                .clicked()
                && let Some(c) = candidate
            {
                state.pick = None;
                state.add(cx, c);
            }
        });
        ui.add(egui::Label::new(Text::caption(cx.tr.t("router.members.key_note"), cx.pal)).wrap());
    }
    if !snapshot.keyless.is_empty() {
        ui.add(
            egui::Label::new(Text::caption(
                cx.tr.f(
                    "router.members.keyless",
                    &[("sites", &snapshot.keyless.join(", "))],
                ),
                cx.pal,
            ))
            .wrap(),
        );
    }
    ui.add_space(space::XS);
}

/// Claude Code's providers on the router (its own first), each with its
/// Anthropic endpoint or why it cannot serve Claude Code.
fn claude_members(ui: &mut Ui, cx: &Cx<'_>, claude: &superba_core::RouterClaudeView) {
    let (status, key) = if claude.routed {
        (Status::Ok, "router.claude.routed")
    } else {
        (Status::Warn, "router.claude.pending")
    };
    section_with(
        ui,
        cx,
        &cx.tr.t("router.claude.title"),
        Some(&cx.tr.t("router.claude.help")),
        |ui| {
            ui.add(Badge::new(cx.tr.t(key)).status(status));
        },
    );
    if let Some(url) = &claude.base_url {
        FormRow::new(Text::small(cx.tr.t("router.claude.address"), cx.pal))
            .label_width(0.0)
            .show(ui, |ui| {
                ui.add(
                    ValueChip::new(url, Shape::Url)
                        .plain()
                        .size(crate::design::tokens::text::SMALL)
                        .max_width(ui.available_width()),
                );
            });
    }
    divider(ui, cx);
    for m in &claude.members {
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
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
        indented(ui, |ui| {
            if m.ineligible.is_none() && !m.translated {
                let endpoint = m.endpoint.clone().unwrap_or_default();
                ui.add(
                    ValueChip::new(&endpoint, Shape::Url)
                        .plain()
                        .size(crate::design::tokens::text::SMALL)
                        .max_width(ui.available_width()),
                );
            } else {
                ui.add(
                    egui::Label::new(Text::small(cx.tr.text(&m.connection_text()), cx.pal)).wrap(),
                );
            }
        });
        ui.add_space(space::S);
        divider(ui, cx);
    }
}

/// Where a member's second and later lines start: under its name, past the
/// position number and the status dot.
fn indent_width() -> f32 {
    // The position column, a gap, the 10 pt status dot, a gap.
    space::L + space::S + 10.0 + space::S
}

/// `add` with the member rows' indent.
fn indented<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal_top(|ui| {
        ui.add_space(indent_width());
        ui.vertical(add).inner
    })
    .inner
}

fn candidate_label(cx: &Cx<'_>, c: &model::Candidate) -> String {
    let kind = kind_text(cx, c.kind);
    if c.name.trim().eq_ignore_ascii_case(kind.trim()) {
        c.name.clone()
    } else {
        format!("{} · {kind}", c.name)
    }
}

/// One member: position, health, name and priority with the move and remove
/// actions on its right; under the name its lines in priority order, the line
/// that answered last and the expandable line and protocol details.
fn member_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    m: &MemberView,
    index: usize,
    count: usize,
    busy: bool,
) {
    ui.add_space(space::S);
    // Reserve the action column before laying out names or badges.
    let actions_w = 3.0 * size::CONTROL_SM + 2.0 * space::XS;
    let label_w = (ui.available_width() - actions_w - space::M).max(0.0);
    FormRow::default().label_width(label_w).right().show_with(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.add_sized(
                    [space::L, size::CONTROL_SM],
                    egui::Label::new(Text::small(format!("{}", index + 1), cx.pal)),
                );
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
                ui.add(egui::Label::new(Text::strong(&m.name, cx.pal)).truncate());
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
                ui.spacing_mut().item_spacing.x = space::XS;
                if ui
                    .add(
                        IconButton::new(Icon::Close, cx.tr.t("router.members.remove"))
                            .size(size::CONTROL_SM)
                            .enabled(!busy),
                    )
                    .clicked()
                {
                    state.remove(cx, m.id.clone());
                }
                if ui
                    .add(
                        IconButton::new(Icon::ArrowDown, cx.tr.t("router.members.down"))
                            .size(size::CONTROL_SM)
                            .enabled(!busy && index + 1 < count),
                    )
                    .clicked()
                {
                    state.move_member(cx, index, false);
                }
                if ui
                    .add(
                        IconButton::new(Icon::ArrowUp, cx.tr.t("router.members.up"))
                            .size(size::CONTROL_SM)
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
    indented(ui, |ui| {
        ui.spacing_mut().item_spacing.y = space::XS;
        ui.add(
            egui::Label::new(Text::body(
                crate::pages::shared::priority_text(
                    cx.tr,
                    m.lines.iter().map(|l| line_name(&l.id)),
                ),
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
        Disclosure::new("router-member-details", cx.tr.t("router.members.details"))
            .show(ui, |ui| member_details(ui, cx, m, &line_name));
    });
    ui.add_space(space::XS);
}

/// The member's kind and abilities, how it serves each protocol, and its lines
/// as a table: name with health, address, state, first-token time, requests.
fn member_details(ui: &mut Ui, cx: &Cx<'_>, m: &MemberView, line_name: &dyn Fn(&str) -> String) {
    ui.horizontal_wrapped(|ui| {
        ui.add(Badge::new(kind_text(cx, m.kind)).outline());
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
    if !m.protocols.is_empty() {
        let labels: Vec<&str> = m.protocols.iter().map(|r| r.ingress()).collect();
        let label_w = FormRow::label_width_of(ui, labels.iter().copied());
        for route in &m.protocols {
            FormRow::new(Text::small(route.ingress(), cx.pal))
                .label_width(label_w)
                .show(ui, |ui| {
                    ui.label(
                        Text::small(cx.tr.text(&route.connection_text()), cx.pal)
                            .color(cx.pal.text),
                    );
                });
        }
    }
    if m.lines.is_empty() {
        return;
    }
    ui.add_space(space::XS);
    let caption = crate::design::fonts::font(
        crate::design::tokens::text::CAPTION,
        crate::design::fonts::Weight::Medium,
    );
    let headers = [
        cx.tr.t("router.line.col.line"),
        cx.tr.t("router.line.col.address"),
        cx.tr.t("router.line.col.state"),
        cx.tr.t("router.line.col.ttfb"),
        cx.tr.t("router.line.col.requests"),
    ];
    // Numbers: as wide as their header or "9,999 ms", whichever is wider.
    let number_w = |h: &str| {
        text_width(ui, h, caption.clone()).max(text_width(
            ui,
            "9,999 ms",
            crate::design::fonts::font(
                crate::design::tokens::text::SMALL,
                crate::design::fonts::Weight::Regular,
            ),
        )) + space::M
    };
    let table = Table::new(&[1.0, 1.6, 1.0, 0.0, 0.0])
        .min_widths(&[0.0, 0.0, 0.0, number_w(&headers[3]), number_w(&headers[4])])
        .right_aligned(&[3, 4]);
    table.header(ui, &headers);
    let n = m.lines.len();
    for (i, line) in m.lines.iter().enumerate() {
        let (status, key) = match line.health {
            LineHealth::Up => (Status::Ok, "router.line.up"),
            LineHealth::Probing => (Status::Warn, "router.line.probing"),
            LineHealth::Down => (Status::Error, "router.line.down"),
            LineHealth::Unknown => (Status::Idle, "router.line.unknown"),
        };
        let mut state = cx.tr.t(key);
        let mut tip = Vec::new();
        if let Some(metrics) = &line.metrics {
            if let Some(ms) = metrics.reopens_in_ms
                && line.health == LineHealth::Down
            {
                state = format!(
                    "{state} · {}",
                    cx.tr.f(
                        "router.line.retry_in",
                        &[("time", &model::duration_text(ms))],
                    )
                );
            }
            if metrics.failovers_away > 0 {
                tip.push(cx.tr.f(
                    "router.line.failovers",
                    &[("n", &model::count_text(metrics.failovers_away))],
                ));
            }
            if metrics.active_websockets > 0 {
                tip.push(cx.tr.f(
                    "router.line.websockets",
                    &[("n", &metrics.active_websockets)],
                ));
            }
            if let Some(err) = &metrics.last_error {
                tip.push(format!("{}: {err}", cx.tr.t("router.line.last_error")));
            }
        }
        let error = line
            .metrics
            .as_ref()
            .is_some_and(|m| m.last_error.is_some());
        let ttfb = line
            .metrics
            .as_ref()
            .and_then(|m| m.ttfb_ms)
            .map_or_else(|| "—".to_owned(), |ms| format!("{ms} ms"));
        let requests = line
            .metrics
            .as_ref()
            .map_or_else(|| "—".to_owned(), |m| model::count_text(m.requests));
        table.row(ui, size::ROW, i + 1 == n, |ui, col| match col {
            0 => {
                ui.add(StatusDot::new(status));
                ui.label(Text::small(line_name(&line.id), cx.pal).color(cx.pal.text));
            }
            1 => {
                ui.add(
                    ValueChip::new(&line.base_url, Shape::Url)
                        .plain()
                        .muted(true)
                        .max_width(ui.available_width()),
                );
            }
            2 => {
                let text = Text::small(&state, cx.pal);
                let r = ui.label(if error {
                    text.color(cx.pal.amber)
                } else {
                    text
                });
                if !tip.is_empty() {
                    r.on_hover_text(tip.join("\n"));
                }
            }
            3 => {
                ui.label(Text::small(&ttfb, cx.pal));
            }
            _ => {
                ui.label(Text::small(&requests, cx.pal));
            }
        });
    }
    ui.add_space(space::XS);
}

/// The router's recent events (newest first, at most 30) as a table: when,
/// what, which member / line, and the detail.
fn events(ui: &mut Ui, cx: &Cx<'_>, live: &Live) {
    let n = live.events.len().min(30);
    Disclosure::new(
        "router-events",
        format!("{} ({n})", cx.tr.t("router.events.title")),
    )
    .show(ui, |ui| {
        let headers = [
            cx.tr.t("router.events.col.time"),
            cx.tr.t("router.events.col.event"),
            cx.tr.t("router.events.col.target"),
            cx.tr.t("router.events.col.detail"),
        ];
        let table = Table::new(&[0.8, 1.4, 1.2, 1.6]);
        table.header(ui, &headers);
        let now_ms = superba_core::Timestamp::now().as_millis() as u64;
        for (i, e) in live.events.iter().rev().take(30).enumerate() {
            let target: Vec<&str> = [e.member.as_deref(), e.line.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            table.row(ui, size::ROW, i + 1 == n, |ui, col| match col {
                0 => {
                    ui.label(Text::small(
                        cx.tr.f(
                            "router.events.ago",
                            &[(
                                "time",
                                &model::duration_text(now_ms.saturating_sub(e.at_ms)),
                            )],
                        ),
                        cx.pal,
                    ));
                }
                1 => {
                    ui.add(StatusDot::new(if e.severe {
                        Status::Warn
                    } else {
                        Status::Idle
                    }));
                    ui.label(Text::small(cx.tr.t(e.kind_key), cx.pal).color(cx.pal.text));
                }
                2 => {
                    ui.label(Text::small(target.join(" / "), cx.pal));
                }
                _ => {
                    if let Some(d) = &e.detail {
                        ui.label(Text::small(d, cx.pal)).on_hover_text(d);
                    }
                }
            });
        }
    });
}
