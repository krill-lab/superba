//! "Add provider" (UX §3.3): step 1 picks a preset from `catalog::presets()`
//! (Krill pre-selected; the official login opens its detail; a custom
//! OpenAI-compatible site), step 2 asks for what that preset needs — a key (and
//! a check on every line) for a preset site; name, base URL and API key with a
//! test (reachability, key, `/v1/models`, optionally one Responses call) for a
//! custom site. "Add" (the primary action) only saves the provider: Codex files
//! and processes are untouched, and a toast offers "Switch to this provider".
//! Switching right away is opt-in ("Switch to it right after adding", off by
//! default; a custom site must pass the test first) and plans the switch, which
//! shows its restart prompt. The same quick-pick as the setup wizard. Step 1
//! also offers "Import from other tools" (cc-switch, AI Toolbox; see
//! [`crate::pages::import`]).

use egui::{Align, Layout, TextEdit, Ui, vec2};
use superba_core::catalog::Preset;
use superba_core::{AuthMode, KeyInput, Profile, Secret, SiteId, catalog};

use super::form::{self, FormError, SiteForm};
use super::ops::{self, SiteReport};
use super::{LineChecks, State};
use crate::design::icons::Icon;
use crate::design::widgets::FormRow;
use crate::design::widgets::{
    Badge, Button, Choice, KeyInput as KeyField, Status, StatusDot, Text, choice_cards, dialog,
    link_button,
};
use crate::pages::shared::providers::ProviderId;
use crate::pages::shared::{auto_facts, decision_with, new_profile};
use crate::pages::{Action, Cx, error_text, key_access_status};
use crate::runtime::ReadTask;

/// Step of the dialog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AddStep {
    /// Pick a preset.
    #[default]
    Choose,
    /// Key of a preset site.
    Preset,
    /// A custom OpenAI-compatible site.
    Custom,
}

/// State of the Add provider dialog.
pub struct AddDialog {
    /// Step.
    pub step: AddStep,
    /// Picked preset id (`catalog::PRESET_*`).
    pub pick: String,
    /// Name of the new provider.
    pub name: String,
    /// Base URL (custom sites).
    pub url: String,
    /// Key.
    pub key: String,
    /// Show the key.
    pub reveal: bool,
    /// Also send one Responses request when testing (a few tokens).
    pub responses: bool,
    /// Switch to the new provider right after adding it (opt-in, off by
    /// default: adding only saves; switching restarts Codex).
    pub switch_after: bool,
    /// Validation or save error.
    pub error: Option<String>,
    /// Last test of a custom site.
    pub report: Option<SiteReport>,
    /// The (URL, key) the report was made with.
    tested: Option<(String, String)>,
    test: Option<ReadTask<SiteReport>>,
    /// Key checks on every line of a preset site.
    pub checks: Option<LineChecks>,
    check_task: Option<ReadTask<LineChecks>>,
    saving: Option<(bool, ReadTask<Profile>)>,
    focused: bool,
    /// "Import from other tools" was chosen: the page replaces this dialog with
    /// the import dialog.
    pub open_import: bool,
}

impl std::fmt::Debug for AddDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddDialog")
            .field("step", &self.step)
            .field("pick", &self.pick)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("key_entered", &!self.key.trim().is_empty())
            .finish_non_exhaustive()
    }
}

impl Default for AddDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl AddDialog {
    /// Step 1 with the default preset selected.
    pub fn new() -> Self {
        let pick = catalog::presets()
            .into_iter()
            .find(|p| p.default)
            .map(|p| p.id)
            .unwrap_or_else(|| catalog::PRESET_CUSTOM.to_owned());
        Self {
            step: AddStep::Choose,
            pick,
            name: String::new(),
            url: String::new(),
            key: String::new(),
            reveal: false,
            responses: false,
            switch_after: false,
            error: None,
            report: None,
            tested: None,
            test: None,
            checks: None,
            check_task: None,
            saving: None,
            focused: false,
            open_import: false,
        }
    }

    /// The picked preset.
    pub fn preset(&self) -> Option<Preset> {
        catalog::presets().into_iter().find(|p| p.id == self.pick)
    }

