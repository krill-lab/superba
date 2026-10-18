//! Who changed Codex's configuration (GH#5): cc-switch, AI Toolbox, Codex
//! itself or a hand edit, from the live `config.toml` / `auth.json` and the
//! other tools' own stores (read-only, like the import).
//!
//! Each tool is scored on two kinds of evidence:
//!
//! - **its store**: the Codex provider it records as current (cc-switch's
//!   `settings.json` / `is_current`, AI Toolbox's `is_applied`) uses the address
//!   or the key Codex is configured with now (strong), or another of its
//!   providers does (weaker);
//! - **its markers** in the files: AI Toolbox starts `config.toml` with
//!   `#:schema none` and names `ai-toolbox-codex-model-catalog.json`; cc-switch
//!   writes the key as `experimental_bearer_token` and deletes `auth.json`,
//!   names `cc-switch-model-catalog.json` and keeps a first-write backup of the
//!   file under `~/.cc-switch/backups/live-first-write/`; both have local
//!   gateway addresses and placeholder keys.
//!
//! Without a tool the change is put down to what changed: a ChatGPT login in
//! `auth.json` (Codex's sign-in), only keys the Codex app writes itself (its
//! model picker), or another program / a hand edit.

use std::path::Path;

use serde_json::Value;
use superba_core::{ChangeSource, ForeignApp, ImportSource, LocalizedText};
use url::Url;

use crate::codex;
use crate::foreign::SourceRead;
use crate::{ScanEnv, read_source};

/// Keys the Codex app writes itself (model picker, reasoning effort, speed).
pub const CODEX_APP_KEYS: &[&str] = &[
    "model",
    "model_reasoning_effort",
    "plan_mode_reasoning_effort",
    "service_tier",
];

/// cc-switch's model catalog file name (`codex_config.rs`).
const CCS_CATALOG: &str = "cc-switch-model-catalog.json";
/// AI Toolbox's model catalog file name (`coding/codex/commands.rs`).
const AITB_CATALOG: &str = "ai-toolbox-codex-model-catalog.json";
/// AI Toolbox's gateway placeholder key.
const AITB_GATEWAY_KEY: &str = "ai-toolbox-gateway";
/// cc-switch's proxy placeholder key.
const CCS_PROXY_KEY: &str = "PROXY_MANAGED";
/// cc-switch's default local proxy port.
const CCS_PROXY_PORT: u16 = 15721;

/// The live files of the Codex home.
#[derive(Clone, Copy, Debug)]
pub struct LiveFiles<'a> {
    /// Codex home.
    pub codex_home: &'a Path,
    /// `config.toml` (`None` = absent).
    pub config_toml: Option<&'a [u8]>,
    /// `auth.json` (`None` = absent).
    pub auth_json: Option<&'a [u8]>,
}

/// What changed (the owned paths that differ, whether the credential did).
#[derive(Clone, Copy, Debug, Default)]
pub struct Changed<'a> {
    /// Owned `config.toml` key paths that changed.
    pub keys: &'a [String],
    /// The `auth.json` credential changed.
    pub auth: bool,
}

/// The verdict and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attribution {
    /// Who most likely made the change.
    pub source: ChangeSource,
    /// Why.
    pub evidence: Vec<LocalizedText>,
}

/// Attribute a change, reading cc-switch's and AI Toolbox's stores (blocking
/// file I/O; databases are opened read-only or copied first).
pub fn attribute(live: &LiveFiles<'_>, changed: &Changed<'_>, env: &ScanEnv<'_>) -> Attribution {
    let reads = [
        read_source(ImportSource::Ccs, env),
        read_source(ImportSource::AiToolbox, env),
    ];
    attribute_with(live, changed, &reads, env.home)
}

