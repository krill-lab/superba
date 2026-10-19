//! Seven neutral tool identities, with keyboard selection and spoken status.
use super::{Selection, State};
use crate::design::widgets::{Button, MonogramTile, Status, StatusDot, Text};
use crate::design::{
    a11y,
    fonts::{Weight, font},
    tokens::{radius, size, text},
};
use crate::pages::{Action, Cx, Page, SuperbaSection};
use superba_core::harness::{HarnessId, HarnessRoute};

/// A row with a complete accessibility description.
#[derive(Clone, Debug)]
pub struct Row {
    /// Destination.
    pub selection: Selection,
    /// Provider, models and connection summary.
    pub summary: String,
    /// Status dot.
    pub status: Status,
}
/// Build rows before splitting the page into independently borrowed panes.
pub fn rows(cx: &Cx<'_>, state: &State) -> Vec<Row> {
    let active = cx.data.active_profile();
    let mut rows = vec![
        Row {
            selection: Selection::Codex,
            summary: active
                .map(|p| format!("{} · {}", p.name, p.model.model.as_deref().unwrap_or("—")))
                .unwrap_or_else(|| cx.tr.t("tools.not_configured")),
            status: if active.is_some() {
                Status::Ok
            } else {
                Status::Idle
            },
        },
        Row {
            selection: Selection::Claude,
            summary: state
                .claude
                .as_ref()
                .and_then(|v| v.provider.as_ref())
                .and_then(|id| cx.data.profile(id))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| cx.tr.t("tools.not_configured")),
            status: state.claude.as_ref().map_or(Status::Idle, |v| {
                if !v.pending.is_empty() {
                    Status::Warn
                } else if v.applied.is_some() {
                    Status::Ok
                } else {
                    Status::Idle
                }
            }),
        },
    ];
    for h in HarnessId::ALL {
        let settings = state.settings(h);
        let summary = state.summaries.iter().find(|s| s.harness == h);
        let view = state.views.get(&h);
        let follows_changed = settings.provider.is_none()
            && summary
                .is_some_and(|s| s.applied && s.applied_provider.as_ref() != active.map(|p| &p.id));
        let pending = follows_changed
            || state.drafts.contains_key(&h)
            || view.is_some_and(|v| !v.pending.is_empty());
        let bad = summary.is_some_and(|s| s.drift > 0)
            || view.is_some_and(|v| v.files.iter().any(|f| !f.status.writable()));
        let applied =
            summary.is_some_and(|s| s.applied) || view.is_some_and(|v| v.applied.is_some());
        let status = if pending || bad {
            Status::Warn
        } else if applied {
            Status::Ok
        } else {
            Status::Idle
        };
        let label = cx.tr.t(if bad {
            "tools.changed"
        } else if pending {
            "tools.pending_short"
        } else if applied {
            "tools.applied"
        } else {
            "tools.not_configured"
        });
        let provider = settings
            .provider
            .as_ref()
            .and_then(|id| cx.data.profile(id))
            .or(if settings.provider.is_none() {
                active
            } else {
                None
            });
        let route = cx.tr.t(if settings.route == HarnessRoute::Routed {
            "tools.routed"
        } else {
            "tools.direct"
        });
        let text = provider.map_or(label.clone(), |p| {
            format!("{} · {} · {}", p.name, route, label)
        });
        rows.push(Row {
            selection: Selection::Tool(h),
            summary: text,
            status,
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
            ui.add_space(if i == 0 { 0.0 } else { 12.0 });
            ui.label(Text::small(
                cx.tr.t(if i == 0 {
                    "tools.own_pages"
                } else {
                    "tools.other"
                }),
                cx.pal,
            ));
            ui.add_space(4.0);
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
        }
        if response.has_focus() {
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
        MonogramTile::new(row.selection.monogram())
            .paint_at(ui, egui::pos2(rect.left() + 23.0, rect.center().y));
        let left = rect.left() + 47.0;
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
        if response.has_focus() {
            crate::design::widgets::focus_ring(ui.painter(), rect, radius::ROW, cx.pal);
        }
    }
    ui.add_space(16.0);
    if ui
        .add(Button::ghost(cx.tr.t("tools.updates")).small())
        .on_hover_text(cx.tr.t("tools.updates"))
        .clicked()
    {
        out.push(Action::Go(Page::Superba(SuperbaSection::Updates)));
    }
    picked
}
