//! The wizard's steps (UX §3.2), background reads and the custom-site draft.

use egui::{Align, Align2, Frame, Layout, Margin, Rect, Sense, Stroke, TextEdit, Ui, pos2, vec2};
use superba_core::{AuthMode, AuthShape, KeyAccess, LegacySessionChoice, LineCaps, Profile, Site};

use super::{Step, Wizard, legacy_count, probe_request};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::size;
use crate::design::tokens::{radius, text};
use crate::design::widgets::FormRow;
use crate::design::widgets::choice_card::choice_card_ui;
use crate::design::widgets::{
    Badge, Button, Card, Choice, Dropdown, KeyInput, Latency, LatencyBadge, Status, StatusDot,
    Text, choice_cards, link_button,
};
use crate::pages::{Action, Cx, auth_mode_for_line, key_access_status};

// ---------------------------------------------------------------------------
// Background reads
// ---------------------------------------------------------------------------

/// Poll the wizard's background reads (key in auth.json, probe) once per frame.
pub(super) fn poll(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, _out: &mut Vec<Action>) {
    let mut pending = false;
    load_original_key(cx, w);
    if let Some(read) = &w.original_read {
        if let Some(result) = read.poll() {
            match result {
                Ok(key) => w.original_key = key,
                Err(error) => w.original_error = Some(error),
            }
            w.original_read = None;
        } else {
            pending = true;
        }
    }
    if let Some(task) = &w.probe {
        if let Some(result) = task.poll() {
            pick_best_line(cx, w, result.as_ref().ok());
            w.probe_report = Some(result);
            w.probe = None;
        } else {
            pending = true;
        }
    }
    if pending {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }
}

/// Read the key of the current configuration when it belongs to the chosen
/// site (offered as "use the key found in auth.json").
fn load_original_key(cx: &Cx<'_>, w: &mut Wizard) {
    if w.original_requested || w.official() || w.custom() {
        return;
    }
    let (Some(site), Some(config)) = (
        w.site.clone(),
        cx.data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.config.as_ref()),
    ) else {
        return;
    };
    if w.editing_profile(cx.data)
        .is_some_and(|p| p.key_ref.is_some())
    {
        return;
    }
    w.original_requested = true;
    if crate::runtime::config_matches_site(config, &site)
        && crate::pages::auth_last4(&config.auth).is_some()
    {
        w.original_read = Some(crate::runtime::original_key_task(config.clone(), site));
    }
}

/// Probe the chosen line with the wizard's key (line latency, key access and the
/// site's model list for the default-model picker).
fn start_probe(cx: &Cx<'_>, w: &mut Wizard) {
    let Some(site) = w.site.as_ref() else {
        return;
    };
    let line = if w.custom() { None } else { w.line.clone() };
    let key = w.key_input(cx.data);
    let wanted = (site.id.clone(), line.clone());
    if w.probe_for.as_ref() == Some(&wanted) && (w.probe.is_some() || w.probe_report.is_some()) {
        return;
    }
    w.probe_for = Some(wanted);
    w.probe_report = None;
    let mut request = probe_request(&site.id, line, key, w.custom() && w.custom_responses);
    if w.custom() {
        // The draft is tested as it is, before anything is saved.
        request.site = Some(site.clone());
    }
    let service = cx.service.clone();
    w.probe = Some(crate::runtime::task(async move {
        service.probe_site(request).await
    }));
}

/// Pre-select the reachable line with the lowest latency until the user picks
/// one.
fn pick_best_line(cx: &Cx<'_>, w: &mut Wizard, _report: Option<&superba_core::SiteProbeReport>) {
    if w.line_chosen {
        return;
    }
    let Some(site) = w.site.as_ref() else {
        return;
    };
    let best = site
        .lines
        .iter()
        .filter_map(|l| {
            let caps = cx.data.site_caps(&site.id, &l.id)?;
            (caps.reachable && caps.codex_path.is_some())
                .then_some((caps.latency_ms.unwrap_or(u32::MAX), l.id.clone()))
        })
        .min();
    if let Some((_, line)) = best {
        w.line = Some(line);
    }
}

/// Entering a step starts what it needs.
pub(super) fn on_enter(cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) {
    match w.step {
        Step::Detect | Step::Confirm
            if cx.data.sessions.value.is_none() && !cx.data.sessions.loading =>
        {
            out.push(Action::LoadSessions);
        }
        Step::LineModels => {
            if let Some(site) = w.site.clone() {
                if !w.custom() && !cx.data.probing_site(&site.id) && !site.lines.is_empty() {
                    out.push(Action::Probe(site.id.clone()));
                }
                if w.line.is_none() {
                    w.line = site.default_line.clone();
                }
            }
            queue_key_checks(cx, w, out);
            start_probe(cx, w);
        }
        _ => {}
    }
}

