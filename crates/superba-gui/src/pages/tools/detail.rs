//! The detail of the selected row, one column of sections: a header (identity,
//! state, install state, secondary actions), then for a tool «连接», «模型»,
//! «选项» and «配置文件» as settings rows; for Codex and Claude Code a
//! read-only summary with one action that opens their own page.
use super::form::{self, Rows};
use super::{Selection, State, models};
use crate::design::elide::Shape;
use crate::design::icons::Icon;
use crate::design::tokens::{size, space, text};
use crate::design::widgets::value_chip::ValueChip;
use crate::design::widgets::{
    Badge, Button, Dropdown, IconButton, Segmented, Status, StatusDot, Text, dialog, dialog_body,
};
use crate::pages::{Action, Cx, Page, SuperbaSection, callout};
use egui::{RichText, Ui};
use superba_core::harness::*;
use superba_core::i18n::Severity;

/// Widest provider field.
const FIELD_W: f32 = 260.0;

/// Draw the chosen summary or tool.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.y = 0.0;
    match state.selected.harness() {
        Some(h) => tool(ui, cx, state, h, out),
        None => summary(ui, cx, state, out),
    }
    ui.add_space(space::XL);
}

/// The install state under the tool's name.
fn install_text(cx: &Cx<'_>, h: HarnessId, install: Option<&HarnessInstall>) -> String {
    match install {
        None => cx.tr.t("tools.not_installed"),
        Some(i) => match (&i.version, h) {
            (Some(v), HarnessId::OpenCode) => cx.tr.f(
                "tools.installed_version",
                &[(
                    "version",
                    &format!(
                        "{v} · OpenCode {}",
                        i.opencode_major.unwrap_or(OpencodeMajor::V2).as_str()
                    ),
                )],
            ),
            (Some(v), _) => cx.tr.f("tools.installed_version", &[("version", v)]),
            (None, _) => cx.tr.t("tools.installed"),
        },
    }
}

fn tool(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, h: HarnessId, out: &mut Vec<Action>) {
    let tool_state = state.tool_state(cx, h);
    let install = state.views.get(&h).map(|v| v.install.clone()).or_else(|| {
        state
            .summaries
            .iter()
            .find(|s| s.harness == h)
            .map(|s| s.install.clone())
    });
    let installed = install.as_ref().is_none_or(|i| i.is_some());
    let subtitle = match &install {
        Some(i) => install_text(cx, h, i.as_ref()),
        None => cx.tr.t("tools.loading"),
    };
    form::header(
        ui,
        cx,
        state.selected.monogram(),
        state.selected.name(),
        Some((tool_state.status(), cx.tr.t(tool_state.key()))),
        &subtitle,
        |ui| {
            if ui
                .add(Button::ghost(cx.tr.t("tools.usage")).small())
                .clicked()
            {
                out.push(Action::Go(Page::Usage));
            }
            if !installed
                && ui
                    .add(Button::new(cx.tr.t("tools.install")).small())
                    .clicked()
            {
                out.push(Action::Go(Page::Superba(SuperbaSection::Updates)));
            }
        },
    );
    ui.add_space(space::XL);
    if let Some(e) = &state.error {
        callout(ui, cx, Status::Error, e);
        ui.add_space(space::M);
    }
    if let Some(f) = state.view_load.failure() {
        callout(ui, cx, Status::Error, &f.text(cx));
        ui.add_space(space::S);
        if ui
            .add(Button::new(cx.tr.t("tools.retry")).small())
            .clicked()
        {
            state.load_view(cx, h);
        }
        ui.add_space(space::M);
    }
    if let Some(f) = state.preview_load.failure() {
        callout(ui, cx, Status::Error, &f.text(cx));
        ui.add_space(space::M);
    }
    let Some(saved) = state.views.get(&h).cloned() else {
        ui.label(Text::muted(cx.tr.t("tools.loading"), cx.pal));
        return;
    };
    let view = state
        .preview
        .as_ref()
        .filter(|v| v.harness == h)
        .cloned()
        .unwrap_or(saved);
    let mut settings = state.settings(h);
    let before = settings.clone();
    connection(ui, cx, state, &view, &mut settings);
    form::section_gap(ui);
    models::section(ui, cx, state, h, &mut settings);
    form::section_gap(ui);
    options(ui, cx, h, &mut settings, out);
    if settings != before {
        state.edit(h, settings);
    }
    form::section_gap(ui);
    files(ui, cx, state, &view, h);
    let notes: Vec<_> = view
        .notes
        .iter()
        .filter(|n| n.text.key != "router.protocol.translated")
        .chain(
            state
                .report
                .as_ref()
                .filter(|r| r.harness == h)
                .map(|r| r.notes.as_slice())
                .unwrap_or_default(),
        )
        .collect();
    if !notes.is_empty() {
        ui.add_space(space::M);
        ui.spacing_mut().item_spacing.y = space::XS;
        for note in notes {
            let status = match note.severity {
                Severity::Info => Status::Idle,
                Severity::Warning => Status::Warn,
                Severity::Danger => Status::Error,
            };
            crate::pages::note(ui, cx, status, cx.tr.text(&note.text));
        }
    }
}

