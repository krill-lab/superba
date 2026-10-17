//! Mirror list and the race for the newest valid feed index.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use superba_core::{FeedSourceChoice, NetworkSettings};
use tokio::time::Instant;
use url::Url;

use super::FEEDS_SIGNATURE_CONTEXT;
use super::data::MAX_FEED_INDEX_BYTES;
use super::format::{FeedIndex, INDEX_FILE, SIGNATURE_FILE, parse_index};
use crate::error::{CatalogError, Result};
use crate::integrity::{SignatureStatus, TrustedKey, verify_signature_with_context};
use crate::remote::Transport;

/// Environment variable with extra mirror base URLs (comma separated), added to
/// the list for tests and private mirrors; they never bypass verification.
pub const ENV_SUPERBA_FEED_MIRRORS: &str = "SUPERBA_FEED_MIRRORS";

/// Where the feeds are published: a GitHub repository, a ref and a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedLocation {
    /// `owner/repo`.
    pub repo: String,
    /// Branch or tag.
    pub git_ref: String,
    /// Directory inside the repository (empty = the root).
    pub path: String,
}

impl Default for FeedLocation {
    fn default() -> Self {
        Self {
            repo: super::FEEDS_REPO.to_owned(),
            git_ref: super::FEEDS_REF.to_owned(),
            path: super::FEEDS_PATH.to_owned(),
        }
    }
}

/// One place the feed directory can be read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    /// Short name (`raw`, `jsdelivr`, a user mirror's host).
    pub name: String,
    /// Base URL of the feed directory (ends with `/`).
    pub base: Url,
}

impl Mirror {
    /// A mirror at `base` (a `/` is appended when missing). https, or http on a
    /// loopback address; no credentials, query or fragment.
    pub fn new(name: impl Into<String>, base: &str) -> Result<Mirror> {
        let mut s = base.trim().to_owned();
        if !s.ends_with('/') {
            s.push('/');
        }
        let url =
            Url::parse(&s).map_err(|e| CatalogError::Invalid(format!("mirror `{s}`: {e}")))?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d == "localhost",
            None => false,
        };
        let ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
        if !ok
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CatalogError::Invalid(format!(
                "mirror `{s}` must be an https URL without credentials, query or fragment"
            )));
        }
        Ok(Mirror {
            name: name.into(),
            base: url,
        })
    }

    /// URL of a feeds-relative file.
    pub fn url(&self, rel: &str) -> Result<Url> {
        self.base
            .join(rel)
            .map_err(|e| CatalogError::Invalid(format!("mirror URL for {rel}: {e}")))
    }
}

/// The built-in mirrors for `location` (spike A7): GitHub raw, the jsDelivr hosts
/// and their mirrors, raw.githack; the third-party GitHub proxies only when
/// `third_party` is on. `choice` narrows to raw or the jsDelivr family.
pub fn builtin_mirrors(
    location: &FeedLocation,
    choice: FeedSourceChoice,
    third_party: bool,
) -> Vec<Mirror> {
    let FeedLocation {
        repo,
        git_ref,
        path,
    } = location;
    let dir = if path.trim_matches('/').is_empty() {
        String::new()
    } else {
        format!("{}/", path.trim_matches('/'))
    };
    let raw = format!("https://raw.githubusercontent.com/{repo}/{git_ref}/{dir}");
    let jsd = |host: &str| format!("https://{host}/gh/{repo}@{git_ref}/{dir}");
    let mut out: Vec<(String, String)> = Vec::new();
    let want_raw = matches!(choice, FeedSourceChoice::Auto | FeedSourceChoice::GitHubRaw);
    let want_jsd = matches!(choice, FeedSourceChoice::Auto | FeedSourceChoice::JsDelivr);
    if want_raw {
        out.push(("raw".into(), raw.clone()));
    }
    if want_jsd {
        for (name, host) in [
            ("jsdelivr", "cdn.jsdelivr.net"),
            ("jsdelivr-testingcf", "testingcf.jsdelivr.net"),
            ("jsdelivr-quantil", "quantil.jsdelivr.net"),
            ("jsdelivr-gcore", "gcore.jsdelivr.net"),
            ("jsdelivr-fastly", "fastly.jsdelivr.net"),
            ("jsdmirror", "cdn.jsdmirror.com"),
            ("onmicrosoft", "jsd.onmicrosoft.cn"),
        ] {
            out.push((name.into(), jsd(host)));
        }
    }
    if matches!(choice, FeedSourceChoice::Auto) {
        out.push((
            "githack".into(),
            format!("https://raw.githack.com/{repo}/{git_ref}/{dir}"),
        ));
    }
    if third_party && want_raw {
        for (name, prefix) in [
            ("gh-proxy", "https://gh-proxy.com/"),
            ("ghfast", "https://ghfast.top/"),
        ] {
            out.push((name.into(), format!("{prefix}{raw}")));
        }
    }
    out.into_iter()
        .filter_map(|(n, b)| Mirror::new(n, &b).ok())
        .collect()
}

