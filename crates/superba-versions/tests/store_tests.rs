//! Codex releases, verified downloads from every source, the store and local
//! standalone installs, against a loopback mock server.
#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use superba_core::{
    AppOs, CodexRelease, CodexVersion, DataDir, DownloadSource, Error, FeedSnapshot, FeedSource,
    NetworkSettings, StandaloneLayout, VersionStore, local_codex_package_target,
};
use superba_versions::HttpVersionStore;
use superba_versions::sources::{Endpoints, npm_platform, release_of};
use support::{Mock, Route, integrity, package, sha256_hex};

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

fn target() -> &'static str {
    local_codex_package_target().unwrap_or("x86_64-unknown-linux-musl")
}

fn endpoints(mock: &Mock) -> Endpoints {
    Endpoints {
        codex_releases: mock.url("/gh"),
        codex_latest: mock.url("/gh-latest"),
        npm: mock.url("/npm"),
        npmmirror: mock.url("/npmmirror"),
        proxies: vec![mock.url("/proxy/")],
        packages_only_from_releases: false,
        app: mock.url("/app"),
        appcast: mock.url("/appcast"),
    }
}

fn store(tmp: &Path, mock: &Mock, network: NetworkSettings) -> HttpVersionStore {
    HttpVersionStore::new(&DataDir::new(tmp.join("data")), network)
        .with_endpoints(endpoints(mock))
        .with_platform(local_codex_package_target(), AppOs::Linux, "x86_64")
}

fn gh_path(version: &str) -> String {
    format!("/gh/rust-v{version}/codex-package-{}.tar.gz", target())
}

fn sums_path(version: &str) -> String {
    format!("/gh/rust-v{version}/codex-package_SHA256SUMS")
}

fn npm_paths(reg: &str, version: &str) -> (String, String) {
    let platform = npm_platform(target()).unwrap();
    (
        format!("/{reg}/@openai/codex/-/codex-{version}-{platform}.tgz"),
        format!("/{reg}/@openai/codex/{version}-{platform}"),
    )
}

/// Serve `version` on GitHub with a matching SHA256SUMS.
fn serve_github(mock: &Mock, version: &str) -> Vec<u8> {
    let pkg = package(version, target(), false);
    mock.route(&gh_path(version), Route::ok(pkg.clone()));
    mock.route(
        &sums_path(version),
        Route::ok(format!(
            "{}  codex-package-{}.tar.gz\n",
            sha256_hex(&pkg),
            target()
        )),
    );
    pkg
}

/// Serve `version` on an npm registry with a matching integrity.
fn serve_npm(mock: &Mock, reg: &str, version: &str) -> Vec<u8> {
    let pkg = package(version, target(), true);
    mock.route(&npm_paths(reg, version).0, Route::ok(pkg.clone()));
    serve_npm_doc(mock, reg, version, &pkg);
    pkg
}

/// Serve the version document of `version` on an npm registry, with the
/// integrity of `pkg`.
fn serve_npm_doc(mock: &Mock, reg: &str, version: &str, pkg: &[u8]) {
    mock.route(
        &npm_paths(reg, version).1,
        Route::ok(format!(
            "{{\"dist\":{{\"integrity\":\"{}\",\"tarball\":\"x\"}}}}",
            integrity(pkg)
        )),
    );
}

fn net(source: DownloadSource, proxies: bool) -> NetworkSettings {
    NetworkSettings {
        download_source: source,
        third_party_proxies: proxies,
        ..NetworkSettings::default()
    }
}

