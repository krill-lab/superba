//! Site and line catalog: the built-in sites, user-defined compatible sites (stored
//! by the app layer in `profiles.json`) and loopback test lines for e2e runs.
//!
//! This module only validates and serves sites; persistence belongs to
//! `superba-config`'s `ProfileStore`. Validation keeps keys safe: a site's key is
//! only ever sent to that site's line origins, so a site definition that could
//! point a key somewhere unexpected (credentials in a URL, absolute endpoint
//! paths on another host, plain http to a non-loopback host) is refused.

use superba_core::catalog::{self, KRILL_SITE_ID, OFFICIAL_SITE_ID};
use superba_core::{
    AuthMode, CodexPath, EnvSource, Error, Line, LineId, LineSource, LocalizedText, Origin, Result,
    Site, SiteEndpoints, SiteId, SiteKind,
};
use url::Url;

/// Environment variable with extra Krill lines for e2e runs, e.g.
/// `SUPERBA_TEST_LINES="a=http://127.0.0.1:18001,b=http://127.0.0.1:18002"`.
/// Only loopback origins are accepted; the lines are added to the Krill site with
/// [`LineSource::Test`].
pub const ENV_SUPERBA_TEST_LINES: &str = "SUPERBA_TEST_LINES";

/// Longest accepted site or line id.
const MAX_ID_LEN: usize = 64;

/// Built-in sites plus validated custom sites, in display order (built-ins first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteCatalog {
    sites: Vec<Site>,
}

impl Default for SiteCatalog {
    fn default() -> Self {
        Self::builtin()
    }
}

impl SiteCatalog {
    /// Only the built-in sites (Krill with its three lines, Official).
    pub fn builtin() -> Self {
        Self {
            sites: catalog::builtin_sites(),
        }
    }

    /// Built-ins plus `custom` sites. Every custom site must pass
    /// [`validate_custom_site`] and ids must be unique.
    pub fn with_custom(custom: impl IntoIterator<Item = Site>) -> Result<Self> {
        let mut cat = Self::builtin();
        for site in custom {
            cat.add_custom(site)?;
        }
        Ok(cat)
    }

    /// Built-ins, then the test lines from [`ENV_SUPERBA_TEST_LINES`] (if set),
    /// then `custom` sites.
    pub fn from_env(env: &dyn EnvSource, custom: impl IntoIterator<Item = Site>) -> Result<Self> {
        let mut cat = Self::builtin();
        if let Some(spec) = env.var_non_empty(ENV_SUPERBA_TEST_LINES) {
            let lines = parse_test_lines(&spec)?;
            cat.add_lines(&SiteId::new(KRILL_SITE_ID), lines)?;
        }
        for site in custom {
            cat.add_custom(site)?;
        }
        Ok(cat)
    }

    /// Add one validated custom site.
    pub fn add_custom(&mut self, site: Site) -> Result<()> {
        validate_custom_site(&site)?;
        if self.site(&site.id).is_some() {
            return Err(Error::Conflict {
                message: format!("a site with id {:?} already exists", site.id.as_str()),
            });
        }
        self.sites.push(site);
        Ok(())
    }

    /// Append lines to an existing site (e2e test lines). Ids and origins must stay
    /// unique and the result must still validate.
    pub fn add_lines(&mut self, site_id: &SiteId, lines: Vec<Line>) -> Result<()> {
        let site = self
            .sites
            .iter_mut()
            .find(|s| &s.id == site_id)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {site_id}"),
            })?;
        let mut candidate = site.clone();
        candidate.lines.extend(lines);
        validate_site(&candidate)?;
        *site = candidate;
        Ok(())
    }

    /// All sites in display order.
    pub fn sites(&self) -> &[Site] {
        &self.sites
    }

    /// Consume into the site list.
    pub fn into_sites(self) -> Vec<Site> {
        self.sites
    }

    /// Look up a site.
    pub fn site(&self, id: &SiteId) -> Option<&Site> {
        self.sites.iter().find(|s| &s.id == id)
    }

    /// Sites that were not compiled in (user-defined compatible sites).
    pub fn custom_sites(&self) -> impl Iterator<Item = &Site> {
        self.sites.iter().filter(|s| !is_builtin_site_id(&s.id))
    }

    /// Resolve a site and a line (`None` = the site's default line).
    pub fn resolve(&self, site_id: &SiteId, line_id: Option<&LineId>) -> Result<(&Site, &Line)> {
        let site = self.site(site_id).ok_or_else(|| Error::NotFound {
            what: format!("site {site_id}"),
        })?;
        let line = match line_id {
            Some(id) => site.line(id),
            None => site.default_line(),
        }
        .ok_or_else(|| Error::NotFound {
            what: match line_id {
                Some(id) => format!("line {id} of site {site_id}"),
                None => format!("a line of site {site_id}"),
            },
        })?;
        Ok((site, line))
    }
}

