//! Superba self-update against a local mock release API and asset server.

mod support;

use std::path::{Path, PathBuf};

use superba_core::{
    DataDir, NetworkSettings, SelfInstallOutcome, SelfUpdateStatus, Timestamp, UpdateChannel,
};
use superba_versions::selfupdate::release::{Os, Platform, SUMS_FILE};
use superba_versions::selfupdate::{Endpoints, SelfUpdater};
use support::{Mock, Route, sha256_hex, targz};

const LINUX: Platform = Platform {
    os: Os::Linux,
    arch: "x86_64",
    musl: false,
};

struct Fixture {
    mock: Mock,
    data: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        Self {
            mock: Mock::start().await,
            data: tempfile::tempdir().unwrap(),
        }
    }

    fn updater(&self) -> SelfUpdater {
        SelfUpdater::new(&DataDir::new(self.data.path()), NetworkSettings::default())
            .with_endpoints(Endpoints {
                api: self.mock.url("/api"),
                downloads: self.mock.url("/dl"),
                proxies: Vec::new(),
                downloads_overridden: true,
            })
            .with_platform(LINUX)
            .with_current("0.1.0")
    }
}

fn release_json(tag: &str, prerelease: bool, with_asset: bool, size: u64) -> String {
    let version = tag.trim_start_matches('v');
    let mut assets = vec![format!(r#"{{"name":"{SUMS_FILE}","size":100}}"#)];
    if with_asset {
        assets.push(format!(
            r#"{{"name":"superba-{version}-linux-x86_64.tar.gz","size":{size}}}"#
        ));
    }
    format!(
        r#"{{"tag_name":"{tag}","draft":false,"prerelease":{prerelease},"body":"Notes for {tag}\r\n- one","published_at":"2026-10-17T00:00:00Z","html_url":"https://evil.example/x","assets":[{}]}}"#,
        assets.join(",")
    )
}

/// The Linux package of `version` and the SHA256SUMS that vouches for it.
fn package(version: &str) -> (Vec<u8>, String) {
    let top = format!("superba-{version}-linux-x86_64");
    let script = |name: &str| format!("#!/bin/sh\necho \"{name} {version}\"\n");
    let (g, s, r) = (script("superba"), script("spb"), script("superba-router"));
    let paths = [
        format!("{top}/superba"),
        format!("{top}/spb"),
        format!("{top}/superba-router"),
    ];
    let tar = targz(
        &[
            (&paths[0], g.as_bytes(), 0o755),
            (&paths[1], s.as_bytes(), 0o755),
            (&paths[2], r.as_bytes(), 0o755),
            (&format!("{top}/README.md"), b"readme", 0o644),
        ],
        &[],
    );
    let sums = format!("{}  {top}.tar.gz\n", sha256_hex(&tar));
    (tar, sums)
}

fn serve_release(f: &Fixture, tag: &str, prerelease: bool) -> Vec<u8> {
    let version = tag.trim_start_matches('v');
    let (tar, sums) = package(version);
    let asset = format!("superba-{version}-linux-x86_64.tar.gz");
    f.mock.route(
        "/api/releases/latest",
        Route::ok(release_json(tag, prerelease, true, tar.len() as u64))
            .header("ETag", &format!("\"etag-{tag}\"")),
    );
    f.mock
        .route(&format!("/dl/{tag}/{asset}"), Route::ok(tar.clone()));
    f.mock
        .route(&format!("/dl/{tag}/{SUMS_FILE}"), Route::ok(sums));
    tar
}

fn install_dir(parent: &Path, version: &str) -> PathBuf {
    let dir = parent.join("install");
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["superba", "spb", "superba-router"] {
        let p = dir.join(n);
        std::fs::write(&p, format!("#!/bin/sh\necho \"{n} {version}\"\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    dir
}

#[tokio::test]
async fn finds_a_newer_release_and_renders_its_notes() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let view = f.updater().check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::Available);
    assert_eq!(view.current, "0.1.0");
    let latest = view.latest.unwrap();
    assert_eq!(latest.version, "0.2.0");
    assert_eq!(latest.tag, "v0.2.0");
    assert!(!latest.prerelease);
    assert_eq!(latest.notes, "Notes for v0.2.0\n- one", "CRLF normalised");
    assert_eq!(
        latest.asset.as_deref(),
        Some("superba-0.2.0-linux-x86_64.tar.gz")
    );
    assert_eq!(
        latest.url, "https://github.com/krill-lab/superba/releases/tag/v0.2.0",
        "the link is built from the tag, not taken from the API"
    );
    assert!(view.checked_at.is_some());
    assert!(!view.no_package);
}

#[tokio::test]
async fn up_to_date_when_the_release_is_not_newer() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.1.0", false);
    let view = f.updater().check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::UpToDate);
    // A running pre-release is older than its release (on Stable; a
    // pre-release build follows Preview unless Stable is chosen).
    let view = f
        .updater()
        .with_current("0.1.0-rc.1")
        .with_preferences(true, Some(UpdateChannel::Stable))
        .check(true)
        .await;
    assert_eq!(view.status, SelfUpdateStatus::Available);
}

#[tokio::test]
async fn private_repository_is_a_calm_state_not_an_error() {
    for status in [404_u16, 401, 403] {
        let f = Fixture::new().await;
        f.mock.route("/api/releases/latest", Route::status(status));
        let u = f.updater();
        let view = u.check(true).await;
        assert_eq!(view.status, SelfUpdateStatus::PageUnavailable, "{status}");
        assert!(view.latest.is_none());
        assert!(view.detail.is_none());
        assert!(view.checked_at.is_some());
        // An automatic check right after does not ask again (no retry loop).
        let before = f.mock.hits("/api");
        let again = u.check(false).await;
        assert_eq!(again.status, SelfUpdateStatus::PageUnavailable);
        assert_eq!(f.mock.hits("/api"), before, "{status}: cached for 24 h");
        assert!(!again.offered());
    }
}

#[tokio::test]
async fn rate_limit_and_server_errors_are_check_failed_and_retried_later() {
    let f = Fixture::new().await;
    f.mock.route(
        "/api/releases/latest",
        Route::status(403).header("x-ratelimit-remaining", "0"),
    );
    let t0 = Timestamp::from_millis(1_800_000_000_000);
    let u = f.updater().with_now(t0);
    let view = u.check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::CheckFailed);
    assert_eq!(view.detail.as_deref(), Some("rate limited"));
    let hits = f.mock.hits("/api");
    // Within the hour nothing is asked; after it, it is.
    u.check(false).await;
    assert_eq!(f.mock.hits("/api"), hits);
    let later = f
        .updater()
        .with_now(Timestamp::from_millis(t0.as_millis() + 3_700_000));
    later.check(false).await;
    assert_eq!(f.mock.hits("/api"), hits + 1);

    f.mock.route("/api/releases/latest", Route::status(500));
    let view = f.updater().check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::CheckFailed);
    assert_eq!(view.detail.as_deref(), Some("HTTP 500"));
}

