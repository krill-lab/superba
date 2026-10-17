//! Codex and desktop app versions: release lists, the Superba-managed Codex store,
//! pinning the app's Codex (`CODEX_CLI_PATH`), compatibility tiers and app
//! updates (Updates page, `spb codex|app`).
//!
//! Facts (v0.3 spike B):
//! - every Codex release ships `codex-package-<target>.tar.gz` plus
//!   `codex-package_SHA256SUMS`; the npm platform tarballs hold the same layout;
//!   a package is verified **before** extraction;
//! - pinning: the app main process reads `CODEX_CLI_PATH` at launch (a full app
//!   restart); a bad path silently falls back to the bundled Codex, so the
//!   app-server's process image is the only proof;
//! - the app refuses an app-server below the floor in its `app.asar`
//!   ([`crate::codex::APP_SERVER_FLOOR_FALLBACK`] when unreadable).
//!
//! Additive owner in v0.3 Phase 1: the versions/hosts builder.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::app::{AppInstall, CodexBinary};
use crate::codex::CodexVersion;
use crate::error::{Error, Result};
use crate::i18n::{LocalizedText, Notice};
use crate::ids::TargetId;
use crate::time::Timestamp;

/// One Codex release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexRelease {
    /// Version.
    pub version: CodexVersion,
    /// Git tag (`rust-v0.162.0`).
    #[serde(default)]
    pub tag: String,
    /// Release date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Timestamp>,
    /// Published as a pre-release (app-bundled alphas are).
    #[serde(default)]
    pub prerelease: bool,
    /// Desktop app builds that bundle it (`26.1013.31256`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundled_by: Vec<String>,
    /// sha256 of `codex-package-<target>.tar.gz` by target triple.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sha256: BTreeMap<String, String>,
}

/// Operating system of an app package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppOs {
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

/// Kind of an app package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppPackageKind {
    /// Debian package.
    Deb,
    /// RPM package.
    Rpm,
    /// macOS zip (Sparkle full update).
    MacZip,
    /// Windows MSIX.
    Msix,
    /// Windows MSIX bundle (packages for several architectures).
    MsixBundle,
    /// Windows APPX (framework dependencies such as VCLibs).
    Appx,
    /// Windows APPX bundle.
    AppxBundle,
}

impl AppPackageKind {
    /// The kind of a Windows package file by its extension (`.msix`,
    /// `.msixbundle`, `.appx`, `.appxbundle`, any case).
    pub fn windows_from_file_name(name: &str) -> Option<Self> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "msix" => AppPackageKind::Msix,
            "msixbundle" => AppPackageKind::MsixBundle,
            "appx" => AppPackageKind::Appx,
            "appxbundle" => AppPackageKind::AppxBundle,
            _ => return None,
        })
    }

    /// An MSIX / APPX package or bundle (opened with App Installer).
    pub fn is_windows_package(self) -> bool {
        matches!(
            self,
            AppPackageKind::Msix
                | AppPackageKind::MsixBundle
                | AppPackageKind::Appx
                | AppPackageKind::AppxBundle
        )
    }
}

/// One downloadable app package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPackage {
    /// OS.
    pub os: AppOs,
    /// Kind.
    pub kind: AppPackageKind,
    /// Architecture (`amd64`, `arm64`, `x86_64`, `aarch64`, `x64`).
    pub arch: String,
    /// Download URL.
    pub url: String,
    /// sha256 (hex), when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Sparkle `edSignature` (macOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// One desktop app build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRelease {
    /// App version (`26.1013.31256`).
    pub version: String,
    /// Release date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Timestamp>,
    /// Codex version it bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundled_codex: Option<CodexVersion>,
    /// Packages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<AppPackage>,
    /// Microsoft Store product id (Windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_product_id: Option<String>,
}

/// How a Codex binary was installed (decides how it is updated).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodexInstallKind {
    /// Official standalone layout (`~/.codex/packages/standalone`, `~/.local/bin`
    /// symlink); updated in place by Superba.
    Standalone,
    /// `npm i -g @openai/codex` (Superba shows the command).
    Npm,
    /// Homebrew (Superba shows the command).
    Homebrew,
    /// Bundled in the desktop app.
    AppBundled,
    /// Superba's own verified store (`<data dir>/codex/<ver>-<target>/`).
    SuperbaStore,
    /// Anything else.
    Other,
}