/// True for the ids of compiled-in sites.
pub fn is_builtin_site_id(id: &SiteId) -> bool {
    id.as_str() == KRILL_SITE_ID || id.as_str() == OFFICIAL_SITE_ID
}

/// Build a user-defined OpenAI-compatible site from its Responses base URL
/// (e.g. `https://api.example.com/v1`). The site gets one line (the URL's
/// origin) and its models path is `<base path>/models`, so key checks hit the
/// same catalog the profile's `openai_base_url` serves.
pub fn new_compatible_site(id: SiteId, name: &str, base_url: &Url) -> Result<Site> {
    let origin = Origin::parse(&base_url.origin().ascii_serialization())?;
    if !base_url.username().is_empty() || base_url.password().is_some() {
        return Err(Error::invalid("the base URL must not contain credentials"));
    }
    if base_url.query().is_some() || base_url.fragment().is_some() {
        return Err(Error::invalid(
            "the base URL must not contain a query or fragment",
        ));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::invalid("the site name must not be empty"));
    }
    let base_path = base_url.path().trim_end_matches('/');
    let endpoints = SiteEndpoints {
        models_unauth: format!("{base_path}/models"),
        ..SiteEndpoints::default()
    };
    let line_id = LineId::new("default");
    let site = Site {
        id,
        kind: SiteKind::Compatible,
        name: LocalizedText::new("", name),
        homepage: None,
        lines: vec![Line {
            id: line_id.clone(),
            name: LocalizedText::new("", origin.host()),
            origin,
            source: LineSource::User,
        }],
        capabilities: catalog::compatible_capabilities(),
        endpoints,
        default_line: Some(line_id),
        model_map: Default::default(),
    };
    validate_custom_site(&site)?;
    Ok(site)
}

/// Parse [`ENV_SUPERBA_TEST_LINES`]: comma-separated `id=origin` pairs, loopback
/// origins only.
pub fn parse_test_lines(spec: &str) -> Result<Vec<Line>> {
    let mut out: Vec<Line> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (id, origin) = part.split_once('=').ok_or_else(|| {
            Error::invalid(format!(
                "{ENV_SUPERBA_TEST_LINES}: expected id=origin, got {part:?}"
            ))
        })?;
        let id = id.trim();
        validate_id("test line id", id)?;
        let origin = Origin::parse(origin.trim())?;
        if !origin.is_loopback() {
            return Err(Error::invalid(format!(
                "{ENV_SUPERBA_TEST_LINES}: {origin} is not a loopback origin"
            )));
        }
        out.push(Line {
            id: LineId::new(id),
            name: LocalizedText::new("", format!("Test {id}")),
            origin,
            source: LineSource::Test,
        });
    }
    if out.is_empty() {
        return Err(Error::invalid(format!(
            "{ENV_SUPERBA_TEST_LINES} is set but lists no lines"
        )));
    }
    Ok(out)
}