#[tokio::test]
async fn an_unreachable_server_fails_quietly() {
    let f = Fixture::new().await;
    let dead = {
        let m = Mock::start().await;
        m.url("/api")
    };
    let u = f.updater().with_endpoints(Endpoints {
        api: dead,
        ..Endpoints::default()
    });
    let view = u.check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::CheckFailed);
}

#[tokio::test]
async fn etag_makes_the_second_check_conditional() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let u = f.updater();
    u.check(true).await;
    let view = u.check(true).await;
    assert_eq!(
        view.status,
        SelfUpdateStatus::Available,
        "304 keeps the answer"
    );
    let log = f.mock.log();
    assert_eq!(log[0].if_none_match, None);
    assert_eq!(log[1].if_none_match.as_deref(), Some("\"etag-v0.2.0\""));
}

#[tokio::test]
async fn automatic_checks_follow_the_preference_and_the_interval() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let t0 = Timestamp::from_millis(1_800_000_000_000);
    // Off: an automatic check does nothing, a manual one still works.
    let off = f.updater().with_now(t0).with_preferences(false, None);
    assert_eq!(off.check(false).await.status, SelfUpdateStatus::NotChecked);
    assert_eq!(f.mock.hits("/api"), 0);
    assert_eq!(off.check(true).await.status, SelfUpdateStatus::Available);
    assert_eq!(f.mock.hits("/api"), 1);
    // On: within 24 h it is cached, after 24 h it asks again.
    let on = f
        .updater()
        .with_now(Timestamp::from_millis(t0.as_millis() + 3_600_000));
    on.check(false).await;
    assert_eq!(f.mock.hits("/api"), 1);
    let day = f
        .updater()
        .with_now(Timestamp::from_millis(t0.as_millis() + 25 * 3_600_000));
    day.check(false).await;
    assert_eq!(f.mock.hits("/api"), 2);
}

