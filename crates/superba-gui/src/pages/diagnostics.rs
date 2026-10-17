//! Diagnostics (UX §3.10): doctor findings, the Codex processes of this
//! computer, environment facts and the activity log.

use egui::{Align, Layout, ScrollArea, Ui};
use superba_core::{CodexProcessKind, EnvObservation, FeedOrigin, Severity};

use super::{Action, Cx, Page, empty_state, error_state, kv, page_header, section};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Card, Status, StatusDot, Table, Text};

/// Draw the diagnostics page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    page_header(ui, cx, Page::Diagnostics, |ui| {
        if ui
            .add(
                Button::new(cx.tr.t("diag.run"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(cx.data.doctor.loading),
            )
            .clicked()
        {
            out.push(Action::RunDoctor);
        }
    });

    checks(ui, cx);
    ui.add_space(24.0);
    processes(ui, cx);
    ui.add_space(24.0);
    environment(ui, cx);
    ui.add_space(24.0);
    activity_log(ui, cx, out);
}

fn checks(ui: &mut Ui, cx: &Cx<'_>) {
    section(ui, cx, &cx.tr.t("diag.checks.title"), None);
    match (&cx.data.doctor.value, &cx.data.doctor.error) {
        (Some(findings), _) if !findings.is_empty() => {
            Card::new().flat().show(ui, |ui| {
                let n = findings.len();
                for (i, f) in findings.iter().enumerate() {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(20, 14))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                let status = match f.severity {
                                    Severity::Info => Status::Ok,
                                    Severity::Warning => Status::Warn,
                                    Severity::Danger => Status::Error,
                                };
                                let (r, _) = ui.allocate_exact_size(
                                    egui::vec2(10.0, 20.0),
                                    egui::Sense::hover(),
                                );
                                StatusDot::new(status).paint_at(ui, r.center());
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.label(Text::strong(cx.tr.text(&f.title), cx.pal));
                                    if let Some(d) = &f.detail {
                                        ui.label(Text::small(cx.tr.text(d), cx.pal));
                                    }
                                });
                                if f.fixable {
                                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                        ui.add(
                                            Button::new(cx.tr.t("diag.fix")).small().enabled(false),
                                        )
                                        .on_disabled_hover_text(cx.tr.t("diag.fix_soon"));
                                    });
                                }
                            });
                        });
                    if i + 1 < n {
                        crate::shell::divider(ui, cx.pal);
                    }
                }
            });
        }
        (Some(_), _) => empty_state(
            ui,
            cx,
            Icon::Check,
            &cx.tr.t("diag.all_good"),
            &cx.tr.t("diag.all_good_body"),
        ),
        (None, Some(e)) => error_state(ui, cx, &cx.tr.t("diag.failed"), e),
        (None, None) => empty_state(
            ui,
            cx,
            Icon::Diagnostics,
            &cx.tr.t("diag.not_run"),
            &cx.tr.t("diag.not_run_body"),
        ),
    }
}

/// Codex processes of this computer (moved from the 0.2 Targets page): the
/// configuration they started with and their auth address variable.
fn processes(ui: &mut Ui, cx: &Cx<'_>) {
    section(
        ui,
        cx,
        &cx.tr.t("diag.proc.title"),
        Some(&cx.tr.t("diag.proc.subtitle")),
    );
    let procs: Vec<_> = cx
        .data
        .overview
        .value
        .as_ref()
        .map(|o| {
            o.cli
                .processes
                .iter()
                .filter(|p| !matches!(p.kind, CodexProcessKind::AppMain))
                .collect()
        })
        .unwrap_or_default();
    if procs.is_empty() {
        ui.label(Text::small(cx.tr.t("diag.proc.none"), cx.pal));
        return;
    }
    Card::new().flat().show(ui, |ui| {
        let table = Table::new(&[0.8, 1.6, 1.3, 1.6, 1.6]);
        table.header(
            ui,
            &[
                cx.tr.t("diag.proc.col.pid"),
                cx.tr.t("diag.proc.col.kind"),
                cx.tr.t("diag.proc.col.started"),
                cx.tr.t("diag.proc.col.config"),
                cx.tr.t("diag.proc.col.env"),
            ],
        );
        let n = procs.len();
        for (i, p) in procs.iter().enumerate() {
            table.row(ui, 36.0, i + 1 == n, |ui, col| match col {
                0 => {
                    ui.label(Text::mono(p.pid.to_string(), cx.pal));
                }
                1 => {
                    ui.label(Text::small(kind_label(cx, &p.kind), cx.pal));
                }
                2 => {
                    ui.label(Text::small(
                        p.started_at.map(|t| cx.ago(t)).unwrap_or_default(),
                        cx.pal,
                    ));
                }
                3 => {
                    if p.stale {
                        ui.add(StatusDot::new(Status::Warn));
                        ui.label(Text::small(cx.tr.t("diag.proc.stale"), cx.pal));
                    } else {
                        ui.add(StatusDot::new(Status::Ok));
                        ui.label(Text::small(cx.tr.t("diag.proc.current"), cx.pal));
                    }
                }
                _ => env_label(ui, cx, &p.authapi_env),
            });
        }
    });
    ui.add_space(6.0);
    ui.add(egui::Label::new(Text::small(cx.tr.t("diag.proc.hint"), cx.pal)).wrap());
}

