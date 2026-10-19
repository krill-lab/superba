//! Setup wizard (UX §3.2), re-enterable: Detect -> Provider -> Credentials ->
//! Line & models -> Confirm. The provider step is the preset quick-pick
//! ([`superba_core::catalog::presets`]: Krill pre-selected, the official login, a
//! custom OpenAI-compatible site). A re-run pre-selects the applied provider and
//! edits its profile instead of creating another one. Finishing saves the profile
//! (key into the secret store) and plans the switch through the normal flow, so
//! the countdown / confirmation rules apply unchanged.
//!
//! A custom site is tested before anything is saved
//! (`SiteProbeRequest::site`): the credentials step builds a draft of the site
//! and its profile, the line & models step probes the draft with the typed key,
//! and only Apply saves the site, the profile and the key (then plans the
//! switch). Skip or Escape saves only the dismissal preference, without
//! creating providers or writing to a coding tool's configuration.
//!
//! The provider step also offers "Import from other tools" (cc-switch, AI
//! Toolbox; [`crate::pages::import`]), shown in place of the steps: importing
//! saves providers only (nothing is written to Codex), then the wizard closes on
//! the Providers page with the first imported provider selected and a switch to
//! it offered.

mod steps;

use std::collections::HashSet;

use egui::{Align, Area, Id, Layout, Order, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use superba_core::{
    AuthChoice, AuthMode, KeyInput as KeyRefInput, LegacySessionChoice, LineId, Profile, ProfileId,
    RestoreTarget, Secret, Site, SiteId, SiteKind, SiteProbeReport, SiteProbeRequest,
    SwitchRequest,
    catalog::{self, Preset},
    codex::PROVIDER_OPENAI,
};

use super::{Action, Cx, Data};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Text};
use crate::runtime::ReadTask;

/// Number of steps.
pub const STEPS: usize = 5;

/// A step of the wizard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// What Superba found (app, CLI, hosts, current configuration, sessions).
    Detect,
    /// The preset quick-pick.
    Provider,
    /// Key (Krill-like presets), site address and key (custom), or the
    /// official login explained.
    Credentials,
    /// Line tiles and the default model (skipped for the official login).
    LineModels,
    /// What happens, the legacy sessions choice.
    Confirm,
}

impl Step {
    /// Every step, in order.
    pub const ALL: [Step; STEPS] = [
        Step::Detect,
        Step::Provider,
        Step::Credentials,
        Step::LineModels,
        Step::Confirm,
    ];

    /// 0-based position.
    pub fn index(self) -> usize {
        Step::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// i18n key of the stepper label.
    pub fn label_key(self) -> &'static str {
        match self {
            Step::Detect => "wizard.step.detect",
            Step::Provider => "wizard.step.provider",
            Step::Credentials => "wizard.step.credentials",
            Step::LineModels => "wizard.step.line_models",
            Step::Confirm => "wizard.step.confirm",
        }
    }
}

/// A custom site: the stored one a re-run edits and the unsaved draft built
/// from the form (tested before Apply saves it).
#[derive(Debug, Default)]
pub(crate) struct CustomSave {
    /// The stored site and profile (the edit target of a re-run).
    pub(crate) saved: Option<(SiteId, ProfileId)>,
    /// The site and profile built from the form; saved by Apply.
    pub(crate) draft: Option<(Site, Profile)>,
    pub(crate) error: Option<String>,
}

