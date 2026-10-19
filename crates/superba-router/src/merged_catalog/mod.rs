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
use crate::state::{Generation, lock};

pub(crate) use probe::test;
pub(crate) use route::{Selection, select};

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

/// Clean a profile or site name into an ASCII provider namespace.
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
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "provider".into()
    } else {
        out.into()
    }
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
            if matches!(slug.as_str(), "krill" | "krill-usage" | "krill-balance")
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

    /// Stable unique slugs; priority reordering does not rename providers.
    pub fn slugs(&self, config: &RouterConfig) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = self
            .settings
            .provider_slugs
            .iter()
            .filter(|(id, _)| config.member(id).is_some())
            .map(|(id, name)| (id.clone(), name.clone()))
            .collect();
        let mut used: BTreeSet<String> = out.values().cloned().collect();
        let mut members: Vec<_> = config.members.iter().collect();
        members.sort_by(|a, b| a.id.cmp(&b.id));
        for m in members {
            if out.contains_key(&m.id) {
                continue;
            }
            let name = self
                .members
                .get(&m.id)
                .map(|p| p.name.as_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(&m.name);
            let mut base = provider_slug(name);
            if m.kind != crate::MemberKind::Krill
                && matches!(base.as_str(), "krill" | "krill-usage" | "krill-balance")
            {
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

#[derive(Clone)]
pub(crate) struct Target {
    pub member: usize,
    pub upstream: String,
    pub standard: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Snapshot {
    pub view: RouterCatalogView,
    pub routes: BTreeMap<String, Vec<Target>>,
}

/// Per-generation caches. Each upstream slot and probe is single-flight.
#[derive(Default)]
pub(crate) struct Catalog {
    sources: Mutex<HashMap<String, Arc<tokio::sync::Mutex<cache::Entry>>>>,
    probes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<RouterModelTest>>>>>,
    latest: Mutex<Option<Snapshot>>,
}

pub(crate) async fn snapshot(g: &Arc<Generation>, query: &str) -> Snapshot {
    let result = merge::build(g, query).await;
    *lock(&g.merged_catalog.latest) = Some(result.clone());
    result
}

pub(crate) async fn report(g: &Arc<Generation>) -> RouterCatalogView {
    snapshot(g, "client_version=0.163.0").await.view
}

pub(crate) async fn response(
    g: &Arc<Generation>,
    query: &str,
) -> http::Response<crate::state::RespBody> {
    use sha2::{Digest, Sha256};
    use superba_core::CatalogReadiness;
    let snap = snapshot(g, query).await;
    let entries: Vec<_> = snap
        .view
        .models
        .iter()
        .filter(|m| m.readiness == CatalogReadiness::Ready)
        .map(|m| &m.entry)
        .collect();
    let bytes = serde_json::to_vec(&serde_json::json!({"models":entries})).expect("catalog JSON");
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(&bytes)));
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
