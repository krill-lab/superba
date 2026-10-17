//! Superba's signed data feeds (`catalog/feeds/`): built-in model list, price
//! table, Codex releases, desktop app builds and per-version `prompt.md`.
//!
//! Layout ([`format`]): `index.json` (`{"format":1,"serial":N,"files":[..]}`) and
//! `index.json.sig`, an ed25519 signature over [`FEEDS_SIGNATURE_CONTEXT`] + the
//! index bytes (a feed index can never pass as a settings catalog index or the
//! other way round), plus `models.json`, `prices.json`, `codex-versions.json`,
//! `app-versions.json`, `prompts.json` and `prompts/<sha8>.md`.
//!
//! [`FeedStore`] serves the compiled-in snapshot and the verified cache under
//! `<data dir>/feeds/` without network; [`HttpFeeds`] adds the online refresh
//! (at most daily unless asked, only with
//! [`superba_core::NetworkSettings::online_checks`]): mirrors raced with a
//! 150 ms stagger and 3 s per request, the highest valid serial within 1.5 s of
//! the first valid index, never below the serial in use, every file checked
//! against the index, the winner remembered and tried first next time; any
//! failure keeps the cache, then the snapshot.
//!
//! Until the public feeds location exists ([`FEEDS_PUBLISHED`]) the built-in
//! mirrors are not contacted: only the user's extra mirrors and
//! [`ENV_SUPERBA_FEED_MIRRORS`] are raced, so by default Superba uses its
//! compiled-in lists without any network request.

pub mod data;
pub mod format;
pub mod mirrors;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use superba_core::{
    Clock, CodexVersion, DataDir, Error, FeedOrigin, FeedSnapshot, FeedSource, FeedsStatus,
    NetworkSettings, Result, SystemClock, Timestamp,
};

pub use data::FeedData;
pub use mirrors::{
    ENV_SUPERBA_FEED_MIRRORS, FeedLocation, Mirror, RaceConfig, builtin_mirrors, mirror_list,
};

use crate::error::CatalogError;
use crate::integrity::{REVOKED_KEY_IDS, TRUSTED_KEYS, TrustedKey, validate_relative_path};
use crate::remote::Transport;
use crate::store::TrustPolicy;

/// Signature context of the feed index (differs from the settings catalog's).
pub const FEEDS_SIGNATURE_CONTEXT: &[u8] = b"superba-feeds-index-v1\n";

/// Public repository of the feeds (placeholder until its location is decided;
/// until it exists Superba uses the compiled-in snapshot).
pub const FEEDS_REPO: &str = "krill-lab/superba-feeds";
/// Branch the feeds are published on.
pub const FEEDS_REF: &str = "main";
/// Directory of the feeds inside [`FEEDS_REPO`] (the root).
pub const FEEDS_PATH: &str = "";
/// The public feeds location exists: race the built-in mirrors. Off until it is
/// created (the mirror code is tested against local mirrors).
pub const FEEDS_PUBLISHED: bool = false;

/// The compiled-in feeds, verified once.
fn builtin() -> std::result::Result<Arc<FeedData>, String> {
    static BUILTIN: OnceLock<std::result::Result<Arc<FeedData>, String>> = OnceLock::new();
    BUILTIN
        .get_or_init(|| {
            let files = crate::bundle::builtin_feed_files().map_err(|e| e.to_string())?;
            FeedData::from_files(
                files,
                TrustPolicy::ManifestOnly,
                TRUSTED_KEYS,
                REVOKED_KEY_IDS,
            )
            .map(Arc::new)
            .map_err(|e| e.to_string())
        })
        .clone()
}

/// The compiled-in feeds (verified against their index; a signature is checked
/// and reported but not required: the snapshot is part of the signed binary).
pub fn builtin_feeds() -> crate::Result<Arc<FeedData>> {
    builtin().map_err(|e| CatalogError::Integrity {
        path: "feeds/index.json".into(),
        message: e,
    })
}

/// `<data dir>/feeds/state.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct FeedState {
    /// Last online check (successful or not), wall-clock ms.
    #[serde(default)]
    checked_at: Option<i64>,
    /// When the cached data was fetched, wall-clock ms.
    #[serde(default)]
    fetched_at: Option<i64>,
    /// Mirror that served the cached data / won the last race.
    #[serde(default)]
    mirror: Option<String>,
    /// Why the last check failed.
    #[serde(default)]
    last_error: Option<Error>,
}

/// The compiled-in snapshot and the verified cache (no network).
#[derive(Clone, Debug)]
pub struct FeedStore {
    dir: PathBuf,
    trusted: &'static [TrustedKey],
    revoked: &'static [&'static str],
    builtin: Option<Arc<FeedData>>,
}

