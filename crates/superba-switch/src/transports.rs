//! Transport checks of a provider's lines ([`superba_core::SwitchService::line_transports`]):
//! the Responses WebSocket and zstd request bodies per line, probed with the
//! provider's key at the base URL the local router uses for that line, cached
//! in `<data dir>/cache/transports.json` per (site, line, base URL, key
//! fingerprint) with the time of the check, and handed to the router as
//! per-line hints ([`Switcher::apply_transport_hints`]).
//!
//! The cache holds no key (a short SHA-256 fingerprint only) and no answer
//! body beyond the redacted one-line detail.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use superba_core::{
    Error, KeyInput, Line, LineId, LineTransports, LineTransportsRequest, Profile,
    ResolvedEndpoints, Result, Secret, Site, SiteKind, TransportRefresh,
};
use superba_router::config::MemberConfig;
use url::Url;

use crate::Switcher;

/// File name under the data dir's `cache/`.
pub(crate) const CACHE_FILE: &str = "transports.json";
/// Format of the cache file.
const FORMAT: u32 = 1;
/// Entries kept at most (oldest checks are dropped first).
const MAX_ENTRIES: usize = 512;

/// The cache file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    #[serde(default)]
    entries: Vec<Entry>,
}

/// One cached check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// Key fingerprint (first 8 bytes of SHA-256, hex).
    key: String,
    /// Base URL the line was checked at.
    base_url: String,
    /// The result.
    result: LineTransports,
}

/// Short, irreversible fingerprint of a key.
pub(crate) fn key_fingerprint(key: &Secret) -> String {
    let digest = Sha256::digest(key.expose().as_bytes());
    hex::encode(&digest[..8])
}

/// The cache at `path`.
#[derive(Clone, Debug)]
pub(crate) struct TransportCache {
    path: PathBuf,
}

impl TransportCache {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> CacheFile {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
            .filter(|f| f.format == FORMAT)
            .unwrap_or_default()
    }

    /// The cached result of `line` at `base` for `key`.
    pub(crate) fn get(
        &self,
        site: &Site,
        line: &LineId,
        base: &Url,
        key: &str,
    ) -> Option<LineTransports> {
        self.load()
            .entries
            .into_iter()
            .find(|e| {
                e.key == key
                    && e.base_url == base.as_str()
                    && e.result.site_id == site.id
                    && &e.result.line_id == line
            })
            .map(|e| e.result)
    }

    /// Store `results` (each with its base URL) for `key`, replacing earlier
    /// checks of the same line, base URL and key.
    pub(crate) fn put(&self, key: &str, results: &[(Url, LineTransports)]) -> Result<()> {
        let mut file = self.load();
        file.format = FORMAT;
        file.entries.retain(|e| {
            !results.iter().any(|(base, r)| {
                e.key == key
                    && e.base_url == base.as_str()
                    && e.result.site_id == r.site_id
                    && e.result.line_id == r.line_id
            })
        });
        file.entries.extend(results.iter().map(|(base, r)| Entry {
            key: key.to_owned(),
            base_url: base.to_string(),
            result: r.clone(),
        }));
        if file.entries.len() > MAX_ENTRIES {
            file.entries
                .sort_by_key(|e| std::cmp::Reverse(e.result.checked_at));
            file.entries.truncate(MAX_ENTRIES);
        }
        let mut bytes = serde_json::to_vec_pretty(&file)
            .map_err(|e| Error::other(format!("transport cache: {e}")))?;
        bytes.push(b'\n');
        write_atomic(&self.path, &bytes)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    let tmp = dir.join(format!(
        ".{CACHE_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(path, &e)
    })
}

/// The lines of `profile`'s provider in priority order: the profile's order,
/// then the site's other lines (what the router fails over across), without
/// the disabled ones: transport checks never dial them.
pub(crate) fn provider_lines<'a>(site: &'a Site, profile: &Profile) -> Vec<&'a Line> {
    profile
        .enabled_line_ids(site)
        .iter()
        .filter_map(|id| site.line(id))
        .collect()
}

impl Switcher {
    fn transport_cache(&self) -> TransportCache {
        TransportCache::new(self.deps.ctx.data_dir.cache_dir().join(CACHE_FILE))
    }

