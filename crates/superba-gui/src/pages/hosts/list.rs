//! The host list (260 px), in the Surge list look (user request 24): a server
//! icon with the status dot on its corner, the label and one hint line
//! (unsynced settings / synced / not connected) across the row, the Codex
//! version on the right of the name.
//! Connected hosts first. The rows are rounded tiles separated by spacing,
//! without divider lines. The empty state never suggests ssh-config hosts.

use egui::{CursorIcon, Rect, Sense, Ui, pos2, vec2};
use superba_core::HostView;

use super::State;
use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, size, space, text};
use crate::design::widgets::{RowContent, Status, paint_row};
use crate::pages::{Cx, empty_state};

/// Height of one host row.
pub const ROW: f32 = size::ROW_LIST2;
/// Space between two host rows.
const GAP: f32 = space::XS;

/// The hint line of a host.
pub fn hint(cx: &Cx<'_>, h: &HostView) -> String {
    if h.host.connected != Some(true) {
        let sync = if h.settings.last_sync.is_some() {
            cx.tr.t("hosts.list.synced")
        } else {
            cx.tr.t("hosts.sync.never")
        };
        return format!("{} · {sync}", cx.tr.t("hosts.not_connected"));
    }
    if h.probe_error.is_some() {
        return cx.tr.t("hosts.list.probe_failed");
    }
    match h.pending_sync {
        Some(n) if n > 0 => cx.tr.f("hosts.list.unsynced", &[("n", &n)]),
        Some(_) => cx.tr.t("hosts.list.synced"),
        None if h.settings.last_sync.is_none() => cx.tr.t("hosts.sync.never"),
        None => cx.tr.t("hosts.list.synced"),
    }
}

/// Status of a host's dot.
pub fn dot(h: &HostView) -> Status {
    match (h.host.connected, &h.probe_error) {
        (Some(true), Some(_)) => Status::Warn,
        (Some(true), None) => Status::Ok,
        _ => Status::Idle,
    }
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let current = state.current().map(|h| h.host.target_id.clone());
    let hosts: Vec<HostView> = state.sorted().into_iter().cloned().collect();
    let rect = ui.available_rect_before_wrap();
    let mut y = rect.min.y;
    let mut shown = 0;
    for h in &hosts {
        if y + ROW > rect.max.y {
            break;
        }
        let r = Rect::from_min_size(pos2(rect.min.x, y), vec2(rect.width(), ROW));
        let resp = ui
            .interact(
                r,
                ui.scope_id().with(("host", h.host.target_id.as_str())),
                Sense::click(),
            )
            .on_hover_cursor(CursorIcon::PointingHand);
        let selected = current.as_ref() == Some(&h.host.target_id);
        if selected {
            ui.painter().rect_filled(r, radius::ROW, cx.pal.active);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, radius::ROW, cx.pal.hover.gamma_multiply(0.7));
        }
        let version = h
            .state
            .as_ref()
            .and_then(|s| s.codex_version.as_ref())
            .map(|v| v.to_string());
        let mut row = RowContent::new(Icon::Server, h.host.label.clone());
        row.dot = Some(cx.pal.status(dot(h)));
        // The Codex version on the name's line; the status line under the name
        // gets the whole width (no "Codex" caption beside it).
        row.subtitle = Some(hint(cx, h));
        row.value = version;
        paint_row(ui, r, &row);
        if resp.clicked() {
            state.selected = Some(h.host.target_id.clone());
        }
        shown += 1;
        y += ROW + GAP;
    }
    if shown < hosts.len() {
        ui.painter().text(
            pos2(rect.min.x + 8.0, y + 4.0),
            egui::Align2::LEFT_TOP,
            cx.tr.f("hosts.list.more", &[("n", &(hosts.len() - shown))]),
            font(text::CAPTION, Weight::Regular),
            cx.pal.text_subtle,
        );
    }
    ui.allocate_rect(rect, Sense::hover());
}

pub(super) fn empty(ui: &mut Ui, cx: &Cx<'_>, state: &State) {
    if state.hosts.value.is_none() {
        match &state.hosts.error {
            Some(e) => crate::pages::error_state(ui, cx, &cx.tr.t("hosts.load_failed"), e),
            None => {
                ui.label(crate::design::widgets::Text::muted(
                    cx.tr.t("hosts.loading"),
                    cx.pal,
                ));
            }
        }
        return;
    }
    empty_state(
        ui,
        cx,
        Icon::Server,
        &cx.tr.t("hosts.empty.title"),
        &cx.tr.t("hosts.empty.body"),
    );
}