fn environment(ui: &mut Ui, cx: &Cx<'_>) {
    section(ui, cx, &cx.tr.t("diag.env.title"), None);
    Card::new().flat().show(ui, |ui| {
        if let Some(o) = cx.data.overview.value.as_ref() {
            kv(ui, cx, &cx.tr.t("diag.env.codex_home"), |ui| {
                ui.label(Text::mono(o.codex_home.display().to_string(), cx.pal));
            });
            if let Some(c) = &o.config {
                kv(ui, cx, &cx.tr.t("diag.env.provider"), |ui| {
                    ui.label(Text::mono(
                        c.model_provider.clone().unwrap_or_else(|| "openai".into()),
                        cx.pal,
                    ));
                });
                if let Some(p) = &c.profile_override {
                    kv(ui, cx, &cx.tr.t("diag.env.profile_override"), |ui| {
                        ui.label(Text::mono(p.clone(), cx.pal));
                    });
                }
            }
        }
        if let Some(paths) = &cx.data.paths {
            kv(ui, cx, &cx.tr.t("diag.env.data_dir"), |ui| {
                super::elided(ui, Text::mono(paths.data_dir.display().to_string(), cx.pal));
            });
            kv(ui, cx, &cx.tr.t("diag.env.router_db"), |ui| {
                super::elided(
                    ui,
                    Text::mono(paths.router_db.display().to_string(), cx.pal),
                );
            });
        }
        kv(
            ui,
            cx,
            &cx.tr.t("diag.env.catalog"),
            |ui| match catalog_label() {
                Some(label) => {
                    ui.label(Text::mono(label, cx.pal));
                }
                None => {
                    ui.label(Text::muted(cx.tr.t("diag.env.unknown"), cx.pal));
                }
            },
        );
        kv(ui, cx, &cx.tr.t("diag.env.model_source"), |ui| {
            match (&cx.data.feeds.value, cx.data.feeds.loading) {
                (Some(f), _) => {
                    let origin = match &f.source {
                        FeedOrigin::Builtin => cx.tr.t("diag.env.feed_builtin"),
                        FeedOrigin::Cache => cx.tr.t("diag.env.feed_cache"),
                        _ => cx.tr.t("diag.env.feed_online"),
                    };
                    let mut parts = vec![origin];
                    if let Some(m) = &f.mirror {
                        parts.push(m.clone());
                    }
                    if let Some(t) = f.fetched_at {
                        parts.push(cx.ago(t));
                    }
                    parts.push(format!("#{}", f.serial));
                    super::elided(ui, Text::body(parts.join(" · "), cx.pal));
                }
                (None, true) => {
                    ui.add(StatusDot::new(Status::Busy));
                }
                (None, false) => {
                    ui.label(Text::muted(cx.tr.t("diag.env.unknown"), cx.pal));
                }
            }
        });
        kv(ui, cx, &cx.tr.t("diag.env.cjk_font"), |ui| {
            match &cx.fonts.cjk_regular {
                Some(f) => {
                    let bold = if cx.fonts.cjk_bold.is_some() {
                        cx.tr.t("diag.env.cjk_bold")
                    } else {
                        cx.tr.t("diag.env.cjk_regular_only")
                    };
                    ui.label(Text::body(format!("{} · {}", f.family, bold), cx.pal));
                }
                None => {
                    ui.add(StatusDot::new(Status::Warn));
                    ui.label(Text::small(cx.tr.t("diag.env.cjk_missing"), cx.pal));
                }
            }
        });
        kv(ui, cx, &cx.tr.t("diag.env.version"), |ui| {
            ui.label(Text::mono(env!("CARGO_PKG_VERSION"), cx.pal));
        });
    });
}