/// [`attribute`] with the stores already read (`home` is where cc-switch keeps
/// its first-write backups).
pub fn attribute_with(
    live: &LiveFiles<'_>,
    changed: &Changed<'_>,
    reads: &[SourceRead],
    home: &Path,
) -> Attribution {
    let facts = Facts::read(live);
    let mut best: Option<Candidate> = None;
    for tool in [Tool::Ccs, Tool::AiToolbox] {
        let mut c = Candidate {
            tool,
            ..Candidate::default()
        };
        c.markers(&facts, live, home);
        if let Some(read) = reads.iter().find(|r| r.source == tool.source()) {
            c.store(&facts, read);
        }
        if c.score > 0 && best.as_ref().is_none_or(|b| c.beats(b)) {
            best = Some(c);
        }
    }
    if let Some(c) = best {
        return Attribution {
            source: c.tool.change_source(c.provider),
            evidence: c.evidence,
        };
    }
    let ev = |key: &str, fallback: &str| {
        vec![LocalizedText::new(
            format!("ownership.evidence.{key}"),
            fallback,
        )]
    };
    if changed.auth && facts.login && changed.keys.is_empty() {
        return Attribution {
            source: ChangeSource::CodexLogin,
            evidence: ev("login", "auth.json now holds a ChatGPT login."),
        };
    }
    if !changed.auth
        && !changed.keys.is_empty()
        && changed
            .keys
            .iter()
            .all(|k| CODEX_APP_KEYS.contains(&k.as_str()))
    {
        return Attribution {
            source: ChangeSource::CodexApp,
            evidence: ev(
                "only_model_keys",
                "Only keys the Codex app writes itself changed.",
            ),
        };
    }
    Attribution::default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Tool {
    #[default]
    Ccs,
    AiToolbox,
}

impl Tool {
    fn source(self) -> ImportSource {
        match self {
            Tool::Ccs => ImportSource::Ccs,
            Tool::AiToolbox => ImportSource::AiToolbox,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tool::Ccs => "cc-switch",
            Tool::AiToolbox => "AI Toolbox",
        }
    }

    fn change_source(self, provider: Option<String>) -> ChangeSource {
        match self {
            Tool::Ccs => ChangeSource::CcSwitch { provider },
            Tool::AiToolbox => ChangeSource::AiToolbox { provider },
        }
    }
}

/// What the live files configure.
#[derive(Debug, Default)]
struct Facts {
    text: String,
    toml: codex::CodexToml,
    catalog: Option<String>,
    /// The address Codex sends requests to.
    base_url: Option<String>,
    /// The key Codex sends (TOML bearer token, else auth.json's API key).
    key: Option<String>,
    /// auth.json is a ChatGPT login.
    login: bool,
    /// The login's e-mail address (from its id token), lower case.
    login_email: Option<String>,
    auth_missing: bool,
}

impl Facts {
    fn read(live: &LiveFiles<'_>) -> Self {
        let text = live
            .config_toml
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let table = codex::parse_table(&text).unwrap_or_default();
        let toml = codex::read_toml(&table);
        let catalog = table
            .get("model_catalog_json")
            .and_then(toml::Value::as_str)
            .map(|s| s.trim().to_owned());
        let custom = toml.provider_id.as_deref().is_some_and(|p| p != "openai");
        let base_url = if custom {
            toml.active.as_ref().and_then(|t| t.base_url.clone())
        } else {
            toml.openai_base_url.clone()
        };
        let auth: Option<serde_json::Map<String, Value>> = live
            .auth_json
            .and_then(|b| serde_json::from_slice::<Value>(b).ok())
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            });
        let auth_key = auth
            .as_ref()
            .and_then(|m| m.get("OPENAI_API_KEY"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let login = auth
            .as_ref()
            .and_then(|m| m.get("tokens"))
            .is_some_and(Value::is_object);
        let key = toml.bearer_token().map(str::to_owned).or(auth_key);
        let login_email = auth
            .as_ref()
            .and_then(|m| m.get("tokens"))
            .and_then(|t| t.get("id_token"))
            .and_then(Value::as_str)
            .and_then(jwt_email);
        Self {
            text,
            catalog,
            base_url,
            key,
            login,
            login_email,
            auth_missing: live.auth_json.is_none(),
            toml,
        }
    }
}

#[derive(Debug, Default)]
struct Candidate {
    tool: Tool,
    score: u32,
    /// The best store match was the tool's current provider.
    current: bool,
    provider: Option<String>,
    evidence: Vec<LocalizedText>,
}

fn evidence(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(format!("ownership.evidence.{key}"), fallback)
}

impl Candidate {
    fn beats(&self, other: &Candidate) -> bool {
        (self.score, self.current) > (other.score, other.current)
    }

    fn add(&mut self, score: u32, text: LocalizedText) {
        self.score += score;
        self.evidence.push(text);
    }

    fn markers(&mut self, f: &Facts, live: &LiveFiles<'_>, home: &Path) {
        let tool = self.tool.name();
        let catalog_text = |file: &str| {
            evidence(
                "marker_catalog",
                "config.toml names {tool}'s model catalog file {file}.",
            )
            .arg("tool", tool)
            .arg("file", file)
        };
        let gateway_text = |url: &str| {
            evidence(
                "marker_gateway",
                "Codex points at {tool}'s local gateway ({url}).",
            )
            .arg("tool", tool)
            .arg("url", url)
        };
        match self.tool {
            Tool::AiToolbox => {
                if f.text
                    .trim_start_matches('\u{feff}')
                    .starts_with("#:schema none")
                {
                    self.add(
                        1,
                        evidence(
                            "marker_schema",
                            "config.toml starts with “#:schema none”, which AI Toolbox writes.",
                        ),
                    );
                }
                if f.catalog.as_deref() == Some(AITB_CATALOG) {
                    self.add(1, catalog_text(AITB_CATALOG));
                }
                let gateway = f.key.as_deref() == Some(AITB_GATEWAY_KEY)
                    || f.base_url.as_deref().is_some_and(|u| {
                        is_loopback(u) && u.trim_end_matches('/').ends_with("/openai/v1")
                    });
                if gateway {
                    self.add(2, gateway_text(f.base_url.as_deref().unwrap_or_default()));
                }
            }
            Tool::Ccs => {
                if f.catalog.as_deref() == Some(CCS_CATALOG) {
                    self.add(1, catalog_text(CCS_CATALOG));
                }
                let gateway = f.key.as_deref() == Some(CCS_PROXY_KEY)
                    || f.base_url.as_deref().is_some_and(|u| {
                        is_loopback(u)
                            && Url::parse(u).is_ok_and(|u| u.port() == Some(CCS_PROXY_PORT))
                    });
                if gateway {
                    self.add(2, gateway_text(f.base_url.as_deref().unwrap_or_default()));
                }
                let bearer = f
                    .toml
                    .custom_active()
                    .is_some_and(|t| t.bearer_token.is_some());
                if bearer && f.auth_missing {
                    self.add(
                        1,
                        evidence(
                            "marker_bearer",
                            "The provider table carries its key as experimental_bearer_token and auth.json was removed, as cc-switch does.",
                        ),
                    );
                }
                if let Some(path) = ccs_first_write_backup(home, live.codex_home) {
                    self.add(
                        1,
                        evidence(
                            "marker_backup",
                            "cc-switch keeps a first-write backup of this config.toml ({path}).",
                        )
                        .arg("path", path.display()),
                    );
                }
            }
        }
    }

    fn store(&mut self, f: &Facts, read: &SourceRead) {
        let tool = self.tool.name();
        let codex = read.providers.iter().filter(|p| p.app == ForeignApp::Codex);
        let mut found: Option<(u32, bool, String, LocalizedText)> = None;
        for p in codex {
            let name = p.name.clone();
            let mut hit: Option<(u32, LocalizedText)> = None;
            if p.official {
                let codex_official = f
                    .toml
                    .provider_id
                    .as_deref()
                    .is_none_or(|id| id == "openai");
                // An account named by its e-mail must be the signed-in one;
                // an official card without one ("OpenAI Official") is weak.
                let weight = if p.name.contains('@') {
                    let same = f
                        .login_email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(p.name.trim()));
                    if same { 3 } else { 0 }
                } else {
                    1
                };
                if p.current && f.login && codex_official && weight > 0 {
                    hit = Some((
                        weight,
                        evidence(
                            "store_official",
                            "{tool}'s current Codex provider “{provider}” is the official ChatGPT login.",
                        ),
                    ));
                }
            } else {
                let url = matches!(
                    (&p.base_url, &f.base_url),
                    (Some(a), Some(b)) if same_url(a, b)
                );
                let key = matches!(
                    (p.key.secret(), &f.key),
                    (Some(a), Some(b)) if a.expose() == b.as_str()
                );
                if url || key {
                    let weight = if p.current { 3 } else { 2 };
                    let text = match (p.current, url) {
                        (true, true) => evidence(
                            "store_current",
                            "{tool}'s current Codex provider “{provider}” uses this address.",
                        ),
                        (true, false) => evidence(
                            "store_current_key",
                            "{tool}'s current Codex provider “{provider}” uses this key.",
                        ),
                        (false, _) => evidence(
                            "store_other",
                            "{tool} has a Codex provider “{provider}” with this address.",
                        ),
                    };
                    hit = Some((weight + u32::from(url && key), text));
                }
            }
            if let Some((score, text)) = hit {
                let text = text.arg("tool", tool).arg("provider", &name);
                if found
                    .as_ref()
                    .is_none_or(|(s, c, _, _)| (score, p.current) > (*s, *c))
                {
                    found = Some((score, p.current, name, text));
                }
            }
        }
        if let Some((score, current, name, text)) = found {
            self.current = current;
            self.provider = Some(name);
            // The store says the most; it comes first.
            self.score += score;
            self.evidence.insert(0, text);
        }
    }
}

