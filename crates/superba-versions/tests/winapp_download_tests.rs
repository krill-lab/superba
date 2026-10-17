//! Windows app package downloads against a local server: progress, resume
//! with `Range` + `If-Range`, cancel, checksum / size / identity refusal, the
//! source order (official first, the Store mirror as fallback), the mirror
//! lookup and its cache, and framework dependencies.

mod winapp_support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pretty_assertions::assert_eq;
use superba_core::{
    AppOs, AppPackageKind, AppPackageSource, DataDir, DownloadProgress, DownloadProgressSink,
    DownloadStage, Error, InstalledPackage, NetworkSettings, VersionStore, WindowsAppRequest,
    WindowsAppSource,
};
use superba_versions::HttpVersionStore;
use superba_versions::sources::Endpoints;
use superba_versions::winapp::download::{self, Target, part_path};
use superba_versions::winapp::mirror::ListedSize;
use superba_versions::winapp::{MICROSOFT_PUBLISHER, OPENAI_STORE_PUBLISHER};
use winapp_support::{
    Mock, Route, listed_size, listing, manifest, msix, real_manifest, sha1_hex, sha256_hex,
};

/// Records reports; cancels once `cancel_at` bytes are done.
#[derive(Default)]
struct Sink {
    reports: Mutex<Vec<DownloadProgress>>,
    cancel_at: Option<u64>,
}

impl DownloadProgressSink for Sink {
    fn report(&self, p: DownloadProgress) {
        self.reports.lock().unwrap().push(p);
    }

    fn cancel_requested(&self) -> bool {
        self.cancel_at.is_some_and(|at| {
            self.reports
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.stage == DownloadStage::Downloading && r.done >= at)
        })
    }
}

impl Sink {
    fn reports(&self) -> Vec<DownloadProgress> {
        self.reports.lock().unwrap().clone()
    }
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

fn target(mock: &Mock, dir: &Path, body: &[u8], sha1: Option<String>) -> Target {
    Target {
        url: mock.url("/cdn/files/pkg"),
        dest: dir.join("pkg.msix"),
        sha1,
        listed_size: ListedSize::parse(&listed_size(body.len())),
        max: 1 << 30,
        source: AppPackageSource::StoreMirror,
        index: 1,
        count: 1,
    }
}

fn client(mock: &Mock) -> reqwest::Client {
    let base = mock.base.clone();
    download::client(Arc::new(move |u: &str| u.starts_with(&base))).unwrap()
}

#[tokio::test]
async fn a_download_reports_progress_is_checked_and_not_repeated() {
    let mock = Mock::start().await;
    let body = payload(300_000);
    mock.route(
        "/cdn/files/pkg",
        Route::ok(body.clone())
            .etag("\"v1\"")
            .slow(50_000, Duration::from_millis(5)),
    );
    let tmp = tempfile::tempdir().unwrap();
    let t = target(&mock, tmp.path(), &body, Some(sha1_hex(&body)));
    let sink = Sink::default();
    let got = download::fetch(&client(&mock), &t, &sink).await.unwrap();
    assert_eq!(got.size, body.len() as u64);
    assert_eq!(got.sha256, sha256_hex(&body));
    assert_eq!(std::fs::read(&t.dest).unwrap(), body);
    assert!(!part_path(&t.dest).exists());
    let reports = sink.reports();
    let last_dl = reports
        .iter()
        .rfind(|r| r.stage == DownloadStage::Downloading)
        .unwrap();
    assert_eq!((last_dl.done, last_dl.total), (300_000, Some(300_000)));
    assert_eq!(last_dl.file, "pkg.msix");
    assert_eq!(last_dl.source, Some(AppPackageSource::StoreMirror));
    assert!(reports.iter().any(|r| r.stage == DownloadStage::Verifying));
    assert!(
        reports
            .windows(2)
            .all(|w| w[0].done <= w[1].done || w[1].stage != w[0].stage)
    );

    // Already verified: no second transfer.
    download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap();
    assert_eq!(mock.requests("/cdn/files/pkg").len(), 1);
}

#[tokio::test]
async fn a_checksum_mismatch_is_refused_and_the_file_deleted() {
    let mock = Mock::start().await;
    let body = payload(100_000);
    mock.route("/cdn/files/pkg", Route::ok(body.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let t = target(&mock, tmp.path(), &body, Some("0".repeat(40)));
    let e = download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Invalid { message } if message.contains("SHA-1")),
        "{e:?}"
    );
    assert!(!t.dest.exists());
    assert!(!part_path(&t.dest).exists());
}

#[tokio::test]
async fn a_size_other_than_listed_or_an_etag_of_another_file_is_refused_up_front() {
    let mock = Mock::start().await;
    let body = payload(100_000);
    let tmp = tempfile::tempdir().unwrap();
    mock.route("/cdn/files/pkg", Route::ok(payload(400_000)));
    let t = target(&mock, tmp.path(), &body, Some(sha1_hex(&body)));
    let e = download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Invalid { message } if message.contains("listing says")),
        "{e:?}"
    );

    // The Store CDN's ETag is the base64 SHA-1 of the file.
    assert_eq!(
        download::etag_sha1("\"Kfa1/lLyeMsKmDcziCBEPRU97bY=\"").as_deref(),
        Some("29f6b5fe52f278cb0a9837338820443d153dedb6")
    );
    assert_eq!(download::etag_sha1("\"0x8DF2C24054871D5\""), None);
    mock.route(
        "/cdn/files/pkg",
        Route::ok(body.clone()).etag("\"Kfa1/lLyeMsKmDcziCBEPRU97bY=\""),
    );
    let e = download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Invalid { message } if message.contains("the CDN says")),
        "{e:?}"
    );
}

