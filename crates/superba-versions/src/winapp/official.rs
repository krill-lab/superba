//! OpenAI's own Windows release files on `persistent.oaistatic.com`:
//! `windows-release.json` (the MSIX of each architecture) and
//! `windows-store-update.json` (the Store build and the app release it is).

use serde::Deserialize;
use superba_core::{Error, Result};

/// The host OpenAI serves the Windows packages from.
pub const OFFICIAL_HOST: &str = "persistent.oaistatic.com";

/// One MSIX of `windows-release.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfficialPackage {
    /// MSIX architecture (`x64`, `arm64`).
    pub arch: String,
    /// Download URL (checked by the caller's predicate).
    pub url: String,
}

/// `windows-release.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfficialRelease {
    /// MSIX package version (`26.1013.4894.0`).
    pub package_version: String,
    /// Release date as published (`2026-10-17T05:55:44Z`).
    pub release_date: Option<String>,
    /// Packages whose URL was accepted.
    pub packages: Vec<OfficialPackage>,
}

impl OfficialRelease {
    /// The package for an MSIX architecture.
    pub fn package_for(&self, arch: &str) -> Option<&OfficialPackage> {
        self.packages
            .iter()
            .find(|p| p.arch.eq_ignore_ascii_case(arch))
    }
}

/// An https URL on [`OFFICIAL_HOST`] (default port, no credentials).
pub fn is_official_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some(OFFICIAL_HOST)
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

/// Parse `windows-release.json`, keeping only packages whose URL `accept`s
/// (see [`is_official_url`]).
pub fn parse_release(text: &str, accept: impl Fn(&str) -> bool) -> Result<OfficialRelease> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Package {
        architecture: String,
        download_url: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Release {
        package_version: String,
        #[serde(default)]
        release_date: Option<String>,
        #[serde(default)]
        packages: Vec<Package>,
    }
    let r: Release = serde_json::from_str(text)
        .map_err(|e| Error::parse("windows-release.json", e.to_string()))?;
    if !valid_version(&r.package_version) {
        return Err(Error::parse(
            "windows-release.json",
            format!("unexpected packageVersion {:?}", r.package_version),
        ));
    }
    let packages = r
        .packages
        .into_iter()
        .filter(|p| {
            let ok = accept(&p.download_url);
            if !ok {
                tracing::warn!(url = %p.download_url, "windows-release.json package URL refused");
            }
            ok && !p.architecture.is_empty()
        })
        .map(|p| OfficialPackage {
            arch: p.architecture.to_ascii_lowercase(),
            url: p.download_url,
        })
        .collect();
    Ok(OfficialRelease {
        package_version: r.package_version,
        release_date: r.release_date,
        packages,
    })
}

/// `windows-store-update.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreUpdate {
    /// MSIX build in the Store (`26.1013.4894.0`).
    pub build_version: Option<String>,
    /// App release (`26.1013.40934`).
    pub release_version: Option<String>,
    /// Store product id.
    pub product_id: Option<String>,
    /// MSIX package name (`OpenAI.Codex`).
    pub package_identity: Option<String>,
}

/// Parse `windows-store-update.json`.
pub fn parse_store_update(text: &str) -> Result<StoreUpdate> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        #[serde(default)]
        build_version: Option<String>,
        #[serde(default)]
        release_version: Option<String>,
        #[serde(default)]
        store_product_id: Option<String>,
        #[serde(default)]
        package_identity: Option<String>,
    }
    let r: Raw = serde_json::from_str(text)
        .map_err(|e| Error::parse("windows-store-update.json", e.to_string()))?;
    Ok(StoreUpdate {
        build_version: r.build_version.filter(|v| valid_version(v)),
        release_version: r.release_version,
        product_id: r
            .store_product_id
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric())),
        package_identity: r.package_identity,
    })
}

/// The app release a package version belongs to: the Store's release version
/// when the Store build is that package.
pub fn release_version_of(package_version: &str, store: &StoreUpdate) -> Option<String> {
    (store.build_version.as_deref() == Some(package_version))
        .then(|| store.release_version.clone())
        .flatten()
}

/// Dotted numeric version with 2 to 4 parts.
pub fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (2..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 10 && p.chars().all(|c| c.is_ascii_digit()))
}