/// Wizard state. Entered keys are omitted from `Debug`.
pub struct Wizard {
    /// Current step.
    pub step: Step,
    /// Presets in quick-pick order.
    pub presets: Vec<Preset>,
    /// Chosen preset id.
    pub preset: String,
    /// The site the chosen preset uses (the backend's copy with its lines; on a
    /// re-run the applied site of that kind).
    pub site: Option<Site>,
    /// The profile a re-run edits (`None` = a new one is created).
    pub editing: Option<ProfileId>,
    /// Opened while a key-based provider already existed.
    pub rerun: bool,
    /// Typed key.
    pub key: String,
    /// Show the key.
    pub reveal: bool,
    /// Custom site: name.
    pub custom_name: String,
    /// Custom site: Responses base URL.
    pub custom_url: String,
    /// Custom site: also send one minimal Responses request when testing.
    pub custom_responses: bool,
    pub(crate) custom: CustomSave,
    /// Selected line.
    pub line: Option<LineId>,
    /// The line is settled: the user picked it, or a re-run keeps the edited
    /// provider's current line. Otherwise the fastest line is pre-selected.
    pub line_chosen: bool,
    /// Default model (`None` = Codex's default).
    pub model: Option<String>,
    /// Answer for sessions under a legacy provider.
    pub legacy: Option<LegacySessionChoice>,
    pub(crate) probe: Option<ReadTask<SiteProbeReport>>,
    pub(crate) probe_for: Option<(SiteId, Option<LineId>)>,
    pub(crate) probe_report: Option<superba_core::Result<SiteProbeReport>>,
    pub(crate) checked_key: Option<Secret>,
    pub(crate) checked_modes: HashSet<(LineId, AuthMode)>,
    pub(crate) original_key: Option<Secret>,
    pub(crate) use_original_key: bool,
    pub(crate) original_read: Option<ReadTask<Option<Secret>>>,
    pub(crate) original_requested: bool,
    pub(crate) original_error: Option<superba_core::Error>,
    /// "Import from other tools", shown instead of the steps while open.
    pub import: Option<crate::pages::import::ImportDialog>,
}

impl std::fmt::Debug for Wizard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wizard")
            .field("step", &self.step)
            .field("preset", &self.preset)
            .field("editing", &self.editing)
            .field("line", &self.line)
            .finish_non_exhaustive()
    }
}

/// The preset a site belongs to (by kind: every Krill-kind site is "Krill").
pub fn preset_of(site: &Site) -> &'static str {
    match site.kind {
        SiteKind::Krill => catalog::PRESET_KRILL,
        SiteKind::Official => catalog::PRESET_OFFICIAL,
        _ => catalog::PRESET_CUSTOM,
    }
}

impl Wizard {
    /// A wizard for `data`: the default preset on a first run; on a re-run the
    /// applied provider's preset, its site and profile (edited, never
    /// duplicated).
    pub fn start(data: &Data) -> Self {
        let presets = catalog::presets();
        let rerun = !data.providers().is_empty();
        let active_site = data.active_site();
        let preset = match &active_site {
            Some(site) if rerun || site.kind == SiteKind::Official => preset_of(site).to_owned(),
            _ => presets
                .iter()
                .find(|p| p.default)
                .map_or_else(|| catalog::PRESET_KRILL.to_owned(), |p| p.id.clone()),
        };
        let mut w = Self {
            step: Step::Detect,
            presets,
            preset,
            site: None,
            editing: None,
            rerun,
            key: String::new(),
            reveal: false,
            custom_name: String::new(),
            custom_url: String::new(),
            custom_responses: false,
            custom: CustomSave::default(),
            line: None,
            line_chosen: false,
            model: None,
            legacy: Some(LegacySessionChoice::Migrate),
            probe: None,
            probe_for: None,
            probe_report: None,
            checked_key: None,
            checked_modes: HashSet::new(),
            original_key: None,
            use_original_key: true,
            original_read: None,
            original_requested: false,
            original_error: None,
            import: None,
        };
        w.select_preset(data, &w.preset.clone());
        w
    }

    /// The chosen preset.
    pub fn chosen(&self) -> Option<&Preset> {
        self.presets.iter().find(|p| p.id == self.preset)
    }

    /// The official login is chosen.
    pub fn official(&self) -> bool {
        self.chosen()
            .is_some_and(|p| p.site.kind == SiteKind::Official)
    }

    /// A custom compatible site is chosen.
    pub fn custom(&self) -> bool {
        self.chosen()
            .is_some_and(|p| p.site.kind == SiteKind::Compatible)
    }

