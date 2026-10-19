#![allow(missing_docs)]
//! Updates page, the ChatGPT app on Windows: download the installer package
//! with progress and cancel, show its source, version, signer and the
//! framework packages to install first, open App Installer (the user installs),
//! offer to reopen ChatGPT through Superba, explain failures; the Store and
//! winget stay offered. Preferences -> Network chooses the source.

mod router_harness;
mod support;

use std::path::PathBuf;
use std::sync::Arc;

use router_harness::Harness;
use superba_core::*;
use superba_gui::pages::updates::{self, State};
use superba_gui::pages::{Action, Page};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn harness(service: Arc<FakeService>, locale: Locale) -> Harness {
    Harness::new([1280.0, 2600.0], locale, service, false)
}

const DOWNLOADS: &str = r"C:\Users\me\AppData\Local\Superba\data\downloads";

fn windows_view(service: &FakeService, source: WindowsAppSource) {
    windows_view_arch(service, source, "x64", Some("x64"));
}

fn windows_view_arch(
    service: &FakeService,
    source: WindowsAppSource,
    native: &str,
    installed: Option<&str>,
) {
    let mut view = service.versions_view();
    view.app.update_available = true;
    view.app.action = Some(AppUpdateAction::WindowsPackage {
        package: Some(AppPackage {
            os: AppOs::Windows,
            kind: AppPackageKind::Msix,
            arch: native.into(),
            url: format!(
                "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-{native}.msix"
            ),
            sha256: None,
            size: Some(if native == "arm64" {
                1_022_040_777
            } else {
                948_993_815
            }),
            signature: None,
        }),
        source,
        store_url: Some("ms-windows-store://pdp/?ProductId=9PLM9XGG6VKS".into()),
        command: Some("winget upgrade --id 9PLM9XGG6VKS --source msstore".into()),
        native_arch: Some(native.into()),
        installed_arch: installed.map(str::to_owned),
    });
    view.app_codex.supported = false;
    service.set_versions(view);
}

fn package_file(name: &str, id: &str, version: &str, publisher: &str) -> WindowsPackageFile {
    WindowsPackageFile {
        path: PathBuf::from(format!(r"{DOWNLOADS}\{name}")),
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

fn report(source: AppPackageSource, deps: bool) -> AppDownloadReport {
    let app = package_file(
        "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
        "OpenAI.Codex",
        "26.1013.4894.0",
        "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
    );
    let dependencies = if deps {
        vec![package_file(
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
            "Microsoft.VCLibs.140.00.UWPDesktop",
            "14.0.33728.0",
            "CN=Microsoft Corporation",
        )]
    } else {
        Vec::new()
    };
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
            source,
            fallback_reasons: if source == AppPackageSource::StoreMirror {
                vec!["official server (persistent.oaistatic.com): HTTP 503".into()]
            } else {
                Vec::new()
            },
            package: app,
            release_version: Some("26.1013.40934".into()),
            dependencies,
            unresolved: if deps {
                vec![MsixDependency {
                    name: "Microsoft.UI.Xaml.2.8".into(),
                    min_version: "8.2310.30001.0".into(),
                    publisher: "CN=Microsoft Corporation".into(),
                }]
            } else {
                Vec::new()
            },
        }),
    }
}

fn loaded(f: &router_harness::Frame) -> bool {
    f.has("下载安装包") || f.has("Download installer")
}

