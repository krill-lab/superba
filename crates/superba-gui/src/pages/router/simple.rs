//! Simple router controls above the full mode's detailed usage view.

use egui::{Align, Layout, Ui};

use super::{Op, State, dialogs, doctor, mode, model, ready, tick, usage_content};
use crate::design::icons::Icon;
use crate::design::tokens::space;
use crate::design::widgets::{Button, Status, StatusDot, Text, Toggle};
use crate::pages::{Action, Cx, Page, divider};

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let on = mode(cx).is_router();
    tick(ui, cx, state, out, on);
    header(ui, cx, state, out);
    gap(ui, cx);
    super::view::notices(ui, cx, state);
    super::eligibility::panel(ui, cx, state, out);
    service(ui, cx, state);
    gap(ui, cx);
    ui.horizontal_wrapped(|ui| {
        ui.label(Text::muted(cx.tr.t("router.simple.advanced"), cx.pal));
        if ui
            .add(Button::new(cx.tr.t("router.simple.full_mode")).small())
            .clicked()
        {
            out.push(Action::SetSimpleMode(false));
        }
    });
    gap(ui, cx);
    if on {
        usage_content(ui, cx, state, out);
    } else {
        ui.add_space(space::L);
        ui.vertical_centered(|ui| {
            ui.label(Text::muted(cx.tr.t("router.simple.usage_off"), cx.pal));
        });
    }
    dialogs(ui, cx, state, out);
}

fn gap(ui: &mut Ui, cx: &Cx<'_>) {
    ui.add_space(space::M);
    divider(ui, cx);
    ui.add_space(space::M);
}

fn header(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let mut on = mode(cx).is_router();
    let enabled = !cx.busy && state.op.is_none() && (on || ready(cx, state));
    ui.label(Text::page_title(cx.tr.t(Page::Router.title_key()), cx.pal));
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 36.0 - space::L).max(120.0);
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.add(
                egui::Label::new(Text::small(cx.tr.t("router.simple.description"), cx.pal)).wrap(),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let hint = cx.tr.t(if on || ready(cx, state) {
                "router.preview_hint"
            } else {
                "router.eligibility.none"
            });
            if ui
                .add(Toggle::new(&mut on, cx.tr.t("router.mode_label")).enabled(enabled))
                .on_hover_text(&hint)
                .on_disabled_hover_text(hint)
                .changed()
            {
                state.switch_mode(cx, on, out);
            }
        });
    });
}

fn service(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(snapshot) = state.snapshot.clone() else {
        return;
    };
    let live = snapshot.live.as_ref().ok();
    let installed = snapshot.service.as_ref().is_some_and(|s| s.installed);
    let running = live.is_some()
        || snapshot
            .service
            .as_ref()
            .is_some_and(|s| s.running == Some(true));
    ui.horizontal_wrapped(|ui| {
        ui.label(Text::small(cx.tr.t("router.service_label"), cx.pal));
        ui.add(StatusDot::new(if running {
            Status::Ok
        } else {
            Status::Idle
        }));
        ui.label(Text::body(
            cx.tr.t(if running {
                "router.service.running"
            } else if installed {
                "router.service.stopped"
            } else {
                "router.service.not_installed"
            }),
            cx.pal,
        ));
        for (key, value) in [
            (
                "router.live.uptime",
                live.map(|l| model::duration_text(l.uptime_ms)),
            ),
            (
                "router.live.requests",
                live.map(|l| model::count_text(l.totals.requests)),
            ),
            (
                "router.live.active",
                live.map(|l| model::count_text(l.totals.active_requests)),
            ),
            (
                "router.live.capacity",
                live.map(|l| format!("{} / {}", l.local.in_flight, l.local.max_concurrent)),
            ),
        ] {
            ui.add_space(space::S);
            ui.label(Text::small(cx.tr.t(key), cx.pal));
            ui.label(Text::body(value.unwrap_or_else(|| "—".into()), cx.pal));
        }
    });
    actions(
        ui,
        cx,
        state,
        installed,
        running,
        !snapshot.members.is_empty(),
    );
}

fn actions(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    installed: bool,
    running: bool,
    members: bool,
) {
    let busy = cx.busy || state.op.is_some();
    let op = state.op.as_ref().map(|(op, _)| *op);
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        if ui
            .add(
                Button::new(cx.tr.t("router.service.restart"))
                    .small()
                    .loading(op == Some(Op::Restart))
                    .enabled(!busy && (installed || running)),
            )
            .clicked()
        {
            state.start(cx, Op::Restart, |b| b.restart());
        }
        if ui
            .add(
                Button::new(cx.tr.t("router.service.reinstall"))
                    .small()
                    .loading(op == Some(Op::Install))
                    .enabled(!busy && members),
            )
            .clicked()
        {
            state.start(cx, Op::Install, |b| b.install());
        }
        if ui
            .add(
                Button::new(cx.tr.t("router.service.uninstall"))
                    .small()
                    .loading(op == Some(Op::Uninstall))
                    .enabled(
                        !busy && installed && !cx.data.router_enabled() && !mode(cx).is_router(),
                    ),
            )
            .on_disabled_hover_text(cx.tr.t("router.service.uninstall_hint"))
            .clicked()
        {
            state.start(cx, Op::Uninstall, |b| b.uninstall());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::new(cx.tr.t("router.doctor.open"))
                        .small()
                        .icon(Icon::Diagnostics)
                        .loading(state.doctor.loading()),
                )
                .clicked()
            {
                doctor::open(state, cx);
            }
        });
    });
}
