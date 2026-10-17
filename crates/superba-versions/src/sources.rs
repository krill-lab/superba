//! Where Codex releases and packages come from, and the endpoints (overridable
//! for tests and the e2e harness).

use std::collections::BTreeMap;

use serde::Deserialize;
use superba_core::{CodexRelease, CodexVersion, DownloadSource, NetworkSettings};

use crate::http::Checksum;

/// Replaces the GitHub release download base
/// (`https://github.com/openai/codex/releases/download`) and makes it the only
/// package source (e2e: a local package cache serving `rust-v<ver>/<file>` and
/// `codex-package_SHA256SUMS`). Every package is still checksum-verified, but
/// against that server's SHA256SUMS, so only an https URL (or http on a
/// loopback address) is accepted ([`override_base`]) and a warning is logged
/// while it is active.
pub const ENV_SUPERBA_CODEX_PACKAGES: &str = "SUPERBA_CODEX_PACKAGES";

/// Replaces the desktop app download base
/// (`https://persistent.oaistatic.com/codex-app-prod`): APT/RPM indexes, Windows
/// JSON and packages (e2e: a local fixture server). Accepted and logged like
/// [`ENV_SUPERBA_CODEX_PACKAGES`].
pub const ENV_SUPERBA_APP_DOWNLOADS: &str = "SUPERBA_APP_DOWNLOADS";

/// Base URLs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// GitHub release asset base: `<base>/rust-v<ver>/<file>`.
    pub codex_releases: String,
    /// GitHub `releases/latest` page (answers with a redirect to the tag).
    pub codex_latest: String,
    /// npm registry.
    pub npm: String,
    /// npmmirror registry.
    pub npmmirror: String,
    /// Third-party GitHub proxies (prefixes of a GitHub URL), used only when
    /// [`NetworkSettings::third_party_proxies`] is on.
    pub proxies: Vec<String>,
    /// Only [`Endpoints::codex_releases`] serves packages (an override is set).
    pub packages_only_from_releases: bool,
    /// Desktop app downloads (`.../linux/deb`, `windows-release.json`, ...).
    pub app: String,
    /// Sparkle appcast (macOS).
    pub appcast: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            codex_releases: "https://github.com/openai/codex/releases/download".into(),
            codex_latest: "https://github.com/openai/codex/releases/latest".into(),
            npm: "https://registry.npmjs.org".into(),
            npmmirror: "https://registry.npmmirror.com".into(),
            proxies: vec!["https://gh-proxy.com/".into(), "https://ghfast.top/".into()],
            packages_only_from_releases: false,
            app: "https://persistent.oaistatic.com/codex-app-prod".into(),
            appcast: "https://updates.oaistatic.com/codex/app/appcast".into(),
        }
    }
}

/// The base URL of an endpoint override without a trailing `/`, when it is
/// acceptable: https, or http on a loopback address, without credentials,
/// query or fragment (the rule of the feed mirrors,
/// `superba_catalog::feeds::Mirror::new`).
pub fn override_base(value: &str) -> Option<String> {
    let base = value.trim().trim_end_matches('/');
    let url = url::Url::parse(base).ok()?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d == "localhost",
        None => false,
    };
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    (scheme_ok
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| base.to_owned())
}

/// The accepted override in `var`, if any; logs a warning once per process
/// while it is active, and when it is ignored.
fn checked_override(lookup: &impl Fn(&str) -> Option<String>, var: &'static str) -> Option<String> {
    static LOGGED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let raw = lookup(var).filter(|v| !v.trim().is_empty())?;
    let base = override_base(&raw);
    let first = {
        let mut logged = LOGGED.lock().unwrap_or_else(|p| p.into_inner());
        let first = !logged.contains(&var);
        if first {
            logged.push(var);
        }
        first
    };
    if first {
        match &base {
            Some(b) => tracing::warn!(
                var,
                base = %b,
                "endpoint override active: downloads and their checksums come from this server"
            ),
            None => tracing::warn!(
                var,
                "endpoint override ignored: not an https URL (or http on a loopback address) \
                 without credentials, query or fragment"
            ),
        }
    }
    base
}

impl Endpoints {
    /// Defaults with the [`ENV_SUPERBA_CODEX_PACKAGES`] /
    /// [`ENV_SUPERBA_APP_DOWNLOADS`] overrides applied (`lookup` reads a
    /// variable). A value that [`override_base`] refuses is ignored.
    pub fn with_overrides(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut e = Self::default();
        if let Some(base) = checked_override(&lookup, ENV_SUPERBA_CODEX_PACKAGES) {
            e.codex_releases = base;
            e.packages_only_from_releases = true;
        }
        if let Some(base) = checked_override(&lookup, ENV_SUPERBA_APP_DOWNLOADS) {
            e.app = base;
        }
        e
    }

    /// `<codex_releases>/rust-v<ver>/<file>`.
    pub fn release_asset(&self, version: &CodexVersion, file: &str) -> String {
        format!(
            "{}/rust-v{version}/{file}",
            self.codex_releases.trim_end_matches('/')
        )
    }
}

