//! Decisions which cannot be inferred from a successful network probe.

use egui::{ScrollArea, Ui};
use superba_core::*;

use super::view::t;
use super::{Answer, Repair, Run, State};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Card, Text};
use crate::pages::{Action, Cx, Page};

fn body(ui: &mut Ui, cx: &Cx<'_>, key: &str) {
    if key.contains("advice.") {
        Card::new().padding(14).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.label(Text::strong("•", cx.pal));
                ui.add(egui::Label::new(Text::body(t(cx, key), cx.pal)).wrap());
            });
        });
    } else {
        super::view::body(ui, cx, key);
    }
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(run) = state.run.clone() else {
        return;
    };
    ui.scope(|ui| {
        ui.horizontal(|ui| {
            let art_height = 112.0;
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(128.0, art_height), egui::Sense::hover());
            super::artwork::advice(ui, rect, &mut state.advice_art);
            ui.add(egui::Label::new(Text::title(t(cx, "advice_intro"), cx.pal).size(24.0)).wrap());
        });
        use TroubleshootingIssue as Issue;
        match run.issue {
            Issue::Policy => {
                body(ui, cx, "troubleshooting.advice.policy");
                body(ui, cx, "troubleshooting.advice.fork");
            }
            Issue::Server => {
                body(ui, cx, "server_kind");
                let mut unauthorized = run.server_unauthorized;
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut unauthorized, Answer::Yes, t(cx, "server_401"));
                    ui.selectable_value(&mut unauthorized, Answer::No, t(cx, "server_other"));
                });
                if unauthorized != run.server_unauthorized {
                    if let Some(run) = state.run.as_mut() {
                        run.server_unauthorized = unauthorized;
                        run.isolated_server = false;
                        run.resolved = None;
                    }
                    state.remember();
                }
                if unauthorized == Answer::Yes {
                    body(ui, cx, "troubleshooting.advice.key_valid");
                } else if unauthorized == Answer::No && !run.isolated_server {
                    service_advice(ui, cx, out);
                    if ui.add(Button::new(t(cx, "server_others"))).clicked() {
                        state.error = None;
                        if let Some(run) = state.run.as_mut() {
                            run.resolved = Some(false);
                        }
                        state.remember();
                    }
                    if ui.add(Button::primary(t(cx, "server_only_me"))).clicked() {
                        if let Some(run) = state.run.as_mut() {
                            run.isolated_server = true;
                        }
                        state.inspect(cx, true, false, out);
                    }
                } else if unauthorized == Answer::No {
                    body(ui, cx, "troubleshooting.advice.request");
                }
            }
            Issue::Models => models(ui, cx, state, out),
            Issue::Sessions => {
                if let Some(report) = &run.report {
                    let scan_complete = report.checks.iter().any(|c| {
                        c.id == "sessions"
                            && matches!(
                                c.status,
                                TroubleshootingStatus::Passed | TroubleshootingStatus::Warning
                            )
                    });
                    for (provider, count) in &report.legacy_providers {
                        ui.label(Text::mono(format!("{provider} · {count}"), cx.pal));
                    }
                    if !report.legacy_providers.is_empty() && !run.migrated {
                        body(ui, cx, "troubleshooting.advice.migrate");
                        if ui
                            .add(Button::primary(t(cx, "migrate")).enabled(!cx.busy))
                            .clicked()
                        {
                            state.queue(Repair::Migrate, out);
                        }
                    } else if scan_complete {
                        body(ui, cx, "troubleshooting.advice.fork");
                    } else {
                        body(ui, cx, "troubleshooting.advice.sessions_unknown");
                    }
                } else {
                    body(ui, cx, "troubleshooting.advice.fork");
                }
            }
            Issue::Other => service_advice(ui, cx, out),
            Issue::Speed => {
                body(ui, cx, "troubleshooting.advice.speed");
                let mut selected = run.speed;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut selected, SpeedTier::Fast, "Fast");
                    ui.selectable_value(&mut selected, SpeedTier::Ultrafast, "UltraFast");
                });
                if selected != run.speed {
                    if let Some(run) = state.run.as_mut() {
                        run.speed = selected;
                    }
                    state.inspect(cx, true, true, out);
                }
            }
            Issue::WebSocket => body(ui, cx, "troubleshooting.advice.websocket"),
            Issue::Stability => {
                if let Some(report) = &run.report {
                    for check in report
                        .checks
                        .iter()
                        .filter(|c| matches!(c.id.as_str(), "stability:switch" | "stability:ws"))
                    {
                        Card::new().padding(14).show(ui, |ui| {
                            ui.add(
                                egui::Label::new(Text::body(cx.tr.text(&check.detail), cx.pal))
                                    .wrap(),
                            );
                        });
                    }
                }
                body(ui, cx, "troubleshooting.advice.proxy");
                body(ui, cx, "troubleshooting.advice.without_proxy");
            }
            Issue::All => body(ui, cx, "troubleshooting.advice.all"),
            _ => body(ui, cx, "troubleshooting.advice.request"),
        }
        if run.resolved == Some(false) {
            body(ui, cx, "troubleshooting.advice.unresolved");
            if matches!(run.issue, Issue::Sessions | Issue::Policy) {
                body(ui, cx, "troubleshooting.advice.fork");
            }
        }
        let report = run.report.as_ref();
        let model_recovery = run.issue != Issue::Models
            || (run.model_missing
                && run.new_model != Answer::Yes
                && run.added_today != Answer::Yes
                && run.new_model != Answer::Unanswered
                && run.added_today != Answer::Unanswered);
        let recovery = match run.issue {
            Issue::Other => true,
            Issue::Speed | Issue::WebSocket => {
                report.is_some_and(|r| r.config_ok) && run.restarted && run.resolved == Some(false)
            }
            Issue::Models => report.is_some_and(|r| r.config_ok) && run.model_missing,
            Issue::Server if run.server_unauthorized != Answer::No => false,
            Issue::Request | Issue::Server => {
                report.is_some_and(|r| r.config_ok)
                    && (run.resolved == Some(false)
                        || report.is_some_and(|r| {
                            r.checks
                                .iter()
                                .filter(|c| !c.id.starts_with("update:"))
                                .any(|c| c.status != TroubleshootingStatus::Passed)
                        }))
            }
            _ => false,
        };
        if recovery && model_recovery && !run.rebuilt {
            body(ui, cx, "troubleshooting.advice.reinitialize");
            let available = report.map_or_else(
                || {
                    cx.data
                        .active_profile()
                        .is_some_and(|p| p.builtin != Some(BuiltinProfile::Original))
                },
                |r| r.can_reinitialize,
            );
            if ui
                .add(Button::new(t(cx, "reinitialize")).enabled(available && !cx.busy))
                .clicked()
            {
                state.confirming_reinitialize = true;
            }
            if !available {
                body(ui, cx, "provider_missing");
            }
        }
        if run.issue == Issue::Other
            && ui
                .add(Button::new(t(cx, "official")).enabled(!cx.busy))
                .clicked()
        {
            state.queue(Repair::Official, out);
        }
        if report.is_some_and(|r| r.profile_id.is_none())
            && ui.add(Button::new(t(cx, "choose_provider"))).clicked()
        {
            out.push(Action::Go(Page::Providers));
        }
        ui.add_space(12.0);
        ui.label(Text::title(t(cx, "more_actions"), cx.pal));
        ui.horizontal_wrapped(|ui| {
            let editor = ui.add(Button::ghost(t(cx, "edit_config")).icon(Icon::Settings));
            let editor = if state.simple_mode {
                editor.on_hover_text(t(cx, "switch_full_mode"))
            } else {
                editor
            };
            if editor.clicked() {
                open_editor(out);
            }
            if ui
                .add(Button::ghost(t(cx, "copy_diagnostics")).icon(Icon::Copy))
                .clicked()
            {
                out.push(Action::CopyDiagnostics(super::view::report_text(cx, &run)));
            }
            if ui
                .add(Button::ghost(t(cx, "bug")).icon(Icon::External))
                .clicked()
            {
                out.push(Action::OpenUrl(format!(
                    "{}/issues/new",
                    crate::pages::superba::home::PROJECT_URL
                )));
            }
        });
    });
}

