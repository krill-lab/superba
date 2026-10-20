//! The 连接 section: four setting rows — the provider, the route, the
//! provider's Anthropic endpoint (copy, check, change) and Claude Code with
//! its `settings.json`.

use egui::Ui;
use superba_core::claude::anthropic_preset;
use superba_core::{
    AnthropicAuth, ClaudeConfig, ClaudeFileStatus, ClaudeIneligible, ClaudeRoute, ClaudeView,
    ProfileId,
};

use super::{
    EndpointEdit, State, current_provider, endpoint_of, help_line, provider_label, providers,
    section_head, setting_row,
};
use crate::design::elide::Shape;
use crate::design::icons::Icon;
use crate::design::tokens::{size, space, text};
use crate::design::widgets::value_chip::ValueChip;
use crate::design::widgets::{Button, Dropdown, IconButton, Segmented, Status};
use crate::pages::shared::loading;
use crate::pages::{Action, Cx};

/// Minimum width of the provider dropdown.
const PROVIDER_W: f32 = 240.0;
/// Room of the route's segmented control.
const ROUTE_W: f32 = 200.0;
/// Room of an address or a path with its buttons (at 960×640 the help
/// beside it keeps about 300 points).
pub(super) const VALUE_W: f32 = 380.0;

pub(super) fn section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
    out: &mut Vec<Action>,
) {
    section_head(ui, cx, &cx.tr.t("claude.section.connection"), None, |_| {});
    provider_row(ui, cx, state, view, config);
    route_row(ui, cx, state, view, config);
    endpoint_row(ui, cx, state, view, config, out);
    claude_row(ui, cx, view, out);
}

fn provider_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
) {
    let current = current_provider(view, config);
    // A provider without an Anthropic endpoint: why it cannot serve Claude
    // Code directly, or that the router converts for it.
    let missing = current
        .as_ref()
        .filter(|id| endpoint_of(cx, id).is_none())
        .map(|_| {
            if config.route == ClaudeRoute::Router {
                (Status::Idle, cx.tr.t("claude_routing.translated"))
            } else {
                (
                    Status::Warn,
                    cx.tr.text(&ClaudeIneligible::NoAnthropicEndpoint.text()),
                )
            }
        });
    setting_row(
        ui,
        cx,
        &cx.tr.t("claude.provider.title"),
        |ui| match &missing {
            Some((status, text)) => help_line(ui, cx, Some(*status), text),
            None => help_line(ui, cx, None, &cx.tr.t("claude.provider.help")),
        },
        false,
        PROVIDER_W + space::L,
        |ui| provider_picker(ui, cx, state, view, config),
    );
}

fn provider_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
) {
    let list = providers(cx);
    let follows = config.provider.is_none();
    let current = current_provider(view, config);
    let name = |id: &ProfileId| {
        cx.data
            .profile(id)
            .map(|p| provider_label(cx, p))
            .unwrap_or_else(|| id.to_string())
    };
    let selected = match (&current, follows) {
        (Some(id), true) => cx
            .tr
            .f("claude.provider.follows_named", &[("name", &name(id))]),
        (Some(id), false) => name(id),
        (None, _) => cx.tr.t("claude.provider.none"),
    };
    let mut pick: Option<Option<ProfileId>> = None;
    Dropdown::new("claude-provider")
        .label(cx.tr.t("claude.provider.title"))
        .selected_text(selected)
        .loading(state.connect.load.loading())
        .width(PROVIDER_W.min(ui.available_width()))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(follows, cx.tr.t("claude.provider.follow"))
                .clicked()
            {
                pick = Some(None);
            }
            for (p, endpoint) in &list {
                let label = provider_label(cx, p);
                let on = !follows && config.provider.as_ref() == Some(&p.id);
                if endpoint.is_some() || config.route == ClaudeRoute::Router {
                    if ui.selectable_label(on, label).clicked() {
                        pick = Some(Some(p.id.clone()));
                    }
                } else {
                    ui.add_enabled(false, egui::Button::selectable(false, label))
                        .on_disabled_hover_text(
                            cx.tr.text(&ClaudeIneligible::NoAnthropicEndpoint.text()),
                        );
                }
            }
        });
    if let Some(p) = pick {
        state.connect.edit(view, |c| c.provider = p.clone());
        if let Some(id) = p.or_else(|| view.provider.clone()) {
            state.connect.load_models(cx, &id, false);
        }
    }
}

