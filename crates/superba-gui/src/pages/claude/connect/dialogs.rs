//! The dialogs of the page that are not about one model: the provider's
//! Anthropic endpoint, and the plan of an apply or a restore (what changes in
//! `settings.json`).

use egui::{TextEdit, vec2};
use superba_core::claude::{AnthropicPreset, anthropic_preset, anthropic_presets};
use superba_core::{AnthropicAuth, AnthropicEndpoint, ClaudePlanKind};

use super::State;
use crate::design::fonts::{Weight, mono_font};
use crate::design::tokens::text;
use crate::design::widgets::{
    Button, Change, ChangeLabels, ChangeList, Dropdown, Segmented, Status, Text, Toggle, dialog,
    dialog_body,
};
use crate::pages::{Action, Cx};

pub(super) fn endpoint(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(edit) = state.connect.endpoint.as_mut() else {
        return;
    };
    let profile = cx.data.profile(&edit.provider).cloned();
    let site = profile.as_ref().and_then(|p| cx.data.site(&p.site_id));
    let origin = match (&site, &profile) {
        (Some(s), Some(p)) => superba_core::claude::profile_origin(s, p),
        _ => None,
    };
    let mut close = false;
    let mut save: Option<Option<AnthropicEndpoint>> = None;
    let presets = anthropic_presets();
    let r = dialog(ctx, egui::Id::unique("claude-endpoint"), 520.0, |ui| {
        ui.label(Text::title(cx.tr.t("claude.endpoint.dialog_title"), cx.pal));
        dialog_body(ui, |ui| {
            ui.label(Text::small(cx.tr.t("claude.endpoint.dialog_body"), cx.pal));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(Text::small(cx.tr.t("claude.endpoint.preset"), cx.pal));
                let current = edit
                    .preset
                    .as_deref()
                    .and_then(|id| presets.iter().find(|p| p.id == id))
                    .map(|p| cx.tr.text(&p.label()))
                    .unwrap_or_else(|| cx.tr.t("claude.endpoint.custom"));
                let mut pick: Option<Option<&AnthropicPreset>> = None;
                Dropdown::new("claude-endpoint-preset")
                    .label(cx.tr.t("claude.endpoint.preset"))
                    .selected_text(current)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                edit.preset.is_none(),
                                cx.tr.t("claude.endpoint.custom"),
                            )
                            .clicked()
                        {
                            pick = Some(None);
                        }
                        for p in &presets {
                            let usable = p.base_url.is_some() || origin.is_some();
                            let on = edit.preset.as_deref() == Some(p.id);
                            let r = ui.add_enabled(
                                usable,
                                egui::Button::selectable(on, cx.tr.text(&p.label())),
                            );
                            if r.clicked() {
                                pick = Some(Some(p));
                            }
                        }
                    });
                if let Some(p) = pick {
                    match p {
                        Some(p) => {
                            edit.preset = Some(p.id.to_owned());
                            if let Some(e) = p.endpoint(origin.as_ref()) {
                                edit.url = e.base_text();
                                edit.auth = e.auth;
                            }
                        }
                        None => edit.preset = None,
                    }
                    edit.error = None;
                }
            });
            let field = ui
                .add(
                    TextEdit::singleline(&mut edit.url)
                        .hint_text("https://api.example.com/anthropic")
                        .font(mono_font(text::SMALL, Weight::Regular))
                        .desired_width(f32::INFINITY)
                        .margin(vec2(10.0, 6.0)),
                )
                .accessible_name(cx.tr.t("claude.endpoint.title"));
            if field.changed() {
                edit.preset = None;
                edit.error = None;
            }
            ui.horizontal(|ui| {
                ui.label(Text::small(cx.tr.t("claude.endpoint.auth"), cx.pal));
                Segmented::new("claude-endpoint-auth", &mut edit.auth)
                    .label(cx.tr.t("claude.endpoint.auth"))
                    .small()
                    .option(
                        AnthropicAuth::Bearer,
                        cx.tr.t("claude.endpoint.auth_bearer"),
                    )
                    .option(AnthropicAuth::ApiKey, cx.tr.t("claude.endpoint.auth_key"))
                    .show(ui);
            });
            if let Some(p) = edit.preset.as_deref().and_then(anthropic_preset) {
                ui.horizontal(|ui| {
                    if ui.link(cx.tr.t("claude.endpoint.docs")).clicked() {
                        out.push(Action::OpenUrl(p.doc_url.to_owned()));
                    }
                });
                if p.models == superba_core::claude::ModelsListing::No {
                    ui.label(Text::caption(cx.tr.t("claude.endpoint.no_list"), cx.pal));
                }
            }
            if let Some(e) = edit.error {
                crate::pages::note(ui, cx, Status::Error, cx.tr.t(e));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t("claude.endpoint.save")))
                .clicked()
            {
                match AnthropicEndpoint::parse_base(&edit.url) {
                    Ok(url) => {
                        save = Some(Some(AnthropicEndpoint {
                            base_url: url,
                            auth: edit.auth,
                            preset: edit.preset.clone(),
                        }));
                    }
                    Err(key) => edit.error = Some(key),
                }
            }
            if ui
                .add(Button::ghost(cx.tr.t("claude.endpoint.reset")))
                .on_hover_text(cx.tr.t("claude.endpoint.reset_tip"))
                .clicked()
            {
                save = Some(None);
            }
            if ui.add(Button::ghost(cx.tr.t("claude.cancel"))).clicked() {
                close = true;
            }
        });
    });
    if r.should_close() {
        close = true;
    }
    if let (Some(e), Some(mut p)) = (save, profile) {
        p.anthropic = e;
        out.push(Action::SaveProfile {
            profile: Box::new(p),
            key: None,
        });
        close = true;
    }
    if close {
        state.connect.endpoint = None;
    }
}