fn service_advice(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    body(ui, cx, "troubleshooting.advice.service");
    if ui
        .add(Button::new(t(cx, "status_page")).icon(Icon::External))
        .clicked()
    {
        out.push(Action::OpenUrl("https://status.openai.com/".into()));
    }
}

fn open_editor(out: &mut Vec<Action>) {
    out.push(Action::SetSimpleMode(false));
    out.push(Action::PageCommand {
        page: Page::Settings,
        command: "files:config@local".into(),
    });
}

pub(super) fn rollback_bar(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    Card::new().padding(12).show(ui, |ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), crate::design::tokens::size::CONTROL),
            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
            |ui| {
                ui.label(Text::body(t(cx, "rollback_prompt"), cx.pal));
                if ui
                    .add(Button::ghost(t(cx, "rollback_config")).icon(Icon::Restore))
                    .clicked()
                {
                    out.push(Action::SetSimpleMode(false));
                    out.push(Action::PageCommand {
                        page: Page::Superba(crate::pages::SuperbaSection::Data),
                        command: "focus:superba.backups.title".into(),
                    });
                }
            },
        );
    });
}

fn question(ui: &mut Ui, cx: &Cx<'_>, key: &str, value: &mut Answer) {
    body(ui, cx, key);
    ui.horizontal_wrapped(|ui| {
        for (answer, key) in [
            (Answer::Yes, "yes"),
            (Answer::No, "no"),
            (Answer::Unsure, "unsure"),
        ] {
            ui.selectable_value(value, answer, t(cx, key));
        }
    });
}