#[tokio::test]
async fn online_checks_off_sends_nothing_unless_the_user_asks() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let net = NetworkSettings {
        online_checks: false,
        ..NetworkSettings::default()
    };
    let u = SelfUpdater::new(&DataDir::new(f.data.path()), net)
        .with_current("0.1.0")
        .with_endpoints(f.updater().endpoints().clone())
        .with_platform(LINUX);
    assert_eq!(
        u.check(false).await.status,
        SelfUpdateStatus::OnlineChecksOff
    );
    assert_eq!(f.mock.hits("/api"), 0, "no automatic check");
    assert_eq!(u.view().status, SelfUpdateStatus::OnlineChecksOff);
    // An explicit check still goes out ("checks only when you ask").
    assert_eq!(u.check(true).await.status, SelfUpdateStatus::Available);
    assert_eq!(f.mock.hits("/api"), 1);
}

#[tokio::test]
async fn skipping_a_version_persists_until_a_newer_one_appears() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let u = f.updater();
    let view = u.check(true).await;
    assert!(view.offered());
    let view = u.skip(Some("0.2.0".into()));
    assert!(view.is_skipped());
    assert!(!view.offered(), "a skipped version is not offered");
    assert_eq!(view.status, SelfUpdateStatus::Available);
    // A new updater (a new process) still sees the skip.
    assert!(f.updater().view().is_skipped());
    // The tag form is normalised.
    assert_eq!(
        f.updater().skip(Some("v0.2.0".into())).skipped.as_deref(),
        Some("0.2.0")
    );
    // A newer release is offered again.
    serve_release(&f, "v0.3.0", false);
    let view = f.updater().check(true).await;
    assert_eq!(view.latest.as_ref().unwrap().version, "0.3.0");
    assert!(view.offered());
    // Clearing the skip.
    let view = f.updater().skip(None);
    assert_eq!(view.skipped, None);
}

