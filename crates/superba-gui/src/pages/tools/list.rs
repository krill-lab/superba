//! Seven neutral tool identities, with keyboard selection and spoken status.
use super::{Selection, State, ToolState};
use crate::design::widgets::{Button, MonogramTile, Status, StatusDot, Text};
use crate::design::{
    a11y,
    fonts::{Weight, font},
    tokens::{radius, size, space, text},
};

/// Side of a row's monogram ([`MonogramTile`]'s default).
const TILE: f32 = 30.0;
/// Left edge of the monograms and the group captions.
const TILE_X: f32 = space::S;
use crate::pages::{Action, Cx, Page, SuperbaSection};
use superba_core::harness::{HarnessId, HarnessRoute};

/// A row with a complete accessibility description.
#[derive(Clone, Debug)]
pub struct Row {
    /// Destination.
    pub selection: Selection,
    /// Provider, connection and state, or why there is none.
    pub summary: String,
    /// Status dot.
    pub status: Status,
}

/// The line under a row's name: provider, connection and state
/// ("Krill · 直连 · 已应用"), or only why there is none (`idle`: "未配置",
/// "未安装"). Codex has no state of its own.
pub fn summary_line(
    provider: Option<&str>,
    route: &str,
    state: Option<&str>,
    idle: Option<&str>,
) -> String {
    match (idle, provider) {
        (None, Some(p)) => std::iter::once(p)
            .chain(Some(route))
            .chain(state)
            .collect::<Vec<_>>()
            .join(" · "),
        (idle, _) => idle.unwrap_or("—").to_owned(),
    }
}

fn route_label(cx: &Cx<'_>, routed: bool) -> String {
    cx.tr.t(if routed {
        "tools.routed"
    } else {
        "tools.direct"
    })
}

fn claude_state(state: &State) -> ToolState {
    state.claude.as_ref().map_or(ToolState::NotConfigured, |v| {
        ToolState::of(
            v.applied.is_some(),
            false,
            !v.pending.is_empty(),
            false,
            false,
        )
    })
}

/// The header badge of Codex ("in use" while a provider is applied) and Claude Code.
pub fn summary_state(cx: &Cx<'_>, state: &State, selection: Selection) -> (Status, String) {
    if selection == Selection::Codex {
        if cx.data.active_profile().is_some() {
            (Status::Ok, cx.tr.t("tools.in_use"))
        } else {
            (Status::Idle, cx.tr.t("tools.not_configured"))
        }
    } else {
        let s = claude_state(state);
        (s.status(), cx.tr.t(s.key()))
    }
}

