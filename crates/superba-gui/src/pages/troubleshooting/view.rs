//! The demo's three screens, using the application's own components and palette.

use egui::{ScrollArea, Ui, vec2};
use superba_core::*;

use super::{Repair, Run, Screen, State};
use crate::design::icons::{self, Icon};
use crate::design::widgets::{Button, Card, Status, StatusDot, Text, dialog, dialog_body};
use crate::pages::{Action, Cx};

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
    ui.horizontal(|ui| {
        ui.label(Text::page_title(
            t(
                cx,
                match state.screen {
                    Screen::Select => "title",
                    Screen::Workbench => "workbench",
                    Screen::Result => "result",
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
    ScrollArea::vertical()
        .id_salt(("troubleshooting", state.screen as u8))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
            match state.screen {
                Screen::Select => select(ui, cx, state, out),
                Screen::Workbench => workbench(ui, cx, state),
                Screen::Result => result(ui, cx, state, out),
            }
            ui.add_space(24.0);
        });
    if state.confirming_reinitialize {
        confirm(ui, cx, state, out);
    }
}

fn select(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    Card::new().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(Text::title(t(cx, "environment"), cx.pal));
            if ui
                .add(
                    Button::primary(t(cx, "all"))
                        .icon(Icon::Diagnostics)
                        .enabled(!cx.busy && !state.busy()),
                )
                .clicked()
            {
                state.start(TroubleshootingIssue::All, cx, out);
            }
        });
        ui.add_space(12.0);
        let overview = cx.data.overview.value.as_ref();
        let facts = [
            (
                "troubleshooting.fact.codex",
                overview.map(|o| {
                    if o.app.session.is_some() {
                        t(cx, "running")
                    } else {
                        t(cx, "not_running")
                    }
                }),
            ),
            (
                "troubleshooting.fact.provider",
                cx.data.active_profile().map(|p| p.name.clone()),
            ),
            (
                "troubleshooting.fact.config",
                overview.and_then(|o| o.config.as_ref()).map(|s| {
                    t(
                        cx,
                        if s.parse_error.is_some() {
                            "needs_check"
                        } else {
                            "readable"
                        },
                    )
                }),
            ),
        ];
        ui.horizontal_wrapped(|ui| {
            for (key, value) in facts {
                ui.vertical(|ui| {
                    ui.set_min_width(150.0);
                    ui.label(Text::caption(t(cx, key), cx.pal));
                    ui.label(Text::strong(
                        value.unwrap_or_else(|| t(cx, "unknown")),
                        cx.pal,
                    ));
                });
                ui.add_space(24.0);
            }
        });
    });
    if ui.available_width() >= 960.0 {
        let width = ui.available_width();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width((width - 20.0) * 0.68);
                cards(ui, cx, state, out);
            });
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.set_width((width - 20.0) * 0.32);
                aside(ui, cx, state);
            });
        });
    } else {
        cards(ui, cx, state, out);
        aside(ui, cx, state);
    }
}

fn cards(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(Text::title(t(cx, "common"), cx.pal));
        for (i, key) in [
            "troubleshooting.filter.all",
            "troubleshooting.filter.connection",
            "troubleshooting.filter.config",
            "troubleshooting.filter.session",
        ]
        .iter()
        .enumerate()
        {
            if ui.selectable_label(state.filter == i, t(cx, key)).clicked() {
                state.filter = i;
            }
        }
    });
    ui.add_space(12.0);
    let issues: Vec<_> = TroubleshootingIssue::CARDS
        .into_iter()
        .filter(|issue| match state.filter {
            1 => matches!(
                issue,
                TroubleshootingIssue::Request
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
    let columns = if ui.available_width() >= 540.0 { 2 } else { 1 };
    let width = (ui.available_width() - 16.0 * (columns - 1) as f32) / columns as f32;
    for row in issues.chunks(columns) {
        ui.horizontal_top(|ui| {
            for issue in row {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    Card::new().show(ui, |ui| {
                        ui.set_min_height(154.0);
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
                        let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), egui::Sense::hover());
                        icons::paint(ui.painter(), r, icon, cx.pal.text_muted);
                        ui.add(egui::Label::new(Text::strong(title(cx, *issue), cx.pal)).wrap());
                        body(ui, cx, &format!("desc.{}", issue.id()));
                        ui.add_space(6.0);
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
        ui.add_space(12.0);
    }
}

fn aside(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    Card::new().show(ui, |ui| {
        ui.label(Text::title(t(cx, "how"), cx.pal));
        for (n, key) in [
            "troubleshooting.step.choose",
            "troubleshooting.step.check",
            "troubleshooting.step.confirm",
        ]
        .iter()
        .enumerate()
        {
            ui.add_space(12.0);
            ui.horizontal_top(|ui| {
                ui.label(Text::mono(format!("{:02}", n + 1), cx.pal));
                ui.add(egui::Label::new(Text::body(t(cx, key), cx.pal)).wrap());
            });
        }
    });
    ui.add_space(16.0);
    Card::new().show(ui, |ui| {
        ui.label(Text::title(t(cx, "history"), cx.pal));
        if state.history.is_empty() {
            body(ui, cx, "history_empty");
        }
        let mut chosen = None;
        for run in &state.history {
            let label = format!(
                "{} · {}",
                title(cx, run.issue),
                t(
                    cx,
                    match run.resolved {
                        Some(true) => "confirmed",
                        Some(false) => "unresolved",
                        None => "unconfirmed",
                    }
                )
            );
            if ui
                .add(Button::ghost(label).small().enabled(!state.busy()))
                .clicked()
            {
                chosen = Some(run.clone());
            }
        }
        if let Some(run) = chosen {
            state.error = run.error.clone();
            state.run = Some(run);
            state.screen = Screen::Result;
        }
    });
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
    let heading = title(cx, run.issue);
    if ui.available_width() >= 860.0 {
        let width = ui.available_width();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width((width - 16.0) * 0.61);
                workbench_progress(ui, cx, state, &run, &progress, &heading);
            });
            ui.vertical(|ui| {
                ui.set_width((width - 16.0) * 0.39);
                workbench_log(ui, cx, &progress);
            });
        });
    } else {
        workbench_progress(ui, cx, state, &run, &progress, &heading);
        workbench_log(ui, cx, &progress);
    }
}

fn workbench_progress(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    run: &Run,
    progress: &[TroubleshootingCheck],
    heading: &str,
) {
    Card::new().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(Text::title(heading, cx.pal));
        });
        ui.add_space(12.0);
        body(
            ui,
            cx,
            if state.pending.is_some() {
                "waiting_flow"
            } else {
                "checking"
            },
        );
        ui.add_space(16.0);
        for (index, key) in [
            "troubleshooting.stage.config",
            "troubleshooting.stage.backend",
            "troubleshooting.stage.repair",
            "troubleshooting.stage.verify",
        ]
        .iter()
        .enumerate()
        {
            let current = if state.pending.is_some() {
                2
            } else if !run.repairs.is_empty() {
                3
            } else if progress
                .iter()
                .any(|c| c.id.starts_with("backend:") || c.id.starts_with("models:"))
            {
                1
            } else {
                0
            };
            ui.horizontal_top(|ui| {
                ui.add(StatusDot::new(if index < current {
                    Status::Ok
                } else if index == current {
                    Status::Warn
                } else {
                    Status::Idle
                }));
                ui.label(Text::strong(
                    format!("{:02}  {}", index + 1, t(cx, key)),
                    cx.pal,
                ));
            });
            ui.add_space(10.0);
        }
        if state.pending.is_none() && ui.add(Button::new(t(cx, "cancel"))).clicked() {
            state.cancel();
        }
    });
}