    /// A save is running.
    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The custom site passed a test with the current URL and key.
    pub fn tested_ok(&self) -> bool {
        let current = (self.url.trim().to_owned(), self.key.trim().to_owned());
        self.tested.as_ref() == Some(&current)
            && self
                .report
                .as_ref()
                .is_some_and(|r| r.reachable() && r.key_ok())
    }

    /// Save the new provider of step 2 ("Add" or, with `switch`, "Add and
    /// switch"): a preset profile with its key, or a custom site and its profile.
    /// Validation errors are shown in the dialog.
    pub fn start_save(&mut self, cx: &Cx<'_>, switch: bool) {
        if self.saving.is_some() {
            return;
        }
        self.error = None;
        match self.step {
            AddStep::Preset => {
                let Some(preset) = self.preset() else {
                    return;
                };
                let (name, key) = (self.name.trim().to_owned(), self.key.trim().to_owned());
                if name.is_empty() || key.is_empty() {
                    self.error = Some(cx.tr.t("providers.add.required"));
                    return;
                }
                let site = cx.data.site(&preset.site.id).unwrap_or(preset.site);
                let taken = taken_ids(cx);
                let mut profile = new_profile(&site, name.clone());
                profile.id = form::preset_profile_id(&site.id, &name, &taken);
                self.reveal = false;
                self.saving = Some((
                    switch,
                    crate::runtime::task(ops::add_preset(
                        cx.service.clone(),
                        profile,
                        Secret::new(key),
                    )),
                ));
            }
            AddStep::Custom => {
                if switch && !self.tested_ok() {
                    self.error = Some(cx.tr.t("providers.add.test_first"));
                    return;
                }
                let site_form = SiteForm {
                    mode: Some(form::FormMode::Add),
                    name: self.name.clone(),
                    url: self.url.clone(),
                    key: self.key.clone(),
                    reveal: false,
                    model: String::new(),
                    mapping: Vec::new(),
                };
                let taken = taken_ids(cx);
                match form::prepare(&site_form, &taken, None, |id, name, url| {
                    superba_switch::new_compatible_site(id, name, url)
                }) {
                    Ok(prepared) => {
                        self.reveal = false;
                        self.saving = Some((
                            switch,
                            crate::runtime::task(ops::add(cx.service.clone(), prepared)),
                        ));
                    }
                    Err(e) => self.error = Some(form_error_text(cx, &e)),
                }
            }
            AddStep::Choose => {}
        }
    }

    /// Test the typed custom site with the typed key (reachability, key,
    /// `/v1/models`, the optional Responses call).
    pub fn start_test(&mut self, cx: &Cx<'_>) {
        if self.test.is_some() {
            return;
        }
        self.error = None;
        self.tested = Some((self.url.trim().to_owned(), self.key.trim().to_owned()));
        self.test = Some(start_test(cx, self));
    }

    /// Set a test result (tests).
    pub fn set_report(&mut self, report: SiteReport) {
        self.tested = Some((self.url.trim().to_owned(), self.key.trim().to_owned()));
        self.report = Some(report);
    }

    /// Collect finished work: (still running, finished with the provider to
    /// select).
    pub(super) fn poll(
        &mut self,
        cx: &Cx<'_>,
        out: &mut Vec<Action>,
    ) -> (bool, Option<Option<ProviderId>>) {
        let mut running = false;
        if let Some(task) = &self.test {
            match task.poll() {
                Some(r) => {
                    self.test = None;
                    self.report = r.ok();
                }
                None => running = true,
            }
        }
        if let Some(task) = &self.check_task {
            match task.poll() {
                Some(r) => {
                    self.check_task = None;
                    match r {
                        Ok(c) => self.checks = Some(c),
                        Err(e) => self.error = Some(error_text(cx.tr, &e)),
                    }
                }
                None => running = true,
            }
        }
        if let Some((switch, task)) = &self.saving {
            let switch = *switch;
            match task.poll() {
                Some(Ok(profile)) => {
                    self.saving = None;
                    let text = cx.tr.f("providers.add.added", &[("name", &profile.name)]);
                    out.push(Action::Refresh);
                    if switch {
                        // Opted in: plan the switch (its prompt says what restarts).
                        out.push(Action::Toast {
                            status: Status::Ok,
                            text,
                        });
                        out.push(Action::Switch {
                            profile: profile.id.clone(),
                            line: None,
                            auth: None,
                        });
                    } else {
                        // Saved only; switching stays a separate, explicit click.
                        out.push(Action::OfferSwitch {
                            text,
                            profile: profile.id.clone(),
                        });
                    }
                    return (false, Some(Some(ProviderId::Profile(profile.id))));
                }
                Some(Err(e)) => {
                    self.saving = None;
                    self.error = Some(error_text(cx.tr, &e));
                }
                None => running = true,
            }
        }
        (running, None)
    }
}

