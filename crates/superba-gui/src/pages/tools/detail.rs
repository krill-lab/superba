//! Connection, model selection, tool options and owned configuration files.
use super::{Selection, State, provider};
use crate::design::widgets::{
    Badge, Button, Card, Dropdown, MonogramTile, Pager, Segmented, Status, Text, dialog,
};
use crate::pages::{Action, Cx, Page};
use egui::{RichText, Ui};
use superba_core::harness::*;
use superba_core::{ModelSource, ModelVisibility};

fn label(ui: &mut Ui, cx: &Cx<'_>, key: &str) {
    ui.label(Text::strong(cx.tr.t(key), cx.pal));
    ui.add_space(4.0);
}
/// Draw the chosen summary or editor.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.horizontal_wrapped(|ui| {
        ui.add(MonogramTile::new(state.selected.monogram()).size(40.0));
        ui.heading(state.selected.name());
        if let Some(h) = state.selected.harness()
            && let Some(view) = state.views.get(&h)
        {
            if let Some(install) = &view.install
                && let Some(version) = &install.version
            {
                let version = if h == HarnessId::OpenCode {
                    format!(
                        "OpenCode {} · {version}",
                        install.opencode_major.unwrap_or(OpencodeMajor::V2).as_str()
                    )
                } else {
                    version.clone()
                };
                ui.label(Text::small(version, cx.pal));
            }
            let (status, key) = if !view.pending.is_empty() || state.drafts.contains_key(&h) {
                (Status::Warn, "tools.pending_short")
            } else if view.applied.is_some() {
                (Status::Ok, "tools.applied")
            } else {
                (Status::Idle, "tools.not_configured")
            };
            ui.add(Badge::new(cx.tr.t(key)).status(status));
        }
        if ui
            .add(Button::ghost(cx.tr.t("tools.usage")).small())
            .clicked()
        {
            out.push(Action::Go(Page::Usage));
        }
    });
    ui.add_space(12.0);
    if let Some(e) = &state.error {
        ui.add(egui::Label::new(Text::body(e, cx.pal).color(cx.pal.red)).wrap());
    }
    let Some(h) = state.selected.harness() else {
        summary(ui, cx, state, out);
        return;
    };
    if let Some(f) = state.view_load.failure() {
        ui.add(egui::Label::new(Text::body(f.text(cx), cx.pal).color(cx.pal.red)).wrap());
        if ui.add(Button::new(cx.tr.t("tools.retry"))).clicked() {
            state.load_view(cx, h);
        }
    }
    if let Some(f) = state.preview_load.failure() {
        ui.add(egui::Label::new(Text::body(f.text(cx), cx.pal).color(cx.pal.red)).wrap());
    }
    let Some(saved) = state.views.get(&h).cloned() else {
        ui.label(cx.tr.t("tools.loading"));
        return;
    };
    let view = state
        .preview
        .as_ref()
        .filter(|v| v.harness == h)
        .cloned()
        .unwrap_or(saved);
    if view.install.is_none() {
        ui.add(egui::Label::new(Text::small(cx.tr.t("tools.not_installed"), cx.pal)).wrap());
    }
    let mut settings = state.settings(h);
    let before = settings.clone();
    if ui.available_width() >= 650.0 {
        ui.columns(2, |cols| {
            connection(&mut cols[0], cx, state, &view, &mut settings);
            options(&mut cols[0], cx, h, &mut settings, out);
            models(&mut cols[1], cx, state, h, &mut settings, out);
        });
    } else {
        connection(ui, cx, state, &view, &mut settings);
        ui.add_space(14.0);
        models(ui, cx, state, h, &mut settings, out);
        ui.add_space(14.0);
        options(ui, cx, h, &mut settings, out);
    }
    if settings != before {
        state.edit(h, settings);
    }
    ui.add_space(12.0);
    label(ui, cx, "tools.files");
    for file in &view.files {
        Card::new().flat().show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(superba_core::paths::display_path(&file.path)).monospace(),
                )
                .truncate(),
            );
            let status = match &file.status {
                HarnessFileStatus::Absent => cx.tr.t("tools.file_absent"),
                HarnessFileStatus::Valid => cx.tr.t("tools.file_valid"),
                HarnessFileStatus::Invalid { line, column, .. } => cx
                    .tr
                    .f("tools.file_invalid", &[("line", line), ("column", column)]),
                _ => cx.tr.t("tools.file_unreadable"),
            };
            let owned = view.applied.as_ref().map_or(0, |a| {
                a.owned
                    .iter()
                    .filter(|p| p.starts_with(&format!("{} ", file.key)))
                    .count()
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(Text::small(
                    format!("{} · {}", status, cx.tr.f("tools.owned", &[("n", &owned)])),
                    cx.pal,
                ));
                if file.holds_key {
                    ui.label(Text::small(cx.tr.t("tools.plain_key"), cx.pal));
                }
                if ui.add(Button::new(cx.tr.t("tools.edit")).small()).clicked() {
                    state.editor.open(cx, h, file.key.clone(), false);
                }
                if ui
                    .add(
                        Button::ghost(cx.tr.f("tools.backup_count", &[("n", &file.backups)]))
                            .small(),
                    )
                    .clicked()
                {
                    state.editor.open(cx, h, file.key.clone(), true);
                }
            });
        });
    }
    for note in view
        .notes
        .iter()
        .filter(|n| n.text.key != "router.protocol.translated")
    {
        ui.add(egui::Label::new(Text::small(cx.tr.text(&note.text), cx.pal)).wrap());
    }
    if let Some(report) = &state.report
        && report.harness == h
    {
        for note in &report.notes {
            ui.add(egui::Label::new(Text::small(cx.tr.text(&note.text), cx.pal)).wrap());
        }
    }
}
fn field(ui: &mut Ui, cx: &Cx<'_>, key: &str, show: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [66.0, 28.0],
            egui::Label::new(Text::small(cx.tr.t(key), cx.pal)),
        );
        show(ui);
    });
}
fn connection(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &State,
    view: &HarnessView,
    s: &mut HarnessSettings,
) {
    label(ui, cx, "tools.connection");
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
    field(ui, cx, "tools.provider", |ui| {
        Dropdown::new("tool-provider")
            .label(cx.tr.t("tools.provider"))
            .selected_text(name)
            .width(ui.available_width())
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
    field(ui, cx, "tools.protocol", |ui| {
        let mut protocol = s
            .protocol
            .or_else(|| view.endpoint.as_ref().map(|e| e.protocol))
            .or_else(|| view.protocols.first().copied());
        let old = protocol;
        let mut control =
            Segmented::new("tool-protocol", &mut protocol).label(cx.tr.t("tools.protocol"));
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
    });
    if let Some(note) = view
        .notes
        .iter()
        .find(|n| n.text.key == "router.protocol.translated")
    {
        field(ui, cx, "tools.eligibility", |ui| {
            ui.label(Text::body(cx.tr.text(&note.text), cx.pal));
        });
    }
    field(ui, cx, "tools.route", |ui| {
        Segmented::new("tool-route", &mut s.route)
            .label(cx.tr.t("tools.route"))
            .option(HarnessRoute::Direct, cx.tr.t("tools.direct"))
            .option(HarnessRoute::Routed, cx.tr.t("tools.routed"))
            .show(ui);
    });
    field(ui, cx, "tools.address", |ui| {
        if state.preview_load.loading() {
            ui.label(cx.tr.t("tools.loading"));
        } else if let Some(endpoint) = &view.endpoint {
            let text = endpoint.base_text();
            if ui
                .add(crate::design::widgets::IconButton::new(
                    crate::design::icons::Icon::Copy,
                    cx.tr.t("tools.copy_address"),
                ))
                .clicked()
            {
                ui.ctx().copy_text(text.clone());
            }
            ui.add(egui::Label::new(RichText::new(&text).monospace()).wrap());
        } else {
            ui.label("—");
        }
    });
    ui.add(
        egui::Label::new(Text::small(
            cx.tr.t(if s.route == HarnessRoute::Routed {
                "tools.route_hint"
            } else {
                "tools.direct_hint"
            }),
            cx.pal,
        ))
        .wrap(),
    );
}
fn models(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    h: HarnessId,
    s: &mut HarnessSettings,
    out: &mut Vec<Action>,
) {
    label(ui, cx, "tools.models");
    let Some(id) = provider(cx, s) else {
        ui.label(cx.tr.t("tools.choose_provider"));
        return;
    };
    if let Some(p) = cx.data.profile(&id) {
        let mut source = p.model_source.unwrap_or(ModelSource::Auto);
        let old = source;
        let label = |source| {
            cx.tr.t(match source {
                ModelSource::Auto => "models.list_source.auto",
                ModelSource::Codex => "models.list_source.codex",
                ModelSource::Standard => "models.list_source.standard",
            })
        };
        Dropdown::new("tool-source")
            .label(cx.tr.t("models.list_source.label"))
            .selected_text(label(source))
            .show_ui(ui, |ui| {
                for value in [ModelSource::Auto, ModelSource::Codex, ModelSource::Standard] {
                    if ui.selectable_label(source == value, label(value)).clicked() {
                        source = value;
                    }
                }
            });
        if source != old {
            let mut p = p.clone();
            p.model_source = Some(source);
            out.push(Action::SaveProfile {
                profile: Box::new(p),
                key: None,
            });
        }
    }
    ui.horizontal_wrapped(|ui| {
        if ui.add(Button::new(cx.tr.t("tools.pull")).small()).clicked() {
            state.pull_models(cx, id.clone(), true);
        }
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text(cx.tr.t("tools.search"))
                .desired_width(ui.available_width().min(210.0)),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::TextInput,
                ui.is_enabled(),
                cx.tr.t("tools.search"),
            )
        });
        if response.changed() {
            state.model_page = 0;
        }
    });
    if let Some(f) = state.model_load.failure() {
        ui.add(egui::Label::new(Text::small(f.text(cx), cx.pal).color(cx.pal.red)).wrap());
    }
    if state.model_load.loading() {
        ui.label(cx.tr.t("tools.loading"));
    }
    let Some(list) = state.models.get(&id) else {
        return;
    };
    for e in list
        .sources
        .iter()
        .filter_map(|s| s.error.as_ref())
        .chain(list.fallback_reason.as_ref())
    {
        ui.add(
            egui::Label::new(
                Text::small(crate::pages::error_text(cx.tr, e), cx.pal).color(cx.pal.red),
            )
            .wrap(),
        );
    }
    let mut available: Vec<_> = list
        .models
        .iter()
        .filter(|m| m.supported_in_api && m.visibility != ModelVisibility::None)
        .map(|m| (m.slug.clone(), m.context_window))
        .collect();
    for id in &s.models {
        if !available.iter().any(|(slug, _)| slug == id) {
            available.push((id.clone(), None));
        }
    }
    let query = state.search.to_lowercase();
    let filtered: Vec<_> = available
        .iter()
        .filter(|m| m.0.to_lowercase().contains(&query))
        .collect();
    let total = filtered.len();
    let count = if s.models.is_empty() {
        available.len()
    } else {
        s.models.len()
    };
    ui.label(Text::small(
        format!("{count} / {}", available.len()),
        cx.pal,
    ));
    state.model_page = state.model_page.min(total.saturating_sub(1) / 6);
    for model in filtered.into_iter().skip(state.model_page * 6).take(6) {
        ui.horizontal(|ui| {
            let mut checked = s.models.is_empty() || s.models.contains(&model.0);
            if ui.checkbox(&mut checked, &model.0).changed() {
                if s.models.is_empty() {
                    s.models = available.iter().map(|m| m.0.clone()).collect();
                }
                if checked {
                    if !s.models.contains(&model.0) {
                        s.models.push(model.0.clone());
                    }
                } else if s.models.len() > 1 {
                    s.models.retain(|id| id != &model.0);
                    if s.default_model.as_ref() == Some(&model.0) {
                        s.default_model = s.models.first().cloned();
                    }
                }
            }
            if let Some(w) = model.1 {
                ui.label(Text::small(format!("{}K", w / 1000), cx.pal));
            }
        });
    }
    Pager::new(("tool-models", h), &mut state.model_page, total, 6)
        .compact()
        .labels(cx.tr.t("tools.previous"), cx.tr.t("tools.next"))
        .show(ui);
    let selected: Vec<_> = available
        .iter()
        .filter(|m| s.models.is_empty() || s.models.contains(&m.0))
        .collect();
    Dropdown::new("tool-default-model")
        .label(cx.tr.t("tools.default_model"))
        .selected_text(
            s.default_model
                .as_deref()
                .or_else(|| selected.first().map(|m| m.0.as_str()))
                .unwrap_or("—"),
        )
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for m in selected {
                if ui
                    .selectable_label(s.default_model.as_ref() == Some(&m.0), &m.0)
                    .clicked()
                {
                    s.default_model = Some(m.0.clone());
                }
            }
        });
    ui.add(egui::Label::new(Text::small(cx.tr.t("tools.models_hint"), cx.pal)).wrap());
}
fn options(ui: &mut Ui, cx: &Cx<'_>, h: HarnessId, s: &mut HarnessSettings, out: &mut Vec<Action>) {
    ui.add_space(8.0);
    label(ui, cx, "tools.options");
    match h {
        HarnessId::OpenCode => {
            let mut v = s.options.opencode_major.unwrap_or(OpencodeMajor::V2);
            if Segmented::new("tool-opencode", &mut v)
                .label(cx.tr.t("tools.config_format"))
                .option(OpencodeMajor::V2, "OpenCode 2")
                .option(OpencodeMajor::V1, "OpenCode 1")
                .show(ui)
                .changed()
            {
                s.options.opencode_major = Some(v);
            }
        }
        HarnessId::Dsh => {
            Segmented::new("tool-telemetry", &mut s.options.dsh_telemetry)
                .label(cx.tr.t("tools.telemetry"))
                .option(DshTelemetry::Off, cx.tr.t("tools.telemetry_off"))
                .option(DshTelemetry::Unmanaged, cx.tr.t("tools.telemetry_keep"))
                .show(ui);
        }
        HarnessId::WorkBuddy => {
            for (edition, key) in [
                (WorkBuddyEdition::Cn, "tools.workbuddy_cn"),
                (WorkBuddyEdition::Intl, "tools.workbuddy_intl"),
            ] {
                let mut on = s.options.workbuddy_editions.contains(&edition)
                    || (s.options.workbuddy_editions.is_empty() && edition == WorkBuddyEdition::Cn);
                if ui.checkbox(&mut on, cx.tr.t(key)).changed() {
                    if s.options.workbuddy_editions.is_empty() {
                        s.options.workbuddy_editions.push(WorkBuddyEdition::Cn);
                    }
                    if on {
                        s.options.workbuddy_editions.push(edition);
                        s.options.workbuddy_editions.sort();
                        s.options.workbuddy_editions.dedup();
                    } else if s.options.workbuddy_editions.len() > 1 {
                        s.options.workbuddy_editions.retain(|e| *e != edition);
                    }
                }
            }
        }
        _ => {}
    }
    let stats = cx.data.profiles.value.as_ref().is_some_and(|set| {
        let line = &set.settings.router.stats.line;
        line.enabled && line.harnesses.iter().any(|id| id.id() == h.as_str())
    });
    ui.label(Text::small(
        cx.tr.t(if stats {
            "tools.stats_on"
        } else {
            "tools.stats_off"
        }),
        cx.pal,
    ));
    if ui
        .add(Button::ghost(cx.tr.t("tools.stats_link")).small())
        .clicked()
    {
        out.push(Action::Go(Page::Router));
    }
}
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
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut row = |key: &str, value: String| rows.push((cx.tr.t(key), value));
    if let Some(p) = p {
        row("tools.provider", p.name.clone());
        row("tools.authentication", cx.tr.text(&p.auth_mode.text()));
        if let Some(line) = &p.line_id {
            row(
                "tools.line",
                cx.data.line_label(cx.tr, Some(&p.site_id), line),
            );
        }
        if !claude {
            row(
                "tools.default_model",
                p.model.model.clone().unwrap_or_else(|| "—".into()),
            );
        }
    }
    row(
        "tools.route",
        cx.tr.t(if routed {
            "tools.routed"
        } else {
            "tools.direct"
        }),
    );
    row(
        "tools.statistics",
        cx.tr.t(if stats {
            "tools.enabled"
        } else {
            "tools.disabled"
        }),
    );
    if claude {
        if let Some(v) = &state.claude {
            if let Some(e) = v
                .router_base_url
                .as_ref()
                .filter(|_| routed)
                .or_else(|| v.endpoint.as_ref().map(|e| &e.base_url))
            {
                row("tools.address", e.to_string());
            }
            if let Some(id) = &v.provider {
                let models = v.config.models_of(id);
                for slot in superba_core::ClaudeSlot::ALL {
                    row(
                        match slot {
                            superba_core::ClaudeSlot::Opus => "tools.model_opus",
                            superba_core::ClaudeSlot::Sonnet => "tools.model_sonnet",
                            superba_core::ClaudeSlot::Haiku => "tools.model_haiku",
                            superba_core::ClaudeSlot::Fable => "tools.model_fable",
                            superba_core::ClaudeSlot::Custom => "tools.model_custom",
                        },
                        models
                            .slots
                            .get(&slot)
                            .map(|m| m.model.clone())
                            .unwrap_or_else(|| "—".into()),
                    );
                }
            }
            if let Some(version) = v.install.as_ref().and_then(|i| i.version.as_ref()) {
                row("tools.version", version.to_string());
            }
        }
    } else if let Some(version) = cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.cli.terminal_cli())
        .and_then(|b| b.version.as_ref())
    {
        row("tools.version", version.to_string());
    }
    Card::new().flat().show(ui, |ui| {
        label(ui, cx, "tools.connection");
        let value_width = (ui.available_width() - 140.0).max(120.0);
        egui::Grid::new(("tool-summary-properties", state.selected))
            .num_columns(2)
            .spacing([24.0, 6.0])
            .min_col_width(108.0)
            .max_col_width(value_width)
            .show(ui, |ui| {
                for (key, value) in rows {
                    ui.label(Text::small(key, cx.pal));
                    ui.add(egui::Label::new(Text::body(value, cx.pal)).wrap());
                    ui.end_row();
                }
            });
        ui.add_space(18.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::primary(cx.tr.t(if claude {
                    "tools.open_claude"
                } else {
                    "tools.open_codex"
                })))
                .clicked()
            {
                out.push(Action::Go(if claude {
                    Page::ClaudeCode
                } else {
                    Page::Settings
                }));
            }
            if ui
                .add(Button::ghost(cx.tr.t("tools.open_providers")))
                .clicked()
            {
                out.push(Action::Go(Page::Providers));
            }
        });
    });
}
/// Shared masked plan review, with an explicit drift choice.
pub fn review(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(plan) = state.plan.clone() else {
        return;
    };
    let mut apply = false;
    let mut cancel = false;
    let response = dialog(
        ui.ctx(),
        egui::Id::unique("tool-plan"),
        560.0_f32.min(ui.ctx().content_rect().width() - 64.0),
        |ui| {
            ui.heading(cx.tr.t(if plan.kind == HarnessPlanKind::Restore {
                "tools.review_restore"
            } else {
                "tools.review_apply"
            }));
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
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
                });
            for note in &plan.notes {
                ui.add(egui::Label::new(cx.tr.text(&note.text)).wrap());
            }
            if plan.drift().count() > 0 {
                ui.add(egui::Label::new(cx.tr.t("tools.drift_hint")).wrap());
                ui.checkbox(&mut state.take_over, cx.tr.t("tools.take_over"));
            }
            ui.horizontal(|ui| {
                cancel = ui.add(Button::new(cx.tr.t("tools.cancel"))).clicked();
                apply = ui.add(Button::primary(cx.tr.t("tools.confirm"))).clicked();
            });
        },
    );
    if cancel || response.should_close() {
        state.plan = None;
    } else if apply {
        state.apply(cx);
    }
}
