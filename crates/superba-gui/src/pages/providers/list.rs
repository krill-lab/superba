//! The provider list (left column, UX §3.3), in the Surge list look (user
//! request 24): an icon tile (a green dot on it for the provider in use), the
//! name and one secondary line (lines and auth for presets, host and auth for
//! custom sites, the login for the official one), and on the right the primary
//! line's latency with "in use" under it. ↑/↓ move the selection, Enter switches
//! to the selected provider. The rows are rounded tiles separated by spacing,
//! without divider lines.
//!
//! A right click opens the entry's menu (rename, duplicate, test, delete). The
//! name is renamed in place: a click on the name of the selected entry (after
//! the double-click delay, so a double click is not split), a double click on
//! any entry, F2, or Rename in the menu turn it into a field with the text
//! selected; Enter saves, Escape or a click elsewhere cancels, and an empty or
//! taken name is refused with the reason under the field.

use std::time::Duration;

use egui::{Key, Rect, Sense, Ui, vec2};
use superba_core::AuthMode;

use super::State;
use super::actions::{self, RenameOutcome};
use crate::design::icons::Icon;
use crate::design::tokens::{size, space};
use crate::design::widgets::{Button, PagedTable, RowContent, Status, Table, list_row, paint_row};
use crate::pages::shared::providers::{Provider, ProviderKind};
use crate::pages::shared::{auto_facts, decision_with, ordered_lines};
use crate::pages::{Action, Cx};

/// Display name of a provider.
pub(super) fn name(cx: &Cx<'_>, p: &Provider) -> String {
    match p.kind {
        ProviderKind::Official => cx.tr.t("providers.official.name"),
        _ => p
            .profile
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| cx.data.site_name(cx.tr, &p.site)),
    }
}

/// The auth a provider resolves to on its primary line ("PAT", "API key").
pub(super) fn auth_label(cx: &Cx<'_>, p: &Provider) -> Option<String> {
    let profile = p.profile.as_ref()?;
    let lines = ordered_lines(&p.site, Some(profile));
    let primary = lines.first();
    let caps = primary.and_then(|l| cx.data.site_caps(&p.site.id, &l.id));
    let d = decision_with(&p.site, primary, caps, Some(profile), auto_facts(cx.data));
    d.mode.map(|m| cx.auth_short(m))
}

/// The secondary line of a row.
pub(super) fn secondary(cx: &Cx<'_>, p: &Provider) -> String {
    match p.kind {
        ProviderKind::Official => cx.tr.t("providers.official.short"),
        ProviderKind::Custom => {
            let host = p
                .profile
                .as_ref()
                .and_then(|p| p.base_url_override.as_ref())
                .and_then(|u| u.host_str().map(str::to_owned))
                .or_else(|| p.site.default_line().map(|l| l.origin.host().to_owned()))
                .unwrap_or_default();
            if p.has_key() {
                format!("{host} · {}", cx.auth_short(AuthMode::ApiKey))
            } else {
                format!("{host} · {}", cx.tr.t("providers.no_key"))
            }
        }
        ProviderKind::Preset => {
            let lines = cx
                .tr
                .f("providers.lines.count", &[("n", &p.site.lines.len())]);
            match (p.has_key(), auth_label(cx, p)) {
                (true, Some(auth)) => format!("{lines} · {auth}"),
                (true, None) => lines,
                (false, _) => format!("{lines} · {}", cx.tr.t("providers.no_key")),
            }
        }
    }
}