/// Check the key on every probed line (Automatic: the mode the line can use).
fn queue_key_checks(cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) {
    if w.custom() || w.official() {
        return;
    }
    let Some(site) = w.site.clone() else {
        return;
    };
    let key = w.selected_key();
    if key != w.checked_key {
        w.checked_key = key.clone();
        w.checked_modes.clear();
    }
    let stored = w
        .editing_profile(cx.data)
        .filter(|p| p.key_ref.is_some())
        .map(|p| p.id.clone());
    for line in &site.lines {
        // Wait for capabilities, then validate the mode this line can use.
        let Some(caps) = cx.data.site_caps(&site.id, &line.id) else {
            continue;
        };
        let mode = auth_mode_for_line(AuthMode::Pat, Some(caps));
        if key.is_none() && stored.is_none() {
            continue;
        }
        if !w.checked_modes.insert((line.id.clone(), mode)) {
            continue;
        }
        out.push(Action::CheckKey {
            site: site.id.clone(),
            profile: stored.clone(),
            line: line.id.clone(),
            mode,
            key: key.clone(),
        });
    }
}

/// The selected line's key check rejected the key (unknown or revoked, no
/// quota, no Codex access): continuing would only fail the switch. Network
/// errors and other unclear results do not stop the wizard.
pub(super) fn rejected_on_line(cx: &Cx<'_>, w: &Wizard) -> Option<KeyAccess> {
    let line = w.line.as_ref()?;
    let site = w.site.as_ref()?;
    let mode = auth_mode_for_line(AuthMode::Pat, cx.data.site_caps(&site.id, line));
    match cx.data.key_checks.get(&(line.clone(), mode)) {
        Some(Ok(k))
            if matches!(
                k.access,
                KeyAccess::InvalidKey | KeyAccess::QuotaExhausted | KeyAccess::ChannelDenied
            ) =>
        {
            Some(k.access.clone())
        }
        _ => None,
    }
}

/// The custom site's test passed: the site answered and the key was not
/// rejected (a site whose key check is unclear may still be used).
pub(super) fn custom_test_passed(w: &Wizard) -> bool {
    match &w.probe_report {
        Some(Ok(r)) => {
            r.caps.reachable
                && !r.key_check.as_ref().is_some_and(|k| {
                    matches!(
                        k.access,
                        KeyAccess::InvalidKey
                            | KeyAccess::QuotaExhausted
                            | KeyAccess::ChannelDenied
                    )
                })
        }
        _ => false,
    }
}

/// Build the custom site and its profile from the form (a re-run edits the
/// stored site and profile) as an unsaved draft, which the next step tests and
/// Apply saves. `false` (with an error under the form) when the form is not
/// valid; nothing is ever written here.
pub(super) fn draft_custom(cx: &Cx<'_>, w: &mut Wizard) -> bool {
    let name = w.custom_name.trim().to_owned();
    let url = match url::Url::parse(w.custom_url.trim()) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u,
        _ => {
            w.custom.error = Some(cx.tr.t("wizard.custom.invalid_url"));
            return false;
        }
    };
    let (id, existing): (superba_core::SiteId, Option<Profile>) = match &w.custom.saved {
        Some((site, profile)) => (site.clone(), cx.data.profile(profile).cloned()),
        None => {
            let taken: Vec<String> = cx
                .data
                .sites
                .iter()
                .map(|s| s.id.to_string())
                .chain(cx.data.profiles.value.iter().flat_map(|set| {
                    set.profiles
                        .iter()
                        .map(|p| p.id.to_string())
                        .chain(set.custom_sites.iter().map(|s| s.id.to_string()))
                }))
                .collect();
            let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
            (super::custom_site_id(&name, &taken).into(), None)
        }
    };
    let mut site = match superba_switch::new_compatible_site(id.clone(), &name, &url) {
        Ok(s) => s,
        Err(e) => {
            w.custom.error = Some(crate::pages::error_text(cx.tr, &e));
            return false;
        }
    };
    // A re-run keeps the stored site's model map.
    if let Some(stored) = cx.data.site(&id) {
        site.model_map = stored.model_map.clone();
    }
    let mut profile = existing.unwrap_or_else(|| {
        let mut p = crate::pages::shared::new_profile(&site, name.clone());
        p.id = id.as_str().into();
        p
    });
    profile.name = name;
    if w.key.trim().is_empty() && profile.key_ref.is_none() {
        w.custom.error = Some(cx.tr.t("wizard.custom.key_required"));
        return false;
    }
    w.custom.error = None;
    // A changed form is tested again.
    w.probe = None;
    w.probe_for = None;
    w.probe_report = None;
    w.line = site.default_line.clone();
    w.site = Some(site.clone());
    w.custom.draft = Some((site, profile));
    true
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------

pub(super) fn stepper(ui: &mut Ui, cx: &Cx<'_>, w: &Wizard) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let n = Step::ALL.len();
    let gap = 6.0;
    let seg_w = (rect.width() - gap * (n as f32 - 1.0)) / n as f32;
    let current = w.step.index();
    for (i, step) in Step::ALL.iter().enumerate() {
        let x = rect.min.x + i as f32 * (seg_w + gap);
        let bar = Rect::from_min_size(pos2(x, rect.min.y), vec2(seg_w, 3.0));
        let skipped = *step == Step::LineModels && w.official();
        let color = if i <= current && !skipped {
            cx.pal.text
        } else {
            cx.pal.active
        };
        ui.painter().rect_filled(bar, 2, color);
        let tcolor = if i == current {
            cx.pal.text
        } else if i < current && !skipped {
            cx.pal.text_muted
        } else {
            cx.pal.text_subtle
        };
        ui.painter().text(
            pos2(x, rect.min.y + 12.0),
            Align2::LEFT_TOP,
            cx.tr.t(step.label_key()),
            font(
                text::CAPTION,
                if i == current {
                    Weight::Medium
                } else {
                    Weight::Regular
                },
            ),
            tcolor,
        );
    }
}

