//! 「故障转移与恢复」 / "Failover and recovery" (user bug 117): the rules that
//! were read-only text in a disclosure, now settings. Each change is saved to
//! the router configuration and a running router reloads it (no restart of
//! Codex): the first and the longest cooldown of a failed line, the statuses
//! that fail over to the next line, how long a conversation stays on its line
//! and whether it returns to the preferred member once that has recovered.

use egui::Ui;
use superba_core::RouterFailover;

use super::{Op, State};
use crate::design::tokens::space;
use crate::design::widgets::{Status, Text, Toggle};
use crate::pages::{Cx, section};

/// The rules shown: the edit being saved, else the saved ones.
fn current(state: &State) -> Option<RouterFailover> {
    state
        .failover_draft
        .clone()
        .or_else(|| state.snapshot.as_ref().and_then(|s| s.failover.clone()))
}

/// Save `next` when it differs from what is shown.
fn save(state: &mut State, cx: &Cx<'_>, next: RouterFailover) {
    if current(state).as_ref() == Some(&next) || next.validate().is_err() {
        return;
    }
    state.failover_draft = Some(next.clone());
    state.start(cx, Op::Failover, |b| b.set_failover(next));
}

/// A number field committed when the drag ends, the field loses focus or a
/// step was clicked (not on every frame of a drag).
fn number(
    ui: &mut Ui,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    suffix: &str,
    name: &str,
    enabled: bool,
) -> bool {
    let r = ui
        .add_enabled(
            enabled,
            egui::DragValue::new(value)
                .range(range)
                .suffix(suffix)
                .speed(0.25),
        )
        .accessible_name(name);
    r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus())
}

pub(super) fn section_ui(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    section(
        ui,
        cx,
        &cx.tr.t("router.failover.title"),
        Some(&cx.tr.t("router.failover.caption")),
    );
    let saved = current(state);
    let enabled = saved.is_some() && state.op.is_none();
    if saved.is_none() {
        crate::pages::note(ui, cx, Status::Idle, cx.tr.t("router.failover.unknown"));
        ui.add_space(space::S);
    }
    let f = saved.unwrap_or_default();
    let secs = cx.tr.t("router.failover.seconds");
    let mins = cx.tr.t("router.failover.minutes");

    let title = cx.tr.t("router.failover.cooldown");
    let mut next = f.clone();
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.cooldown_hint"),
        false,
        |ui| {
            if number(
                ui,
                &mut next.cooldown_secs,
                RouterFailover::COOLDOWN,
                &secs,
                &title,
                enabled,
            ) {
                next.max_cooldown_secs = next.max_cooldown_secs.max(next.cooldown_secs);
                save(state, cx, next.clone());
            }
        },
    );

    let title = cx.tr.t("router.failover.max_cooldown");
    let mut next = f.clone();
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.max_cooldown_hint"),
        false,
        |ui| {
            let range = f.cooldown_secs..=*RouterFailover::MAX_COOLDOWN.end();
            if number(
                ui,
                &mut next.max_cooldown_secs,
                range,
                &secs,
                &title,
                enabled,
            ) {
                save(state, cx, next.clone());
            }
        },
    );

    // The statuses: one check box each, the gateway errors on by default.
    ui.add_space(space::M);
    ui.label(Text::strong(cx.tr.t("router.failover.statuses"), cx.pal));
    ui.add(
        egui::Label::new(Text::small(
            cx.tr.t("router.failover.statuses_hint"),
            cx.pal,
        ))
        .wrap(),
    );
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        for code in RouterFailover::STATUSES {
            let mut on = f.failover_statuses.contains(&code);
            let label = cx.tr.f("router.failover.status", &[("code", &code)]);
            if ui
                .add_enabled(enabled, egui::Checkbox::new(&mut on, label))
                .changed()
            {
                let mut next = f.clone();
                next.failover_statuses.retain(|c| *c != code);
                if on {
                    next.failover_statuses.push(code);
                    next.failover_statuses.sort_unstable();
                }
                save(state, cx, next);
            }
        }
    });
    ui.add_space(space::M);
    crate::pages::divider(ui, cx);

    let title = cx.tr.t("router.failover.sticky");
    let mut next = f.clone();
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.sticky_hint"),
        false,
        |ui| {
            if number(
                ui,
                &mut next.sticky_idle_minutes,
                RouterFailover::STICKY_IDLE,
                &mins,
                &title,
                enabled,
            ) {
                save(state, cx, next.clone());
            }
        },
    );

    let title = cx.tr.t("router.failover.return");
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.return_hint"),
        true,
        |ui| {
            let mut on = f.return_to_preferred;
            if ui
                .add(Toggle::new(&mut on, &title).enabled(enabled))
                .changed()
            {
                let mut next = f.clone();
                next.return_to_preferred = on;
                save(state, cx, next);
            }
        },
    );
}
