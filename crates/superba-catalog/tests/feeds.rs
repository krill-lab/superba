//! Signed data feeds: the compiled-in snapshot, the verified cache and the mirror
//! race (in-memory mirrors with delays and faults; no network).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use superba_catalog::feeds::format::{
    FeedIndex, INDEX_FILE, MODELS_FILE, ModelsFeed, SIGNATURE_FILE, parse_index, to_pretty_json,
};
use superba_catalog::feeds::{
    FEEDS_SIGNATURE_CONTEXT, FeedData, FeedLocation, FeedRefresh, FeedStore, HttpFeeds, Mirror,
    RaceConfig, builtin_feeds, builtin_mirrors, mirror_list,
};
use superba_catalog::format::{SIGNATURE_FORMAT, SignatureEntry, SignatureFile};
use superba_catalog::integrity::{
    SIGNATURE_CONTEXT, TrustedKey, key_id, sha256_hex, signed_message_with_context,
};
use superba_catalog::remote::Transport;
use superba_catalog::{CatalogError, TrustPolicy};
use superba_core::{
    CodexVersion, Error, FeedOrigin, FeedSource, FeedSourceChoice, ManualClock, NetworkSettings,
    Timestamp,
};
use url::Url;

mod common;
use common::test_key;

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

fn leak(k: TrustedKey) -> &'static [TrustedKey] {
    Box::leak(vec![k].into_boxed_slice())
}

/// The compiled-in feed files with `serial`, the models file marked by `marker`
/// (an extra model slug), re-indexed and signed with `key` over `context`.
fn feed_files(
    serial: u64,
    marker: &str,
    key: &SigningKey,
    context: &[u8],
) -> BTreeMap<String, Vec<u8>> {
    let mut files = superba_catalog::builtin_feed_files().unwrap();
    files.remove(SIGNATURE_FILE);
    let mut models: ModelsFeed = serde_json::from_slice(&files[MODELS_FILE]).unwrap();
    models
        .models
        .push(superba_core::ModelSpec::new(marker.to_owned()));
    files.insert(
        MODELS_FILE.into(),
        to_pretty_json(&models).unwrap().into_bytes(),
    );
    let mut index: FeedIndex = parse_index(&files[INDEX_FILE]).unwrap();
    index.serial = serial;
    for f in &mut index.files {
        let b = &files[&f.path];
        f.sha256 = sha256_hex(b);
        f.size = b.len() as u64;
    }
    let index_bytes = to_pretty_json(&index).unwrap().into_bytes();
    let msg = signed_message_with_context(context, &index_bytes);
    let sig = SignatureFile {
        format: SIGNATURE_FORMAT,
        signatures: vec![SignatureEntry {
            key_id: key_id(&key.verifying_key().to_bytes()),
            algorithm: "ed25519".into(),
            signature: base64::engine::general_purpose::STANDARD.encode(key.sign(&msg).to_bytes()),
        }],
    };
    files.insert(INDEX_FILE.into(), index_bytes);
    files.insert(
        SIGNATURE_FILE.into(),
        superba_catalog::format::to_pretty_json(&sig)
            .unwrap()
            .into_bytes(),
    );
    files
}

/// What one mirror answers.
#[derive(Clone)]
enum Answer {
    Files(BTreeMap<String, Vec<u8>>),
    Html,
    Down,
    Slow(Duration, BTreeMap<String, Vec<u8>>),
}

