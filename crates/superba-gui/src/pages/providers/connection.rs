//! The Connection tab of a provider (UX §3.3): its lines ([`super::lines`]:
//! the line in use, switching and the order, latency, WebSocket / zstd support,
//! auth availability and the key check per line; a custom site: its base URL,
//! editable inline), the automatic authentication decision with the manual
//! choice under Advanced, the key (masked, where it is stored, check on all
//! lines, replace), and summaries of the provider's models and Codex settings
//! with links to the Models and Settings pages.

use egui::{Align, Layout, RichText, Sense, TextEdit, Ui, vec2};
use superba_core::{
    AuthChoice, AuthMode, AuthReason, KeyAccess, KeyInput, KeyRef, Line, LineCaps, ModelListSource,
    Profile, Secret, Site, SiteKind,
};

use super::{KeyEdit, State, form, ops};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::text;
use crate::design::widgets::{
    Button, KeyInput as KeyField, Latency, LatencyBadge, Segmented, Status, StatusDot, Text,
    link_button,
};
use crate::pages::shared::providers::{Provider, ProviderKind};
use crate::pages::shared::{auto_facts, decision_with, new_profile, ordered_lines};
use crate::pages::{Action, Cx, Page, divider, elided, error_text, key_access_status};

/// Width of the labels of the summary rows.
const LABEL_W: f32 = 72.0;
/// Height of a line row.
const LINE_H: f32 = 46.0;

/// Draw the tab.
pub(super) fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    p: &Provider,
    out: &mut Vec<Action>,
) {
    let site = &p.site;
    let profile = p.profile.as_ref();
    // The line probes this tab shows (once per site; Refresh probes again).
    if !site.lines.is_empty()
        && !cx.data.caps.contains_key(&site.id)
        && state.probed.insert(site.id.clone())
    {
        out.push(Action::Probe(site.id.clone()));
    }
    super::transports::poll(ui, cx, state, p);
    if p.kind == ProviderKind::Custom {
        base_url(ui, cx, state, p);
    } else {
        super::lines::show(ui, cx, state, p, out);
    }
    ui.add_space(6.0);
    divider(ui, cx);
    ui.add_space(6.0);
    if site.supports(AuthMode::Pat) {
        auth(ui, cx, state, p, out);
    }
    key(ui, cx, state, p, out);
    super::compaction::poll(ui, cx, p);
    super::compaction::show(ui, cx, p, out);
    if let Some(profile) = profile {
        models(ui, cx, state, profile, out);
        settings(ui, cx, profile, out);
    }
}

/// One summary row: label on the left, content after it, on one center line
/// ([`FormRow`](crate::design::widgets::FormRow)).
pub(super) fn row(ui: &mut Ui, cx: &Cx<'_>, label: &str, content: impl FnOnce(&mut Ui)) {
    crate::design::widgets::FormRow::new(Text::small(label, cx.pal))
        .label_width(LABEL_W)
        .gap(8.0)
        .height(32.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            content(ui);
        });
}

/// A summary row with actions on the right: the actions are laid out first
/// and keep their size, the text left of them is shortened instead (at
/// 960×640 a long reason or summary never squeezes a link into wrapping).
pub(super) fn row_split(
    ui: &mut Ui,
    cx: &Cx<'_>,
    label: &str,
    actions: impl FnOnce(&mut Ui),
    content: impl FnOnce(&mut Ui),
) {
    row(ui, cx, label, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            actions(ui);
            ui.with_layout(Layout::left_to_right(Align::Center), content);
        });
    });
}