#[tokio::test]
async fn the_preview_channel_reads_the_release_list() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let list = |extra: &str| {
        format!(
            "[{extra}{},{},{},{}]",
            release_json("v0.3.0-rc.1", true, true, 10),
            release_json("v0.3.0-beta.2", true, true, 10),
            r#"{"tag_name":"v9.9.9-rc.1","draft":true,"prerelease":true,"assets":[]}"#,
            release_json("v0.2.0", false, true, 10),
        )
    };
    f.mock.route(
        "/api/releases",
        Route::ok(list("")).header("ETag", "\"list\""),
    );
    // A stable build follows Stable: releases/latest only.
    let stable_only = f.updater().check(true).await;
    assert_eq!(stable_only.latest.as_ref().unwrap().version, "0.2.0");
    assert_eq!(
        f.mock.hits("/api/releases?"),
        0,
        "the list is not requested"
    );

    let preview = Some(UpdateChannel::Preview);
    let view = f
        .updater()
        .with_preferences(true, preview)
        .check(true)
        .await;
    let latest = view.latest.clone().unwrap();
    assert_eq!(
        latest.version, "0.3.0-rc.1",
        "newest release incl. pre-releases, drafts ignored"
    );
    assert!(latest.prerelease);
    assert_eq!(view.status, SelfUpdateStatus::Available);
    assert_eq!(view.channel, UpdateChannel::Preview);
    assert_eq!(view.latest_stable.unwrap().version, "0.2.0");
    assert_eq!(
        f.mock.hits("/api/releases/latest"),
        1,
        "the list holds a final release: releases/latest is not asked again"
    );
    // Back on Stable the cache answers without asking.
    let off = f.updater().view();
    assert_eq!(off.latest.unwrap().version, "0.2.0");
    // A final release newer than every pre-release wins on Preview too.
    f.mock.route(
        "/api/releases",
        Route::ok(list(&format!(
            "{},",
            release_json("v0.4.0", false, true, 10)
        )))
        .header("ETag", "\"list2\""),
    );
    let view = f
        .updater()
        .with_preferences(true, preview)
        .check(true)
        .await;
    assert_eq!(view.latest.unwrap().version, "0.4.0");
}

#[tokio::test]
async fn a_release_without_a_package_for_this_system_cannot_be_installed() {
    let f = Fixture::new().await;
    f.mock.route(
        "/api/releases/latest",
        Route::ok(release_json("v0.2.0", false, false, 0)),
    );
    let u = f.updater();
    let view = u.check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::Available);
    assert!(view.no_package);
    let e = u.install(false).await.unwrap_err();
    assert!(e.to_string().contains("package for this system"), "{e}");
}

#[tokio::test]
async fn download_is_verified_against_sha256sums_of_the_same_release() {
    let f = Fixture::new().await;
    let tar = serve_release(&f, "v0.2.0", false);
    let u = f.updater();
    let view = u.check(true).await;
    let release = view.latest.unwrap();
    let asset = release.asset.clone().unwrap();
    let path = u.download(&release, &asset).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), tar);

    // Wrong checksum: refused, nothing is kept.
    let f2 = Fixture::new().await;
    serve_release(&f2, "v0.2.0", false);
    f2.mock.route(
        &format!("/dl/v0.2.0/{SUMS_FILE}"),
        Route::ok(format!("{}  {asset}\n", "0".repeat(64))),
    );
    let u2 = f2.updater();
    let release = u2.check(true).await.latest.unwrap();
    let e = u2.download(&release, &asset).await.unwrap_err();
    assert!(e.to_string().contains("does not match"), "{e}");
    let dir = f2
        .data
        .path()
        .join("downloads/superba-update/superba-0.2.0");
    assert!(!dir.join(&asset).exists());
    assert!(!dir.join(format!("{asset}.part")).exists());

    // No entry for the asset in SHA256SUMS.txt: refused before downloading.
    let f3 = Fixture::new().await;
    serve_release(&f3, "v0.2.0", false);
    f3.mock.route(
        &format!("/dl/v0.2.0/{SUMS_FILE}"),
        Route::ok("abc  other.tar.gz\n"),
    );
    let u3 = f3.updater();
    let release = u3.check(true).await.latest.unwrap();
    let e = u3.download(&release, &asset).await.unwrap_err();
    assert!(e.to_string().contains("checksum"), "{e}");
    assert_eq!(f3.mock.hits(&format!("/dl/v0.2.0/{asset}")), 0);
}

#[tokio::test]
async fn download_refuses_a_size_that_differs_from_the_release_listing() {
    let f = Fixture::new().await;
    let (tar, sums) = package("0.2.0");
    let asset = "superba-0.2.0-linux-x86_64.tar.gz";
    f.mock.route(
        "/api/releases/latest",
        Route::ok(release_json("v0.2.0", false, true, tar.len() as u64 + 7)),
    );
    f.mock.route(&format!("/dl/v0.2.0/{asset}"), Route::ok(tar));
    f.mock
        .route(&format!("/dl/v0.2.0/{SUMS_FILE}"), Route::ok(sums));
    let u = f.updater();
    let release = u.check(true).await.latest.unwrap();
    let e = u.download(&release, asset).await.unwrap_err();
    assert!(e.to_string().contains("bytes"), "{e}");
}

