//! [`HttpVersionStore::download_windows_app`]: the sources in the configured
//! order, each producing a verified package (and the framework packages it
//! still needs), or a reason that is reported when every source failed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use superba_core::{
    AppDownloadReport, AppOs, AppPackage, AppPackageKind, AppPackageSource, DownloadProgress,
    DownloadProgressSink, DownloadStage, Error, MsixDependency, Result, Timestamp,
    WindowsAppDownload, WindowsAppRequest, WindowsPackageFile,
};

use super::download::{self, Target};
use super::mirror::{self, MirrorFile};
use super::msix::{self, PackageManifest};
use super::official::{self, StoreUpdate};
use super::{STORE_PRODUCT_ID, msix_arch, trust};
use crate::{HttpVersionStore, MAX_APP_PACKAGE_BYTES, METADATA_TIMEOUT, http};

/// How long a Store mirror answer is reused (its links stay valid for hours;
/// the resolver challenges clients that ask again too soon).
const LISTING_TTL: Duration = Duration::from_secs(30 * 60);

/// A cached link is not used this close to its expiry.
const EXPIRY_MARGIN_SECS: i64 = 10 * 60;

/// Largest framework package accepted.
const MAX_DEPENDENCY_BYTES: u64 = 512 * 1024 * 1024;

/// A cached resolver answer.
#[derive(Debug, Serialize, Deserialize)]
struct CachedListing {
    product_id: String,
    fetched_at: Timestamp,
    files: Vec<CachedFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedFile {
    name: String,
    url: String,
    expires: Option<String>,
    sha1: Option<String>,
    size: Option<String>,
}

impl From<&MirrorFile> for CachedFile {
    fn from(f: &MirrorFile) -> Self {
        Self {
            name: f.name.clone(),
            url: f.url.clone(),
            expires: f.expires.clone(),
            sha1: f.sha1.clone(),
            size: f.size.as_ref().map(|s| s.text.clone()),
        }
    }
}

impl From<CachedFile> for MirrorFile {
    fn from(f: CachedFile) -> Self {
        Self {
            size: f.size.as_deref().and_then(mirror::ListedSize::parse),
            name: f.name,
            url: f.url,
            expires: f.expires,
            sha1: f.sha1,
        }
    }
}

/// `P1=<unix seconds>` of a temporary CDN link.
fn link_expiry(url: &str) -> Option<i64> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "P1")
        .and_then(|(_, v)| v.parse().ok())
}

/// A file name that is safe as a single path component.
fn safe_file_name(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~'));
    ok.then(|| name.to_owned())
}

fn source_label(source: AppPackageSource) -> &'static str {
    match source {
        AppPackageSource::Official => "official server (persistent.oaistatic.com)",
        _ => "Microsoft Store via store.rg-adguard.net",
    }
}

fn package_file(
    path: PathBuf,
    manifest: &PackageManifest,
    got: &download::Fetched,
    listed_sha1: bool,
) -> WindowsPackageFile {
    WindowsPackageFile {
        path,
        identity: manifest.identity.clone(),
        size: got.size,
        sha256: got.sha256.clone(),
        sha1: listed_sha1.then(|| got.sha1.clone()),
        signer: None,
    }
}

async fn manifest_of(path: &Path, arch: &str) -> Result<PackageManifest> {
    let (p, a) = (path.to_path_buf(), arch.to_owned());
    tokio::task::spawn_blocking(move || msix::read_manifest(&p, &a))
        .await
        .map_err(|e| Error::other(format!("background task failed: {e}")))?
}

impl HttpVersionStore {
    /// `url` may be followed for a Store mirror download: the Microsoft CDN,
    /// or the test CDN base of [`crate::sources::Endpoints::store_cdn_extra`].
    pub(crate) fn store_cdn_url(&self, url: &str) -> bool {
        mirror::is_store_cdn_url(url)
            || self
                .endpoints
                .store_cdn_extra
                .as_deref()
                .is_some_and(|b| crate::app::under_base(b, url))
    }

    /// `windows-store-update.json`, best effort.
    async fn store_update(&self) -> Option<StoreUpdate> {
        let client = http::client(true).ok()?;
        let url = format!(
            "{}/windows-store-update.json",
            self.endpoints.app.trim_end_matches('/')
        );
        let text = http::get_text(&client, &url, METADATA_TIMEOUT).await.ok()?;
        official::parse_store_update(&text).ok()
    }

