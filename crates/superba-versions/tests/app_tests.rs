//! Desktop app builds: parsers against saved public samples (APT `Packages`,
//! RPM `repomd.xml` / `primary.xml`, Sparkle appcast, Windows Store JSON) and
//! the per-OS latest check and verified package download against a loopback
//! mock server.

mod support;

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use superba_core::{
    AppOs, AppPackage, AppPackageKind, AppRelease, CodexVersion, DataDir, Error, FeedSnapshot,
    FeedSource, NetworkSettings, VersionStore,
};
use superba_versions::HttpVersionStore;
use superba_versions::app::{
    app_version_cmp, parse_appcast, parse_packages, parse_primary, parse_repomd, parse_windows,
    random_installation_id, under_base,
};
use superba_versions::sources::Endpoints;
use support::{Mock, Route, sha256_hex};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

const BASE: &str = "https://persistent.oaistatic.com/codex-app-prod";

#[test]
fn apt_packages_index() {
    let (version, p) = parse_packages(
        &fixture("Packages-amd64"),
        &format!("{BASE}/linux/deb"),
        "amd64",
    )
    .unwrap();
    assert_eq!(version, "26.1013.40934");
    assert_eq!(p.kind, AppPackageKind::Deb);
    assert_eq!(p.arch, "amd64");
    assert_eq!(
        p.url,
        format!("{BASE}/linux/deb/pool/main/c/chatgpt/chatgpt_26.1013.40934_amd64.deb")
    );
    assert_eq!(p.size, Some(502_087_434));
    assert_eq!(
        p.sha256.as_deref(),
        Some("3ca59e7f9a6641b9e2fb9ad680683784b5ab8026ff7418fb212a0ebe5df3a1d7")
    );
    // Other packages and unsafe file names are ignored.
    assert!(
        parse_packages(
            "Package: other\nVersion: 1\nFilename: x.deb\n",
            BASE,
            "amd64"
        )
        .is_none()
    );
    assert!(
        parse_packages(
            "Package: chatgpt\nVersion: 1\nFilename: ../x.deb\n",
            BASE,
            "amd64"
        )
        .is_none()
    );
}

#[test]
fn rpm_repomd_and_primary() {
    let (href, sha) = parse_repomd(&fixture("repomd.xml")).unwrap();
    assert_eq!(
        href,
        "repodata/4698ece2555c7baa2de9178558b3d0907364840d9956887adc5d5b4f5e435977-primary.xml.gz"
    );
    assert_eq!(
        sha.as_deref(),
        Some("4698ece2555c7baa2de9178558b3d0907364840d9956887adc5d5b4f5e435977")
    );
    let root = format!("{BASE}/linux/rpm/x86_64");
    let (version, p) = parse_primary(&fixture("primary.xml"), &root)
        .unwrap()
        .unwrap();
    assert_eq!(version, "26.1013.40934");
    assert_eq!(p.kind, AppPackageKind::Rpm);
    assert_eq!(p.arch, "x86_64");
    assert_eq!(p.url, format!("{root}/chatgpt-26.1013.40934-1.x86_64.rpm"));
    assert_eq!(p.size, Some(559_195_437));
    assert_eq!(
        p.sha256.as_deref(),
        Some("4af029bbaab60a5220466d5b678b5a64ee3b1b63cada8f0002ee66062bc8395f")
    );
    assert!(parse_repomd("<repomd/>").is_err());
}

#[test]
fn sparkle_appcast() {
    let r = parse_appcast(&fixture("appcast-arm64.xml"), "arm64")
        .unwrap()
        .unwrap();
    assert_eq!(r.version, "26.1013.31256");
    assert_eq!(r.packages.len(), 1, "deltas are not packages");
    let p = &r.packages[0];
    assert_eq!(p.kind, AppPackageKind::MacZip);
    assert_eq!(
        p.url,
        format!("{BASE}/ChatGPT-darwin-arm64-26.1013.31256.zip")
    );
    assert_eq!(p.size, Some(722_756_051));
    assert!(p.signature.as_deref().unwrap().starts_with("Ko3n2pwN"));
    assert!(
        parse_appcast("<rss><channel/></rss>", "arm64")
            .unwrap()
            .is_none()
    );
    // Only an https enclosure becomes a package (it is downloaded or opened).
    let https = format!("{BASE}/ChatGPT-darwin-arm64-26.1013.31256.zip");
    for url in [
        https.replacen("https://", "http://", 1),
        "file:///Applications/ChatGPT.app".to_owned(),
        "javascript:alert(1)".to_owned(),
        "/relative/ChatGPT.zip".to_owned(),
    ] {
        let xml = fixture("appcast-arm64.xml").replace(&https, &url);
        let r = parse_appcast(&xml, "arm64").unwrap().unwrap();
        assert_eq!(r.version, "26.1013.31256");
        assert!(r.packages.is_empty(), "{url}");
    }
}