fn heading(ui: &mut Ui, cx: &Cx<'_>, title: &str, body: &str) {
    ui.label(Text::h1(title, cx.pal));
    ui.add_space(4.0);
    ui.add(egui::Label::new(Text::muted(body, cx.pal)).wrap());
    ui.add_space(18.0);
}

fn check_row(ui: &mut Ui, cx: &Cx<'_>, status: Status, title: String, detail: String) {
    ui.horizontal_top(|ui| {
        ui.set_min_height(if detail.is_empty() { 24.0 } else { 36.0 });
        let (r, _) = ui.allocate_exact_size(vec2(10.0, 20.0), Sense::hover());
        StatusDot::new(status).paint_at(ui, r.center());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(Text::strong(title, cx.pal));
            if !detail.is_empty() {
                ui.add(egui::Label::new(Text::small(detail, cx.pal)).wrap());
            }
        });
    });
}

/// A selectable radio card; returns true when clicked.
fn radio_card(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    selected: bool,
    add: impl FnOnce(&mut Ui),
) -> bool {
    // The option-card look of the provider step and the Add dialog (user bug
    // 4, review ux-visual #16): gray fill and a check when selected.
    let id = ui.scope_id().with(("wizard-radio", id));
    let r = choice_card_ui(ui, id, label, selected, true, add);
    if r.gained_focus() {
        r.scroll_to_me(Some(Align::Center));
    }
    r.clicked()
}

/// One field of [`review_changes`]: label, the value now and after Apply.
pub struct FieldChange {
    /// Field name.
    pub label: String,
    /// Value now.
    pub old: String,
    /// Value after Apply.
    pub new: String,
}

impl FieldChange {
    /// The value changes.
    pub fn changed(&self) -> bool {
        self.old != self.new
    }
}

/// Line, authentication and default model of the edited provider now and after
/// Apply. "Now" is what the provider uses: its applied line and mode when it is
/// the applied provider.
pub fn field_changes(
    cx: &Cx<'_>,
    site: &Site,
    before: &Profile,
    after: &Profile,
    line: Option<&superba_core::LineId>,
    decision: &superba_core::AuthDecision,
) -> Vec<FieldChange> {
    let tr = cx.tr;
    let line_name = |l: &superba_core::LineId| cx.data.line_label(tr, Some(&site.id), l);
    let old_line = super::current_line(cx.data, before);
    let applied = cx
        .data
        .active_state()
        .filter(|a| a.profile_id.as_ref() == Some(&before.id));
    let old_mode = applied
        .and_then(|a| a.auth_mode)
        .or_else(|| super::auth_decision(cx, site, old_line.as_ref(), Some(before)).mode);
    let auth = |choice: superba_core::AuthChoice, mode: Option<AuthMode>| {
        let mode = mode.map_or_else(|| tr.t("wizard.review.none"), |m| cx.auth_short(m));
        if choice.is_auto() {
            tr.f("wizard.review.auto_mode", &[("mode", &mode)])
        } else {
            mode
        }
    };
    let model = |m: &Option<String>| {
        m.clone()
            .unwrap_or_else(|| tr.t("wizard.review.model_default"))
    };
    let none = tr.t("wizard.review.none");
    vec![
        FieldChange {
            label: tr.t("wizard.review.field.line"),
            old: old_line.as_ref().map_or_else(|| none.clone(), line_name),
            new: line.map_or_else(|| none.clone(), line_name),
        },
        FieldChange {
            label: tr.t("wizard.review.field.auth"),
            old: auth(before.auth_choice(), old_mode),
            new: auth(after.auth_choice(), decision.mode),
        },
        FieldChange {
            label: tr.t("wizard.review.field.model"),
            old: model(&before.model.model),
            new: model(&after.model.model),
        },
    ]
}

/// The fields of [`field_changes`]: "old → new" for a change, the value and
/// "unchanged" otherwise.
fn review_changes(
    ui: &mut Ui,
    cx: &Cx<'_>,
    site: &Site,
    before: &Profile,
    after: &Profile,
    line: Option<&superba_core::LineId>,
    decision: &superba_core::AuthDecision,
) {
    ui.label(Text::caption(cx.tr.t("wizard.review.changes"), cx.pal));
    for f in field_changes(cx, site, before, after, line, decision) {
        FormRow::new(&f.label).label_width(96.0).show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if f.changed() {
                ui.label(Text::body(f.old, cx.pal).color(cx.pal.text_muted));
                ui.label(Text::body("→", cx.pal).color(cx.pal.text_muted));
                ui.label(Text::strong(f.new, cx.pal));
            } else {
                ui.label(Text::body(f.new, cx.pal));
                ui.label(Text::small(cx.tr.t("wizard.review.unchanged"), cx.pal));
            }
        });
    }
}

fn note_box(ui: &mut Ui, cx: &Cx<'_>, text: String) {
    Frame::new()
        .fill(cx.pal.bg_subtle)
        .corner_radius(radius::CONTROL)
        .stroke(Stroke::new(1.0, cx.pal.border))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
        });
}