fn connection(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &State,
    view: &HarnessView,
    s: &mut HarnessSettings,
) {
    form::section(ui, cx, &cx.tr.t("tools.connection"), None);
    let mut rows = Rows::new();
    let name = s
        .provider
        .as_ref()
        .map(|id| {
            cx.data
                .profile(id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.to_string())
        })
        .unwrap_or_else(|| {
            cx.tr.f(
                "tools.follow",
                &[(
                    "provider",
                    &cx.data
                        .active_profile()
                        .map(|p| p.name.as_str())
                        .unwrap_or("—"),
                )],
            )
        });
    rows.row(ui, cx, &cx.tr.t("tools.provider"), None, |ui| {
        Dropdown::new("tool-provider")
            .label(cx.tr.t("tools.provider"))
            .selected_text(name)
            .fixed_width(ui.available_width().min(FIELD_W))
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(s.provider.is_none(), cx.tr.t("tools.follow_short"))
                    .clicked()
                {
                    s.provider = None;
                    s.protocol = None;
                }
                for p in cx
                    .data
                    .providers()
                    .into_iter()
                    .filter(|p| p.key_ref.is_some())
                {
                    if ui
                        .selectable_label(s.provider.as_ref() == Some(&p.id), &p.name)
                        .clicked()
                    {
                        s.provider = Some(p.id.clone());
                        s.protocol = None;
                    }
                }
            });
    });
    let translated = view
        .notes
        .iter()
        .find(|n| n.text.key == "router.protocol.translated")
        .map(|n| cx.tr.text(&n.text));
    rows.row(
        ui,
        cx,
        &cx.tr.t("tools.protocol"),
        translated.as_deref(),
        |ui| {
            let mut protocol = s
                .protocol
                .or_else(|| view.endpoint.as_ref().map(|e| e.protocol))
                .or_else(|| view.protocols.first().copied());
            let old = protocol;
            let mut control = Segmented::new(("tool-protocol", view.harness), &mut protocol)
                .label(cx.tr.t("tools.protocol"))
                .small();
            for p in view.harness.protocols().iter().copied() {
                let name = match p {
                    HarnessProtocol::Responses => "Responses",
                    HarnessProtocol::Chat => "Chat",
                    HarnessProtocol::Anthropic => "Anthropic",
                    HarnessProtocol::Gemini => "Gemini",
                };
                control = if view.protocols.contains(&p) {
                    control.option(Some(p), name)
                } else {
                    control.disabled_option(Some(p), name, cx.tr.t("tools.protocol_unavailable"))
                };
            }
            control.show(ui);
            if old != protocol {
                s.protocol = protocol;
            }
        },
    );
    let hint = cx.tr.t(if s.route == HarnessRoute::Routed {
        "tools.route_hint"
    } else {
        "tools.direct_hint"
    });
    rows.row(ui, cx, &cx.tr.t("tools.route"), Some(&hint), |ui| {
        Segmented::new(("tool-route", view.harness), &mut s.route)
            .label(cx.tr.t("tools.route"))
            .small()
            .option(HarnessRoute::Direct, cx.tr.t("tools.direct"))
            .option(HarnessRoute::Routed, cx.tr.t("tools.routed"))
            .show(ui);
    });
    rows.row(ui, cx, &cx.tr.t("tools.address"), None, |ui| {
        if state.preview_load.loading() {
            ui.label(Text::muted(cx.tr.t("tools.loading"), cx.pal));
        } else if let Some(endpoint) = &view.endpoint {
            address(ui, cx, &endpoint.base_text());
        } else {
            ui.label(Text::muted("—", cx.pal));
        }
    });
}

