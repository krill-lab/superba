//! Installed coding tools, with the same file checks and drift as their editors.
use egui::Ui;
use std::collections::BTreeSet;
use superba_core::harness::{HarnessFileStatus, HarnessSummary, HarnessView};
use superba_core::paths::display_path;
use superba_core::{ClaudeFileStatus, ClaudeView, Error, Result, Severity};

use crate::design::widgets::{Button, Card, Status, StatusDot, Table, Text};
use crate::pages::superba::maintenance::{self, Read};
use crate::pages::{Action, Cx, Page, section};

#[derive(Debug)]
struct Snapshot {
    claude: Result<ClaudeView>,
    harnesses: Result<Vec<(HarnessSummary, Result<HarnessView>)>>,
}

/// Background checks and expanded rows, retained across section navigation.
#[derive(Debug, Default)]
pub struct State {
    read: Read<Snapshot>,
    expanded: BTreeSet<String>,
    probe: Read<superba_core::ClaudeProbe>,
    probing: bool,
}

impl State {
    /// Recheck after a refresh or on returning to Diagnostics.
    pub fn invalidate(&mut self) {
        self.read.invalidate();
    }
    pub(super) fn loading(&self) -> bool {
        self.read.load.loading()
    }
    pub(super) fn poll(&mut self, ui: &Ui, cx: &Cx<'_>) {
        let service = cx.service.clone();
        self.read.poll(ui, move || async move {
            let claude = service.claude_view().await;
            let harnesses = match service.harness_list().await {
                Ok(list) => {
                    let mut rows = Vec::new();
                    for summary in list.into_iter().filter(|s| s.installed) {
                        let view = service.harness_view(summary.harness).await;
                        rows.push((summary, view));
                    }
                    Ok(rows)
                }
                Err(e) => Err(e),
            };
            Ok(Snapshot { claude, harnesses })
        });
        if self.probing {
            let service = cx.service.clone();
            self.probe
                .poll(ui, move || async move { service.claude_probe(None).await });
        }
    }
}

#[derive(Debug)]
struct Row {
    id: String,
    name: String,
    program: String,
    files: String,
    status: Status,
    note: String,
    details: Vec<String>,
    page: Page,
}