// ---------------------------------------------------------------------------
// 1 Detect
// ---------------------------------------------------------------------------

pub(super) fn detect(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) {
    heading(
        ui,
        cx,
        &cx.tr.t("wizard.detect.title"),
        &cx.tr.t("wizard.detect.body"),
    );
    let Some(o) = cx.data.overview.value.as_ref() else {
        ui.label(Text::muted(cx.tr.t("overview.loading"), cx.pal));
        return;
    };
    if cx.data.sessions.value.is_none() && !cx.data.sessions.loading {
        out.push(Action::LoadSessions);
    }
    Card::new().show(ui, |ui| {
        match o.app.installs.first() {
            Some(i) => check_row(
                ui,
                cx,
                Status::Ok,
                cx.tr.t("wizard.detect.app"),
                format!(
                    "{} · {}",
                    i.app_version.clone().unwrap_or_default(),
                    i.path.display()
                ),
            ),
            None => check_row(
                ui,
                cx,
                Status::Idle,
                cx.tr.t("wizard.detect.no_app"),
                cx.tr.t("wizard.detect.no_app_body"),
            ),
        }
        match o.cli.binaries.first() {
            Some(b) => check_row(
                ui,
                cx,
                Status::Ok,
                cx.tr.t("wizard.detect.cli"),
                format!(
                    "codex {} · {}",
                    b.version
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    b.path.display()
                ),
            ),
            None => check_row(
                ui,
                cx,
                Status::Idle,
                cx.tr.t("wizard.detect.no_cli"),
                String::new(),
            ),
        }
        if o.app.installs.is_empty() || o.cli.binaries.is_empty() {
            ui.horizontal_wrapped(|ui| {
                if o.app.installs.is_empty()
                    && link_button(ui, &cx.tr.t("pages.detect.download"), true).clicked()
                {
                    out.push(Action::OpenUrl("https://chatgpt.com/download".into()));
                }
                if o.cli.binaries.is_empty()
                    && link_button(ui, "npm i -g @openai/codex", false)
                        .on_hover_text(cx.tr.t("pages.detect.copy_install"))
                        .clicked()
                {
                    out.push(Action::Copy("npm i -g @openai/codex".into()));
                }
            });
            ui.add_space(8.0);
        }
        // SSH hosts the app knows (app records only; the remote-control devices
        // and WSL distributions it reports are not configured, user bug 36);
        // green only when one of them is connected now (review ux-visual #15).
        let known: Vec<&superba_core::RemoteHost> = match cx.data.hosts.value.as_ref() {
            Some(h) => h.iter().map(|h| &h.host).collect(),
            None => o.remotes.iter().map(|r| &r.host).collect(),
        };
        let hosts: Vec<(String, bool)> = known
            .into_iter()
            .filter(|h| h.is_managed())
            .map(|h| {
                (
                    crate::pages::shared::hosts::title(cx.tr, h),
                    h.connected == Some(true),
                )
            })
            .collect();
        let live = hosts.iter().filter(|(_, on)| *on).count();
        check_row(
            ui,
            cx,
            if live > 0 { Status::Ok } else { Status::Idle },
            if hosts.is_empty() {
                cx.tr.t("wizard.detect.remotes_none")
            } else {
                cx.tr.f(
                    "wizard.detect.remotes",
                    &[("n", &hosts.len()), ("live", &live)],
                )
            },
            hosts
                .iter()
                .map(|(l, _)| l.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        let (status, label) = cx.hot_status(&o.app.hot_switch);
        check_row(ui, cx, status, cx.tr.t("wizard.detect.hot"), label);
        // Current configuration.
        let config = o.config.as_ref().map(|c| {
            let auth = match &c.auth {
                AuthShape::Missing => cx.tr.t("auth.shape.missing"),
                AuthShape::ChatGpt { email, .. } => format!(
                    "{} {}",
                    cx.tr.t("auth.shape.chatgpt"),
                    email.clone().unwrap_or_default()
                )
                .trim()
                .to_owned(),
                AuthShape::ApiKey { key_last4 } => {
                    format!("{} ••••{key_last4}", cx.tr.t("auth.shape.apikey"))
                }
                AuthShape::Pat { key_last4 } => format!("PAT ••••{key_last4}"),
                _ => cx.tr.t("auth.shape.unknown"),
            };
            let base = c
                .openai_base_url
                .clone()
                .unwrap_or_else(|| cx.tr.t("wizard.detect.default_url"));
            format!("{base} · {auth}")
        });
        let original = match (o.baseline_captured, o.baseline_at) {
            (true, Some(at)) => cx
                .tr
                .f("wizard.detect.original_kept_at", &[("when", &cx.ago(at))]),
            (true, None) => cx.tr.t("wizard.detect.original_kept"),
            _ => cx.tr.t("wizard.detect.original_will_save"),
        };
        check_row(ui, cx, Status::Idle, original, config.unwrap_or_default());
        // Sessions under another provider (hidden in the app until migrated).
        match legacy_count(cx) {
            Some((provider, count)) => check_row(
                ui,
                cx,
                Status::Warn,
                cx.tr.f(
                    "wizard.legacy.title",
                    &[("count", &count), ("provider", &provider)],
                ),
                cx.tr.t("wizard.detect.legacy_later"),
            ),
            None if cx.data.sessions.loading => check_row(
                ui,
                cx,
                Status::Busy,
                cx.tr.t("wizard.detect.scanning"),
                String::new(),
            ),
            None => {}
        }
    });
    let _ = w;
}

// ---------------------------------------------------------------------------
// 2 Provider
// ---------------------------------------------------------------------------

pub(super) fn provider(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, _out: &mut Vec<Action>) {
    heading(
        ui,
        cx,
        &cx.tr.t("wizard.provider.title"),
        &cx.tr.t("wizard.provider.body"),
    );
    // The same option cards as the Add provider dialog.
    let items: Vec<Choice<String>> = w
        .presets
        .iter()
        .map(|preset| {
            let (title, body) = preset_text(cx, preset);
            Choice {
                value: preset.id.clone(),
                title,
                body: (!body.is_empty()).then_some(body),
                badge: preset.default.then(|| cx.tr.t("wizard.preset.default")),
                enabled: true,
            }
        })
        .collect();
    let mut pick = w.preset.clone();
    if choice_cards(ui, &mut pick, &items) {
        w.select_preset(cx.data, &pick);
    }
    ui.add_space(12.0);
    if let Some(home) = w
        .chosen()
        .filter(|p| p.key_placeholder.is_some())
        .and_then(|p| p.site.homepage.clone())
    {
        let row_h = ui.spacing().interact_size.y;
        FormRow::unlabeled().height(row_h).show(ui, |ui| {
            let host = home.host_str().unwrap_or(home.as_str()).to_owned();
            ui.label(Text::small(cx.tr.t("wizard.provider.get_key"), cx.pal));
            if link_button(ui, &host, true).clicked() {
                _out.push(Action::OpenUrl(home.to_string()));
            }
        });
    }
    if w.rerun {
        ui.add_space(8.0);
        ui.label(Text::small(cx.tr.t("wizard.rerun.keeps"), cx.pal));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("wizard.provider.import"))
                    .icon(Icon::Download)
                    .small(),
            )
            .on_hover_text(cx.tr.t("wizard.provider.import_tip"))
            .clicked()
        {
            w.import = Some(crate::pages::import::ImportDialog::open(cx));
        }
    });
}