/// A URL elided in the middle (full value on hover) with a copy button after it.
fn address(ui: &mut Ui, cx: &Cx<'_>, url: &str) {
    if ui
        .add(IconButton::new(Icon::Copy, cx.tr.t("tools.copy_address")).size(size::CONTROL_SM))
        .clicked()
    {
        ui.ctx().copy_text(url.to_owned());
    }
    ui.add(
        ValueChip::new(url, Shape::Url)
            .plain()
            .size(text::SMALL)
            .max_width(ui.available_width()),
    );
}

fn options(ui: &mut Ui, cx: &Cx<'_>, h: HarnessId, s: &mut HarnessSettings, out: &mut Vec<Action>) {
    form::section(ui, cx, &cx.tr.t("tools.options"), None);
    let mut rows = Rows::new();
    match h {
        HarnessId::OpenCode => rows.row(ui, cx, &cx.tr.t("tools.config_format"), None, |ui| {
            let mut v = s.options.opencode_major.unwrap_or(OpencodeMajor::V2);
            if Segmented::new("tool-opencode", &mut v)
                .label(cx.tr.t("tools.config_format"))
                .small()
                .option(OpencodeMajor::V2, "OpenCode 2")
                .option(OpencodeMajor::V1, "OpenCode 1")
                .show(ui)
                .changed()
            {
                s.options.opencode_major = Some(v);
            }
        }),
        HarnessId::Dsh => rows.row(ui, cx, &cx.tr.t("tools.telemetry"), None, |ui| {
            Segmented::new("tool-telemetry", &mut s.options.dsh_telemetry)
                .label(cx.tr.t("tools.telemetry"))
                .small()
                .option(DshTelemetry::Off, cx.tr.t("tools.telemetry_off"))
                .option(DshTelemetry::Unmanaged, cx.tr.t("tools.telemetry_keep"))
                .show(ui);
        }),
        HarnessId::WorkBuddy => rows.row(
            ui,
            cx,
            &cx.tr.t("tools.editions"),
            Some(&cx.tr.t("tools.editions_hint")),
            |ui| {
                // Right to left: the international edition is drawn first.
                for (edition, key) in [
                    (WorkBuddyEdition::Intl, "tools.workbuddy_intl"),
                    (WorkBuddyEdition::Cn, "tools.workbuddy_cn"),
                ] {
                    let editions = &mut s.options.workbuddy_editions;
                    let mut on = editions.contains(&edition)
                        || (editions.is_empty() && edition == WorkBuddyEdition::Cn);
                    if ui.checkbox(&mut on, cx.tr.t(key)).changed() {
                        if editions.is_empty() {
                            editions.push(WorkBuddyEdition::Cn);
                        }
                        if on {
                            editions.push(edition);
                            editions.sort();
                            editions.dedup();
                        } else if editions.len() > 1 {
                            editions.retain(|e| *e != edition);
                        }
                    }
                }
            },
        ),
        _ => {}
    }
    let stats = cx.data.profiles.value.as_ref().is_some_and(|set| {
        let line = &set.settings.router.stats.line;
        line.enabled && line.harnesses.iter().any(|id| id.id() == h.as_str())
    });
    rows.row(
        ui,
        cx,
        &cx.tr.t("tools.stats_row"),
        Some(&cx.tr.t("tools.stats_help")),
        |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("tools.stats_link"))
                        .trailing(Icon::ArrowRight)
                        .small(),
                )
                .clicked()
            {
                out.push(Action::Go(Page::Router));
            }
            ui.label(Text::muted(
                cx.tr.t(if stats {
                    "tools.enabled"
                } else {
                    "tools.disabled"
                }),
                cx.pal,
            ));
        },
    );
}

/// A file's state: dot and text.
fn file_status(cx: &Cx<'_>, status: &HarnessFileStatus) -> (Status, String) {
    match status {
        HarnessFileStatus::Absent => (Status::Idle, cx.tr.t("tools.file_absent")),
        HarnessFileStatus::Valid => (Status::Ok, cx.tr.t("tools.file_valid")),
        HarnessFileStatus::Invalid { line, column, .. } => (
            Status::Error,
            cx.tr
                .f("tools.file_invalid", &[("line", line), ("column", column)]),
        ),
        _ => (Status::Error, cx.tr.t("tools.file_unreadable")),
    }
}