/// Structural validation of any site (built-in, custom or test-extended).
pub fn validate_site(site: &Site) -> Result<()> {
    let sid = site.id.as_str();
    validate_id("site id", sid)?;
    if site.name.fallback.trim().is_empty() && site.name.key.trim().is_empty() {
        return Err(Error::invalid(format!("site {sid}: empty name")));
    }
    if let Some(home) = &site.homepage
        && home.scheme() != "https"
    {
        return Err(Error::invalid(format!(
            "site {sid}: homepage must be https"
        )));
    }
    let caps = &site.capabilities;
    if caps.auth_modes.is_empty() {
        return Err(Error::invalid(format!("site {sid}: no auth modes")));
    }
    for (i, m) in caps.auth_modes.iter().enumerate() {
        if caps.auth_modes[..i].contains(m) {
            return Err(Error::invalid(format!(
                "site {sid}: duplicate auth mode {m:?}"
            )));
        }
    }
    match site.kind {
        SiteKind::Official => {
            if caps.auth_modes.iter().any(|m| m.uses_key()) {
                return Err(Error::invalid(format!(
                    "site {sid}: the official site has no key modes"
                )));
            }
            if !site.lines.is_empty() || !caps.codex_paths.is_empty() {
                return Err(Error::invalid(format!(
                    "site {sid}: the official site has no lines or Codex paths"
                )));
            }
        }
        SiteKind::Krill => {
            if caps
                .auth_modes
                .iter()
                .any(|m| !matches!(m, AuthMode::Pat | AuthMode::ApiKey))
            {
                return Err(Error::invalid(format!(
                    "site {sid}: Krill sites offer PAT and API key only"
                )));
            }
            if caps.codex_paths.is_empty() || caps.codex_paths.contains(&CodexPath::AsIs) {
                return Err(Error::invalid(format!(
                    "site {sid}: Krill sites need /backend-api/codex or /codex/v1 paths"
                )));
            }
            if caps.auth_modes.contains(&AuthMode::Pat)
                && !caps.codex_paths.contains(&CodexPath::BackendApiCodex)
            {
                return Err(Error::invalid(format!(
                    "site {sid}: PAT needs the /backend-api/codex path"
                )));
            }
        }
        SiteKind::Compatible => {
            if caps.auth_modes != [AuthMode::ApiKey] {
                return Err(Error::invalid(format!(
                    "site {sid}: compatible sites use API key auth only"
                )));
            }
            if caps.codex_paths != [CodexPath::AsIs] {
                return Err(Error::invalid(format!(
                    "site {sid}: compatible sites use their base URL as is"
                )));
            }
        }
        _ => {
            return Err(Error::invalid(format!("site {sid}: unknown site kind")));
        }
    }
    if site.kind != SiteKind::Official && site.lines.is_empty() {
        return Err(Error::invalid(format!("site {sid}: no lines")));
    }
    for (i, line) in site.lines.iter().enumerate() {
        validate_id("line id", line.id.as_str())?;
        let earlier = &site.lines[..i];
        if earlier.iter().any(|l| l.id == line.id) {
            return Err(Error::invalid(format!(
                "site {sid}: duplicate line id {}",
                line.id
            )));
        }
        if earlier.iter().any(|l| l.origin == line.origin) {
            return Err(Error::invalid(format!(
                "site {sid}: duplicate line origin {}",
                line.origin
            )));
        }
        if line.source == LineSource::Test && !line.origin.is_loopback() {
            return Err(Error::invalid(format!(
                "site {sid}: test line {} must use a loopback origin",
                line.id
            )));
        }
    }
    if let Some(d) = &site.default_line
        && site.line(d).is_none()
    {
        return Err(Error::invalid(format!(
            "site {sid}: default line {d} does not exist"
        )));
    }
    validate_endpoints(site)?;
    validate_model_map(site)
}

/// Longest accepted model name in a model map.
const MAX_MODEL_LEN: usize = 128;
/// Most entries in one site's model map.
const MAX_MODEL_MAP: usize = 64;

/// True for a plausible model name: 1-128 printable ASCII characters without
/// whitespace or quotes (it is written into `config.toml` and request bodies).
pub fn valid_model_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_MODEL_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'\''))
}

/// Model map entries are valid model names, at most 64, and never map a model
/// to itself.
fn validate_model_map(site: &Site) -> Result<()> {
    if site.model_map.len() > MAX_MODEL_MAP {
        return Err(Error::invalid(format!(
            "site {}: at most {MAX_MODEL_MAP} model mappings",
            site.id
        )));
    }
    for (codex, mapped) in &site.model_map {
        if !valid_model_name(codex) || !valid_model_name(mapped) {
            return Err(Error::invalid(format!(
                "site {}: invalid model mapping {codex:?} -> {mapped:?}",
                site.id
            )));
        }
        if codex == mapped {
            return Err(Error::invalid(format!(
                "site {}: model {codex:?} is mapped to itself",
                site.id
            )));
        }
    }
    Ok(())
}

/// [`validate_site`] plus the rules for user-defined sites: compatible kind, not a
/// built-in id, no built-in lines.
pub fn validate_custom_site(site: &Site) -> Result<()> {
    if is_builtin_site_id(&site.id) {
        return Err(Error::invalid(format!(
            "site id {:?} is reserved for a built-in site",
            site.id.as_str()
        )));
    }
    if site.kind != SiteKind::Compatible {
        return Err(Error::invalid(format!(
            "site {}: custom sites must be OpenAI-compatible sites",
            site.id
        )));
    }
    if site.lines.iter().any(|l| l.source == LineSource::Builtin) {
        return Err(Error::invalid(format!(
            "site {}: custom sites cannot carry built-in lines",
            site.id
        )));
    }
    validate_site(site)
}