/// Build rows before splitting the page into independently borrowed panes.
pub fn rows(cx: &Cx<'_>, state: &State) -> Vec<Row> {
    let active = cx.data.active_profile();
    let not_configured = cx.tr.t("tools.not_configured");
    let not_installed = cx.tr.t("tools.not_installed");
    let claude = claude_state(state);
    let claude_provider = state
        .claude
        .as_ref()
        .and_then(|v| v.provider.as_ref())
        .and_then(|id| cx.data.profile(id));
    let claude_label = cx.tr.t(claude.key());
    let mut rows = vec![
        Row {
            selection: Selection::Codex,
            summary: summary_line(
                active.map(|p| p.name.as_str()),
                &route_label(cx, cx.data.router_enabled()),
                None,
                active.is_none().then_some(not_configured.as_str()),
            ),
            status: if active.is_some() {
                Status::Ok
            } else {
                Status::Idle
            },
        },
        Row {
            selection: Selection::Claude,
            summary: summary_line(
                claude_provider.map(|p| p.name.as_str()),
                &route_label(cx, cx.data.claude_routed()),
                Some(&claude_label),
                (claude == ToolState::NotConfigured).then_some(not_configured.as_str()),
            ),
            status: claude.status(),
        },
    ];
    for h in HarnessId::ALL {
        let settings = state.settings(h);
        let tool = state.tool_state(cx, h);
        let installed = state
            .views
            .get(&h)
            .map(|v| v.install.is_some())
            .or_else(|| {
                state
                    .summaries
                    .iter()
                    .find(|s| s.harness == h)
                    .map(|s| s.installed)
            })
            .unwrap_or(true);
        let provider = match &settings.provider {
            Some(id) => cx.data.profile(id),
            None => active,
        };
        let label = cx.tr.t(tool.key());
        rows.push(Row {
            selection: Selection::Tool(h),
            summary: summary_line(
                provider.map(|p| p.name.as_str()),
                &route_label(cx, settings.route == HarnessRoute::Routed),
                Some(&label),
                (tool == ToolState::NotConfigured).then_some(if installed {
                    not_configured.as_str()
                } else {
                    not_installed.as_str()
                }),
            ),
            status: tool.status(),
        });
    }
    rows
}
/// Draw the list; Up/Down on a focused row selects its neighbour.
pub fn show(
    ui: &mut egui::Ui,
    cx: &Cx<'_>,
    rows: &[Row],
    selected: Selection,
    out: &mut Vec<Action>,
) -> Option<Selection> {
    ui.spacing_mut().item_spacing.y = 2.0;
    let mut picked = None;
    for (i, row) in rows.iter().enumerate() {
        if i == 0 || i == 2 {
            ui.add_space(if i == 0 { 0.0 } else { space::M });
            ui.horizontal(|ui| {
                // In line with the monograms of the rows below.
                ui.add_space(TILE_X);
                ui.label(Text::caption(
                    cx.tr.t(if i == 0 {
                        "tools.own_pages"
                    } else {
                        "tools.other"
                    }),
                    cx.pal,
                ));
            });
            ui.add_space(space::XS);
        }
        let is_selected = selected == row.selection
            || (selected == Selection::Initial
                && row.selection == Selection::Tool(HarnessId::OpenCode));
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), size::TOOL_ROW),
            egui::Sense::hover(),
        );
        let response = ui.interact(
            rect,
            ui.scope_id().with(("tool-row", row.selection)),
            egui::Sense::click(),
        );
        let name = format!("{}, {}", row.selection.name(), row.summary);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::ListItem, ui.is_enabled(), is_selected, &name)
        });
        a11y::selectable(&response, egui::Role::ListItem, &name, is_selected);
        if response.clicked() {
            picked = Some(row.selection);
            // A clicked row keeps keyboard focus, so Up/Down continue from it.
            response.request_focus();
        }
        if response.gained_focus() {
            response.scroll_to_me(None);
        }
        if response.has_focus() {
            // The rows move the selection with Up/Down themselves; egui must not
            // also move the focus to the widget below.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let next = ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    Some((i + 1).min(rows.len() - 1))
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    Some(i.saturating_sub(1))
                } else {
                    None
                }
            });
            if let Some(n) = next {
                picked = Some(rows[n].selection);
                ui.memory_mut(|m| {
                    m.request_focus(ui.scope_id().with(("tool-row", rows[n].selection)))
                });
            }
        }
        if is_selected || response.hovered() {
            ui.painter().rect_filled(
                rect,
                radius::ROW,
                if is_selected {
                    cx.pal.active
                } else {
                    cx.pal.hover
                },
            );
        }
        MonogramTile::new(row.selection.monogram()).paint_at(
            ui,
            egui::pos2(rect.left() + TILE_X + TILE / 2.0, rect.center().y),
        );
        let left = rect.left() + TILE_X + TILE + space::M;
        ui.painter().text(
            egui::pos2(left, rect.top() + 9.0),
            egui::Align2::LEFT_TOP,
            row.selection.name(),
            font(text::BODY, Weight::Medium),
            cx.pal.text,
        );
        let mut job = egui::text::LayoutJob::simple_singleline(
            row.summary.clone(),
            font(text::SMALL, Weight::Regular),
            cx.pal.text_muted,
        );
        job.wrap.max_width = (rect.right() - left - 18.0).max(16.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            egui::pos2(left, rect.top() + 29.0),
            galley,
            cx.pal.text_muted,
        );
        let dot = egui::pos2(rect.right() - 10.0, rect.center().y);
        if row.status == Status::Idle {
            ui.painter()
                .circle_stroke(dot, 3.0, egui::Stroke::new(1.0, cx.pal.text_muted));
        } else {
            StatusDot::new(row.status).size(6.0).paint_at(ui, dot);
        }
        if crate::design::widgets::show_focus(&response) {
            crate::design::widgets::focus_ring(ui.painter(), rect, radius::ROW, cx.pal);
        }
    }
    ui.add_space(space::M);
    if ui
        .add(
            Button::ghost(cx.tr.t("tools.updates"))
                .icon(crate::design::icons::Icon::Download)
                .small(),
        )
        .clicked()
    {
        out.push(Action::Go(Page::Superba(SuperbaSection::Updates)));
    }
    picked
}
