//! The demo's three screens, using the application's own components and palette.

use egui::{ScrollArea, Ui, vec2};
use superba_core::*;

use super::{Repair, Run, Screen, State};
use crate::design::icons::{self, Icon};
use crate::design::widgets::{
    Button, Card, Disclosure, Segmented, Status, StatusDot, Text, dialog, dialog_body,
};
use crate::pages::{Action, Cx, Page};

pub(super) fn t(cx: &Cx<'_>, key: &str) -> String {
    if key.starts_with("troubleshooting.") {
        cx.tr.t(key)
    } else {
        cx.tr.t(&format!("troubleshooting.{key}"))
    }
}
fn title(cx: &Cx<'_>, issue: TroubleshootingIssue) -> String {
    t(cx, &format!("issue.{}", issue.id()))
}
pub(super) fn body(ui: &mut Ui, cx: &Cx<'_>, key: &str) {
    ui.add(egui::Label::new(Text::small(t(cx, key), cx.pal)).wrap());
}

fn status(value: TroubleshootingStatus) -> Status {
    match value {
        TroubleshootingStatus::Passed => Status::Ok,
        TroubleshootingStatus::Warning => Status::Warn,
        TroubleshootingStatus::Failed => Status::Error,
        TroubleshootingStatus::Unknown => Status::Idle,
    }
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if state.screen == Screen::Complete {
        complete(ui, cx, state, out);
        return;
    }
    ui.horizontal(|ui| {
        ui.label(Text::page_title(
            t(
                cx,
                match state.screen {
                    Screen::Select => "title",
                    Screen::Workbench => "workbench",
                    Screen::Result => "result",
                    Screen::Next => "next",
                    Screen::Complete => "complete",
                },
            ),
            cx.pal,
        ));
        if state.screen != Screen::Select
            && state.pending.is_none()
            && ui.add(Button::ghost(t(cx, "back")).small()).clicked()
        {
            state.cancel();
        }
    });
    ui.add_space(16.0);
    if state.screen == Screen::Result {
        result_header(ui, cx, state, out);
        ui.add_space(16.0);
    }
    ScrollArea::vertical()
        .id_salt(("troubleshooting", state.screen as u8))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
            match state.screen {
                Screen::Select => select(ui, cx, state, out),
                Screen::Workbench => workbench(ui, cx, state),
                Screen::Result => {
                    if let Some(run) = state.run.clone() {
                        super::guidance::model_restart(ui, cx, state, &run, out);
                        report(ui, cx, state, &run, out);
                    }
                }
                Screen::Next => super::guidance::show(ui, cx, state, out),
                Screen::Complete => {}
            }
            if state.simple_mode && state.screen != Screen::Complete {
                super::guidance::rollback_bar(ui, cx, out);
            }
            ui.add_space(24.0);
        });
    if state.confirming_reinitialize {
        confirm(ui, cx, state, out);
    }
    if state.confirming_cleanup {
        confirm_cleanup(ui, cx, state, out);
    }
}

fn complete(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let now = ui.input(|i| i.time);
    let started = *state.completed_at.get_or_insert(now);
    let elapsed = now - started;
    if elapsed >= 5.35 {
        state.completed_at = None;
        state.screen = Screen::Select;
        out.push(Action::Go(Page::Overview));
        return;
    }
    ui.scope(|ui| {
        ui.set_opacity((1.0 - ((elapsed - 5.0) / 0.35).clamp(0.0, 1.0)) as f32);
        let area = ui.available_rect_before_wrap();
        let width = area
            .width()
            .min(440.0)
            .min((area.height() - 100.0).max(120.0) * 4.0 / 3.0);
        let scene = egui::Rect::from_center_size(
            area.center() - vec2(0.0, 40.0),
            vec2(width, width * 0.75),
        );
        super::artwork::paint(ui, scene, &mut state.celebration, true, state.reduce_motion);
        let label = egui::Rect::from_min_max(
            egui::pos2(area.left(), scene.bottom() + 8.0),
            egui::pos2(area.right(), scene.bottom() + 60.0),
        );
        ui.put(
            label,
            egui::Label::new(Text::page_title(t(cx, "happy_coding"), cx.pal)),
        );
        let footer =
            egui::Rect::from_min_max(egui::pos2(area.left(), area.bottom() - 48.0), area.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(footer), |ui| {
            let remaining = (5.0 - elapsed).max(0.0);
            ui.add(
                egui::ProgressBar::new((remaining / 5.0) as f32)
                    .desired_width(ui.available_width())
                    .text(cx.tr.f(
                        "troubleshooting.return_countdown",
                        &[("seconds", &remaining.ceil())],
                    )),
            );
        });
    });
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(if elapsed < 5.0 {
            100
        } else {
            16
        }));
}