#[tokio::test]
async fn a_broken_transfer_resumes_with_range_and_if_range() {
    let mock = Mock::start().await;
    let body = payload(200_000);
    mock.routes(
        "/cdn/files/pkg",
        vec![
            Route::ok(body.clone()).etag("\"v1\"").cut_after(80_000),
            Route::ok(body.clone()).etag("\"v1\""),
        ],
    );
    let tmp = tempfile::tempdir().unwrap();
    let t = target(&mock, tmp.path(), &body, Some(sha1_hex(&body)));
    let sink = Sink::default();
    download::fetch(&client(&mock), &t, &sink).await.unwrap();
    assert_eq!(std::fs::read(&t.dest).unwrap(), body);
    let seen = mock.requests("/cdn/files/pkg");
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].header("range"), None);
    assert_eq!(seen[1].header("range"), Some("bytes=80000-"));
    assert_eq!(seen[1].header("if-range"), Some("\"v1\""));
    assert!(sink.reports().iter().any(|r| r.resumed_from == 80_000));
}

#[tokio::test]
async fn a_cancel_keeps_the_partial_file_and_the_next_attempt_resumes_it() {
    let mock = Mock::start().await;
    let body = payload(400_000);
    mock.route(
        "/cdn/files/pkg",
        Route::ok(body.clone())
            .etag("\"v1\"")
            .slow(20_000, Duration::from_millis(10)),
    );
    let tmp = tempfile::tempdir().unwrap();
    let t = target(&mock, tmp.path(), &body, Some(sha1_hex(&body)));
    let sink = Sink {
        cancel_at: Some(100_000),
        ..Sink::default()
    };
    let e = download::fetch(&client(&mock), &t, &sink)
        .await
        .unwrap_err();
    assert_eq!(e, Error::Cancelled);
    let kept = std::fs::metadata(part_path(&t.dest)).unwrap().len();
    assert!((100_000..400_000).contains(&kept), "{kept}");
    assert!(!t.dest.exists());

    mock.route("/cdn/files/pkg", Route::ok(body.clone()).etag("\"v1\""));
    download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&t.dest).unwrap(), body);
    let seen = mock.requests("/cdn/files/pkg");
    assert_eq!(
        seen.last().unwrap().header("range"),
        Some(format!("bytes={kept}-").as_str())
    );
}