#[tokio::test]
async fn download_rejects_package_names_that_climb_out() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let u = f.updater();
    let release = u.check(true).await.latest.unwrap();
    for bad in ["../x.tar.gz", "a/b.tar.gz", ".hidden", "a\\b"] {
        assert!(u.download(&release, bad).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn third_party_proxies_are_opt_in_and_never_serve_checksums() {
    let asset = "superba-0.2.0-linux-x86_64.tar.gz";
    let proxy = Mock::start().await;
    let f = Fixture::new().await;
    let tar = serve_release(&f, "v0.2.0", false);
    // The direct download fails; the proxy serves the file under the exact path.
    f.mock
        .route(&format!("/dl/v0.2.0/{asset}"), Route::status(500));
    let direct = format!("{}/v0.2.0/{asset}", f.mock.url("/dl"));
    proxy.route(&format!("/{direct}"), Route::ok(tar.clone()));
    let updater = |proxies_on: bool| {
        let net = NetworkSettings {
            third_party_proxies: proxies_on,
            ..NetworkSettings::default()
        };
        SelfUpdater::new(&DataDir::new(f.data.path()), net)
            .with_current("0.1.0")
            .with_endpoints(Endpoints {
                api: f.mock.url("/api"),
                downloads: f.mock.url("/dl"),
                proxies: vec![format!("{}/", proxy.base)],
                downloads_overridden: false,
            })
            .with_platform(LINUX)
    };
    let off = updater(false);
    let release = off.check(true).await.latest.unwrap();
    assert!(off.download(&release, asset).await.is_err());
    assert_eq!(proxy.log().len(), 0, "proxies are opt-in");

    let on = updater(true);
    let path = on.download(&release, asset).await.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), tar);
    let seen = proxy.log();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].path.ends_with(asset));
    assert!(
        !seen.iter().any(|s| s.path.contains(SUMS_FILE)),
        "checksums come from GitHub only"
    );

    // A proxy that serves tampered bytes is refused by the checksum.
    let evil = Mock::start().await;
    evil.route(&format!("/{direct}"), Route::ok(b"tampered".to_vec()));
    let net = NetworkSettings {
        third_party_proxies: true,
        ..NetworkSettings::default()
    };
    let other = tempfile::tempdir().unwrap();
    let u = SelfUpdater::new(&DataDir::new(other.path()), net)
        .with_current("0.1.0")
        .with_endpoints(Endpoints {
            api: f.mock.url("/api"),
            downloads: f.mock.url("/dl"),
            proxies: vec![format!("{}/", evil.base)],
            downloads_overridden: false,
        })
        .with_platform(LINUX);
    assert!(u.download(&release, asset).await.is_err());
}