/// The mirrors to race: the built-in ones (only once the public feeds location
/// exists, `published`), the user's extra mirrors and `env` (the value of
/// [`ENV_SUPERBA_FEED_MIRRORS`]). Invalid entries are skipped; duplicates removed.
pub fn mirror_list(
    network: &NetworkSettings,
    location: &FeedLocation,
    published: bool,
    env: Option<&str>,
) -> Vec<Mirror> {
    let mut out = Vec::new();
    if published {
        out.extend(builtin_mirrors(
            location,
            network.feed_source,
            network.third_party_proxies,
        ));
    }
    let user = network.extra_mirrors.iter().map(String::as_str);
    let env = env
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty());
    for base in user.chain(env) {
        if let Ok(m) = Mirror::new(host_name(base), base) {
            out.push(m);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|m| seen.insert(m.base.to_string()));
    out
}

fn host_name(base: &str) -> String {
    Url::parse(base)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| u.port().map_or(h.to_owned(), |p| format!("{h}:{p}")))
        })
        .unwrap_or_else(|| "mirror".into())
}

/// Timing of the race and the downloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaceConfig {
    /// Delay between starting two mirrors (the remembered winner starts at once).
    pub stagger: Duration,
    /// Timeout of one index or signature request.
    pub per_request: Duration,
    /// The whole race.
    pub overall: Duration,
    /// After the first valid index, wait this long for a higher serial (stale CDN
    /// copies).
    pub settle: Duration,
    /// Timeout of one file download.
    pub file_timeout: Duration,
    /// Minimum time between two automatic checks.
    pub min_interval: Duration,
}