fn store_entries(tmp: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(tmp.join("data/codex"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && n != "packages" && n != "releases.json")
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// downloads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn github_package_is_verified_with_sha256sums_extracted_and_reused() {
    let mock = Mock::start().await;
    serve_github(&mock, "0.162.0");
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    let stored = s.ensure(&v("0.162.0")).await.unwrap();
    assert_eq!(stored.version, v("0.162.0"));
    assert_eq!(stored.target, target());
    assert!(stored.executable.is_file());
    assert_eq!(
        stored.executable,
        tmp.path()
            .join(format!("data/codex/0.162.0-{}/bin/codex", target()))
    );
    let archive = tmp.path().join(format!(
        "data/codex/packages/codex-package-0.162.0-{}.tar.gz",
        target()
    ));
    assert!(archive.is_file());
    assert_eq!(s.stored().await.unwrap().len(), 1);
    // Already stored: nothing is fetched again.
    let before = mock.log().len();
    s.ensure(&v("0.162.0")).await.unwrap();
    assert_eq!(mock.log().len(), before);
    // The verified archive serves remote uploads without another download.
    let p = s.package_path(&v("0.162.0"), target()).await.unwrap();
    assert_eq!(p, archive);
    assert_eq!(mock.log().len(), before);
}

#[tokio::test]
async fn a_corrupted_github_download_is_rejected_and_npm_used() {
    let mock = Mock::start().await;
    let good = serve_github(&mock, "0.162.0");
    let mut bad = good.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0xff;
    mock.route(&gh_path("0.162.0"), Route::ok(bad));
    serve_npm(&mock, "npm", "0.162.0");
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    let stored = s.ensure(&v("0.162.0")).await.unwrap();
    assert!(stored.executable.is_file());
    assert_eq!(mock.hits(&gh_path("0.162.0")), 1);
    assert_eq!(mock.hits(&npm_paths("npm", "0.162.0").0), 1);
    assert_eq!(mock.hits("/npmmirror"), 0);
    let packages: Vec<String> = std::fs::read_dir(tmp.path().join("data/codex/packages"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        packages.iter().all(|p| !p.contains("codex-package-")),
        "the rejected archive is gone: {packages:?}"
    );
}

#[tokio::test]
async fn truncated_and_mismatching_downloads_never_reach_the_store() {
    let mock = Mock::start().await;
    let good = serve_github(&mock, "0.162.0");
    mock.route(
        &gh_path("0.162.0"),
        Route::ok(good[..good.len() - 100].to_vec()),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, net(DownloadSource::GitHub, false));
    let err = s.ensure(&v("0.162.0")).await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("checksum"), "{err}");
    assert!(store_entries(tmp.path()).is_empty());
    // No published checksum at all: refused before downloading.
    mock.route(&sums_path("0.162.0"), Route::status(404));
    mock.route(&gh_path("0.162.0"), Route::ok(good));
    let err = s.ensure(&v("0.162.0")).await.unwrap_err();
    assert!(err.to_string().contains("no published checksum"), "{err}");
    assert!(store_entries(tmp.path()).is_empty());
}

#[tokio::test]
async fn npmmirror_is_used_alone_when_chosen_with_the_npmjs_checksum() {
    let mock = Mock::start().await;
    serve_github(&mock, "0.162.0");
    let pkg = serve_npm(&mock, "npmmirror", "0.162.0");
    serve_npm_doc(&mock, "npm", "0.162.0", &pkg);
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, net(DownloadSource::NpmMirror, true));
    s.ensure(&v("0.162.0")).await.unwrap();
    assert_eq!(mock.hits("/gh"), 0);
    assert_eq!(mock.hits("/proxy/"), 0);
    let (tgz, doc) = npm_paths("npmmirror", "0.162.0");
    assert_eq!(mock.hits(&tgz), 1);
    assert_eq!(
        mock.hits(&doc),
        0,
        "the mirror's own metadata is never read"
    );
    assert_eq!(mock.hits(&npm_paths("npm", "0.162.0").1), 1);
    assert_eq!(mock.hits(&npm_paths("npm", "0.162.0").0), 0);
}

#[tokio::test]
async fn a_tampered_npmmirror_tarball_with_a_matching_mirror_integrity_is_rejected() {
    let mock = Mock::start().await;
    let genuine = package("0.162.0", target(), true);
    serve_npm_doc(&mock, "npm", "0.162.0", &genuine);
    // The mirror serves another (well-formed) package and vouches for it.
    let vendor = format!("package/vendor/{}/", target());
    let codex = support::fake_codex("0.162.0");
    let (bin, evil) = (format!("{vendor}bin/codex"), format!("{vendor}bin/evil"));
    let tampered = support::targz(&[(&bin, &codex, 0o755), (&evil, b"evil", 0o755)], &[]);
    mock.route(
        &npm_paths("npmmirror", "0.162.0").0,
        Route::ok(tampered.clone()),
    );
    serve_npm_doc(&mock, "npmmirror", "0.162.0", &tampered);
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, net(DownloadSource::NpmMirror, false));
    let err = s.ensure(&v("0.162.0")).await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("checksum"), "{err}");
    assert!(store_entries(tmp.path()).is_empty());
    assert_eq!(mock.hits(&npm_paths("npmmirror", "0.162.0").1), 0);
}