impl FeedStore {
    /// The store under `<data dir>/feeds/`.
    pub fn new(data_dir: &DataDir) -> Self {
        Self::from_dir(data_dir.feeds_dir())
    }

    /// Use `dir` as the cache directory.
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            trusted: TRUSTED_KEYS,
            revoked: REVOKED_KEY_IDS,
            builtin: None,
        }
    }

    /// Accept signatures from `trusted` instead of [`TRUSTED_KEYS`] (tests and
    /// tools).
    pub fn with_keys(
        mut self,
        trusted: &'static [TrustedKey],
        revoked: &'static [&'static str],
    ) -> Self {
        self.trusted = trusted;
        self.revoked = revoked;
        self
    }

    /// Use `data` as the compiled-in snapshot (tests and tools).
    pub fn with_builtin(mut self, data: Arc<FeedData>) -> Self {
        self.builtin = Some(data);
        self
    }

    /// The cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn tree(&self) -> PathBuf {
        self.dir.join("tree")
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    fn builtin_data(&self) -> crate::Result<Arc<FeedData>> {
        match &self.builtin {
            Some(b) => Ok(Arc::clone(b)),
            None => builtin_feeds(),
        }
    }

    fn state(&self) -> FeedState {
        std::fs::read(self.state_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &FeedState) {
        let res = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let tmp = self.dir.join(format!(".state-{}.tmp", std::process::id()));
            std::fs::write(
                &tmp,
                serde_json::to_vec(state).map_err(std::io::Error::other)?,
            )?;
            std::fs::rename(&tmp, self.state_path())
        })();
        if let Err(e) = res {
            tracing::debug!(error = %e, "cannot write the feed state");
        }
    }

    /// The verified cache, when present and valid.
    fn cached(&self) -> Option<FeedData> {
        let tree = self.tree();
        let index = std::fs::read(tree.join(format::INDEX_FILE)).ok()?;
        let parsed = format::parse_index(&index).ok()?;
        let mut files = BTreeMap::new();
        files.insert(format::INDEX_FILE.to_owned(), index);
        if let Ok(sig) = std::fs::read(tree.join(format::SIGNATURE_FILE)) {
            files.insert(format::SIGNATURE_FILE.to_owned(), sig);
        }
        for f in &parsed.files {
            validate_relative_path(&f.path).ok()?;
            files.insert(f.path.clone(), std::fs::read(tree.join(&f.path)).ok()?);
        }
        FeedData::from_files(
            files,
            TrustPolicy::RequireSignature,
            self.trusted,
            self.revoked,
        )
        .ok()
    }

    /// The data in use: the verified cache when its serial is above the
    /// compiled-in one, else the compiled-in snapshot.
    pub fn load(&self) -> crate::Result<(Arc<FeedData>, FeedOrigin)> {
        let builtin = self.builtin_data()?;
        match self.cached() {
            Some(c) if c.serial() > builtin.serial() => Ok((Arc::new(c), FeedOrigin::Cache)),
            _ => Ok((builtin, FeedOrigin::Builtin)),
        }
    }

    /// Replace the cache with verified `files` (written beside, then swapped).
    fn write_tree(&self, files: &BTreeMap<String, Vec<u8>>) -> crate::Result<()> {
        let io = |p: &Path, e: std::io::Error| CatalogError::io(p, &e);
        let next = self.dir.join("tree.new");
        let old = self.dir.join("tree.old");
        let tree = self.tree();
        for d in [&next, &old] {
            if d.exists() {
                std::fs::remove_dir_all(d).map_err(|e| io(d, e))?;
            }
        }
        for (rel, bytes) in files {
            validate_relative_path(rel)?;
            let p = next.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
            }
            std::fs::write(&p, bytes).map_err(|e| io(&p, e))?;
        }
        if tree.exists() {
            std::fs::rename(&tree, &old).map_err(|e| io(&tree, e))?;
        }
        std::fs::rename(&next, &tree).map_err(|e| io(&next, e))?;
        if old.exists() {
            std::fs::remove_dir_all(&old).map_err(|e| io(&old, e))?;
        }
        Ok(())
    }

    fn snapshot_now(&self) -> Result<FeedSnapshot> {
        let (data, origin) = self.load().map_err(Error::from)?;
        let state = self.state();
        let (source, fetched_at) = match origin {
            FeedOrigin::Builtin => (FeedOrigin::Builtin, None),
            _ => (
                FeedOrigin::Cache,
                state.fetched_at.map(Timestamp::from_millis),
            ),
        };
        Ok(data.snapshot(source, fetched_at))
    }

    fn status_now(&self, online_checks: bool) -> Result<FeedsStatus> {
        let (data, origin) = self.load().map_err(Error::from)?;
        let builtin = self.builtin_data().map_err(Error::from)?;
        let state = self.state();
        let cached = matches!(origin, FeedOrigin::Cache);
        let prices = data.snapshot(origin.clone(), None).prices;
        let prices_rev = (!prices.entries.is_empty()).then_some(prices.rev);
        Ok(FeedsStatus {
            serial: data.serial(),
            builtin_serial: builtin.serial(),
            source: origin,
            fetched_at: cached
                .then_some(state.fetched_at)
                .flatten()
                .map(Timestamp::from_millis),
            checked_at: state.checked_at.map(Timestamp::from_millis),
            mirror: state.mirror,
            online_checks,
            last_error: state.last_error,
            prices_rev,
        })
    }
}