#[derive(Default)]
struct Mirrors {
    by_base: HashMap<String, Answer>,
    requests: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Transport for Mirrors {
    async fn get(&self, url: &Url, max_bytes: u64) -> superba_catalog::Result<Vec<u8>> {
        self.requests.lock().unwrap().push(url.to_string());
        let net = |m: &str| CatalogError::Network {
            url: url.to_string(),
            message: m.into(),
        };
        let (base, rel) = self
            .by_base
            .keys()
            .find_map(|b| {
                url.as_str()
                    .strip_prefix(b.as_str())
                    .map(|r| (b.clone(), r))
            })
            .ok_or_else(|| net("no such host"))?;
        let files = match self.by_base[&base].clone() {
            Answer::Down => return Err(net("connection refused")),
            Answer::Html => return Ok(b"<!doctype html><html>landing page</html>".to_vec()),
            Answer::Slow(d, f) => {
                tokio::time::sleep(d).await;
                f
            }
            Answer::Files(f) => f,
        };
        match files.get(rel) {
            Some(b) if b.len() as u64 <= max_bytes => Ok(b.clone()),
            Some(_) => Err(net("too large")),
            None => Err(net("HTTP 404")),
        }
    }
}

fn base(name: &str) -> String {
    format!("https://{name}.mirror.test/feeds/")
}

fn mirror(name: &str) -> Mirror {
    Mirror::new(name, &base(name)).unwrap()
}

fn race() -> RaceConfig {
    RaceConfig {
        stagger: Duration::from_millis(5),
        per_request: Duration::from_millis(300),
        overall: Duration::from_secs(3),
        settle: Duration::from_millis(150),
        file_timeout: Duration::from_secs(1),
        min_interval: Duration::from_secs(24 * 3600),
    }
}

struct Setup {
    feeds: HttpFeeds,
    transport: Arc<Mirrors>,
    clock: Arc<ManualClock>,
    _dir: tempfile::TempDir,
}

const T0: Timestamp = Timestamp::from_millis(1_792_000_000_000);

fn setup(
    answers: Vec<(&str, Answer)>,
    trusted: &'static [TrustedKey],
    network: NetworkSettings,
) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(Mirrors {
        by_base: answers.iter().map(|(n, a)| (base(n), a.clone())).collect(),
        requests: Mutex::new(Vec::new()),
    });
    let clock = Arc::new(ManualClock::new(T0));
    let store = FeedStore::from_dir(dir.path().join("feeds")).with_keys(trusted, &[]);
    let feeds = HttpFeeds::new(store, network)
        .with_transport(transport.clone())
        .with_clock(clock.clone())
        .with_race(race())
        .with_mirrors(answers.iter().map(|(n, _)| mirror(n)).collect());
    Setup {
        feeds,
        transport,
        clock,
        _dir: dir,
    }
}

fn builtin_serial() -> u64 {
    builtin_feeds().unwrap().serial()
}

fn has_model(snap: &superba_core::FeedSnapshot, slug: &str) -> bool {
    snap.models.iter().any(|m| m.slug == slug)
}

// ---------------------------------------------------------------------------

#[test]
fn the_builtin_snapshot_verifies_and_carries_every_feed() {
    let data = builtin_feeds().unwrap();
    assert!(data.serial() >= 1);
    let snap = data.snapshot(FeedOrigin::Builtin, None);
    assert!(has_model(&snap, "gpt-6-astra"), "{:?}", snap.models);
    assert!(snap.models.iter().all(|m| m.raw.is_none()));
    let alpha = snap
        .codex_versions
        .iter()
        .find(|r| r.version == v("0.163.0-alpha.10.2"))
        .expect("app-bundled alpha listed");
    assert!(alpha.prerelease);
    assert!(alpha.bundled_by.iter().any(|a| a.starts_with("26.1013.")));
    assert!(alpha.sha256.contains_key("x86_64-unknown-linux-musl"));
    let newest = &snap.codex_versions[0];
    assert!(
        snap.codex_versions
            .iter()
            .all(|r| r.version <= newest.version)
    );
    let app = &snap.app_versions[0];
    assert!(app.bundled_codex.is_some());
    assert!(app.packages.iter().any(|p| p.sha256.is_some()));
    // Prompts: exact, an alpha of a listed release, a version below every one.
    let p163 = data.prompt(&v("0.163.0")).expect("prompt of 0.163.0");
    assert!(p163.len() > 1000);
    assert_eq!(
        data.prompt(&v("0.163.0-alpha.10.2")).as_deref(),
        Some(p163.as_str())
    );
    assert_eq!(data.prompt(&v("0.199.0")).as_deref(), Some(p163.as_str()));
    assert!(data.prompt(&v("0.140.0")).is_some());
    assert!(data.prompt(&v("0.100.0")).is_none());
    // Prices come only from public price pages: every entry names its source
    // and the date it was read.
    for (model, e) in &snap.prices.entries {
        assert!(
            e.source
                .as_deref()
                .is_some_and(|s| s.starts_with("https://")),
            "{model}"
        );
        // Each entry keeps the date its own page was read; the table's
        // revision is the newest of them.
        let rev = e.rev.as_deref().unwrap_or_default();
        assert!(
            rev.len() == 10 && rev <= snap.prices.rev.as_str(),
            "{model}: {rev}"
        );
    }
    // The built-in table holds OpenAI's list prices, tier by tier, and the
    // feed index that lists it carries a valid signature of a trusted key.
    let astra = snap.prices.get("gpt-6-astra").expect("gpt-6-astra priced");
    assert_eq!(astra.input, superba_core::Usd::from_micros(10_000_000));
    for tier in ["priority", "flex", "ultrafast"] {
        assert!(astra.tier_prices.contains_key(tier), "{tier}");
    }
    assert!(snap.prices.get("codex-auto-review").is_none());
    assert!(data.signature().is_valid(), "{:?}", data.signature());
}