fn file_label(cx: &Cx<'_>, key: &str, status: &HarnessFileStatus) -> (Status, String) {
    let (severity, label) = match status {
        HarnessFileStatus::Valid => (Status::Ok, cx.tr.t("superba.diagnostics.valid")),
        HarnessFileStatus::Absent => (Status::Idle, cx.tr.t("superba.diagnostics.absent")),
        HarnessFileStatus::Invalid { line, column, .. } => (
            Status::Error,
            cx.tr.f(
                "superba.diagnostics.invalid",
                &[("line", line), ("column", column)],
            ),
        ),
        HarnessFileStatus::NotObject | HarnessFileStatus::Unreadable { .. } => {
            (Status::Error, cx.tr.t("superba.diagnostics.unreadable"))
        }
    };
    // Parser / OS errors may contain file values; diagnostics only reports the
    // file and location. Keys and configuration contents never enter Copy.
    (severity, format!("{key} · {label}"))
}
fn rank(status: Status) -> u8 {
    match status {
        Status::Error => 3,
        Status::Warn => 2,
        Status::Idle => 1,
        _ => 0,
    }
}
fn worse(a: Status, b: Status) -> Status {
    if rank(a) >= rank(b) { a } else { b }
}
fn program(cx: &Cx<'_>, version: Option<&str>, path: Option<&std::path::Path>) -> String {
    match (version, path) {
        (Some(v), Some(p)) => format!("{v} · {}", display_path(p)),
        (None, Some(p)) => display_path(p),
        (Some(v), None) => v.to_owned(),
        (None, None) => cx.tr.t("superba.diagnostics.config_only"),
    }
}
fn drift(cx: &Cx<'_>, n: usize) -> String {
    cx.tr.f("superba.diagnostics.drift", &[("n", &n)])
}
fn rows(cx: &Cx<'_>, state: &State) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(o) = &cx.data.overview.value
        && (!o.cli.binaries.is_empty() || !o.app.installs.is_empty())
    {
        let install = o.cli.terminal_cli().or_else(|| o.cli.binaries.first());
        let version = install
            .and_then(|i| i.version.as_ref())
            .map(ToString::to_string);
        let (mut status, mut files) = match &o.config {
            Some(c) if c.parse_error.is_some() => {
                (Status::Error, cx.tr.t("superba.diagnostics.codex_invalid"))
            }
            Some(c) if c.config_sha256.is_some() => {
                (Status::Ok, cx.tr.t("superba.diagnostics.codex_valid"))
            }
            Some(_) => (Status::Idle, cx.tr.t("superba.diagnostics.codex_absent")),
            None => (Status::Idle, cx.tr.t("superba.diagnostics.unknown")),
        };
        let drifted = o
            .config
            .as_ref()
            .map_or(0, |c| c.drift.len() + usize::from(c.auth_drift.is_some()));
        if drifted > 0 {
            status = worse(status, Status::Warn);
            files = format!("{files} · {}", drift(cx, drifted));
        }
        rows.push(Row {
            id: "codex".into(),
            name: "Codex".into(),
            program: program(cx, version.as_deref(), install.map(|i| i.path.as_path())),
            files,
            status,
            note: cx.tr.f(
                "superba.diagnostics.processes",
                &[
                    ("n", &o.cli.processes.len()),
                    ("stale", &o.cli.stale_count()),
                ],
            ),
            details: vec![display_path(&o.codex_home.join("config.toml"))],
            page: Page::Settings,
        });
    }
    let view = state
        .read
        .value
        .as_ref()
        .and_then(|s| s.claude.as_ref().ok());
    let diag = cx.data.claude_diag.value.as_ref();
    if view.is_some_and(|v| v.install.is_some() || v.file != ClaudeFileStatus::Absent)
        || diag.is_some_and(|d| d.install.is_some() || d.file != ClaudeFileStatus::Absent)
    {
        let file = view
            .map(|v| &v.file)
            .or_else(|| diag.map(|d| &d.file))
            .expect("Claude state");
        let f = match file {
            ClaudeFileStatus::Valid => HarnessFileStatus::Valid,
            ClaudeFileStatus::Absent => HarnessFileStatus::Absent,
            ClaudeFileStatus::Invalid { line, column, .. } => HarnessFileStatus::Invalid {
                line: *line,
                column: *column,
                message: String::new(),
            },
            _ => HarnessFileStatus::Unreadable {
                message: String::new(),
            },
        };
        let (mut status, mut files) = file_label(cx, "settings.json", &f);
        let n = view
            .and_then(|v| v.applied.as_ref())
            .map_or(0, |a| a.drift.len());
        if n > 0 {
            status = worse(status, Status::Warn);
            files = format!("{files} · {}", drift(cx, n));
        }
        let mut details = Vec::new();
        if let Some(d) = diag {
            for finding in &d.findings {
                status = worse(
                    status,
                    match finding.severity {
                        Severity::Danger => Status::Error,
                        Severity::Warning => Status::Warn,
                        _ => Status::Ok,
                    },
                );
                details.push(cx.tr.text(&finding.title));
                if let Some(detail) = &finding.detail {
                    details.push(cx.tr.text(detail));
                }
            }
        }
        let install = view
            .and_then(|v| v.install.as_ref())
            .or_else(|| diag.and_then(|d| d.install.as_ref()));
        let note = diag
            .and_then(|d| d.findings.iter().find(|f| f.severity != Severity::Info))
            .map(|f| cx.tr.text(&f.title))
            .unwrap_or_else(|| cx.tr.t("superba.diagnostics.claude_details"));
        rows.push(Row {
            id: "claude".into(),
            name: "Claude Code".into(),
            program: program(
                cx,
                install.and_then(|i| i.version.as_deref()),
                install.map(|i| i.binary.as_path()),
            ),
            files,
            status,
            note,
            details,
            page: Page::ClaudeCode,
        });
    }
    if let Some(Ok(harnesses)) = state.read.value.as_ref().map(|s| &s.harnesses) {
        for (summary, result) in harnesses {
            let install = result
                .as_ref()
                .ok()
                .and_then(|v| v.install.as_ref())
                .or(summary.install.as_ref());
            let mut row = Row {
                id: summary.harness.to_string(),
                name: summary.harness.display_name().into(),
                program: program(
                    cx,
                    install.and_then(|i| i.version.as_deref()),
                    install.and_then(|i| i.binary.as_deref()),
                ),
                files: cx.tr.t("superba.diagnostics.unknown"),
                status: Status::Idle,
                note: String::new(),
                details: Vec::new(),
                page: Page::Tools,
            };
            match result {
                Ok(v) => {
                    row.status = Status::Ok;
                    let mut labels = Vec::new();
                    for file in &v.files {
                        let (status, label) = file_label(cx, &file.key, &file.status);
                        row.status = worse(row.status, status);
                        labels.push(label.clone());
                        row.details
                            .push(format!("{} · {label}", display_path(&file.path)));
                    }
                    row.files = if labels.is_empty() {
                        cx.tr.t("superba.diagnostics.unknown")
                    } else {
                        labels.join("; ")
                    };
                    let n = v.applied.as_ref().map_or(summary.drift, |a| a.drift.len());
                    if n > 0 {
                        row.status = worse(row.status, Status::Warn);
                        row.files = format!("{} · {}", row.files, drift(cx, n));
                    }
                    if !v.pending.is_empty() {
                        row.details.push(cx.tr.t("superba.diagnostics.pending"));
                    }
                    row.note = v
                        .notes
                        .first()
                        .map(|n| cx.tr.text(&n.text))
                        .unwrap_or_else(|| {
                            if v.files.iter().any(|f| f.holds_key) {
                                cx.tr.t("superba.diagnostics.plain_key")
                            } else {
                                cx.tr.t("superba.diagnostics.checked")
                            }
                        });
                    row.details
                        .extend(v.notes.iter().map(|n| cx.tr.text(&n.text)));
                    if n > 0 {
                        row.details.push(drift(cx, n));
                    }
                }
                Err(e) => {
                    row.status = Status::Error;
                    row.note = crate::pages::error_text(cx.tr, e);
                }
            }
            rows.push(row);
        }
    }
    rows
}

