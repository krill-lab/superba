//! `spb app download` on Windows (over the fake service): `--source` picks the
//! source for one run, the report names the source, the package, its signer
//! and what to install first, and `--open` hands the dependencies, then the
//! app, to App Installer through the service (never installed by Superba).

mod common;

use std::path::PathBuf;

use common::*;
use superba_core::*;

fn file(name: &str, id: &str, version: &str, publisher: &str) -> WindowsPackageFile {
    WindowsPackageFile {
        path: PathBuf::from(format!("/fake/downloads/{name}")),
        identity: MsixIdentity {
            name: id.into(),
            version: version.into(),
            arch: "x64".into(),
            publisher: publisher.into(),
            publisher_id: "2p2nqsd0c76g0".into(),
            framework: false,
        },
        size: 948_993_815,
        sha256: "e2".repeat(32),
        sha1: Some("29f6b5fe52f278cb0a9837338820443d153dedb6".into()),
        signer: Some(PackageSigner {
            subject: "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B".into(),
            issuer: Some("CN=Microsoft Marketplace CA G 024, O=Microsoft Corporation".into()),
        }),
    }
}

fn windows_report() -> AppDownloadReport {
    let app = file(
        "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
        "OpenAI.Codex",
        "26.1013.4894.0",
        "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
    );
    let dep = file(
        "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
        "Microsoft.VCLibs.140.00.UWPDesktop",
        "14.0.33728.0",
        "CN=Microsoft Corporation",
    );
    AppDownloadReport {
        package: AppPackage {
            os: AppOs::Windows,
            kind: AppPackageKind::Msix,
            arch: "x64".into(),
            url: "http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/e1f72c97"
                .into(),
            sha256: Some(app.sha256.clone()),
            size: Some(app.size),
            signature: None,
        },
        path: app.path.clone(),
        sha256: app.sha256.clone(),
        command: None,
        windows: Some(WindowsAppDownload {
            source: AppPackageSource::StoreMirror,
            fallback_reasons: vec!["official server (persistent.oaistatic.com): HTTP 503".into()],
            package: app,
            release_version: Some("26.1013.40934".into()),
            dependencies: vec![dep],
            unresolved: vec![MsixDependency {
                name: "Microsoft.UI.Xaml.2.8".into(),
                min_version: "8.2310.30001.0".into(),
                publisher: "CN=Microsoft Corporation".into(),
            }],
        }),
    }
}

#[tokio::test]
async fn store_source_downloads_reports_and_opens_dependencies_first() {
    let service = FakeService::new();
    service.v3.lock().unwrap().windows_download = Some(windows_report());
    let (code, stdout, _stderr) = invoke(
        service.clone(),
        &["app", "download", "--source", "store", "--open"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    let v3 = service.v3.lock().unwrap().clone();
    assert_eq!(
        v3.app_downloads,
        vec![AppDownloadRequest {
            source: Some(WindowsAppSource::StoreMirrorOnly),
        }]
    );
    assert_eq!(
        v3.opened_packages,
        vec![
            PathBuf::from(
                "/fake/downloads/Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx"
            ),
            PathBuf::from("/fake/downloads/OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix"),
        ]
    );
    for want in [
        "Source: Microsoft Store CDN, link resolved by store.rg-adguard.net (third-party)",
        "Not used: official server (persistent.oaistatic.com): HTTP 503",
        "Package: OpenAI.Codex 26.1013.4894.0 (x64), app release 26.1013.40934",
        "Signed by CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
        "Install first: Microsoft.VCLibs.140.00.UWPDesktop 14.0.33728.0",
        "Needed but not downloaded: Microsoft.UI.Xaml.2.8 8.2310.30001.0 or newer",
        "Opened Microsoft.VCLibs.140.00.UWPDesktop in App Installer: install it first.",
        "Opened App Installer: click Install there.",
        "spb launch --restart",
    ] {
        assert!(stdout.contains(want), "{want} missing:\n{stdout}");
    }
    // Superba never runs an install command for an MSIX.
    assert!(!stdout.contains("Add-AppxPackage") && !stdout.contains("sudo"));
}

#[tokio::test]
async fn official_source_without_open_only_downloads_and_json_is_one_document() {
    let service = FakeService::new();
    service.v3.lock().unwrap().windows_download = Some(windows_report());
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["app", "download", "--source", "official", "--json"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let json = one_json(&stdout);
    assert_eq!(json["windows"]["source"], "store_mirror");
    assert_eq!(
        json["windows"]["package"]["identity"]["name"],
        "OpenAI.Codex"
    );
    let v3 = service.v3.lock().unwrap().clone();
    assert_eq!(
        v3.app_downloads[0].source,
        Some(WindowsAppSource::OfficialOnly)
    );
    assert!(v3.opened_packages.is_empty());

    // No --source: the network setting decides (the service gets none).
    let (code, _, _) = invoke(service.clone(), &["app", "download"]).await;
    assert_eq!(code, 0);
    assert_eq!(service.v3.lock().unwrap().app_downloads[1].source, None);
}

#[tokio::test]
async fn chinese_output_names_the_source_and_the_installer_step() {
    let service = FakeService::new();
    service.v3.lock().unwrap().windows_download = Some(windows_report());
    let (code, stdout, _) = invoke(
        service.clone(),
        &["--locale", "zh-CN", "app", "download", "--open"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    for want in [
        "来源：Microsoft Store CDN，链接由第三方 store.rg-adguard.net 解析",
        "安装包：OpenAI.Codex 26.1013.4894.0（x64），app 版本 26.1013.40934",
        "需先安装：Microsoft.VCLibs.140.00.UWPDesktop 14.0.33728.0",
        "已打开应用安装程序：请在其中点击“安装”。",
    ] {
        assert!(stdout.contains(want), "{want} missing:\n{stdout}");
    }
}

#[test]
fn a_bad_source_value_is_refused_naming_the_valid_ones() {
    use clap::Parser as _;
    let e = superba_cli::Cli::try_parse_from(["spb", "app", "download", "--source", "mirror"])
        .unwrap_err()
        .to_string();
    assert!(e.contains("official") && e.contains("store"), "{e}");
    assert!(
        superba_cli::Cli::try_parse_from(["spb", "app", "download", "--source", "official"])
            .is_ok()
    );
}

#[tokio::test]
async fn app_check_reports_an_emulated_install_and_the_native_package() {
    let service = FakeService::new();
    {
        let mut v3 = service.v3.lock().unwrap();
        v3.versions.app.update_available = false;
        v3.versions.app.action = Some(AppUpdateAction::WindowsPackage {
            package: None,
            source: WindowsAppSource::OfficialFirst,
            store_url: Some("ms-windows-store://pdp/?ProductId=9PLM9XGG6VKS".into()),
            command: Some("winget upgrade --id 9PLM9XGG6VKS --source msstore".into()),
            native_arch: Some("arm64".into()),
            installed_arch: Some("x64".into()),
        });
    }
    let (code, stdout, _) = invoke(service.clone(), &["--locale", "zh-CN", "app", "check"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("已安装 x64 版（仿真运行），可换成 arm64 原生版。"),
        "{stdout}"
    );
    let (_, stdout, _) = invoke(service, &["app", "check"]).await;
    assert!(
        stdout.contains(
            "The x64 build is installed (it runs emulated); the native arm64 build can replace it."
        ),
        "{stdout}"
    );
}