#[tokio::test]
async fn the_offline_store_serves_the_snapshot_without_a_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = FeedStore::from_dir(dir.path());
    let snap = store.snapshot(true).await.unwrap();
    assert_eq!(snap.source, FeedOrigin::Builtin);
    assert_eq!(snap.serial, builtin_serial());
    assert_eq!(snap.fetched_at, None);
    let status = store.status().await.unwrap();
    assert_eq!(status.serial, builtin_serial());
    assert_eq!(status.builtin_serial, builtin_serial());
    assert!(!status.online_checks);
    // A price revision is reported only for a table with prices.
    assert_eq!(
        status.prices_rev.is_some(),
        !snap.prices.entries.is_empty(),
        "{:?}",
        status.prices_rev
    );
    assert!(
        store
            .prompt(&v("0.162.0-alpha.17.2"))
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_feed_index_never_passes_as_a_settings_index_and_back() {
    let (sk, tk) = test_key(41);
    let files = feed_files(builtin_serial() + 1, "x", &sk, SIGNATURE_CONTEXT);
    let err = FeedData::from_files(files, TrustPolicy::RequireSignature, &[tk], &[]).unwrap_err();
    assert!(matches!(err, CatalogError::Signature(_)), "{err:?}");
    let files = feed_files(builtin_serial() + 1, "x", &sk, FEEDS_SIGNATURE_CONTEXT);
    assert!(FeedData::from_files(files.clone(), TrustPolicy::RequireSignature, &[tk], &[]).is_ok());
    // The same signature over the settings context does not verify.
    let status = superba_catalog::integrity::verify_signature(
        &files[INDEX_FILE],
        Some(&files[SIGNATURE_FILE]),
        &[tk],
        &[],
    );
    assert!(!status.is_valid());
}

#[test]
fn tampered_or_truncated_files_are_rejected() {
    let (sk, tk) = test_key(42);
    let mut files = feed_files(builtin_serial() + 1, "x", &sk, FEEDS_SIGNATURE_CONTEXT);
    let m = files.get_mut(MODELS_FILE).unwrap();
    m.truncate(m.len() / 2);
    let err = FeedData::from_files(files, TrustPolicy::RequireSignature, &[tk], &[]).unwrap_err();
    assert!(matches!(err, CatalogError::Integrity { .. }), "{err:?}");
}

#[tokio::test]
async fn a_newer_signed_feed_is_fetched_cached_and_used() {
    let (sk, tk) = test_key(43);
    let serial = builtin_serial() + 3;
    let files = feed_files(serial, "remote-model", &sk, FEEDS_SIGNATURE_CONTEXT);
    let s = setup(
        vec![("good", Answer::Files(files))],
        leak(tk),
        NetworkSettings::default(),
    );
    let out = s.feeds.refresh(false).await.unwrap();
    assert_eq!(
        out,
        FeedRefresh::Updated {
            serial,
            mirror: "good".into(),
            downloaded: 1
        }
    );
    let snap = s.feeds.store().snapshot(false).await.unwrap();
    assert_eq!(snap.source, FeedOrigin::Cache);
    assert_eq!(snap.serial, serial);
    assert_eq!(snap.fetched_at, Some(T0));
    assert!(has_model(&snap, "remote-model"));
    let status = s.feeds.status().await.unwrap();
    assert_eq!(status.mirror.as_deref(), Some("good"));
    assert_eq!(status.checked_at, Some(T0));
    assert!(status.online_checks);
    assert_eq!(status.last_error, None);
    // Checked within a day: an automatic check makes no request.
    let before = s.transport.requests.lock().unwrap().len();
    assert_eq!(s.feeds.refresh(false).await.unwrap(), FeedRefresh::NotDue);
    assert_eq!(s.transport.requests.lock().unwrap().len(), before);
    // A forced check finds the data in use.
    assert_eq!(
        s.feeds.refresh(true).await.unwrap(),
        FeedRefresh::UpToDate {
            serial,
            mirror: "good".into()
        }
    );
    // A day later the snapshot call checks again by itself.
    s.clock.advance(Duration::from_secs(25 * 3600));
    s.feeds.snapshot(false).await.unwrap();
    assert!(s.transport.requests.lock().unwrap().len() > before + 2);
}

#[tokio::test]
async fn the_highest_serial_wins_over_a_stale_cdn_copy_and_bad_mirrors() {
    let (sk, tk) = test_key(44);
    let (wrong, _) = test_key(45);
    let b = builtin_serial();
    let stale = feed_files(b + 1, "stale", &sk, FEEDS_SIGNATURE_CONTEXT);
    let fresh = feed_files(b + 2, "fresh", &sk, FEEDS_SIGNATURE_CONTEXT);
    let foreign_key = feed_files(b + 9, "foreign", &wrong, FEEDS_SIGNATURE_CONTEXT);
    let settings_ctx = feed_files(b + 9, "ctx", &sk, SIGNATURE_CONTEXT);
    let s = setup(
        vec![
            ("down", Answer::Down),
            ("html", Answer::Html),
            ("stale", Answer::Files(stale)),
            ("badkey", Answer::Files(foreign_key)),
            ("badctx", Answer::Files(settings_ctx)),
            ("fresh", Answer::Files(fresh)),
        ],
        leak(tk),
        NetworkSettings::default(),
    );
    let out = s.feeds.refresh(true).await.unwrap();
    assert_eq!(
        out,
        FeedRefresh::Updated {
            serial: b + 2,
            mirror: "fresh".into(),
            downloaded: 1
        }
    );
    let snap = s.feeds.store().snapshot(false).await.unwrap();
    assert!(has_model(&snap, "fresh"));
    // The winner is tried first next time.
    assert_eq!(s.feeds.mirrors()[0].name, "fresh");
}

#[tokio::test]
async fn a_mirror_that_times_out_is_skipped() {
    let (sk, tk) = test_key(46);
    let b = builtin_serial();
    let newest = feed_files(b + 7, "slow", &sk, FEEDS_SIGNATURE_CONTEXT);
    let ok = feed_files(b + 1, "quick", &sk, FEEDS_SIGNATURE_CONTEXT);
    let s = setup(
        vec![
            ("slow", Answer::Slow(Duration::from_secs(5), newest)),
            ("quick", Answer::Files(ok)),
        ],
        leak(tk),
        NetworkSettings::default(),
    );
    let started = std::time::Instant::now();
    let out = s.feeds.refresh(true).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(out, FeedRefresh::Updated { serial, ref mirror, .. } if serial == b + 1 && mirror == "quick"),
        "{out:?}"
    );
}