fn select(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    Card::new().padding(12).show(ui, |ui| {
        ui.scope_builder(egui::UiBuilder::new(), |ui| {
            ui.spacing_mut().item_spacing = vec2(12.0, 8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(Text::title(t(cx, "environment"), cx.pal));
                if ui
                    .add(
                        Button::primary(t(cx, "all"))
                            .icon(Icon::Diagnostics)
                            .small()
                            .enabled(!cx.busy && !state.busy()),
                    )
                    .clicked()
                {
                    state.start(TroubleshootingIssue::All, cx, out);
                }
            });
            let overview = cx.data.overview.value.as_ref();
            let facts = [
                (
                    "troubleshooting.fact.codex",
                    overview.map(|o| {
                        if o.app.session.is_some() {
                            (t(cx, "running"), Status::Ok)
                        } else {
                            (t(cx, "not_running"), Status::Warn)
                        }
                    }),
                ),
                (
                    "troubleshooting.fact.provider",
                    cx.data
                        .active_profile()
                        .map(|p| (p.name.clone(), Status::Ok)),
                ),
                (
                    "troubleshooting.fact.config",
                    overview.and_then(|o| o.config.as_ref()).map(|s| {
                        if s.parse_error.is_some() {
                            (t(cx, "needs_check"), Status::Error)
                        } else {
                            (t(cx, "readable"), Status::Ok)
                        }
                    }),
                ),
            ];
            ui.horizontal_wrapped(|ui| {
                for (key, value) in facts {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(Text::caption(t(cx, key), cx.pal));
                        let (label, tone) =
                            value.unwrap_or_else(|| (t(cx, "unknown"), Status::Idle));
                        let color = cx.pal.status(tone);
                        egui::Frame::new()
                            .fill(cx.pal.tint(color))
                            .corner_radius(6)
                            .inner_margin(egui::Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(Text::caption(&label, cx.pal).color(color))
                                        .truncate(),
                                )
                                .on_hover_text(label);
                            });
                    });
                }
            });
        });
    });
    cards(ui, cx, state, out);
}