    pub(crate) async fn windows_app_impl(
        &self,
        request: WindowsAppRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        // The machine's native architecture (the caller's: an x64 Superba on
        // ARM64 Windows still asks for arm64).
        let arch = msix_arch(&request.arch).ok_or_else(|| Error::Unsupported {
            what: format!("the ChatGPT app on a {} computer", request.arch),
        })?;
        let store = self.store_update().await.unwrap_or_default();
        let mut reasons: Vec<String> = Vec::new();
        let order = request.source.order();
        let mut last: Option<Error> = None;
        for &source in order {
            if progress.cancel_requested() {
                return Err(Error::Cancelled);
            }
            progress.report(DownloadProgress::stage(
                DownloadStage::Resolving,
                Some(source),
            ));
            let attempt = match source {
                AppPackageSource::Official => {
                    let work = self.official_package(arch, &request, &store, progress.as_ref());
                    until_cancelled(work, progress.as_ref()).await
                }
                _ => {
                    let work = self.mirror_package(arch, &request, &store, progress.as_ref());
                    until_cancelled(work, progress.as_ref()).await
                }
            };
            match attempt {
                Ok(mut report) => {
                    if let Some(w) = report.windows.as_mut() {
                        w.fallback_reasons = reasons;
                    }
                    download::prune_app_packages(&self.downloads_dir(), &report.path);
                    return Ok(report);
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => {
                    tracing::warn!(source = source.code(), error = %e, "Windows app package source failed");
                    reasons.push(format!("{}: {}", source_label(source), e.detail()));
                    last = Some(e);
                }
            }
        }
        match (order.len(), last) {
            (1, Some(e)) => Err(e),
            _ => Err(Error::Other {
                message: format!(
                    "no source provided a verified ChatGPT package for Windows {arch}: {}",
                    reasons.join("; ")
                ),
            }),
        }
    }

    async fn official_package(
        &self,
        arch: &str,
        request: &WindowsAppRequest,
        store: &StoreUpdate,
        progress: &dyn DownloadProgressSink,
    ) -> Result<AppDownloadReport> {
        let client = http::client(true)?;
        let url = format!(
            "{}/windows-release.json",
            self.endpoints.app.trim_end_matches('/')
        );
        let text = http::get_text(&client, &url, METADATA_TIMEOUT).await?;
        let release = official::parse_release(&text, |u| self.official_app_url(u))?;
        let pkg = release
            .package_for(arch)
            .or_else(|| {
                (arch == "arm64")
                    .then(|| release.package_for("x64"))
                    .flatten()
            })
            .ok_or_else(|| Error::NotFound {
                what: format!("a {arch} package in windows-release.json"),
            })?
            .clone();
        let dest = self.downloads_dir().join(
            safe_file_name(&format!(
                "ChatGPT-{}-{}.msix",
                release.package_version, pkg.arch
            ))
            .ok_or_else(|| Error::invalid("unexpected package version"))?,
        );
        let accept: Arc<dyn Fn(&str) -> bool + Send + Sync> = {
            let me = self.clone();
            Arc::new(move |u: &str| me.official_app_url(u))
        };
        let got = download::fetch(
            &download::client(accept)?,
            &Target {
                url: pkg.url.clone(),
                dest: dest.clone(),
                sha1: None,
                listed_size: None,
                max: MAX_APP_PACKAGE_BYTES,
                source: AppPackageSource::Official,
                index: 1,
                count: 1,
            },
            progress,
        )
        .await?;
        progress.report(DownloadProgress {
            stage: DownloadStage::Verifying,
            source: Some(AppPackageSource::Official),
            file: dest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            done: got.size,
            total: Some(got.size),
            resumed_from: 0,
            index: 1,
            count: 1,
        });
        let manifest = match manifest_of(&dest, arch).await.and_then(|m| {
            trust::check_app_identity(&m, arch, Some(&release.package_version)).map(|()| m)
        }) {
            Ok(m) => m,
            Err(e) => {
                download::remove_all(&dest);
                return Err(e);
            }
        };
        let (dependencies, unresolved) = self
            .dependencies_for(&manifest, request, progress, None)
            .await?;
        let release_version = official::release_version_of(&release.package_version, store);
        Ok(report(
            AppPackage {
                os: AppOs::Windows,
                kind: AppPackageKind::Msix,
                arch: pkg.arch.clone(),
                url: pkg.url.clone(),
                sha256: Some(got.sha256.clone()),
                size: Some(got.size),
                signature: None,
            },
            WindowsAppDownload {
                source: AppPackageSource::Official,
                fallback_reasons: Vec::new(),
                package: package_file(dest, &manifest, &got, false),
                release_version,
                dependencies,
                unresolved,
            },
        ))
    }

    fn listing_cache(&self, product_id: &str) -> PathBuf {
        self.downloads_dir()
            .join(format!("store-mirror-{product_id}.json"))
    }

    /// The resolver's files for `product_id` (cached while fresh).
    async fn mirror_listing(&self, product_id: &str, fresh: bool) -> Result<Vec<MirrorFile>> {
        let cache = self.listing_cache(product_id);
        if !fresh
            && let Some(c) = std::fs::read(&cache)
                .ok()
                .and_then(|b| serde_json::from_slice::<CachedListing>(&b).ok())
        {
            let age = Timestamp::now().as_millis() - c.fetched_at.as_millis();
            let now_s = Timestamp::now().as_millis() / 1000;
            let links_ok = c
                .files
                .iter()
                .all(|f| link_expiry(&f.url).is_none_or(|e| e - EXPIRY_MARGIN_SECS > now_s));
            if c.product_id == product_id
                && age >= 0
                && (age as u128) < LISTING_TTL.as_millis()
                && links_ok
            {
                return Ok(c.files.into_iter().map(MirrorFile::from).collect());
            }
        }
        let base = self.endpoints.store_mirror.trim_end_matches('/');
        let url = format!("{base}{}", mirror::API_PATH);
        let client = http::client(false)?;
        let send = client
            .post(&url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::REFERER, format!("{base}/"))
            .header(reqwest::header::ORIGIN, base)
            .body(mirror::request_body(product_id, mirror::RING))
            .send();
        let resp = tokio::time::timeout(Duration::from_secs(30), send)
            .await
            .map_err(|_| Error::Timeout {
                what: format!("POST {url}"),
            })?
            .map_err(|e| Error::Network {
                url: url.clone(),
                message: e.to_string(),
            })?;
        let status = resp.status().as_u16();
        let challenge = resp
            .headers()
            .get("cf-mitigated")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("challenge"));
        let body = tokio::time::timeout(Duration::from_secs(30), resp.bytes())
            .await
            .map_err(|_| Error::Timeout {
                what: format!("POST {url}"),
            })?
            .map_err(|e| Error::Network {
                url: url.clone(),
                message: e.to_string(),
            })?;
        if body.len() > http::MAX_TEXT_BYTES {
            return Err(Error::parse(&url, "answer too large"));
        }
        let files = mirror::check_answer(status, challenge, &String::from_utf8_lossy(&body))?;
        let cached = CachedListing {
            product_id: product_id.to_owned(),
            fetched_at: Timestamp::now(),
            files: files.iter().map(CachedFile::from).collect(),
        };
        if let (Some(dir), Ok(b)) = (cache.parent(), serde_json::to_vec_pretty(&cached))
            && std::fs::create_dir_all(dir).is_ok()
        {
            let _ = std::fs::write(&cache, b);
        }
        Ok(files)
    }