/// npm platform suffix of a package target (`linux-x64`).
pub fn npm_platform(target: &str) -> Option<&'static str> {
    Some(match target {
        "x86_64-unknown-linux-musl" => "linux-x64",
        "aarch64-unknown-linux-musl" => "linux-arm64",
        "x86_64-apple-darwin" => "darwin-x64",
        "aarch64-apple-darwin" => "darwin-arm64",
        "x86_64-pc-windows-msvc" => "win32-x64",
        "aarch64-pc-windows-msvc" => "win32-arm64",
        _ => return None,
    })
}

/// Every package target Superba knows.
pub const TARGETS: [&str; 6] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// `codex-package-<target>.tar.gz`.
pub fn package_file(target: &str) -> String {
    format!("codex-package-{target}.tar.gz")
}

/// Archive layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// `bin/codex` at the root (GitHub `codex-package-*`).
    Package,
    /// npm platform tarball: the package under `package/vendor/<target>/`.
    Npm,
}

/// One place to download a package from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// `github`, `npm`, `npmmirror`, `proxy:<host>`.
    pub source: String,
    /// URL.
    pub url: String,
    /// Local file name of the verified archive.
    pub file_name: String,
    /// Layout.
    pub layout: Layout,
    /// Checksum to verify against. npm candidates get theirs just before the
    /// download (`dist.integrity` from registry.npmjs.org, for npmjs and
    /// npmmirror alike); a candidate still without one is skipped.
    pub checksum: Option<Checksum>,
}

/// Parse `codex-package_SHA256SUMS` (`<hex>  <file>` lines).
pub fn parse_sha256sums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let sum = it.next()?;
            let file = it.next()?.trim_start_matches('*');
            (sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| (file.to_owned(), sum.to_ascii_lowercase()))
        })
        .collect()
}

/// `dist` of an npm version document.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NpmDist {
    /// Tarball URL.
    #[serde(default)]
    pub tarball: Option<String>,
    /// `sha512-<base64>`.
    #[serde(default)]
    pub integrity: Option<String>,
    /// Unpacked size.
    #[serde(default, rename = "unpackedSize")]
    pub unpacked_size: Option<u64>,
}

/// Parse an npm version document (`{"dist": {...}}`).
pub fn parse_npm_dist(json: &str) -> Option<NpmDist> {
    #[derive(Deserialize)]
    struct Doc {
        dist: NpmDist,
    }
    serde_json::from_str::<Doc>(json).ok().map(|d| d.dist)
}

/// The order of package sources for `network`.
pub fn source_order(network: &NetworkSettings, endpoints: &Endpoints) -> Vec<&'static str> {
    if endpoints.packages_only_from_releases {
        return vec!["github"];
    }
    let mut v = match network.download_source {
        DownloadSource::GitHub => vec!["github"],
        DownloadSource::Npm => vec!["npm"],
        DownloadSource::NpmMirror => vec!["npmmirror"],
        _ => vec!["github", "npm", "npmmirror"],
    };
    if network.third_party_proxies
        && !matches!(
            network.download_source,
            DownloadSource::Npm | DownloadSource::NpmMirror
        )
    {
        v.push("proxies");
    }
    v
}

/// Release versions from an npm dist-tags document: version-like values
/// without a platform suffix, at least 0.100.0, newest first.
pub fn parse_dist_tags(json: &str) -> Vec<CodexRelease> {
    let Ok(map) = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(json) else {
        return Vec::new();
    };
    let mut out: Vec<CodexVersion> = map
        .values()
        .filter_map(|v| v.as_str())
        .filter(|v| !["linux", "darwin", "win32"].iter().any(|p| v.contains(p)))
        .filter_map(|v| CodexVersion::parse(v).ok())
        .filter(|v| v.meets(&CodexVersion::new(0, 100, 0)))
        .collect();
    out.sort();
    out.dedup();
    out.into_iter()
        .rev()
        .map(|version| release_of(version, "npm dist-tags"))
        .collect()
}

/// The version of a GitHub release tag URL (`.../tag/rust-v0.163.0`).
pub fn parse_latest_redirect(location: &str) -> Option<CodexVersion> {
    let tag = location.trim_end_matches('/').rsplit('/').next()?;
    CodexVersion::parse(tag.strip_prefix("rust-v")?).ok()
}

/// A bare release (no checksums or dates).
pub fn release_of(version: CodexVersion, _origin: &str) -> CodexRelease {
    CodexRelease {
        tag: format!("rust-v{version}"),
        prerelease: version.pre.is_some(),
        version,
        date: None,
        bundled_by: Vec::new(),
        sha256: BTreeMap::new(),
    }
}

/// Union of release lists by version: the first list's entry wins, later lists
/// add missing versions and fill empty checksum maps / dates. Newest first.
pub fn merge_releases(lists: &[Vec<CodexRelease>]) -> Vec<CodexRelease> {
    let mut by: BTreeMap<CodexVersion, CodexRelease> = BTreeMap::new();
    for list in lists {
        for r in list {
            match by.get_mut(&r.version) {
                None => {
                    by.insert(r.version.clone(), r.clone());
                }
                Some(have) => {
                    if have.sha256.is_empty() {
                        have.sha256 = r.sha256.clone();
                    }
                    if have.date.is_none() {
                        have.date = r.date;
                    }
                    for b in &r.bundled_by {
                        if !have.bundled_by.contains(b) {
                            have.bundled_by.push(b.clone());
                        }
                    }
                }
            }
        }
    }
    by.into_values().rev().collect()
}