#[tokio::test]
async fn a_changed_file_restarts_instead_of_mixing_two_versions() {
    let mock = Mock::start().await;
    let old = payload(150_000);
    let new: Vec<u8> = payload(150_000).iter().map(|b| b ^ 0x5a).collect();
    mock.route(
        "/cdn/files/pkg",
        Route::ok(old.clone()).etag("\"old\"").cut_after(60_000),
    );
    let tmp = tempfile::tempdir().unwrap();
    // Without a checksum (the official server publishes none) a stale part is
    // only resumed while the validator still matches.
    let mut t = target(&mock, tmp.path(), &old, None);
    t.listed_size = None;
    let sink = Sink {
        cancel_at: Some(50_000),
        ..Sink::default()
    };
    let _ = download::fetch(&client(&mock), &t, &sink).await;
    assert!(part_path(&t.dest).exists());
    mock.route("/cdn/files/pkg", Route::ok(new.clone()).etag("\"new\""));
    download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&t.dest).unwrap(), new);
    assert_eq!(
        mock.requests("/cdn/files/pkg")
            .last()
            .unwrap()
            .header("if-range"),
        Some("\"old\"")
    );
}

#[tokio::test]
async fn redirects_are_followed_only_to_accepted_hosts() {
    let mock = Mock::start().await;
    let body = payload(10_000);
    mock.route(
        "/cdn/files/pkg",
        Route::status(302).header("Location", "http://localhost:1/elsewhere"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let t = target(&mock, tmp.path(), &body, Some(sha1_hex(&body)));
    let e = download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Http { status: 302, .. }), "{e:?}");
    mock.route(
        "/cdn/files/pkg",
        Route::status(302).header("Location", &mock.url("/cdn/files/real")),
    );
    mock.route("/cdn/files/real", Route::ok(body.clone()));
    download::fetch(&client(&mock), &t, &Sink::default())
        .await
        .unwrap();
}

// --- the whole flow ----------------------------------------------------------

const VCLIBS: &str = "Microsoft.VCLibs.140.00.UWPDesktop";

struct World {
    mock: Mock,
    tmp: tempfile::TempDir,
}

impl World {
    async fn new() -> Self {
        Self {
            mock: Mock::start().await,
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    fn store(&self, source: WindowsAppSource, arch: &str) -> HttpVersionStore {
        let net = NetworkSettings {
            windows_app_source: source,
            ..NetworkSettings::default()
        };
        HttpVersionStore::new(&DataDir::new(self.tmp.path().join("data")), net)
            .with_endpoints(Endpoints {
                app: self.mock.url("/app"),
                store_mirror: self.mock.url("/mirror"),
                store_cdn_extra: Some(self.mock.url("/cdn")),
                ..Endpoints::default()
            })
            .with_platform(None, AppOs::Windows, arch)
    }

    fn official(&self, version: &str, package: &[u8]) {
        let release = format!(
            r#"{{"schemaVersion":1,"releaseDate":"2026-10-17T05:55:44Z","packageVersion":"{version}","packages":[
              {{"architecture":"x64","downloadUrl":"{}"}},
              {{"architecture":"arm64","downloadUrl":"https://evil.example/ChatGPT-arm64.msix"}}]}}"#,
            self.mock
                .url(&format!("/app/releases/{version}/ChatGPT-x64.msix"))
        );
        self.mock
            .route("/app/windows-release.json", Route::ok(release));
        self.mock.route(
            &format!("/app/releases/{version}/ChatGPT-x64.msix"),
            Route::ok(package.to_vec()).etag("\"0x8DF2C24054871D5\""),
        );
        self.mock.route(
            "/app/windows-store-update.json",
            Route::ok(format!(
                r#"{{"schemaVersion":1,"buildVersion":"{version}","storeProductId":"9PLM9XGG6VKS","packageIdentity":"OpenAI.Codex","releaseVersion":"26.1013.40934"}}"#
            )),
        );
    }

    /// The mirror lists `files` (`(file name, bytes)`) served from `/cdn/`.
    fn mirror(&self, files: &[(&str, &[u8])]) {
        let rows: Vec<(String, String, String, String)> = files
            .iter()
            .enumerate()
            .map(|(i, (name, bytes))| {
                let path = format!("/cdn/filestreamingservice/files/{i}");
                self.mock.route(&path, Route::ok(bytes.to_vec()));
                (
                    (*name).to_owned(),
                    format!(
                        "{}?P1=4102444800&P2=404&P3=2&P4=token",
                        self.mock.url(&path)
                    ),
                    sha1_hex(bytes),
                    listed_size(bytes.len()),
                )
            })
            .collect();
        let refs: Vec<(&str, &str, &str, &str)> = rows
            .iter()
            .map(|(a, b, c, d)| (a.as_str(), b.as_str(), c.as_str(), d.as_str()))
            .collect();
        self.mock
            .route("/mirror/api/GetFiles", Route::ok(listing(&refs)));
    }
}