fn files(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &HarnessView, h: HarnessId) {
    form::section(ui, cx, &cx.tr.t("tools.files"), None);
    let mut rows = Rows::new();
    let applied = view.applied.is_some();
    for file in &view.files {
        let (status, status_text) = file_status(cx, &file.status);
        let owned = view.applied.as_ref().map_or(0, |a| {
            a.owned
                .iter()
                .filter(|p| p.starts_with(&format!("{} ", file.key)))
                .count()
        });
        let path = superba_core::paths::display_path(&file.path);
        let mut edit = false;
        let mut backups = false;
        rows.row_with(
            ui,
            ("tool-file", &file.key),
            |ui| {
                ui.spacing_mut().item_spacing.y = space::XS;
                ui.add(
                    ValueChip::new(&path, Shape::Path)
                        .plain()
                        .size(text::SMALL)
                        .max_width(ui.available_width()),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = space::XS + 2.0;
                    ui.add(StatusDot::new(status));
                    ui.label(Text::small(
                        [
                            status_text.clone(),
                            cx.tr.f("tools.owned", &[("n", &owned)]),
                            cx.tr.f("tools.backup_n", &[("n", &file.backups)]),
                        ]
                        .join(" · "),
                        cx.pal,
                    ));
                    if file.holds_key {
                        ui.add(if applied {
                            Badge::new(cx.tr.t("tools.plain_key")).status(Status::Warn)
                        } else {
                            Badge::new(cx.tr.t("tools.plain_key_after")).hollow()
                        });
                    }
                });
            },
            |ui| {
                backups = ui
                    .add(Button::new(cx.tr.t("tools.backups")).small())
                    .clicked();
                edit = ui.add(Button::new(cx.tr.t("tools.edit")).small()).clicked();
            },
        );
        if edit || backups {
            state.editor.open(cx, h, file.key.clone(), backups);
        }
    }
}

/// Codex and Claude Code: what they use, read-only, and the page that edits it.
fn summary(ui: &mut Ui, cx: &Cx<'_>, state: &State, out: &mut Vec<Action>) {
    let claude = state.selected == Selection::Claude;
    let p = if claude {
        state
            .claude
            .as_ref()
            .and_then(|v| v.provider.as_ref())
            .and_then(|id| cx.data.profile(id))
    } else {
        cx.data.active_profile()
    };
    let routed = if claude {
        cx.data.claude_routed()
    } else {
        cx.data.router_enabled()
    };
    let harness = if claude {
        superba_core::RequestHarness::ClaudeCode
    } else {
        superba_core::RequestHarness::Codex
    };
    let stats = cx.data.profiles.value.as_ref().is_some_and(|set| {
        set.settings.router.stats.line.enabled
            && set.settings.router.stats.line.harnesses.contains(&harness)
    });
    let version = if claude {
        state
            .claude
            .as_ref()
            .and_then(|v| v.install.as_ref())
            .and_then(|i| i.version.as_ref())
            .map(|v| v.to_string())
    } else {
        cx.data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.cli.terminal_cli())
            .and_then(|b| b.version.as_ref())
            .map(|v| v.to_string())
    };
    let subtitle = match &version {
        Some(v) => cx.tr.f("tools.installed_version", &[("version", v)]),
        None => cx.tr.t("tools.version_unknown"),
    };
    let (open_key, page) = if claude {
        ("tools.open_claude", Page::ClaudeCode)
    } else {
        ("tools.open_codex", Page::Settings)
    };
    let badge = super::list::summary_state(cx, state, state.selected);
    form::header(
        ui,
        cx,
        state.selected.monogram(),
        state.selected.name(),
        Some(badge),
        &subtitle,
        |ui| {
            if ui
                .add(
                    Button::new(cx.tr.t(open_key))
                        .trailing(Icon::ArrowRight)
                        .small(),
                )
                .clicked()
            {
                out.push(Action::Go(page));
            }
        },
    );
    ui.add_space(space::XL);

    let value = |ui: &mut Ui, v: &str| {
        ui.add(egui::Label::new(Text::body(v, cx.pal)).truncate());
    };
    form::section(
        ui,
        cx,
        &cx.tr.t("tools.connection"),
        Some(&cx.tr.t("tools.summary_hint")),
    );
    let mut rows = Rows::new();
    match p {
        Some(p) => {
            rows.row(ui, cx, &cx.tr.t("tools.provider"), None, |ui| {
                value(ui, &p.name)
            });
            rows.row(ui, cx, &cx.tr.t("tools.authentication"), None, |ui| {
                value(ui, &cx.tr.text(&p.auth_mode.text()))
            });
            if let Some(line) = &p.line_id {
                rows.row(ui, cx, &cx.tr.t("tools.line"), None, |ui| {
                    value(ui, &cx.data.line_label(cx.tr, Some(&p.site_id), line))
                });
            }
        }
        None => rows.row(ui, cx, &cx.tr.t("tools.provider"), None, |ui| {
            value(ui, &cx.tr.t("tools.not_configured"))
        }),
    }
    rows.row(ui, cx, &cx.tr.t("tools.route"), None, |ui| {
        value(
            ui,
            &cx.tr.t(if routed {
                "tools.routed"
            } else {
                "tools.direct"
            }),
        )
    });
    if claude
        && let Some(v) = &state.claude
        && let Some(url) = v
            .router_base_url
            .as_ref()
            .filter(|_| routed)
            .or_else(|| v.endpoint.as_ref().map(|e| &e.base_url))
    {
        rows.row(ui, cx, &cx.tr.t("tools.address"), None, |ui| {
            address(ui, cx, url.as_str().trim_end_matches('/'))
        });
    }
    rows.row(ui, cx, &cx.tr.t("tools.stats_row"), None, |ui| {
        value(
            ui,
            &cx.tr.t(if stats {
                "tools.enabled"
            } else {
                "tools.disabled"
            }),
        )
    });

    form::section_gap(ui);
    form::section(ui, cx, &cx.tr.t("tools.models"), None);
    let mut rows = Rows::new();
    let mono = |ui: &mut Ui, v: &str| {
        ui.add(egui::Label::new(Text::mono(v, cx.pal)).truncate());
    };
    if claude {
        let slots = state
            .claude
            .as_ref()
            .and_then(|v| v.provider.as_ref().map(|id| v.config.models_of(id)));
        let mut any = false;
        for slot in superba_core::ClaudeSlot::ALL {
            let Some(model) = slots.as_ref().and_then(|m| m.slots.get(&slot)) else {
                continue;
            };
            any = true;
            let key = match slot {
                superba_core::ClaudeSlot::Opus => "tools.model_opus",
                superba_core::ClaudeSlot::Sonnet => "tools.model_sonnet",
                superba_core::ClaudeSlot::Haiku => "tools.model_haiku",
                superba_core::ClaudeSlot::Fable => "tools.model_fable",
                superba_core::ClaudeSlot::Custom => "tools.model_custom",
            };
            rows.row(ui, cx, &cx.tr.t(key), None, |ui| mono(ui, &model.model));
        }
        if !any {
            rows.row(ui, cx, &cx.tr.t("tools.slots"), None, |ui| {
                value(ui, &cx.tr.t("tools.slots_unset"))
            });
        }
    } else {
        let model = p.and_then(|p| p.model.model.clone());
        rows.row(
            ui,
            cx,
            &cx.tr.t("tools.default_model"),
            None,
            |ui| match &model {
                Some(m) => mono(ui, m),
                None => value(ui, &cx.tr.t("tools.model_unset")),
            },
        );
    }
}

