//! The Windows app package in the switch service: the configured or requested
//! source reaches the version store with the installed packages, every file's
//! signature is checked (a refused app is deleted, a refused dependency is
//! dropped), the signer is cached for the open that follows, and only
//! Superba's own verified downloads are handed to App Installer.

mod fakes;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use superba_core::*;
use superba_switch::Switcher;
use superba_versions::winapp::{MICROSOFT_PUBLISHER, OPENAI_STORE_PUBLISHER};

const MARKETPLACE: &str = "C=US, S=Washington, L=Redmond, O=Microsoft Corporation, OU=AOC, CN=Microsoft Marketplace CA G 024";
const MS_PCA: &str =
    "C=US, S=Washington, L=Redmond, O=Microsoft Corporation, CN=Microsoft Code Signing PCA 2011";
const VCLIBS: &str = "Microsoft.VCLibs.140.00.UWPDesktop";

/// The fake app controller plus the package methods.
struct PkgApp {
    inner: Arc<fakes::FakeApp>,
    /// Signer by file name (`None`: the signature is not accepted).
    signers: Mutex<Vec<(String, Option<PackageSigner>)>>,
    verified: Mutex<Vec<PathBuf>>,
    opened: Mutex<Vec<PathBuf>>,
    installed: Vec<InstalledPackage>,
}

#[async_trait]
impl AppController for PkgApp {
    async fn detect_installs(&self) -> Result<Vec<AppInstall>> {
        self.inner.detect_installs().await
    }
    async fn codex_binaries(&self) -> Result<Vec<CodexBinary>> {
        self.inner.codex_binaries().await
    }
    async fn running_session(&self) -> Result<Option<AppSession>> {
        self.inner.running_session().await
    }
    async fn launch(&self, options: &LaunchOptions) -> Result<AppSession> {
        self.inner.launch(options).await
    }
    async fn probe_bridge(&self, session: &AppSession) -> Result<BridgeStatus> {
        self.inner.probe_bridge(session).await
    }
    async fn hot_restart(
        &self,
        s: &AppSession,
        r: &HotRestartRequest,
    ) -> Result<HotRestartOutcome> {
        self.inner.hot_restart(s, r).await
    }
    async fn quit(&self, s: &AppSession, o: &QuitOptions) -> Result<QuitOutcome> {
        self.inner.quit(s, o).await
    }
    async fn processes(&self, home: &CodexHome) -> Result<Vec<ProcessInfo>> {
        self.inner.processes(home).await
    }
    async fn turn_activity(
        &self,
        home: &CodexHome,
        s: Option<&AppSession>,
    ) -> Result<TurnActivity> {
        self.inner.turn_activity(home, s).await
    }
    async fn renderer_state(&self, s: &AppSession) -> Result<RendererState> {
        self.inner.renderer_state(s).await
    }
    async fn return_to_thread(&self, s: &AppSession, t: &ThreadRef) -> Result<()> {
        self.inner.return_to_thread(s, t).await
    }
    async fn restart_daemon(&self, home: &CodexHome) -> Result<()> {
        self.inner.restart_daemon(home).await
    }
    async fn stop_daemon(&self, home: &CodexHome) -> Result<()> {
        self.inner.stop_daemon(home).await
    }
    async fn remote_connections(&self, s: &AppSession) -> Result<Vec<RemoteHost>> {
        self.inner.remote_connections(s).await
    }
    async fn launcher_entry(&self) -> Result<bool> {
        self.inner.launcher_entry().await
    }
    async fn set_launcher_entry(&self, enabled: bool) -> Result<()> {
        self.inner.set_launcher_entry(enabled).await
    }
    async fn app_server_exe(&self, s: &AppSession) -> Result<Option<PathBuf>> {
        self.inner.app_server_exe(s).await
    }
    async fn app_server_floor(&self, i: &AppInstall) -> Result<CodexVersion> {
        self.inner.app_server_floor(i).await
    }
    async fn codex_debug_models(&self, b: &Path, c: Option<&Path>) -> Result<String> {
        self.inner.codex_debug_models(b, c).await
    }
    async fn cli_install_kind(&self, b: &Path) -> Result<CodexInstallKind> {
        self.inner.cli_install_kind(b).await
    }
    async fn installed_packages(&self) -> Result<Vec<InstalledPackage>> {
        Ok(self.installed.clone())
    }
    async fn verify_package_signature(&self, path: &Path) -> Result<PackageSigner> {
        self.verified.lock().unwrap().push(path.to_path_buf());
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match self
            .signers
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| *n == name)
        {
            Some((_, Some(s))) => Ok(s.clone()),
            _ => Err(Error::Invalid {
                message: format!("Windows did not accept the signature of {name}"),
            }),
        }
    }
    async fn open_package_installer(&self, path: &Path) -> Result<()> {
        self.opened.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

/// A version store whose Windows download returns `report` (files written by
/// the test) and records the request.
struct PkgStore {
    inner: Arc<fakes::v3::FakeVersions>,
    report: Mutex<Option<Result<AppDownloadReport>>>,
    requests: Mutex<Vec<WindowsAppRequest>>,
}

#[async_trait]
impl VersionStore for PkgStore {
    async fn codex_releases(&self, refresh: bool) -> Result<Vec<CodexRelease>> {
        self.inner.codex_releases(refresh).await
    }
    async fn app_latest(&self, refresh: bool) -> Result<Option<AppRelease>> {
        self.inner.app_latest(refresh).await
    }
    async fn stored(&self) -> Result<Vec<StoredCodex>> {
        self.inner.stored().await
    }
    async fn ensure(&self, v: &CodexVersion) -> Result<StoredCodex> {
        self.inner.ensure(v).await
    }
    async fn package_path(&self, v: &CodexVersion, t: &str) -> Result<PathBuf> {
        self.inner.package_path(v, t).await
    }
    async fn prune(&self, keep: &[CodexVersion]) -> Result<Vec<CodexVersion>> {
        self.inner.prune(keep).await
    }
    async fn download_app(&self, p: &AppPackage) -> Result<AppDownloadReport> {
        self.inner.download_app(p).await
    }
    async fn download_windows_app(
        &self,
        request: WindowsAppRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        self.requests.lock().unwrap().push(request);
        progress.report(DownloadProgress::stage(DownloadStage::Resolving, None));
        self.report.lock().unwrap().clone().unwrap()
    }
    async fn install_standalone(
        &self,
        v: &CodexVersion,
        l: &StandaloneLayout,
    ) -> Result<InstallReport> {
        self.inner.install_standalone(v, l).await
    }
    async fn rollback_standalone(&self, l: &StandaloneLayout) -> Result<InstallReport> {
        self.inner.rollback_standalone(l).await
    }
}

struct Setup {
    world: fakes::World,
    app: Arc<PkgApp>,
    store: Arc<PkgStore>,
    switcher: Switcher,
    downloads: PathBuf,
}

fn manifest(name: &str, version: &str, publisher: &str, framework: bool) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <Package xmlns=\"http://schemas.microsoft.com/appx/manifest/foundation/windows10\">\
         <Identity Name=\"{name}\" ProcessorArchitecture=\"x64\" Version=\"{version}\" Publisher=\"{publisher}\" />\
         <Properties><Framework>{framework}</Framework></Properties></Package>"
    )
}