impl CodexInstallKind {
    /// Classify a `codex` binary by its *resolved* (symbolic-link free) path,
    /// local or remote (`\\` is treated as `/`): npm global installs
    /// (`node_modules/@openai/codex...`), Homebrew (`Cellar/`, `Caskroom/`,
    /// `homebrew/`, `linuxbrew/`), the official standalone layout
    /// (`<CODEX_HOME>/packages/standalone/`), else [`CodexInstallKind::Other`].
    /// App bundles and Superba's store need local knowledge and are recognised by
    /// the caller first.
    pub fn from_resolved_path(path: &str) -> Self {
        let p = path.replace('\\', "/");
        let lower = p.to_ascii_lowercase();
        if lower.contains("/node_modules/@openai/codex") {
            CodexInstallKind::Npm
        } else if [
            "/cellar/codex/",
            "/caskroom/codex/",
            "/homebrew/",
            "/linuxbrew/",
        ]
        .iter()
        .any(|m| lower.contains(m))
        {
            CodexInstallKind::Homebrew
        } else if p.contains("/packages/standalone/") {
            CodexInstallKind::Standalone
        } else {
            CodexInstallKind::Other
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            CodexInstallKind::Standalone => {
                LocalizedText::new("install_kind.standalone", "Standalone")
            }
            CodexInstallKind::Npm => LocalizedText::new("install_kind.npm", "npm"),
            CodexInstallKind::Homebrew => LocalizedText::new("install_kind.homebrew", "Homebrew"),
            CodexInstallKind::AppBundled => {
                LocalizedText::new("install_kind.app_bundled", "Bundled with the app")
            }
            CodexInstallKind::SuperbaStore => {
                LocalizedText::new("install_kind.superba_store", "Managed by Superba")
            }
            CodexInstallKind::Other => LocalizedText::new("install_kind.other", "Other"),
        }
    }
}

/// How a Codex version fits the desktop app (spike B6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompatTier {
    /// Exactly the version the app bundles.
    Same,
    /// Same minor, or one minor newer.
    Compatible,
    /// Older than the bundled one but at or above the app's floor.
    Older,
    /// Below the app's floor (blocked).
    BelowFloor,
    /// Two or more minors newer than the bundled one (untested protocol).
    Newer,
}

impl CompatTier {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            CompatTier::Same => LocalizedText::new("compat_tier.same", "Same as the app"),
            CompatTier::Compatible => LocalizedText::new("compat_tier.compatible", "Compatible"),
            CompatTier::Older => LocalizedText::new(
                "compat_tier.older",
                "Older: basic chat works, newer features may be missing",
            ),
            CompatTier::BelowFloor => {
                LocalizedText::new("compat_tier.below_floor", "Below the app's minimum version")
            }
            CompatTier::Newer => LocalizedText::new(
                "compat_tier.newer",
                "Much newer than the app's own Codex (untested)",
            ),
        }
    }

    /// True if Superba refuses to pin or install it for the app.
    pub fn is_blocked(self) -> bool {
        matches!(self, CompatTier::BelowFloor)
    }

    /// How `candidate` fits a desktop app that bundles `bundled` and refuses
    /// app-servers below `floor` (spike B6): below the floor (pre-release
    /// ignored) = [`CompatTier::BelowFloor`]; exactly the bundled version =
    /// [`CompatTier::Same`]; same major and the same minor or one minor newer =
    /// [`CompatTier::Compatible`]; two or more minors newer (or a newer major,
    /// or the `0.0.0` development build) = [`CompatTier::Newer`]; anything else
    /// at or above the floor = [`CompatTier::Older`]. Without a known bundled
    /// version every version at or above the floor is
    /// [`CompatTier::Compatible`].
    pub fn of(
        bundled: Option<&CodexVersion>,
        floor: &CodexVersion,
        candidate: &CodexVersion,
    ) -> CompatTier {
        if candidate.triple() == (0, 0, 0) {
            return CompatTier::Newer;
        }
        if !candidate.meets(floor) {
            return CompatTier::BelowFloor;
        }
        let Some(bundled) = bundled else {
            return CompatTier::Compatible;
        };
        if candidate == bundled {
            return CompatTier::Same;
        }
        if candidate.major != bundled.major {
            return if candidate.major > bundled.major {
                CompatTier::Newer
            } else {
                CompatTier::Older
            };
        }
        match candidate.minor.cmp(&bundled.minor) {
            std::cmp::Ordering::Equal => CompatTier::Compatible,
            std::cmp::Ordering::Greater if candidate.minor == bundled.minor + 1 => {
                CompatTier::Compatible
            }
            std::cmp::Ordering::Greater => CompatTier::Newer,
            std::cmp::Ordering::Less => CompatTier::Older,
        }
    }
}

