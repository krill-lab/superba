//! The ChatGPT app package on Windows: download (the version store), the
//! signature check of every file (the app controller's `WinVerifyTrust`,
//! judged by `superba_versions::winapp::trust`), and opening it with App
//! Installer, where the user installs it. After the install, the registry
//! based MSIX discovery finds the new version on the next refresh; the
//! Updates page then offers to reopen the app through Superba (hot switching
//! needs the app started by Superba).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use superba_core::{
    AppDownloadReport, AppDownloadRequest, AppInstall, AppPackageKind, DownloadProgress,
    DownloadProgressSink, DownloadStage, Error, InstalledPackage, PackageSigner, Result,
    WindowsAppRequest, WindowsPackageFile,
};
use superba_versions::winapp::trust::{self, SignerRole};
use superba_versions::winapp::{OPENAI_PUBLISHER_ID, PACKAGE_NAME, download, msix, version_cmp};

use crate::Switcher;

/// The machine's native architecture and the installed app's (Windows).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsArch {
    /// Native MSIX architecture (`arm64`, `x64`).
    pub native: String,
    /// Architecture of the installed app package, when it is registered.
    pub installed: Option<String>,
}

impl WindowsArch {
    /// `native` and the architecture of the app package registered for
    /// `install` (an MSIX install's id is its family name), else of OpenAI's
    /// `OpenAI.Codex`; the newest version when several are registered.
    pub fn of(native: &str, install: Option<&AppInstall>, packages: &[InstalledPackage]) -> Self {
        let family = |p: &InstalledPackage| format!("{}_{}", p.name, p.publisher_id);
        let pick = |want: &dyn Fn(&InstalledPackage) -> bool| {
            packages
                .iter()
                .filter(|p| want(p))
                .max_by(|a, b| version_cmp(&a.version, &b.version))
                .map(|p| p.arch.to_ascii_lowercase())
        };
        let installed = install
            .and_then(|i| pick(&|p| family(p) == i.id))
            .or_else(|| pick(&|p| p.name == PACKAGE_NAME && p.publisher_id == OPENAI_PUBLISHER_ID));
        Self {
            native: native.to_owned(),
            installed,
        }
    }

    /// The installed app runs emulated (x64 on ARM64).
    pub fn emulated(&self) -> bool {
        self.installed
            .as_deref()
            .is_some_and(|i| superba_core::emulated_arch(i, &self.native))
    }
}

/// Signers already checked, by file (path, size, modification time): a 1 GB
/// package is not verified again when it is opened right after its download.
type SignerCache = HashMap<PathBuf, (u64, Option<SystemTime>, PackageSigner)>;

