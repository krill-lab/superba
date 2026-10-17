//! # superba-versions
//!
//! [`HttpVersionStore`] implements [`superba_core::VersionStore`]:
//!
//! - **Codex releases**: the signed feed's list (when a [`FeedSource`] is
//!   attached), refreshed with npm dist-tags (npmjs, then npmmirror) or the
//!   GitHub `releases/latest` redirect; cached for a day in
//!   `<data dir>/codex/releases.json`; online checks follow
//!   [`NetworkSettings::online_checks`].
//! - **Packages** ([`sources`]): GitHub `codex-package-<target>.tar.gz`
//!   (sha256 from the feed, else `codex-package_SHA256SUMS` from GitHub), the npm
//!   platform tarball from npmjs or npmmirror (both verified against the
//!   `dist.integrity` sha512 of registry.npmjs.org: npmmirror's own metadata is
//!   never trusted, so npmmirror is skipped when npmjs does not answer), and
//!   third-party GitHub proxies only when enabled and only with a sha256 from a
//!   trusted source. Downloads resume into `.part` files and are verified
//!   **before** extraction; a source whose file fails verification is skipped.
//! - **Store** ([`store`]): safe extraction ([`extract`]) into
//!   `<data dir>/codex/<ver>-<target>/`, `bin/codex --version` must equal the
//!   requested version, retention keeps three plus the pinned / in-use ones; an
//!   inter-process lock serialises writers.
//! - **App builds** ([`app`]): APT `Packages` / RPM `primary.xml.gz` (Linux,
//!   sha256), Sparkle appcast (macOS), Windows Store JSON; verified Linux
//!   package downloads into `<data dir>/downloads/`. Never runs privileged
//!   commands.
//! - **Windows app package** ([`winapp`]): the MSIX from OpenAI's server or,
//!   as the alternative, from the Microsoft Store CDN through the
//!   store.rg-adguard.net resolver; resumable with progress and cancel, size /
//!   SHA-1 / manifest identity checked, framework dependencies that are not
//!   installed fetched too. Opening it (App Installer) is the caller's.
//! - **Local standalone installs** ([`standalone`]) for the CLI on `PATH`.
//!
//! Compatibility tiers: [`compat_tier`] ([`CompatTier::of`], spike B6).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod extract;
pub mod http;
pub mod selfupdate;
pub mod sources;
pub mod standalone;
pub mod store;
pub mod winapp;
mod xml;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use superba_core::{
    AppDownloadReport, AppOs, AppPackage, AppPackageKind, AppRelease, CodexRelease, CodexVersion,
    CompatTier, DataDir, DownloadProgressSink, Error, FeedSource, InstallReport, InstallTarget,
    NetworkSettings, Result, StandaloneLayout, StoredCodex, Timestamp, VersionStore,
    WindowsAppRequest, local_codex_package_target,
};

use crate::http::Checksum;
use crate::sources::{Candidate, Endpoints, Layout, package_file};
use crate::store::{StoreDirs, VERIFIED_MARKER, exe_name};

/// How long cached release lists and app builds are used without an online
/// check ("at most once a day").
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Deadline of one metadata request (index, dist-tags, SHA256SUMS).
pub const METADATA_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest Codex package accepted (0.162 linux-x64: 163 MB).
pub const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

/// Largest app package accepted (the 0.1013 deb is 502 MB, the msix ~950 MB).
pub const MAX_APP_PACKAGE_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// How `candidate` fits a desktop app that bundles `bundled` and refuses
/// app-servers below `floor` (spike B6; see [`CompatTier::of`]).
pub fn compat_tier(
    bundled: Option<&CodexVersion>,
    floor: &CodexVersion,
    candidate: &CodexVersion,
) -> CompatTier {
    CompatTier::of(bundled, floor, candidate)
}

/// The production [`VersionStore`].
#[derive(Clone)]
pub struct HttpVersionStore {
    data_dir: DataDir,
    network: NetworkSettings,
    endpoints: Endpoints,
    feed: Option<Arc<dyn FeedSource>>,
    target: Option<String>,
    os: AppOs,
    arch: String,
}

impl std::fmt::Debug for HttpVersionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpVersionStore")
            .field("data_dir", &self.data_dir)
            .field("network", &self.network)
            .field("endpoints", &self.endpoints)
            .field("feed", &self.feed.is_some())
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// Cached JSON with the time of the online check that produced it.
#[derive(Debug, Serialize, Deserialize)]
struct Cached<T> {
    checked_at: Timestamp,
    data: T,
}

