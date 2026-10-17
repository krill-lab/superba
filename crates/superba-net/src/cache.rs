//! Caches for account data.
//!
//! - **Memory** ([`SummaryCache`]): account summaries per (site, line origin, key
//!   fingerprint) for [`SUMMARY_TTL`] (errors for [`SUMMARY_ERROR_TTL`]), so an
//!   overview polled every few seconds does not query the billing endpoints each
//!   time. A site's billing endpoints that answered 404/405 are remembered as
//!   missing for [`MISSING_TTL`] per origin.
//! - **Disk** ([`UsageCache`]): usage reports per (site, key fingerprint, range)
//!   under `<data dir>/cache/usage/`, because the usage API is rate-limited
//!   (Krill: 10 requests per hour per user) and reports must survive restarts of
//!   the short-lived `spb` process. Files hold only usage numbers, never keys; the
//!   key appears only as a truncated SHA-256 fingerprint inside the hashed file
//!   name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use superba_core::{AccountSummary, Error, Secret, SiteId, Timestamp, UsageReport};

/// Fresh account summaries are reused for this long.
pub(crate) const SUMMARY_TTL: Duration = Duration::from_secs(60);
/// Failed account summaries are reused for this long (a dead line is not
/// re-queried on every overview poll).
pub(crate) const SUMMARY_ERROR_TTL: Duration = Duration::from_secs(20);
/// Billing endpoints that answered 404/405 are not asked again for this long.
pub(crate) const MISSING_TTL: Duration = Duration::from_secs(600);
/// Usage cache files older than this are deleted when a new one is written.
const USAGE_FILE_MAX_AGE: Duration = Duration::from_secs(14 * 86_400);

/// Short, irreversible fingerprint of a key (first 8 bytes of its SHA-256).
pub(crate) fn key_fingerprint(key: &Secret) -> String {
    let digest = Sha256::digest(key.expose().as_bytes());
    hex::encode(&digest[..8])
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SummaryKey {
    site: SiteId,
    origin: String,
    key: String,
}

#[derive(Debug, Clone)]
struct SummaryEntry {
    at: Duration,
    value: Result<Option<AccountSummary>, Error>,
}

/// In-memory account summary cache (monotonic clock times).
#[derive(Debug, Default)]
pub(crate) struct SummaryCache {
    entries: Mutex<HashMap<SummaryKey, SummaryEntry>>,
    missing_billing: Mutex<HashMap<(SiteId, String), Duration>>,
}

impl SummaryCache {
    fn key(site: &SiteId, origin: &str, key: &Secret) -> SummaryKey {
        SummaryKey {
            site: site.clone(),
            origin: origin.to_owned(),
            key: key_fingerprint(key),
        }
    }

    /// A cached answer still within its TTL at monotonic time `now`.
    pub fn get(
        &self,
        site: &SiteId,
        origin: &str,
        key: &Secret,
        now: Duration,
    ) -> Option<Result<Option<AccountSummary>, Error>> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(&Self::key(site, origin, key))?;
        let ttl = if entry.value.is_ok() {
            SUMMARY_TTL
        } else {
            SUMMARY_ERROR_TTL
        };
        (now.saturating_sub(entry.at) < ttl).then(|| entry.value.clone())
    }

    /// Remember an answer.
    pub fn put(
        &self,
        site: &SiteId,
        origin: &str,
        key: &Secret,
        now: Duration,
        value: Result<Option<AccountSummary>, Error>,
    ) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|_, e| now.saturating_sub(e.at) < SUMMARY_TTL);
            entries.insert(
                Self::key(site, origin, key),
                SummaryEntry { at: now, value },
            );
        }
    }

    /// True while the billing endpoints of `origin` are known to be missing.
    pub fn billing_missing(&self, site: &SiteId, origin: &str, now: Duration) -> bool {
        self.missing_billing
            .lock()
            .ok()
            .and_then(|m| m.get(&(site.clone(), origin.to_owned())).copied())
            .is_some_and(|at| now.saturating_sub(at) < MISSING_TTL)
    }

    /// Record that the billing endpoints of `origin` are missing (or not anymore).
    pub fn set_billing_missing(&self, site: &SiteId, origin: &str, now: Duration, missing: bool) {
        if let Ok(mut m) = self.missing_billing.lock() {
            let k = (site.clone(), origin.to_owned());
            if missing {
                m.insert(k, now);
            } else {
                m.remove(&k);
            }
        }
    }
}

/// On-disk file format of one cached usage report.
#[derive(Debug, Serialize, Deserialize)]
struct UsageFile {
    format: u32,
    report: UsageReport,
}

const USAGE_FILE_FORMAT: u32 = 1;

/// On-disk usage report cache (`None` dir = disabled).
#[derive(Debug, Default)]
pub(crate) struct UsageCache {
    dir: Option<PathBuf>,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl UsageCache {
    /// Cache under `dir` (created on first write).
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    fn path(&self, site: &SiteId, key: &Secret, start: Timestamp, days: u32) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let id = format!(
            "usage|{}|{}|{}|{}",
            site.as_str(),
            key_fingerprint(key),
            start.as_millis(),
            days
        );
        let name = hex::encode(&Sha256::digest(id.as_bytes())[..16]);
        Some(dir.join(format!("{name}.json")))
    }

