//! Contracts of the Windows app package download: the Preferences source
//! setting (old files without it read as "official first"), the update
//! action, the report, progress and its cancel flag.

use std::path::PathBuf;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use superba_core::*;

fn round_trip<T>(value: &T) -> serde_json::Value
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value);
    json
}

#[test]
fn the_windows_source_defaults_to_official_first_and_orders_the_sources() {
    let old: NetworkSettings = serde_json::from_str(r#"{"online_checks":true}"#).unwrap();
    assert_eq!(old.windows_app_source, WindowsAppSource::OfficialFirst);
    assert!(NetworkSettings::default().is_default());
    let json = round_trip(&NetworkSettings {
        windows_app_source: WindowsAppSource::StoreMirrorOnly,
        ..NetworkSettings::default()
    });
    assert_eq!(json["windows_app_source"], "store_mirror_only");
    assert_eq!(
        WindowsAppSource::OfficialFirst.order(),
        [AppPackageSource::Official, AppPackageSource::StoreMirror]
    );
    assert_eq!(
        WindowsAppSource::OfficialOnly.order(),
        [AppPackageSource::Official]
    );
    assert_eq!(
        WindowsAppSource::StoreMirrorOnly.order(),
        [AppPackageSource::StoreMirror]
    );
    assert_eq!(AppPackageSource::Official.code(), "official");
    assert_eq!(AppPackageSource::StoreMirror.code(), "store");
    assert!(
        AppPackageSource::StoreMirror
            .text()
            .fallback
            .contains("third-party")
    );
}

#[test]
fn windows_package_kinds_come_from_the_file_extension() {
    for (name, kind) in [
        ("ChatGPT-x64.msix", AppPackageKind::Msix),
        (
            "OpenAI.Codex_1.0.0.0_neutral_~_2p2nqsd0c76g0.MSIXBUNDLE",
            AppPackageKind::MsixBundle,
        ),
        (
            "Microsoft.VCLibs.140.00_14.0.33519.0_x64__8wekyb3d8bbwe.appx",
            AppPackageKind::Appx,
        ),
        ("x.appxbundle", AppPackageKind::AppxBundle),
    ] {
        assert_eq!(
            AppPackageKind::windows_from_file_name(name),
            Some(kind),
            "{name}"
        );
        assert!(kind.is_windows_package());
    }
    for name in ["a.BlockMap", "a.eappx", "a.deb", "msix"] {
        assert_eq!(AppPackageKind::windows_from_file_name(name), None, "{name}");
    }
    assert!(!AppPackageKind::Deb.is_windows_package());
}

fn identity() -> MsixIdentity {
    MsixIdentity {
        name: "OpenAI.Codex".into(),
        version: "26.1013.4894.0".into(),
        arch: "x64".into(),
        publisher: "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B".into(),
        publisher_id: "2p2nqsd0c76g0".into(),
        framework: false,
    }
}

#[test]
fn the_windows_action_and_report_round_trip() {
    let action = AppUpdateAction::WindowsPackage {
        package: Some(AppPackage {
            os: AppOs::Windows,
            kind: AppPackageKind::Msix,
            arch: "x64".into(),
            url: "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix"
                .into(),
            sha256: None,
            size: Some(948_993_815),
            signature: None,
        }),
        source: WindowsAppSource::OfficialFirst,
        store_url: Some("ms-windows-store://pdp/?ProductId=9PLM9XGG6VKS".into()),
        command: Some("winget upgrade --id 9PLM9XGG6VKS --source msstore".into()),
    };
    let json = round_trip(&action);
    assert_eq!(json["kind"], "windows_package");
    // Written by an older Superba: only the kind.
    let bare: AppUpdateAction = serde_json::from_str(r#"{"kind":"windows_package"}"#).unwrap();
    assert!(matches!(
        bare,
        AppUpdateAction::WindowsPackage {
            package: None,
            source: WindowsAppSource::OfficialFirst,
            ..
        }
    ));

    assert_eq!(identity().family_name(), "OpenAI.Codex_2p2nqsd0c76g0");
    let file = WindowsPackageFile {
        path: PathBuf::from(
            r"C:\Users\u\AppData\Local\Superba\downloads\ChatGPT-26.1013.4894.0-x64.msix",
        ),
        identity: identity(),
        size: 948_993_815,
        sha256: "e2".repeat(32),
        sha1: Some("29f6b5fe52f278cb0a9837338820443d153dedb6".into()),
        signer: Some(PackageSigner {
            subject: "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B".into(),
            issuer: Some("CN=Microsoft Marketplace CA G 024, O=Microsoft Corporation".into()),
        }),
    };
    let report = AppDownloadReport {
        package: AppPackage {
            os: AppOs::Windows,
            kind: AppPackageKind::Msix,
            arch: "x64".into(),
            url: "http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/e1f72c97"
                .into(),
            sha256: Some("e2".repeat(32)),
            size: Some(948_993_815),
            signature: None,
        },
        path: file.path.clone(),
        sha256: file.sha256.clone(),
        command: None,
        windows: Some(WindowsAppDownload {
            source: AppPackageSource::StoreMirror,
            fallback_reasons: vec!["official server: HTTP 503".into()],
            package: file,
            release_version: Some("26.1013.40934".into()),
            dependencies: Vec::new(),
            unresolved: vec![MsixDependency {
                name: "Microsoft.VCLibs.140.00.UWPDesktop".into(),
                min_version: "14.0.30704.0".into(),
                publisher: "CN=Microsoft Corporation".into(),
            }],
        }),
    };
    let json = round_trip(&report);
    assert_eq!(json["windows"]["source"], "store_mirror");
    // A Linux report has no Windows part at all.
    let linux: AppDownloadReport = serde_json::from_str(
        r#"{"package":{"os":"linux","kind":"deb","arch":"amd64","url":"u"},"path":"/d/c.deb","sha256":"ab"}"#,
    )
    .unwrap();
    assert!(linux.windows.is_none());
    round_trip(&AppDownloadRequest {
        source: Some(WindowsAppSource::OfficialOnly),
    });
    round_trip(&InstalledPackage {
        name: "Microsoft.VCLibs.140.00.UWPDesktop".into(),
        version: "14.0.33519.0".into(),
        arch: "x64".into(),
        publisher_id: "8wekyb3d8bbwe".into(),
    });
}

#[test]
fn progress_has_a_fraction_and_the_shared_sink_carries_the_cancel() {
    let mut p =
        DownloadProgress::stage(DownloadStage::Downloading, Some(AppPackageSource::Official));
    assert_eq!(p.fraction(), None);
    p.done = 250;
    p.total = Some(1000);
    assert_eq!(p.fraction(), Some(0.25));
    p.done = 2000;
    assert_eq!(p.fraction(), Some(1.0));
    round_trip(&p);

    let shared = Arc::new(SharedDownloadProgress::default());
    let sink: Arc<dyn DownloadProgressSink> = shared.clone();
    assert!(shared.latest().is_none());
    sink.report(p.clone());
    assert_eq!(shared.latest(), Some(p));
    assert!(!sink.cancel_requested());
    shared.cancel();
    assert!(sink.cancel_requested() && shared.cancelled());
    assert!(!NoDownloadProgress.cancel_requested());
}