/// Title and body of a preset card.
pub fn preset_text(cx: &Cx<'_>, preset: &superba_core::catalog::Preset) -> (String, String) {
    let key = |k: &str| format!("wizard.preset.{}.{k}", preset.id);
    let title_key = key("title");
    let body_key = key("body");
    let title = match cx.tr.t(&title_key) {
        t if t == title_key => cx.tr.text(&preset.site.name),
        t => t,
    };
    let body = match cx.tr.t(&body_key) {
        t if t == body_key => String::new(),
        t => t,
    };
    (title, body)
}

// ---------------------------------------------------------------------------
// 3 Credentials
// ---------------------------------------------------------------------------

pub(super) fn credentials(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) {
    if w.official() {
        heading(
            ui,
            cx,
            &cx.tr.t("wizard.credentials.official.title"),
            &cx.tr.t("wizard.credentials.official.body"),
        );
        note_box(ui, cx, cx.tr.t("wizard.credentials.official.note"));
        return;
    }
    if w.custom() {
        custom_form(ui, cx, w);
        return;
    }
    let site_name = w
        .site
        .as_ref()
        .map(|s| cx.tr.text(&s.name))
        .unwrap_or_default();
    heading(
        ui,
        cx,
        &cx.tr
            .f("wizard.credentials.key.title", &[("provider", &site_name)]),
        &cx.tr.t("wizard.credentials.key.body"),
    );
    if let Some(key) = &w.original_key {
        let title = cx
            .tr
            .f("pages.wizard.found_key", &[("last4", &key.last4())]);
        let chosen = w.use_original_key && w.key.trim().is_empty();
        if radio_card(ui, "found-key", &title.clone(), chosen, |ui| {
            ui.label(Text::strong(title, cx.pal));
            ui.label(Text::small(cx.tr.t("pages.wizard.found_key_body"), cx.pal));
        }) {
            w.use_original_key = true;
            w.key.clear();
        }
        ui.add_space(8.0);
    } else if w.original_read.is_some() {
        ui.label(Text::small(
            cx.tr.t("wizard.credentials.reading_key"),
            cx.pal,
        ));
    } else if w.original_error.is_some() {
        crate::pages::note(
            ui,
            cx,
            Status::Warn,
            cx.tr.t("wizard.credentials.read_key_failed"),
        );
    }
    let placeholder = w
        .chosen()
        .and_then(|p| p.key_placeholder.clone())
        .unwrap_or_default();
    KeyInput::new(
        "wizard-key",
        &mut w.key,
        &mut w.reveal,
        &placeholder,
        cx.tr.t("wizard.credentials.reveal"),
    )
    .show(ui);
    if !w.key.trim().is_empty() {
        w.use_original_key = false;
    }
    if let Some(profile) = w.editing_profile(cx.data).filter(|p| p.key_ref.is_some())
        && w.key.trim().is_empty()
        && !(w.use_original_key && w.original_key.is_some())
    {
        ui.add_space(8.0);
        let last4 = crate::pages::profile_key_suffix(ui, cx, Some(profile));
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Ok));
            ui.label(Text::small(
                match last4 {
                    Some(l) => cx.tr.f("wizard.credentials.saved_key", &[("last4", &l)]),
                    None => cx.tr.t("wizard.credentials.saved_key_plain"),
                },
                cx.pal,
            ));
        });
    }
    let _ = out;
}