fn route_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
) {
    let router_ready = view.router_base_url.is_some();
    let help = match (&view.router_base_url, config.route) {
        (Some(url), ClaudeRoute::Router) => cx.tr.f("claude.route.via", &[("url", &url.as_str())]),
        (None, _) => cx.tr.t("claude.route.help_soon"),
        _ => cx.tr.t("claude.route.help_direct"),
    };
    setting_row(
        ui,
        cx,
        &cx.tr.t("claude.route.title"),
        |ui| help_line(ui, cx, None, &help),
        false,
        ROUTE_W,
        |ui| {
            let mut route = config.route;
            let seg = Segmented::new("claude-route", &mut route)
                .label(cx.tr.t("claude.route.title"))
                .small()
                .option(ClaudeRoute::Direct, cx.tr.t("claude.route.direct"));
            let seg = if router_ready {
                seg.option(ClaudeRoute::Router, cx.tr.t("claude.route.router"))
            } else {
                seg.disabled_option(
                    ClaudeRoute::Router,
                    cx.tr.t("claude.route.router"),
                    cx.tr.t("claude.route.router_soon"),
                )
            };
            seg.show(ui);
            if route != config.route {
                state.connect.edit(view, |c| c.route = route);
            }
        },
    );
}

fn auth_label(cx: &Cx<'_>, a: AnthropicAuth) -> String {
    cx.tr.t(match a {
        AnthropicAuth::Bearer => "claude.endpoint.auth_bearer",
        AnthropicAuth::ApiKey => "claude.endpoint.auth_key",
    })
}

/// The model the endpoint check asks: the default model, else the first
/// slot's.
fn probe_model(config: &ClaudeConfig, provider: Option<&ProfileId>) -> Option<String> {
    provider.and_then(|p| {
        let m = config.models_of(p);
        m.main
            .clone()
            .or_else(|| m.slots.values().next().map(|s| s.model.clone()))
    })
}

fn endpoint_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &ClaudeView,
    config: &ClaudeConfig,
    out: &mut Vec<Action>,
) {
    let provider = current_provider(view, config);
    let endpoint = provider.as_ref().and_then(|p| endpoint_of(cx, p));
    let probing = state.connect.probe_task.is_some();
    let probe = state.connect.probe.clone();
    setting_row(
        ui,
        cx,
        &cx.tr.t("claude.endpoint.title"),
        |ui| {
            let Some(e) = &endpoint else {
                help_line(ui, cx, Some(Status::Warn), &cx.tr.t("claude.endpoint.none"));
                return;
            };
            let mut facts = Vec::new();
            if let Some(p) = e.preset.as_deref().and_then(anthropic_preset) {
                facts.push(cx.tr.text(&p.label()));
            }
            facts.push(auth_label(cx, e.auth));
            if view.endpoint_suggested && e.preset.is_some() {
                facts.push(cx.tr.t("claude.endpoint.suggested"));
            }
            help_line(ui, cx, None, &facts.join(" · "));
            if probing {
                loading::spinner_small(ui, cx, &cx.tr.t("claude.endpoint.checking"));
            } else if let Some(p) = &probe {
                let (status, msg) = if p.ok {
                    (
                        Status::Ok,
                        cx.tr.f(
                            "claude.endpoint.check_ok",
                            &[("ms", &p.latency_ms), ("model", &p.model)],
                        ),
                    )
                } else {
                    let error = p.message.clone().unwrap_or_else(|| {
                        p.status.map(|s| format!("HTTP {s}")).unwrap_or_default()
                    });
                    (
                        Status::Error,
                        cx.tr
                            .f("claude.endpoint.check_failed", &[("error", &error)]),
                    )
                };
                help_line(ui, cx, Some(status), &msg);
            }
        },
        false,
        VALUE_W,
        |ui| {
            // Right to left: Change, Check, Copy, then the address in the room
            // that is left.
            if let Some(p) = &provider
                && ui
                    .add(Button::new(cx.tr.t("claude.endpoint.edit")).small())
                    .clicked()
            {
                let e = endpoint_of(cx, p);
                state.connect.endpoint = Some(EndpointEdit {
                    provider: p.clone(),
                    preset: e.as_ref().and_then(|e| e.preset.clone()),
                    url: e.as_ref().map(|e| e.base_text()).unwrap_or_default(),
                    auth: e.map(|e| e.auth).unwrap_or_default(),
                    error: None,
                });
            }
            let Some(e) = &endpoint else {
                return;
            };
            let model = probe_model(config, provider.as_ref());
            let check = ui
                .add(
                    Button::new(cx.tr.t("claude.endpoint.check"))
                        .small()
                        .loading(probing)
                        .enabled(model.is_some() && !probing),
                )
                .on_hover_text(cx.tr.t("claude.endpoint.check_tip"))
                .on_disabled_hover_text(cx.tr.t("claude.endpoint.check_needs_model"));
            if check.clicked() {
                state.connect.probe(cx, model);
            }
            let url = e.base_text();
            if ui
                .add(
                    IconButton::new(Icon::Copy, cx.tr.t("claude.endpoint.copy"))
                        .size(size::CONTROL_SM),
                )
                .clicked()
            {
                out.push(Action::Copy(url.clone()));
            }
            let room = ui.available_width();
            ui.add(
                ValueChip::new(&url, Shape::Url)
                    .plain()
                    .size(text::SMALL)
                    .max_width(room)
                    .tooltip(url.clone()),
            );
        },
    );
}