fn cards(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(Text::title(t(cx, "common"), cx.pal));
        let mut tabs = Segmented::new("troubleshooting_filter", &mut state.filter).small();
        for (i, key) in [
            "troubleshooting.filter.all",
            "troubleshooting.filter.connection",
            "troubleshooting.filter.config",
            "troubleshooting.filter.session",
        ]
        .iter()
        .enumerate()
        {
            tabs = tabs.option(i, t(cx, key));
        }
        tabs.show(ui);
    });
    let issues: Vec<_> = TroubleshootingIssue::CARDS
        .into_iter()
        .filter(|issue| match state.filter {
            1 => matches!(
                issue,
                TroubleshootingIssue::Request
                    | TroubleshootingIssue::Stability
                    | TroubleshootingIssue::Server
                    | TroubleshootingIssue::WebSocket
            ),
            2 => matches!(
                issue,
                TroubleshootingIssue::Speed | TroubleshootingIssue::Models
            ),
            3 => matches!(
                issue,
                TroubleshootingIssue::Policy | TroubleshootingIssue::Sessions
            ),
            _ => true,
        })
        .collect();
    let columns = if ui.available_width() >= 900.0 {
        3
    } else if ui.available_width() >= 520.0 {
        2
    } else {
        1
    };
    let width = (ui.available_width() - 10.0 * (columns - 1) as f32) / columns as f32;
    ui.spacing_mut().item_spacing.y = 10.0;
    for row in issues.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for issue in row {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    Card::new().padding(14).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.set_min_height(86.0);
                        let icon = match issue {
                            TroubleshootingIssue::Models => Icon::Model,
                            TroubleshootingIssue::Sessions | TroubleshootingIssue::Policy => {
                                Icon::Chat
                            }
                            TroubleshootingIssue::Speed => Icon::Bolt,
                            TroubleshootingIssue::WebSocket => Icon::Lines,
                            TroubleshootingIssue::Other => Icon::Help,
                            _ => Icon::Server,
                        };
                        ui.horizontal(|ui| {
                            let (r, _) =
                                ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
                            icons::paint(ui.painter(), r, icon, cx.pal.text_muted);
                            let heading = title(cx, *issue);
                            ui.add(egui::Label::new(Text::strong(&heading, cx.pal)).truncate())
                                .on_hover_text(heading);
                        });
                        let description = t(cx, &format!("desc.{}", issue.id()));
                        ui.add(egui::Label::new(Text::small(&description, cx.pal)).truncate())
                            .on_hover_text(description);
                        if ui
                            .add(
                                Button::ghost(t(
                                    cx,
                                    if matches!(
                                        issue,
                                        TroubleshootingIssue::Policy
                                            | TroubleshootingIssue::Server
                                            | TroubleshootingIssue::Other
                                    ) {
                                        "view_advice"
                                    } else {
                                        "start"
                                    },
                                ))
                                .small()
                                .icon(Icon::ArrowRight)
                                .enabled(!cx.busy && !state.busy()),
                            )
                            .clicked()
                        {
                            state.start(*issue, cx, out);
                        }
                    });
                });
            }
        });
    }
}

fn workbench(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(run) = state.run.clone() else {
        return;
    };
    let progress = state
        .inspection
        .as_ref()
        .and_then(|r| r.progress.lock().ok().map(|p| p.clone()))
        .unwrap_or_default();
    workbench_progress(ui, cx, state, &run, &progress, &title(cx, run.issue));
}

