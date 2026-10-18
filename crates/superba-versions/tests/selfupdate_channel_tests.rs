//! Superba self-update channels (user bug 73): Stable reads `releases/latest`,
//! Preview the release list ordered by semver precedence; the running build
//! picks the default; an older version is never offered, only installed when
//! named; GitHub's rate limit is respected.

mod support;

use std::path::{Path, PathBuf};

use superba_core::{
    DataDir, Error, NetworkSettings, SelfInstallOutcome, SelfUpdateStatus, Timestamp, UpdateChannel,
};
use superba_versions::selfupdate::record::{Attempt, Stage};
use superba_versions::selfupdate::release::{Os, Platform, SUMS_FILE, parse_list};
use superba_versions::selfupdate::{Endpoints, SelfUpdater};
use support::{Mock, Route, sha256_hex, targz};

const LINUX: Platform = Platform {
    os: Os::Linux,
    arch: "x86_64",
    musl: false,
};

const T0: i64 = 1_800_000_000_000;

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

    /// An updater running `current`, with the channel left to the build.
    fn updater(&self, current: &str) -> SelfUpdater {
        SelfUpdater::new(&DataDir::new(self.data.path()), NetworkSettings::default())
            .with_endpoints(Endpoints {
                api: self.mock.url("/api"),
                downloads: self.mock.url("/dl"),
                proxies: Vec::new(),
                downloads_overridden: true,
            })
            .with_platform(LINUX)
            .with_current(current)
            .with_now(Timestamp::from_millis(T0))
    }

    fn on(&self, current: &str, channel: UpdateChannel) -> SelfUpdater {
        self.updater(current).with_preferences(true, Some(channel))
    }

    fn latest(&self, tag: &str) {
        self.mock.route(
            "/api/releases/latest",
            Route::ok(release(tag, false)).header("ETag", &format!("\"latest-{tag}\"")),
        );
    }

    fn list(&self, tags: &[(&str, bool)], etag: &str) {
        let items: Vec<String> = tags.iter().map(|(t, pre)| release(t, *pre)).collect();
        self.mock.route(
            "/api/releases",
            Route::ok(format!("[{}]", items.join(","))).header("ETag", etag),
        );
    }
}

fn release(tag: &str, prerelease: bool) -> String {
    let version = tag.trim_start_matches('v');
    format!(
        r#"{{"tag_name":"{tag}","draft":false,"prerelease":{prerelease},"body":"n","published_at":"2026-10-17T00:00:00Z","assets":[{{"name":"{SUMS_FILE}","size":9}},{{"name":"superba-{version}-linux-x86_64.tar.gz","size":10}}]}}"#
    )
}

#[test]
fn the_list_is_ordered_by_semver_precedence_not_by_publication() {
    // Newest first by creation, as GitHub lists them, but deliberately shuffled.
    let body = format!(
        "[{},{},{},{},{},{},{},{}]",
        release("v0.2.0-alpha.9", true),
        release("v0.2.0-rc.1", true),
        release("v0.2.0-alpha.10", true),
        release("v0.1.0", false),
        release("v0.2.0-beta.1", true),
        r#"{"tag_name":"v9.0.0","draft":true,"prerelease":false,"assets":[]}"#,
        r#"{"tag_name":"nightly-2026","draft":false,"prerelease":true,"assets":[]}"#,
        release("v0.1.1", false),
    );
    let answer = parse_list(body.as_bytes(), &LINUX).unwrap();
    assert_eq!(
        answer.newest.unwrap().version,
        "0.2.0-rc.1",
        "alpha < beta < rc; drafts and foreign tags are skipped"
    );
    assert_eq!(answer.newest_stable.unwrap().version, "0.1.1");

    let body = format!(
        "[{},{},{}]",
        release("v0.2.0-alpha.9", true),
        release("v0.2.0-alpha.10", true),
        release("v0.2.0", false),
    );
    let answer = parse_list(body.as_bytes(), &LINUX).unwrap();
    assert_eq!(
        answer.newest.unwrap().version,
        "0.2.0",
        "final beats its rc"
    );
    let body = format!(
        "[{},{}]",
        release("v0.2.0-alpha.9", true),
        release("v0.2.0-alpha.10", true)
    );
    let answer = parse_list(body.as_bytes(), &LINUX).unwrap();
    assert_eq!(
        answer.newest.unwrap().version,
        "0.2.0-alpha.10",
        "numeric parts compare as numbers"
    );
    assert!(answer.newest_stable.is_none());
    // A final tag GitHub marks as pre-release belongs to Preview only.
    let body = format!("[{}]", release("v0.3.0", true));
    let answer = parse_list(body.as_bytes(), &LINUX).unwrap();
    assert_eq!(answer.newest.unwrap().version, "0.3.0");
    assert!(answer.newest_stable.is_none());
    assert!(parse_list(b"{}", &LINUX).is_none(), "not a list");
}