/// A Codex package in Superba's store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCodex {
    /// Version.
    pub version: CodexVersion,
    /// Target triple (`x86_64-unknown-linux-musl`).
    pub target: String,
    /// `bin/codex` inside the store.
    pub executable: PathBuf,
    /// Size on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// When it was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<Timestamp>,
    /// The app's running app-server uses it.
    #[serde(default)]
    pub in_use: bool,
    /// The app is pinned to it.
    #[serde(default)]
    pub pinned: bool,
}

/// What the app's Codex is pinned to ([`crate::AppCodexSettings::pin`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppCodexPin {
    /// The Codex bundled with the app (`CODEX_CLI_PATH` removed at launch).
    #[default]
    Bundled,
    /// A version from Superba's verified store.
    Version {
        /// Version.
        version: CodexVersion,
    },
    /// A user-chosen executable.
    Path {
        /// Path.
        path: PathBuf,
    },
}

/// One pin candidate on the Updates page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexCandidate {
    /// The release.
    pub release: CodexRelease,
    /// Fit with the app.
    pub tier: CompatTier,
    /// Already in the store.
    #[serde(default)]
    pub stored: bool,
}

/// What a per-OS app update offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppUpdateAction {
    /// Linux: download and verify the package, then open the installer or show
    /// the command (never runs privileged commands).
    LinuxPackage {
        /// The package.
        package: AppPackage,
        /// `sudo apt install ./chatgpt_<ver>_amd64.deb` style command to show.
        command: String,
    },
    /// macOS: the app updates itself (Sparkle); a full package link as fallback.
    MacSparkle {
        /// Full package, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<AppPackage>,
    },
    /// Windows: Microsoft Store page and the `winget` command.
    WindowsStore {
        /// `ms-windows-store://pdp/?ProductId=...`.
        store_url: String,
        /// `winget upgrade --id ... --source msstore`.
        command: String,
    },
    /// Windows: download the MSIX package (OpenAI's server, else the Microsoft
    /// Store CDN, per [`crate::NetworkSettings::windows_app_source`]), verify it
    /// and open it with App Installer, where the user installs it; the Store page
    /// and `winget` stay as alternatives.
    WindowsPackage {
        /// The official package for this machine, when `windows-release.json`
        /// lists one (the Store mirror's file is resolved at download time).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<AppPackage>,
        /// The configured source.
        #[serde(default)]
        source: crate::profile::WindowsAppSource,
        /// `ms-windows-store://pdp/?ProductId=...`, when the product is known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_url: Option<String>,
        /// `winget upgrade --id ... --source msstore`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        /// The machine's native MSIX architecture (`arm64` on ARM64 Windows
        /// even while Superba runs as x64 under emulation).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_arch: Option<String>,
        /// Architecture of the installed app package, from the registry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        installed_arch: Option<String>,
    },
    /// Nothing to do (up to date or unknown).
    None,
}

impl AppUpdateAction {
    /// Windows: the installed app's architecture and the machine's native one
    /// when they differ (`("x64", "arm64")`: the app runs emulated and the
    /// native package can replace it).
    pub fn emulated_install(&self) -> Option<(&str, &str)> {
        match self {
            AppUpdateAction::WindowsPackage {
                native_arch: Some(native),
                installed_arch: Some(installed),
                ..
            } => emulated_arch(installed, native).then_some((installed.as_str(), native.as_str())),
            _ => None,
        }
    }
}