fn read_cache<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<Cached<T>> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_cache<T: Serialize>(path: &Path, data: T) {
    let c = Cached {
        checked_at: Timestamp::now(),
        data,
    };
    if let (Some(dir), Ok(bytes)) = (path.parent(), serde_json::to_vec_pretty(&c))
        && std::fs::create_dir_all(dir).is_ok()
    {
        let tmp = path.with_extension(format!("tmp-{}", store::nonce()));
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn fresh(checked_at: Timestamp) -> bool {
    let age = Timestamp::now().as_millis() - checked_at.as_millis();
    age >= 0 && (age as u128) < CHECK_INTERVAL.as_millis()
}

fn this_os() -> AppOs {
    if cfg!(target_os = "macos") {
        AppOs::Macos
    } else if cfg!(windows) {
        AppOs::Windows
    } else {
        AppOs::Linux
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::other(format!("background task failed: {e}")))?
}

impl HttpVersionStore {
    /// A store under `data_dir` using `network` (download source, proxies,
    /// online checks). Endpoint overrides for tests and the e2e harness are read
    /// from [`sources::ENV_SUPERBA_CODEX_PACKAGES`] and
    /// [`sources::ENV_SUPERBA_APP_DOWNLOADS`].
    pub fn new(data_dir: &DataDir, network: NetworkSettings) -> Self {
        Self {
            data_dir: data_dir.clone(),
            network,
            endpoints: Endpoints::with_overrides(|k| std::env::var(k).ok()),
            feed: None,
            target: local_codex_package_target().map(str::to_owned),
            os: this_os(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }

    /// Use the signed feed for release lists, package checksums and the Codex
    /// bundled by each app build.
    pub fn with_feed(mut self, feed: Arc<dyn FeedSource>) -> Self {
        self.feed = Some(feed);
        self
    }

    /// Replace the endpoints (tests).
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Pretend to be another machine (tests): its package target, app OS and
    /// architecture (`std::env::consts::ARCH` style).
    pub fn with_platform(mut self, target: Option<&str>, os: AppOs, arch: &str) -> Self {
        self.target = target.map(str::to_owned);
        self.os = os;
        self.arch = arch.to_owned();
        self
    }

    /// The machine's native architecture for desktop app packages (Windows:
    /// `arm64` when this x64 build runs emulated on ARM64), instead of this
    /// process's.
    pub fn with_native_arch(mut self, arch: &str) -> Self {
        self.arch = arch.to_owned();
        self
    }

    /// `<data dir>/codex/`.
    pub fn store_dir(&self) -> PathBuf {
        self.data_dir.codex_store_dir()
    }

    /// `<data dir>/downloads/`.
    pub fn downloads_dir(&self) -> PathBuf {
        self.data_dir.downloads_dir()
    }

    /// The network settings in use.
    pub fn network(&self) -> &NetworkSettings {
        &self.network
    }

    /// The endpoints in use.
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    fn dirs(&self) -> StoreDirs {
        StoreDirs {
            root: self.store_dir(),
        }
    }

    fn local_target(&self) -> Result<String> {
        self.target.clone().ok_or_else(|| Error::Unsupported {
            what: "Codex packages for this platform".into(),
        })
    }

    async fn feed_releases(&self, refresh: bool) -> Vec<CodexRelease> {
        match &self.feed {
            Some(f) => f
                .snapshot(refresh && self.network.online_checks)
                .await
                .map(|s| s.codex_versions)
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    async fn feed_apps(&self) -> Vec<AppRelease> {
        match &self.feed {
            Some(f) => f
                .snapshot(false)
                .await
                .map(|s| s.app_versions)
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Releases from npm dist-tags (npmjs, then npmmirror), else the GitHub
    /// latest redirect.
    async fn online_releases(&self) -> Result<Vec<CodexRelease>> {
        let client = http::client(true)?;
        let mut last = None;
        for reg in [&self.endpoints.npm, &self.endpoints.npmmirror] {
            let url = format!(
                "{}/-/package/@openai/codex/dist-tags",
                reg.trim_end_matches('/')
            );
            match http::get_text(&client, &url, METADATA_TIMEOUT).await {
                Ok(text) => {
                    let list = sources::parse_dist_tags(&text);
                    if !list.is_empty() {
                        return Ok(list);
                    }
                    last = Some(Error::parse(&url, "no versions"));
                }
                Err(e) => last = Some(e),
            }
        }
        let client = http::client(false)?;
        match http::redirect_target(&client, &self.endpoints.codex_latest, METADATA_TIMEOUT).await {
            Ok(loc) => match sources::parse_latest_redirect(&loc) {
                Some(v) => Ok(vec![sources::release_of(v, "github")]),
                None => Err(Error::parse(
                    &self.endpoints.codex_latest,
                    "not a release tag",
                )),
            },
            Err(e) => Err(last.unwrap_or(e)),
        }
    }

    /// The trusted sha256 of `codex-package-<target>.tar.gz`: the feed's, else
    /// GitHub's `codex-package_SHA256SUMS` (fetched directly, never through a
    /// proxy).
    async fn trusted_sha256(&self, version: &CodexVersion, target: &str) -> Option<String> {
        let from_feed = self
            .feed_releases(false)
            .await
            .into_iter()
            .find(|r| &r.version == version)
            .and_then(|r| r.sha256.get(target).cloned());
        if from_feed.is_some() {
            return from_feed;
        }
        let client = http::client(true).ok()?;
        let url = self
            .endpoints
            .release_asset(version, "codex-package_SHA256SUMS");
        let text = http::get_text(&client, &url, METADATA_TIMEOUT).await.ok()?;
        sources::parse_sha256sums(&text).remove(&package_file(target))
    }

    /// Download candidates in the configured order.
    async fn candidates(&self, version: &CodexVersion, target: &str) -> Vec<Candidate> {
        let gh_url = self.endpoints.release_asset(version, &package_file(target));
        let package_name = store::archive_name(version, target, Layout::Package);
        let mut out = Vec::new();
        let mut sha: Option<Option<String>> = None;
        for source in sources::source_order(&self.network, &self.endpoints) {
            match source {
                "github" => {
                    let s = match &sha {
                        Some(s) => s.clone(),
                        None => {
                            let s = self.trusted_sha256(version, target).await;
                            sha = Some(s.clone());
                            s
                        }
                    };
                    out.push(Candidate {
                        source: "github".into(),
                        url: gh_url.clone(),
                        file_name: package_name.clone(),
                        layout: Layout::Package,
                        checksum: s.map(Checksum::Sha256),
                    });
                }
                "npm" | "npmmirror" => {
                    let Some(platform) = sources::npm_platform(target) else {
                        continue;
                    };
                    let reg = if source == "npm" {
                        &self.endpoints.npm
                    } else {
                        &self.endpoints.npmmirror
                    };
                    let reg = reg.trim_end_matches('/');
                    out.push(Candidate {
                        source: source.into(),
                        url: format!("{reg}/@openai/codex/-/codex-{version}-{platform}.tgz"),
                        file_name: store::archive_name(version, target, Layout::Npm),
                        layout: Layout::Npm,
                        checksum: None,
                    });
                }
                "proxies" => {
                    let s = match &sha {
                        Some(s) => s.clone(),
                        None => {
                            let s = self.trusted_sha256(version, target).await;
                            sha = Some(s.clone());
                            s
                        }
                    };
                    // A proxy is used only with a checksum from a trusted source.
                    let Some(s) = s else { continue };
                    for p in &self.endpoints.proxies {
                        out.push(Candidate {
                            source: format!("proxy:{}", host_of(p)),
                            url: format!("{p}{gh_url}"),
                            file_name: package_name.clone(),
                            layout: Layout::Package,
                            checksum: Some(Checksum::Sha256(s.clone())),
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The checksum of the npm platform tarball: `dist.integrity` of the version
    /// document on registry.npmjs.org, for npmjs and npmmirror alike (a mirror
    /// never vouches for its own files).
    async fn npm_integrity(&self, version: &CodexVersion, target: &str) -> Option<Checksum> {
        let platform = sources::npm_platform(target)?;
        let reg = self.endpoints.npm.trim_end_matches('/');
        let client = http::client(true).ok()?;
        let url = format!("{reg}/@openai/codex/{version}-{platform}");
        let text = http::get_text(&client, &url, METADATA_TIMEOUT).await.ok()?;
        let dist = sources::parse_npm_dist(&text)?;
        dist.integrity
            .filter(|i| i.contains("sha512-"))
            .map(Checksum::Sha512Integrity)
    }

    /// A verified archive of `version` for `target` (downloaded when missing).
    async fn verified_archive(
        &self,
        version: &CodexVersion,
        target: &str,
    ) -> Result<(PathBuf, Layout)> {
        let dirs = self.dirs();
        for layout in [Layout::Package, Layout::Npm] {
            let p = dirs.archive(version, target, layout);
            if p.is_file() && sidecar_matches(&p) {
                return Ok((p, layout));
            }
        }
        let client = http::client(true)?;
        let mut errors: Vec<String> = Vec::new();
        let mut npm_sum: Option<Option<Checksum>> = None;
        for mut c in self.candidates(version, target).await {
            if c.checksum.is_none() && c.layout == Layout::Npm {
                if npm_sum.is_none() {
                    npm_sum = Some(self.npm_integrity(version, target).await);
                }
                c.checksum = npm_sum.clone().flatten();
            }
            let Some(sum) = c.checksum.clone() else {
                errors.push(if c.source == "npmmirror" {
                    "npmmirror: no trusted checksum (registry.npmjs.org did not answer)".to_owned()
                } else {
                    format!("{}: no published checksum", c.source)
                });
                continue;
            };
            let dest = dirs.packages().join(&c.file_name);
            tracing::info!(source = %c.source, url = %c.url, %version, target, "downloading Codex");
            match http::download(&client, &c.url, &dest, &sum, None, MAX_PACKAGE_BYTES).await {
                Ok(sha) => {
                    let side = sidecar(&dest);
                    std::fs::write(&side, format!("{sha}\n")).map_err(|e| Error::io(&side, &e))?;
                    return Ok((dest, c.layout));
                }
                Err(e) => {
                    tracing::warn!(source = %c.source, error = %e, "Codex download failed");
                    errors.push(format!("{}: {}", c.source, e.detail()));
                }
            }
        }
        Err(Error::NotFound {
            what: format!(
                "a verified Codex {version} package for {target} ({})",
                if errors.is_empty() {
                    "no source configured".to_owned()
                } else {
                    errors.join("; ")
                }
            ),
        })
    }

    async fn ensure_target(&self, version: &CodexVersion, target: &str) -> Result<StoredCodex> {
        let dirs = self.dirs();
        let d2 = dirs.clone();
        let _lock = blocking(move || store::lock(&d2)).await?;
        if let Some(s) = store::list(&dirs, Some(target))
            .into_iter()
            .find(|s| &s.version == version)
        {
            return Ok(s);
        }
        let (archive, layout) = self.verified_archive(version, target).await?;
        let (d3, v, t) = (dirs.clone(), version.clone(), target.to_owned());
        blocking(move || unpack(&d3, &archive, layout, &v, &t)).await?;
        store::list(&dirs, Some(target))
            .into_iter()
            .find(|s| &s.version == version)
            .ok_or_else(|| Error::other("the stored Codex disappeared"))
    }
}

pub(crate) fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn sidecar(archive: &Path) -> PathBuf {
    let mut s = archive.as_os_str().to_owned();
    s.push(".sha256");
    PathBuf::from(s)
}

/// The archive still hashes to what was recorded when it was verified.
fn sidecar_matches(archive: &Path) -> bool {
    let Ok(want) = std::fs::read_to_string(sidecar(archive)) else {
        return false;
    };
    http::verify_file(archive, &Checksum::Sha256(want.trim().to_owned()), None).is_ok()
}

/// Extract a verified archive into the store and check its `--version`.
fn unpack(
    dirs: &StoreDirs,
    archive: &Path,
    layout: Layout,
    version: &CodexVersion,
    target: &str,
) -> Result<()> {
    let tmp = store::temp_dir(dirs, &format!("{version}-{target}"));
    let result = (|| {
        extract::extract(archive, layout, target, &tmp)?;
        let exe = tmp.join("bin").join(exe_name(target));
        if !exe.is_file() {
            return Err(Error::Invalid {
                message: format!("{} holds no bin/{}", archive.display(), exe_name(target)),
            });
        }
        if Some(target) == local_codex_package_target() {
            let got = standalone::version_of(&exe);
            if got.as_ref() != Some(version) {
                return Err(Error::Invalid {
                    message: format!(
                        "the packaged codex reports {}, not {version}",
                        got.map_or_else(|| "nothing".to_owned(), |v| v.to_string())
                    ),
                });
            }
        }
        std::fs::write(tmp.join(VERIFIED_MARKER), format!("{version}\n"))
            .map_err(|e| Error::io(&tmp, &e))?;
        let dest = dirs.entry(version, target);
        if dest.exists() {
            store::remove_dir(&dest)?;
        }
        std::fs::rename(&tmp, &dest).map_err(|e| Error::io(&dest, &e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

impl HttpVersionStore {
    async fn app_latest_online(&self) -> Result<Option<AppRelease>> {
        let client = http::client(true)?;
        let base = self.endpoints.app.trim_end_matches('/').to_owned();
        match self.os {
            AppOs::Linux => {
                let mut found: Vec<(String, AppPackage)> = Vec::new();
                let mut errors = Vec::new();
                if let Some(a) = app::deb_arch(&self.arch) {
                    let url = format!("{base}/linux/deb/dists/stable/main/binary-{a}/Packages");
                    match http::get_text(&client, &url, METADATA_TIMEOUT).await {
                        Ok(t) => {
                            found.extend(app::parse_packages(&t, &format!("{base}/linux/deb"), a))
                        }
                        Err(e) => errors.push(e),
                    }
                }
                if let Some(a) = app::rpm_arch(&self.arch) {
                    let root = format!("{base}/linux/rpm/{a}");
                    match self.rpm_latest(&client, &root).await {
                        Ok(p) => found.extend(p),
                        Err(e) => errors.push(e),
                    }
                }
                let Some(version) = found
                    .iter()
                    .map(|(v, _)| v.clone())
                    .max_by(|a, b| app::app_version_cmp(a, b))
                else {
                    return match errors.into_iter().next() {
                        Some(e) => Err(e),
                        None => Ok(None),
                    };
                };
                let packages = found
                    .into_iter()
                    .filter(|(v, _)| v == &version)
                    .map(|(_, p)| p)
                    .collect();
                Ok(Some(AppRelease {
                    version,
                    date: None,
                    bundled_codex: None,
                    packages,
                    store_product_id: None,
                }))
            }
            AppOs::Macos => {
                let arch = app::mac_arch(&self.arch).unwrap_or("arm64");
                let url = format!(
                    "{}?installation_id={}&arch={arch}&app_version=0.0.0&beta=false",
                    self.endpoints.appcast,
                    app::random_installation_id()
                );
                let text = http::get_text(&client, &url, METADATA_TIMEOUT).await?;
                app::parse_appcast(&text, arch)
            }
            _ => {
                let store = http::get_text(
                    &client,
                    &format!("{base}/windows-store-update.json"),
                    METADATA_TIMEOUT,
                )
                .await?;
                let release = http::get_text(
                    &client,
                    &format!("{base}/windows-release.json"),
                    METADATA_TIMEOUT,
                )
                .await
                .ok();
                let mut r = app::parse_windows(&store, release.as_deref())?;
                r.packages.retain(|p| self.official_app_url(&p.url));
                // The size of this machine's package (for the download button):
                // windows-release.json does not list it.
                let url =
                    winapp::windows_package_for(&r.packages, &self.arch).map(|p| p.url.clone());
                if let Some(url) = url {
                    let size = http::content_length(&client, &url, METADATA_TIMEOUT).await;
                    for p in r.packages.iter_mut().filter(|p| p.url == url) {
                        p.size = size;
                    }
                }
                Ok(Some(r))
            }
        }
    }

    async fn rpm_latest(
        &self,
        client: &reqwest::Client,
        root: &str,
    ) -> Result<Option<(String, AppPackage)>> {
        let repomd = http::get_text(
            client,
            &format!("{root}/repodata/repomd.xml"),
            METADATA_TIMEOUT,
        )
        .await?;
        let (href, sha) = app::parse_repomd(&repomd)?;
        let url = format!("{root}/{href}");
        let gz = http::get_bytes(client, &url, METADATA_TIMEOUT).await?;
        if let Some(want) = sha {
            use sha2::Digest;
            if hex::encode(sha2::Sha256::digest(&gz)) != want {
                return Err(Error::Invalid {
                    message: format!("{url} does not match the checksum in repomd.xml"),
                });
            }
        }
        let xml = app::gunzip(&gz, http::MAX_TEXT_BYTES)?;
        app::parse_primary(&xml, root)
    }

    fn app_cache(&self) -> PathBuf {
        self.downloads_dir().join("app-latest.json")
    }

    /// `url` is under the official desktop app download base
    /// (`https://persistent.oaistatic.com/codex-app-prod/`) or the active
    /// [`sources::ENV_SUPERBA_APP_DOWNLOADS`] override.
    fn official_app_url(&self, url: &str) -> bool {
        app::under_base(&Endpoints::default().app, url) || app::under_base(&self.endpoints.app, url)
    }

    /// The feed's newest build for this OS. Its packages are kept only when
    /// they are official downloads: the feed's signature covers the URL, but
    /// whoever holds a signing key must not point Superba at another host.
    async fn feed_app_latest(&self) -> Option<AppRelease> {
        let mut apps: Vec<AppRelease> = self
            .feed_apps()
            .await
            .into_iter()
            .filter(|a| a.packages.is_empty() || a.packages.iter().any(|p| p.os == self.os))
            .collect();
        apps.sort_by(|a, b| app::app_version_cmp(&b.version, &a.version));
        apps.into_iter().next().map(|mut a| {
            a.packages.retain(|p| {
                let official = self.official_app_url(&p.url);
                if p.os == self.os && !official {
                    tracing::warn!(url = %p.url, "feed app package outside the official download base dropped");
                }
                p.os == self.os && official
            });
            a
        })
    }

    /// Fill what the vendor index lacks from the feed (bundled Codex, date,
    /// checksums of the same files).
    async fn enrich(&self, mut r: AppRelease) -> AppRelease {
        if let Some(f) = self
            .feed_apps()
            .await
            .into_iter()
            .find(|a| a.version == r.version)
        {
            if r.bundled_codex.is_none() {
                r.bundled_codex = f.bundled_codex.clone();
            }
            if r.date.is_none() {
                r.date = f.date;
            }
            for p in &mut r.packages {
                if p.sha256.is_none()
                    && let Some(fp) = f.packages.iter().find(|fp| fp.url == p.url)
                {
                    p.sha256 = fp.sha256.clone();
                }
            }
        }
        r
    }
}

#[async_trait]
impl VersionStore for HttpVersionStore {
    async fn codex_releases(&self, refresh: bool) -> Result<Vec<CodexRelease>> {
        let feed = self.feed_releases(refresh).await;
        let cache_path = self.dirs().releases_cache();
        let cached: Option<Cached<Vec<CodexRelease>>> = read_cache(&cache_path);
        let use_cache = cached
            .as_ref()
            .is_some_and(|c| !refresh && fresh(c.checked_at));
        let online = if self.network.online_checks && !use_cache {
            match self.online_releases().await {
                Ok(list) => {
                    write_cache(&cache_path, &list);
                    Some(list)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Codex release check failed");
                    if feed.is_empty() && cached.is_none() {
                        return Err(e);
                    }
                    None
                }
            }
        } else {
            None
        };
        let cached = cached.map(|c| c.data).unwrap_or_default();
        Ok(sources::merge_releases(&[
            feed,
            online.unwrap_or_default(),
            cached,
        ]))
    }

    async fn app_latest(&self, refresh: bool) -> Result<Option<AppRelease>> {
        let cache_path = self.app_cache();
        let cached: Option<Cached<Option<AppRelease>>> = read_cache(&cache_path);
        if let Some(c) = cached.as_ref().filter(|c| !refresh && fresh(c.checked_at)) {
            return Ok(match c.data.clone() {
                Some(r) => Some(self.enrich(r).await),
                None => self.feed_app_latest().await,
            });
        }
        if !self.network.online_checks {
            return Ok(match cached.and_then(|c| c.data) {
                Some(r) => Some(self.enrich(r).await),
                None => self.feed_app_latest().await,
            });
        }
        match self.app_latest_online().await {
            Ok(r) => {
                write_cache(&cache_path, &r);
                Ok(match r {
                    Some(r) => Some(self.enrich(r).await),
                    None => self.feed_app_latest().await,
                })
            }
            Err(e) => {
                tracing::warn!(error = %e, "desktop app update check failed");
                match cached.and_then(|c| c.data) {
                    Some(r) => Ok(Some(self.enrich(r).await)),
                    None => match self.feed_app_latest().await {
                        Some(r) => Ok(Some(r)),
                        None => Err(e),
                    },
                }
            }
        }
    }

    async fn stored(&self) -> Result<Vec<StoredCodex>> {
        let dirs = self.dirs();
        let target = self.target.clone();
        blocking(move || Ok(store::list(&dirs, target.as_deref()))).await
    }

    async fn ensure(&self, version: &CodexVersion) -> Result<StoredCodex> {
        let target = self.local_target()?;
        self.ensure_target(version, &target).await
    }

    async fn package_path(&self, version: &CodexVersion, target: &str) -> Result<PathBuf> {
        if !sources::TARGETS.contains(&target) {
            return Err(Error::invalid(format!(
                "unknown Codex package target {target:?}"
            )));
        }
        let dirs = self.dirs();
        let d2 = dirs.clone();
        let _lock = blocking(move || store::lock(&d2)).await?;
        Ok(self.verified_archive(version, target).await?.0)
    }

    async fn prune(&self, keep: &[CodexVersion]) -> Result<Vec<CodexVersion>> {
        let target = self.local_target()?;
        let dirs = self.dirs();
        let keep = keep.to_vec();
        blocking(move || {
            let _lock = store::lock(&dirs)?;
            store::prune(&dirs, &target, &keep)
        })
        .await
    }

    async fn download_app(&self, package: &AppPackage) -> Result<AppDownloadReport> {
        if !self.official_app_url(&package.url) {
            return Err(Error::Unsupported {
                what: format!(
                    "downloading {} (not under the official download base)",
                    package.url
                ),
            });
        }
        let Some(sha) = package.sha256.clone() else {
            return Err(Error::Unsupported {
                what: format!(
                    "downloading {} (no published sha256 to verify it)",
                    package.url
                ),
            });
        };
        let name = package
            .url
            .rsplit('/')
            .next()
            .filter(|n| {
                !n.is_empty()
                    && !n.starts_with('.')
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
            })
            .ok_or_else(|| Error::invalid(format!("unexpected package URL {}", package.url)))?
            .to_owned();
        let dest = self.downloads_dir().join(&name);
        let want = Checksum::Sha256(sha.clone());
        if dest.is_file() {
            let d = dest.clone();
            let (w, size) = (want.clone(), package.size);
            if blocking(move || http::verify_file(&d, &w, size))
                .await
                .is_ok()
            {
                return Ok(report(package, dest, sha));
            }
            let _ = std::fs::remove_file(&dest);
        }
        let client = http::client(true)?;
        let got = http::download(
            &client,
            &package.url,
            &dest,
            &want,
            package.size,
            MAX_APP_PACKAGE_BYTES,
        )
        .await?;
        Ok(report(package, dest, got))
    }

    async fn download_windows_app(
        &self,
        request: WindowsAppRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        self.windows_app_impl(request, progress).await
    }

    async fn install_standalone(
        &self,
        version: &CodexVersion,
        layout: &StandaloneLayout,
    ) -> Result<InstallReport> {
        let stored = self.ensure(version).await?;
        let entry = stored
            .executable
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::other("store entry without a directory"))?;
        let name = format!("{version}-{}", stored.target);
        let layout = layout.clone();
        let outcome = blocking(move || standalone::install(&entry, &name, &layout)).await?;
        Ok(InstallReport {
            version: version.clone(),
            target: InstallTarget::Local,
            executable: Some(outcome.visible.display().to_string()),
            previous: outcome.previous,
            notes: Vec::new(),
        })
    }

    async fn rollback_standalone(&self, layout: &StandaloneLayout) -> Result<InstallReport> {
        let layout = layout.clone();
        let (outcome, version) = blocking(move || {
            let o = standalone::rollback(&layout)?;
            let v = standalone::version_of(&o.visible);
            Ok((o, v))
        })
        .await?;
        let version = version.ok_or_else(|| Error::Process {
            message: "the restored codex did not report its version".into(),
        })?;
        Ok(InstallReport {
            version,
            target: InstallTarget::Local,
            executable: Some(outcome.visible.display().to_string()),
            previous: outcome.previous,
            notes: Vec::new(),
        })
    }
}

fn report(package: &AppPackage, path: PathBuf, sha256: String) -> AppDownloadReport {
    let command = match package.kind {
        AppPackageKind::Deb => Some(format!("sudo apt install {}", path.display())),
        AppPackageKind::Rpm => Some(format!("sudo dnf install {}", path.display())),
        _ => None,
    };
    AppDownloadReport {
        package: package.clone(),
        path,
        sha256,
        command,
        windows: None,
    }
}
