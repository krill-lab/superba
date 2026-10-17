//! The newest desktop app build per OS (spike B2): APT `Packages` and RPM
//! `primary.xml.gz` (Linux, with sha256), the Sparkle appcast (macOS) and the
//! Windows Store / MSIX JSON.

use std::io::Read as _;

use serde::Deserialize;
use superba_core::{AppOs, AppPackage, AppPackageKind, AppRelease, Error, Result};

use crate::xml;

/// Debian architecture of this machine.
pub fn deb_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("amd64"),
        "aarch64" | "arm64" => Some("arm64"),
        _ => None,
    }
}

/// RPM architecture of this machine.
pub fn rpm_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        _ => None,
    }
}

/// Sparkle architecture of this machine.
pub fn mac_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" | "x64" => Some("x64"),
        "aarch64" | "arm64" => Some("arm64"),
        _ => None,
    }
}

/// The `chatgpt` stanza of an APT `Packages` index: `(version, package)`.
/// `base` is the repository root (`.../linux/deb`).
pub fn parse_packages(text: &str, base: &str, arch: &str) -> Option<(String, AppPackage)> {
    let mut best: Option<(String, AppPackage)> = None;
    for stanza in text.split("\n\n") {
        let mut fields = std::collections::BTreeMap::new();
        for line in stanza.lines() {
            if line.starts_with([' ', '\t']) {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                fields.insert(k.trim().to_owned(), v.trim().to_owned());
            }
        }
        if fields.get("Package").map(String::as_str) != Some("chatgpt") {
            continue;
        }
        let (Some(version), Some(file)) = (fields.get("Version"), fields.get("Filename")) else {
            continue;
        };
        if file.contains("..") || file.starts_with('/') {
            continue;
        }
        let package = AppPackage {
            os: AppOs::Linux,
            kind: AppPackageKind::Deb,
            arch: fields
                .get("Architecture")
                .cloned()
                .unwrap_or_else(|| arch.to_owned()),
            url: format!("{}/{}", base.trim_end_matches('/'), file),
            sha256: fields
                .get("SHA256")
                .filter(|s| s.len() == 64)
                .map(|s| s.to_ascii_lowercase()),
            size: fields.get("Size").and_then(|s| s.parse().ok()),
            signature: None,
        };
        if best
            .as_ref()
            .is_none_or(|(v, _)| app_version_cmp(version, v).is_gt())
        {
            best = Some((version.clone(), package));
        }
    }
    best
}

/// `(href, sha256)` of the primary metadata in an RPM `repomd.xml`.
pub fn parse_repomd(xml_text: &str) -> Result<(String, Option<String>)> {
    let els = xml::elements(xml_text, "repomd.xml")?;
    let mut in_primary = None;
    let (mut href, mut sha) = (None, None);
    for (i, e) in els.iter().enumerate() {
        if e.path == "repomd/data" {
            in_primary = (e.attr("type") == Some("primary")).then_some(i);
            continue;
        }
        if in_primary.is_none() {
            continue;
        }
        match e.path.as_str() {
            "repomd/data/location" => href = e.attr("href").map(str::to_owned),
            "repomd/data/checksum" if e.attr("type") == Some("sha256") => {
                sha = Some(e.text.trim().to_ascii_lowercase());
            }
            _ => {}
        }
        if href.is_some() && sha.is_some() {
            break;
        }
    }
    let href = href.ok_or_else(|| Error::parse("repomd.xml", "no primary metadata"))?;
    if href.contains("..") || href.starts_with('/') {
        return Err(Error::parse("repomd.xml", "unsafe primary location"));
    }
    Ok((href, sha))
}

/// Decompress a gzip body (bounded).
pub fn gunzip(bytes: &[u8], max: usize) -> Result<String> {
    let mut out = String::new();
    flate2::read::GzDecoder::new(bytes)
        .take(max as u64 + 1)
        .read_to_string(&mut out)
        .map_err(|e| Error::parse("primary.xml.gz", e.to_string()))?;
    if out.len() > max {
        return Err(Error::parse("primary.xml.gz", "too large"));
    }
    Ok(out)
}