/// What a line offers (PAT and API key, API key only) and the key check result.
pub(super) fn availability(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &State,
    p: &Provider,
    line: &Line,
    caps: Option<&LineCaps>,
) {
    let checking = state.check_task.as_ref().is_some_and(|(id, _)| id == &p.id);
    if checking {
        ui.add(StatusDot::new(Status::Busy));
        elided(ui, Text::small(cx.tr.t("lines.key.checking"), cx.pal));
        return;
    }
    let mode = decision_with(
        &p.site,
        Some(line),
        caps,
        p.profile.as_ref(),
        auto_facts(cx.data),
    )
    .mode
    .unwrap_or(AuthMode::ApiKey);
    let checked = state
        .checks
        .get(&p.id)
        .and_then(|c| c.iter().find(|(l, m, _)| l == &line.id && *m == mode))
        .map(|(_, _, r)| r);
    let caps_text = match caps {
        Some(c) if !c.reachable || c.codex_path.is_none() => Some(cx.tr.t("caps.unavailable")),
        Some(c) if c.allows(AuthMode::Pat) => Some(cx.tr.t("caps.pat_and_key")),
        Some(c) if c.allows(AuthMode::ApiKey) => Some(cx.tr.t("caps.key_only")),
        Some(_) => Some(cx.tr.t("caps.unavailable")),
        None => None,
    };
    let access = match checked {
        Some(Ok(k)) => Some(Ok(k.access.clone())),
        Some(Err(e)) => Some(Err(error_text(cx.tr, e))),
        None => caps
            .and_then(|c| c.key_access.clone())
            .filter(|a| !(mode == AuthMode::ApiKey && *a == KeyAccess::EdgeBlocked))
            .map(Ok),
    };
    match access {
        Some(Ok(a)) => {
            ui.add(StatusDot::new(key_access_status(&a)));
            let label = if a.is_ok() {
                caps_text.unwrap_or_else(|| cx.tr.text(&a.text()))
            } else {
                cx.tr.text(&a.text())
            };
            elided(ui, Text::small(label, cx.pal));
        }
        Some(Err(e)) => {
            ui.add(StatusDot::new(Status::Error));
            elided(ui, Text::small(e, cx.pal));
        }
        None => {
            if let Some(t) = caps_text {
                elided(ui, Text::small(t, cx.pal));
            }
        }
    }
}

/// A custom site's base URL (one line), editable inline, with its test result.
/// Width right of the Base URL field: the latency badge (80), the transport
/// badges and the auth note, with their gaps.
const BASE_URL_SIDE: f32 = 340.0;

fn base_url(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider) {
    let site = &p.site;
    let saved = p
        .profile
        .as_ref()
        .and_then(|p| p.base_url_override.as_ref())
        .map(|u| u.as_str().trim_end_matches('/').to_owned())
        .unwrap_or_else(|| form::base_url(site));
    let caps = site
        .default_line()
        .and_then(|l| cx.data.site_caps(&site.id, &l.id));
    let probing = cx.data.probing_site(&site.id);
    let url_label = ui
        .label(Text::strong(cx.tr.t("providers.base_url"), cx.pal))
        .id;
    ui.horizontal(|ui| {
        ui.set_min_height(LINE_H);
        ui.spacing_mut().item_spacing.x = 8.0;
        let editing = state
            .base_url
            .as_ref()
            .is_some_and(|(id, _)| id == &site.id);
        if !editing {
            state.base_url = Some((site.id.clone(), saved.clone()));
        }
        let buffer = &mut state.base_url.as_mut().expect("set above").1;
        let changed = buffer.trim().trim_end_matches('/') != saved;
        // Room right of the field: latency, the WS / zstd badges and the auth
        // note ("仅 API Key" was cut, review ux-visual #12), and Save while edited.
        let reserve = BASE_URL_SIDE + if changed { 72.0 } else { 0.0 };
        ui.add_enabled(
            p.profile.is_some() && !cx.busy && state.base_url_task.is_none(),
            TextEdit::singleline(buffer)
                .id(egui::Id::unique(("providers-base-url", site.id.as_str())))
                .font(egui::TextStyle::Monospace)
                .desired_width((ui.available_width() - reserve).max(200.0))
                .margin(vec2(10.0, 6.0)),
        )
        .labelled_by(url_label)
        .on_hover_text(cx.tr.t("providers.base_url_hint"));
        if changed
            && ui
                .add(
                    Button::primary(cx.tr.t("providers.base_url_save"))
                        .small()
                        .loading(state.base_url_task.is_some()),
                )
                .clicked()
        {
            save_base_url(state, cx, p);
        }
        let latency = match caps {
            None if probing => Latency::Probing,
            None => Latency::Unknown,
            Some(c) if !c.reachable => Latency::Unreachable,
            Some(c) => c.latency_ms.map_or(Latency::Unknown, Latency::Ms),
        };
        ui.add_sized(
            [80.0, 22.0],
            LatencyBadge::new(latency).name(crate::pages::overview::latency_text(cx, latency)),
        );
        if let Some(line) = site.default_line() {
            super::transports::badges(ui, cx, p, &line.id);
            availability(ui, cx, state, p, line, caps);
        }
    });
    super::transports::footer(ui, cx, p);
    if let Some(e) = &state.base_url_error {
        ui.label(Text::small(e, cx.pal).color(cx.pal.red));
    }
    let testing = state.tests.contains_key(&site.id);
    if let Some(report) = state.reports.get(&site.id) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            super::add::summary(ui, cx, report);
        });
    } else if testing {
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Busy));
            ui.label(Text::small(cx.tr.t("providers.test.running"), cx.pal));
        });
    }
}