/// Translated form error.
pub(super) fn form_error_text(cx: &Cx<'_>, e: &FormError) -> String {
    match e {
        FormError::Rejected(message) => cx.tr.f("sites.form.rejected", &[("detail", message)]),
        FormError::DuplicateMapping(model) => {
            cx.tr.f("sites.form.duplicate_mapping", &[("model", model)])
        }
        other => cx.tr.t(other.key()),
    }
}

/// Title and body of a preset in the quick-pick.
fn preset_text(cx: &Cx<'_>, p: &Preset) -> (String, String) {
    let title_key = format!("providers.preset.{}.title", p.id);
    let body_key = format!("providers.preset.{}.body", p.id);
    let title = cx.tr.t(&title_key);
    let body = cx.tr.t(&body_key);
    (
        if title == title_key {
            cx.tr.text(&p.site.name)
        } else {
            title
        },
        if body == body_key {
            String::new()
        } else {
            body
        },
    )
}

/// Ids in use by sites and profiles (new ids must avoid them).
fn taken_ids<'a>(cx: &'a Cx<'_>) -> Vec<&'a str> {
    cx.data
        .sites
        .iter()
        .map(|s| s.id.as_str())
        .chain(cx.data.profiles.value.iter().flat_map(|set| {
            set.profiles
                .iter()
                .map(|p| p.id.as_str())
                .chain(set.custom_sites.iter().map(|s| s.id.as_str()))
        }))
        .collect()
}

fn taken_names<'a>(cx: &'a Cx<'_>) -> Vec<&'a str> {
    cx.data
        .profiles
        .value
        .iter()
        .flat_map(|s| s.profiles.iter().map(|p| p.name.as_str()))
        .collect()
}

/// Draw the dialog when open.
pub(super) fn show(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(d) = state.add.as_mut() else {
        return;
    };
    let mut close = false;
    let mut select = None;
    let saving = d.saving();
    let response = dialog(ctx, egui::Id::unique("providers-add"), 640.0, |ui| {
        let title = match d.step {
            AddStep::Choose => cx.tr.t("providers.add.step1"),
            AddStep::Preset | AddStep::Custom => cx.tr.t("providers.add.step2"),
        };
        ui.label(Text::h2(
            format!("{} · {title}", cx.tr.t("providers.add.title")),
            cx.pal,
        ));
        ui.add_space(4.0);
        match d.step {
            AddStep::Choose => choose(ui, cx, d, &mut close, &mut select),
            AddStep::Preset => preset_step(ui, cx, d, &mut close, out),
            AddStep::Custom => custom_step(ui, cx, d, &mut close),
        }
    });
    if let Some(id) = select {
        state.add = None;
        state.select(id);
        return;
    }
    if d.open_import {
        state.add = None;
        state.open_import(cx);
        return;
    }
    if !saving && (close || response.should_close()) {
        state.add = None;
    }
}