fn workbench_progress(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    run: &Run,
    progress: &super::Progress,
    heading: &str,
) {
    Card::new().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        let art = (state.pending.is_none()
            && run.repairs.is_empty()
            && state.inspection.as_ref().is_some_and(|i| i.automatic))
        .then(|| {
            egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().right() - 112.0, ui.cursor().top() - 12.0),
                vec2(112.0, 84.0),
            )
        });
        if let Some(art) = art {
            super::artwork::paint(ui, art, &mut state.art, false, state.reduce_motion);
        }
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
                ui.cursor().min,
                egui::pos2(
                    art.map_or(ui.max_rect().right(), |art| art.left() - 8.0),
                    ui.max_rect().bottom(),
                ),
            )),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.add(
                        egui::Label::new(Text::page_title(
                            cx.tr
                                .f("troubleshooting.diagnosing", &[("issue", &heading)]),
                            cx.pal,
                        ))
                        .wrap(),
                    );
                });
                ui.add_space(12.0);
                body(
                    ui,
                    cx,
                    if state.pending.is_some() {
                        "waiting_flow"
                    } else if run.issue == TroubleshootingIssue::Stability && progress.backend {
                        "measuring"
                    } else {
                        "checking"
                    },
                );
            },
        );
        ui.add_space(art.map_or(16.0, |art| (art.bottom() - ui.cursor().top()).max(16.0)));
        let current = if state.pending.is_some() {
            2
        } else if !run.repairs.is_empty() {
            3
        } else if progress.backend {
            1
        } else {
            0
        };
        let changed = state.drawer_stage != Some((run.id, current));
        state.drawer_stage = Some((run.id, current));
        for (index, key) in [
            "troubleshooting.stage.config",
            "troubleshooting.stage.backend",
            "troubleshooting.stage.repair",
            "troubleshooting.stage.verify",
        ]
        .iter()
        .enumerate()
        {
            Disclosure::new(
                ("troubleshooting-stage", run.id, index),
                format!("{:02}  {}", index + 1, t(cx, key)),
            )
            .section()
            .open(changed.then_some(index == current))
            .max_body_height(
                (ui.clip_rect().bottom() - ui.cursor().top() - (4 - index) as f32 * 50.0 - 64.0)
                    .max(144.0),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let previous = run
                    .report
                    .as_ref()
                    .map(|r| r.checks.as_slice())
                    .unwrap_or_default();
                let checks = if current >= 2 && index < 2 {
                    previous
                } else {
                    &progress.checks
                };
                let mut shown = false;
                if index == 2 {
                    for repair in &run.repairs {
                        ui.add(egui::Label::new(Text::small(cx.tr.text(repair), cx.pal)).wrap());
                        shown = true;
                    }
                } else {
                    for item in checks {
                        let backend = [
                            "models:",
                            "backend:",
                            "websocket:",
                            "speed:",
                            "stability:",
                            "router",
                        ]
                        .iter()
                        .any(|prefix| item.id.starts_with(prefix));
                        if (index < 2 && index == usize::from(backend))
                            || (index == 3 && current == 3)
                        {
                            if !item.id.starts_with("configuration:ignored:") {
                                check_row(ui, cx, item, Some(state), checks);
                            }
                            shown = true;
                        }
                    }
                }
                if !shown && index == current {
                    body(
                        ui,
                        cx,
                        if index == 2 {
                            "waiting_flow"
                        } else {
                            "preparing"
                        },
                    );
                }
            });
            ui.add_space(10.0);
        }
        if state.pending.is_none() && ui.add(Button::new(t(cx, "cancel"))).clicked() {
            state.cancel();
        }
    });
}

fn outcome(run: &Run, cx: &Cx<'_>) -> (Status, String) {
    if run.error.is_some() {
        return (Status::Error, t(cx, "run_failed"));
    }
    if let Some(resolved) = run.resolved {
        return (
            if resolved { Status::Ok } else { Status::Warn },
            t(cx, if resolved { "confirmed" } else { "unresolved" }),
        );
    }
    if run.checks.is_empty() {
        return (Status::Idle, t(cx, "advice"));
    }
    if run
        .report
        .as_ref()
        .map_or(run.checks.as_slice(), |r| r.checks.as_slice())
        .iter()
        .all(|c| c.status == TroubleshootingStatus::Passed)
    {
        (Status::Ok, t(cx, "checks_passed"))
    } else {
        (Status::Warn, t(cx, "checks_attention"))
    }
}

fn result_header(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(run) = state.run.clone() else {
        return;
    };
    let (s, label) = outcome(&run, cx);
    Card::new().padding(12).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.add(StatusDot::new(s));
            ui.vertical(|ui| {
                ui.label(Text::title(label, cx.pal));
                ui.add(egui::Label::new(Text::small(title(cx, run.issue), cx.pal)).wrap());
            });
        });
        if let Some(error) = &state.error {
            ui.add(
                egui::Label::new(Text::small(cx.tr.text(error), cx.pal).color(cx.pal.red)).wrap(),
            );
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::primary(t(cx, "resolved")).enabled(!state.busy()))
                .clicked()
            {
                if let Some(run) = state.run.as_mut() {
                    run.resolved = Some(true);
                }
                state.remember();
                state.completed_at = Some(ui.input(|i| i.time));
                state.screen = Screen::Complete;
            }
            if ui
                .add(Button::new(t(cx, "still_broken")).enabled(!state.busy()))
                .clicked()
            {
                if let Some(run) = state.run.as_mut() {
                    run.resolved = Some(false);
                }
                state.remember();
                state.screen = Screen::Next;
            }
            if ui
                .add(
                    Button::ghost(t(cx, "retry"))
                        .icon(Icon::Refresh)
                        .enabled(!state.busy() && !cx.busy),
                )
                .clicked()
            {
                state.start(run.issue, cx, out);
            }
        });
    });
}