/// Save an edited base URL (the site and its profile; the key stays).
fn save_base_url(state: &mut State, cx: &Cx<'_>, p: &Provider) {
    let (Some(profile), Some((_, text))) = (&p.profile, &state.base_url) else {
        return;
    };
    let form = form::SiteForm {
        mode: Some(form::FormMode::Edit {
            site: p.site.id.clone(),
            profile: profile.id.clone(),
        }),
        name: p.site.name.fallback.clone(),
        url: text.trim().to_owned(),
        key: String::new(),
        reveal: false,
        model: profile.model.model.clone().unwrap_or_default(),
        mapping: form::rows(&p.site.model_map),
    };
    match form::prepare(&form, &[], Some((&p.site, profile)), |id, name, url| {
        superba_switch::new_compatible_site(id, name, url)
    }) {
        Ok(mut prepared) => {
            prepared.profile.name = profile.name.clone();
            state.base_url_error = None;
            state.base_url_task = Some(crate::runtime::task(ops::update(
                cx.service.clone(),
                prepared,
            )));
        }
        Err(e) => {
            state.base_url_error = Some(super::add::form_error_text(cx, &e));
        }
    }
}

/// "Automatic → PAT · reason", with the manual choice under Advanced.
fn auth(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider, out: &mut Vec<Action>) {
    let site = &p.site;
    let profile = p.profile.as_ref();
    let lines = ordered_lines(site, profile);
    let primary = lines.first();
    let caps = primary.and_then(|l| cx.data.site_caps(&site.id, &l.id));
    let d = decision_with(site, primary, caps, profile, auto_facts(cx.data));
    // Advanced keeps its size; a long reason (router mode) is shortened.
    row_split(
        ui,
        cx,
        &cx.tr.t("providers.row.auth"),
        |ui| {
            let label = if state.advanced {
                cx.tr.t("connection.auth.hide_advanced")
            } else {
                cx.tr.t("connection.auth.advanced")
            };
            if link_button(ui, &label, false).clicked() {
                state.advanced = !state.advanced;
            }
        },
        |ui| {
            let status = match d.mode {
                None => Status::Error,
                Some(_) if d.fell_back() => Status::Idle,
                Some(_) => Status::Ok,
            };
            ui.add(StatusDot::new(status));
            let mode = d
                .mode
                .map(|m| cx.auth_short(m))
                .unwrap_or_else(|| cx.tr.t("connection.auth.none"));
            let main = if d.reason == AuthReason::Manual {
                cx.tr.f("connection.auth.manual", &[("mode", &mode)])
            } else {
                format!("{} → {mode}", cx.tr.text(&d.choice.text()))
            };
            ui.label(
                RichText::new(main)
                    .font(font(text::SMALL, Weight::Medium))
                    .color(cx.pal.text),
            );
            if d.reason != AuthReason::Manual {
                let line = primary.map(|l| cx.tr.text(&l.name)).unwrap_or_default();
                let reason = cx.tr.f(
                    "connection.auth.reason",
                    &[("line", &line), ("reason", &cx.localize(&d.reason.text()))],
                );
                let text = format!("· {reason}");
                let r = ui.add(egui::Label::new(Text::small(&text, cx.pal)).truncate());
                // The whole reason on hover, then its detail.
                let tip = match d.reason.detail() {
                    Some(detail) => format!("{reason}\n{}", cx.localize(detail)),
                    None => reason,
                };
                r.on_hover_text(tip);
            }
        },
    );
    if state.advanced {
        let mut choice = profile.map_or(AuthChoice::Auto, Profile::auth_choice);
        let before = choice;
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W + 8.0);
            ui.add_enabled_ui(profile.is_some() && !cx.busy, |ui| {
                let mut seg = Segmented::new("providers-auth", &mut choice).small();
                for c in AuthChoice::ALL {
                    let usable = match c {
                        AuthChoice::Pat => site.supports(AuthMode::Pat),
                        AuthChoice::ApiKey => site.supports(AuthMode::ApiKey),
                        _ => true,
                    };
                    if usable {
                        seg = seg.option(c, cx.tr.text(&c.text()));
                    }
                }
                seg.show(ui);
            });
            ui.add(
                egui::Label::new(Text::caption(
                    cx.tr.t("connection.auth.advanced_body"),
                    cx.pal,
                ))
                .truncate(),
            )
            .on_hover_text(cx.tr.t("connection.auth.advanced_body"));
        });
        if choice != before
            && let Some(p) = profile
        {
            let mut changed = p.clone();
            changed.set_auth(choice);
            out.push(Action::SaveProfile {
                profile: Box::new(changed),
                key: None,
            });
        }
    }
}