#[tokio::test]
async fn npmmirror_is_skipped_without_a_trusted_checksum() {
    let mock = Mock::start().await;
    // npmjs does not answer; the mirror has the package and its own integrity.
    serve_npm(&mock, "npmmirror", "0.162.0");
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, net(DownloadSource::NpmMirror, false));
    let err = s.ensure(&v("0.162.0")).await.unwrap_err();
    assert!(err.to_string().contains("no trusted checksum"), "{err}");
    assert_eq!(
        mock.hits("/npmmirror/"),
        0,
        "nothing fetched from the mirror"
    );
    assert!(store_entries(tmp.path()).is_empty());
}

#[tokio::test]
async fn proxies_only_when_enabled_and_only_with_a_trusted_checksum() {
    let mock = Mock::start().await;
    let pkg = serve_github(&mock, "0.162.0");
    // GitHub's file itself is unreachable; the proxy has it.
    mock.route(&gh_path("0.162.0"), Route::status(503));
    let proxied = format!("/proxy/{}", mock.url(&gh_path("0.162.0")));
    mock.route(&proxied, Route::ok(pkg));
    let tmp = tempfile::tempdir().unwrap();
    let off = store(tmp.path(), &mock, net(DownloadSource::GitHub, false));
    assert!(off.ensure(&v("0.162.0")).await.is_err());
    assert_eq!(mock.hits("/proxy/"), 0);
    let on = store(tmp.path(), &mock, net(DownloadSource::GitHub, true));
    on.ensure(&v("0.162.0")).await.unwrap();
    assert_eq!(mock.hits("/proxy/"), 1);

    // Without SHA256SUMS (or a feed checksum) the proxy is never asked.
    let mock2 = Mock::start().await;
    let pkg = package("0.163.0", target(), false);
    mock2.route(
        &format!("/proxy/{}", mock2.url(&gh_path("0.163.0"))),
        Route::ok(pkg),
    );
    let tmp2 = tempfile::tempdir().unwrap();
    let s = store(tmp2.path(), &mock2, net(DownloadSource::GitHub, true));
    assert!(s.ensure(&v("0.163.0")).await.is_err());
    assert_eq!(mock2.hits("/proxy/"), 0);
}

#[tokio::test]
async fn an_interrupted_download_resumes_with_a_range_request() {
    let mock = Mock::start().await;
    let pkg = serve_github(&mock, "0.162.0");
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join(format!(
        "data/codex/packages/codex-package-0.162.0-{}.tar.gz.part",
        target()
    ));
    support::write(&part, &pkg[..pkg.len() / 3]);
    let s = store(tmp.path(), &mock, net(DownloadSource::GitHub, false));
    s.ensure(&v("0.162.0")).await.unwrap();
    let seen = mock
        .log()
        .into_iter()
        .find(|r| r.path == gh_path("0.162.0"))
        .unwrap();
    assert_eq!(seen.range, Some(format!("bytes={}-", pkg.len() / 3)));
    assert!(!part.exists());
}

