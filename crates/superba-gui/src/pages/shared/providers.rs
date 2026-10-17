//! Providers as the UI lists them (UX §2.2, §3.3): a provider is a profile (site +
//! lines + auth + key + its model defaults and Codex settings). Presets (Krill
//! kind), custom OpenAI-compatible sites (also a site that has no key yet) and the
//! official login. Shared by Providers, Models and Settings, so none of them
//! special-cases a site id.

use superba_core::{BuiltinProfile, Profile, ProfileId, Site, SiteId, SiteKind, catalog};

use crate::pages::Data;

/// What a provider is in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderKind {
    /// A preset site (Krill kind: built-in lines, PAT, usage API).
    Preset,
    /// A custom OpenAI-compatible site.
    Custom,
    /// The official ChatGPT login.
    Official,
}

impl ProviderKind {
    /// Kind of `site`.
    pub fn of(site: &Site) -> Self {
        match site.kind {
            SiteKind::Official => ProviderKind::Official,
            SiteKind::Compatible => ProviderKind::Custom,
            _ => ProviderKind::Preset,
        }
    }

    /// i18n key of the kind badge.
    pub fn badge_key(self) -> &'static str {
        match self {
            ProviderKind::Preset => "providers.kind.preset",
            ProviderKind::Custom => "providers.kind.custom",
            ProviderKind::Official => "providers.kind.official",
        }
    }
}

/// Identity of a list entry: a profile, or a custom site that has no profile yet.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    /// A profile (also the official built-in).
    Profile(ProfileId),
    /// A custom site without a key (its profile is created with the key).
    Site(SiteId),
}

impl ProviderId {
    /// Command form (`profile:<id>` / `site:<id>`).
    pub fn command(&self) -> String {
        match self {
            ProviderId::Profile(p) => format!("profile:{p}"),
            ProviderId::Site(s) => format!("site:{s}"),
        }
    }
}

/// One provider row.
#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    /// Identity.
    pub id: ProviderId,
    /// The profile (absent for a key-less custom site).
    pub profile: Option<Profile>,
    /// Its site.
    pub site: Site,
    /// Kind.
    pub kind: ProviderKind,
    /// Codex uses it now.
    pub applied: bool,
}

impl Provider {
    /// The profile id, when there is a profile.
    pub fn profile_id(&self) -> Option<&ProfileId> {
        self.profile.as_ref().map(|p| &p.id)
    }

    /// A key is stored for it (always true for the official login).
    pub fn has_key(&self) -> bool {
        self.kind == ProviderKind::Official
            || self.profile.as_ref().is_some_and(|p| p.key_ref.is_some())
    }
}

/// The site `id` from the backend list, else the built-in one.
pub fn site(data: &Data, id: &SiteId) -> Option<Site> {
    data.site(id)
}

/// The profile Codex uses now (from the overview).
pub fn applied_profile_id(data: &Data) -> Option<&ProfileId> {
    data.active_state()?.profile_id.as_ref()
}

/// The profile `id`.
pub fn profile<'a>(data: &'a Data, id: &ProfileId) -> Option<&'a Profile> {
    data.profile(id)
}

/// Every provider, in list order: the applied one first, then presets, custom
/// sites, the official login (each group by name).
pub fn list(data: &Data) -> Vec<Provider> {
    let applied = applied_profile_id(data);
    let applied_site = data.active_site_id().cloned();
    let mut out: Vec<Provider> = Vec::new();
    let profiles = data
        .profiles
        .value
        .as_ref()
        .map(|s| s.profiles.as_slice())
        .unwrap_or_default();
    for p in profiles {
        if p.builtin == Some(BuiltinProfile::Original) {
            continue;
        }
        let official = p.builtin == Some(BuiltinProfile::Official);
        let site_id = if official {
            SiteId::new(catalog::OFFICIAL_SITE_ID)
        } else {
            p.site_id.clone()
        };
        let Some(site) = site(data, &site_id) else {
            continue;
        };
        // The built-in official profile stands for the login; a user profile on
        // the official site is not a provider of its own.
        if site.kind == SiteKind::Official && !official {
            continue;
        }
        out.push(Provider {
            id: ProviderId::Profile(p.id.clone()),
            applied: Some(&p.id) == applied
                || (official
                    && applied.is_none()
                    && applied_site
                        .as_ref()
                        .is_some_and(|s| s == catalog::OFFICIAL_SITE_ID)),
            kind: ProviderKind::of(&site),
            profile: Some(p.clone()),
            site,
        });
    }
    // Custom sites that have no profile yet.
    for s in data.sites.iter().filter(|s| s.kind == SiteKind::Compatible) {
        if !out
            .iter()
            .any(|p| p.site.id == s.id && p.kind == ProviderKind::Custom)
        {
            out.push(Provider {
                id: ProviderId::Site(s.id.clone()),
                profile: None,
                site: s.clone(),
                kind: ProviderKind::Custom,
                applied: false,
            });
        }
    }
    // The official login is always offered.
    if !out.iter().any(|p| p.kind == ProviderKind::Official) {
        let site = catalog::official_site();
        let builtin = catalog::builtin_profiles()
            .into_iter()
            .find(|p| p.builtin == Some(BuiltinProfile::Official));
        out.push(Provider {
            id: ProviderId::Profile(ProfileId::new(catalog::PROFILE_OFFICIAL)),
            applied: applied_site
                .as_ref()
                .is_some_and(|s| s == catalog::OFFICIAL_SITE_ID),
            kind: ProviderKind::Official,
            profile: builtin,
            site,
        });
    }
    out.sort_by(|a, b| {
        b.applied
            .cmp(&a.applied)
            .then(a.kind.cmp(&b.kind))
            .then_with(|| name_of(a).to_lowercase().cmp(&name_of(b).to_lowercase()))
    });
    out
}

/// The stored display name (profile name, else the site's English name).
fn name_of(p: &Provider) -> String {
    p.profile
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| p.site.name.fallback.clone())
}

/// Providers whose models and Codex settings the pages edit: every provider with
/// a profile (key-less custom sites have none), the official login last.
pub fn with_profiles(data: &Data) -> Vec<Provider> {
    list(data)
        .into_iter()
        .filter(|p| p.profile.is_some())
        .collect()
}
