//! Layered Codex catalogs and credential-scoped capability evidence.
//!
//! Discovery never runs inference. Standard-list models remain pending until
//! the Models page's test completes a two-turn function call. A generation owns
//! all caches, so a changed key, endpoint or member cannot inherit old evidence.

mod cache;
mod merge;
mod probe;
mod route;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use superba_core::{
    Error, ModelDefaults, ModelOverride, Result, RouterCatalogSettings, RouterCatalogView,
    RouterModelTest,
};

use crate::config::RouterConfig;
use crate::state::{Generation, lock, unix_ms};

pub(crate) use probe::test;
pub(crate) use route::{Selection, select, targets};

/// Per-provider catalog choices (credentials are stored separately).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemberCatalog {
    /// Profile or site display name, used for prefixed entries.
    pub name: String,
    /// Per-model overrides; prefixed slugs may override bare-slug choices.
    pub overrides: BTreeMap<String, ModelOverride>,
    /// Global values folded into models without their own overrides.
    pub defaults: ModelDefaults,
}

/// Inputs supplied by the switch service to the merged catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogConfig {
    /// User namespace choices.
    pub settings: RouterCatalogSettings,
    /// The installed consumer's bundled ModelInfo entries (L0).
    pub bundled: Vec<Value>,
    /// Provider overrides, keyed by member id.
    pub members: BTreeMap<String, MemberCatalog>,
}

/// Clean a name into an ASCII provider namespace: lower-case letters, digits
/// and single hyphens, at most 48 characters. Empty when nothing is left (a
/// name in another script); [`CatalogConfig::slugs`] then falls back to the
/// domain and to a number, never to a shared placeholder.
pub fn provider_slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Second-level labels under a country code that are part of the public
/// suffix (`co.uk`, `com.cn`). A short list, not the Public Suffix List: the
/// slug can always be edited.
const SECOND_LEVEL: [&str; 10] = [
    "co", "com", "net", "org", "ac", "gov", "edu", "ne", "or", "go",
];

/// The provider part of a URL's domain: `api.foo.com` -> `foo`,
/// `foo.com.cn` -> `foo`, `api.foo.co.uk` -> `foo`. IP addresses, `localhost`,
/// single-label hosts and punycode labels have none.
pub fn domain_slug(url: &url::Url) -> Option<String> {
    let url::Host::Domain(host) = url.host()? else {
        return None;
    };
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    let n = labels.len();
    let suffix = if n >= 3 && labels[n - 1].len() == 2 && SECOND_LEVEL.contains(&labels[n - 2]) {
        2
    } else {
        1
    };
    let label = labels.get(n.checked_sub(suffix + 1)?)?;
    if label.starts_with("xn--") {
        return None;
    }
    Some(provider_slug(label)).filter(|s| !s.is_empty())
}

/// Names Krill reserves for its own members.
fn reserved(slug: &str) -> bool {
    matches!(slug, "krill" | "krill-usage" | "krill-balance")
}