    /// Download one listed file into the downloads directory.
    async fn fetch_listed(
        &self,
        file: &MirrorFile,
        max: u64,
        index: usize,
        count: usize,
        progress: &dyn DownloadProgressSink,
    ) -> Result<(PathBuf, download::Fetched)> {
        let dest = self.downloads_dir().join(
            safe_file_name(&file.name)
                .ok_or_else(|| Error::invalid(format!("unexpected file name {:?}", file.name)))?,
        );
        let accept: Arc<dyn Fn(&str) -> bool + Send + Sync> = {
            let me = self.clone();
            Arc::new(move |u: &str| me.store_cdn_url(u))
        };
        let got = download::fetch(
            &download::client(accept)?,
            &Target {
                url: file.url.clone(),
                dest: dest.clone(),
                sha1: file.sha1.clone(),
                listed_size: file.size.clone(),
                max,
                source: AppPackageSource::StoreMirror,
                index,
                count,
            },
            progress,
        )
        .await?;
        Ok((dest, got))
    }

    async fn mirror_package(
        &self,
        arch: &str,
        request: &WindowsAppRequest,
        store: &StoreUpdate,
        progress: &dyn DownloadProgressSink,
    ) -> Result<AppDownloadReport> {
        let product_id = store.product_id.as_deref().unwrap_or(STORE_PRODUCT_ID);
        let url_ok = |u: &str| self.store_cdn_url(u);
        let mut fresh = false;
        let (files, (file, name), (dest, got)) = loop {
            let files = self.mirror_listing(product_id, fresh).await?;
            let picked = mirror::select_app(&files, arch, &url_ok).ok_or_else(|| Error::NotFound {
                what: format!(
                    "a ChatGPT package for {arch} among the {} files store.rg-adguard.net listed",
                    files.len()
                ),
            })?;
            match self
                .fetch_listed(&picked.0, MAX_APP_PACKAGE_BYTES, 1, 1, progress)
                .await
            {
                Ok(done) => break (files, picked, done),
                // A cached link that expired: ask the resolver again, once.
                Err(Error::Http {
                    status: 403 | 410, ..
                }) if !fresh => {
                    let _ = std::fs::remove_file(self.listing_cache(product_id));
                    fresh = true;
                }
                Err(e) => return Err(e),
            }
        };
        let manifest = match manifest_of(&dest, arch)
            .await
            .and_then(|m| trust::check_app_identity(&m, arch, Some(&name.version)).map(|()| m))
        {
            Ok(m) => m,
            Err(e) => {
                download::remove_all(&dest);
                return Err(e);
            }
        };
        let (dependencies, unresolved) = self
            .dependencies_for(&manifest, request, progress, Some(&files))
            .await?;
        let release_version = official::release_version_of(
            manifest
                .bundle_version
                .as_deref()
                .unwrap_or(&manifest.identity.version),
            store,
        );
        Ok(report(
            AppPackage {
                os: AppOs::Windows,
                kind: name.kind,
                arch: name.arch.clone(),
                url: download::shown(&file.url),
                sha256: Some(got.sha256.clone()),
                size: Some(got.size),
                signature: None,
            },
            WindowsAppDownload {
                source: AppPackageSource::StoreMirror,
                fallback_reasons: Vec::new(),
                package: package_file(dest, &manifest, &got, true),
                release_version,
                dependencies,
                unresolved,
            },
        ))
    }

