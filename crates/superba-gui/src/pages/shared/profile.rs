//! Profile helpers shared by the provider pages and the wizard.

use superba_core::{AuthChoice, AuthMode, ModelDefaults, Profile, ProfileId, Site};

/// A new key-based profile for `site` (Automatic authentication, default lines).
/// Krill profiles start with Krill's recommended Codex settings, like the wizard
/// and `spb profile add`.
pub fn new_profile(site: &Site, name: String) -> Profile {
    let mut p = Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        anthropic: None,
        model_source: None,
        id: ProfileId::new(site.id.as_str()),
        name,
        builtin: None,
        site_id: site.id.clone(),
        line_id: None,
        backup_lines: Vec::new(),
        disabled_lines: Default::default(),
        auth_mode: AuthMode::Pat,
        auth: AuthChoice::Auto,
        key_ref: None,
        base_url_override: None,
        model: ModelDefaults::default(),
        codex_settings: if site.kind == superba_core::SiteKind::Krill {
            superba_core::CodexSettings::krill_recommended()
        } else {
            Default::default()
        },
    };
    // On a site without PAT, Automatic and API key are the same; the profile
    // stores the plain API-key form so every reader of `auth_mode` agrees.
    if site.supports(AuthMode::Pat) {
        p.set_auth(AuthChoice::Auto);
    } else {
        p.auth_mode = AuthMode::ApiKey;
        p.set_auth(AuthChoice::ApiKey);
    }
    p
}

/// What flipping "Disable OpenAI telemetry" of `profile` asks for (user bug
/// 137): save the profile with `analytics.enabled = false` written (`off`) or
/// no longer managed, and, when Codex uses the profile now, the usual switch
/// to it so the change reaches `config.toml` (the switch previews it first).
/// Empty when nothing changes.
pub fn telemetry_actions(
    data: &crate::pages::Data,
    profile: &Profile,
    off: bool,
) -> Vec<crate::pages::Action> {
    if profile.codex_settings.openai_telemetry_off() == off {
        return Vec::new();
    }
    let mut changed = profile.clone();
    changed.codex_settings.set_openai_telemetry_off(off);
    let mut out = vec![crate::pages::Action::SaveProfile {
        profile: Box::new(changed),
        key: None,
    }];
    if super::providers::applied_profile_id(data) == Some(&profile.id) {
        out.push(crate::pages::Action::Switch {
            profile: profile.id.clone(),
            line: None,
            auth: None,
        });
    }
    out
}