fn check_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    check: &TroubleshootingCheck,
    mut state: Option<&mut State>,
    related: &[TroubleshootingCheck],
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
        ui.add(StatusDot::new(status(check.status)));
        ui.vertical(|ui| {
            ui.add(egui::Label::new(Text::strong(cx.tr.text(&check.title), cx.pal)).wrap());
            ui.add(egui::Label::new(Text::small(cx.tr.text(&check.detail), cx.pal)).wrap());
            if check.retry.is_some()
                && let Some(state) = state.as_deref_mut()
            {
                let retrying = state
                    .inspection
                    .as_ref()
                    .and_then(|i| i.retry_id.as_deref())
                    == Some(check.id.as_str());
                if retrying {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(Text::small(t(cx, "retrying_check"), cx.pal));
                    });
                } else if ui
                    .add(
                        Button::ghost(t(cx, "retry_check"))
                            .small()
                            .icon(Icon::Refresh)
                            .enabled(!state.busy() && !cx.busy),
                    )
                    .clicked()
                {
                    state.retry(&check.id, cx);
                }
            }
            if check.id == "configuration" {
                let ignored: Vec<_> = related
                    .iter()
                    .filter(|c| c.id.starts_with("configuration:ignored:"))
                    .collect();
                if !ignored.is_empty() {
                    let enabled = state.as_ref().is_some_and(|s| {
                        !s.busy()
                            && !cx.busy
                            && s.run
                                .as_ref()
                                .and_then(|r| r.report.as_ref())
                                .is_some_and(|r| r.cleanup.is_some())
                    });
                    let mut clean = false;
                    Disclosure::new("ignored-config", t(cx, "ignored_config"))
                        .max_body_height(
                            (ui.clip_rect().bottom() - ui.cursor().top() - 140.0).max(144.0),
                        )
                        .show_with_header(
                            ui,
                            |ui, line| {
                                ui.scope_builder(
                                    egui::UiBuilder::new()
                                        .max_rect(line)
                                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                                    |ui| {
                                        clean = ui
                                            .add(
                                                Button::new(t(cx, "cleanup"))
                                                    .small()
                                                    .enabled(enabled),
                                            )
                                            .on_disabled_hover_text(t(
                                                cx,
                                                if cx.busy
                                                    || state.as_ref().is_some_and(|s| s.busy())
                                                {
                                                    "cleanup_busy"
                                                } else {
                                                    "cleanup_unavailable"
                                                },
                                            ))
                                            .clicked();
                                    },
                                );
                            },
                            |ui| {
                                for item in ignored {
                                    check_row(ui, cx, item, None, &[]);
                                }
                            },
                        );
                    if clean && let Some(state) = state.as_deref_mut() {
                        state.confirming_cleanup = true;
                    }
                }
            }
        });
    });
    ui.add_space(4.0);
}