/// "0.163.0 · #serial" of the built-in settings catalog.
fn catalog_label() -> Option<String> {
    let store = superba_catalog::CatalogStore::builtin().ok()?;
    let index = store.index();
    Some(format!("{} · #{}", index.latest, index.serial))
}

fn activity_log(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(Text::title(cx.tr.t("diag.log.title"), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("diag.log.copy"))
                        .icon(Icon::Copy)
                        .small()
                        .enabled(!cx.log.is_empty()),
                )
                .clicked()
            {
                let text = cx
                    .log
                    .iter()
                    .map(|l| format!("{} {}", l.at, l.text))
                    .collect::<Vec<_>>()
                    .join("\n");
                out.push(Action::Copy(text));
            }
        });
    });
    ui.add_space(4.0);
    Card::new()
        .padding(12)
        .fill(cx.pal.bg_subtle)
        .show(ui, |ui| {
            if cx.log.is_empty() {
                ui.label(Text::small(cx.tr.t("diag.log.empty"), cx.pal));
                return;
            }
            // Newest first. Short logs are drawn directly: a scroll area sizes its
            // viewport from the previous frame and would clip a line just added.
            let rows = |ui: &mut egui::Ui| {
                for l in cx.log.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.add(StatusDot::new(l.status).size(6.0));
                        ui.label(Text::mono_muted(super::clock_time(l.at, true), cx.pal));
                        ui.label(Text::mono(l.text.clone(), cx.pal));
                    });
                }
            };
            if cx.log.len() <= 8 {
                rows(ui);
            } else {
                ScrollArea::vertical()
                    .id_salt("activity-log")
                    .max_height(240.0)
                    .auto_shrink([false, true])
                    .show(ui, rows);
            }
        });
}

/// The auth address variable of a process: the line it names (by origin, on any
/// known site), missing, or unknown.
fn env_label(ui: &mut Ui, cx: &Cx<'_>, env: &EnvObservation) {
    match env {
        EnvObservation::Present(v) => {
            let name = cx
                .data
                .sites
                .iter()
                .flat_map(|s| s.lines.iter())
                .find(|l| l.origin.as_str() == *v)
                .map(|l| cx.tr.text(&l.name))
                .unwrap_or_else(|| v.clone());
            ui.add(StatusDot::new(Status::Ok));
            super::elided(ui, Text::small(name, cx.pal));
        }
        EnvObservation::Absent => {
            ui.add(StatusDot::new(Status::Warn));
            ui.label(Text::small(cx.tr.t("diag.proc.env_missing"), cx.pal));
        }
        EnvObservation::Unknown => {
            ui.add(StatusDot::new(Status::Idle));
            ui.label(Text::small(cx.tr.t("diag.proc.env_unknown"), cx.pal));
        }
    }
}

fn kind_label(cx: &Cx<'_>, k: &CodexProcessKind) -> String {
    let key = match k {
        CodexProcessKind::AppMain => "proc.app_main",
        CodexProcessKind::AppServer => "proc.app_server",
        CodexProcessKind::CliTui => "proc.cli_tui",
        CodexProcessKind::CliExec => "proc.cli_exec",
        CodexProcessKind::CliDaemon => "proc.cli_daemon",
        CodexProcessKind::IdeAppServer => "proc.ide",
        CodexProcessKind::RemoteLink { .. } => "proc.remote_link",
        _ => "proc.other",
    };
    cx.tr.t(key)
}
