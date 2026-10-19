//! What Automatic authentication resolves to, the way the planner decides it
//! (shared by Overview, Providers and the wizard), and the switch action for a line.

use superba_core::{
    AuthChoice, AuthContext, AuthDecision, AuthMode, Line, LineCaps, LineId, Profile, Site,
};

use crate::pages::Action;

/// What the planner knows besides the line when it resolves Automatic
/// authentication (see `superba_switch::planner`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoFacts {
    /// Every local Codex supports PAT (`None` = unknown); see
    /// `superba_switch::rules::pat_version_ok`.
    pub pat_version_ok: Option<bool>,
    /// Router mode is on (this computer's Codex goes through the local router).
    pub router: bool,
}

/// The facts of [`AutoFacts`] from the overview (app-bundled Codex and the CLI
/// first on the PATH, like the planner) and the router settings.
pub fn auto_facts(data: &crate::pages::Data) -> AutoFacts {
    let pat_version_ok = data.overview.value.as_ref().and_then(|o| {
        let install_id = o
            .app
            .session
            .as_ref()
            .and_then(|s| s.install_id.clone())
            .or_else(|| {
                data.profiles
                    .value
                    .as_ref()
                    .and_then(|p| p.settings.preferred_app_id.clone())
            });
        let consumers = superba_switch::rules::local_versioned_consumers(
            install_id.as_deref(),
            &o.app.installs,
            &o.cli.binaries,
            superba_switch::planner::APP_CONSUMER_LABEL,
        );
        superba_switch::rules::pat_version_ok(&consumers)
    });
    AutoFacts {
        pat_version_ok,
        router: data.router_enabled(),
    }
}

/// What Automatic (or the manual choice) resolves to on `line`, with nothing
/// known about the Codex versions or router mode.
pub fn decision(
    site: &Site,
    line: Option<&Line>,
    caps: Option<&LineCaps>,
    profile: Option<&Profile>,
) -> AuthDecision {
    decision_with(site, line, caps, profile, AutoFacts::default())
}

/// What Automatic (or the manual choice) resolves to on `line`, the way the
/// planner decides it: a local Codex too old for PAT and router mode make
/// Automatic use an API key.
pub fn decision_with(
    site: &Site,
    line: Option<&Line>,
    caps: Option<&LineCaps>,
    profile: Option<&Profile>,
    facts: AutoFacts,
) -> AuthDecision {
    // A site without PAT has one mode; show it as Automatic with its reason.
    let choice = if site.supports(AuthMode::Pat) {
        profile.map_or(AuthChoice::Auto, Profile::auth_choice)
    } else {
        AuthChoice::Auto
    };
    let mut context = AuthContext::new(site);
    context.line = line;
    context.caps = caps;
    context.custom_base_url = profile.is_some_and(|p| p.base_url_override.is_some());
    context.router = facts.router && profile.is_some_and(|p| !p.is_builtin());
    if choice.is_auto() {
        context.pat_version_ok = facts.pat_version_ok;
    }
    choice.decide(&context)
}

/// A switch of `profile` to `line`. The auth mode is left to the planner, which
/// resolves the profile's choice (Automatic: PAT, or an API key with a stated
/// reason on a line without PAT); an explicit mode here would be stored as a
/// manual choice.
pub fn switch_action(data: &crate::pages::Data, profile: &Profile, line: &LineId) -> Action {
    if data.router_enabled()
        && let Some(site) = data.site(&profile.site_id)
    {
        let mut profile = profile.clone();
        if profile.prefer_line(&site, line.clone()).is_ok() {
            return Action::SaveProfile {
                profile: Box::new(profile),
                key: None,
            };
        }
    }
    Action::Switch {
        profile: profile.id.clone(),
        line: Some(line.clone()),
        auth: None,
    }
}