fn request(source: WindowsAppSource) -> WindowsAppRequest {
    WindowsAppRequest {
        source,
        installed: Vec::new(),
        arch: "x64".into(),
    }
}

fn app_msix(version: &str, deps: &[(&str, &str, &str)]) -> Vec<u8> {
    msix(
        &manifest(
            "OpenAI.Codex",
            version,
            "x64",
            OPENAI_STORE_PUBLISHER,
            false,
            deps,
        ),
        &payload(50_000),
    )
}

#[tokio::test]
async fn the_official_package_is_downloaded_and_identified() {
    let w = World::new().await;
    let pkg = msix(&real_manifest(), &payload(80_000));
    w.official("26.1013.4894.0", &pkg);
    let s = w.store(WindowsAppSource::OfficialFirst, "x86_64");
    let sink = Arc::new(Sink::default());
    let r = s
        .download_windows_app(request(WindowsAppSource::OfficialFirst), sink.clone())
        .await
        .unwrap();
    let win = r.windows.clone().unwrap();
    assert_eq!(win.source, AppPackageSource::Official);
    assert!(win.fallback_reasons.is_empty());
    assert_eq!(win.release_version.as_deref(), Some("26.1013.40934"));
    assert_eq!(
        win.package.identity.family_name(),
        "OpenAI.Codex_2p2nqsd0c76g0"
    );
    assert_eq!(win.package.identity.version, "26.1013.4894.0");
    assert_eq!(win.package.sha256, sha256_hex(&pkg));
    assert_eq!(win.package.sha1, None);
    assert!(win.dependencies.is_empty() && win.unresolved.is_empty());
    assert_eq!(
        r.path,
        w.tmp
            .path()
            .join("data/downloads/ChatGPT-26.1013.4894.0-x64.msix")
    );
    assert_eq!(r.package.kind, AppPackageKind::Msix);
    assert_eq!(std::fs::read(&r.path).unwrap(), pkg);
    let stages: Vec<DownloadStage> = sink.reports().iter().map(|p| p.stage).collect();
    assert_eq!(stages.first(), Some(&DownloadStage::Resolving));
    assert!(stages.contains(&DownloadStage::Downloading));
    // The mirror was never asked.
    assert!(w.mock.requests("/mirror/api/GetFiles").is_empty());
}

