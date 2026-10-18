//! GitHub release documents and the asset a platform installs.

use semver::Version;
use serde::Deserialize;
use superba_core::SelfRelease;

use super::version::{cmp, parse_tag};

/// Where the release pages live (for links; never taken from the API).
pub const RELEASES_PAGE: &str = "https://github.com/krill-lab/superba/releases/tag";

/// The name of the checksum file attached to every release.
pub const SUMS_FILE: &str = "SHA256SUMS.txt";

/// Longest release note kept (characters).
const MAX_NOTES: usize = 20_000;

/// Operating system of a package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    /// macOS.
    Macos,
    /// Windows.
    Windows,
    /// Linux.
    Linux,
}

/// The machine a package is chosen for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    /// Operating system.
    pub os: Os,
    /// `std::env::consts::ARCH` (`x86_64`, `aarch64`, ...).
    pub arch: &'static str,
    /// A static musl build (the headless `spb` archive).
    pub musl: bool,
}

impl Platform {
    /// This build.
    pub fn current() -> Self {
        Self {
            os: if cfg!(target_os = "macos") {
                Os::Macos
            } else if cfg!(windows) {
                Os::Windows
            } else {
                Os::Linux
            },
            arch: std::env::consts::ARCH,
            musl: cfg!(target_env = "musl"),
        }
    }
}

/// The release asset for `platform`, or `None` when no package is published
/// for it.
pub fn asset_name(version: &str, platform: &Platform) -> Option<String> {
    Some(match (platform.os, platform.arch, platform.musl) {
        (Os::Macos, _, _) => format!("superba-{version}-macos-universal.zip"),
        (Os::Windows, "x86_64", _) => format!("superba-{version}-windows-x86_64.msi"),
        (Os::Linux, "x86_64", true) => format!("spb-{version}-linux-x86_64-musl.tar.gz"),
        (Os::Linux, "x86_64", false) => format!("superba-{version}-linux-x86_64.tar.gz"),
        (Os::Linux, "aarch64", false) => format!("superba-{version}-linux-aarch64.tar.gz"),
        _ => return None,
    })
}

#[derive(Debug, Deserialize)]
struct ApiAsset {
    name: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

fn clip(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    if text.chars().count() <= MAX_NOTES {
        return text;
    }
    let mut s: String = text.chars().take(MAX_NOTES).collect();
    s.push('…');
    s
}

impl ApiRelease {
    /// The release when it is published and its tag is one we accept.
    fn into_release(self, platform: &Platform) -> Option<(Version, SelfRelease)> {
        if self.draft {
            return None;
        }
        let version = parse_tag(&self.tag_name)?;
        let text = version.to_string();
        let asset = asset_name(&text, platform);
        let found = asset
            .as_ref()
            .and_then(|a| self.assets.iter().find(|x| &x.name == a));
        let has_sums = self.assets.iter().any(|a| a.name == SUMS_FILE);
        let release = SelfRelease {
            tag: self.tag_name.clone(),
            version: text,
            prerelease: self.prerelease || !version.pre.is_empty(),
            notes: clip(self.body.as_deref().unwrap_or_default()),
            published_at: self.published_at,
            url: format!("{RELEASES_PAGE}/{}", self.tag_name),
            // Only a package that is listed together with its checksum file.
            asset: found.filter(|_| has_sums).map(|a| a.name.clone()),
            asset_size: found.and_then(|a| a.size),
        };
        Some((version, release))
    }
}

/// `releases/latest` document.
pub fn parse_latest(body: &[u8], platform: &Platform) -> Option<SelfRelease> {
    let api: ApiRelease = serde_json::from_slice(body).ok()?;
    api.into_release(platform).map(|(_, r)| r)
}

/// What one `/releases` page holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListAnswer {
    /// The newest release, pre-releases included (the Preview channel).
    pub newest: Option<SelfRelease>,
    /// The newest final release (the Stable channel).
    pub newest_stable: Option<SelfRelease>,
}

/// The newest release and the newest final release of a `/releases` page, by
/// semver precedence (`alpha < beta < rc < final`, numeric parts compared as
/// numbers), not by publication date. Drafts and foreign tags are skipped; a
/// release GitHub marks as pre-release counts as one even with a final tag.
/// `None` when the document is not a list.
pub fn parse_list(body: &[u8], platform: &Platform) -> Option<ListAnswer> {
    let list: Vec<serde_json::Value> = serde_json::from_slice(body).ok()?;
    let releases: Vec<(Version, SelfRelease)> = list
        .into_iter()
        .filter_map(|v| serde_json::from_value::<ApiRelease>(v).ok())
        .filter_map(|r| r.into_release(platform))
        .collect();
    let newest = |stable_only: bool| {
        releases
            .iter()
            .filter(|(_, r)| !(stable_only && r.prerelease))
            .max_by(|(a, _), (b, _)| cmp(a, b))
            .map(|(_, r)| r.clone())
    };
    Some(ListAnswer {
        newest: newest(false),
        newest_stable: newest(true),
    })
}