pub(super) fn plan(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State) {
    let Some(plan) = state.connect.plan.clone() else {
        return;
    };
    let mut close = false;
    let mut write = false;
    let r = dialog(ctx, egui::Id::unique("claude-plan"), 760.0, |ui| {
        ui.label(Text::title(
            cx.tr.t(if plan.kind == ClaudePlanKind::Restore {
                "claude.plan.title_restore"
            } else {
                "claude.plan.title"
            }),
            cx.pal,
        ));
        dialog_body(ui, |ui| {
            ui.label(Text::small(
                cx.tr.f(
                    "claude.plan.body",
                    &[(
                        "path",
                        &superba_core::paths::display_path(
                            &state.view.as_ref().expect("loaded view").settings_path,
                        ),
                    )],
                ),
                cx.pal,
            ));
            let changes: Vec<Change> = plan
                .changes
                .iter()
                .map(|c| {
                    let row = Change::new(&c.slot, c.before.clone(), c.after.clone());
                    if c.drift {
                        row.flag(cx.tr.t("claude.plan.drift"), Status::Warn)
                    } else {
                        row
                    }
                })
                .collect();
            let labels = ChangeLabels {
                none: cx.tr.t("claude.plan.absent"),
                removed: cx.tr.t("claude.plan.removed"),
            };
            ui.add_space(4.0);
            ChangeList::new(&changes, &labels).show(ui);
            crate::pages::shared::notices::notice_list(
                ui,
                cx,
                egui::Id::unique("claude-plan-notices"),
                &plan.notes,
            );
            if plan.drift().count() > 0 {
                ui.horizontal(|ui| {
                    ui.add(Toggle::new(
                        &mut state.connect.take_over,
                        cx.tr.t("claude.plan.take_over"),
                    ));
                    ui.label(Text::small(cx.tr.t("claude.plan.take_over"), cx.pal));
                });
            }
            ui.label(Text::caption(cx.tr.t("claude.plan.backup"), cx.pal));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t(
                    if plan.kind == ClaudePlanKind::Restore {
                        "claude.plan.restore"
                    } else {
                        "claude.plan.write"
                    },
                )))
                .clicked()
            {
                write = true;
            }
            if ui.add(Button::ghost(cx.tr.t("claude.cancel"))).clicked() {
                close = true;
            }
        });
    });
    if r.should_close() {
        close = true;
    }
    if write {
        state.connect.apply(cx);
    } else if close {
        state.connect.plan = None;
    }
}