/// The `email` claim of an (unverified) id token, lower case.
fn jwt_email(jwt: &str) -> Option<String> {
    use base64::Engine as _;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("email")
        .or_else(|| claims.get("https://api.openai.com/profile")?.get("email"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
}

fn is_loopback(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    })
}

/// Two addresses name the same endpoint (scheme, host, port and path; a
/// trailing `/` and the letter case of the host do not matter).
fn same_url(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        Url::parse(s.trim()).ok().map(|u| {
            format!(
                "{}://{}:{}{}",
                u.scheme(),
                u.host_str().unwrap_or_default().to_ascii_lowercase(),
                u.port_or_known_default().unwrap_or_default(),
                u.path().trim_end_matches('/')
            )
        })
    };
    match (norm(a), norm(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/'),
    }
}

/// cc-switch's first-write backup of `<codex home>/config.toml`, when it has
/// one: `~/.cc-switch/backups/live-first-write/<hash>-config.toml` with a
/// `.source` file naming the path it came from.
fn ccs_first_write_backup(home: &Path, codex_home: &Path) -> Option<std::path::PathBuf> {
    let dir = home
        .join(".cc-switch")
        .join("backups")
        .join("live-first-write");
    let target = codex_home.join("config.toml");
    let canonical = std::fs::canonicalize(&target).ok();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("source") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let named = Path::new(source.trim());
        let same = named == target
            || canonical
                .as_deref()
                .is_some_and(|c| std::fs::canonicalize(named).ok().as_deref() == Some(c));
        if same {
            return Some(path.with_extension(""));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_compare_without_trailing_slash_and_host_case() {
        assert!(same_url(
            "https://API.example.com/v1/",
            "https://api.example.com/v1"
        ));
        assert!(same_url(
            "https://api.example.com:443/v1",
            "https://api.example.com/v1"
        ));
        assert!(!same_url(
            "https://api.example.com/v1",
            "https://api.example.com/v2"
        ));
        assert!(is_loopback("http://127.0.0.1:37123/openai/v1"));
        assert!(is_loopback("http://localhost:15721/v1"));
        assert!(!is_loopback("https://relay.example.com/v1"));
    }
}
