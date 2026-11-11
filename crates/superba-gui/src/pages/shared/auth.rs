//! Whether a provider's API key can be used on a line, the way the planner
//! decides it (shared by Overview, Providers and the wizard), and the switch
//! action for a line.

use superba_core::{AuthDecision, LineCaps, LineId, Profile, Site};

use crate::pages::Action;

/// Whether the API key can be used on a line of `site` with probe `caps`
/// (`None` = not probed yet).
pub fn decision(site: &Site, caps: Option<&LineCaps>) -> AuthDecision {
    superba_core::key_auth(site, caps)
}

/// A switch of `profile` to `line`.
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