#[tokio::test]
async fn linux_install_replaces_the_binaries_in_a_temp_install_dir() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let dir = install_dir(f.data.path(), "0.1.0");
    let u = f.updater().with_exe(dir.join("spb"));
    let view = u.check(true).await;
    assert!(view.offered());

    let result = u.install(false).await.unwrap();
    assert_eq!(result.outcome, SelfInstallOutcome::Installed);
    assert_eq!(result.version, "0.2.0");
    let mut replaced = result.replaced.clone();
    replaced.sort();
    assert_eq!(replaced, ["spb", "superba", "superba-router"]);
    assert!(result.finish.is_none(), "no relaunch asked for");
    for n in ["superba", "spb", "superba-router"] {
        let text = std::fs::read_to_string(dir.join(n)).unwrap();
        assert_eq!(text, format!("#!/bin/sh\necho \"{n} 0.2.0\"\n"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("spb"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 3, "no temporary files left: {names:?}");
    // README is not an installed program: it was not copied.
    assert!(!dir.join("README.md").exists());
    // The package and staging directory are removed after a successful install.
    let dl = f.data.path().join("downloads/superba-update/superba-0.2.0");
    assert!(!dl.join("superba-0.2.0-linux-x86_64.tar.gz").exists());
    assert!(std::fs::read_dir(&dl).map_or(true, |mut r| r.next().is_none()));

    // Running as 0.2.0 now: nothing newer, install refuses.
    let again = f.updater().with_current("0.2.0").with_exe(dir.join("spb"));
    assert_eq!(again.check(true).await.status, SelfUpdateStatus::UpToDate);
    assert!(again.install(false).await.is_err());
}

#[tokio::test]
async fn linux_install_asks_for_a_relaunch_of_the_gui_when_requested() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let dir = install_dir(f.data.path(), "0.1.0");
    let u = f.updater().with_exe(dir.join("superba"));
    let result = u.install(true).await.unwrap();
    let plan = result.finish.expect("a second stage");
    assert_eq!(
        plan.exe,
        std::fs::canonicalize(&dir).unwrap().join("superba")
    );
    assert_eq!(plan.args.relaunch.as_deref(), Some(plan.exe.as_path()));
    assert_eq!(plan.args.wait_pid, Some(std::process::id()));
}

#[tokio::test]
async fn a_package_that_is_checksum_valid_but_unsafe_is_refused_without_touching_the_install() {
    let f = Fixture::new().await;
    let version = "0.2.0";
    let top = format!("superba-{version}-linux-x86_64");
    let tar = targz(
        &[(&format!("{top}/spb"), b"#!/bin/sh\necho spb 0.2.0\n", 0o755)],
        &[(&format!("{top}/superba"), "/etc/passwd")],
    );
    let asset = format!("{top}.tar.gz");
    f.mock.route(
        "/api/releases/latest",
        Route::ok(release_json("v0.2.0", false, true, tar.len() as u64)),
    );
    f.mock
        .route(&format!("/dl/v0.2.0/{asset}"), Route::ok(tar.clone()));
    f.mock.route(
        &format!("/dl/v0.2.0/{SUMS_FILE}"),
        Route::ok(format!("{}  {asset}\n", sha256_hex(&tar))),
    );
    let dir = install_dir(f.data.path(), "0.1.0");
    let u = f.updater().with_exe(dir.join("spb"));
    let e = u.install(false).await.unwrap_err();
    assert!(e.to_string().contains("link"), "{e}");
    for n in ["superba", "spb", "superba-router"] {
        let text = std::fs::read_to_string(dir.join(n)).unwrap();
        assert!(text.contains("0.1.0"), "{n} untouched");
    }
}

#[tokio::test]
async fn a_new_spb_that_reports_another_version_is_refused() {
    // The new spb is only started when the package is for the running machine
    // (a Linux package cannot run on macOS or Windows), so this Linux fixture
    // exercises the version check only on Linux x86_64.
    if Platform::current() != LINUX {
        return;
    }
    let f = Fixture::new().await;
    let version = "0.2.0";
    let top = format!("superba-{version}-linux-x86_64");
    let tar = targz(
        &[
            (&format!("{top}/spb"), b"#!/bin/sh\necho spb 0.9.9\n", 0o755),
            (&format!("{top}/superba"), b"#!/bin/sh\n", 0o755),
        ],
        &[],
    );
    let asset = format!("{top}.tar.gz");
    f.mock.route(
        "/api/releases/latest",
        Route::ok(release_json("v0.2.0", false, true, tar.len() as u64)),
    );
    f.mock
        .route(&format!("/dl/v0.2.0/{asset}"), Route::ok(tar.clone()));
    f.mock.route(
        &format!("/dl/v0.2.0/{SUMS_FILE}"),
        Route::ok(format!("{}  {asset}\n", sha256_hex(&tar))),
    );
    let dir = install_dir(f.data.path(), "0.1.0");
    let e = f
        .updater()
        .with_exe(dir.join("spb"))
        .install(false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("reports"), "{e}");
    assert!(
        std::fs::read_to_string(dir.join("spb"))
            .unwrap()
            .contains("0.1.0")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unwritable_install_dir_ends_in_manual_steps_with_the_verified_package() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let dir = install_dir(f.data.path(), "0.1.0");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::File::create(dir.join(".probe")).is_ok() {
        // Running as root: permissions do not apply, nothing to test.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let u = f.updater().with_exe(dir.join("spb"));
    let result = u.install(false).await.unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(result.outcome, SelfInstallOutcome::Manual);
    let package = result.package.unwrap();
    assert!(package.is_file(), "the verified package is kept");
    assert!(result.steps.len() >= 2);
    assert!(
        result
            .steps
            .iter()
            .any(|s| s.key == "selfupdate.manual.linux")
    );
    assert!(
        std::fs::read_to_string(dir.join("spb"))
            .unwrap()
            .contains("0.1.0")
    );
}

#[tokio::test]
async fn cleanup_removes_the_backup_once_the_new_version_runs() {
    let f = Fixture::new().await;
    let backup = f.data.path().join(".Superba.app.previous");
    std::fs::create_dir_all(backup.join("Contents")).unwrap();
    let state = serde_json::json!({
        "cleanup": {"path": backup, "version": "0.2.0"}
    });
    std::fs::write(f.data.path().join("selfupdate.json"), state.to_string()).unwrap();
    // Still the old version: the backup stays.
    f.updater()
        .with_exe(f.data.path().join("spb"))
        .check(false)
        .await;
    assert!(backup.exists());
    // The new version has started: it goes.
    f.updater()
        .with_current("0.2.0")
        .with_exe(f.data.path().join("spb"))
        .check(false)
        .await;
    assert!(!backup.exists());
}

#[tokio::test]
async fn windows_installs_through_the_msi_handoff_and_never_swaps_files() {
    const WINDOWS: Platform = Platform {
        os: Os::Windows,
        arch: "x86_64",
        musl: false,
    };
    let f = Fixture::new().await;
    let msi = b"not really an msi".to_vec();
    let asset = "superba-0.2.0-windows-x86_64.msi";
    let json = format!(
        r#"{{"tag_name":"v0.2.0","draft":false,"prerelease":false,"body":"n","assets":[{{"name":"{SUMS_FILE}","size":9}},{{"name":"{asset}","size":{}}}]}}"#,
        msi.len()
    );
    f.mock.route("/api/releases/latest", Route::ok(json));
    f.mock
        .route(&format!("/dl/v0.2.0/{asset}"), Route::ok(msi.clone()));
    f.mock.route(
        &format!("/dl/v0.2.0/{SUMS_FILE}"),
        Route::ok(format!("{}  {asset}\n", sha256_hex(&msi))),
    );
    let dir = f.data.path().join("Programs/Superba");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("superba.exe"), b"old gui").unwrap();
    let u = f
        .updater()
        .with_platform(WINDOWS)
        .with_exe(dir.join("superba.exe"));
    let result = u.install(true).await.unwrap();
    assert_eq!(result.outcome, SelfInstallOutcome::Handoff);
    let plan = result.finish.expect("the second stage runs msiexec");
    assert_eq!(std::fs::read(plan.args.msi.as_ref().unwrap()).unwrap(), msi);
    assert_eq!(
        plan.args.relaunch.as_deref(),
        Some(
            std::fs::canonicalize(dir.join("superba.exe"))
                .unwrap()
                .as_path()
        )
    );
    assert!(
        plan.exe.is_file() && !plan.exe.starts_with(&dir),
        "the helper lives outside the install folder"
    );
    assert_eq!(
        std::fs::read(dir.join("superba.exe")).unwrap(),
        b"old gui",
        "nothing is swapped here"
    );
}

#[tokio::test]
async fn an_install_logs_its_steps_and_records_the_attempt() {
    use superba_versions::selfupdate::record::Stage;
    use superba_versions::selfupdate::state::State;
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let dir = install_dir(f.data.path(), "0.1.0");
    let u = f.updater().with_exe(dir.join("superba"));
    let result = u.install(true).await.unwrap();
    let log_path = f.data.path().join("logs").join("selfupdate.log");
    assert_eq!(result.log, log_path);
    let log = std::fs::read_to_string(&log_path).unwrap();
    for want in [
        "Superba 0.1.0 -> 0.2.0 (v0.2.0)",
        "downloading http://127.0.0.1",
        "/dl/v0.2.0/superba-0.2.0-linux-x86_64.tar.gz (direct)",
        "downloaded and verified ",
        "(SHA-256 ",
        "install layout: Files",
        "extracted into ",
        "replaced [",
        "Installed: replaced [",
        "second stage: ",
        "--finish-update",
        "--state ",
        "selfupdate.json",
    ] {
        assert!(log.contains(want), "{want:?} missing in:\n{log}");
    }
    let plan = result.finish.expect("a second stage");
    assert_eq!(plan.args.log.as_deref(), Some(log_path.as_path()));
    assert_eq!(
        plan.args.state.as_deref(),
        Some(f.data.path().join("selfupdate.json").as_path())
    );
    let attempt = State::load(&f.data.path().join("selfupdate.json"))
        .last_attempt
        .unwrap();
    assert_eq!(
        (attempt.from.as_str(), attempt.to.as_str()),
        ("0.1.0", "0.2.0")
    );
    assert_eq!(attempt.stage, Stage::Installed);
    assert_eq!(u.view().problem, None, "installed, waiting for the restart");
}

#[tokio::test]
async fn a_failed_install_is_logged_and_recorded_as_seen() {
    use superba_versions::selfupdate::record::Stage;
    use superba_versions::selfupdate::state::State;
    let f = Fixture::new().await;
    let tar = b"not the package".to_vec();
    let asset = "superba-0.2.0-linux-x86_64.tar.gz";
    f.mock.route(
        "/api/releases/latest",
        Route::ok(release_json("v0.2.0", false, true, tar.len() as u64)),
    );
    f.mock.route(&format!("/dl/v0.2.0/{asset}"), Route::ok(tar));
    f.mock.route(
        &format!("/dl/v0.2.0/{SUMS_FILE}"),
        Route::ok(format!("{}  {asset}\n", "0".repeat(64))),
    );
    let dir = install_dir(f.data.path(), "0.1.0");
    let u = f.updater().with_exe(dir.join("spb"));
    let e = u.install(false).await.unwrap_err();
    let log = std::fs::read_to_string(f.data.path().join("logs/selfupdate.log")).unwrap();
    assert!(log.contains("download from direct failed: "), "{log}");
    assert!(log.contains(&format!("FAILED: {e}")), "{log}");
    let attempt = State::load(&f.data.path().join("selfupdate.json"))
        .last_attempt
        .unwrap();
    assert_eq!(attempt.stage, Stage::Failed);
    assert!(attempt.seen, "the window that started it showed the error");
    let problem = u.view().problem.expect("on the Updates page");
    assert!(problem.seen && !problem.installed);
}

#[tokio::test]
async fn cancelled_preparation_keeps_the_installed_binaries_and_no_failure_notice() {
    let f = Fixture::new().await;
    serve_release(&f, "v0.2.0", false);
    let install = install_dir(f.data.path(), "0.1.0");
    let before = std::fs::read(install.join("superba")).unwrap();
    let control = std::sync::Arc::new(superba_core::UpdateControl::default());
    assert!(control.cancel());
    let updater = f
        .updater()
        .with_exe(install.join("superba"))
        .with_control(control);
    assert!(matches!(
        updater.install(false).await,
        Err(superba_core::Error::Cancelled)
    ));
    assert_eq!(std::fs::read(install.join("superba")).unwrap(), before);
    assert!(updater.view().problem.is_none());
}