#[test]
fn only_urls_under_the_download_base_are_official() {
    for ok in [
        format!("{BASE}/linux/deb/pool/main/c/chatgpt/chatgpt_26.1013.40934_amd64.deb"),
        format!("{BASE}/ChatGPT-darwin-arm64-26.1013.31256.zip"),
    ] {
        assert!(under_base(BASE, &ok), "{ok}");
        assert!(under_base(&format!("{BASE}/"), &ok), "{ok}");
    }
    for bad in [
        "https://evil.example/codex-app-prod/x.deb".to_owned(),
        format!("{BASE}/../x.deb"),
        format!("{BASE}/%2e%2e/x.deb"),
        format!("{BASE}-evil/x.deb"),
        format!("{BASE}/"),
        BASE.to_owned(),
        BASE.replacen("https://", "http://", 1) + "/x.deb",
        BASE.replacen("https://", "https://user@", 1) + "/x.deb",
        "https://persistent.oaistatic.com:8443/codex-app-prod/x.deb".to_owned(),
        "https://persistent.oaistatic.com.evil.example/codex-app-prod/x.deb".to_owned(),
    ] {
        assert!(!under_base(BASE, &bad), "{bad}");
    }
    // The e2e / screenshot override: https on loopback.
    assert!(under_base(
        "https://127.0.0.1:4444",
        "https://127.0.0.1:4444/linux/deb/pool/main/c/chatgpt/chatgpt_26.1020.10000_amd64.deb"
    ));
    assert!(!under_base(
        "https://127.0.0.1:4444",
        "https://127.0.0.1:4445/linux/deb/x.deb"
    ));
}

#[test]
fn windows_store_and_msix() {
    let r = parse_windows(
        &fixture("windows-store-update.json"),
        Some(&fixture("windows-release.json")),
    )
    .unwrap();
    assert_eq!(r.version, "26.1013.40934");
    assert_eq!(r.store_product_id.as_deref(), Some("9PLM9XGG6VKS"));
    assert_eq!(r.packages.len(), 2);
    assert!(r.packages.iter().all(|p| p.kind == AppPackageKind::Msix));
    assert!(parse_windows("{}", None).is_err());
}

#[test]
fn app_versions_compare_numerically_and_ids_are_random() {
    assert!(app_version_cmp("26.1013.40934", "26.1013.31256").is_gt());
    assert!(app_version_cmp("26.1007.71326", "26.1013.31256").is_lt());
    assert!(app_version_cmp("26.1013.9", "26.1013.10").is_lt());
    let (a, b) = (random_installation_id(), random_installation_id());
    assert_eq!(a.len(), 36);
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------
// latest per OS against the mock server
// ---------------------------------------------------------------------------

fn endpoints(mock: &Mock) -> Endpoints {
    Endpoints {
        app: mock.url("/app"),
        appcast: mock.url("/appcast"),
        ..Endpoints::default()
    }
}

fn store(tmp: &Path, mock: &Mock, os: AppOs, arch: &str) -> HttpVersionStore {
    HttpVersionStore::new(&DataDir::new(tmp.join("data")), NetworkSettings::default())
        .with_endpoints(endpoints(mock))
        .with_platform(Some("x86_64-unknown-linux-musl"), os, arch)
}

fn gz(text: &str) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(text.as_bytes()).unwrap();
    e.finish().unwrap()
}

fn serve_linux(mock: &Mock) {
    mock.route(
        "/app/linux/deb/dists/stable/main/binary-amd64/Packages",
        Route::ok(fixture("Packages-amd64")),
    );
    let primary = gz(&fixture("primary.xml"));
    let repomd = fixture("repomd.xml").replace(
        "4698ece2555c7baa2de9178558b3d0907364840d9956887adc5d5b4f5e435977</checksum>",
        &format!("{}</checksum>", sha256_hex(&primary)),
    );
    mock.route(
        "/app/linux/rpm/x86_64/repodata/repomd.xml",
        Route::ok(repomd),
    );
    mock.route(
        "/app/linux/rpm/x86_64/repodata/4698ece2555c7baa2de9178558b3d0907364840d9956887adc5d5b4f5e435977-primary.xml.gz",
        Route::ok(primary),
    );
}

struct Apps(Vec<AppRelease>);