    /// Choose a preset: resolve its site and the profile a re-run edits, and
    /// pre-fill a custom site's form from the edited site.
    pub fn select_preset(&mut self, data: &Data, id: &str) {
        self.preset = id.to_owned();
        let Some(preset) = self.chosen().cloned() else {
            return;
        };
        let active_site = data.active_site();
        let active_profile = data.active_profile();
        let same_kind = |s: &Site| s.kind == preset.site.kind;
        let site = match preset.site.kind {
            SiteKind::Official => Some(preset.site.clone()),
            SiteKind::Compatible => active_site.filter(|s| same_kind(s)),
            _ => active_site
                .filter(|s| same_kind(s))
                .or_else(|| data.site(&preset.site.id))
                .or_else(|| Some(preset.site.clone())),
        };
        self.editing = match (&site, preset.site.kind) {
            (_, SiteKind::Official) | (None, _) => None,
            (Some(site), _) => active_profile
                .filter(|p| p.site_id == site.id)
                .or_else(|| data.providers().into_iter().find(|p| p.site_id == site.id))
                .map(|p| p.id.clone()),
        };
        if preset.site.kind == SiteKind::Compatible {
            match (&site, &self.editing) {
                (Some(site), Some(profile)) => {
                    self.custom_name = data
                        .profile(profile)
                        .map_or_else(|| site.id.to_string(), |p| p.name.clone());
                    self.custom_url = site_base_url(site);
                    self.custom.saved = Some((site.id.clone(), profile.clone()));
                }
                _ => {
                    self.custom_name.clear();
                    self.custom_url.clear();
                    self.custom.saved = None;
                }
            }
            self.custom.draft = None;
        }
        self.model = self
            .editing
            .as_ref()
            .and_then(|id| data.profile(id))
            .and_then(|p| p.model.model.clone());
        // A re-run keeps the provider's current line (a look around must not
        // move it); only a new provider gets the fastest line pre-selected
        // (review ux-visual #6).
        let current = self
            .editing
            .as_ref()
            .and_then(|id| data.profile(id))
            .and_then(|p| current_line(data, p))
            .filter(|l| site.as_ref().is_some_and(|s| s.line(l).is_some()));
        self.line_chosen = current.is_some();
        self.line = current.or_else(|| site.as_ref().and_then(|s| s.default_line.clone()));
        self.site = site;
        self.probe = None;
        self.probe_for = None;
        self.probe_report = None;
        self.checked_key = None;
        self.checked_modes.clear();
        self.original_requested = false;
        self.original_key = None;
        self.original_read = None;
        self.original_error = None;
    }

    /// The custom site's draft passed its test (reachable, key not rejected).
    pub fn custom_test_passed(&self) -> bool {
        self.custom.draft.is_some() && steps::custom_test_passed(self)
    }

    /// The edited profile.
    pub fn editing_profile<'a>(&self, data: &'a Data) -> Option<&'a Profile> {
        self.editing.as_ref().and_then(|id| data.profile(id))
    }

    /// The next step from `self.step` (the official login skips Line & models).
    pub fn next_step(&self) -> Option<Step> {
        let next = match self.step {
            Step::Detect => Step::Provider,
            Step::Provider => Step::Credentials,
            Step::Credentials if self.official() => Step::Confirm,
            Step::Credentials => Step::LineModels,
            Step::LineModels => Step::Confirm,
            Step::Confirm => return None,
        };
        Some(next)
    }

    /// The previous step.
    pub fn prev_step(&self) -> Option<Step> {
        let prev = match self.step {
            Step::Detect => return None,
            Step::Provider => Step::Detect,
            Step::Credentials => Step::Provider,
            Step::LineModels => Step::Credentials,
            Step::Confirm if self.official() => Step::Credentials,
            Step::Confirm => Step::LineModels,
        };
        Some(prev)
    }

    /// The key the switch uses: a typed one, else the one found in auth.json.
    pub fn selected_key(&self) -> Option<Secret> {
        if !self.key.trim().is_empty() {
            Some(Secret::new(self.key.trim()))
        } else if self.use_original_key {
            self.original_key.clone()
        } else {
            None
        }
    }

    /// The key to check / probe with: typed or found, else the edited profile's
    /// stored key.
    pub fn key_input(&self, data: &Data) -> Option<KeyRefInput> {
        if let Some(k) = self.selected_key() {
            return Some(KeyRefInput::Secret(k));
        }
        self.editing_profile(data)
            .filter(|p| p.key_ref.is_some())
            .map(|p| KeyRefInput::Profile(p.id.clone()))
    }
}

/// What the wizard wants the app to do.
#[derive(Debug)]
pub enum WizardOutcome {
    /// Keep showing.
    Continue,
    /// Close without changes.
    Close,
    /// Save the site (a custom one, first), the profile (and key) and plan
    /// `request`.
    Finish {
        /// A custom site to save before the profile (new or edited).
        site: Option<Box<Site>>,
        /// Profile to save.
        profile: Box<Profile>,
        /// Key to store.
        key: Option<Secret>,
        /// Switch request.
        request: Box<SwitchRequest>,
    },
}