    /// The cached report for this key and range, if any (unreadable or foreign
    /// files count as missing).
    pub fn load(
        &self,
        site: &SiteId,
        key: &Secret,
        start: Timestamp,
        days: u32,
    ) -> Option<UsageReport> {
        let path = self.path(site, key, start, days)?;
        let bytes = std::fs::read(&path).ok()?;
        let file: UsageFile = serde_json::from_slice(&bytes).ok()?;
        (file.format == USAGE_FILE_FORMAT
            && &file.report.site_id == site
            && file.report.start == start
            && file.report.days == days)
            .then_some(file.report)
    }

    /// Store a report (atomic replace) and prune files not written for two weeks
    /// (by their modification time, so against the real clock). Failures are
    /// logged and otherwise ignored: the cache is an optimisation.
    pub fn store(&self, key: &Secret, report: &UsageReport) {
        let Some(path) = self.path(&report.site_id, key, report.start, report.days) else {
            return;
        };
        if let Err(e) = write_atomic(&path, report) {
            tracing::debug!(error = %e, "cannot write the usage cache");
        }
        if let Some(dir) = path.parent() {
            prune(dir, SystemTime::now());
        }
    }
}

fn write_atomic(path: &Path, report: &UsageReport) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut report = report.clone();
    report.from_cache = false;
    report.notice = None;
    let bytes = serde_json::to_vec(&UsageFile {
        format: USAGE_FILE_FORMAT,
        report,
    })
    .map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn prune(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > USAGE_FILE_MAX_AGE);
        let is_json = path.extension().is_some_and(|e| e == "json");
        if old && is_json {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use superba_core::{CostSource, Usd};

    use super::*;

    fn report(site: &str, start: Timestamp, days: u32) -> UsageReport {
        UsageReport {
            site_id: SiteId::new(site),
            start,
            end: start.saturating_add(Duration::from_secs(u64::from(days) * 86_400)),
            days,
            daily: Vec::new(),
            models: Vec::new(),
            total_cost: Some(Usd::from_micros(42)),
            cost_source: Some(CostSource::Costs),
            usage_available: true,
            fetched_at: start,
            from_cache: true,
            notice: None,
        }
    }

    #[test]
    fn fingerprints_are_short_and_do_not_contain_the_key() {
        let key = Secret::new("sk-secret-value-1234");
        let fp = key_fingerprint(&key);
        assert_eq!(fp.len(), 16);
        assert!(!fp.contains("1234"));
        assert_ne!(fp, key_fingerprint(&Secret::new("sk-secret-value-1235")));
    }

    #[test]
    fn usage_reports_round_trip_per_key_and_range() {
        let dir = tempfile::tempdir().unwrap();
        let cache = UsageCache::new(Some(dir.path().join("usage")));
        let key = Secret::new("sk-secret-value-1234");
        let start = Timestamp::from_millis(1_793_491_200_000);
        let r = report("krill", start, 7);
        assert!(cache.load(&r.site_id, &key, start, 7).is_none());
        cache.store(&key, &r);
        let back = cache.load(&r.site_id, &key, start, 7).unwrap();
        assert!(!back.from_cache, "flags are not persisted");
        assert_eq!(back.total_cost, r.total_cost);
        assert!(cache.load(&r.site_id, &key, start, 30).is_none());
        assert!(
            cache
                .load(&r.site_id, &Secret::new("sk-other"), start, 7)
                .is_none()
        );
        for entry in std::fs::read_dir(dir.path().join("usage")).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.contains("1234") && !name.starts_with(".tmp"));
            let text = std::fs::read_to_string(entry.path()).unwrap();
            assert!(!text.contains("sk-secret"));
        }
        assert!(
            UsageCache::default()
                .load(&r.site_id, &key, start, 7)
                .is_none()
        );
    }

    #[test]
    fn summaries_expire_and_errors_expire_sooner() {
        let cache = SummaryCache::default();
        let site = SiteId::new("krill");
        let key = Secret::new("sk-secret-value-1234");
        let t0 = Duration::from_secs(100);
        cache.put(&site, "https://a", &key, t0, Ok(None));
        assert!(
            cache
                .get(&site, "https://a", &key, t0 + Duration::from_secs(59))
                .is_some()
        );
        assert!(
            cache
                .get(&site, "https://a", &key, t0 + SUMMARY_TTL)
                .is_none()
        );
        assert!(cache.get(&site, "https://b", &key, t0).is_none());
        cache.put(&site, "https://a", &key, t0, Err(Error::other("x")));
        assert!(
            cache
                .get(&site, "https://a", &key, t0 + Duration::from_secs(19))
                .is_some()
        );
        assert!(
            cache
                .get(&site, "https://a", &key, t0 + SUMMARY_ERROR_TTL)
                .is_none()
        );

        assert!(!cache.billing_missing(&site, "https://a", t0));
        cache.set_billing_missing(&site, "https://a", t0, true);
        assert!(cache.billing_missing(&site, "https://a", t0 + Duration::from_secs(599)));
        assert!(!cache.billing_missing(&site, "https://a", t0 + MISSING_TTL));
        cache.set_billing_missing(&site, "https://a", t0, false);
        assert!(!cache.billing_missing(&site, "https://a", t0));
    }
}