fn choose(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &mut AddDialog,
    close: &mut bool,
    select: &mut Option<ProviderId>,
) {
    let items: Vec<Choice<String>> = catalog::presets()
        .iter()
        .map(|p| {
            let (title, mut body) = preset_text(cx, p);
            if p.id == catalog::PRESET_OFFICIAL {
                body = cx.tr.t("providers.add.official_exists");
            }
            Choice {
                value: p.id.clone(),
                title,
                body: (!body.is_empty()).then_some(body),
                badge: p.default.then(|| cx.tr.t("providers.preset.default")),
                enabled: true,
            }
        })
        .collect();
    choice_cards(ui, &mut d.pick, &items);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui
            .add(
                Button::ghost(cx.tr.t("providers.add.import"))
                    .icon(Icon::Download)
                    .small(),
            )
            .on_hover_text(cx.tr.t("providers.add.import_tip"))
            .clicked()
        {
            d.open_import = true;
        }
    });
    ui.add_space(4.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui
            .add(Button::primary(cx.tr.t("providers.add.continue")).kbd("Enter"))
            .clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Enter))
        {
            match d.pick.as_str() {
                catalog::PRESET_OFFICIAL => {
                    *select = Some(ProviderId::Profile(superba_core::ProfileId::new(
                        catalog::PROFILE_OFFICIAL,
                    )));
                }
                catalog::PRESET_CUSTOM => {
                    d.step = AddStep::Custom;
                    d.focused = false;
                }
                _ => {
                    let base = d
                        .preset()
                        .map(|p| preset_text(cx, &p).0)
                        .unwrap_or_default();
                    d.name = form::unique_name(&base, &taken_names(cx));
                    d.step = AddStep::Preset;
                    d.focused = false;
                }
            }
        }
        if ui
            .add(Button::ghost(cx.tr.t("action.cancel")).kbd("Esc"))
            .clicked()
        {
            *close = true;
        }
    });
}

fn field_label(ui: &mut Ui, cx: &Cx<'_>, text: &str) -> egui::Response {
    ui.label(Text::strong(text, cx.pal))
}

