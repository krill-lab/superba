//! Remote compaction of a provider ([`superba_core::SwitchService::provider_compaction`],
//! GitHub issue 3): the check ([`superba_core::LineProber::probe_compaction`])
//! at the base URL the local router uses for the provider's first line, cached
//! in `<data dir>/cache/compaction.json` per (site, base URL, key fingerprint)
//! with the time of the check and the model it used, combined with the
//! profile's choice ([`superba_core::CompactionChoice`]) and Superba's fact for
//! Krill (native), and handed to the router as the member's
//! [`superba_router::config::MemberConfig::compaction`].
//!
//! A check costs a few tokens, so only the provider page (opened: missing or
//! stale results; Test / Refresh: always) and `spb provider compaction --check`
//! ask for one; the planner and the router read the cache only.
//!
//! The cache holds no key (a short SHA-256 fingerprint) and no content.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    CompactionCheck, CompactionRequest, CompactionSupport, Error, KeyInput, Profile,
    ProviderCompaction, ResolvedEndpoints, Result, Secret, Site, SiteKind, Timestamp,
    TransportRefresh,
};
use url::Url;

use crate::Switcher;
use crate::transports::{key_fingerprint, provider_lines};

/// File name under the data dir's `cache/`.
pub(crate) const CACHE_FILE: &str = "compaction.json";
/// Format of the cache file.
const FORMAT: u32 = 1;
/// Entries kept at most (oldest checks are dropped first).
const MAX_ENTRIES: usize = 256;

/// Superba's fact for a site: Krill compacts natively (its relay passes
/// `compaction_trigger` to the Codex backend); anything else is unknown until
/// checked.
pub fn builtin_support(site: &Site) -> CompactionSupport {
    match site.kind {
        SiteKind::Krill => CompactionSupport::Native,
        _ => CompactionSupport::Unknown,
    }
}

/// The cache file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    #[serde(default)]
    entries: Vec<Entry>,
}

/// One cached check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// Site.
    pub site_id: String,
    /// Base URL the provider was checked at.
    pub base_url: String,
    /// Key fingerprint (first 8 bytes of SHA-256, hex).
    pub key: String,
    /// When.
    pub checked_at: Timestamp,
    /// The model the check used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The result.
    pub check: CompactionCheck,
}

/// The cache at `path`.
#[derive(Clone, Debug)]
pub(crate) struct CompactionCache {
    path: PathBuf,
}

impl CompactionCache {
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

    /// The cached check of `site` at `base` for `key`.
    pub(crate) fn get(&self, site: &Site, base: &Url, key: &str) -> Option<Entry> {
        self.load()
            .entries
            .into_iter()
            .find(|e| e.key == key && e.base_url == base.as_str() && e.site_id == site.id.as_str())
    }

