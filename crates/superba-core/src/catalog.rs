//! Built-in sites, lines and profiles.
//!
//! The Krill lines are public entry points. Line health is never hard-coded here:
//! e.g. the CN line's edge currently intercepts `/backend-api/wham/*`, which the
//! [`crate::LineProber`] detects at run time and reports through [`crate::LineCaps`].

use serde::{Deserialize, Serialize};
use url::Url;

use crate::auth::AuthMode;
use crate::i18n::LocalizedText;
use crate::ids::{LineId, ProfileId, SiteId};
use crate::origin::{CodexPath, Origin};
use crate::profile::{BuiltinProfile, ModelDefaults, Profile};
use crate::settings::{AuthChoice, CodexSettings};
use crate::site::{Line, LineSource, Site, SiteCapabilities, SiteEndpoints, SiteKind};

/// Site id of Krill.
pub const KRILL_SITE_ID: &str = "krill";
/// Site id of the official ChatGPT login.
pub const OFFICIAL_SITE_ID: &str = "official";
/// Krill main web site.
pub const KRILL_HOMEPAGE: &str = "https://www.krill-code.com";
/// Official ChatGPT web site.
pub const OFFICIAL_HOMEPAGE: &str = "https://chatgpt.com";

/// Krill line: China express (国内极速).
pub const KRILL_LINE_CN_FAST: &str = "cn-fast";
/// Krill line: overseas (海外线路).
pub const KRILL_LINE_OVERSEAS: &str = "overseas";
/// Krill line: three-network optimized CDN (三网优化 CDN).
pub const KRILL_LINE_CDN: &str = "cdn";
/// Line used before any probe result exists.
pub const KRILL_DEFAULT_LINE: &str = KRILL_LINE_OVERSEAS;

/// Origin of [`KRILL_LINE_CN_FAST`].
pub const KRILL_ORIGIN_CN_FAST: &str = "https://api-slb.kbsfdxc.net";
/// Origin of [`KRILL_LINE_OVERSEAS`].
pub const KRILL_ORIGIN_OVERSEAS: &str = "https://api.kbsfdxc.net";
/// Origin of [`KRILL_LINE_CDN`].
pub const KRILL_ORIGIN_CDN: &str = "https://api.kbsfdxc.com";

/// Profile id of the built-in "original configuration" restore anchor.
pub const PROFILE_ORIGINAL: &str = "original";
/// Profile id of the built-in "official ChatGPT login" profile.
pub const PROFILE_OFFICIAL: &str = "official";

/// Registrable domains operated by Krill. Used to recognise legacy tutorial lines
/// (`export CODEX_AUTHAPI_BASE_URL=...`) and legacy `[model_providers.krill]` tables.
pub const KRILL_DOMAINS: &[&str] = &[
    "krill-code.com",
    "krill-code.net",
    "cdn-krill-ai.com",
    "krill-ai.com",
    "kbsfdxc.com",
    "kbsfdxc.net",
];

/// True if `host` is a Krill domain or one of its subdomains.
pub fn is_krill_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    KRILL_DOMAINS
        .iter()
        .any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

fn origin(s: &str) -> Origin {
    Origin::parse(s).expect("built-in origin is valid")
}

fn url(s: &str) -> Url {
    Url::parse(s).expect("built-in URL is valid")
}

/// The Krill site with its three public lines.
pub fn krill_site() -> Site {
    let line = |id: &str, key: &str, fallback: &str, o: &str| Line {
        id: LineId::new(id),
        name: LocalizedText::new(key, fallback),
        origin: origin(o),
        source: LineSource::Builtin,
    };
    Site {
        id: SiteId::new(KRILL_SITE_ID),
        kind: SiteKind::Krill,
        name: LocalizedText::new("site.krill", "Krill"),
        homepage: Some(url(KRILL_HOMEPAGE)),
        lines: vec![
            line(
                KRILL_LINE_CN_FAST,
                "line.krill.cn-fast",
                "China Express",
                KRILL_ORIGIN_CN_FAST,
            ),
            line(
                KRILL_LINE_OVERSEAS,
                "line.krill.overseas",
                "Overseas",
                KRILL_ORIGIN_OVERSEAS,
            ),
            line(
                KRILL_LINE_CDN,
                "line.krill.cdn",
                "Multi-carrier CDN",
                KRILL_ORIGIN_CDN,
            ),
        ],
        capabilities: SiteCapabilities {
            auth_modes: vec![AuthMode::Pat, AuthMode::ApiKey],
            codex_paths: vec![CodexPath::BackendApiCodex, CodexPath::CodexV1],
            latency_probe: true,
            key_check: true,
            account_summary: true,
            usage_api: true,
            model_catalog: true,
            // Production support of tier-suffix model names is unconfirmed (v0.3
            // decision 2): off until confirmed.
            tier_model_suffix: false,
        },
        endpoints: SiteEndpoints::default(),
        default_line: Some(LineId::new(KRILL_DEFAULT_LINE)),
        model_map: Default::default(),
    }
}