/// The provider with the most sessions that is not `openai` (sessions hidden in
/// the app until they are migrated).
pub(crate) fn legacy_count(cx: &Cx<'_>) -> Option<(String, u32)> {
    let (report, _) = cx.data.sessions.value.as_ref()?;
    report
        .distribution()
        .into_iter()
        .filter(|(p, c)| p != PROVIDER_OPENAI && *c > 0)
        .max_by_key(|(_, c)| *c)
}

/// Draw the wizard as a full-window layer.
pub fn show(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    w: &mut Wizard,
    out: &mut Vec<Action>,
) -> WizardOutcome {
    let mut outcome = WizardOutcome::Continue;
    let screen = ctx.content_rect();
    let area = Area::new(Id::unique("wizard"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(true);
    // Keep Tab on the visible wizard rather than the pages behind its backdrop.
    ctx.memory_mut(|memory| memory.set_modal_layer(area.layer()));
    area.show(ctx, |ui| {
        let painter = ui.painter();
        painter.rect_filled(screen, 0, cx.pal.bg);
        // Swallow clicks on the backdrop.
        ui.allocate_rect(screen, Sense::CLICK);
        let width = 680.0_f32.min(screen.width() - 48.0);
        let col = Rect::from_min_size(
            pos2(screen.center().x - width / 2.0, screen.min.y + 36.0),
            vec2(width, screen.height() - 56.0),
        );
        let mut ui = ui.new_child(
            UiBuilder::new()
                .max_rect(col)
                .layout(Layout::top_down(Align::Min)),
        );
        outcome = body(&mut ui, cx, w, out);
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        // Escape leaves the import view first (not while it imports).
        match &w.import {
            Some(d) if d.busy() => {}
            Some(_) => w.import = None,
            None => outcome = WizardOutcome::Close,
        }
    }
    outcome
}

/// The import view inside the wizard; `Close` when it finished with imports.
fn import_view(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) -> WizardOutcome {
    use crate::pages::import::{self, ImportOutcomeUi};
    let Some(d) = w.import.as_mut() else {
        return WizardOutcome::Continue;
    };
    let list_height = (ui.available_height() - 150.0).max(160.0);
    let outcome = import::body(ui, cx, d, out, list_height);
    match &outcome {
        ImportOutcomeUi::Continue => WizardOutcome::Continue,
        ImportOutcomeUi::Close => {
            w.import = None;
            WizardOutcome::Continue
        }
        ImportOutcomeUi::Done {
            imported, first, ..
        } => {
            w.import = None;
            if *imported == 0 {
                return WizardOutcome::Continue;
            }
            import::finished_actions(cx, &outcome, out);
            out.push(match first {
                Some(id) => Action::PageCommand {
                    page: super::Page::Providers,
                    command: format!("profile:{id}"),
                },
                None => Action::Go(super::Page::Providers),
            });
            WizardOutcome::Close
        }
    }
}

fn body(ui: &mut Ui, cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) -> WizardOutcome {
    let mut outcome = WizardOutcome::Continue;
    steps::poll(ui, cx, w, out);
    ui.horizontal(|ui| {
        crate::shell::brand_mark(ui, cx.pal, 22.0);
        ui.label(Text::strong("Superba", cx.pal));
        ui.label(Text::small(cx.tr.t("wizard.title"), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::ghost(cx.tr.t("wizard.skip")).small())
                .clicked()
            {
                outcome = WizardOutcome::Close;
            }
        });
    });
    ui.add_space(20.0);
    if w.import.is_some() {
        let o = import_view(ui, cx, w, out);
        return match o {
            WizardOutcome::Continue => outcome,
            other => other,
        };
    }
    steps::stepper(ui, cx, w);
    ui.add_space(18.0);

    let footer_h = 56.0;
    let avail = ui.available_height() - footer_h;
    ScrollArea::vertical()
        .max_height(avail.max(120.0))
        .auto_shrink([false, true])
        .show(ui, |ui| match w.step {
            Step::Detect => steps::detect(ui, cx, w, out),
            Step::Provider => steps::provider(ui, cx, w, out),
            Step::Credentials => steps::credentials(ui, cx, w, out),
            Step::LineModels => steps::line_models(ui, cx, w, out),
            Step::Confirm => steps::confirm(ui, cx, w),
        });

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if let Some(prev) = w.prev_step()
            && ui.add(Button::new(cx.tr.t("wizard.back"))).clicked()
        {
            w.step = prev;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let last = w.next_step().is_none();
            let testing = w.custom() && w.step == Step::LineModels && w.probe.is_some();
            let can = can_continue(cx, w);
            let label = if last {
                cx.tr.t("wizard.apply")
            } else {
                cx.tr.t("wizard.continue")
            };
            let mut b = Button::primary(label)
                .enabled(can && !cx.busy)
                .loading(testing);
            if !last {
                b = b.trailing(Icon::ArrowRight);
            }
            // Enter continues when no control has the keyboard (a focused card
            // or field keeps Enter for itself).
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter))
                && ui.ctx().memory(|m| m.focused().is_none());
            if ui.add(b).clicked() || (enter && can && !cx.busy) {
                if last {
                    outcome = finish(cx, w, out);
                } else {
                    advance(cx, w, out);
                }
            }
            ui.label(Text::small(
                cx.tr.f(
                    "wizard.progress",
                    &[("n", &(w.step.index() + 1)), ("total", &STEPS)],
                ),
                cx.pal,
            ));
        });
    });
    outcome
}

