//! Forms of the Providers page: the custom OpenAI-compatible site form (add, and
//! the base URL edit of a saved site) with its validation, the model-mapping
//! rows (`Site::model_map`, edited per model on the Models page), and the ids and
//! names of new providers. Pure: no I/O, so it is unit tested.

use std::collections::BTreeMap;

use superba_core::{AuthMode, ModelDefaults, Profile, ProfileId, Secret, Site, SiteId, SiteKind};

/// Ids Superba reserves for built-in sites and profiles, and every preset id of
/// the quick-pick (`catalog::presets()`): the switching service treats a new
/// profile saved under a preset id as a setup-wizard re-run and merges it into
/// the provider's existing profile on that site (possibly the applied one), so a
/// provider added or duplicated here must never get one.
pub const RESERVED_IDS: [&str; 4] = ["krill", "official", "original", "custom"];

/// Make a unique ASCII id from a display name (at most 48 characters including any
/// numeric collision suffix), never one of the built-in ids or `taken`.
pub fn slug_id(name: &str, taken: &[&str]) -> String {
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
    let is_taken = |id: &str| RESERVED_IDS.contains(&id) || taken.contains(&id);
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

/// The Responses API base URL of a compatible site (origin + the path the user
/// entered, without the trailing `/models` the probe uses).
pub fn base_url(site: &Site) -> String {
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

/// One row of the model mapping editor: the model Codex asks for and the model
/// this site should receive instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapRow {
    /// Model name Codex sends (`gpt-5.5`).
    pub from: String,
    /// Model name this site serves (`my-provider/gpt-5.5`).
    pub to: String,
}

/// Rows from a stored mapping, sorted by the Codex name.
pub fn rows(map: &BTreeMap<String, String>) -> Vec<MapRow> {
    map.iter()
        .map(|(from, to)| MapRow {
            from: from.clone(),
            to: to.clone(),
        })
        .collect()
}

/// Whether the form adds a site or edits one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormMode {
    /// New site and profile.
    Add,
    /// Existing site and its profile.
    Edit {
        /// Site being edited.
        site: SiteId,
        /// Its profile.
        profile: ProfileId,
    },
}

/// Form fields. The typed key is never shown in `Debug`.
#[derive(Clone, Default)]
pub struct SiteForm {
    /// Add or edit (`None` behaves like add).
    pub mode: Option<FormMode>,
    /// Display name.
    pub name: String,
    /// Responses API base URL (`https://api.example.com/v1`).
    pub url: String,
    /// API key; on edit an empty field keeps the stored key.
    pub key: String,
    /// Show the key.
    pub reveal: bool,
    /// Default model written as `model = ...` (empty = leave the user's value).
    pub model: String,
    /// Model mapping rows (empty rows are ignored).
    pub mapping: Vec<MapRow>,
}

impl std::fmt::Debug for SiteForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteForm")
            .field("mode", &self.mode)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("key_entered", &!self.key.trim().is_empty())
            .field("model", &self.model)
            .field("mapping", &self.mapping)
            .finish()
    }
}

impl SiteForm {
    /// An empty add form.
    pub fn add() -> Self {
        Self {
            mode: Some(FormMode::Add),
            ..Self::default()
        }
    }

    /// A form prefilled from a saved site (and its model map) and its profile.
    pub fn edit(site: &Site, profile: &Profile) -> Self {
        Self {
            mode: Some(FormMode::Edit {
                site: site.id.clone(),
                profile: profile.id.clone(),
            }),
            name: site.name.fallback.clone(),
            url: profile
                .base_url_override
                .as_ref()
                .map(|u| u.as_str().trim_end_matches('/').to_owned())
                .unwrap_or_else(|| base_url(site)),
            key: String::new(),
            reveal: false,
            model: profile.model.model.clone().unwrap_or_default(),
            mapping: rows(&site.model_map),
        }
    }

    /// True when editing.
    pub fn is_edit(&self) -> bool {
        matches!(self.mode, Some(FormMode::Edit { .. }))
    }

    /// The typed key, if any.
    pub fn typed_key(&self) -> Option<Secret> {
        let key = self.key.trim();
        (!key.is_empty()).then(|| Secret::new(key))
    }

    /// The parsed base URL, if valid.
    pub fn parsed_url(&self) -> Option<url::Url> {
        url::Url::parse(self.url.trim()).ok()
    }
}

/// Why a form cannot be saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormError {
    /// Name, URL or (when adding) key is empty.
    Required,
    /// The URL does not parse.
    InvalidUrl,
    /// The URL is not https (plain http only for loopback).
    InsecureUrl,
    /// The site constructor refused the URL or name (message from the backend).
    Rejected(String),
    /// A mapping row has only one side.
    IncompleteMapping,
    /// The same Codex model is mapped twice.
    DuplicateMapping(String),
}