#[tokio::test]
async fn a_package_reporting_another_version_is_refused() {
    let mock = Mock::start().await;
    let pkg = package("0.161.0", target(), false);
    mock.route(&gh_path("0.162.0"), Route::ok(pkg.clone()));
    mock.route(
        &sums_path("0.162.0"),
        Route::ok(format!(
            "{}  codex-package-{}.tar.gz\n",
            sha256_hex(&pkg),
            target()
        )),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, net(DownloadSource::GitHub, false));
    let err = s.ensure(&v("0.162.0")).await.unwrap_err();
    assert!(err.to_string().contains("0.161.0"), "{err}");
    assert!(store_entries(tmp.path()).is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join("data/codex"))
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.starts_with(".tmp-").then_some(n)
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn packages_for_another_target_are_downloaded_but_not_extracted() {
    let mock = Mock::start().await;
    let other = "aarch64-unknown-linux-musl";
    let pkg = package("0.162.0", other, false);
    mock.route(
        &format!("/gh/rust-v0.162.0/codex-package-{other}.tar.gz"),
        Route::ok(pkg.clone()),
    );
    mock.route(
        &sums_path("0.162.0"),
        Route::ok(format!(
            "{}  codex-package-{other}.tar.gz\n",
            sha256_hex(&pkg)
        )),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    let p = s.package_path(&v("0.162.0"), other).await.unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), pkg);
    assert!(store_entries(tmp.path()).is_empty());
    assert!(s.package_path(&v("0.162.0"), "sparc-sun").await.is_err());
}

#[tokio::test]
async fn retention_keeps_the_newest_three_and_every_kept_version() {
    let mock = Mock::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    for ver in ["0.150.1", "0.160.0", "0.161.0", "0.162.0", "0.163.0"] {
        serve_github(&mock, ver);
        s.ensure(&v(ver)).await.unwrap();
    }
    // 0.150.1 is pinned, 0.160.0 is the running app-server's.
    let removed = s.prune(&[v("0.150.1"), v("0.160.0")]).await.unwrap();
    assert_eq!(removed, Vec::<CodexVersion>::new());
    let removed = s.prune(&[v("0.150.1")]).await.unwrap();
    assert_eq!(removed, vec![v("0.160.0")]);
    let left: Vec<String> = s
        .stored()
        .await
        .unwrap()
        .iter()
        .map(|x| x.version.to_string())
        .collect();
    assert_eq!(left, vec!["0.163.0", "0.162.0", "0.161.0", "0.150.1"]);
    let archives = std::fs::read_dir(tmp.path().join("data/codex/packages"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("0.160.0")
        })
        .count();
    assert_eq!(archives, 0);
}

// ---------------------------------------------------------------------------
// release lists
// ---------------------------------------------------------------------------

struct Feed(Vec<CodexRelease>);