#[tokio::test]
async fn the_channel_follows_the_running_build_until_the_user_chooses() {
    let f = Fixture::new().await;
    assert_eq!(f.updater("0.1.0").channel(), UpdateChannel::Stable);
    assert_eq!(f.updater("0.1.1-alpha.3").channel(), UpdateChannel::Preview);
    assert_eq!(f.updater("0.2.0-rc.1").channel(), UpdateChannel::Preview);
    assert_eq!(
        f.on("0.1.1-alpha.3", UpdateChannel::Stable).channel(),
        UpdateChannel::Stable
    );
    assert_eq!(
        f.on("0.1.0", UpdateChannel::Preview).channel(),
        UpdateChannel::Preview
    );
    let view = f.updater("0.1.1-alpha.3").view();
    assert_eq!(view.channel, UpdateChannel::Preview);
    assert!(!view.channel_chosen);
    assert!(f.on("0.1.0", UpdateChannel::Stable).view().channel_chosen);
    // The same rule in the core type the GUI and the CLI use.
    assert_eq!(
        UpdateChannel::for_version("0.1.1-alpha.3"),
        UpdateChannel::Preview
    );
    assert_eq!(UpdateChannel::for_version("v0.1.0"), UpdateChannel::Stable);
    assert_eq!(
        UpdateChannel::for_version("0.1.0+build.5"),
        UpdateChannel::Stable
    );
}

#[tokio::test]
async fn a_preview_build_is_offered_the_next_alpha() {
    // The reported case: 0.1.1-alpha.3 saw "Latest 0.1.0 · Up to date".
    let f = Fixture::new().await;
    f.latest("v0.1.0");
    f.list(
        &[
            ("v0.1.1-alpha.4", true),
            ("v0.1.1-alpha.3", true),
            ("v0.1.0", false),
        ],
        "\"l1\"",
    );
    let view = f.updater("0.1.1-alpha.3").check(true).await;
    assert_eq!(view.channel, UpdateChannel::Preview);
    assert_eq!(view.status, SelfUpdateStatus::Available);
    assert!(view.offered());
    assert_eq!(view.latest.as_ref().unwrap().version, "0.1.1-alpha.4");
    assert_eq!(view.latest_preview.unwrap().version, "0.1.1-alpha.4");
    assert_eq!(view.latest_stable.unwrap().version, "0.1.0");
    assert_eq!(f.mock.hits("/api/releases?per_page="), 1);
    assert_eq!(
        f.mock.hits("/api/releases/latest"),
        0,
        "one request: the page has a final release"
    );
}

#[tokio::test]
async fn an_older_version_is_never_offered() {
    let f = Fixture::new().await;
    f.latest("v0.1.0");
    f.list(&[("v0.1.1-alpha.2", true), ("v0.1.0", false)], "\"l1\"");
    // A preview build that chose Stable keeps its build until a newer final one.
    let stable = f.on("0.1.1-alpha.3", UpdateChannel::Stable);
    let view = stable.check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::UpToDate);
    assert!(!view.offered());
    assert_eq!(view.latest.as_ref().unwrap().version, "0.1.0");
    assert_eq!(view.latest_stable.as_ref().unwrap().version, "0.1.0");
    let e = stable.install(false).await.unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "{e}");
    // Preview whose newest is older than the running alpha.
    let view = f.updater("0.1.1-alpha.3").check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::UpToDate);
    // A final release is newer than its own release candidates.
    f.list(&[("v0.2.0-rc.1", true), ("v0.1.0", false)], "\"l2\"");
    let view = f.on("0.2.0", UpdateChannel::Preview).check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::UpToDate);
    // The first final release after the preview build is offered on Stable.
    f.latest("v0.1.1");
    let view = stable.check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::Available);
    assert_eq!(view.latest.unwrap().version, "0.1.1");
}