impl FormError {
    /// i18n key of the message (`Rejected` carries its own text).
    pub fn key(&self) -> &'static str {
        match self {
            FormError::Required => "sites.form.required",
            FormError::InvalidUrl => "sites.form.invalid_url",
            FormError::InsecureUrl => "sites.form.insecure_url",
            FormError::Rejected(_) => "sites.form.rejected",
            FormError::IncompleteMapping => "sites.form.incomplete_mapping",
            FormError::DuplicateMapping(_) => "sites.form.duplicate_mapping",
        }
    }
}

/// A validated form, ready to save.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// Site to save (custom sites are user data, never a built-in id).
    pub site: Site,
    /// Its profile (API-key auth).
    pub profile: Profile,
    /// New key (always set when adding; `None` keeps the stored key on edit).
    pub key: Option<Secret>,
}

/// Parse the mapping rows: blank rows are dropped, half-filled rows and duplicate
/// Codex names are errors.
pub fn parse_mapping(rows: &[MapRow]) -> Result<BTreeMap<String, String>, FormError> {
    let mut map = BTreeMap::new();
    for row in rows {
        let (from, to) = (row.from.trim(), row.to.trim());
        match (from.is_empty(), to.is_empty()) {
            (true, true) => continue,
            (false, false) => {}
            _ => return Err(FormError::IncompleteMapping),
        }
        if map.insert(from.to_owned(), to.to_owned()).is_some() {
            return Err(FormError::DuplicateMapping(from.to_owned()));
        }
    }
    Ok(map)
}

/// The URL rule of custom sites: https, or http to a loopback host (local tests).
pub fn check_url(url: &url::Url) -> Result<(), FormError> {
    let loopback = matches!(
        url.host(),
        Some(url::Host::Ipv4(ip)) if ip.is_loopback()
    ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
        || url.host_str() == Some("localhost");
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        "http" => Err(FormError::InsecureUrl),
        _ => Err(FormError::InvalidUrl),
    }
}

/// Validate `form`. `taken` holds every site and profile id in use; `existing` is
/// the saved site and profile when editing. `make_site` builds the site from
/// (id, name, url) with the backend's own validation
/// (`superba_switch::new_compatible_site`).
pub fn prepare(
    form: &SiteForm,
    taken: &[&str],
    existing: Option<(&Site, &Profile)>,
    make_site: impl FnOnce(SiteId, &str, &url::Url) -> superba_core::Result<Site>,
) -> Result<Prepared, FormError> {
    let name = form.name.trim();
    let key = form.typed_key();
    let editing = existing.is_some();
    if name.is_empty() || form.url.trim().is_empty() || (!editing && key.is_none()) {
        return Err(FormError::Required);
    }
    let url = form.parsed_url().ok_or(FormError::InvalidUrl)?;
    check_url(&url)?;
    let mapping = parse_mapping(&form.mapping)?;
    let model = form.model.trim();
    let model = (!model.is_empty()).then(|| model.to_owned());
    let (site, profile) = match existing {
        Some((site, profile)) => {
            let mut new_site = make_site(site.id.clone(), name, &url)
                .map_err(|e| FormError::Rejected(e.to_string()))?;
            // Keep what the form does not edit.
            new_site.homepage = site.homepage.clone();
            new_site.model_map = mapping;
            let mut profile = profile.clone();
            profile.name = name.to_owned();
            profile.model.model = model;
            // An explicit override (profiles made with `spb --base-url`) follows
            // the edited URL instead of silently winning over it.
            if profile.base_url_override.is_some() {
                profile.base_url_override = Some(url.clone());
            }
            (new_site, profile)
        }
        None => {
            let id = slug_id(name, taken);
            let mut site = make_site(SiteId::new(id.as_str()), name, &url)
                .map_err(|e| FormError::Rejected(e.to_string()))?;
            site.model_map = mapping;
            let profile = Profile {
                model_overrides: Default::default(),
                model_picker: Default::default(),
                compaction: Default::default(),
                anthropic: None,
                model_source: None,
                id: ProfileId::new(id.as_str()),
                name: name.to_owned(),
                builtin: None,
                site_id: site.id.clone(),
                line_id: None,
                auth_mode: AuthMode::ApiKey,
                key_ref: None,
                base_url_override: None,
                model: ModelDefaults {
                    auto_compact: None,
                    context_window: None,
                    speed: None,
                    model,
                    reasoning_effort: None,
                },
                auth: superba_core::AuthChoice::from_mode(AuthMode::ApiKey),
                backup_lines: Vec::new(),
                disabled_lines: Default::default(),
                codex_settings: Default::default(),
            };
            (site, profile)
        }
    };
    debug_assert_eq!(site.kind, SiteKind::Compatible);
    Ok(Prepared { site, profile, key })
}

/// Profile id for a new provider on a preset site: the site id itself while no
/// profile has it (`krill`, the id the wizard and `spb` use), else a unique id
/// derived from the name.
pub fn preset_profile_id(site: &SiteId, name: &str, taken: &[&str]) -> ProfileId {
    if !taken.contains(&site.as_str()) {
        return ProfileId::new(site.as_str());
    }
    ProfileId::new(slug_id(name, taken))
}

/// `base` when no provider has that name, else `base 2`, `base 3`, ...
pub fn unique_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .unwrap_or_else(|| base.to_owned())
}
