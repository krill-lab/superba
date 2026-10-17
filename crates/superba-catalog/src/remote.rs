//! Optional remote updates of the catalog, **disabled by default**.
//!
//! When enabled, [`refresh`] downloads `dist/index.json` and its signature from a
//! configurable base URL (by default the repository's `catalog/` directory on
//! `raw.githubusercontent.com`), requires a valid signature from a trusted key
//! ([`crate::integrity::TRUSTED_KEYS`]), refuses an index whose serial is lower than
//! the one in use, downloads only files whose digest changed, checks every digest and
//! then replaces the cache directory. [`crate::store::load`] uses the cache only when
//! it verifies again and is newer than the built-in snapshot; anything else falls back
//! to the built-in snapshot. No credentials are ever sent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{CatalogError, Result};
use crate::format::{INDEX_PATH, SIGNATURE_PATH};
use crate::integrity::{
    REVOKED_KEY_IDS, TRUSTED_KEYS, TrustedKey, parse_index, sha256_hex, validate_relative_path,
};
use crate::store::{CatalogStore, StoreOrigin, TrustPolicy};

/// Default base URL: the repository's `catalog/` directory on the default branch.
pub const DEFAULT_BASE_URL: &str =
    "https://raw.githubusercontent.com/krill-lab/superba/main/catalog/";

/// Size limit of the index and of the signature file.
pub const MAX_INDEX_BYTES: u64 = 1024 * 1024;
/// Size limit of one catalog file.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Size limit of a whole catalog.
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// Remote update settings. The default is **disabled**.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Fetch updates at all.
    #[serde(default)]
    pub enabled: bool,
    /// Base URL of a catalog tree (`<base>dist/index.json`); https, or http on a
    /// loopback address for tests.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Minimum time between two checks.
    #[serde(default = "default_interval")]
    pub min_interval_secs: u64,
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_owned()
}

fn default_interval() -> u64 {
    24 * 60 * 60
}

impl Default for RemoteConfig {
    fn default() -> Self {
        RemoteConfig {
            enabled: false,
            base_url: default_base_url(),
            min_interval_secs: default_interval(),
        }
    }
}

impl RemoteConfig {
    /// The validated base URL (ends with `/`).
    pub fn base(&self) -> Result<Url> {
        let mut s = self.base_url.trim().to_owned();
        if !s.ends_with('/') {
            s.push('/');
        }
        let url = Url::parse(&s)
            .map_err(|e| CatalogError::Invalid(format!("catalog base URL `{s}`: {e}")))?;
        let loopback = matches!(
            url.host(),
            Some(url::Host::Ipv4(ip)) if ip.is_loopback()
        ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
            || url.host_str() == Some("localhost");
        let ok_scheme = url.scheme() == "https" || (url.scheme() == "http" && loopback);
        if !ok_scheme || !url.username().is_empty() || url.password().is_some() {
            return Err(CatalogError::Invalid(format!(
                "catalog base URL must be https without credentials: `{s}`"
            )));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(CatalogError::Invalid(format!(
                "catalog base URL must not have a query or fragment: `{s}`"
            )));
        }
        Ok(url)
    }
}

/// Fetches bytes over the network. Implemented with reqwest by [`HttpTransport`];
/// tests use in-memory transports.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// GET `url`, failing when the body exceeds `max_bytes` or the status is not 200.
    async fn get(&self, url: &Url, max_bytes: u64) -> Result<Vec<u8>>;
}

/// reqwest transport (no redirects to other hosts, no cookies, no credentials).
#[cfg(feature = "http")]
#[derive(Clone, Debug)]
pub struct HttpTransport {
    client: reqwest::Client,
}

#[cfg(feature = "http")]
impl HttpTransport {
    /// A client with a 30 s timeout and a Superba user agent.
    pub fn new() -> Result<HttpTransport> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent(concat!("superba-catalog/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| CatalogError::Invalid(format!("HTTP client: {e}")))?;
        Ok(HttpTransport { client })
    }
}

#[cfg(feature = "http")]
#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn get(&self, url: &Url, max_bytes: u64) -> Result<Vec<u8>> {
        let net = |m: String| CatalogError::Network {
            url: url.to_string(),
            message: m,
        };
        let mut resp = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| net(e.to_string()))?;
        if resp.status() != reqwest::StatusCode::OK {
            return Err(net(format!("HTTP {}", resp.status().as_u16())));
        }
        if resp.content_length().is_some_and(|l| l > max_bytes) {
            return Err(net(format!("larger than {max_bytes} bytes")));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| net(e.to_string()))? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > max_bytes {
                return Err(net(format!("larger than {max_bytes} bytes")));
            }
        }
        Ok(body)
    }
}