fn claude_row(ui: &mut Ui, cx: &Cx<'_>, view: &ClaudeView, out: &mut Vec<Action>) {
    let path = superba_core::paths::display_path(&view.settings_path);
    setting_row(
        ui,
        cx,
        &cx.tr.t("claude.status.title"),
        |ui| {
            match &view.install {
                Some(i) => help_line(
                    ui,
                    cx,
                    Some(Status::Ok),
                    &cx.tr.f(
                        "claude.status.version",
                        &[("version", &i.version.clone().unwrap_or_else(|| "?".into()))],
                    ),
                ),
                None => help_line(
                    ui,
                    cx,
                    Some(Status::Warn),
                    &cx.tr.t("claude.status.not_found"),
                ),
            }
            let (status, text) = file_state(cx, view);
            help_line(ui, cx, Some(status), &text);
        },
        true,
        VALUE_W,
        |ui| {
            if ui
                .add(
                    IconButton::new(Icon::Copy, cx.tr.t("claude.status.copy_path"))
                        .size(size::CONTROL_SM),
                )
                .clicked()
            {
                out.push(Action::Copy(path.clone()));
            }
            let room = ui.available_width();
            ui.add(
                ValueChip::new(&path, Shape::Path)
                    .plain()
                    .size(text::SMALL)
                    .max_width(room)
                    .tooltip(path.clone()),
            );
        },
    );
    ui.add_space(space::XS);
}

/// The state of `settings.json`: not valid, or what Superba applied there.
fn file_state(cx: &Cx<'_>, view: &ClaudeView) -> (Status, String) {
    match &view.file {
        ClaudeFileStatus::Absent | ClaudeFileStatus::Valid => {}
        _ => return (Status::Error, cx.tr.t("claude.status.file_invalid_short")),
    }
    match &view.applied {
        Some(a) if !a.drift.is_empty() => (
            Status::Warn,
            cx.tr.f("claude.status.drift", &[("n", &a.drift.len())]),
        ),
        Some(_) if !view.pending.is_empty() => (
            Status::Warn,
            cx.tr
                .f("claude.status.pending", &[("n", &view.pending.len())]),
        ),
        Some(a) => (
            Status::Ok,
            cx.tr.f(
                "claude.status.applied",
                &[("n", &a.owned.len()), ("at", &cx.ago(a.at))],
            ),
        ),
        None if view.file == ClaudeFileStatus::Absent => {
            (Status::Idle, cx.tr.t("claude.status.file_absent_short"))
        }
        None => (Status::Idle, cx.tr.t("claude.status.not_applied")),
    }
}