fn label(ui: &mut Ui, cx: &Cx<'_>, text: &str, mono: bool) {
    let rich = if mono {
        Text::mono(text, cx.pal)
    } else {
        Text::small(text, cx.pal)
    };
    ui.add(egui::Label::new(rich).truncate())
        .on_hover_text(text);
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    section(ui, cx, &cx.tr.t("superba.diagnostics.tools"), None);
    state.read.message(ui, cx);
    if let Some(snapshot) = &state.read.value {
        for error in [
            snapshot.claude.as_ref().err(),
            snapshot.harnesses.as_ref().err(),
            cx.data.claude_diag.error.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !matches!(error, Error::NotImplemented { .. }) {
                crate::pages::note(
                    ui,
                    cx,
                    Status::Error,
                    crate::pages::error_text(cx.tr, error),
                );
            }
        }
    }
    let rows = rows(cx, state);
    if rows.is_empty() && !state.loading() {
        ui.label(Text::small(cx.tr.t("superba.diagnostics.none"), cx.pal));
    }
    Card::new().flat().show(ui, |ui| {
        let narrow = ui.available_width() < 650.0;
        let table = if narrow {
            Table::new(&[1.2, 3.5])
        } else {
            Table::new(&[1.3, 2.1, 2.3, 2.2])
        };
        let headers: Vec<_> = if narrow {
            vec!["tool", "status"]
        } else {
            vec!["tool", "program", "files", "notes"]
        }
        .into_iter()
        .map(|k| cx.tr.t(&format!("superba.diagnostics.col.{k}")))
        .collect();
        table.header(ui, &headers);
        for (i, row) in rows.iter().enumerate() {
            ui.push_id(&row.id, |ui| {
                let mut open = state.expanded.contains(&row.id);
                let response = table.row(
                    ui,
                    if narrow { 64.0 } else { 38.0 },
                    i + 1 == rows.len(),
                    |ui, col| {
                        if col == 0 {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            crate::design::icons::paint(
                                ui.painter(),
                                rect,
                                if open {
                                    crate::design::icons::Icon::ChevronDown
                                } else {
                                    crate::design::icons::Icon::ChevronRight
                                },
                                cx.pal.text_muted,
                            );
                            label(ui, cx, &row.name, false);
                        } else if narrow {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.spacing_mut().interact_size.y = 17.0;
                                label(ui, cx, &row.program, true);
                                ui.horizontal(|ui| {
                                    ui.add(StatusDot::new(row.status));
                                    label(ui, cx, &row.files, false);
                                });
                                label(ui, cx, &row.note, false);
                            });
                        } else {
                            match col {
                                1 => label(ui, cx, &row.program, true),
                                2 => {
                                    ui.add(StatusDot::new(row.status));
                                    label(ui, cx, &row.files, false);
                                }
                                _ => label(ui, cx, &row.note, false),
                            }
                        }
                    },
                );
                maintenance::expander(
                    ui,
                    response,
                    &format!("{}, {}, {}, {}", row.name, row.program, row.files, row.note),
                    &mut open,
                );
                if open {
                    state.expanded.insert(row.id.clone());
                    egui::Frame::new().inner_margin(12).show(ui, |ui| {
                        for text in [&row.program, &row.files, &row.note]
                            .into_iter()
                            .chain(row.details.iter())
                        {
                            ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
                        }
                        if ui
                            .add(
                                Button::ghost(
                                    cx.tr
                                        .f("superba.diagnostics.configure", &[("tool", &row.name)]),
                                )
                                .small(),
                            )
                            .clicked()
                        {
                            out.push(Action::Go(row.page));
                        }
                        if row.id == "claude" {
                            probe(ui, cx, state);
                        }
                    });
                } else {
                    state.expanded.remove(&row.id);
                }
            });
        }
    });
}