#[test]
fn the_windows_row_offers_the_installer_its_source_and_the_store() {
    let service = fake();
    windows_view(&service, WindowsAppSource::OfficialFirst);
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    for want in [
        "下载安装包（949 MB）",
        "安装包来源",
        "官方优先（失败时用 Store 镜像）",
        "在 Microsoft Store 中打开",
        "winget upgrade --id 9PLM9XGG6VKS --source msstore",
        "Superba 只下载并校验安装包，由你在“应用安装程序”中点击安装；不会静默安装，也不会请求管理员权限。",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    assert!(!f.has("打开安装程序"));
    h.click_text(&f, "更改");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(matches!(
        f.actions[0],
        Action::Go(Page::Superba(superba_gui::pages::SuperbaSection::Network))
    ));

    // English, and the other source settings.
    windows_view(&service, WindowsAppSource::StoreMirrorOnly);
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    assert!(f.has("Download installer (949 MB)"), "{}", f.all_text());
    assert!(f.has("Microsoft Store mirror only"));
}

#[test]
fn a_download_shows_progress_and_can_be_cancelled() {
    let service = fake();
    windows_view(&service, WindowsAppSource::OfficialFirst);
    service.set_windows_download(report(AppPackageSource::Official, false), true);
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.click_text(&f, "下载安装包（949 MB）");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("474.5 / 949.0 MB"),
    );
    assert!(
        f.has("ChatGPT-26.1013.4894.0-x64.msix：474.5 / 949.0 MB（50%）"),
        "{}",
        f.all_text()
    );
    assert!(state.windows.downloading());
    assert_eq!(
        service.app_download_requests(),
        vec![AppDownloadRequest::default()]
    );
    h.click_text(&f, "取消");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("已停止下载"),
    );
    assert!(f.has("已停止下载。再次下载会从中断处继续。"));
    assert!(!state.windows.downloading());
    assert!(state.windows.report.is_none());
    // Downloading again is offered.
    assert!(f.has("下载安装包（949 MB）"));
}

#[test]
fn a_verified_package_is_explained_and_opened_dependencies_first() {
    let service = fake();
    windows_view(&service, WindowsAppSource::OfficialFirst);
    service.set_windows_download(report(AppPackageSource::StoreMirror, true), false);
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.click_text(&f, "下载安装包（949 MB）");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("打开安装程序"),
    );
    for want in [
        "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
        // The release version, the package version labeled (user bug 43).
        "26.1013.40934（包 26.1013.4894.0） · x64",
        "Microsoft Store CDN，链接由第三方 store.rg-adguard.net 解析",
        "store.rg-adguard.net 是第三方链接解析服务",
        "未使用：official server (persistent.oaistatic.com): HTTP 503",
        "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B，已由 Windows 验证",
        "需先安装：Microsoft.VCLibs.140.00.UWPDesktop 14.0.33728.0",
        "还需要 Microsoft.UI.Xaml.2.8 8.2310.30001.0 或更高版本",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    assert!(!f.has("下载安装包（949 MB）"));
    // The dependency's button comes first.
    assert_eq!(f.all_text().matches("打开安装程序").count(), 2);
    let first = f.find("打开安装程序").unwrap();
    h.click_at(first.center());
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| !service.opened_packages().is_empty(),
    );
    assert!(
        service.opened_packages()[0]
            .to_string_lossy()
            .contains("Microsoft.VCLibs")
    );
    assert!(!state.windows.opened.is_empty() || state.windows.busy());
}

#[test]
fn after_app_installer_opens_superba_offers_to_reopen_chatgpt() {
    let service = fake();
    windows_view(&service, WindowsAppSource::OfficialFirst);
    service.set_windows_download(report(AppPackageSource::Official, false), false);
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.click_text(&f, "下载安装包（949 MB）");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("打开安装程序"),
    );
    assert!(f.has("OpenAI 官方下载服务器（persistent.oaistatic.com）"));
    assert!(!f.has("store.rg-adguard.net 是第三方链接解析服务"));
    h.click_text(&f, "打开安装程序");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("重新打开 ChatGPT（通过 Superba）"),
    );
    assert!(f.has("已打开“应用安装程序”：请在其中点击“安装”。"));
    assert_eq!(
        service.opened_packages(),
        vec![PathBuf::from(format!(
            r"{DOWNLOADS}\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix"
        ))]
    );
    h.click_text(&f, "重新打开 ChatGPT（通过 Superba）");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(f.actions.iter().any(|a| matches!(a, Action::RestartApp)));
}