#[tokio::test]
async fn the_store_mirror_is_used_when_the_official_server_fails() {
    let w = World::new().await;
    w.mock
        .route("/app/windows-release.json", Route::status(503));
    let pkg = app_msix("26.1014.5.0", &[]);
    w.mirror(&[
        (
            "OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.BlockMap|1121963",
            b"" as &[u8],
        ),
        ("OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix", &pkg),
        ("OpenAI.Codex_26.1014.5.0_arm64__2p2nqsd0c76g0.msix", b"arm"),
    ]);
    let s = w.store(WindowsAppSource::OfficialFirst, "x86_64");
    let r = s
        .download_windows_app(
            request(WindowsAppSource::OfficialFirst),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap();
    let win = r.windows.unwrap();
    assert_eq!(win.source, AppPackageSource::StoreMirror);
    assert_eq!(win.fallback_reasons.len(), 1);
    assert!(
        win.fallback_reasons[0].contains("503"),
        "{:?}",
        win.fallback_reasons
    );
    assert_eq!(win.package.sha1.as_deref(), Some(sha1_hex(&pkg).as_str()));
    assert_eq!(
        r.path,
        w.tmp
            .path()
            .join("data/downloads/OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix")
    );
    // The reported URL carries no signed token.
    assert!(!r.package.url.contains("P4="));
    // The lookup is what the site's form sends, with its referer.
    let post = &w.mock.requests("/mirror/api/GetFiles")[0];
    assert_eq!(post.method, "POST");
    assert_eq!(post.body, "type=ProductId&url=9PLM9XGG6VKS&ring=RP&lang=");
    assert_eq!(
        post.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        post.header("referer"),
        Some(format!("{}/", w.mock.url("/mirror")).as_str())
    );

    // A second download (here: of the same, already verified file) reuses the
    // cached answer instead of asking the resolver again.
    s.download_windows_app(
        request(WindowsAppSource::StoreMirrorOnly),
        Arc::new(Sink::default()),
    )
    .await
    .unwrap();
    assert_eq!(w.mock.requests("/mirror/api/GetFiles").len(), 1);
}

#[tokio::test]
async fn official_only_reports_the_official_failure_and_never_asks_the_mirror() {
    let w = World::new().await;
    w.mock
        .route("/app/windows-release.json", Route::status(404));
    let s = w.store(WindowsAppSource::OfficialOnly, "x86_64");
    let e = s
        .download_windows_app(
            request(WindowsAppSource::OfficialOnly),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Http { status: 404, .. }), "{e:?}");
    assert!(w.mock.requests("/mirror/api/GetFiles").is_empty());
}

#[tokio::test]
async fn a_challenged_mirror_is_explained_and_both_failures_are_named() {
    let w = World::new().await;
    w.mock
        .route("/app/windows-release.json", Route::status(500));
    let challenge = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/rg-adguard-challenge.html"
    ))
    .unwrap();
    w.mock.route(
        "/mirror/api/GetFiles",
        Route::ok(challenge)
            .with_status(403)
            .header("cf-mitigated", "challenge"),
    );
    let s = w.store(WindowsAppSource::StoreMirrorOnly, "x86_64");
    let e = s
        .download_windows_app(
            request(WindowsAppSource::StoreMirrorOnly),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Http { status: 403, message, .. } if message.contains("browser check")),
        "{e:?}"
    );
    let e = s
        .download_windows_app(
            request(WindowsAppSource::OfficialFirst),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap_err();
    let detail = e.detail();
    assert!(
        detail.contains("official server") && detail.contains("500"),
        "{detail}"
    );
    assert!(
        detail.contains("store.rg-adguard.net") && detail.contains("browser check"),
        "{detail}"
    );
}

#[tokio::test]
async fn a_package_of_someone_else_is_refused_and_deleted() {
    let w = World::new().await;
    let evil = msix(
        &manifest(
            "OpenAI.Codex",
            "26.1013.4894.0",
            "x64",
            "CN=Evil",
            false,
            &[],
        ),
        b"evil",
    );
    w.official("26.1013.4894.0", &evil);
    let s = w.store(WindowsAppSource::OfficialOnly, "x86_64");
    let e = s
        .download_windows_app(
            request(WindowsAppSource::OfficialOnly),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Invalid { message } if message.contains("publisher")),
        "{e:?}"
    );
    assert!(
        !w.tmp
            .path()
            .join("data/downloads/ChatGPT-26.1013.4894.0-x64.msix")
            .exists()
    );

    // A file that is not the version windows-release.json announced either.
    let other = app_msix("26.1013.3656.0", &[]);
    w.official("26.1013.4894.0", &other);
    let e = s
        .download_windows_app(
            request(WindowsAppSource::OfficialOnly),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Invalid { message } if message.contains("version")),
        "{e:?}"
    );
}