#[tokio::test]
async fn a_page_of_prereleases_only_also_asks_for_the_latest_final_release() {
    let f = Fixture::new().await;
    f.latest("v0.1.0");
    f.list(
        &[("v0.2.0-beta.1", true), ("v0.2.0-alpha.1", true)],
        "\"l\"",
    );
    let view = f.on("0.1.0", UpdateChannel::Preview).check(true).await;
    assert_eq!(view.latest.unwrap().version, "0.2.0-beta.1");
    assert_eq!(view.latest_stable.unwrap().version, "0.1.0");
    assert_eq!(f.mock.hits("/api/releases/latest"), 1);
    // No release at all: a calm "no release page".
    let f = Fixture::new().await;
    f.mock.route("/api/releases", Route::ok("[]"));
    let view = f.on("0.1.0", UpdateChannel::Preview).check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::PageUnavailable);
}

#[tokio::test]
async fn both_caches_stay_conditional_across_channel_switches() {
    let f = Fixture::new().await;
    f.latest("v0.1.0");
    f.list(&[("v0.2.0-rc.1", true), ("v0.1.0", false)], "\"list\"");
    let preview = f.on("0.1.0", UpdateChannel::Preview);
    let stable = f.on("0.1.0", UpdateChannel::Stable);
    preview.check(true).await;
    stable.check(true).await;
    // Switched back: the cached answer is shown before any request ...
    let cached = preview.view();
    assert_eq!(cached.status, SelfUpdateStatus::Available);
    assert_eq!(cached.latest.unwrap().version, "0.2.0-rc.1");
    // ... and the next request is conditional (a 304 keeps the answer).
    let view = preview.check(true).await;
    assert_eq!(view.latest.unwrap().version, "0.2.0-rc.1");
    let lists: Vec<_> = f
        .mock
        .log()
        .into_iter()
        .filter(|s| s.path.starts_with("/api/releases?"))
        .collect();
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[0].if_none_match, None);
    assert_eq!(lists[1].if_none_match.as_deref(), Some("\"list\""));
    // A channel that was never asked is "not checked", not "up to date".
    let g = Fixture::new().await;
    g.latest("v0.1.0");
    g.on("0.1.0", UpdateChannel::Stable).check(true).await;
    let other = g.on("0.1.0", UpdateChannel::Preview).view();
    assert_eq!(other.status, SelfUpdateStatus::NotChecked);
    // An automatic check after a switch asks at once (not cached for 24 h).
    let before = g.mock.hits("/api/releases?");
    g.list(&[("v0.1.0", false)], "\"x\"");
    g.on("0.1.0", UpdateChannel::Preview).check(false).await;
    assert_eq!(g.mock.hits("/api/releases?"), before + 1);
}

#[tokio::test]
async fn the_rate_limit_reset_holds_back_every_request_and_keeps_the_answer() {
    let f = Fixture::new().await;
    f.latest("v0.2.0");
    let u = f.updater("0.1.0");
    assert_eq!(u.check(true).await.status, SelfUpdateStatus::Available);
    let reset = (T0 / 1000 + 600).to_string();
    f.mock.route(
        "/api/releases/latest",
        Route::status(403)
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", &reset),
    );
    // The ETag check is rate limited: the cached answer stays, with the reason.
    let view = u.check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::Available);
    assert_eq!(view.latest.as_ref().unwrap().version, "0.2.0");
    assert_eq!(view.detail.as_deref(), Some("rate limited"));
    assert_eq!(
        view.limited_until,
        Some(Timestamp::from_millis(T0 + 600_000))
    );
    let hits = f.mock.hits("/api");
    // A check the user asks for inside the window sends nothing either.
    let again = u.check(true).await;
    assert_eq!(f.mock.hits("/api"), hits);
    assert_eq!(again.status, SelfUpdateStatus::Available);
    // After the reset it asks again.
    f.latest("v0.2.0");
    let later = f
        .updater("0.1.0")
        .with_now(Timestamp::from_millis(T0 + 601_000));
    let view = later.check(true).await;
    assert_eq!(f.mock.hits("/api"), hits + 1);
    assert_eq!(view.limited_until, None);
    assert_eq!(view.detail, None);

    // A secondary limit (429 + Retry-After) without a cached answer.
    let g = Fixture::new().await;
    g.mock.route(
        "/api/releases/latest",
        Route::status(429).header("retry-after", "60"),
    );
    let view = g.updater("0.1.0").check(true).await;
    assert_eq!(view.status, SelfUpdateStatus::CheckFailed);
    assert_eq!(
        view.limited_until,
        Some(Timestamp::from_millis(T0 + 60_000))
    );
    // A far reset time never blocks for more than an hour.
    let h = Fixture::new().await;
    h.mock.route(
        "/api/releases/latest",
        Route::status(403)
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", &(T0 / 1000 + 86_400).to_string()),
    );
    let view = h.updater("0.1.0").check(true).await;
    assert_eq!(
        view.limited_until,
        Some(Timestamp::from_millis(T0 + 3_600_000))
    );
}