#[test]
fn failures_are_explained() {
    let service = fake();
    windows_view(&service, WindowsAppSource::OfficialFirst);
    service.fail_windows_download(Error::Other {
        message: "no source provided a verified ChatGPT package for Windows x64: official server \
                  (persistent.oaistatic.com): HTTP 503; Microsoft Store via store.rg-adguard.net: \
                  store.rg-adguard.net asked for a browser check (Cloudflare challenge)"
            .into(),
    });
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.click_text(&f, "下载安装包（949 MB）");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("browser check"),
    );
    assert!(f.has("可以在 Superba → 网络 中更换安装包来源，或改用 Microsoft Store。"));
    assert!(!f.has("打开安装程序"));

    // Opening can fail too (App Installer missing).
    let service = fake();
    windows_view(&service, WindowsAppSource::OfficialFirst);
    service.set_windows_download(report(AppPackageSource::Official, false), false);
    service.fail_open_package(Error::Process {
        message: "Windows could not open the package with its installer (App Installer)".into(),
    });
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.click_text(&f, "Download installer (949 MB)");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("Open installer"),
    );
    h.click_text(&f, "Open installer");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("could not open the package"),
    );
    assert!(!f.has("Reopen ChatGPT (through Superba)"));
}

#[test]
fn preferences_choose_the_windows_package_source() {
    let service = fake();
    let mut h = harness(service, Locale::ZhCn);
    let mut settings = Settings::default();
    let f = h.frames(2, |ui, cx, _out| {
        superba_gui::pages::superba::network::windows_app_source_row(ui, cx, &mut settings)
    });
    for want in [
        "ChatGPT 安装包来源",
        "官方优先",
        "仅官方",
        "仅 Store 镜像",
        "第三方的 store.rg-adguard.net",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    assert_eq!(
        settings.network.windows_app_source,
        WindowsAppSource::OfficialFirst
    );
    h.click_text(&f, "仅 Store 镜像");
    h.frames(5, |ui, cx, _out| {
        superba_gui::pages::superba::network::windows_app_source_row(ui, cx, &mut settings)
    });
    assert_eq!(
        settings.network.windows_app_source,
        WindowsAppSource::StoreMirrorOnly
    );
}

#[test]
fn progress_lines_name_the_stage() {
    let service = fake();
    let mut h = harness(service, Locale::En);
    let mut lines = Vec::new();
    h.frames(1, |_ui, cx, _out| {
        lines = vec![
            updates::windows_app::progress_line(
                cx,
                &DownloadProgress::stage(
                    DownloadStage::Resolving,
                    Some(AppPackageSource::StoreMirror),
                ),
            ),
            updates::windows_app::progress_line(
                cx,
                &DownloadProgress {
                    file: "a.msix".into(),
                    done: 600_000_000,
                    total: Some(948_993_815),
                    resumed_from: 400_000_000,
                    index: 2,
                    count: 2,
                    ..DownloadProgress::stage(DownloadStage::Downloading, None)
                },
            ),
            updates::windows_app::progress_line(
                cx,
                &DownloadProgress {
                    file: "a.msix".into(),
                    ..DownloadProgress::stage(DownloadStage::Verifying, None)
                },
            ),
        ];
    });
    assert_eq!(
        lines,
        [
            "Looking up the package: Microsoft Store CDN, link resolved by store.rg-adguard.net (third-party)…",
            "a.msix: 600.0 of 949.0 MB (63%) · resumed at 400.0 MB · 2/2",
            "Verifying a.msix…",
        ]
    );
}

#[test]
fn an_x64_install_on_arm64_offers_the_native_package() {
    let service = fake();
    windows_view_arch(
        &service,
        WindowsAppSource::OfficialFirst,
        "arm64",
        Some("x64"),
    );
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("原生版"),
    );
    for want in [
        "已安装 x64 版（仿真运行），可换成 arm64 原生版。",
        "下载 arm64 原生版安装包（1022 MB）",
        "先在 Windows 设置 → 应用 中卸载 ChatGPT",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // Native installs say nothing about emulation.
    windows_view_arch(
        &service,
        WindowsAppSource::OfficialFirst,
        "arm64",
        Some("arm64"),
    );
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("Download installer"),
    );
    assert!(f.has("Download installer (1022 MB)"), "{}", f.all_text());
    assert!(!f.has("emulated"));
}