#[tokio::test]
async fn missing_framework_dependencies_are_downloaded_and_installed_ones_skipped() {
    let w = World::new().await;
    let pkg = app_msix(
        "26.1014.5.0",
        &[(VCLIBS, "14.0.30704.0", MICROSOFT_PUBLISHER)],
    );
    let vclibs = msix(
        &manifest(
            VCLIBS,
            "14.0.33728.0",
            "x64",
            MICROSOFT_PUBLISHER,
            true,
            &[],
        ),
        b"vclibs",
    );
    w.mirror(&[
        ("OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix", &pkg),
        (
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
            &vclibs,
        ),
        (
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x86__8wekyb3d8bbwe.appx",
            b"x86",
        ),
    ]);
    let s = w.store(WindowsAppSource::StoreMirrorOnly, "x86_64");
    let sink = Arc::new(Sink::default());
    let r = s
        .download_windows_app(request(WindowsAppSource::StoreMirrorOnly), sink.clone())
        .await
        .unwrap();
    let win = r.windows.unwrap();
    assert_eq!(win.dependencies.len(), 1);
    assert_eq!(win.dependencies[0].identity.name, VCLIBS);
    assert_eq!(win.dependencies[0].identity.arch, "x64");
    assert!(
        win.dependencies[0]
            .path
            .ends_with("Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx")
    );
    assert!(win.unresolved.is_empty());
    assert!(sink.reports().iter().any(|p| p.index == 2 && p.count == 2));

    // Already installed: only the app.
    let w = World::new().await;
    w.mirror(&[
        ("OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix", &pkg),
        (
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
            &vclibs,
        ),
    ]);
    let s = w.store(WindowsAppSource::StoreMirrorOnly, "x86_64");
    let mut req = request(WindowsAppSource::StoreMirrorOnly);
    req.installed = vec![InstalledPackage {
        name: VCLIBS.into(),
        version: "14.0.33519.0".into(),
        arch: "x64".into(),
        publisher_id: "8wekyb3d8bbwe".into(),
    }];
    let r = s
        .download_windows_app(req, Arc::new(Sink::default()))
        .await
        .unwrap();
    assert!(r.windows.unwrap().dependencies.is_empty());
    assert!(
        w.mock
            .requests("/cdn/filestreamingservice/files/1")
            .is_empty()
    );

    // Official only: the dependency cannot be fetched and is reported.
    let w = World::new().await;
    w.official("26.1014.5.0", &pkg);
    let s = w.store(WindowsAppSource::OfficialOnly, "x86_64");
    let r = s
        .download_windows_app(
            request(WindowsAppSource::OfficialOnly),
            Arc::new(Sink::default()),
        )
        .await
        .unwrap();
    let win = r.windows.unwrap();
    assert!(win.dependencies.is_empty());
    assert_eq!(win.unresolved.len(), 1);
    assert_eq!(win.unresolved[0].name, VCLIBS);
}

#[tokio::test]
async fn an_unknown_architecture_is_unsupported() {
    let w = World::new().await;
    let s = w.store(WindowsAppSource::OfficialFirst, "x86_64");
    let mut req = request(WindowsAppSource::OfficialFirst);
    req.arch = "riscv64".into();
    let e = s
        .download_windows_app(req, Arc::new(Sink::default()))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
}

fn arm64_msix(version: &str) -> Vec<u8> {
    msix(
        &manifest(
            "OpenAI.Codex",
            version,
            "arm64",
            OPENAI_STORE_PUBLISHER,
            false,
            &[],
        ),
        &payload(30_000),
    )
}