fn signer_cache() -> &'static Mutex<SignerCache> {
    static CACHE: std::sync::OnceLock<Mutex<SignerCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn file_key(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

impl Switcher {
    /// The signer of `path` (cached while the file is unchanged).
    async fn package_signer(&self, path: &Path) -> Result<PackageSigner> {
        let key = file_key(path);
        if let Some(key) = key
            && let Some((len, mtime, signer)) = signer_cache()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(path)
            && (*len, *mtime) == key
        {
            return Ok(signer.clone());
        }
        let signer = self.deps.app.verify_package_signature(path).await?;
        if let Some((len, mtime)) = key {
            signer_cache()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(path.to_path_buf(), (len, mtime, signer.clone()));
        }
        Ok(signer)
    }

    /// Check the signature of one downloaded file; a refused file is deleted.
    async fn check_file(&self, file: &mut WindowsPackageFile, role: SignerRole) -> Result<()> {
        let checked = match self.package_signer(&file.path).await {
            Ok(signer) => trust::check_signer(&file.identity, &signer, role).map(|()| signer),
            Err(e) => Err(e),
        };
        match checked {
            Ok(signer) => {
                file.signer = Some(signer);
                Ok(())
            }
            Err(e) => {
                download::remove_all(&file.path);
                Err(e)
            }
        }
    }

    /// Windows: download the app package (the network setting's sources unless
    /// `request` names one), with the framework packages it still needs, and
    /// check every signature. Public for tests; `download_app_package` uses it
    /// on Windows.
    pub async fn download_windows_app(
        &self,
        request: AppDownloadRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        let set = self.deps.profiles.load().await?;
        let source = request
            .source
            .unwrap_or(set.settings.network.windows_app_source);
        let installed = self.deps.app.installed_packages().await.unwrap_or_default();
        let store = (self.deps.versions)(&set.settings.network);
        // The native architecture: arm64 on ARM64 Windows even while this x64
        // build runs emulated (an x64 package only when no arm64 one is offered).
        let arch = superba_appctl::package::native_arch().to_owned();
        let mut report = store
            .download_windows_app(
                WindowsAppRequest {
                    source,
                    installed,
                    arch,
                },
                progress.clone(),
            )
            .await?;
        let Some(win) = report.windows.as_mut() else {
            return Err(Error::other(
                "the package download returned no Windows details",
            ));
        };
        progress.report(DownloadProgress {
            file: win
                .package
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            done: win.package.size,
            total: Some(win.package.size),
            ..DownloadProgress::stage(DownloadStage::Verifying, Some(win.source))
        });
        self.check_file(&mut win.package, SignerRole::App).await?;
        let mut kept = Vec::new();
        for mut dep in std::mem::take(&mut win.dependencies) {
            match self.check_file(&mut dep, SignerRole::Dependency).await {
                Ok(()) => kept.push(dep),
                Err(e) => {
                    tracing::warn!(file = %dep.path.display(), error = %e, "dependency refused");
                    win.unresolved.push(superba_core::MsixDependency {
                        name: dep.identity.name.clone(),
                        min_version: dep.identity.version.clone(),
                        publisher: dep.identity.publisher.clone(),
                    });
                }
            }
        }
        win.dependencies = kept;
        Ok(report)
    }

    /// Windows: open a downloaded package with App Installer after checking
    /// that it is one of Superba's downloads and that its manifest and
    /// signature are still right. Public for tests; `open_app_package` uses it.
    pub async fn open_windows_package(&self, path: PathBuf) -> Result<()> {
        let downloads = self.deps.ctx.data_dir.downloads_dir();
        let inside = match (path.canonicalize(), downloads.canonicalize()) {
            (Ok(p), Ok(d)) => p.parent() == Some(d.as_path()),
            _ => false,
        };
        let kind = path
            .file_name()
            .and_then(|n| AppPackageKind::windows_from_file_name(&n.to_string_lossy()));
        if !inside || kind.is_none() {
            return Err(Error::Invalid {
                message: format!(
                    "{} is not a package Superba downloaded into {}",
                    path.display(),
                    downloads.display()
                ),
            });
        }
        let arch = superba_appctl::package::native_arch();
        let p = path.clone();
        let a = arch.to_owned();
        let manifest = tokio::task::spawn_blocking(move || msix::read_manifest(&p, &a))
            .await
            .map_err(|e| Error::other(format!("background task failed: {e}")))??;
        let role = if manifest.identity.name == PACKAGE_NAME {
            trust::check_app_identity(&manifest, arch, None)?;
            SignerRole::App
        } else {
            SignerRole::Dependency
        };
        let signer = self.package_signer(&path).await?;
        trust::check_signer(&manifest.identity, &signer, role)?;
        tracing::info!(file = %path.display(), "opening the package with App Installer");
        self.deps.app.open_package_installer(&path).await
    }

    pub(crate) async fn download_app_package_impl(
        &self,
        request: AppDownloadRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        if cfg!(windows) {
            self.download_windows_app(request, progress).await
        } else {
            self.download_app_update_impl().await
        }
    }

    pub(crate) async fn open_app_package_impl(&self, path: PathBuf) -> Result<()> {
        if cfg!(windows) {
            self.open_windows_package(path).await
        } else {
            Err(Error::Unsupported {
                what: "opening app packages with App Installer outside Windows".into(),
            })
        }
    }
}