fn custom_form(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard) {
    heading(
        ui,
        cx,
        &cx.tr.t("wizard.credentials.custom.title"),
        &cx.tr.t("wizard.credentials.custom.body"),
    );
    let field = |ui: &mut Ui, label: String, value: &mut String, hint: &str, mono: bool| {
        ui.label(Text::caption(label, cx.pal));
        let mut edit = TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(ui.available_width());
        if mono {
            edit = edit.font(egui::TextStyle::Monospace);
        }
        ui.add(edit);
        ui.add_space(8.0);
    };
    field(
        ui,
        cx.tr.t("wizard.custom.name"),
        &mut w.custom_name,
        "My API",
        false,
    );
    field(
        ui,
        cx.tr.t("wizard.custom.url"),
        &mut w.custom_url,
        "https://api.example.com/v1",
        true,
    );
    ui.label(Text::caption(cx.tr.t("wizard.custom.key"), cx.pal));
    KeyInput::new(
        "wizard-custom-key",
        &mut w.key,
        &mut w.reveal,
        "",
        cx.tr.t("wizard.credentials.reveal"),
    )
    .show(ui);
    if w.custom.saved.is_some() && w.key.trim().is_empty() {
        ui.add_space(4.0);
        ui.label(Text::small(cx.tr.t("wizard.custom.key_kept"), cx.pal));
    }
    ui.add_space(8.0);
    ui.checkbox(&mut w.custom_responses, cx.tr.t("wizard.custom.responses"));
    ui.add_space(8.0);
    note_box(ui, cx, cx.tr.t("wizard.custom.saved_note"));
    if let Some(e) = &w.custom.error {
        ui.add_space(8.0);
        crate::pages::note(ui, cx, Status::Error, e.clone());
    }
}

// ---------------------------------------------------------------------------
// 4 Line & models
// ---------------------------------------------------------------------------

pub(super) fn line_models(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) {
    queue_key_checks(cx, w, out);
    let Some(site) = w.site.clone() else {
        return;
    };
    if w.custom() {
        heading(
            ui,
            cx,
            &cx.tr.t("wizard.custom.test.title"),
            &cx.tr.t("wizard.custom.test.body"),
        );
        custom_probe(ui, cx, w);
    } else {
        heading(
            ui,
            cx,
            &cx.tr.t("wizard.line.title"),
            &cx.tr.t(if keeps_current_line(cx, w) {
                "wizard.line.body_current"
            } else {
                "wizard.line.body"
            }),
        );
        for line in &site.lines {
            line_card(ui, cx, w, &site, line);
            ui.add_space(8.0);
        }
        if let Some(access) = rejected_on_line(cx, w) {
            crate::pages::note(
                ui,
                cx,
                Status::Error,
                cx.tr.f(
                    "pages.wizard.key_rejected",
                    &[("reason", &cx.tr.text(&access.text()))],
                ),
            );
        }
    }
    ui.add_space(16.0);
    model_picker(ui, cx, w);
}

/// A re-run selected the edited provider's current line (not the fastest one).
fn keeps_current_line(cx: &Cx<'_>, w: &Wizard) -> bool {
    !w.custom()
        && w.editing_profile(cx.data)
            .and_then(|p| super::current_line(cx.data, p))
            .is_some_and(|l| w.line.as_ref() == Some(&l))
}

fn line_card(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, site: &Site, line: &superba_core::Line) {
    let caps = cx.data.site_caps(&site.id, &line.id);
    let mode = auth_mode_for_line(AuthMode::Pat, caps);
    let selected = w.line.as_ref() == Some(&line.id);
    let probing = cx.data.probing_site(&site.id);
    let name = cx.tr.text(&line.name);
    let clicked = radio_card(ui, ("line", line.id.as_str()), &name, selected, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(Text::strong(cx.tr.text(&line.name), cx.pal));
                crate::pages::elided(ui, Text::mono_muted(line.origin.as_str(), cx.pal));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(LatencyBadge::new(latency(caps, probing)));
            });
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let key = (line.id.clone(), mode);
            let api_only =
                caps.is_some_and(|c| !c.allows(AuthMode::Pat) && c.allows(AuthMode::ApiKey));
            if cx.data.key_checking.contains(&key) {
                ui.add(Badge::new(cx.tr.t("wizard.line.checking")).status(Status::Busy));
            } else if let Some(Ok(k)) = cx.data.key_checks.get(&key) {
                let label = if k.access.is_ok() && api_only {
                    cx.tr.t("pages.key.api_only")
                } else {
                    cx.tr.text(&k.access.text())
                };
                ui.add(Badge::new(label).status(key_access_status(&k.access)));
            } else if let Some(c) = caps {
                ui.add(Badge::new(caps_label(cx, c)));
            }
        });
    });
    if clicked {
        w.line = Some(line.id.clone());
        w.line_chosen = true;
    }
}