/// The Linux package of `version`, its SHA256SUMS and the tag document.
fn serve_tag(f: &Fixture, tag: &str) {
    let version = tag.trim_start_matches('v');
    let top = format!("superba-{version}-linux-x86_64");
    let script = |name: &str| format!("#!/bin/sh\necho \"{name} {version}\"\n");
    let (g, s, r) = (script("superba"), script("spb"), script("superba-router"));
    let tar = targz(
        &[
            (&format!("{top}/superba"), g.as_bytes(), 0o755),
            (&format!("{top}/spb"), s.as_bytes(), 0o755),
            (&format!("{top}/superba-router"), r.as_bytes(), 0o755),
        ],
        &[],
    );
    let asset = format!("{top}.tar.gz");
    let doc = format!(
        r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"body":"n","assets":[{{"name":"{SUMS_FILE}","size":9}},{{"name":"{asset}","size":{}}}]}}"#,
        tar.len()
    );
    f.mock
        .route(&format!("/api/releases/tags/{tag}"), Route::ok(doc));
    f.mock.route(
        &format!("/dl/{tag}/{SUMS_FILE}"),
        Route::ok(format!("{}  {asset}\n", sha256_hex(&tar))),
    );
    f.mock.route(&format!("/dl/{tag}/{asset}"), Route::ok(tar));
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
async fn an_older_version_is_installed_only_when_named() {
    let f = Fixture::new().await;
    serve_tag(&f, "v0.1.0");
    let dir = install_dir(f.data.path(), "0.1.1-alpha.3");
    let u = f.updater("0.1.1-alpha.3").with_exe(dir.join("spb"));
    let result = u.install_version("v0.1.0", false).await.unwrap();
    assert_eq!(result.outcome, SelfInstallOutcome::Installed);
    assert_eq!(result.version, "0.1.0");
    let text = std::fs::read_to_string(dir.join("spb")).unwrap();
    assert!(text.contains("spb 0.1.0"), "{text}");
}

#[test]
fn a_finished_downgrade_is_judged_by_the_version_it_left() {
    let v = |s: &str| superba_versions::selfupdate::version::parse_version(s).unwrap();
    let at = Timestamp::from_millis(T0);
    let done = |from: &str, to: &str| {
        let mut a = Attempt::new(from, to, Path::new("/tmp/selfupdate.log"), at);
        a.stage = Stage::Done;
        a
    };
    let down = done("0.1.1-alpha.3", "0.1.0");
    assert!(down.problem(&v("0.1.0"), at).is_none(), "it took effect");
    assert!(
        down.problem(&v("0.1.1-alpha.3"), at).is_some(),
        "still the version it left"
    );
    let up = done("0.1.0", "0.2.0");
    assert!(up.problem(&v("0.2.0"), at).is_none());
    assert!(up.problem(&v("0.3.0"), at).is_none(), "updated again since");
    assert!(up.problem(&v("0.1.0"), at).is_some(), "not applied");
}

#[tokio::test]
async fn a_named_version_is_checked_before_anything_is_downloaded() {
    let f = Fixture::new().await;
    serve_tag(&f, "v0.1.0");
    let u = f.updater("0.1.0");
    let e = u.install_version("0.1.0", false).await.unwrap_err();
    assert!(
        matches!(e, Error::Invalid { .. }),
        "the running version: {e}"
    );
    let e = u.install_version("latest", false).await.unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e}");
    let e = u.install_version("0.0.9", false).await.unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "no such release: {e}");
    // The Windows installer only upgrades: an older MSI is refused up front.
    let win = f.updater("0.1.1-alpha.3").with_platform(Platform {
        os: Os::Windows,
        arch: "x86_64",
        musl: false,
    });
    let e = win.install_version("0.1.0", false).await.unwrap_err();
    assert!(matches!(e, Error::Unsupported { .. }), "{e}");
    assert!(e.to_string().contains("uninstall"), "{e}");
    assert_eq!(f.mock.hits("/dl/"), 0, "nothing was downloaded");
}