/// The official ChatGPT login pseudo-site (no lines).
pub fn official_site() -> Site {
    Site {
        id: SiteId::new(OFFICIAL_SITE_ID),
        kind: SiteKind::Official,
        name: LocalizedText::new("site.official", "Official ChatGPT"),
        homepage: Some(url(OFFICIAL_HOMEPAGE)),
        lines: Vec::new(),
        capabilities: SiteCapabilities {
            auth_modes: vec![AuthMode::OfficialChatGpt],
            codex_paths: Vec::new(),
            latency_probe: false,
            key_check: false,
            account_summary: false,
            usage_api: false,
            model_catalog: false,
            tier_model_suffix: false,
        },
        endpoints: SiteEndpoints::default(),
        default_line: None,
        model_map: Default::default(),
    }
}

/// Capabilities of a user-created OpenAI-compatible (Responses API) site.
pub fn compatible_capabilities() -> SiteCapabilities {
    SiteCapabilities {
        auth_modes: vec![AuthMode::ApiKey],
        codex_paths: vec![CodexPath::AsIs],
        latency_probe: false,
        key_check: false,
        account_summary: true,
        usage_api: false,
        model_catalog: true,
        tier_model_suffix: false,
    }
}

/// All built-in sites (Krill first).
pub fn builtin_sites() -> Vec<Site> {
    vec![krill_site(), official_site()]
}

/// The two built-in profiles: "Original configuration" and "Official ChatGPT".
pub fn builtin_profiles() -> Vec<Profile> {
    vec![
        Profile {
            id: ProfileId::new(PROFILE_ORIGINAL),
            name: "Original configuration".into(),
            builtin: Some(BuiltinProfile::Original),
            site_id: SiteId::new(OFFICIAL_SITE_ID),
            line_id: None,
            backup_lines: Vec::new(),
            disabled_lines: Default::default(),
            auth_mode: AuthMode::OfficialChatGpt,
            auth: AuthChoice::Auto,
            key_ref: None,
            base_url_override: None,
            model: ModelDefaults::default(),
            codex_settings: CodexSettings::new(),
            model_overrides: Default::default(),
            model_picker: Default::default(),
            compaction: Default::default(),
            anthropic: None,
            model_source: None,
        },
        Profile {
            id: ProfileId::new(PROFILE_OFFICIAL),
            name: "Official ChatGPT".into(),
            builtin: Some(BuiltinProfile::Official),
            site_id: SiteId::new(OFFICIAL_SITE_ID),
            line_id: None,
            backup_lines: Vec::new(),
            disabled_lines: Default::default(),
            auth_mode: AuthMode::OfficialChatGpt,
            auth: AuthChoice::Auto,
            key_ref: None,
            base_url_override: None,
            model: ModelDefaults::default(),
            codex_settings: CodexSettings::new(),
            model_overrides: Default::default(),
            model_picker: Default::default(),
            compaction: Default::default(),
            anthropic: None,
            model_source: None,
        },
    ]
}

/// Preset id of Krill (the default selection of the quick-pick).
pub const PRESET_KRILL: &str = "krill";
/// Preset id of the official ChatGPT login.
pub const PRESET_OFFICIAL: &str = "official";
/// Preset id of the custom OpenAI-compatible site template.
pub const PRESET_CUSTOM: &str = "custom";
/// Site id of the compatible-site template. Never stored: a site created from the
/// template gets its own id (`superba_net::new_compatible_site`).
pub const COMPATIBLE_TEMPLATE_SITE_ID: &str = "compatible";

/// A provider preset of the first-run wizard and the Add-provider dialog. Presets
/// are data: nothing else in the UI special-cases a site; site-specific UI follows
/// [`SiteCapabilities`]. Titles and descriptions are the UI's
/// `preset.<id>.title` / `preset.<id>.body` texts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// Stable id ([`PRESET_KRILL`], [`PRESET_OFFICIAL`], [`PRESET_CUSTOM`]).
    pub id: String,
    /// The site a profile created from the preset uses; for [`PRESET_CUSTOM`] a
    /// template without lines ([`compatible_template_site`]).
    pub site: Site,
    /// Pre-selected in the quick-pick.
    pub default: bool,
    /// Placeholder of the key field (`nb_…` for Krill); `None` = a neutral one.
    #[serde(default)]
    pub key_placeholder: Option<String>,
}

impl Preset {
    /// What a site created from this preset supports.
    pub fn caps(&self) -> &SiteCapabilities {
        &self.site.capabilities
    }
}

/// The custom-site template: compatible capabilities, no lines.
pub fn compatible_template_site() -> Site {
    Site {
        id: SiteId::new(COMPATIBLE_TEMPLATE_SITE_ID),
        kind: SiteKind::Compatible,
        name: LocalizedText::new("site.compatible", "OpenAI-compatible site"),
        homepage: None,
        lines: Vec::new(),
        capabilities: compatible_capabilities(),
        endpoints: SiteEndpoints::default(),
        default_line: None,
        model_map: Default::default(),
    }
}

/// Provider presets in quick-pick order: Krill (default), the official login, a
/// custom compatible site.
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            id: PRESET_KRILL.into(),
            site: krill_site(),
            default: true,
            key_placeholder: Some("nb_\u{2026}".into()),
        },
        Preset {
            id: PRESET_OFFICIAL.into(),
            site: official_site(),
            default: false,
            key_placeholder: None,
        },
        Preset {
            id: PRESET_CUSTOM.into(),
            site: compatible_template_site(),
            default: false,
            key_placeholder: None,
        },
    ]
}