/// An app package built for `installed` runs emulated on a `native` machine
/// (x64 or x86 on arm64, x86 on x64); neutral packages never do.
pub fn emulated_arch(installed: &str, native: &str) -> bool {
    let (i, n) = (installed.to_ascii_lowercase(), native.to_ascii_lowercase());
    i != "neutral"
        && i != n
        && matches!(
            (i.as_str(), n.as_str()),
            ("x64" | "x86", "arm64") | ("x86", "x64")
        )
}

/// The ChatGPT app section of the Updates page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppVersionsSection {
    /// The install in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<AppInstall>,
    /// Latest build for this OS (from the feed or the vendor index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<AppRelease>,
    /// A newer build exists.
    #[serde(default)]
    pub update_available: bool,
    /// What updating means here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AppUpdateAction>,
}

/// The "Codex used by the app" section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppCodexSection {
    /// Pinning is offered on this OS (not on Windows in 0.3).
    #[serde(default)]
    pub supported: bool,
    /// The configured pin.
    #[serde(default)]
    pub pin: AppCodexPin,
    /// Version of the Codex the running app-server uses, when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_version: Option<CodexVersion>,
    /// Its executable (app-server process image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_exe: Option<PathBuf>,
    /// The app's app-server floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<CodexVersion>,
    /// Pin candidates, newest first.
    #[serde(default)]
    pub candidates: Vec<CodexCandidate>,
    /// Packages in the store.
    #[serde(default)]
    pub stored: Vec<StoredCodex>,
    /// The running app-server is not the pinned binary (rc export, bad path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch: Option<LocalizedText>,
}

/// What a local CLI row offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CliUpdateAction {
    /// Superba updates it in place (standalone layout).
    Install {
        /// Target version.
        version: CodexVersion,
    },
    /// Show a command (npm, Homebrew).
    Command {
        /// The command.
        command: String,
    },
    /// Up to date or not updatable.
    None,
}

/// One local Codex CLI binary on the Updates page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliVersionRow {
    /// The binary.
    pub binary: CodexBinary,
    /// Install kind.
    pub kind: CodexInstallKind,
    /// Newest release of the channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<CodexVersion>,
    /// Action.
    pub action: CliUpdateAction,
}

/// One remote host on the Updates page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteVersionRow {
    /// Host.
    pub target_id: TargetId,
    /// Label.
    pub label: String,
    /// Codex version on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<CodexVersion>,
    /// Install kind on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_kind: Option<CodexInstallKind>,
    /// Fit with the local app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<CompatTier>,
    /// Probe error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

/// The Updates page ([`crate::SwitchService::versions`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionsView {
    /// Last successful online check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Timestamp>,
    /// The ChatGPT app.
    #[serde(default)]
    pub app: AppVersionsSection,
    /// The Codex the app uses.
    #[serde(default)]
    pub app_codex: AppCodexSection,
    /// Local CLI binaries (PATH order).
    #[serde(default)]
    pub cli: Vec<CliVersionRow>,
    /// Remote hosts the app is connected to.
    #[serde(default)]
    pub remote: Vec<RemoteVersionRow>,
    /// Notes (offline, feed stale, bundled Codex moved ahead of the pin).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

impl VersionsView {
    /// The Codex the app should run for the configured pin: the stored
    /// version's executable, the custom path, else the bundled Codex (`None`
    /// when unknown, e.g. a pinned version not downloaded yet). The full path
    /// shown next to [`AppCodexSection::mismatch`], which names it briefly.
    pub fn expected_codex(&self) -> Option<PathBuf> {
        let pin = &self.app_codex;
        match &pin.pin {
            AppCodexPin::Version { version } => pin
                .stored
                .iter()
                .find(|s| &s.version == version)
                .map(|s| s.executable.clone()),
            AppCodexPin::Path { path } => Some(path.clone()),
            _ => self
                .app
                .install
                .as_ref()
                .and_then(|i| i.bundled_codex.clone()),
        }
    }
}

/// Where [`crate::SwitchService::install_codex`] installs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstallTarget {
    /// The local standalone CLI install (release dir + `current` + symlink).
    Local,
    /// Superba's store only (for pinning).
    Store,
    /// A remote host (uploaded over SSH stdin into `~/.local/bin`).
    Remote {
        /// Host.
        target_id: TargetId,
    },
}

/// A Codex install request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    /// Version.
    pub version: CodexVersion,
    /// Where.
    pub target: InstallTarget,
}