    /// Store `entry`, replacing an earlier check of the same site, base URL and
    /// key.
    pub(crate) fn put(&self, entry: Entry) -> Result<()> {
        let mut file = self.load();
        file.format = FORMAT;
        file.entries.retain(|e| {
            !(e.key == entry.key && e.base_url == entry.base_url && e.site_id == entry.site_id)
        });
        file.entries.push(entry);
        if file.entries.len() > MAX_ENTRIES {
            file.entries
                .sort_by_key(|e| std::cmp::Reverse(e.checked_at));
            file.entries.truncate(MAX_ENTRIES);
        }
        let mut bytes = serde_json::to_vec_pretty(&file)
            .map_err(|e| Error::other(format!("compaction cache: {e}")))?;
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

/// The base URL a provider is checked at: the router's base URL of its first
/// line (`None` without lines).
pub(crate) fn check_base(site: &Site, profile: &Profile) -> Result<Option<Url>> {
    match provider_lines(site, profile).first() {
        Some(line) => crate::router::member_line_base(site, profile, line).map(Some),
        None => Ok(None),
    }
}

/// The record of `profile` from `entry` (the cached check, if any).
pub(crate) fn record(site: &Site, profile: &Profile, entry: Option<Entry>) -> ProviderCompaction {
    ProviderCompaction::new(
        profile.id.clone(),
        site.id.clone(),
        profile.compaction,
        builtin_support(site),
        entry.map(|e| (e.checked_at, e.model, e.check)),
    )
}

impl Switcher {
    fn compaction_cache(&self) -> CompactionCache {
        CompactionCache::new(self.deps.ctx.data_dir.cache_dir().join(CACHE_FILE))
    }

    /// The provider's record from the cache only (no request): what the
    /// planner and the router hints use.
    pub(crate) fn cached_compaction(
        &self,
        site: &Site,
        profile: &Profile,
        key: Option<&Secret>,
    ) -> ProviderCompaction {
        let entry = key.and_then(|key| {
            let base = check_base(site, profile).ok().flatten()?;
            self.compaction_cache()
                .get(site, &base, &key_fingerprint(key))
        });
        record(site, profile, entry)
    }

    /// The model a check uses: the profile's model (mapped to the site's
    /// name), else the first model the provider lists.
    async fn compaction_model(
        &self,
        site: &Site,
        profile: &Profile,
        base: &Url,
        key: &Secret,
    ) -> Result<String> {
        if let Some(m) = profile
            .model
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            return Ok(site.site_model(m).to_owned());
        }
        let models = self.deps.prober.list_models(base, key).await?;
        models.first().map(|m| m.id.clone()).ok_or_else(|| {
            Error::invalid(
                "no model to check compaction with: the provider lists no models; set the provider's model",
            )
        })
    }

    pub(crate) async fn provider_compaction_impl(
        &self,
        request: CompactionRequest,
    ) -> Result<ProviderCompaction> {
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
                "the official ChatGPT login compacts with the ChatGPT backend; there is nothing to check",
            ));
        }
        let key = match profile.key_ref {
            Some(_) => Some(
                self.resolve_key(KeyInput::Profile(profile.id.clone()))
                    .await?,
            ),
            None => None,
        };
        let cached = self.cached_compaction(&site, &profile, key.as_ref());
        let due = match request.refresh {
            TransportRefresh::All => true,
            TransportRefresh::Missing => cached.is_stale(self.deps.clock.now()),
            _ => false,
        };
        let (Some(key), true) = (key, due) else {
            return Ok(cached);
        };
        let Some(base) = check_base(&site, &profile)? else {
            return Ok(cached);
        };
        let model = self.compaction_model(&site, &profile, &base, &key).await?;
        let endpoints = ResolvedEndpoints::for_base_url(&base)?;
        let check = self
            .deps
            .prober
            .probe_compaction(&endpoints, &key, &model)
            .await?;
        let entry = Entry {
            site_id: site.id.to_string(),
            base_url: base.to_string(),
            key: key_fingerprint(&key),
            checked_at: self.deps.clock.now(),
            model: Some(model),
            check,
        };
        if let Err(e) = self.compaction_cache().put(entry.clone()) {
            tracing::warn!(error = %e, "compaction check not cached");
        }
        let fresh = record(&site, &profile, Some(entry));
        // A router member's new value reaches the running router (a reload).
        if fresh.support != cached.support
            && set.settings.router.members.contains(&profile.id)
            && let Err(e) = self.resync_router_members(&set).await
        {
            tracing::warn!(error = %e, "router compaction hint not updated after a check");
        }
        Ok(fresh)
    }
}

// ---------------------------------------------------------------------------
// Switch notices
// ---------------------------------------------------------------------------

/// Conversations compacted in the last this-many days are counted by
/// [`compacted_elsewhere`].
pub const RECENT_DAYS: u64 = 7;
/// Longest wait for the rollout index while planning (a cold index keeps
/// filling in the background; the notice is left out then).
const SCAN_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// One conversation's latest compaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compacted {
    /// Thread.
    pub thread_id: String,
    /// The provider (profile) that compacted it.
    pub profile_id: superba_core::ProfileId,
    /// Its name.
    pub name: String,
}