#[async_trait]
impl FeedSource for Apps {
    async fn snapshot(&self, _refresh: bool) -> superba_core::Result<FeedSnapshot> {
        Ok(FeedSnapshot {
            app_versions: self.0.clone(),
            ..FeedSnapshot::default()
        })
    }

    async fn prompt(&self, _: &superba_core::CodexVersion) -> superba_core::Result<Option<String>> {
        Err(superba_core::Error::not_implemented("FeedSource::prompt"))
    }

    async fn status(&self) -> superba_core::Result<superba_core::FeedsStatus> {
        Err(superba_core::Error::not_implemented("FeedSource::status"))
    }
}

#[tokio::test]
async fn linux_latest_from_apt_and_rpm_with_the_feeds_bundled_codex() {
    let mock = Mock::start().await;
    serve_linux(&mock);
    let tmp = tempfile::tempdir().unwrap();
    let feed = Apps(vec![AppRelease {
        version: "26.1013.40934".into(),
        date: None,
        bundled_codex: Some(CodexVersion::parse("0.163.0-alpha.10.2").unwrap()),
        packages: Vec::new(),
        store_product_id: None,
    }]);
    let s = store(tmp.path(), &mock, AppOs::Linux, "x86_64").with_feed(Arc::new(feed));
    let r = s.app_latest(true).await.unwrap().unwrap();
    assert_eq!(r.version, "26.1013.40934");
    assert_eq!(
        r.bundled_codex,
        Some(CodexVersion::parse("0.163.0-alpha.10.2").unwrap())
    );
    let kinds: Vec<AppPackageKind> = r.packages.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, vec![AppPackageKind::Deb, AppPackageKind::Rpm]);
    assert!(r.packages.iter().all(|p| p.sha256.is_some()));
    assert!(
        r.packages[0]
            .url
            .starts_with(&mock.url("/app/linux/deb/pool/"))
    );
    // Cached for a day: no new request without refresh.
    let n = mock.log().len();
    s.app_latest(false).await.unwrap().unwrap();
    assert_eq!(mock.log().len(), n);
}

#[tokio::test]
async fn a_tampered_rpm_index_is_ignored_and_the_deb_still_answers() {
    let mock = Mock::start().await;
    serve_linux(&mock);
    mock.route(
        "/app/linux/rpm/x86_64/repodata/4698ece2555c7baa2de9178558b3d0907364840d9956887adc5d5b4f5e435977-primary.xml.gz",
        Route::ok(gz("<metadata/>")),
    );
    let tmp = tempfile::tempdir().unwrap();
    let r = store(tmp.path(), &mock, AppOs::Linux, "x86_64")
        .app_latest(true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.packages.len(), 1);
    assert_eq!(r.packages[0].kind, AppPackageKind::Deb);
}