/// Result of a Codex install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    /// Version installed.
    pub version: CodexVersion,
    /// Where.
    pub target: InstallTarget,
    /// Installed executable (remote paths are POSIX strings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// The version it replaced (kept for rollback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<CodexVersion>,
    /// Notes (restart needed, another codex earlier on the user's PATH).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// A downloaded, verified app package ([`crate::SwitchService::download_app_update`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDownloadReport {
    /// The package.
    pub package: AppPackage,
    /// Local file.
    pub path: PathBuf,
    /// sha256 of the file (checked against the package's).
    pub sha256: String,
    /// Command to install it (shown, never run with privileges).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Windows: where the package came from, its identity, signer and the
    /// framework packages to install first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<WindowsAppDownload>,
}

/// Where a Windows app package came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppPackageSource {
    /// OpenAI's download server
    /// (`persistent.oaistatic.com/codex-app-prod/windows-release.json`).
    Official,
    /// The Microsoft Store CDN (`*.dl.delivery.mp.microsoft.com`), its
    /// temporary links resolved by store.rg-adguard.net, a third-party resolver.
    StoreMirror,
}

impl AppPackageSource {
    /// Stable code (`official`, `store`), as `spb app download --source` takes it.
    pub fn code(self) -> &'static str {
        match self {
            AppPackageSource::Official => "official",
            AppPackageSource::StoreMirror => "store",
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            AppPackageSource::Official => LocalizedText::new(
                "app_source.official",
                "OpenAI download server (persistent.oaistatic.com)",
            ),
            AppPackageSource::StoreMirror => LocalizedText::new(
                "app_source.store_mirror",
                "Microsoft Store CDN, link resolved by store.rg-adguard.net (third-party)",
            ),
        }
    }
}

/// Identity of an MSIX / APPX package, from its `AppxManifest.xml` (or the
/// bundle's `AppxBundleManifest.xml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsixIdentity {
    /// Package name (`OpenAI.Codex`).
    pub name: String,
    /// Package version (`26.1013.4894.0`).
    pub version: String,
    /// Processor architecture (`x64`, `arm64`, `neutral`).
    pub arch: String,
    /// Publisher distinguished name (`CN=50BDFD77-...`).
    pub publisher: String,
    /// Publisher id derived from [`MsixIdentity::publisher`] (`2p2nqsd0c76g0`).
    pub publisher_id: String,
    /// A framework package (VCLibs and the like).
    #[serde(default)]
    pub framework: bool,
}

impl MsixIdentity {
    /// `Name_PublisherId`.
    pub fn family_name(&self) -> String {
        format!("{}_{}", self.name, self.publisher_id)
    }
}

/// A package dependency declared in an `AppxManifest.xml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsixDependency {
    /// Package name (`Microsoft.VCLibs.140.00.UWPDesktop`).
    pub name: String,
    /// Lowest acceptable version.
    pub min_version: String,
    /// Publisher distinguished name.
    pub publisher: String,
}

/// An MSIX / APPX package registered on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPackage {
    /// Package name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Architecture (`x64`, `arm64`, `x86`, `neutral`).
    pub arch: String,
    /// Publisher id (`8wekyb3d8bbwe`).
    pub publisher_id: String,
}

/// The signer of a package whose signature Windows accepted
/// (`WinVerifyTrust`: chain to a trusted root, timestamp honoured).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSigner {
    /// Subject distinguished name of the signing certificate.
    pub subject: String,
    /// Issuer distinguished name of the signing certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

/// One verified package file of a Windows app download (the app, or a
/// framework package it needs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsPackageFile {
    /// Local file.
    pub path: PathBuf,
    /// Identity from its manifest.
    pub identity: MsixIdentity,
    /// Size in bytes.
    pub size: u64,
    /// sha256 (hex) of the file.
    pub sha256: String,
    /// sha1 (hex) the source listed and the file matched (Store mirror).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    /// The verified signer (set once the signature was checked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<PackageSigner>,
}

/// The Windows part of an [`AppDownloadReport`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsAppDownload {
    /// Where the package came from.
    pub source: AppPackageSource,
    /// Why the sources tried before failed (the official server, when the
    /// Store mirror was used as the fallback).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_reasons: Vec<String>,
    /// The app package.
    pub package: WindowsPackageFile,
    /// The app release the package belongs to (`26.1013.40934`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    /// Framework packages that are not installed yet, in install order: open
    /// (install) these before the app.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<WindowsPackageFile>,
    /// Dependencies that are neither installed nor downloadable here (App
    /// Installer tries to get them from the Microsoft Store).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<MsixDependency>,
}