    pub(crate) async fn line_transports_impl(
        &self,
        request: LineTransportsRequest,
    ) -> Result<Vec<LineTransports>> {
        let set = self.deps.profiles.load().await?;
        let profile = set
            .profile(&request.profile)
            .ok_or_else(|| Error::NotFound {
                what: format!("profile {}", request.profile),
            })?
            .clone();
        let site = self.site(&profile.site_id).await?;
        if profile.is_builtin() || site.kind == SiteKind::Official {
            return Err(Error::invalid(
                "the official ChatGPT login has no lines to check",
            ));
        }
        let key = self
            .resolve_key(KeyInput::Profile(profile.id.clone()))
            .await?;
        let fp = key_fingerprint(&key);
        let wanted: BTreeSet<&LineId> = request.lines.iter().collect();
        let lines: Vec<&Line> = provider_lines(&site, &profile)
            .into_iter()
            .filter(|l| wanted.is_empty() || wanted.contains(&l.id))
            .collect();
        let cache = self.transport_cache();
        let now = self.deps.clock.now();
        let mut out = Vec::with_capacity(lines.len());
        let mut todo = Vec::new();
        for line in &lines {
            let base = crate::router::member_line_base(&site, &profile, line)?;
            let cached = cache.get(&site, &line.id, &base, &fp);
            let probe = match (&cached, request.refresh) {
                (_, TransportRefresh::All) => true,
                (None, TransportRefresh::Missing) => true,
                (Some(c), TransportRefresh::Missing) => c.is_stale(now),
                _ => false,
            };
            if probe {
                todo.push((line.id.clone(), base));
            }
            out.push(cached);
        }
        if todo.is_empty() {
            return Ok(out.into_iter().flatten().collect());
        }
        let prober = &self.deps.prober;
        let checks = todo.iter().map(|(_, base)| async {
            let endpoints = ResolvedEndpoints::for_base_url(base)?;
            prober.probe_transports(&endpoints, &key).await
        });
        let results = futures_util::future::join_all(checks).await;
        let checked_at = self.deps.clock.now();
        let mut fresh = Vec::new();
        let mut first_error = None;
        for ((line, base), r) in todo.into_iter().zip(results) {
            match r {
                Ok(p) => fresh.push((
                    base,
                    LineTransports::from_probe(site.id.clone(), line, checked_at, p),
                )),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        if fresh.is_empty()
            && let Some(e) = first_error
        {
            return Err(e);
        }
        let changed = fresh.iter().any(|(base, r)| {
            cache.get(&site, &r.line_id, base, &fp).is_none_or(|old| {
                old.websocket.support != r.websocket.support || old.zstd.support != r.zstd.support
            })
        });
        if let Err(e) = cache.put(&fp, &fresh) {
            tracing::warn!(error = %e, "transport checks not cached");
        }
        for (_, r) in fresh {
            if let Some(slot) = lines
                .iter()
                .position(|l| l.id == r.line_id)
                .and_then(|i| out.get_mut(i))
            {
                *slot = Some(r);
            }
        }
        // A router member's new hints reach the running router (a reload).
        if changed
            && set.settings.router.members.contains(&profile.id)
            && let Err(e) = self.resync_router_members(&set).await
        {
            tracing::warn!(error = %e, "router hints not updated after a transport check");
        }
        Ok(out.into_iter().flatten().collect())
    }

    /// Per-line zstd and WebSocket hints of `member` (built from `profile` on
    /// `site` with `key`) from the cached checks, and the member's WebSocket
    /// flag derived from them. Lines never checked keep `None`.
    pub(crate) fn apply_transport_hints(
        &self,
        member: &mut MemberConfig,
        site: &Site,
        profile: &Profile,
        key: &Secret,
    ) {
        let cache = self.transport_cache();
        let fp = key_fingerprint(key);
        for (line_cfg, line) in member.lines.iter_mut().zip(provider_lines(site, profile)) {
            if let Some(r) = cache.get(site, &line.id, &line_cfg.base_url, &fp) {
                line_cfg.zstd = r.zstd.support.as_hint();
                line_cfg.websocket = r.websocket.support.as_hint();
            }
        }
        member.apply_websocket_hints();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superba_core::{Timestamp, TransportCheck, TransportProbe, TransportSupport};

    fn result(line: &str, at: i64, zstd: TransportSupport) -> LineTransports {
        LineTransports::from_probe(
            superba_core::SiteId::new("krill"),
            LineId::new(line),
            Timestamp::from_millis(at),
            TransportProbe {
                websocket: TransportCheck::new(
                    TransportSupport::Unsupported,
                    Some(426),
                    "HTTP 426",
                ),
                zstd: TransportCheck::new(zstd, Some(400), "decoded"),
            },
        )
    }

    #[test]
    fn the_cache_keeps_one_result_per_line_base_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TransportCache::new(dir.path().join("cache").join(CACHE_FILE));
        let site = superba_core::catalog::krill_site();
        let base = Url::parse("https://a.example/backend-api/codex").unwrap();
        let other = Url::parse("https://b.example/backend-api/codex").unwrap();
        let line = LineId::new("a");
        assert_eq!(cache.get(&site, &line, &base, "k1"), None);
        let first = result("a", 1, TransportSupport::Unknown);
        cache.put("k1", &[(base.clone(), first.clone())]).unwrap();
        assert_eq!(cache.get(&site, &line, &base, "k1"), Some(first));
        assert_eq!(cache.get(&site, &line, &base, "k2"), None, "other key");
        assert_eq!(cache.get(&site, &line, &other, "k1"), None, "other base");
        let second = result("a", 2, TransportSupport::Supported);
        cache.put("k1", &[(base.clone(), second.clone())]).unwrap();
        assert_eq!(cache.get(&site, &line, &base, "k1"), Some(second));
        assert_eq!(cache.load().entries.len(), 1);
        // A key never lands in the file; garbage reads as empty.
        let key = Secret::new("sk-transport-cache-test-0123456789");
        let fp = key_fingerprint(&key);
        cache
            .put(&fp, &[(other, result("a", 3, TransportSupport::Supported))])
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join("cache").join(CACHE_FILE)).unwrap();
        assert!(!text.contains(key.expose()));
        assert_eq!(fp.len(), 16);
        std::fs::write(dir.path().join("cache").join(CACHE_FILE), b"{oops").unwrap();
        assert_eq!(cache.get(&site, &line, &base, "k1"), None);
    }
}