#[tokio::test]
async fn windows_and_macos_latest() {
    let mock = Mock::start().await;
    mock.route(
        "/app/windows-store-update.json",
        Route::ok(fixture("windows-store-update.json")),
    );
    mock.route(
        "/app/windows-release.json",
        Route::ok(fixture("windows-release.json")),
    );
    mock.route("/appcast", Route::ok(fixture("appcast-arm64.xml")));
    let tmp = tempfile::tempdir().unwrap();
    let w = store(tmp.path(), &mock, AppOs::Windows, "x86_64")
        .app_latest(true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(w.store_product_id.as_deref(), Some("9PLM9XGG6VKS"));
    let tmp = tempfile::tempdir().unwrap();
    let m = store(tmp.path(), &mock, AppOs::Macos, "aarch64")
        .app_latest(true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(m.version, "26.1013.31256");
    let q = mock
        .log()
        .into_iter()
        .find(|s| s.path.starts_with("/appcast"))
        .unwrap()
        .path;
    assert!(q.contains("arch=arm64"), "{q}");
    assert!(q.contains("installation_id="), "{q}");
}

#[tokio::test]
async fn offline_answers_from_the_feed_without_requests() {
    let mock = Mock::start().await;
    serve_linux(&mock);
    let tmp = tempfile::tempdir().unwrap();
    let feed = Apps(vec![AppRelease {
        version: "26.1007.71326".into(),
        date: None,
        bundled_codex: None,
        packages: vec![AppPackage {
            os: AppOs::Linux,
            kind: AppPackageKind::Deb,
            arch: "amd64".into(),
            url: format!("{BASE}/linux/deb/pool/main/c/chatgpt/chatgpt_26.1007.71326_amd64.deb"),
            sha256: Some("00".repeat(32)),
            size: Some(1),
            signature: None,
        }],
        store_product_id: None,
    }]);
    let offline = NetworkSettings {
        online_checks: false,
        ..NetworkSettings::default()
    };
    let s = HttpVersionStore::new(&DataDir::new(tmp.path().join("data")), offline)
        .with_endpoints(endpoints(&mock))
        .with_platform(Some("x86_64-unknown-linux-musl"), AppOs::Linux, "x86_64")
        .with_feed(Arc::new(feed));
    let r = s.app_latest(true).await.unwrap().unwrap();
    assert_eq!(r.version, "26.1007.71326");
    assert_eq!(r.packages.len(), 1);
    assert!(mock.log().is_empty());
}

#[tokio::test]
async fn feed_packages_outside_the_official_download_base_are_dropped() {
    let mock = Mock::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let deb = |url: String| AppPackage {
        os: AppOs::Linux,
        kind: AppPackageKind::Deb,
        arch: "amd64".into(),
        url,
        sha256: Some("00".repeat(32)),
        size: Some(1),
        signature: None,
    };
    let feed = Apps(vec![AppRelease {
        version: "26.1007.71326".into(),
        date: None,
        bundled_codex: None,
        packages: vec![
            deb(format!(
                "{BASE}/linux/deb/pool/main/c/chatgpt/chatgpt_26.1007.71326_amd64.deb"
            )),
            deb("https://evil.example/chatgpt_26.1007.71326_amd64.deb".into()),
            deb(format!("{BASE}/../evil/chatgpt_26.1007.71326_amd64.deb")),
            // The endpoint override (e2e: a loopback https fixture server).
            deb(mock.url("/app/linux/deb/pool/chatgpt_26.1007.71326_amd64.deb")),
        ],
        store_product_id: None,
    }]);
    let offline = NetworkSettings {
        online_checks: false,
        ..NetworkSettings::default()
    };
    let s = HttpVersionStore::new(&DataDir::new(tmp.path().join("data")), offline)
        .with_endpoints(endpoints(&mock))
        .with_platform(Some("x86_64-unknown-linux-musl"), AppOs::Linux, "x86_64")
        .with_feed(Arc::new(feed));
    let r = s.app_latest(true).await.unwrap().unwrap();
    let urls: Vec<&str> = r.packages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            format!("{BASE}/linux/deb/pool/main/c/chatgpt/chatgpt_26.1007.71326_amd64.deb"),
            mock.url("/app/linux/deb/pool/chatgpt_26.1007.71326_amd64.deb"),
        ]
    );
    // A package from elsewhere is never downloaded.
    let err = s
        .download_app(&deb(
            "https://evil.example/chatgpt_26.1007.71326_amd64.deb".into()
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    assert!(mock.log().is_empty());
}

// ---------------------------------------------------------------------------
// package downloads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn app_packages_are_downloaded_and_verified_never_installed() {
    let mock = Mock::start().await;
    let body = b"!<arch>\ndebian-binary fake deb".to_vec();
    mock.route(
        "/app/linux/deb/pool/chatgpt_26.1013.40934_amd64.deb",
        Route::ok(body.clone()),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, AppOs::Linux, "x86_64");
    let mut p = AppPackage {
        os: AppOs::Linux,
        kind: AppPackageKind::Deb,
        arch: "amd64".into(),
        url: mock.url("/app/linux/deb/pool/chatgpt_26.1013.40934_amd64.deb"),
        sha256: Some(sha256_hex(&body)),
        size: Some(body.len() as u64),
        signature: None,
    };
    let r = s.download_app(&p).await.unwrap();
    assert_eq!(
        r.path,
        tmp.path()
            .join("data/downloads/chatgpt_26.1013.40934_amd64.deb")
    );
    assert_eq!(std::fs::read(&r.path).unwrap(), body);
    assert_eq!(
        r.command.as_deref(),
        Some(format!("sudo apt install {}", r.path.display()).as_str())
    );
    // Already there and valid: no second download.
    let n = mock.log().len();
    s.download_app(&p).await.unwrap();
    assert_eq!(mock.log().len(), n);
    // A wrong checksum is an error and leaves nothing behind.
    let tmp2 = tempfile::tempdir().unwrap();
    let s2 = store(tmp2.path(), &mock, AppOs::Linux, "x86_64");
    p.sha256 = Some("11".repeat(32));
    let err = s2.download_app(&p).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(
        !tmp2
            .path()
            .join("data/downloads/chatgpt_26.1013.40934_amd64.deb")
            .exists()
    );
    assert!(
        !tmp2
            .path()
            .join("data/downloads/chatgpt_26.1013.40934_amd64.deb.part")
            .exists()
    );
    // No checksum: refused.
    p.sha256 = None;
    assert!(matches!(
        s2.download_app(&p).await,
        Err(Error::Unsupported { .. })
    ));
}