#[async_trait]
impl FeedSource for Feed {
    async fn snapshot(&self, _refresh: bool) -> superba_core::Result<FeedSnapshot> {
        Ok(FeedSnapshot {
            codex_versions: self.0.clone(),
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

const DIST_TAGS: &str =
    r#"{"latest":"0.163.0","alpha":"0.164.0-alpha.1","linux-x64":"0.163.0-linux-x64"}"#;

#[tokio::test]
async fn releases_come_from_the_feed_npm_then_github_and_are_cached() {
    let mock = Mock::start().await;
    mock.route(
        "/npm/-/package/@openai/codex/dist-tags",
        Route::ok(DIST_TAGS),
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut feed_entry = release_of(v("0.162.0-alpha.17.2"), "feed");
    feed_entry.bundled_by.push("26.1007.71326".into());
    let s = store(tmp.path(), &mock, NetworkSettings::default())
        .with_feed(Arc::new(Feed(vec![feed_entry])));
    let list = s.codex_releases(true).await.unwrap();
    let names: Vec<String> = list.iter().map(|r| r.version.to_string()).collect();
    assert_eq!(
        names,
        vec!["0.164.0-alpha.1", "0.163.0", "0.162.0-alpha.17.2"]
    );
    assert_eq!(list[2].bundled_by, vec!["26.1007.71326".to_owned()]);
    // Fresh cache: no request without refresh.
    let n = mock.log().len();
    s.codex_releases(false).await.unwrap();
    assert_eq!(mock.log().len(), n);

    // npmjs down: npmmirror; both down: the GitHub latest redirect.
    let mock = Mock::start().await;
    mock.route(
        "/npmmirror/-/package/@openai/codex/dist-tags",
        Route::ok(DIST_TAGS),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    assert_eq!(s.codex_releases(true).await.unwrap().len(), 2);
    let mock = Mock::start().await;
    mock.route(
        "/gh-latest",
        Route::redirect("https://github.com/openai/codex/releases/tag/rust-v0.163.0"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    let list = s.codex_releases(true).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].version, v("0.163.0"));
    // Everything down afterwards: the cache answers.
    mock.route("/gh-latest", Route::status(500));
    assert_eq!(s.codex_releases(true).await.unwrap().len(), 1);
}

#[tokio::test]
async fn no_online_check_when_disabled() {
    let mock = Mock::start().await;
    mock.route(
        "/npm/-/package/@openai/codex/dist-tags",
        Route::ok(DIST_TAGS),
    );
    let tmp = tempfile::tempdir().unwrap();
    let offline = NetworkSettings {
        online_checks: false,
        ..NetworkSettings::default()
    };
    let s = store(tmp.path(), &mock, offline)
        .with_feed(Arc::new(Feed(vec![release_of(v("0.162.0"), "feed")])));
    let list = s.codex_releases(true).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(mock.log().is_empty());
}

#[tokio::test]
async fn feed_checksums_are_trusted_without_sha256sums() {
    let mock = Mock::start().await;
    let pkg = package("0.162.0", target(), false);
    mock.route(&gh_path("0.162.0"), Route::ok(pkg.clone()));
    let mut r = release_of(v("0.162.0"), "feed");
    r.sha256.insert(target().to_owned(), sha256_hex(&pkg));
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, net(DownloadSource::GitHub, false))
        .with_feed(Arc::new(Feed(vec![r])));
    s.ensure(&v("0.162.0")).await.unwrap();
    assert_eq!(mock.hits(&sums_path("0.162.0")), 0);
}

// ---------------------------------------------------------------------------
// local standalone layout
// ---------------------------------------------------------------------------

fn version_of(p: &Path) -> String {
    let out = std::process::Command::new(p)
        .arg("--version")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn local_standalone_install_switch_and_rollback() {
    let mock = Mock::start().await;
    serve_github(&mock, "0.162.0");
    serve_github(&mock, "0.163.0");
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path(), &mock, NetworkSettings::default());
    let layout = StandaloneLayout {
        codex_home: tmp.path().join("home/.codex"),
        bin_dir: tmp.path().join("home/.local/bin"),
    };
    // A hand-installed codex first: restored by the first rollback.
    support::write(&layout.command(), &support::fake_codex("0.150.1"));
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(layout.command(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let r = s.install_standalone(&v("0.162.0"), &layout).await.unwrap();
    assert_eq!(r.previous, Some(v("0.150.1")));
    assert_eq!(version_of(&layout.command()), "codex-cli 0.162.0");
    let current = std::fs::read_link(layout.root().join("current")).unwrap();
    assert!(current.ends_with(format!("releases/0.162.0-{}", target())));
    let r = s.install_standalone(&v("0.163.0"), &layout).await.unwrap();
    assert_eq!(r.previous, Some(v("0.162.0")));
    assert_eq!(version_of(&layout.command()), "codex-cli 0.163.0");
    let r = s.rollback_standalone(&layout).await.unwrap();
    assert_eq!(r.version, v("0.162.0"));
    let r = s.rollback_standalone(&layout).await.unwrap();
    assert_eq!(r.version, v("0.163.0"));
    // The store entry stays intact when the release is removed later.
    let entry: PathBuf = tmp
        .path()
        .join(format!("data/codex/0.162.0-{}/bin/codex", target()));
    assert!(entry.is_file());

    // A fresh layout: the first rollback restores the original command.
    let layout2 = StandaloneLayout {
        codex_home: tmp.path().join("h2/.codex"),
        bin_dir: tmp.path().join("h2/.local/bin"),
    };
    support::write(&layout2.command(), &support::fake_codex("0.150.1"));
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(layout2.command(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    s.install_standalone(&v("0.163.0"), &layout2).await.unwrap();
    let r = s.rollback_standalone(&layout2).await.unwrap();
    assert_eq!(r.version, v("0.150.1"));
    assert!(
        !std::fs::symlink_metadata(layout2.command())
            .unwrap()
            .is_symlink()
    );
    // Nothing kept at all: not found.
    let layout3 = StandaloneLayout {
        codex_home: tmp.path().join("h3/.codex"),
        bin_dir: tmp.path().join("h3/.local/bin"),
    };
    assert!(matches!(
        s.rollback_standalone(&layout3).await,
        Err(Error::NotFound { .. })
    ));
}