/// The newest `chatgpt` package of an RPM `primary.xml`: `(version, package)`.
/// `base` is the architecture's repository root (`.../linux/rpm/x86_64`).
pub fn parse_primary(xml_text: &str, base: &str) -> Result<Option<(String, AppPackage)>> {
    let els = xml::elements(xml_text, "primary.xml")?;
    let mut best: Option<(String, AppPackage)> = None;
    let mut cur: Option<(Option<String>, Option<String>, AppPackage)> = None;
    let mut flush = |cur: &mut Option<(Option<String>, Option<String>, AppPackage)>| {
        if let Some((Some(name), Some(version), pkg)) = cur.take()
            && name == "chatgpt"
            && !pkg.url.is_empty()
            && best
                .as_ref()
                .is_none_or(|(v, _)| app_version_cmp(&version, v).is_gt())
        {
            best = Some((version, pkg));
        }
    };
    for e in &els {
        match e.path.as_str() {
            "metadata/package" => {
                flush(&mut cur);
                cur = Some((
                    None,
                    None,
                    AppPackage {
                        os: AppOs::Linux,
                        kind: AppPackageKind::Rpm,
                        arch: String::new(),
                        url: String::new(),
                        sha256: None,
                        size: None,
                        signature: None,
                    },
                ));
            }
            p => {
                let Some((name, version, pkg)) = cur.as_mut() else {
                    continue;
                };
                match p {
                    "metadata/package/name" => *name = Some(e.text.trim().to_owned()),
                    "metadata/package/arch" => pkg.arch = e.text.trim().to_owned(),
                    "metadata/package/version" => *version = e.attr("ver").map(str::to_owned),
                    "metadata/package/checksum" if e.attr("type") == Some("sha256") => {
                        pkg.sha256 = Some(e.text.trim().to_ascii_lowercase());
                    }
                    "metadata/package/size" => {
                        pkg.size = e.attr("package").and_then(|s| s.parse().ok());
                    }
                    "metadata/package/location" => {
                        if let Some(h) = e
                            .attr("href")
                            .filter(|h| !h.contains("..") && !h.starts_with('/'))
                        {
                            pkg.url = format!("{}/{}", base.trim_end_matches('/'), h);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    flush(&mut cur);
    Ok(best)
}

/// The newest item of a Sparkle appcast as a macOS release.
pub fn parse_appcast(xml_text: &str, arch: &str) -> Result<Option<AppRelease>> {
    let els = xml::elements(xml_text, "appcast")?;
    let mut version: Option<String> = None;
    let mut package: Option<AppPackage> = None;
    let mut seen_item = false;
    for e in &els {
        match e.path.as_str() {
            "rss/channel/item" => {
                if seen_item {
                    break;
                }
                seen_item = true;
            }
            "rss/channel/item/shortVersionString" => version = Some(e.text.trim().to_owned()),
            "rss/channel/item/enclosure" => {
                // Only an https enclosure is ever downloaded or opened.
                if let Some(url) = e.attr("url").filter(|u| is_https(u)) {
                    package = Some(AppPackage {
                        os: AppOs::Macos,
                        kind: AppPackageKind::MacZip,
                        arch: arch.to_owned(),
                        url: url.to_owned(),
                        sha256: None,
                        size: e.attr("length").and_then(|l| l.parse().ok()),
                        signature: e.attr("edSignature").map(str::to_owned),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(version.map(|version| AppRelease {
        version,
        date: None,
        bundled_codex: None,
        packages: package.into_iter().collect(),
        store_product_id: None,
    }))
}

/// `windows-store-update.json` + `windows-release.json` as a Windows release.
pub fn parse_windows(store_json: &str, release_json: Option<&str>) -> Result<AppRelease> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StoreUpdate {
        release_version: String,
        #[serde(default)]
        store_product_id: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WinPackage {
        architecture: String,
        download_url: String,
    }
    #[derive(Deserialize)]
    struct Release {
        #[serde(default)]
        packages: Vec<WinPackage>,
    }
    let s: StoreUpdate = serde_json::from_str(store_json)
        .map_err(|e| Error::parse("windows-store-update.json", e.to_string()))?;
    let packages = release_json
        .and_then(|r| serde_json::from_str::<Release>(r).ok())
        .map(|r| r.packages)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.download_url.starts_with("https://"))
        .map(|p| AppPackage {
            os: AppOs::Windows,
            kind: AppPackageKind::Msix,
            arch: p.architecture,
            url: p.download_url,
            sha256: None,
            size: None,
            signature: None,
        })
        .collect();
    Ok(AppRelease {
        version: s.release_version,
        date: None,
        bundled_codex: None,
        packages,
        store_product_id: s.store_product_id,
    })
}

/// `url` is an absolute https URL.
fn is_https(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.has_host())
}

/// `url` lies under the download base `base` (`<base>/<something>`), compared
/// after URL normalization, so `..` segments, credentials, another scheme, host
/// or port never pass.
pub fn under_base(base: &str, url: &str) -> bool {
    let (Ok(b), Ok(u)) = (
        url::Url::parse(base.trim_end_matches('/')),
        url::Url::parse(url),
    ) else {
        return false;
    };
    let prefix = format!("{}/", b.as_str().trim_end_matches('/'));
    u.username().is_empty()
        && u.password().is_none()
        && u.as_str().len() > prefix.len()
        && u.as_str().starts_with(&prefix)
}

/// Compare app versions (`26.1013.40934`) numerically, component by component.
pub fn app_version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |s: &str| -> Vec<u64> {
        s.split(['.', '-'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    parts(a).cmp(&parts(b))
}

/// A random installation id for the appcast request (a fresh one each time, so
/// requests cannot be linked).
pub fn random_installation_id() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = Sha256::new();
    h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    let x = hex::encode(h.finalize());
    format!(
        "{}-{}-4{}-a{}-{}",
        &x[0..8],
        &x[8..12],
        &x[13..16],
        &x[17..20],
        &x[20..32]
    )
}