fn preset_step(
    ui: &mut Ui,
    cx: &Cx<'_>,
    d: &mut AddDialog,
    close: &mut bool,
    out: &mut Vec<Action>,
) {
    let Some(preset) = d.preset() else {
        d.step = AddStep::Choose;
        return;
    };
    let site = cx.data.site(&preset.site.id).unwrap_or(preset.site.clone());
    let busy = d.saving();
    ui.add_enabled_ui(!busy, |ui| {
        let l = field_label(ui, cx, &cx.tr.t("sites.form.name"));
        ui.add(
            TextEdit::singleline(&mut d.name)
                .id(egui::Id::unique("providers-add-name"))
                .desired_width(f32::INFINITY)
                .margin(vec2(12.0, 8.0)),
        )
        .labelled_by(l.id);
        let l = field_label(ui, cx, &cx.tr.t("providers.row.key"));
        let placeholder = preset
            .key_placeholder
            .clone()
            .unwrap_or_else(|| cx.tr.t("connection.key.placeholder_compatible"));
        let r = KeyField::new(
            "providers-add-key",
            &mut d.key,
            &mut d.reveal,
            placeholder,
            cx.tr.t("lines.key.reveal"),
        )
        .show(ui)
        .labelled_by(l.id);
        if !d.focused {
            r.request_focus();
            d.focused = true;
        }
        if let Some(home) = &site.homepage {
            let row_h = ui.spacing().interact_size.y;
            FormRow::unlabeled().height(row_h).show(ui, |ui| {
                ui.label(Text::small(cx.tr.t("providers.add.get_key"), cx.pal));
                let host = home.host_str().unwrap_or(home.as_str()).to_owned();
                if link_button(ui, &host, true).clicked() {
                    out.push(Action::OpenUrl(home.to_string()));
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let has_key = !d.key.trim().is_empty();
            let checking = d.check_task.is_some();
            if ui
                .add(
                    Button::ghost(cx.tr.t("providers.key.check_all"))
                        .icon(Icon::Bolt)
                        .small()
                        .loading(checking)
                        .enabled(has_key && !checking),
                )
                .on_hover_text(cx.tr.t("lines.key.cost"))
                .clicked()
            {
                let lines = site
                    .lines
                    .iter()
                    .map(|l| {
                        let caps = cx.data.site_caps(&site.id, &l.id);
                        let mode = decision_with(&site, Some(l), caps, None, auto_facts(cx.data))
                            .mode
                            .unwrap_or(AuthMode::ApiKey);
                        (l.id.clone(), mode)
                    })
                    .collect();
                d.error = None;
                d.check_task = Some(crate::runtime::task(ops::check_lines(
                    cx.service.clone(),
                    site.id.clone(),
                    lines,
                    KeyInput::Secret(Secret::new(d.key.trim())),
                )));
            }
        });
        if let Some(checks) = &d.checks {
            for (line, _, r) in checks {
                let name = site
                    .line(line)
                    .map(|l| cx.tr.text(&l.name))
                    .unwrap_or_else(|| line.to_string());
                ui.horizontal(|ui| {
                    match r {
                        Ok(k) => {
                            ui.add(StatusDot::new(key_access_status(&k.access)));
                            ui.label(Text::small(
                                format!("{name} · {}", cx.tr.text(&k.access.text())),
                                cx.pal,
                            ));
                        }
                        Err(e) => {
                            ui.add(StatusDot::new(Status::Error));
                            ui.label(Text::small(
                                format!("{name} · {}", error_text(cx.tr, e)),
                                cx.pal,
                            ));
                        }
                    };
                });
            }
        }
    });
    switch_after(ui, cx, d, !busy);
    if let Some(e) = &d.error {
        ui.add(egui::Label::new(Text::small(e, cx.pal).color(cx.pal.red)).wrap());
    }
    ui.add_space(12.0);
    let mut add = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ready = !d.key.trim().is_empty() && !d.name.trim().is_empty() && !busy;
        let switch = d.switch_after;
        if ui
            .add(
                Button::primary(add_label(cx, switch))
                    .loading(busy)
                    .enabled(ready && !(switch && cx.busy)),
            )
            .clicked()
        {
            add = Some(switch);
        }
        if ui
            .add(Button::ghost(cx.tr.t("providers.add.back")).enabled(!busy))
            .clicked()
        {
            d.step = AddStep::Choose;
        }
        if ui
            .add(
                Button::ghost(cx.tr.t("action.cancel"))
                    .kbd("Esc")
                    .enabled(!busy),
            )
            .clicked()
        {
            *close = true;
        }
    });
    if let Some(switch) = add {
        d.start_save(cx, switch);
    }
}