#[tokio::test]
async fn a_lower_serial_than_the_one_in_use_is_refused() {
    let (sk, tk) = test_key(47);
    let b = builtin_serial();
    let s = setup(
        vec![(
            "m",
            Answer::Files(feed_files(b + 5, "five", &sk, FEEDS_SIGNATURE_CONTEXT)),
        )],
        leak(tk),
        NetworkSettings::default(),
    );
    s.feeds.refresh(true).await.unwrap();
    // The mirror now serves an older (stale) feed: refused, data in use kept.
    let older = Setup {
        feeds: s.feeds.clone().with_transport(Arc::new(Mirrors {
            by_base: [(
                base("m"),
                Answer::Files(feed_files(b + 4, "four", &sk, FEEDS_SIGNATURE_CONTEXT)),
            )]
            .into_iter()
            .collect(),
            requests: Mutex::new(Vec::new()),
        })),
        transport: s.transport.clone(),
        clock: s.clock.clone(),
        _dir: tempfile::tempdir().unwrap(),
    };
    let err = older.feeds.refresh(true).await.unwrap_err();
    assert!(err.to_string().contains("older"), "{err}");
    let snap = older.feeds.store().snapshot(false).await.unwrap();
    assert_eq!(snap.serial, b + 5);
    assert!(has_model(&snap, "five"));
    let status = older.feeds.status().await.unwrap();
    assert!(status.last_error.is_some());
}