fn probe(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    if ui
        .add(
            Button::new(cx.tr.t("superba.diagnostics.probe"))
                .small()
                .loading(state.probe.load.loading())
                .enabled(!cx.busy),
        )
        .clicked()
    {
        state.probe.invalidate();
        state.probing = true;
    }
    if state.probing {
        state.probe.message(ui, cx);
    }
    if let Some(p) = &state.probe.value {
        let status = if p.ok { Status::Ok } else { Status::Error };
        let text = cx.tr.f(
            "superba.diagnostics.probe_result",
            &[
                (
                    "code",
                    &p.status.map_or_else(|| "—".into(), |s| s.to_string()),
                ),
                ("ms", &p.latency_ms),
            ],
        );
        crate::pages::note(ui, cx, status, text);
        if let Some(message) = &p.message {
            ui.add(egui::Label::new(Text::small(message, cx.pal)).wrap());
        }
    }
}

pub(super) fn report(cx: &Cx<'_>, state: &State) -> String {
    let mut lines = vec![format!(
        "Superba {} · {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    )];
    if let Some(checks) = &cx.data.doctor.value {
        for f in checks {
            lines.push(cx.tr.text(&f.title));
            if let Some(d) = &f.detail {
                lines.push(cx.tr.text(d));
            }
        }
    }
    for row in rows(cx, state) {
        lines.push(format!(
            "{} | {} | {} | {}",
            row.name, row.program, row.files, row.note
        ));
        lines.extend(row.details);
    }
    if let Some(o) = &cx.data.overview.value {
        lines.push(format!("Codex: {}", display_path(&o.codex_home)));
        for p in &o.cli.processes {
            lines.push(format!("PID {} · {:?} · stale={}", p.pid, p.kind, p.stale));
        }
    }
    lines.extend(cx.log.iter().map(|l| format!("{} {}", l.at, l.text)));
    lines.join("\n")
}