/// The latest successful compaction of each thread among `rows` (rollout
/// requests, attributed with `a`).
pub fn latest_compactions(
    rows: &[superba_sessions::usage::RolloutRequest],
    a: &crate::usage::Attribution,
) -> Vec<Compacted> {
    let mut latest: std::collections::BTreeMap<&str, &superba_sessions::usage::RolloutRequest> =
        std::collections::BTreeMap::new();
    for r in rows.iter().filter(|r| {
        r.kind == superba_core::RequestKind::Compaction
            && r.status == superba_core::RequestStatus::Ok
    }) {
        let e = latest.entry(r.thread_id.as_str()).or_insert(r);
        if r.completed_at > e.completed_at {
            *e = r;
        }
    }
    latest
        .into_values()
        .map(|r| {
            let (profile_id, name) = a.member(r.model_provider.as_deref(), r.started_at);
            Compacted {
                thread_id: r.thread_id.clone(),
                profile_id,
                name,
            }
        })
        .collect()
}

/// The conversations of `compacted` whose compaction was made by a provider
/// on another site than `dest_site` (`site_of` maps a profile to its site),
/// and the most frequent such provider's name.
pub fn compacted_elsewhere(
    compacted: &[Compacted],
    dest_site: &superba_core::SiteId,
    site_of: impl Fn(&superba_core::ProfileId) -> Option<superba_core::SiteId>,
) -> Option<(usize, String)> {
    let mut by: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut count = 0;
    for c in compacted {
        let Some(site) = site_of(&c.profile_id) else {
            continue;
        };
        if &site != dest_site {
            count += 1;
            *by.entry(c.name.as_str()).or_default() += 1;
        }
    }
    let top = by.into_iter().max_by_key(|(_, n)| *n)?.0.to_owned();
    Some((count, top))
}

impl Switcher {
    /// Remote compaction notices of a switch (cache and rollouts only; no
    /// request is sent): the destination cannot compact (or was never
    /// checked), router members skipped for compaction, and recent
    /// conversations compacted by a provider on another site.
    pub(crate) async fn compaction_notes(
        &self,
        set: &superba_core::ProfileSet,
        sites: &[Site],
        dest: CompactionDest<'_>,
    ) -> Vec<superba_core::Notice> {
        use superba_core::Notice;
        let mut out = Vec::new();
        let text = crate::messages::text;
        if let Some(members) = dest.router_members {
            let skipped: Vec<String> = members
                .iter()
                .filter(|m| m.compaction == CompactionSupport::Unsupported)
                .map(|m| format!("“{}”", m.name))
                .collect();
            if !skipped.is_empty() && skipped.len() == members.len() {
                out.push(Notice::warning(text("switch.warn.compaction_router_none")));
            } else if !skipped.is_empty() {
                out.push(Notice::info(
                    text("switch.warn.compaction_router").arg("members", skipped.join(", ")),
                ));
            }
            return out;
        }
        let (Some(profile), Some(site)) = (dest.profile, dest.site) else {
            return out;
        };
        if site.kind == SiteKind::Official {
            return out;
        }
        let pc = self.cached_compaction(site, profile, dest.key);
        let name = profile.name.clone();
        match pc.support {
            CompactionSupport::Unsupported => out.push(Notice::warning(
                text("switch.warn.compaction_unsupported").arg("site", &name),
            )),
            CompactionSupport::Unknown => out.push(Notice::info(
                text("switch.warn.compaction_unknown").arg("site", &name),
            )),
            _ => {}
        }
        if dest.from_site.is_some_and(|f| f != &site.id)
            && let Some((count, provider)) =
                self.recent_compactions_elsewhere(set, sites, site).await
            && count > 0
        {
            out.push(Notice::warning(
                text("switch.warn.compacted_elsewhere")
                    .arg("count", count.to_string())
                    .arg("provider", provider)
                    .arg("site", &name)
                    .arg("days", RECENT_DAYS.to_string()),
            ));
        }
        out
    }