#[tokio::test]
async fn a_file_that_does_not_match_is_fetched_from_another_mirror() {
    let (sk, tk) = test_key(48);
    let b = builtin_serial();
    let good = feed_files(b + 1, "m1", &sk, FEEDS_SIGNATURE_CONTEXT);
    let mut broken = good.clone();
    let m = broken.get_mut(MODELS_FILE).unwrap();
    m.truncate(m.len() - 10);
    let s = setup(
        vec![
            ("broken", Answer::Files(broken)),
            ("good", Answer::Files(good)),
        ],
        leak(tk),
        NetworkSettings::default(),
    );
    let out = s.feeds.refresh(true).await.unwrap();
    assert!(matches!(out, FeedRefresh::Updated { .. }), "{out:?}");
    assert!(has_model(
        &s.feeds.store().snapshot(false).await.unwrap(),
        "m1"
    ));

    // Only broken copies: nothing is replaced.
    let (sk2, tk2) = test_key(49);
    let good = feed_files(b + 1, "m2", &sk2, FEEDS_SIGNATURE_CONTEXT);
    let mut broken = good.clone();
    broken.insert(MODELS_FILE.into(), b"{}".to_vec());
    let s = setup(
        vec![("broken", Answer::Files(broken))],
        leak(tk2),
        NetworkSettings::default(),
    );
    assert!(s.feeds.refresh(true).await.is_err());
    let snap = s.feeds.store().snapshot(false).await.unwrap();
    assert_eq!(snap.source, FeedOrigin::Builtin);
}

#[tokio::test]
async fn all_mirrors_down_keeps_the_cache_then_the_snapshot() {
    let (sk, tk) = test_key(50);
    let b = builtin_serial();
    let s = setup(
        vec![(
            "m",
            Answer::Files(feed_files(b + 2, "cached", &sk, FEEDS_SIGNATURE_CONTEXT)),
        )],
        leak(tk),
        NetworkSettings::default(),
    );
    s.feeds.refresh(true).await.unwrap();
    let down = s.feeds.clone().with_transport(Arc::new(Mirrors {
        by_base: [(base("m"), Answer::Down)].into_iter().collect(),
        requests: Mutex::new(Vec::new()),
    }));
    let snap = down.snapshot(true).await.unwrap();
    assert_eq!(snap.source, FeedOrigin::Cache);
    assert!(has_model(&snap, "cached"));
    assert!(matches!(
        down.status().await.unwrap().last_error,
        Some(Error::Network { .. })
    ));

    // No cache at all: the snapshot.
    let fresh = setup(
        vec![("m", Answer::Down)],
        leak(tk),
        NetworkSettings::default(),
    );
    let snap = fresh.feeds.snapshot(true).await.unwrap();
    assert_eq!(snap.source, FeedOrigin::Builtin);
    assert_eq!(snap.serial, b);
}