fn latency(caps: Option<&LineCaps>, probing: bool) -> Latency {
    match caps {
        None if probing => Latency::Probing,
        None => Latency::Unknown,
        Some(c) if !c.reachable => Latency::Unreachable,
        Some(c) => c.latency_ms.map_or(Latency::Unknown, Latency::Ms),
    }
}

fn caps_label(cx: &Cx<'_>, c: &LineCaps) -> String {
    if !c.reachable || c.codex_path.is_none() {
        cx.tr.t("caps.unavailable")
    } else if c.allows(AuthMode::Pat) {
        cx.tr.t("caps.pat_and_key")
    } else if c.allows(AuthMode::ApiKey) {
        cx.tr.t("caps.key_only")
    } else {
        cx.tr.t("caps.unavailable")
    }
}

/// The custom site's probe: latency, key, model list, optional Responses call.
fn custom_probe(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard) {
    let Some(result) = &w.probe_report else {
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Busy));
            ui.label(Text::small(cx.tr.t("wizard.custom.testing"), cx.pal));
        });
        return;
    };
    let report = match result {
        Ok(r) => r,
        Err(e) => {
            crate::pages::note(ui, cx, Status::Error, crate::pages::error_text(cx.tr, e));
            return;
        }
    };
    Card::new().show(ui, |ui| {
        let caps = &report.caps;
        let (status, latency) = if caps.reachable {
            (
                Status::Ok,
                caps.latency_ms
                    .map_or_else(|| "—".into(), |ms| format!("{ms} ms")),
            )
        } else {
            (Status::Error, cx.tr.t("caps.unavailable"))
        };
        check_row(ui, cx, status, cx.tr.t("wizard.custom.latency"), latency);
        if let Some(k) = &report.key_check {
            check_row(
                ui,
                cx,
                key_access_status(&k.access),
                cx.tr.t("wizard.custom.key_check"),
                cx.tr.text(&k.access.text()),
            );
        }
        match (&report.models, &report.models_error) {
            (Some(models), _) => check_row(
                ui,
                cx,
                Status::Ok,
                cx.tr.t("wizard.custom.models"),
                cx.tr
                    .f("wizard.models.from_upstream", &[("n", &models.len())]),
            ),
            (None, Some(e)) => check_row(
                ui,
                cx,
                Status::Warn,
                cx.tr.t("wizard.custom.models"),
                crate::pages::error_text(cx.tr, e),
            ),
            (None, None) => {}
        }
        match (&report.responses, &report.responses_error) {
            (Some(r), _) => check_row(
                ui,
                cx,
                if r.ok { Status::Ok } else { Status::Error },
                cx.tr.t("wizard.custom.responses_result"),
                r.latency_ms
                    .map(|ms| format!("{ms} ms"))
                    .or_else(|| r.detail.clone())
                    .unwrap_or_default(),
            ),
            (None, Some(e)) => check_row(
                ui,
                cx,
                Status::Warn,
                cx.tr.t("wizard.custom.responses_result"),
                crate::pages::error_text(cx.tr, e),
            ),
            (None, None) => {}
        }
    });
}

/// The models offered by the picker: the probe's model list (the line's for a
/// preset, the draft site's `/v1/models` for a custom site); with a note when
/// the site lists none.
fn picker_models(w: &Wizard) -> (Vec<(String, String)>, bool) {
    if let Some(Ok(report)) = &w.probe_report
        && let Some(models) = &report.models
    {
        let list = models
            .iter()
            .map(|m| {
                (
                    m.id.clone(),
                    m.display_name.clone().unwrap_or_else(|| m.id.clone()),
                )
            })
            .collect();
        return (list, true);
    }
    (Vec::new(), false)
}

fn model_picker(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard) {
    ui.label(Text::title(cx.tr.t("wizard.models.title"), cx.pal));
    ui.add_space(4.0);
    let loading = w.probe.is_some();
    let (models, upstream) = picker_models(w);
    let default_label = cx.tr.t("wizard.models.codex_default");
    let selected_label = w
        .model
        .as_ref()
        .map(|m| {
            models
                .iter()
                .find(|(slug, _)| slug == m)
                .map_or_else(|| m.clone(), |(_, label)| label.clone())
        })
        .unwrap_or_else(|| default_label.clone());
    FormRow::unlabeled().height(size::CONTROL).show(ui, |ui| {
        Dropdown::new("wizard-model")
            .medium()
            .width(320.0)
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut w.model, None, default_label.clone());
                for (slug, label) in &models {
                    let text = if label == slug {
                        slug.clone()
                    } else {
                        format!("{label} · {slug}")
                    };
                    ui.selectable_value(&mut w.model, Some(slug.clone()), text);
                }
            });
        if loading {
            ui.add(StatusDot::new(Status::Busy));
        }
    });
    let note = if loading {
        None
    } else if upstream && !models.is_empty() {
        Some(
            cx.tr
                .f("wizard.models.from_upstream", &[("n", &models.len())]),
        )
    } else if !models.is_empty() {
        Some(cx.tr.t("wizard.models.builtin"))
    } else {
        Some(cx.tr.t("wizard.models.none"))
    };
    if let Some(note) = note {
        ui.add_space(2.0);
        ui.label(Text::small(note, cx.pal));
    }
}