/// Draw the list and the add button below it.
pub(super) fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    all: &[Provider],
    out: &mut Vec<Action>,
) {
    let total = all.len();
    let current = state
        .current(all)
        .and_then(|c| all.iter().position(|p| p.id == c.id));
    if state.list.selected != current {
        state.list.selected = current;
        if let Some(i) = current {
            state.list.select(i, total);
        }
    }
    let mut add = false;
    let modal = state.is_modal_open();
    // Selection before this frame's clicks: a click on the name of the entry
    // that was already selected starts a rename.
    let before = state.list.selected;
    let mut name_rects: Vec<(usize, Rect)> = Vec::new();
    let mut outcome = None;
    let mut rename_drawn = false;
    let rename = &mut state.rename;
    let r = PagedTable::new("providers-list", Table::new(&[1.0]), total, &mut state.list)
        .row_height(size::ROW_LIST2)
        .tiles(space::XS)
        .keys_when_idle(!modal)
        .footer(|ui| {
            add = ui
                .add(
                    Button::ghost(cx.tr.t("providers.add_short"))
                        .icon(Icon::Plus)
                        .small()
                        .enabled(!modal),
                )
                .clicked();
        })
        .show(ui, |ui, row, _| {
            let p = &all[row];
            match rename.as_mut().filter(|e| e.provider == p.id) {
                Some(edit) => {
                    rename_drawn = true;
                    status_dot(ui, cx, p);
                    let check = |text: &str| actions::rename_problem(cx, all, &p.id, text);
                    outcome = actions::rename_field(ui, cx, edit, check).or(outcome.take());
                }
                None => name_rects.push((row, row_ui(ui, cx, p))),
            }
        });
    if add {
        state.open_add();
    }
    finish_rename(state, all, outcome, rename_drawn, out);
    if r.selection_changed
        && let Some(i) = state.list.selected
        && let Some(p) = all.get(i)
    {
        state.select(p.id.clone());
    }
    // Right click: select the entry and open its menu.
    for (row, resp) in &r.rows {
        let Some(p) = all.get(*row) else {
            continue;
        };
        if modal {
            continue;
        }
        if resp.secondary_clicked() {
            state.select(p.id.clone());
            state.list.select(*row, total);
        }
        actions::context_menu(resp, cx, state, p, out);
    }
    // Double click (or a quick third click): rename in place. Enter: switch to
    // the provider (through the switch flow).
    let double = r
        .rows
        .iter()
        .find(|(_, resp)| resp.double_clicked() || resp.triple_clicked())
        .map(|(row, _)| *row);
    if let Some(row) = double {
        state.rename_armed = None;
        if let Some(p) = all.get(row)
            && !modal
        {
            actions::start_rename(state, p);
        }
    } else if let Some(i) = r.activated
        && let Some(p) = all.get(i)
        && !cx.busy
        && !p.applied
        && let Some(action) = super::detail::switch_action(p)
    {
        out.push(action);
    }
    // F2 renames the selected entry while the list (or nothing) has the focus.
    if !modal
        && state.rename.is_none()
        && ui.input(|i| i.key_pressed(Key::F2))
        && (r.response.has_focus() || ui.memory(|m| m.focused().is_none()))
        && let Some(p) = state.current(all).cloned()
    {
        actions::start_rename(state, &p);
    }
    // A click on the name of the entry that was already selected: rename once
    // the double-click delay has passed without a second click.
    let now = ui.input(|i| i.time);
    for (row, resp) in &r.rows {
        if resp.clicked()
            && double.is_none()
            && before == Some(*row)
            && state.rename.is_none()
            && let Some(pos) = resp.interact_pointer_pos()
            && name_rects
                .iter()
                .any(|(i, rect)| i == row && rect.contains(pos))
            && let Some(p) = all.get(*row)
            && actions::can_rename(p)
        {
            state.rename_armed = Some((p.id.clone(), now));
        }
    }
    if let Some((id, at)) = state.rename_armed.clone() {
        let delay = ui.ctx().options(|o| o.input_options.max_double_click_delay);
        if now - at > delay {
            state.rename_armed = None;
            if let Some(p) = state.current(all).filter(|p| p.id == id).cloned() {
                actions::start_rename(state, &p);
            }
        } else {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(delay - (now - at) + 0.02));
        }
    }
}

/// Apply what the inline name field asked for; an edit whose entry is not on
/// this page any more is dropped.
fn finish_rename(
    state: &mut State,
    all: &[Provider],
    outcome: Option<RenameOutcome>,
    drawn: bool,
    out: &mut Vec<Action>,
) {
    match outcome {
        Some(RenameOutcome::Save(name)) => {
            if let Some(edit) = state.rename.take()
                && let Some(profile) = all
                    .iter()
                    .find(|p| p.id == edit.provider)
                    .and_then(|p| p.profile.as_ref())
                && profile.name != name
            {
                // Saving the profile only: a rename never switches or restarts Codex.
                let mut renamed = profile.clone();
                renamed.name = name;
                out.push(Action::SaveProfile {
                    profile: Box::new(renamed),
                    key: None,
                });
            }
        }
        Some(RenameOutcome::Cancel) => state.rename = None,
        None if !drawn => state.rename = None,
        None => {}
    }
}

/// The icon of a provider's row.
fn icon(p: &Provider) -> Icon {
    match p.kind {
        ProviderKind::Official => Icon::Key,
        ProviderKind::Custom => Icon::Plug,
        ProviderKind::Preset => Icon::Lines,
    }
}

/// The icon tile of a row being renamed (same place as in [`row_ui`]).
fn status_dot(ui: &mut Ui, cx: &Cx<'_>, p: &Provider) {
    let h = ui.max_rect().height();
    let side = list_row::tile_side(h);
    let (r, _) = ui.allocate_exact_size(vec2(10.0 + side + 6.0, h), Sense::hover());
    list_row::icon_tile(
        ui,
        egui::pos2(r.min.x + 10.0 + side / 2.0, r.center().y),
        side,
        icon(p),
        None,
    );
    if p.applied {
        let c = egui::pos2(r.min.x + 10.0 + side - 2.0, r.center().y + side / 2.0 - 2.0);
        ui.painter()
            .circle_filled(c, 4.0, cx.pal.status(Status::Ok));
    }
}

/// Latency of the provider's primary line, when it was probed.
fn primary_latency(cx: &Cx<'_>, p: &Provider) -> Option<String> {
    let lines = ordered_lines(&p.site, p.profile.as_ref());
    let caps = cx.data.site_caps(&p.site.id, &lines.first()?.id)?;
    if !caps.reachable {
        return Some(cx.tr.t("overview.latency.unreachable"));
    }
    caps.latency_ms.map(|ms| format!("{ms} ms"))
}

/// One row; returns where the name was drawn.
fn row_ui(ui: &mut Ui, cx: &Cx<'_>, p: &Provider) -> Rect {
    let rect = ui.max_rect();
    let mut row = RowContent::new(icon(p), name(cx, p));
    row.dot = p.applied.then(|| cx.pal.status(Status::Ok));
    row.subtitle = Some(secondary(cx, p));
    row.value = primary_latency(cx, p);
    row.secondary = p.applied.then(|| cx.tr.t("providers.applied"));
    if row.value.is_none() {
        // "In use" alone sits on the value line.
        row.value = row.secondary.take();
    }
    let name_rect = paint_row(ui, rect, &row);
    ui.allocate_rect(rect, Sense::hover());
    name_rect
}