fn report(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, run: &Run, out: &mut Vec<Action>) {
    Card::new().padding(12).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
        ui.label(Text::title(t(cx, "details"), cx.pal));
        if run.checks.is_empty() {
            body(ui, cx, "text_only");
        }
        // Keep the report's latest observations concise; the copy includes all
        // earlier checks and the separate recovery record.
        if let Some(report) = &run.report {
            for (index, key) in ["stage.config", "stage.backend"].into_iter().enumerate() {
                let checks: Vec<_> = report
                    .checks
                    .iter()
                    .filter(|check| {
                        let backend = [
                            "models:",
                            "backend:",
                            "websocket:",
                            "speed:",
                            "stability:",
                            "router",
                        ]
                        .iter()
                        .any(|prefix| check.id.starts_with(prefix));
                        usize::from(backend) == index
                    })
                    .collect();
                if checks.is_empty() {
                    continue;
                }
                let problem = checks
                    .iter()
                    .any(|c| c.status != TroubleshootingStatus::Passed);
                let mut drawer = Disclosure::new(
                    ("result-stage", run.id, state.result_revision, index),
                    t(cx, key),
                )
                .section()
                .default_open(problem);
                if !problem {
                    drawer = drawer.status(t(cx, "stage_ok"), Status::Ok);
                }
                drawer.show(ui, |ui| {
                    for item in checks
                        .iter()
                        .filter(|c| !c.id.starts_with("configuration:ignored:"))
                    {
                        ui.push_id(&item.id, |ui| {
                            check_row(ui, cx, item, Some(state), &report.checks)
                        });
                    }
                });
            }
        }
        if !run.repairs.is_empty() {
            Disclosure::new(("result-repairs", run.id), t(cx, "stage.repair"))
                .section()
                .status(t(cx, "stage_ok"), Status::Ok)
                .show(ui, |ui| {
                    for repair in &run.repairs {
                        ui.add(egui::Label::new(Text::small(cx.tr.text(repair), cx.pal)).wrap());
                    }
                });
        }
        if ui
            .add(Button::new(t(cx, "copy")).icon(Icon::Copy))
            .clicked()
        {
            out.push(Action::Copy(report_text(cx, run)));
        }
    });
}

pub(super) fn report_text(cx: &Cx<'_>, run: &Run) -> String {
    let mut lines = vec![
        format!("Superba · {}", title(cx, run.issue)),
        outcome(run, cx).1,
    ];
    if let Some(report) = &run.report {
        lines.push(crate::pages::clock_time(report.checked_at, true));
    }
    for item in &run.checks {
        lines.push(format!(
            "{}: {}",
            cx.tr.text(&item.title),
            cx.tr.text(&item.detail)
        ));
    }
    lines.extend(run.repairs.iter().map(|r| cx.tr.text(r)));
    if let Some(error) = &run.error {
        lines.push(cx.tr.text(error));
    }
    lines.join("\n")
}

fn confirm_cleanup(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(cleanup) = state
        .run
        .as_ref()
        .and_then(|r| r.report.as_ref())
        .and_then(|r| r.cleanup.clone())
    else {
        state.confirming_cleanup = false;
        return;
    };
    let mut apply = false;
    let mut cancel = false;
    let response = dialog(
        ui.ctx(),
        egui::Id::unique("troubleshooting-cleanup"),
        540.0,
        |ui| {
            ui.label(Text::title(t(cx, "cleanup"), cx.pal));
            dialog_body(ui, |ui| {
                body(ui, cx, "cleanup_warning");
                for entry in &cleanup.entries {
                    ui.add(egui::Label::new(Text::mono(&entry.path, cx.pal)).wrap());
                }
            });
            ui.horizontal_wrapped(|ui| {
                apply = ui
                    .add(
                        Button::danger(t(cx, "cleanup_confirm")).enabled(!state.busy() && !cx.busy),
                    )
                    .clicked();
                cancel = ui.add(Button::ghost(t(cx, "cancel"))).clicked();
            });
        },
    );
    if apply {
        state.confirming_cleanup = false;
        state.queue(Repair::Cleanup(cleanup), out);
    } else if cancel || response.should_close() {
        state.confirming_cleanup = false;
    }
}

fn confirm(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let mut apply = false;
    let mut cancel = false;
    let response = dialog(
        ui.ctx(),
        egui::Id::unique("troubleshooting-reinitialize"),
        540.0,
        |ui| {
            ui.label(Text::title(t(cx, "reinitialize"), cx.pal));
            dialog_body(ui, |ui| {
                body(ui, cx, "reinitialize_warning");
            });
            ui.horizontal_wrapped(|ui| {
                apply = ui
                    .add(Button::danger(t(cx, "reinitialize_confirm")))
                    .clicked();
                cancel = ui.add(Button::ghost(t(cx, "cancel"))).clicked();
            });
        },
    );
    if apply {
        state.confirming_reinitialize = false;
        state.queue(Repair::Reinitialize, out);
    } else if cancel || response.should_close() {
        state.confirming_reinitialize = false;
    }
}
