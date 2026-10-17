//! Live checks against the real public endpoints (GitHub, npm, the desktop
//! app indexes). Ignored by default; run with
//! `SUPERBA_VERSIONS_ONLINE=1 cargo test -p superba-versions --test online_tests -- --ignored --nocapture`.

use superba_core::{DataDir, NetworkSettings, VersionStore};
use superba_versions::HttpVersionStore;

fn enabled() -> bool {
    std::env::var("SUPERBA_VERSIONS_ONLINE").as_deref() == Ok("1")
}

#[tokio::test]
#[ignore = "network: set SUPERBA_VERSIONS_ONLINE=1"]
async fn live_release_list_and_app_build() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let s = HttpVersionStore::new(&DataDir::new(tmp.path()), NetworkSettings::default());
    let releases = s.codex_releases(true).await.unwrap();
    eprintln!(
        "codex releases: {:?}",
        releases
            .iter()
            .map(|r| r.version.to_string())
            .collect::<Vec<_>>()
    );
    assert!(!releases.is_empty());
    let app = s.app_latest(true).await.unwrap();
    eprintln!("app latest: {app:?}");
    assert!(app.is_some());
}

mod support;

/// A real Codex package from a local cache, served by the mock as GitHub would:
/// `SUPERBA_TEST_CODEX_PACKAGE=<codex-package-<target>.tar.gz>` and
/// `SUPERBA_TEST_CODEX_VERSION=<version>` (the package for this machine).
#[tokio::test]
#[ignore = "large file: set SUPERBA_TEST_CODEX_PACKAGE and SUPERBA_TEST_CODEX_VERSION"]
async fn real_package_is_verified_extracted_and_runs() {
    use superba_core::{AppOs, CodexVersion, local_codex_package_target};
    use superba_versions::sources::Endpoints;
    use support::{Mock, Route, sha256_hex};
    let (Ok(pkg), Ok(version)) = (
        std::env::var("SUPERBA_TEST_CODEX_PACKAGE"),
        std::env::var("SUPERBA_TEST_CODEX_VERSION"),
    ) else {
        return;
    };
    let target = local_codex_package_target().unwrap();
    let bytes = std::fs::read(&pkg).unwrap();
    let mock = Mock::start().await;
    mock.route(
        &format!("/gh/rust-v{version}/codex-package-{target}.tar.gz"),
        Route::ok(bytes.clone()),
    );
    mock.route(
        &format!("/gh/rust-v{version}/codex-package_SHA256SUMS"),
        Route::ok(format!(
            "{}  codex-package-{target}.tar.gz\n",
            sha256_hex(&bytes)
        )),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = HttpVersionStore::new(&DataDir::new(tmp.path()), NetworkSettings::default())
        .with_endpoints(Endpoints {
            codex_releases: mock.url("/gh"),
            packages_only_from_releases: true,
            ..Endpoints::default()
        })
        .with_platform(Some(target), AppOs::Linux, std::env::consts::ARCH);
    let started = std::time::Instant::now();
    let stored = s
        .ensure(&CodexVersion::parse(&version).unwrap())
        .await
        .unwrap();
    eprintln!(
        "stored {} at {} ({} bytes) in {:?}",
        stored.version,
        stored.executable.display(),
        stored.size_bytes.unwrap_or(0),
        started.elapsed()
    );
    let dir = stored.executable.parent().unwrap().parent().unwrap();
    assert!(dir.join("bin/codex-code-mode-host").is_file());
    assert!(dir.join("codex-package.json").is_file());
}