fn workbench_log(ui: &mut Ui, cx: &Cx<'_>, progress: &[TroubleshootingCheck]) {
    Card::new().show(ui, |ui| {
        ui.label(Text::title(t(cx, "live_log"), cx.pal));
        if progress.is_empty() {
            body(ui, cx, "preparing");
        }
        for item in progress {
            check_row(ui, cx, item);
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

fn result(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(run) = state.run.clone() else {
        return;
    };
    let (s, label) = outcome(&run, cx);
    Card::new().show(ui, |ui| {
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
        if !run.repairs.is_empty() {
            ui.add_space(12.0);
            for repair in &run.repairs {
                ui.add(egui::Label::new(Text::body(cx.tr.text(repair), cx.pal)).wrap());
            }
        }
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::primary(t(cx, "resolved"))).clicked() {
                if let Some(run) = state.run.as_mut() {
                    run.resolved = Some(true);
                }
                state.remember();
            }
            if ui.add(Button::new(t(cx, "still_broken"))).clicked() {
                if let Some(run) = state.run.as_mut() {
                    run.resolved = Some(false);
                }
                state.remember();
            }
            if ui
                .add(Button::ghost(t(cx, "retry")).icon(Icon::Refresh))
                .clicked()
            {
                state.start(run.issue, cx, out);
            }
        });
    });
    if ui.available_width() >= 860.0 {
        let width = ui.available_width();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width((width - 16.0) * 0.61);
                report(ui, cx, &run, out);
            });
            ui.vertical(|ui| {
                ui.set_width((width - 16.0) * 0.39);
                super::guidance::show(ui, cx, state, out);
            });
        });
    } else {
        super::guidance::show(ui, cx, state, out);
        report(ui, cx, &run, out);
    }
}

fn check_row(ui: &mut Ui, cx: &Cx<'_>, check: &TroubleshootingCheck) {
    ui.horizontal_top(|ui| {
        ui.add(StatusDot::new(status(check.status)));
        ui.vertical(|ui| {
            ui.add(egui::Label::new(Text::strong(cx.tr.text(&check.title), cx.pal)).wrap());
            ui.add(egui::Label::new(Text::small(cx.tr.text(&check.detail), cx.pal)).wrap());
        });
    });
    ui.add_space(8.0);
}

fn report(ui: &mut Ui, cx: &Cx<'_>, run: &Run, out: &mut Vec<Action>) {
    Card::new().show(ui, |ui| {
        ui.label(Text::title(t(cx, "details"), cx.pal));
        if run.checks.is_empty() {
            body(ui, cx, "text_only");
        }
        // Keep the report's latest observations concise; the copy includes all
        // earlier checks and the separate recovery record.
        if let Some(report) = &run.report {
            for item in &report.checks {
                check_row(ui, cx, item);
            }
        }
        if ui
            .add(Button::new(t(cx, "copy")).icon(Icon::Copy))
            .clicked()
        {
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
            out.push(Action::Copy(lines.join("\n")));
        }
    });
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