#[tokio::test]
async fn online_checks_off_or_no_mirror_makes_no_request() {
    let (_, tk) = test_key(51);
    let off = NetworkSettings {
        online_checks: false,
        ..NetworkSettings::default()
    };
    let s = setup(vec![("m", Answer::Down)], leak(tk), off);
    assert_eq!(s.feeds.refresh(true).await.unwrap(), FeedRefresh::Disabled);
    s.feeds.snapshot(true).await.unwrap();
    assert!(s.transport.requests.lock().unwrap().is_empty());
    assert!(!s.feeds.status().await.unwrap().online_checks);

    let dir = tempfile::tempdir().unwrap();
    let none = HttpFeeds::new(FeedStore::from_dir(dir.path()), NetworkSettings::default())
        .with_env_mirrors(None);
    assert!(
        none.mirrors().is_empty(),
        "no built-in mirror before publishing"
    );
    assert_eq!(none.refresh(true).await.unwrap(), FeedRefresh::NoMirrors);
}

#[test]
fn mirror_lists_follow_the_network_settings() {
    let loc = FeedLocation {
        repo: "owner/feeds".into(),
        git_ref: "main".into(),
        path: "".into(),
    };
    let all = builtin_mirrors(&loc, FeedSourceChoice::Auto, false);
    let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names[0], "raw");
    assert!(names.contains(&"jsdelivr") && names.contains(&"githack"));
    assert!(!names.contains(&"gh-proxy"));
    assert_eq!(
        all[0].url("index.json").unwrap().as_str(),
        "https://raw.githubusercontent.com/owner/feeds/main/index.json"
    );
    let jsd = all.iter().find(|m| m.name == "jsdelivr").unwrap();
    assert_eq!(
        jsd.url("prompts/a.md").unwrap().as_str(),
        "https://cdn.jsdelivr.net/gh/owner/feeds@main/prompts/a.md"
    );
    let sub = FeedLocation {
        path: "catalog/feeds".into(),
        ..loc.clone()
    };
    assert_eq!(
        builtin_mirrors(&sub, FeedSourceChoice::GitHubRaw, true)
            .iter()
            .map(|m| m.base.to_string())
            .collect::<Vec<_>>(),
        [
            "https://raw.githubusercontent.com/owner/feeds/main/catalog/feeds/",
            "https://gh-proxy.com/https://raw.githubusercontent.com/owner/feeds/main/catalog/feeds/",
            "https://ghfast.top/https://raw.githubusercontent.com/owner/feeds/main/catalog/feeds/",
        ]
    );
    assert!(
        builtin_mirrors(&loc, FeedSourceChoice::JsDelivr, true)
            .iter()
            .all(|m| m.base.as_str().contains("/gh/owner/feeds@main/"))
    );

    let net = NetworkSettings {
        extra_mirrors: vec![
            "https://mirror.example.com/feeds".into(),
            "http://not-loopback.example.com/".into(),
        ],
        ..NetworkSettings::default()
    };
    let list = mirror_list(
        &net,
        &loc,
        false,
        Some("http://127.0.0.1:9/f/, ,https://mirror.example.com/feeds/"),
    );
    let bases: Vec<String> = list.iter().map(|m| m.base.to_string()).collect();
    assert_eq!(
        bases,
        ["https://mirror.example.com/feeds/", "http://127.0.0.1:9/f/"]
    );
    assert_eq!(list[1].name, "127.0.0.1:9");
    let published = mirror_list(&net, &loc, true, None);
    assert!(published.len() > 5);
    assert!(Mirror::new("x", "https://user:pw@example.com/").is_err());
    assert!(Mirror::new("x", "https://example.com/?q=1").is_err());
}