/// Shared masked plan review, with an explicit drift choice.
pub fn review(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(plan) = state.plan.clone() else {
        return;
    };
    let mut apply = false;
    let mut cancel = false;
    let screen = ui.ctx().content_rect();
    let response = dialog(
        ui.ctx(),
        egui::Id::unique("tool-plan"),
        560.0_f32.min(screen.width() - 2.0 * space::XXL),
        |ui| {
            ui.label(Text::title(
                cx.tr.t(if plan.kind == HarnessPlanKind::Restore {
                    "tools.review_restore"
                } else {
                    "tools.review_apply"
                }),
                cx.pal,
            ));
            dialog_body(ui, |ui| {
                for c in &plan.changes {
                    ui.label(RichText::new(format!("{} · {}", c.file, c.slot)).monospace());
                    ui.add(
                        egui::Label::new(Text::small(
                            format!(
                                "{} → {}",
                                c.before.as_deref().unwrap_or("—"),
                                c.after.as_deref().unwrap_or("—")
                            ),
                            cx.pal,
                        ))
                        .wrap(),
                    );
                }
                crate::pages::shared::notices::notice_list(
                    ui,
                    cx,
                    egui::Id::unique("tools-plan-notices"),
                    &plan.notes,
                );
                if plan.drift().count() > 0 {
                    ui.add(egui::Label::new(cx.tr.t("tools.drift_hint")).wrap());
                    ui.checkbox(&mut state.take_over, cx.tr.t("tools.take_over"));
                }
            });
            ui.add_space(space::XS);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                apply = ui
                    .add(Button::primary(cx.tr.t("tools.confirm")).small())
                    .clicked();
                cancel = ui
                    .add(Button::new(cx.tr.t("tools.cancel")).small())
                    .clicked();
            });
        },
    );
    if cancel || response.should_close() {
        state.plan = None;
    } else if apply {
        state.apply(cx);
    }
}