/// The placeholder alpha.7 generated for names without ASCII letters
/// (`provider`, `provider-2`). A saved copy of it is dropped, not kept: the
/// user asked for no compatibility path for these aliases.
fn legacy_placeholder(slug: &str) -> bool {
    slug == "provider"
        || slug
            .strip_prefix("provider-")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

impl CatalogConfig {
    /// Check explicitly selected names and model override bounds.
    pub fn validate(&self, config: &RouterConfig) -> Result<()> {
        for value in self.settings.overrides.values() {
            superba_core::check_model_override(None, value, &ModelDefaults::default())
                .map_err(|e| Error::invalid(e.message()))?;
        }
        let mut names = BTreeSet::new();
        for (id, slug) in &self.settings.provider_slugs {
            if config.member(id).is_none() {
                continue;
            }
            if slug.is_empty() || provider_slug(slug) != *slug || !names.insert(slug) {
                return Err(Error::invalid(
                    "provider slugs must be unique lowercase letters, digits and hyphens",
                ));
            }
            if reserved(slug)
                && config
                    .member(id)
                    .is_some_and(|m| m.kind != crate::MemberKind::Krill)
            {
                return Err(Error::invalid(
                    "Krill namespaces are reserved for Krill providers",
                ));
            }
        }
        for member in self.members.values() {
            superba_core::check_all_models(&member.defaults)
                .map_err(|e| Error::invalid(e.message()))?;
            for value in member.overrides.values() {
                superba_core::check_model_override(None, value, &member.defaults)
                    .map_err(|e| Error::invalid(e.message()))?;
            }
        }
        Ok(())
    }

    /// The display name of a member: the profile's name, else the member's.
    pub fn member_name<'a>(&'a self, member: &'a crate::MemberConfig) -> &'a str {
        self.members
            .get(&member.id)
            .map(|p| p.name.as_str())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&member.name)
    }

    /// Stable unique slugs by member id. In order: the user's saved slug, an
    /// all-ASCII name, the main part of the domain, `p<n>` (the member's
    /// position among the members sorted by id). Priority reordering never
    /// renames a provider; colliding names get `-2`, `-3`.
    pub fn slugs(&self, config: &RouterConfig) -> BTreeMap<String, String> {
        let mut members: Vec<_> = config.members.iter().collect();
        members.sort_by(|a, b| a.id.cmp(&b.id));
        let mut out = BTreeMap::new();
        for m in &members {
            let Some(slug) = self.settings.provider_slugs.get(&m.id) else {
                continue;
            };
            let name = self.member_name(m);
            if legacy_placeholder(slug) && provider_slug(name).is_empty() {
                continue;
            }
            if !slug.is_empty() && provider_slug(slug) == *slug {
                out.insert(m.id.clone(), slug.clone());
            }
        }
        let mut used: BTreeSet<String> = out.values().cloned().collect();
        for (index, m) in members.iter().enumerate() {
            if out.contains_key(&m.id) {
                continue;
            }
            let name = self.member_name(m).trim();
            let mut base = if name.is_ascii() {
                provider_slug(name)
            } else {
                String::new()
            };
            if base.is_empty() {
                base = m
                    .lines
                    .iter()
                    .find_map(|l| domain_slug(&l.base_url))
                    .unwrap_or_else(|| format!("p{}", index + 1));
            }
            if m.kind != crate::MemberKind::Krill && reserved(&base) {
                base.push_str("-provider");
            }
            let mut slug = base.clone();
            let mut suffix = 2;
            while used.contains(&slug) {
                let tail = format!("-{suffix}");
                let prefix = &base[..base.len().min(48 - tail.len())];
                slug = format!("{}{tail}", prefix.trim_end_matches('-'));
                suffix += 1;
            }
            used.insert(slug.clone());
            out.insert(m.id.clone(), slug);
        }
        out
    }
}

/// One provider that can serve a model.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub member: usize,
    /// The id sent upstream (exact spelling).
    pub upstream: String,
    /// Reached through the standard endpoint and checked by a tool probe; the
    /// router may run compaction for it (catB). A Codex-catalog entry and the
    /// installed Codex's own models on a native endpoint are not.
    pub standard: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Snapshot {
    /// The management view: one row per canonical model.
    pub view: RouterCatalogView,
    /// Ready targets by requested name (canonical ids and provider aliases),
    /// in member order. Independent of the mode: a name a session already
    /// uses keeps routing after the mode changes.
    pub routes: BTreeMap<String, Vec<Target>>,
    /// Every target, ready or not, by requested name (the Models page test).
    pub targets: BTreeMap<String, Vec<Target>>,
    /// Rows by requested name, aliases included (tests and unknown-name checks).
    pub all: BTreeMap<String, superba_core::RouterCatalogModel>,
    /// What `/models` returns: the mode's ready canonical entries, then the
    /// pinned aliases in prefixed mode.
    pub published: Vec<Value>,
}

/// Per-generation caches. Each upstream slot and probe is single-flight.
#[derive(Default)]
pub(crate) struct Catalog {
    sources: Mutex<HashMap<String, Arc<tokio::sync::Mutex<cache::Entry>>>>,
    probes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<RouterModelTest>>>>>,
    latest: Mutex<Option<Snapshot>>,
    /// ETag of the list `/models` serves now (see [`models_etag`]).
    etag: Mutex<Option<String>>,
    /// Where passed tool probes are kept across router restarts.
    evidence: Mutex<Option<std::path::PathBuf>>,
}