/// What [`crate::SwitchService::download_app_package`] downloads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDownloadRequest {
    /// Windows: the source for this download (`None`: the network setting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::profile::WindowsAppSource>,
}

/// What [`VersionStore::download_windows_app`] downloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsAppRequest {
    /// Sources to try.
    pub source: crate::profile::WindowsAppSource,
    /// MSIX architecture to download for: the machine's native one (`arm64`
    /// on ARM64 Windows, where an x64 package would run emulated); an x64
    /// package only when no arm64 one is offered.
    pub arch: String,
    /// Packages registered on this machine (framework dependencies already
    /// installed are not downloaded).
    pub installed: Vec<InstalledPackage>,
}

/// Stage of a package download.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DownloadStage {
    /// Looking up the package (release JSON, Store mirror listing).
    Resolving,
    /// Transferring bytes.
    Downloading,
    /// Checking size, checksum, manifest and signature.
    Verifying,
}

/// One progress report of a package download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Stage.
    pub stage: DownloadStage,
    /// Source in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AppPackageSource>,
    /// File being downloaded (empty while resolving).
    #[serde(default)]
    pub file: String,
    /// Bytes on disk.
    #[serde(default)]
    pub done: u64,
    /// Total bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Bytes that were already on disk when this transfer started (resumed).
    #[serde(default)]
    pub resumed_from: u64,
    /// 1-based number of this file among `count` (dependencies, then the app).
    #[serde(default)]
    pub index: usize,
    /// Number of files.
    #[serde(default)]
    pub count: usize,
}

impl DownloadProgress {
    /// A report for `stage` with nothing transferred yet.
    pub fn stage(stage: DownloadStage, source: Option<AppPackageSource>) -> Self {
        Self {
            stage,
            source,
            file: String::new(),
            done: 0,
            total: None,
            resumed_from: 0,
            index: 0,
            count: 0,
        }
    }

    /// Fraction done (0..=1), when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done.min(total) as f64 / total as f64) as f32)
    }
}

/// Receives [`DownloadProgress`] reports. Must not block. A download asked to
/// stop ends with [`Error::Cancelled`] and keeps its partial file, so the next
/// attempt resumes it.
pub trait DownloadProgressSink: Send + Sync {
    /// A new report.
    fn report(&self, progress: DownloadProgress);

    /// The user asked to stop. Default: never.
    fn cancel_requested(&self) -> bool {
        false
    }
}

/// A [`DownloadProgressSink`] that drops every report and never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDownloadProgress;

impl DownloadProgressSink for NoDownloadProgress {
    fn report(&self, _progress: DownloadProgress) {}
}

/// A [`DownloadProgressSink`] keeping the latest report and a cancel flag that
/// the UI reads and sets.
#[derive(Debug, Default)]
pub struct SharedDownloadProgress {
    latest: std::sync::Mutex<Option<DownloadProgress>>,
    cancel: std::sync::atomic::AtomicBool,
}

impl SharedDownloadProgress {
    /// The latest report.
    pub fn latest(&self) -> Option<DownloadProgress> {
        self.latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Ask the download to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// A stop was asked for.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl DownloadProgressSink for SharedDownloadProgress {
    fn report(&self, progress: DownloadProgress) {
        *self.latest.lock().unwrap_or_else(|p| p.into_inner()) = Some(progress);
    }

    fn cancel_requested(&self) -> bool {
        self.cancelled()
    }
}

/// The official standalone Codex layout of one machine: releases under
/// `<codex_home>/packages/standalone/releases/<ver>-<target>/`, the `current`
/// link next to them and `<bin_dir>/codex` (`${CODEX_INSTALL_DIR:-~/.local/bin}`)
/// pointing at `current/bin/codex`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandaloneLayout {
    /// The Codex home whose `packages/standalone/` holds the releases.
    pub codex_home: PathBuf,
    /// Directory of the visible `codex` command.
    pub bin_dir: PathBuf,
}