/// A one-entry stored zip holding `AppxManifest.xml`.
fn package_bytes(manifest: &str) -> Vec<u8> {
    let data = manifest.as_bytes();
    let crc = {
        let mut crc = !0_u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    };
    let name = b"AppxManifest.xml";
    let mut out = Vec::new();
    out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    let cd = out.len() as u32;
    out.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(name);
    let cd_len = out.len() as u32 - cd;
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

fn identity(name: &str, version: &str, publisher: &str, framework: bool) -> MsixIdentity {
    MsixIdentity {
        name: name.into(),
        version: version.into(),
        arch: "x64".into(),
        publisher: publisher.into(),
        publisher_id: superba_versions::winapp::publisher_id(publisher),
        framework,
    }
}

fn file(dir: &Path, name: &str, id: MsixIdentity) -> WindowsPackageFile {
    let path = dir.join(name);
    let bytes = package_bytes(&manifest(
        &id.name,
        &id.version,
        &id.publisher,
        id.framework,
    ));
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    WindowsPackageFile {
        path,
        identity: id,
        size: bytes.len() as u64,
        sha256: "ab".repeat(32),
        sha1: None,
        signer: None,
    }
}

fn setup(installed: Vec<InstalledPackage>) -> Setup {
    let world = fakes::World::new();
    let downloads = world.ctx.data_dir.downloads_dir();
    let app = Arc::new(PkgApp {
        inner: world.app.clone(),
        signers: Mutex::new(Vec::new()),
        verified: Mutex::new(Vec::new()),
        opened: Mutex::new(Vec::new()),
        installed,
    });
    let store = Arc::new(PkgStore {
        inner: world.v3.versions.clone(),
        report: Mutex::new(None),
        requests: Mutex::new(Vec::new()),
    });
    let mut deps = world.deps();
    deps.app = app.clone();
    let s = store.clone();
    deps.versions = Arc::new(move |_n: &NetworkSettings| s.clone() as Arc<dyn VersionStore>);
    let switcher = Switcher::new(deps);
    Setup {
        world,
        app,
        store,
        switcher,
        downloads,
    }
}

impl Setup {
    fn app_file(&self) -> WindowsPackageFile {
        file(
            &self.downloads,
            "ChatGPT-26.1013.4894.0-x64.msix",
            identity(
                "OpenAI.Codex",
                "26.1013.4894.0",
                OPENAI_STORE_PUBLISHER,
                false,
            ),
        )
    }

    fn report(&self, package: WindowsPackageFile, deps: Vec<WindowsPackageFile>) {
        *self.store.report.lock().unwrap() = Some(Ok(AppDownloadReport {
            package: AppPackage {
                os: AppOs::Windows,
                kind: AppPackageKind::Msix,
                arch: "x64".into(),
                url: "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix".into(),
                sha256: Some(package.sha256.clone()),
                size: Some(package.size),
                signature: None,
            },
            path: package.path.clone(),
            sha256: package.sha256.clone(),
            command: None,
            windows: Some(WindowsAppDownload {
                source: AppPackageSource::Official,
                fallback_reasons: Vec::new(),
                package,
                release_version: Some("26.1013.40934".into()),
                dependencies: deps,
                unresolved: Vec::new(),
            }),
        }));
    }

    fn sign(&self, file: &str, subject: &str, issuer: &str) {
        self.app.signers.lock().unwrap().push((
            file.into(),
            Some(PackageSigner {
                subject: subject.into(),
                issuer: Some(issuer.into()),
            }),
        ));
    }

    async fn download(&self, source: Option<WindowsAppSource>) -> Result<AppDownloadReport> {
        self.switcher
            .download_windows_app(
                AppDownloadRequest { source },
                Arc::new(SharedDownloadProgress::default()),
            )
            .await
    }
}

#[tokio::test]
async fn the_source_comes_from_the_request_else_the_setting() {
    let installed = vec![InstalledPackage {
        name: VCLIBS.into(),
        version: "14.0.33519.0".into(),
        arch: "x64".into(),
        publisher_id: "8wekyb3d8bbwe".into(),
    }];
    let s = setup(installed.clone());
    let mut set = s.world.profiles.set.lock().unwrap().clone();
    set.settings.network.windows_app_source = WindowsAppSource::StoreMirrorOnly;
    *s.world.profiles.set.lock().unwrap() = set;
    let f = s.app_file();
    s.sign(
        &f.path.file_name().unwrap().to_string_lossy(),
        "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
        MARKETPLACE,
    );
    s.report(f, Vec::new());
    s.download(None).await.unwrap();
    s.download(Some(WindowsAppSource::OfficialOnly))
        .await
        .unwrap();
    let requests = s.store.requests.lock().unwrap().clone();
    assert_eq!(requests[0].source, WindowsAppSource::StoreMirrorOnly);
    assert_eq!(requests[1].source, WindowsAppSource::OfficialOnly);
    assert_eq!(requests[0].installed, installed);
}

#[tokio::test]
async fn a_verified_package_reports_its_signer_and_opens_with_app_installer() {
    let s = setup(Vec::new());
    let f = s.app_file();
    let name = f.path.file_name().unwrap().to_string_lossy().into_owned();
    s.sign(
        &name,
        "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
        MARKETPLACE,
    );
    s.report(f.clone(), Vec::new());
    let r = s.download(None).await.unwrap();
    let signer = r.windows.as_ref().unwrap().package.signer.clone().unwrap();
    assert_eq!(signer.subject, "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");

    s.switcher
        .open_windows_package(f.path.clone())
        .await
        .unwrap();
    assert_eq!(*s.app.opened.lock().unwrap(), vec![f.path.clone()]);
    // The signature was checked once: the open used the cached signer.
    assert_eq!(s.app.verified.lock().unwrap().len(), 1);

    // Changed on disk since: checked again, and refused now.
    let mut bytes = std::fs::read(&f.path).unwrap();
    bytes.extend_from_slice(b"tail");
    std::fs::write(&f.path, bytes).unwrap();
    s.app.signers.lock().unwrap().clear();
    assert!(
        s.switcher
            .open_windows_package(f.path.clone())
            .await
            .is_err()
    );
    assert_eq!(s.app.opened.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_package_signed_by_anyone_else_is_refused_and_deleted() {
    for (subject, issuer) in [
        ("CN=Evil", MARKETPLACE),
        ("CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B", "CN=Evil CA"),
    ] {
        let s = setup(Vec::new());
        let f = s.app_file();
        s.sign(
            &f.path.file_name().unwrap().to_string_lossy(),
            subject,
            issuer,
        );
        s.report(f.clone(), Vec::new());
        let e = s.download(None).await.unwrap_err();
        assert!(matches!(e, Error::Invalid { .. }), "{e:?}");
        assert!(!f.path.exists(), "{subject}");
    }
    // Not accepted by Windows at all.
    let s = setup(Vec::new());
    let f = s.app_file();
    s.report(f.clone(), Vec::new());
    let e = s.download(None).await.unwrap_err();
    assert!(e.detail().contains("did not accept the signature"), "{e:?}");
    assert!(!f.path.exists());
}

#[tokio::test]
async fn a_refused_dependency_is_dropped_and_reported_as_unresolved() {
    let s = setup(Vec::new());
    let app = s.app_file();
    let good = file(
        &s.downloads,
        "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
        identity(VCLIBS, "14.0.33728.0", MICROSOFT_PUBLISHER, true),
    );
    let bad = file(
        &s.downloads,
        "Microsoft.UI.Xaml.2.8_8.2310.30001.0_x64__8wekyb3d8bbwe.appx",
        identity(
            "Microsoft.UI.Xaml.2.8",
            "8.2310.30001.0",
            MICROSOFT_PUBLISHER,
            true,
        ),
    );
    s.sign(
        &app.path.file_name().unwrap().to_string_lossy(),
        OPENAI_STORE_PUBLISHER,
        MARKETPLACE,
    );
    s.sign(
        &good.path.file_name().unwrap().to_string_lossy(),
        "C=US, S=Washington, L=Redmond, O=Microsoft Corporation, CN=Microsoft Corporation",
        MS_PCA,
    );
    s.sign(
        &bad.path.file_name().unwrap().to_string_lossy(),
        "CN=Evil",
        MS_PCA,
    );
    s.report(app, vec![good.clone(), bad.clone()]);
    let r = s.download(None).await.unwrap();
    let w = r.windows.unwrap();
    assert_eq!(w.dependencies.len(), 1);
    assert_eq!(w.dependencies[0].identity.name, VCLIBS);
    assert!(w.dependencies[0].signer.is_some());
    assert_eq!(w.unresolved.len(), 1);
    assert_eq!(w.unresolved[0].name, "Microsoft.UI.Xaml.2.8");
    assert!(!bad.path.exists());
    // A framework package opens too (with its own signer rule).
    s.switcher
        .open_windows_package(good.path.clone())
        .await
        .unwrap();
    assert_eq!(*s.app.opened.lock().unwrap(), vec![good.path]);
}

#[tokio::test]
async fn only_superbas_own_downloads_are_opened() {
    let s = setup(Vec::new());
    let elsewhere = s.world.tmp.path().join("ChatGPT-x64.msix");
    std::fs::write(
        &elsewhere,
        package_bytes(&manifest(
            "OpenAI.Codex",
            "1.0.0.0",
            OPENAI_STORE_PUBLISHER,
            false,
        )),
    )
    .unwrap();
    s.sign("ChatGPT-x64.msix", OPENAI_STORE_PUBLISHER, MARKETPLACE);
    let e = s
        .switcher
        .open_windows_package(elsewhere)
        .await
        .unwrap_err();
    assert!(
        e.detail().contains("not a package Superba downloaded"),
        "{e:?}"
    );
    std::fs::create_dir_all(&s.downloads).unwrap();
    let exe = s.downloads.join("setup.exe");
    std::fs::write(&exe, b"MZ").unwrap();
    assert!(s.switcher.open_windows_package(exe).await.is_err());
    let missing = s.downloads.join("gone.msix");
    assert!(s.switcher.open_windows_package(missing).await.is_err());
    // Someone else's package in the downloads folder: its identity is refused.
    let other = file(
        &s.downloads,
        "Other.App_1.0.0.0_x64__abcdefghjkmnp.msix",
        identity("Other.App", "1.0.0.0", "CN=Other", false),
    );
    s.sign(
        "Other.App_1.0.0.0_x64__abcdefghjkmnp.msix",
        "CN=Other",
        MS_PCA,
    );
    assert!(s.switcher.open_windows_package(other.path).await.is_err());
    assert!(s.app.opened.lock().unwrap().is_empty());
}