/// The body `/models` returns for a snapshot.
fn published_bytes(snap: &Snapshot) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"models":snap.published})).expect("catalog JSON")
}

fn etag_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("\"{}\"", hex::encode(Sha256::digest(bytes)))
}

pub(crate) async fn snapshot(g: &Arc<Generation>, query: &str) -> Snapshot {
    let result = merge::build(g, query).await;
    *lock(&g.merged_catalog.etag) = Some(etag_of(&published_bytes(&result)));
    *lock(&g.merged_catalog.latest) = Some(result.clone());
    result
}

/// What a Responses answer to Codex carries as `X-Models-Etag` (user bug
/// 106). Codex keeps the `ETag` of the list it fetched and refreshes the list
/// at once when a response names another one, instead of after its cache
/// lifetime (300 s) or at its next start. A generation that has not built its
/// list yet (a mode change, a reload) answers with a tag of its own, which
/// differs from anything served before and so makes Codex fetch it.
pub(crate) fn models_etag(g: &Generation) -> String {
    lock(&g.merged_catalog.etag)
        .clone()
        .unwrap_or_else(|| format!("\"superba-{:p}\"", g as *const Generation))
}

pub(crate) async fn report(g: &Arc<Generation>) -> RouterCatalogView {
    snapshot(g, "client_version=0.163.0").await.view
}

pub(crate) async fn response(
    g: &Arc<Generation>,
    query: &str,
) -> http::Response<crate::state::RespBody> {
    let snap = snapshot(g, query).await;
    let bytes = published_bytes(&snap);
    let etag = etag_of(&bytes);
    let mut out = http::Response::new(crate::forward::full(bytes::Bytes::from(bytes)));
    out.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    out.headers_mut()
        .insert(http::header::ETAG, etag.parse().expect("digest header"));
    out.headers_mut().insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("private, max-age=60"),
    );
    out
}

/// How long a passed tool probe keeps a model published (user bug 106: a
/// day-long window made a tested model leave Codex's list the next day).
pub(crate) const PASS_VALID_MS: u64 = 30 * 86_400_000;

/// The file of passed probes in the router's data dir.
const EVIDENCE_FILE: &str = "probe-evidence.json";

/// One passed probe on disk: no secret, only a digest of what it depends on.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Evidence {
    member: String,
    /// Digest of the member's key, kind, model map, lines, protocol routes.
    scope: String,
    model: String,
    tested_at: u64,
    detail: String,
}

/// A digest of everything a member's probe result depends on (the same
/// fields [`inherit_probes`] compares). A changed key, endpoint, mapping or
/// protocol route gives another digest, so old evidence never applies.
fn evidence_scope(g: &Generation, member: usize) -> String {
    use sha2::{Digest, Sha256};
    let m = &g.members[member];
    let mut h = Sha256::new();
    h.update(m.auth.as_bytes());
    h.update(
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            m.cfg.kind,
            m.cfg.model_map,
            m.cfg.lines,
            g.openai.members.get(member).map(|o| &o.cfg),
            g.config.translation,
            g.config
                .claude
                .as_ref()
                .and_then(|c| c.members.get(&m.cfg.id)),
            g.config.gemini_member(&m.cfg.id),
        )
        .as_bytes(),
    );
    hex::encode(h.finalize())
}