#[async_trait]
impl FeedSource for FeedStore {
    async fn snapshot(&self, refresh: bool) -> Result<FeedSnapshot> {
        let _ = refresh;
        self.snapshot_now()
    }

    async fn prompt(&self, version: &CodexVersion) -> Result<Option<String>> {
        let (data, _) = self.load().map_err(Error::from)?;
        Ok(data.prompt(version))
    }

    async fn status(&self) -> Result<FeedsStatus> {
        self.status_now(false)
    }
}

/// What [`HttpFeeds::refresh`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FeedRefresh {
    /// Online checks are off.
    Disabled,
    /// The last check is recent (automatic checks only).
    NotDue,
    /// No mirror is configured (the public location does not exist yet and no
    /// extra mirror is set).
    NoMirrors,
    /// The newest valid index is the one in use.
    UpToDate {
        /// Serial in use.
        serial: u64,
        /// Mirror that answered.
        mirror: String,
    },
    /// A newer feed was verified and cached.
    Updated {
        /// New serial.
        serial: u64,
        /// Mirror that served it.
        mirror: String,
        /// Files downloaded (the rest were reused).
        downloaded: usize,
    },
}

/// [`FeedStore`] plus the online refresh from the mirror list.
#[derive(Clone)]
pub struct HttpFeeds {
    store: FeedStore,
    network: NetworkSettings,
    location: FeedLocation,
    published: bool,
    env_mirrors: Option<String>,
    mirrors: Option<Vec<Mirror>>,
    transport: Option<Arc<dyn Transport>>,
    clock: Arc<dyn Clock>,
    race: RaceConfig,
}

impl std::fmt::Debug for HttpFeeds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFeeds")
            .field("store", &self.store)
            .field("network", &self.network)
            .field("published", &self.published)
            .field("mirrors", &self.mirrors())
            .finish_non_exhaustive()
    }
}

impl HttpFeeds {
    /// Feeds over `store` with the user's network settings; extra mirrors from
    /// [`ENV_SUPERBA_FEED_MIRRORS`]; the reqwest transport.
    pub fn new(store: FeedStore, network: NetworkSettings) -> Self {
        #[cfg(feature = "http")]
        let transport = crate::remote::HttpTransport::new()
            .ok()
            .map(|t| Arc::new(t) as Arc<dyn Transport>);
        #[cfg(not(feature = "http"))]
        let transport = None;
        Self {
            store,
            network,
            location: FeedLocation::default(),
            published: FEEDS_PUBLISHED,
            env_mirrors: std::env::var(ENV_SUPERBA_FEED_MIRRORS).ok(),
            mirrors: None,
            transport,
            clock: Arc::new(SystemClock::new()),
            race: RaceConfig::default(),
        }
    }

    /// Use `transport` for every request (tests use in-memory transports).
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Use `clock` for the daily check and timestamps.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Race timing.
    pub fn with_race(mut self, race: RaceConfig) -> Self {
        self.race = race;
        self
    }

    /// Race exactly `mirrors` (replaces the computed list; tests).
    pub fn with_mirrors(mut self, mirrors: Vec<Mirror>) -> Self {
        self.mirrors = Some(mirrors);
        self
    }

    /// Where the feeds are published and whether that location exists.
    pub fn with_location(mut self, location: FeedLocation, published: bool) -> Self {
        self.location = location;
        self.published = published;
        self
    }

    /// The value of [`ENV_SUPERBA_FEED_MIRRORS`] to use (`None` = none).
    pub fn with_env_mirrors(mut self, env: Option<String>) -> Self {
        self.env_mirrors = env;
        self
    }

    /// The offline store.
    pub fn store(&self) -> &FeedStore {
        &self.store
    }