/// Whether the step's Continue (or Apply) button is enabled.
pub fn can_continue(cx: &Cx<'_>, w: &Wizard) -> bool {
    match w.step {
        Step::Provider => w.chosen().is_some(),
        Step::Credentials if w.official() => true,
        Step::Credentials if w.custom() => {
            !w.custom_name.trim().is_empty()
                && !w.custom_url.trim().is_empty()
                && (!w.key.trim().is_empty() || w.custom.saved.is_some())
        }
        Step::Credentials => w.key_input(cx.data).is_some(),
        Step::LineModels if w.custom() => w.custom_test_passed(),
        Step::LineModels => w.line.is_some() && steps::rejected_on_line(cx, w).is_none(),
        Step::Confirm => legacy_count(cx).is_none() || w.legacy.is_some() || w.official(),
        Step::Detect => true,
    }
}

/// Leave the current step (Continue).
pub fn advance(cx: &Cx<'_>, w: &mut Wizard, out: &mut Vec<Action>) {
    if w.step == Step::Credentials && w.custom() && !steps::draft_custom(cx, w) {
        // The form has an error (shown under it); nothing is written.
        return;
    }
    if let Some(next) = w.next_step() {
        w.step = next;
        steps::on_enter(cx, w, out);
    }
}

/// The line `profile` uses now: the applied line when it is the applied
/// provider, else its first line.
pub fn current_line(data: &Data, profile: &Profile) -> Option<LineId> {
    data.active_state()
        .filter(|a| a.profile_id.as_ref() == Some(&profile.id))
        .and_then(|a| a.line_id.clone())
        .or_else(|| profile.line_priority().into_iter().next())
}

/// The profile the wizard saves for a preset with lines: the edited one with the
/// chosen line first and the chosen default model (its authentication choice,
/// other lines, key and Codex settings are kept), or a new one for `site` on
/// Automatic authentication (the preset's recommended Codex settings, like
/// `spb profile add`).
pub fn wizard_profile(
    existing: Option<&Profile>,
    site: &Site,
    line: Option<LineId>,
    model: Option<String>,
) -> Profile {
    let mut profile = match existing {
        Some(p) => p.clone(),
        None => {
            let name = match site.kind {
                SiteKind::Krill => site.name.render_fallback(),
                _ => site.id.to_string(),
            };
            super::shared::new_profile(site, name)
        }
    };
    if let Some(line) = line {
        let mut lines = vec![line.clone()];
        lines.extend(profile.line_priority().into_iter().filter(|l| l != &line));
        profile.set_line_priority(lines);
    }
    if existing.is_none() && site.supports(AuthMode::Pat) {
        profile.set_auth(AuthChoice::Auto);
    }
    if model.is_some() {
        profile.model.model = model;
    }
    profile
}