fn custom_step(ui: &mut Ui, cx: &Cx<'_>, d: &mut AddDialog, close: &mut bool) {
    let busy = d.saving();
    let testing = d.test.is_some();
    ui.add_enabled_ui(!busy, |ui| {
        let l = field_label(ui, cx, &cx.tr.t("sites.form.name"));
        let r = ui
            .add(
                TextEdit::singleline(&mut d.name)
                    .id(egui::Id::unique("providers-add-custom-name"))
                    .hint_text(cx.tr.t("sites.form.name_hint"))
                    .desired_width(f32::INFINITY)
                    .margin(vec2(12.0, 8.0)),
            )
            .labelled_by(l.id);
        if !d.focused {
            r.request_focus();
            d.focused = true;
        }
        let row_h = ui.spacing().interact_size.y;
        FormRow::unlabeled().height(row_h).show(ui, |ui| {
            field_label(ui, cx, &cx.tr.t("providers.base_url"));
            ui.label(Text::caption(cx.tr.t("providers.base_url_hint"), cx.pal));
        });
        ui.add(
            TextEdit::singleline(&mut d.url)
                .id(egui::Id::unique("providers-add-url"))
                .font(egui::TextStyle::Monospace)
                .hint_text("https://api.example.com/v1")
                .desired_width(f32::INFINITY)
                .margin(vec2(12.0, 8.0)),
        );
        let l = field_label(ui, cx, &cx.tr.t("sites.form.key"));
        KeyField::new(
            "providers-add-custom-key",
            &mut d.key,
            &mut d.reveal,
            cx.tr.t("sites.form.key_hint"),
            cx.tr.t("lines.key.reveal"),
        )
        .show(ui)
        .labelled_by(l.id);
        ui.add_space(4.0);
        // The test button, the Responses switch and its label on one center line.
        FormRow::unlabeled().show(ui, |ui| {
            let ready = !d.key.trim().is_empty() && form_url(d).is_some();
            if ui
                .add(
                    Button::new(cx.tr.t("providers.test.button"))
                        .icon(Icon::Bolt)
                        .small()
                        .loading(testing)
                        .enabled(ready && !testing),
                )
                .clicked()
            {
                d.start_test(cx);
            }
            ui.add(crate::design::widgets::Toggle::new(
                &mut d.responses,
                cx.tr.t("providers.test.responses_optional"),
            ));
            ui.label(Text::small(
                cx.tr.t("providers.test.responses_optional"),
                cx.pal,
            ));
        });
        if let Some(report) = &d.report {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                summary(ui, cx, report);
            });
            if matches!(&report.models, Some(Err(_)) | Some(Ok(_)) if report.model_ids().is_empty())
            {
                ui.label(
                    Text::small(cx.tr.t("providers.test.no_models"), cx.pal).color(cx.pal.amber),
                );
            }
        }
    });
    switch_after(ui, cx, d, !busy);
    if let Some(e) = &d.error {
        ui.add(egui::Label::new(Text::small(e, cx.pal).color(cx.pal.red)).wrap());
    }
    ui.add_space(12.0);
    let mut add = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let switch = d.switch_after;
        // Saving alone needs no test; switching to the site needs a passed one.
        let untested = switch && !d.tested_ok();
        let r = ui.add(
            Button::primary(add_label(cx, switch))
                .loading(busy)
                .enabled(!busy && !untested && !(switch && cx.busy)),
        );
        if untested {
            r.clone()
                .on_disabled_hover_text(cx.tr.t("providers.add.test_first"));
        }
        if r.clicked() {
            add = Some(switch);
        }
        if ui
            .add(Button::ghost(cx.tr.t("providers.add.back")).enabled(!busy))
            .clicked()
        {
            d.step = AddStep::Choose;
        }
        if ui
            .add(
                Button::ghost(cx.tr.t("action.cancel"))
                    .kbd("Esc")
                    .enabled(!busy),
            )
            .clicked()
        {
            *close = true;
        }
    });
    if let Some(switch) = add {
        d.start_save(cx, switch);
    }
}

/// The primary button: "Add", or "Add and switch" when the user opted in.
fn add_label(cx: &Cx<'_>, switch: bool) -> String {
    cx.tr.t(if switch {
        "providers.add.add_switch"
    } else {
        "providers.add.add"
    })
}

/// The opt-in "Switch to it right after adding" (off by default) and one line
/// on what each choice does (always shown, so ticking it does not move the
/// buttons under the pointer).
fn switch_after(ui: &mut Ui, cx: &Cx<'_>, d: &mut AddDialog, enabled: bool) {
    ui.add_space(4.0);
    ui.add_enabled_ui(enabled, |ui| {
        ui.checkbox(&mut d.switch_after, cx.tr.t("providers.add.switch_after"));
    });
    let hint = if d.switch_after {
        "providers.add.switch_after_hint"
    } else {
        "providers.add.save_only_hint"
    };
    ui.add(egui::Label::new(Text::caption(cx.tr.t(hint), cx.pal)).wrap());
}

fn form_url(d: &AddDialog) -> Option<url::Url> {
    let u = url::Url::parse(d.url.trim()).ok()?;
    form::check_url(&u).ok().map(|_| u)
}

/// Test the typed site with the typed key before it is saved (keys go only to
/// the site's origin).
fn start_test(cx: &Cx<'_>, d: &AddDialog) -> ReadTask<SiteReport> {
    let name = if d.name.trim().is_empty() {
        "site".to_owned()
    } else {
        d.name.trim().to_owned()
    };
    let url = form_url(d);
    let key = Secret::new(d.key.trim());
    let responses = d.responses;
    let service = cx.service.clone();
    crate::runtime::task(async move {
        let url = url.ok_or_else(|| superba_core::Error::invalid("invalid base URL"))?;
        let site =
            superba_switch::new_compatible_site(SiteId::new("superba-site-test"), &name, &url)?;
        Ok(ops::test_unsaved(service, site, key, responses).await)
    })
}