pub(super) fn model_restart(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    run: &Run,
    out: &mut Vec<Action>,
) {
    let ready = run.issue == TroubleshootingIssue::Models
        && !run.restarted
        && run.report.as_ref().is_some_and(|r| {
            !r.models.is_empty()
                && r.models.iter().all(|m| {
                    m.refreshed
                        && m.error.is_none()
                        && m.list
                            .as_ref()
                            .is_some_and(|list| list.sources.iter().all(|s| s.error.is_none()))
                })
        });
    if !ready {
        return;
    }
    Card::new().padding(12).show(ui, |ui| {
        ui.horizontal(|ui| {
            let restart = Button::new(t(cx, "restart_chatgpt"))
                .icon(Icon::Refresh)
                .enabled(!cx.busy && !state.busy());
            let width =
                (ui.available_width() - restart.desired_size(ui).x - ui.spacing().item_spacing.x)
                    .max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add(
                        egui::Label::new(Text::body(t(cx, "model_restart_prompt"), cx.pal)).wrap(),
                    );
                },
            );
            if ui.add(restart).clicked() {
                state.queue(Repair::Restart, out);
            }
        });
    });
}

fn models(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(run) = state.run.clone() else {
        return;
    };
    let Some(report) = run.report.as_ref() else {
        return;
    };
    model_restart(ui, cx, state, &run, out);
    if run.model_synced {
        body(ui, cx, "troubleshooting.advice.model_synced");
        return;
    }
    if run.model_missing {
        let mut first = run.new_model;
        let mut second = run.added_today;
        question(ui, cx, "model_new", &mut first);
        question(ui, cx, "model_today", &mut second);
        if let Some(run) = state.run.as_mut() {
            run.new_model = first;
            run.added_today = second;
        }
        if first == Answer::Yes || second == Answer::Yes {
            body(ui, cx, "troubleshooting.advice.model_wait");
            if ui.add(Button::primary(t(cx, "manual_model"))).clicked() {
                open_editor(out);
            }
        } else if first != Answer::Unanswered && second != Answer::Unanswered {
            if first == Answer::Unsure || second == Answer::Unsure {
                body(ui, cx, "troubleshooting.advice.model_unknown");
            }
            if !report.config_ok && ui.add(Button::primary(t(cx, "check_config"))).clicked() {
                state.inspect(cx, true, false, out);
            }
            if report.config_ok {
                body(ui, cx, "troubleshooting.advice.reinitialize");
            }
        }
        state.remember();
        return;
    }
    let refreshed = !report.models.is_empty() && report.models.iter().all(|m| m.refreshed);
    body(
        ui,
        cx,
        if refreshed {
            "model_confirm"
        } else {
            "model_refresh_failed"
        },
    );
    ui.add(
        egui::TextEdit::singleline(&mut state.model_query)
            .hint_text(t(cx, "model_search"))
            .desired_width(ui.available_width()),
    );
    let mut selected = None;
    for supplier in &report.models {
        ui.label(Text::strong(&supplier.name, cx.pal));
        if let Some(error) = &supplier.error {
            ui.add(egui::Label::new(Text::small(cx.tr.text(error), cx.pal)).wrap());
        }
        if let Some(list) = &supplier.list {
            ui.label(Text::caption(
                t(
                    cx,
                    if supplier.refreshed {
                        "model_upstream"
                    } else {
                        "model_cached"
                    },
                ),
                cx.pal,
            ));
            for source in &list.sources {
                if let Some(error) = &source.error {
                    ui.add(
                        egui::Label::new(Text::small(cx.tr.text(&error.to_text()), cx.pal)).wrap(),
                    );
                }
            }
            ScrollArea::vertical()
                .id_salt(("troubleshooting-models", &supplier.profile_id))
                .max_height(190.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let models: Vec<_> = list
                        .models
                        .iter()
                        .filter(|m| {
                            state.model_query.is_empty()
                                || m.slug
                                    .to_lowercase()
                                    .contains(&state.model_query.to_lowercase())
                        })
                        .collect();
                    let columns = ((ui.available_width() + 8.0) / 218.0).floor().max(1.0) as usize;
                    let width =
                        (ui.available_width() - (columns - 1) as f32 * 8.0) / columns as f32;
                    egui::Grid::new(("model-grid", &supplier.profile_id))
                        .num_columns(columns)
                        .spacing(egui::vec2(8.0, 4.0))
                        .min_col_width(width)
                        .max_col_width(width)
                        .striped(true)
                        .show(ui, |ui| {
                            for (index, model) in models.into_iter().enumerate() {
                                let on = run.model.as_deref() == Some(&model.slug)
                                    && run.model_provider.as_ref() == Some(&supplier.profile_id);
                                ui.add_enabled_ui(supplier.refreshed, |ui| {
                                    if ui
                                        .add_sized(
                                            [width, 32.0],
                                            egui::Button::selectable(
                                                on,
                                                Text::mono(&model.slug, cx.pal),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(&model.slug)
                                        .clicked()
                                    {
                                        selected =
                                            Some((supplier.profile_id.clone(), model.slug.clone()));
                                    }
                                });
                                if (index + 1) % columns == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
        }
    }
    if let Some((profile, model)) = selected {
        if let Some(run) = state.run.as_mut() {
            run.model = Some(model);
            run.model_provider = Some(profile);
        }
    }
    let selected_fresh = state
        .run
        .as_ref()
        .and_then(|r| r.model_provider.as_ref())
        .is_some_and(|id| {
            report
                .models
                .iter()
                .any(|m| &m.profile_id == id && m.refreshed)
        });
    if ui
        .add(Button::primary(t(cx, "model_found")).enabled(
            state.run.as_ref().is_some_and(|r| r.model.is_some()) && selected_fresh && !cx.busy,
        ))
        .clicked()
    {
        if let Some(run) = state.run.as_mut() {
            run.sync_model_requested = true;
        }
        state.inspect(cx, true, false, out);
    }
    if refreshed && ui.add(Button::new(t(cx, "model_missing"))).clicked() {
        if let Some(run) = state.run.as_mut() {
            run.model_missing = true;
            run.model = None;
        }
        state.remember();
    }
    if !refreshed && ui.add(Button::new(t(cx, "retry_refresh"))).clicked() {
        state.inspect(cx, false, true, out);
    }
}