#[tokio::test]
async fn an_x64_superba_on_arm64_windows_downloads_the_native_arm64_package() {
    // The process is x64 (emulated); the request carries the native arch.
    let w = World::new().await;
    let arm = arm64_msix("26.1014.5.0");
    let x64 = app_msix("26.1014.5.0", &[]);
    w.mirror(&[
        ("OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix", &x64),
        ("OpenAI.Codex_26.1014.5.0_arm64__2p2nqsd0c76g0.msix", &arm),
    ]);
    let s = w.store(WindowsAppSource::StoreMirrorOnly, "x86_64");
    let mut req = request(WindowsAppSource::StoreMirrorOnly);
    req.arch = "arm64".into();
    let r = s
        .download_windows_app(req, Arc::new(Sink::default()))
        .await
        .unwrap();
    let win = r.windows.unwrap();
    assert_eq!(win.package.identity.arch, "arm64");
    assert!(
        r.path
            .ends_with("OpenAI.Codex_26.1014.5.0_arm64__2p2nqsd0c76g0.msix")
    );
    assert!(
        w.mock
            .requests("/cdn/filestreamingservice/files/0")
            .is_empty()
    );

    // The official server: the arm64 msix of windows-release.json.
    let w = World::new().await;
    let release = format!(
        r#"{{"packageVersion":"26.1014.5.0","packages":[
          {{"architecture":"x64","downloadUrl":"{}"}},
          {{"architecture":"arm64","downloadUrl":"{}"}}]}}"#,
        w.mock.url("/app/releases/26.1014.5.0/ChatGPT-x64.msix"),
        w.mock.url("/app/releases/26.1014.5.0/ChatGPT-arm64.msix"),
    );
    w.mock
        .route("/app/windows-release.json", Route::ok(release));
    w.mock.route(
        "/app/releases/26.1014.5.0/ChatGPT-arm64.msix",
        Route::ok(arm.clone()),
    );
    w.mock.route(
        "/app/releases/26.1014.5.0/ChatGPT-x64.msix",
        Route::ok(x64.clone()),
    );
    let s = w.store(WindowsAppSource::OfficialOnly, "x86_64");
    let mut req = request(WindowsAppSource::OfficialOnly);
    req.arch = "arm64".into();
    let r = s
        .download_windows_app(req, Arc::new(Sink::default()))
        .await
        .unwrap();
    assert_eq!(r.windows.unwrap().package.identity.arch, "arm64");
    assert_eq!(r.package.arch, "arm64");
    assert!(
        w.mock
            .requests("/app/releases/26.1014.5.0/ChatGPT-x64.msix")
            .is_empty()
    );
}

#[tokio::test]
async fn arm64_falls_back_to_x64_only_when_no_arm64_package_is_offered() {
    let w = World::new().await;
    let x64 = app_msix("26.1014.5.0", &[]);
    // windows-release.json's arm64 URL is refused (not on the official host),
    // so only the x64 package is offered.
    w.official("26.1014.5.0", &x64);
    let s = w.store(WindowsAppSource::OfficialOnly, "x86_64");
    let mut req = request(WindowsAppSource::OfficialOnly);
    req.arch = "arm64".into();
    let r = s
        .download_windows_app(req, Arc::new(Sink::default()))
        .await
        .unwrap();
    assert_eq!(r.windows.unwrap().package.identity.arch, "x64");
}

#[test]
fn older_app_packages_are_removed_and_frameworks_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let keep = d.join("OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix");
    for name in [
        "OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix",
        "OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix.verified.json",
        "ChatGPT-26.1013.4894.0-x64.msix",
        "ChatGPT-26.1013.4894.0-x64.msix.verified.json",
        "ChatGPT-26.1015.1.0-x64.msix.part",
        "ChatGPT-26.1015.1.0-x64.msix.part.json",
        "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
        "chatgpt_26.1013.31256_amd64.deb",
        "store-mirror-9PLM9XGG6VKS.json",
    ] {
        std::fs::write(d.join(name), b"x").unwrap();
    }
    download::prune_app_packages(d, &keep);
    let mut left: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
            "OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix",
            "OpenAI.Codex_26.1014.5.0_x64__2p2nqsd0c76g0.msix.verified.json",
            "chatgpt_26.1013.31256_amd64.deb",
            "store-mirror-9PLM9XGG6VKS.json",
        ]
    );
}

#[tokio::test]
async fn a_cancel_does_not_wait_for_a_slow_lookup() {
    let w = World::new().await;
    // The resolver answers only after 10 s.
    w.mock.route(
        "/mirror/api/GetFiles",
        Route::ok(vec![b' '; 4]).slow(1, Duration::from_secs(5)),
    );
    let s = w.store(WindowsAppSource::StoreMirrorOnly, "x86_64");
    let sink = Arc::new(Sink {
        cancel_at: Some(1),
        ..Sink::default()
    });
    let cancel = sink.clone();
    let started = std::time::Instant::now();
    let work = s.download_windows_app(request(WindowsAppSource::StoreMirrorOnly), sink);
    let stop = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.reports.lock().unwrap().push(DownloadProgress {
            done: u64::MAX,
            ..DownloadProgress::stage(DownloadStage::Downloading, None)
        });
    };
    let (r, ()) = tokio::join!(work, stop);
    assert_eq!(r.unwrap_err(), Error::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}