    /// [`compacted_elsewhere`] over the last [`RECENT_DAYS`] of the local
    /// rollouts, within [`SCAN_BUDGET`].
    async fn recent_compactions_elsewhere(
        &self,
        set: &superba_core::ProfileSet,
        sites: &[Site],
        dest: &Site,
    ) -> Option<(usize, String)> {
        let now = self.deps.clock.now();
        let from = now.saturating_sub(std::time::Duration::from_secs(RECENT_DAYS * 86_400));
        let index = self.usage.clone();
        let scan = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .requests(Some(from), None)
        });
        let page = tokio::time::timeout(SCAN_BUDGET, scan).await.ok()?.ok()?;
        if !page
            .rows
            .iter()
            .any(|r| r.kind == superba_core::RequestKind::Compaction)
        {
            return None;
        }
        let a = self.attribution().await;
        let compacted = latest_compactions(&page.rows, &a);
        compacted_elsewhere(&compacted, &dest.id, |id| {
            set.profile(id)
                .and_then(|p| sites.iter().find(|s| s.id == p.site_id))
                .map(|s| s.id.clone())
        })
    }
}

/// What [`Switcher::compaction_notes`] needs to know about a switch.
#[derive(Clone, Copy, Default)]
pub(crate) struct CompactionDest<'a> {
    /// The destination profile (direct mode).
    pub profile: Option<&'a Profile>,
    /// Its site.
    pub site: Option<&'a Site>,
    /// Its key.
    pub key: Option<&'a Secret>,
    /// The site applied before the switch.
    pub from_site: Option<&'a superba_core::SiteId>,
    /// Router mode: the members in order (with their compaction support).
    pub router_members: Option<&'a [superba_router::config::MemberConfig]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use superba_core::catalog::krill_site;

    fn entry(site: &str, base: &str, key: &str, at: i64, s: CompactionSupport) -> Entry {
        Entry {
            site_id: site.into(),
            base_url: base.into(),
            key: key.into(),
            checked_at: Timestamp::from_millis(at),
            model: Some("gpt-x".into()),
            check: CompactionCheck::new(s, Some(200), "x"),
        }
    }

    #[test]
    fn the_cache_keeps_one_check_per_site_base_and_key_and_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache").join(CACHE_FILE);
        let cache = CompactionCache::new(path.clone());
        let site = krill_site();
        let base = Url::parse("https://a.example/backend-api/codex").unwrap();
        assert_eq!(cache.get(&site, &base, "k1"), None);
        let first = entry("krill", base.as_str(), "k1", 1, CompactionSupport::Unknown);
        cache.put(first.clone()).unwrap();
        assert_eq!(cache.get(&site, &base, "k1"), Some(first));
        assert_eq!(cache.get(&site, &base, "k2"), None);
        let second = entry("krill", base.as_str(), "k1", 2, CompactionSupport::Native);
        cache.put(second.clone()).unwrap();
        assert_eq!(cache.get(&site, &base, "k1"), Some(second));
        assert_eq!(cache.load().entries.len(), 1);
        let key = Secret::new("sk-compaction-cache-test-0123456789");
        cache
            .put(entry(
                "krill",
                "https://b.example/",
                &key_fingerprint(&key),
                3,
                CompactionSupport::Native,
            ))
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(key.expose()));
        std::fs::write(&path, b"{oops").unwrap();
        assert_eq!(cache.get(&site, &base, "k1"), None);
    }

    #[test]
    fn krill_is_native_until_a_definite_check_or_a_choice_says_otherwise() {
        let site = krill_site();
        let mut profile = superba_core::catalog::builtin_profiles()[0].clone();
        profile.builtin = None;
        profile.site_id = site.id.clone();
        let r = record(&site, &profile, None);
        assert_eq!(r.support, CompactionSupport::Native);
        assert_eq!(r.source, superba_core::CompactionSource::Builtin);
        // An inconclusive check keeps the fact.
        let unknown = entry("krill", "b", "k", 1, CompactionSupport::Unknown);
        assert_eq!(
            record(&site, &profile, Some(unknown)).support,
            CompactionSupport::Native
        );
        let refused = entry("krill", "b", "k", 1, CompactionSupport::Unsupported);
        let r = record(&site, &profile, Some(refused.clone()));
        assert_eq!(r.support, CompactionSupport::Unsupported);
        assert_eq!(r.source, superba_core::CompactionSource::Check);
        profile.compaction = superba_core::CompactionChoice::Native;
        let r = record(&site, &profile, Some(refused));
        assert_eq!(r.support, CompactionSupport::Native);
        assert_eq!(r.source, superba_core::CompactionSource::Override);
    }
}