impl Default for RaceConfig {
    fn default() -> Self {
        Self {
            stagger: Duration::from_millis(150),
            per_request: Duration::from_secs(3),
            overall: Duration::from_secs(6),
            settle: Duration::from_millis(1500),
            file_timeout: Duration::from_secs(15),
            min_interval: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// A verified index offered by a mirror.
#[derive(Clone, Debug)]
pub(crate) struct Offer {
    pub mirror: Mirror,
    pub index: FeedIndex,
    pub index_bytes: Vec<u8>,
    pub sig_bytes: Vec<u8>,
}

/// Result of a race.
#[derive(Debug, Default)]
pub(crate) struct RaceOutcome {
    /// Valid offers in arrival order.
    pub offers: Vec<Offer>,
    /// Why mirrors failed (name, reason).
    pub failures: Vec<(String, String)>,
}

impl RaceOutcome {
    /// The highest serial (first arrival among equals).
    pub fn best(&self) -> Option<&Offer> {
        let mut best: Option<&Offer> = None;
        for o in &self.offers {
            if best.is_none_or(|b| o.index.serial > b.index.serial) {
                best = Some(o);
            }
        }
        best
    }
}

async fn get_with_timeout(
    transport: &dyn Transport,
    url: &Url,
    max: u64,
    timeout: Duration,
) -> Result<Vec<u8>> {
    match tokio::time::timeout(timeout, transport.get(url, max)).await {
        Ok(r) => r,
        Err(_) => Err(CatalogError::Network {
            url: url.to_string(),
            message: format!("no answer within {} ms", timeout.as_millis()),
        }),
    }
}

/// Fetch and verify one mirror's index and signature.
async fn offer_of(
    transport: Arc<dyn Transport>,
    mirror: Mirror,
    per_request: Duration,
    trusted: &[TrustedKey],
    revoked: &[&str],
) -> std::result::Result<Offer, String> {
    let (iu, su) = (
        mirror.url(INDEX_FILE).map_err(|e| e.to_string())?,
        mirror.url(SIGNATURE_FILE).map_err(|e| e.to_string())?,
    );
    let (index, sig) = tokio::join!(
        get_with_timeout(transport.as_ref(), &iu, MAX_FEED_INDEX_BYTES, per_request),
        get_with_timeout(transport.as_ref(), &su, MAX_FEED_INDEX_BYTES, per_request),
    );
    let index_bytes = index.map_err(|e| e.to_string())?;
    let sig_bytes = sig.map_err(|e| e.to_string())?;
    match verify_signature_with_context(
        FEEDS_SIGNATURE_CONTEXT,
        &index_bytes,
        Some(&sig_bytes),
        trusted,
        revoked,
    ) {
        SignatureStatus::Valid { .. } => {}
        SignatureStatus::Invalid { reason } => return Err(format!("signature: {reason}")),
        SignatureStatus::Unsigned => return Err("unsigned".into()),
    }
    let index = parse_index(&index_bytes).map_err(|e| e.to_string())?;
    Ok(Offer {
        mirror,
        index,
        index_bytes,
        sig_bytes,
    })
}

/// Race `mirrors` (in order; the first starts at once, the rest staggered) for a
/// signed index. Stops at the overall deadline, when every mirror answered, or
/// `settle` after the first valid offer.
pub(crate) async fn race(
    transport: Arc<dyn Transport>,
    mirrors: &[Mirror],
    cfg: RaceConfig,
    trusted: &'static [TrustedKey],
    revoked: &'static [&'static str],
) -> RaceOutcome {
    let mut set = tokio::task::JoinSet::new();
    for (i, m) in mirrors.iter().enumerate() {
        let transport = Arc::clone(&transport);
        let m = m.clone();
        let delay = cfg
            .stagger
            .saturating_mul(u32::try_from(i).unwrap_or(u32::MAX));
        set.spawn(async move {
            tokio::time::sleep(delay).await;
            let name = m.name.clone();
            (
                name,
                offer_of(transport, m, cfg.per_request, trusted, revoked).await,
            )
        });
    }
    let start = Instant::now();
    let overall = start + cfg.overall;
    let mut out = RaceOutcome::default();
    let mut first_valid: Option<Instant> = None;
    loop {
        let until = first_valid.map_or(overall, |t| (t + cfg.settle).min(overall));
        match tokio::time::timeout_at(until, set.join_next()).await {
            Ok(Some(Ok((_, Ok(offer))))) => {
                first_valid.get_or_insert_with(Instant::now);
                out.offers.push(offer);
            }
            Ok(Some(Ok((name, Err(reason))))) => out.failures.push((name, reason)),
            Ok(Some(Err(e))) => out.failures.push(("task".into(), e.to_string())),
            Ok(None) | Err(_) => break,
        }
    }
    set.abort_all();
    out
}

/// Download the files the offer lists that `have` lacks (same size and SHA-256
/// are reused), trying the offering mirror first, then the others; every file is
/// checked against the index.
pub(crate) async fn fetch_files(
    transport: Arc<dyn Transport>,
    offer: &Offer,
    others: &[Mirror],
    have: &BTreeMap<String, Vec<u8>>,
    cfg: RaceConfig,
) -> Result<(BTreeMap<String, Vec<u8>>, usize)> {
    let mut files = BTreeMap::new();
    let mut downloaded = 0;
    let mut order = vec![offer.mirror.clone()];
    order.extend(others.iter().filter(|m| **m != offer.mirror).cloned());
    for f in &offer.index.files {
        crate::integrity::validate_relative_path(&f.path)?;
        if f.size > super::data::MAX_FEED_FILE_BYTES {
            return Err(CatalogError::integrity(&f.path, "file is too large"));
        }
        let matches = |b: &[u8]| {
            b.len() as u64 == f.size
                && crate::integrity::sha256_hex(b).eq_ignore_ascii_case(&f.sha256)
        };
        if let Some(b) = have.get(&f.path).filter(|b| matches(b)) {
            files.insert(f.path.clone(), b.clone());
            continue;
        }
        let mut last = None;
        let mut got = None;
        for m in &order {
            let url = m.url(&f.path)?;
            match get_with_timeout(transport.as_ref(), &url, f.size, cfg.file_timeout).await {
                Ok(b) if matches(&b) => {
                    got = Some(b);
                    break;
                }
                Ok(_) => {
                    last = Some(CatalogError::integrity(
                        &f.path,
                        format!("{} served a file that does not match the index", m.name),
                    ));
                }
                Err(e) => last = Some(e),
            }
        }
        match got {
            Some(b) => {
                downloaded += 1;
                files.insert(f.path.clone(), b);
            }
            None => {
                return Err(last.unwrap_or_else(|| {
                    CatalogError::integrity(&f.path, "no mirror served the file")
                }));
            }
        }
    }
    files.insert(INDEX_FILE.to_owned(), offer.index_bytes.clone());
    files.insert(SIGNATURE_FILE.to_owned(), offer.sig_bytes.clone());
    Ok((files, downloaded))
}