/// Ids are lowercase ASCII `[a-z0-9._-]`, start with a letter or digit, at most
/// 64 characters (they are persisted and used in file names).
fn validate_id(what: &str, id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{what} {id:?}: use 1-{MAX_ID_LEN} lowercase letters, digits, '.', '_' or '-'"
        )))
    }
}

/// Endpoint paths are absolute, without query, fragment, `..` or whitespace, so
/// every request stays on the line's own origin.
fn validate_endpoints(site: &Site) -> Result<()> {
    let e = &site.endpoints;
    for (name, path) in [
        ("models_unauth", e.models_unauth.as_str()),
        ("whoami", e.whoami.as_str()),
        ("accounts_check", e.accounts_check.as_str()),
        ("backend_codex_models", e.backend_codex_models.as_str()),
        ("codex_v1_models", e.codex_v1_models.as_str()),
        ("billing_subscription", e.billing_subscription.as_str()),
        ("billing_usage", e.billing_usage.as_str()),
        ("usage_completions", e.usage_completions.as_str()),
        ("costs", e.costs.as_str()),
    ] {
        validate_path(site, name, path)?;
    }
    Ok(())
}

fn validate_path(site: &Site, name: &str, path: &str) -> Result<()> {
    let bad = !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#', '\\'])
        || path.chars().any(char::is_whitespace)
        || path.split('/').any(|seg| seg == "..");
    if bad {
        return Err(Error::invalid(format!(
            "site {}: endpoint {name} must be an absolute path, got {path:?}",
            site.id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use superba_core::MapEnv;

    use super::*;

    #[test]
    fn builtin_sites_validate() {
        for s in catalog::builtin_sites() {
            validate_site(&s).unwrap_or_else(|e| panic!("{}: {e}", s.id));
        }
        let cat = SiteCatalog::builtin();
        assert_eq!(cat.sites().len(), 2);
        assert_eq!(cat.custom_sites().count(), 0);
        let (site, line) = cat.resolve(&SiteId::new("krill"), None).unwrap();
        assert_eq!(site.id.as_str(), "krill");
        assert_eq!(line.id.as_str(), "overseas");
        assert!(
            cat.resolve(&SiteId::new("krill"), Some(&LineId::new("nope")))
                .is_err()
        );
        assert!(cat.resolve(&SiteId::new("official"), None).is_err());
    }

    #[test]
    fn compatible_sites_are_built_from_a_base_url() {
        let url = Url::parse("https://api.example.com/v1/").unwrap();
        let s = new_compatible_site(SiteId::new("example"), " Example ", &url).unwrap();
        assert_eq!(s.kind, SiteKind::Compatible);
        assert_eq!(s.lines.len(), 1);
        assert_eq!(s.lines[0].origin.as_str(), "https://api.example.com");
        assert_eq!(s.endpoints.models_unauth, "/v1/models");
        assert_eq!(s.name.fallback, "Example");
        let cat = SiteCatalog::with_custom([s.clone()]).unwrap();
        assert_eq!(cat.custom_sites().count(), 1);
        assert!(SiteCatalog::with_custom([s.clone(), s]).is_err());

        let root = Url::parse("https://api.example.com").unwrap();
        let s = new_compatible_site(SiteId::new("root"), "Root", &root).unwrap();
        assert_eq!(s.endpoints.models_unauth, "/models");

        for bad in [
            "http://api.example.com/v1",
            "https://u:p@api.example.com/v1",
            "https://api.example.com/v1?x=1",
        ] {
            let u = Url::parse(bad).unwrap();
            assert!(
                new_compatible_site(SiteId::new("x"), "X", &u).is_err(),
                "{bad}"
            );
        }
        let ok = Url::parse("https://api.example.com/v1").unwrap();
        assert!(new_compatible_site(SiteId::new("Bad Id"), "X", &ok).is_err());
        assert!(new_compatible_site(SiteId::new("krill"), "X", &ok).is_err());
        assert!(new_compatible_site(SiteId::new("x"), "  ", &ok).is_err());
    }

    #[test]
    fn custom_sites_cannot_pose_as_krill_or_reuse_builtin_ids() {
        let mut k = catalog::krill_site();
        k.id = SiteId::new("my-krill");
        assert!(validate_custom_site(&k).is_err(), "kind Krill is reserved");
        let mut c = new_compatible_site(
            SiteId::new("c"),
            "C",
            &Url::parse("https://c.example/v1").unwrap(),
        )
        .unwrap();
        c.capabilities.auth_modes = vec![AuthMode::Pat];
        assert!(validate_custom_site(&c).is_err());
    }

    #[test]
    fn site_rules_catch_broken_definitions() {
        let mut s = catalog::krill_site();
        s.lines.push(s.lines[0].clone());
        assert!(validate_site(&s).is_err(), "duplicate line id");

        let mut s = catalog::krill_site();
        let mut l = s.lines[0].clone();
        l.id = LineId::new("dup-origin");
        s.lines.push(l);
        assert!(validate_site(&s).is_err(), "duplicate origin");

        let mut s = catalog::krill_site();
        s.default_line = Some(LineId::new("missing"));
        assert!(validate_site(&s).is_err());

        let mut s = catalog::krill_site();
        s.endpoints.whoami = "https://evil.example/whoami".into();
        assert!(validate_site(&s).is_err(), "absolute endpoint path");

        let mut s = catalog::krill_site();
        s.endpoints.whoami = "/v1/../x".into();
        assert!(validate_site(&s).is_err());

        let mut s = catalog::krill_site();
        s.capabilities.codex_paths = vec![CodexPath::CodexV1];
        assert!(validate_site(&s).is_err(), "PAT needs backend-api");

        let mut s = catalog::krill_site();
        s.lines.clear();
        assert!(validate_site(&s).is_err());

        let mut s = catalog::krill_site();
        s.lines[0].source = LineSource::Test;
        assert!(validate_site(&s).is_err(), "test lines are loopback only");
    }

    #[test]
    fn model_maps_hold_plausible_names() {
        let mut s = new_compatible_site(
            SiteId::new("c"),
            "C",
            &Url::parse("https://c.example/v1").unwrap(),
        )
        .unwrap();
        s.model_map
            .insert("gpt-5.5".into(), "vendor/gpt-5.5-turbo".into());
        validate_custom_site(&s).unwrap();
        assert_eq!(s.site_model("gpt-5.5"), "vendor/gpt-5.5-turbo");
        assert_eq!(s.site_model("other"), "other");
        for (codex, mapped) in [
            ("a", ""),
            ("", "b"),
            ("a b", "c"),
            ("a", "a"),
            ("x", "q\"z"),
        ] {
            let mut bad = s.clone();
            bad.model_map.insert(codex.into(), mapped.into());
            assert!(
                validate_custom_site(&bad).is_err(),
                "{codex:?} -> {mapped:?}"
            );
        }
        assert!(valid_model_name(&"m".repeat(128)));
        assert!(!valid_model_name(&"m".repeat(129)));
        let mut many = s.clone();
        for i in 0..65 {
            many.model_map.insert(format!("m{i}"), format!("n{i}"));
        }
        assert!(validate_custom_site(&many).is_err());
    }

    #[test]
    fn account_endpoints_are_paths_on_the_line() {
        let mut s = catalog::krill_site();
        validate_site(&s).unwrap();
        s.endpoints.costs = "https://evil.example/v1/organization/costs".into();
        assert!(validate_site(&s).is_err(), "absolute URL");
        let mut s = catalog::krill_site();
        s.endpoints.billing_usage = "//evil.example/usage".into();
        assert!(validate_site(&s).is_err(), "scheme-relative URL");
        let mut s = catalog::krill_site();
        s.endpoints.billing_subscription = "/v1/dashboard/../x".into();
        assert!(validate_site(&s).is_err());
    }

    #[test]
    fn test_lines_come_from_the_environment_and_must_be_loopback() {
        let lines = parse_test_lines("a=http://127.0.0.1:18001, b=http://localhost:18002").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].source, LineSource::Test);
        assert!(parse_test_lines("a=https://api.example.com").is_err());
        assert!(parse_test_lines("a").is_err());
        assert!(parse_test_lines(" , ").is_err());
        assert!(parse_test_lines("A=http://127.0.0.1:1").is_err());

        let env = MapEnv::new().with(ENV_SUPERBA_TEST_LINES, "line-a=http://127.0.0.1:18001");
        let cat = SiteCatalog::from_env(&env, []).unwrap();
        let (_, line) = cat
            .resolve(&SiteId::new("krill"), Some(&LineId::new("line-a")))
            .unwrap();
        assert_eq!(line.origin.as_str(), "http://127.0.0.1:18001");
        let env = MapEnv::new().with(ENV_SUPERBA_TEST_LINES, "overseas=http://127.0.0.1:1");
        assert!(SiteCatalog::from_env(&env, []).is_err(), "id clash");
        let cat = SiteCatalog::from_env(&MapEnv::new(), []).unwrap();
        assert_eq!(cat, SiteCatalog::builtin());
    }
}