fn read_evidence(path: &std::path::Path) -> Vec<Evidence> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Load the passed probes kept in `dir` into a new generation (start-up and
/// every reload), so a tested model stays published after the router (or the
/// computer) restarts.
pub(crate) fn attach_evidence(g: &Generation, dir: Option<&std::path::Path>) {
    let Some(dir) = dir else {
        return;
    };
    let path = dir.join(EVIDENCE_FILE);
    let now = unix_ms();
    let all = read_evidence(&path);
    // Evidence of a member whose key, lines, mapping or protocol routes
    // changed is gone for good: going back to the old settings later must not
    // bring an old pass back without a new test.
    let scopes: HashMap<&str, String> = g
        .members
        .iter()
        .enumerate()
        .map(|(i, m)| (m.cfg.id.as_str(), evidence_scope(g, i)))
        .collect();
    let kept: Vec<Evidence> = all
        .iter()
        .filter(|e| scopes.get(e.member.as_str()).is_none_or(|s| *s == e.scope))
        .filter(|e| now.saturating_sub(e.tested_at) < PASS_VALID_MS)
        .cloned()
        .collect();
    if kept.len() != all.len() {
        write_evidence(&path, &kept);
    }
    {
        let mut probes = lock(&g.merged_catalog.probes);
        for (i, m) in g.members.iter().enumerate() {
            let scope = evidence_scope(g, i);
            for e in kept.iter().filter(|e| {
                e.member == m.cfg.id
                    && e.scope == scope
                    && now.saturating_sub(e.tested_at) < PASS_VALID_MS
            }) {
                probes.entry(probe_key(i, &e.model)).or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(Some(RouterModelTest {
                        model: e.model.clone(),
                        passed: true,
                        detail: e.detail.clone(),
                        tested_at: e.tested_at,
                    })))
                });
            }
        }
    }
    *lock(&g.merged_catalog.evidence) = Some(path);
}

/// Record a probe result on disk: a pass is kept, a failure removes the
/// model's earlier pass. Best effort: the in-memory result already counts.
pub(crate) fn record_evidence(g: &Generation, member: usize, result: &RouterModelTest) {
    let Some(path) = lock(&g.merged_catalog.evidence).clone() else {
        return;
    };
    let Some(m) = g.members.get(member) else {
        return;
    };
    let scope = evidence_scope(g, member);
    let now = unix_ms();
    let mut kept: Vec<Evidence> = read_evidence(&path)
        .into_iter()
        .filter(|e| now.saturating_sub(e.tested_at) < PASS_VALID_MS)
        .filter(|e| !(e.member == m.cfg.id && e.model == result.model))
        .collect();
    if result.passed {
        kept.push(Evidence {
            member: m.cfg.id.clone(),
            scope,
            model: result.model.clone(),
            tested_at: result.tested_at,
            detail: result.detail.clone(),
        });
    }
    write_evidence(&path, &kept);
}

fn write_evidence(path: &std::path::Path, kept: &[Evidence]) {
    if let Ok(bytes) = serde_json::to_vec_pretty(kept) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn probe_key(member: usize, model: &str) -> String {
    format!("{member}:{model}")
}

/// UI-only edits must not spend tokens rechecking unchanged credentials.
/// Keys, model mapping, protocol or endpoint changes invalidate the evidence.
pub(crate) fn inherit_probes(old: &Generation, new: &Generation) {
    let previous = lock(&old.merged_catalog.probes);
    let mut next = lock(&new.merged_catalog.probes);
    for (i, member) in new.members.iter().enumerate() {
        let Some((j, before)) = old
            .members
            .iter()
            .enumerate()
            .find(|(_, m)| m.cfg.id == member.cfg.id)
        else {
            continue;
        };
        if before.auth != member.auth
            || before.cfg.kind != member.cfg.kind
            || before.cfg.model_map != member.cfg.model_map
            || before.cfg.lines != member.cfg.lines
            || old.openai.members[j].cfg != new.openai.members[i].cfg
            || old.config.translation != new.config.translation
            || old
                .config
                .claude
                .as_ref()
                .and_then(|c| c.members.get(&member.cfg.id))
                != new
                    .config
                    .claude
                    .as_ref()
                    .and_then(|c| c.members.get(&member.cfg.id))
            || old.config.gemini_member(&member.cfg.id) != new.config.gemini_member(&member.cfg.id)
        {
            continue;
        }
        let prefix = format!("{j}:");
        for (key, result) in previous.iter() {
            if let Some(model) = key.strip_prefix(&prefix) {
                next.insert(probe_key(i, model), result.clone());
            }
        }
    }
}