/// Result of [`refresh`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Remote updates are switched off; nothing was requested.
    Disabled,
    /// The last check is more recent than [`RemoteConfig::min_interval_secs`].
    NotDue,
    /// The remote index is the one in use.
    UpToDate {
        /// Serial in use.
        serial: u64,
    },
    /// A newer catalog was verified and stored in the cache.
    Updated {
        /// New serial.
        serial: u64,
        /// Number of files downloaded (the rest were already present).
        downloaded: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheState {
    last_check_unix: u64,
}

/// The cache directory of the catalog inside Superba's data directory.
pub fn cache_dir(data_dir: &superba_core::DataDir) -> PathBuf {
    data_dir.root().join("catalog")
}

/// The verified tree inside a cache directory.
pub fn cache_tree(cache_dir: &Path) -> PathBuf {
    cache_dir.join("tree")
}

/// Check for a newer catalog and store it in `cache_dir` (see the module docs).
/// `current` is the catalog in use; `now_unix` is the current time in seconds.
pub async fn refresh(
    config: &RemoteConfig,
    transport: &dyn Transport,
    cache_dir: &Path,
    current: &CatalogStore,
    now_unix: u64,
    force: bool,
) -> Result<RefreshOutcome> {
    refresh_with_keys(
        config,
        transport,
        cache_dir,
        current,
        now_unix,
        force,
        TRUSTED_KEYS,
        REVOKED_KEY_IDS,
    )
    .await
}

/// [`refresh`] with explicit trusted and revoked keys (tests).
#[allow(clippy::too_many_arguments)]
pub async fn refresh_with_keys(
    config: &RemoteConfig,
    transport: &dyn Transport,
    cache_dir: &Path,
    current: &CatalogStore,
    now_unix: u64,
    force: bool,
    trusted: &[TrustedKey],
    revoked: &[&str],
) -> Result<RefreshOutcome> {
    if !config.enabled {
        return Ok(RefreshOutcome::Disabled);
    }
    let base = config.base()?;
    let state_path = cache_dir.join("state.json");
    let last_check = std::fs::read(&state_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<CacheState>(&b).ok())
        .map(|s| s.last_check_unix);
    let due = match last_check {
        None => true,
        // A clock that went backwards makes the check due.
        Some(last) => now_unix < last || now_unix - last >= config.min_interval_secs,
    };
    if !force && !due {
        return Ok(RefreshOutcome::NotDue);
    }

    let join = |rel: &str| {
        base.join(rel)
            .map_err(|e| CatalogError::Invalid(format!("catalog URL for {rel}: {e}")))
    };
    let index_bytes = transport.get(&join(INDEX_PATH)?, MAX_INDEX_BYTES).await?;
    let sig_bytes = transport
        .get(&join(SIGNATURE_PATH)?, MAX_INDEX_BYTES)
        .await?;
    // Signature first: nothing else in an unsigned index is looked at.
    let status =
        crate::integrity::verify_signature(&index_bytes, Some(&sig_bytes), trusted, revoked);
    if !status.is_valid() {
        let reason = match status {
            crate::integrity::SignatureStatus::Invalid { reason } => reason,
            _ => "the index is not signed".into(),
        };
        return Err(CatalogError::Signature(reason));
    }
    let index = parse_index(&index_bytes)?;
    let current_serial = current.index().serial;
    if index.serial < current_serial {
        return Err(CatalogError::Rollback {
            found: index.serial,
            current: current_serial,
        });
    }
    let write_state = || -> Result<()> {
        std::fs::create_dir_all(cache_dir).map_err(|e| CatalogError::io(cache_dir, &e))?;
        let s = serde_json::to_vec(&CacheState {
            last_check_unix: now_unix,
        })
        .map_err(|e| CatalogError::Invalid(e.to_string()))?;
        std::fs::write(&state_path, s).map_err(|e| CatalogError::io(&state_path, &e))
    };
    if index.serial == current_serial && current.file(INDEX_PATH) == Some(index_bytes.as_slice()) {
        write_state()?;
        return Ok(RefreshOutcome::UpToDate {
            serial: current_serial,
        });
    }

    let total: u64 = index.files.iter().map(|f| f.size).sum();
    if total > MAX_TOTAL_BYTES {
        return Err(CatalogError::integrity(INDEX_PATH, "catalog is too large"));
    }
    let mut files = BTreeMap::new();
    let mut downloaded = 0usize;
    for f in &index.files {
        validate_relative_path(&f.path)?;
        if f.size > MAX_FILE_BYTES {
            return Err(CatalogError::integrity(&f.path, "file is too large"));
        }
        let reuse = current
            .file(&f.path)
            .filter(|b| b.len() as u64 == f.size && sha256_hex(b).eq_ignore_ascii_case(&f.sha256));
        let bytes = match reuse {
            Some(b) => b.to_vec(),
            None => {
                downloaded += 1;
                let b = transport.get(&join(&f.path)?, f.size).await?;
                if b.len() as u64 != f.size || !sha256_hex(&b).eq_ignore_ascii_case(&f.sha256) {
                    return Err(CatalogError::integrity(
                        &f.path,
                        "download does not match the index",
                    ));
                }
                b
            }
        };
        files.insert(f.path.clone(), bytes);
    }
    files.insert(INDEX_PATH.to_owned(), index_bytes);
    files.insert(SIGNATURE_PATH.to_owned(), sig_bytes);

    // Full verification, including every version file parsing, before it is stored.
    let tree = cache_tree(cache_dir);
    let store = CatalogStore::from_files_with_keys(
        StoreOrigin::Cache(tree.clone()),
        files,
        TrustPolicy::RequireSignature,
        trusted,
        revoked,
    )?;
    for v in store.versions().cloned().collect::<Vec<_>>() {
        store.exact(&v)?;
    }
    write_tree(cache_dir, store.files())?;
    write_state()?;
    Ok(RefreshOutcome::Updated {
        serial: index.serial,
        downloaded,
    })
}

/// Replace `cache_dir/tree` with `files`: write `tree.new`, then swap.
fn write_tree(cache_dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let io = |p: &Path, e: std::io::Error| CatalogError::io(p, &e);
    let next = cache_dir.join("tree.new");
    let old = cache_dir.join("tree.old");
    let tree = cache_tree(cache_dir);
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