// ---------------------------------------------------------------------------
// 5 Confirm
// ---------------------------------------------------------------------------

pub(super) fn confirm(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard) {
    heading(
        ui,
        cx,
        &cx.tr.t("wizard.review.title"),
        &cx.tr.t("wizard.review.body"),
    );
    Card::new().show(ui, |ui| {
        ui.label(Text::caption(cx.tr.t("wizard.review.destination"), cx.pal));
        if w.official() {
            ui.label(Text::h2(cx.tr.t("wizard.preset.official.title"), cx.pal));
            ui.add(egui::Label::new(Text::small(cx.tr.t("wizard.review.official"), cx.pal)).wrap());
            return;
        }
        let Some(site) = w.site.clone() else {
            return;
        };
        let line = w.line.as_ref().and_then(|l| site.line(l)).cloned();
        let mut dest = vec![cx.tr.text(&site.name)];
        if let Some(l) = &line
            && site.lines.len() > 1
        {
            dest.push(cx.tr.text(&l.name));
        }
        ui.label(Text::h2(dest.join(" · "), cx.pal));
        // The authentication of the profile Apply saves (a re-run keeps the
        // provider's choice; `planned_profile`).
        let planned = if w.custom() {
            w.custom.draft.as_ref().map(|(_, p)| p.clone())
        } else {
            super::planned_profile(cx, w)
        };
        let decision = super::auth_decision(
            cx,
            &site,
            w.line.as_ref(),
            planned.as_ref().or(w.editing_profile(cx.data)),
        );
        let mode = decision.mode.unwrap_or(AuthMode::ApiKey);
        let auth_key = if decision.choice.is_auto() {
            "wizard.review.auth"
        } else {
            "wizard.review.auth_manual"
        };
        ui.add(
            egui::Label::new(Text::small(
                cx.tr.f(
                    auth_key,
                    &[
                        ("mode", &cx.auth_short(mode)),
                        ("reason", &cx.localize(&decision.reason.text())),
                    ],
                ),
                cx.pal,
            ))
            .wrap(),
        );
        // A re-run edits a provider in use: what changes against now (review
        // ux-visual #6).
        if let (Some(before), Some(after)) = (w.editing_profile(cx.data), planned.as_ref())
            && !w.custom()
        {
            ui.add_space(10.0);
            review_changes(ui, cx, &site, before, after, w.line.as_ref(), &decision);
        }
        ui.add_space(12.0);
        let origin = line.as_ref().map(|l| l.origin.as_str()).unwrap_or_default();
        let mut items = Vec::new();
        if !cx
            .data
            .overview
            .value
            .as_ref()
            .is_some_and(|o| o.baseline_captured)
        {
            items.push(cx.tr.t("wizard.review.baseline"));
        }
        items.push(cx.tr.f("wizard.review.config", &[("origin", &origin)]));
        if w.custom() || w.selected_key().is_some() {
            items.push(cx.tr.t("wizard.review.key"));
        }
        if let Some(model) = &w.model {
            items.push(cx.tr.f("wizard.review.model", &[("model", model)]));
        }
        if mode == AuthMode::Pat {
            items.push(cx.tr.f("wizard.review.env", &[("origin", &origin)]));
        }
        items.push(cx.tr.t(if w.editing.is_some() {
            "wizard.review.edits"
        } else {
            "wizard.review.creates"
        }));
        for item in items {
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r.shrink(1.5), Icon::Check, cx.pal.text_muted);
                ui.add(egui::Label::new(Text::body(item, cx.pal)).wrap());
            });
        }
    });
    if let Some((provider, count)) = legacy_count(cx) {
        ui.add_space(14.0);
        ui.label(Text::title(
            cx.tr.f(
                "wizard.legacy.title",
                &[("count", &count), ("provider", &provider)],
            ),
            cx.pal,
        ));
        ui.add_space(8.0);
        if radio_card(
            ui,
            "legacy-migrate",
            &cx.tr.t("wizard.legacy.migrate"),
            w.legacy == Some(LegacySessionChoice::Migrate),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(Text::strong(cx.tr.t("wizard.legacy.migrate"), cx.pal));
                    ui.add(Badge::new(cx.tr.t("wizard.recommended")));
                });
                ui.add(
                    egui::Label::new(Text::small(cx.tr.t("wizard.legacy.migrate_body"), cx.pal))
                        .wrap(),
                );
            },
        ) {
            w.legacy = Some(LegacySessionChoice::Migrate);
        }
        ui.add_space(8.0);
        if radio_card(
            ui,
            "legacy-skip",
            &cx.tr.t("wizard.legacy.skip"),
            w.legacy == Some(LegacySessionChoice::Skip),
            |ui| {
                ui.label(Text::strong(cx.tr.t("wizard.legacy.skip"), cx.pal));
                ui.add(
                    egui::Label::new(Text::small(
                        cx.tr.f("wizard.legacy.skip_body", &[("count", &count)]),
                        cx.pal,
                    ))
                    .wrap(),
                );
            },
        ) {
            w.legacy = Some(LegacySessionChoice::Skip);
        }
    }
    ui.add_space(12.0);
    note_box(ui, cx, cx.tr.t("wizard.review.note"));
}