    /// The framework packages `manifest` needs that are not installed:
    /// downloaded from the Store mirror (its `listing`, or a lookup when the
    /// request allows the mirror), else reported as unresolved.
    async fn dependencies_for(
        &self,
        manifest: &PackageManifest,
        request: &WindowsAppRequest,
        progress: &dyn DownloadProgressSink,
        listing: Option<&[MirrorFile]>,
    ) -> Result<(Vec<WindowsPackageFile>, Vec<MsixDependency>)> {
        let arch = manifest.identity.arch.clone();
        let missing: Vec<MsixDependency> = manifest
            .dependencies
            .iter()
            .filter(|d| !mirror::dependency_installed(d, &arch, &request.installed))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let owned;
        let files: &[MirrorFile] = match listing {
            Some(f) => f,
            None if request
                .source
                .order()
                .contains(&AppPackageSource::StoreMirror) =>
            {
                let id = STORE_PRODUCT_ID;
                match self.mirror_listing(id, false).await {
                    Ok(f) => {
                        owned = f;
                        &owned
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Store mirror lookup for dependencies failed");
                        return Ok((Vec::new(), missing));
                    }
                }
            }
            None => return Ok((Vec::new(), missing)),
        };
        let url_ok = |u: &str| self.store_cdn_url(u);
        let (chosen, mut unresolved) =
            mirror::select_dependencies(files, &missing, &arch, &request.installed, &url_ok);
        let count = chosen.len() + 1;
        let mut out = Vec::new();
        for (i, (file, name)) in chosen.into_iter().enumerate() {
            let dep = missing
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(&name.name))
                .cloned();
            let fetched = self
                .fetch_listed(&file, MAX_DEPENDENCY_BYTES, i + 2, count, progress)
                .await;
            let checked = match fetched {
                Ok((dest, got)) => {
                    let m = manifest_of(&dest, &arch).await;
                    match (m, &dep) {
                        (Ok(m), Some(d)) => match trust::check_dependency_identity(&m, d, &arch) {
                            Ok(()) => Ok(package_file(dest, &m, &got, true)),
                            Err(e) => {
                                download::remove_all(&dest);
                                Err(e)
                            }
                        },
                        (Err(e), _) => {
                            download::remove_all(&dest);
                            Err(e)
                        }
                        (Ok(_), None) => Err(Error::other("dependency vanished")),
                    }
                }
                Err(e) => Err(e),
            };
            match checked {
                Ok(f) => out.push(f),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => {
                    tracing::warn!(file = %file.name, error = %e, "dependency download failed");
                    unresolved.extend(dep);
                }
            }
        }
        Ok((out, unresolved))
    }
}

/// Run `work`, dropping it as soon as a cancel is asked for (a lookup that
/// waits for a slow server must not hold the cancel up; a transfer keeps its
/// partial file either way).
async fn until_cancelled<T>(
    work: impl std::future::Future<Output = Result<T>>,
    progress: &dyn DownloadProgressSink,
) -> Result<T> {
    tokio::pin!(work);
    loop {
        if progress.cancel_requested() {
            return Err(Error::Cancelled);
        }
        tokio::select! {
            r = &mut work => return r,
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

fn report(package: AppPackage, windows: WindowsAppDownload) -> AppDownloadReport {
    AppDownloadReport {
        path: windows.package.path.clone(),
        sha256: windows.package.sha256.clone(),
        package,
        command: None,
        windows: Some(windows),
    }
}