/// What a stored key is shown with before its last four characters.
fn key_prefix(site: &Site) -> &'static str {
    if site.kind == SiteKind::Krill {
        "nb_…"
    } else {
        "…"
    }
}

/// The key: masked with its storage, check on all lines, replace; or the editor.
fn key(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider, out: &mut Vec<Action>) {
    let site = &p.site;
    let profile = p.profile.as_ref();
    let stored = profile.and_then(|p| p.key_ref.as_ref());
    let editing = state
        .key_edit
        .as_ref()
        .is_some_and(|k| k.provider.as_ref() == Some(&p.id))
        || stored.is_none();
    if !editing && let Some(profile) = profile {
        let last4 = (p.applied)
            .then(|| cx.data.active_key_last4().map(str::to_owned))
            .flatten()
            .or_else(|| crate::pages::profile_key_suffix(ui, cx, Some(profile)));
        row_split(
            ui,
            cx,
            &cx.tr.t("providers.row.key"),
            |ui| {
                if ui
                    .add(Button::new(cx.tr.t("providers.key.replace")).small())
                    .clicked()
                {
                    state.key_edit = Some(KeyEdit {
                        provider: Some(p.id.clone()),
                        ..KeyEdit::default()
                    });
                }
                let label = if site.lines.len() > 1 {
                    cx.tr.t("providers.key.check_all")
                } else {
                    cx.tr.t("providers.key.check")
                };
                if ui
                    .add(
                        Button::ghost(label)
                            .small()
                            .loading(state.check_task.is_some())
                            .enabled(!cx.busy && state.check_task.is_none()),
                    )
                    .on_hover_text(cx.tr.t("lines.key.cost"))
                    .clicked()
                {
                    super::detail::start_checks(
                        state,
                        cx,
                        p,
                        KeyInput::Profile(profile.id.clone()),
                    );
                }
            },
            |ui| {
                let (icon, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                icons::paint(ui.painter(), icon, Icon::Key, cx.pal.text_muted);
                ui.label(
                    RichText::new(format!("{}{}", key_prefix(site), last4.unwrap_or_default()))
                        .font(mono_font(text::SMALL, Weight::Medium))
                        .color(cx.pal.text),
                );
                let where_key = match stored {
                    Some(KeyRef::Keyring { .. }) => cx.tr.t("lines.key.in_keyring"),
                    Some(KeyRef::File { .. }) => cx.tr.t("lines.key.in_file"),
                    Some(KeyRef::Env { var }) => cx.tr.f("lines.key.in_env", &[("var", var)]),
                    _ => String::new(),
                };
                elided(ui, Text::small(where_key, cx.pal));
            },
        );
        return;
    }
    // The editor (no key yet, or replacing).
    if state
        .key_edit
        .as_ref()
        .is_none_or(|k| k.provider.as_ref() != Some(&p.id))
    {
        state.key_edit = Some(KeyEdit {
            provider: Some(p.id.clone()),
            ..KeyEdit::default()
        });
    }
    let placeholder = if site.kind == SiteKind::Krill {
        "nb_…".to_owned()
    } else {
        cx.tr.t("connection.key.placeholder_compatible")
    };
    let mut save = false;
    let mut check = false;
    let mut cancel = false;
    let edit = state.key_edit.as_mut().expect("set above");
    row(ui, cx, &cx.tr.t("providers.row.key"), |ui| {
        let r = KeyField::new(
            ("providers-key", p.id.command()),
            &mut edit.key,
            &mut edit.reveal,
            &placeholder,
            cx.tr.t("lines.key.reveal"),
        )
        .width(260.0)
        .show(ui);
        let has_text = !edit.key.trim().is_empty();
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        save = (ui
            .add(
                Button::primary(cx.tr.t("lines.key.save"))
                    .small()
                    .enabled(has_text && !cx.busy),
            )
            .clicked()
            || enter)
            && has_text;
        check = ui
            .add(
                Button::new(cx.tr.t("providers.key.check"))
                    .small()
                    .enabled(has_text && state.check_task.is_none()),
            )
            .on_hover_text(cx.tr.t("lines.key.cost"))
            .clicked();
        if stored.is_some()
            && ui
                .add(Button::ghost(cx.tr.t("action.cancel")).small())
                .clicked()
        {
            cancel = true;
        }
    });
    let typed = Secret::new(edit.key.trim().to_owned());
    if check {
        super::detail::start_checks(state, cx, p, KeyInput::Secret(typed.clone()));
    }
    if save {
        let profile = profile
            .cloned()
            .unwrap_or_else(|| new_profile(site, cx.data.site_name(cx.tr, site)));
        out.push(Action::SaveProfile {
            profile: Box::new(profile),
            key: Some(typed),
        });
        state.key_edit = None;
        state.checks.remove(&p.id);
        // Checks were made with the old key.
        super::transports::forget(ui, p);
    } else if cancel {
        state.key_edit = None;
    }
}

/// "11 models · from the provider · default gpt-5.5 · 2 overrides  Manage →".
fn models(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, profile: &Profile, out: &mut Vec<Action>) {
    state.want_models(cx, &profile.id);
    let default = profile
        .model
        .model
        .clone()
        .unwrap_or_else(|| cx.tr.t("models.codex_default"));
    let overrides = profile.model_overrides.len();
    let text = match state.models.get(&profile.id) {
        Some(Ok(v)) => {
            let source = match &v.list.source {
                ModelListSource::Upstream { .. } | ModelListSource::Union { .. } => {
                    cx.tr.t("providers.models.source.upstream")
                }
                _ => cx.tr.t("providers.models.source.builtin"),
            };
            cx.tr.f(
                "providers.models.summary",
                &[
                    ("n", &v.list.models.len()),
                    ("source", &source),
                    ("model", &default),
                    ("overrides", &overrides),
                ],
            )
        }
        _ => cx.tr.f(
            "providers.models.summary_short",
            &[("model", &default), ("overrides", &overrides)],
        ),
    };
    row_split(
        ui,
        cx,
        &cx.tr.t("providers.row.models"),
        |ui| {
            if link_button(ui, &cx.tr.t("providers.models.manage"), false).clicked() {
                out.push(Action::PageCommand {
                    page: Page::Models,
                    command: format!("profile:{}", profile.id),
                });
            }
        },
        |ui| {
            elided(ui, Text::small(text, cx.pal).color(cx.pal.text));
        },
    );
}

/// "27 Codex settings are written with this provider  Open settings →".
fn settings(ui: &mut Ui, cx: &Cx<'_>, profile: &Profile, out: &mut Vec<Action>) {
    let n = profile.codex_settings.len();
    row_split(
        ui,
        cx,
        &cx.tr.t("providers.row.settings"),
        |ui| {
            if link_button(ui, &cx.tr.t("providers.settings.open"), false).clicked() {
                out.push(Action::PageCommand {
                    page: Page::Settings,
                    command: format!("profile:{}", profile.id),
                });
            }
        },
        |ui| {
            elided(
                ui,
                Text::small(cx.tr.f("providers.settings.summary", &[("n", &n)]), cx.pal)
                    .color(cx.pal.text),
            );
        },
    );
}