/// One-line test result: latency, key, models, Responses, balance.
pub(super) fn summary(ui: &mut Ui, cx: &Cx<'_>, r: &SiteReport) {
    match &r.caps {
        Some(Ok(c)) if c.reachable => {
            let text = match c.latency_ms {
                Some(ms) => cx.tr.f("sites.check.reachable_ms", &[("ms", &ms)]),
                None => cx.tr.t("sites.check.reachable"),
            };
            ui.add(Badge::new(text).status(Status::Ok));
        }
        Some(Ok(c)) => {
            let r = ui.add(Badge::new(cx.tr.t("sites.check.unreachable")).status(Status::Error));
            if let Some(d) = &c.detail {
                r.on_hover_text(cx.localize(d));
            }
        }
        Some(Err(e)) => {
            ui.add(Badge::new(cx.tr.t("sites.check.unreachable")).status(Status::Error))
                .on_hover_text(error_text(cx.tr, e));
        }
        None => {}
    }
    match &r.key {
        Some(Ok(k)) => {
            ui.add(Badge::new(cx.tr.text(&k.access.text())).status(key_access_status(&k.access)));
        }
        Some(Err(e)) => {
            ui.add(Badge::new(cx.tr.t("sites.check.key_failed")).status(Status::Warn))
                .on_hover_text(error_text(cx.tr, e));
        }
        None => {}
    }
    match &r.models {
        Some(Ok(_)) if !r.model_ids().is_empty() => {
            ui.add(Badge::new(
                cx.tr
                    .f("providers.test.models", &[("n", &r.model_ids().len())]),
            ));
        }
        Some(_) => {
            ui.add(Badge::new(cx.tr.t("providers.test.models_none")).status(Status::Warn))
                .on_hover_text(cx.tr.t("providers.test.no_models"));
        }
        None => {}
    }
    match &r.responses {
        Some(Ok(p)) if p.ok => {
            ui.add(responses_badge(cx, true));
        }
        Some(Ok(p)) => {
            let r = ui.add(responses_badge(cx, false));
            if let Some(d) = &p.detail {
                r.on_hover_text(d);
            }
        }
        Some(Err(e)) => {
            ui.add(responses_badge(cx, false))
                .on_hover_text(error_text(cx.tr, e));
        }
        None => {}
    }
    if let Some(Ok(Some(account))) = &r.account
        && let Some(text) = balance_text(cx, account)
    {
        ui.add(Badge::new(text).mono());
    }
    if let Some(at) = r.at {
        ui.label(Text::caption(
            cx.tr.f("sites.check.at", &[("ago", &cx.ago(at))]),
            cx.pal,
        ));
    }
}

/// "Responses" with a drawn check or cross (the bundled fonts have no check mark).
fn responses_badge(cx: &Cx<'_>, ok: bool) -> Badge {
    let (icon, status) = if ok {
        (Icon::Check, Status::Ok)
    } else {
        (Icon::Close, Status::Error)
    };
    Badge::new(cx.tr.t("providers.test.responses"))
        .icon(icon)
        .status(status)
}

/// "$12.30 left of $50.00" from the billing endpoints, when they answered.
pub(super) fn balance_text(cx: &Cx<'_>, account: &superba_core::AccountSummary) -> Option<String> {
    let billing = account.billing.as_ref()?;
    if billing.unlimited {
        return Some(cx.tr.t("sites.balance.unlimited"));
    }
    match (billing.remaining, billing.limit, billing.used) {
        (Some(remaining), Some(limit), _) => Some(cx.tr.f(
            "sites.balance.remaining",
            &[("remaining", &remaining), ("limit", &limit)],
        )),
        (None, Some(limit), _) => Some(cx.tr.f("sites.balance.limit", &[("limit", &limit)])),
        (None, None, Some(used)) => Some(cx.tr.f("sites.balance.used", &[("used", &used)])),
        _ => None,
    }
}