impl StandaloneLayout {
    /// `<codex_home>/packages/standalone`.
    pub fn root(&self) -> PathBuf {
        self.codex_home.join("packages").join("standalone")
    }

    /// `<bin_dir>/codex` (`codex.exe` on Windows).
    pub fn command(&self) -> PathBuf {
        self.bin_dir
            .join(if cfg!(windows) { "codex.exe" } else { "codex" })
    }
}

/// Codex package target triple for an OS (`std::env::consts::OS` style) and
/// architecture (`std::env::consts::ARCH` style): `linux`/`x86_64` ->
/// `x86_64-unknown-linux-musl`. Remote probes pass `uname -s` / `uname -m`
/// lowercased (`darwin`, `arm64`).
pub fn codex_package_target(os: &str, arch: &str) -> Option<&'static str> {
    let arch = match arch.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" | "x64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => return None,
    };
    let target = match (os.to_ascii_lowercase().as_str(), arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos" | "darwin", "x86_64") => "x86_64-apple-darwin",
        ("macos" | "darwin", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => return None,
    };
    Some(target)
}

/// The Codex package target of this machine.
pub fn local_codex_package_target() -> Option<&'static str> {
    codex_package_target(std::env::consts::OS, std::env::consts::ARCH)
}

/// Codex releases, verified downloads, the local Codex store and app packages
/// (`superba-versions`' `HttpVersionStore`). Every download is verified (size,
/// sha256 from the signed feed / `codex-package_SHA256SUMS`, or npm sha512
/// integrity) before anything is extracted; third-party proxies only when
/// [`crate::NetworkSettings::third_party_proxies`] is on. Never runs privileged
/// commands.
#[async_trait]
pub trait VersionStore: Send + Sync {
    /// Codex releases, newest first (feed -> npm dist-tags -> GitHub latest
    /// redirect; cached, `refresh` forces an online check).
    async fn codex_releases(&self, refresh: bool) -> Result<Vec<CodexRelease>>;

    /// The newest desktop app build for this OS, if known.
    async fn app_latest(&self, refresh: bool) -> Result<Option<AppRelease>>;

    /// Packages in the store.
    async fn stored(&self) -> Result<Vec<StoredCodex>>;

    /// Make `version` available in the store for this machine's target (download,
    /// verify, extract, check `bin/codex --version`); returns it.
    async fn ensure(&self, version: &CodexVersion) -> Result<StoredCodex>;

    /// The verified package archive of `version` for `target` (remote upload).
    async fn package_path(&self, version: &CodexVersion, target: &str) -> Result<PathBuf>;

    /// Remove stored versions beyond the retention (keeps `keep` and the newest
    /// three); returns what was removed.
    async fn prune(&self, keep: &[CodexVersion]) -> Result<Vec<CodexVersion>>;

    /// Download and verify an app package into `<data dir>/downloads/`.
    async fn download_app(&self, package: &AppPackage) -> Result<AppDownloadReport>;

    /// Windows: download the ChatGPT MSIX for this machine's architecture from
    /// the sources of `request` (resumable, into `<data dir>/downloads/`), check
    /// its size, checksum and manifest identity, and the framework packages it
    /// needs that are not installed. The caller checks the signatures
    /// ([`crate::AppController::verify_package_signature`]) before opening any of
    /// them. A cancel through `progress` keeps the partial file.
    async fn download_windows_app(
        &self,
        request: WindowsAppRequest,
        progress: std::sync::Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        let _ = (request, progress);
        Err(Error::not_implemented("VersionStore::download_windows_app"))
    }

    /// Install `version` (made available with [`VersionStore::ensure`] first) into
    /// the local standalone layout: a new release directory, then the `current`
    /// and `<bin_dir>/codex` links swapped; the release it replaces is kept for
    /// [`VersionStore::rollback_standalone`]. npm and Homebrew installs are never
    /// touched (the caller shows their update command instead).
    async fn install_standalone(
        &self,
        version: &CodexVersion,
        layout: &StandaloneLayout,
    ) -> Result<InstallReport>;

    /// Switch the local standalone layout back to the release kept by the last
    /// [`VersionStore::install_standalone`].
    async fn rollback_standalone(&self, layout: &StandaloneLayout) -> Result<InstallReport>;
}