/// What the profile's authentication choice resolves to on the wizard's line.
pub(crate) fn auth_decision(
    cx: &Cx<'_>,
    site: &Site,
    line: Option<&LineId>,
    profile: Option<&Profile>,
) -> superba_core::AuthDecision {
    let line = line.and_then(|l| site.line(l));
    super::shared::decision_with(
        site,
        line,
        line.and_then(|l| cx.data.site_caps(&site.id, &l.id)),
        profile,
        super::shared::AutoFacts {
            router: false,
            ..super::shared::auto_facts(cx.data)
        },
    )
}

/// The profile Apply saves for a preset with lines ([`wizard_profile`]). A
/// re-run keeps the provider's authentication choice; a manual one the chosen
/// line cannot use would only block the switch, so it becomes Automatic there
/// (Confirm shows the change).
pub fn planned_profile(cx: &Cx<'_>, w: &Wizard) -> Option<Profile> {
    let site = w.site.as_ref()?;
    let mut profile = wizard_profile(
        w.editing_profile(cx.data),
        site,
        w.line.clone(),
        w.model.clone(),
    );
    if !profile.auth_choice().is_auto()
        && site.supports(AuthMode::Pat)
        && auth_decision(cx, site, w.line.as_ref(), Some(&profile))
            .mode
            .is_none()
    {
        profile.set_auth(AuthChoice::Auto);
    }
    Some(profile)
}

/// Apply (the last step): the profile, key and switch to plan, or the official
/// login restore.
pub fn finish(cx: &Cx<'_>, w: &Wizard, out: &mut Vec<Action>) -> WizardOutcome {
    if w.official() {
        out.push(Action::Restore(RestoreTarget::Official));
        return WizardOutcome::Close;
    }
    let (site, profile, key) = if w.custom() {
        let Some((site, profile)) = w.custom.draft.clone() else {
            return WizardOutcome::Continue;
        };
        let mut profile = profile;
        if w.model.is_some() {
            profile.model.model = w.model.clone();
        }
        (Some(Box::new(site)), profile, w.selected_key())
    } else {
        let Some(profile) = planned_profile(cx, w) else {
            return WizardOutcome::Continue;
        };
        (None, profile, w.selected_key())
    };
    let mut request = SwitchRequest::profile(profile.id.clone());
    request.legacy_sessions = w.legacy;
    WizardOutcome::Finish {
        site,
        profile: Box::new(profile),
        key,
        request: Box::new(request),
    }
}

/// The Responses base URL of a compatible site: the line origin plus the base
/// path recorded in its models endpoint (`/v1/models` -> `<origin>/v1`).
pub fn site_base_url(site: &Site) -> String {
    let Some(line) = site.default_line() else {
        return String::new();
    };
    let path = site
        .endpoints
        .models_unauth
        .strip_suffix("/models")
        .unwrap_or(&site.endpoints.models_unauth);
    format!("{}{path}", line.origin)
}

/// A site id for a new custom site named `name`: ASCII, lower case, at most 48
/// characters, never a built-in id or one in `taken`.
pub fn custom_site_id(name: &str, taken: &[&str]) -> String {
    const LIMIT: usize = 48;
    let mut base = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
        if base.len() == LIMIT {
            break;
        }
    }
    base.truncate(base.trim_end_matches('-').len());
    if base.is_empty() {
        base.push_str("site");
    }
    let reserved = [
        catalog::KRILL_SITE_ID,
        catalog::OFFICIAL_SITE_ID,
        catalog::COMPATIBLE_TEMPLATE_SITE_ID,
        "original",
    ];
    let is_taken = |id: &str| reserved.contains(&id) || taken.contains(&id);
    let mut id = base.clone();
    let mut suffix = 2;
    while is_taken(&id) {
        let tail = format!("-{suffix}");
        let prefix = &base[..base.len().min(LIMIT - tail.len())];
        id = format!("{}{tail}", prefix.trim_end_matches('-'));
        suffix += 1;
    }
    id
}

/// A probe request of `site` (`line`, else its default line) with the wizard's
/// key.
pub(crate) fn probe_request(
    site: &SiteId,
    line: Option<LineId>,
    key: Option<KeyRefInput>,
    responses: bool,
) -> SiteProbeRequest {
    let mut request = SiteProbeRequest::new(site.clone());
    request.line_id = line;
    request.key = key;
    request.responses = responses;
    request
}