    /// The network settings in use.
    pub fn network(&self) -> &NetworkSettings {
        &self.network
    }

    /// The mirrors a refresh races, the last winner first.
    pub fn mirrors(&self) -> Vec<Mirror> {
        let mut list = match &self.mirrors {
            Some(m) => m.clone(),
            None => mirror_list(
                &self.network,
                &self.location,
                self.published,
                self.env_mirrors.as_deref(),
            ),
        };
        if let Some(winner) = self.store.state().mirror
            && let Some(i) = list.iter().position(|m| m.name == winner)
        {
            let m = list.remove(i);
            list.insert(0, m);
        }
        list
    }

    /// Check the mirrors now (`force`) or when the daily check is due. Failures
    /// are recorded in the status and returned; the data in use never changes
    /// unless a newer feed verified completely.
    pub async fn refresh(&self, force: bool) -> Result<FeedRefresh> {
        if !self.network.online_checks {
            return Ok(FeedRefresh::Disabled);
        }
        let mut state = self.store.state();
        let now = self.clock.now();
        if !force
            && let Some(last) = state.checked_at
            && now
                .duration_since(Timestamp::from_millis(last))
                .is_some_and(|age| age < self.race.min_interval)
        {
            return Ok(FeedRefresh::NotDue);
        }
        let mirrors = self.mirrors();
        if mirrors.is_empty() {
            return Ok(FeedRefresh::NoMirrors);
        }
        let result = self.refresh_from(&mirrors).await;
        state.checked_at = Some(now.as_millis());
        match &result {
            Ok(FeedRefresh::Updated { mirror, .. }) => {
                state.fetched_at = Some(now.as_millis());
                state.mirror = Some(mirror.clone());
                state.last_error = None;
            }
            Ok(FeedRefresh::UpToDate { mirror, .. }) => {
                state.mirror = Some(mirror.clone());
                state.last_error = None;
            }
            Ok(_) => {}
            Err(e) => state.last_error = Some(e.clone()),
        }
        self.store.save_state(&state);
        result
    }

    async fn refresh_from(&self, mirrors: &[Mirror]) -> Result<FeedRefresh> {
        let transport = self
            .transport
            .clone()
            .ok_or_else(|| Error::other("no HTTP client for the feeds"))?;
        let (current, _) = self.store.load().map_err(Error::from)?;
        let outcome = mirrors::race(
            Arc::clone(&transport),
            mirrors,
            self.race,
            self.store.trusted,
            self.store.revoked,
        )
        .await;
        let Some(best) = outcome.best() else {
            let detail = outcome
                .failures
                .iter()
                .map(|(m, r)| format!("{m}: {r}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Network {
                url: "feeds".into(),
                message: format!("no mirror served a valid signed feed index ({detail})"),
            });
        };
        if best.index.serial < current.serial() {
            return Err(Error::from(CatalogError::Rollback {
                found: best.index.serial,
                current: current.serial(),
            }));
        }
        if best.index.serial == current.serial() {
            // The data in use (an index with the same serial but other bytes never
            // replaces it: a publisher bumps the serial).
            return Ok(FeedRefresh::UpToDate {
                serial: current.serial(),
                mirror: best.mirror.name.clone(),
            });
        }
        let others: Vec<Mirror> = outcome
            .offers
            .iter()
            .filter(|o| o.index_bytes == best.index_bytes)
            .map(|o| o.mirror.clone())
            .chain(mirrors.iter().cloned())
            .collect();
        let (files, downloaded) =
            mirrors::fetch_files(transport, best, &others, current.files(), self.race)
                .await
                .map_err(Error::from)?;
        let data = FeedData::from_files(
            files,
            TrustPolicy::RequireSignature,
            self.store.trusted,
            self.store.revoked,
        )
        .map_err(Error::from)?;
        self.store.write_tree(data.files()).map_err(Error::from)?;
        Ok(FeedRefresh::Updated {
            serial: data.serial(),
            mirror: best.mirror.name.clone(),
            downloaded,
        })
    }
}

#[async_trait]
impl FeedSource for HttpFeeds {
    async fn snapshot(&self, refresh: bool) -> Result<FeedSnapshot> {
        if let Err(e) = self.refresh(refresh).await {
            tracing::debug!(error = %e, "feed refresh failed; keeping the data in use");
        }
        self.store.snapshot_now()
    }

    async fn prompt(&self, version: &CodexVersion) -> Result<Option<String>> {
        self.store.prompt(version).await
    }

    async fn status(&self) -> Result<FeedsStatus> {
        self.store.status_now(self.network.online_checks)
    }
}
