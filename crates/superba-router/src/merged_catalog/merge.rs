use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use superba_core::{
    CatalogLayer, CatalogProvenance, CatalogReadiness, ModelOverride, RouterCatalogModel,
};

use super::{CatalogConfig, Snapshot, Target, cache, probe_key};
use crate::state::{Generation, lock, unix_ms};
use crate::translate::ir::Protocol;

/// Operating budget of an id nothing is known about, by family. These are
/// defaults to start from, not the provider's maximum: they are never written
/// as `max_context_window`, real metadata always wins and the user can
/// override them. An exact L0/L1 match never gets here.
pub(super) fn family_budget(id: &str) -> u64 {
    // Vendor-qualified ids (`deepseek-ai/DeepSeek-V3`) name the family last.
    let family = id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| family.starts_with(p));
    if starts(&["gpt-", "codex"]) {
        272_000
    } else if starts(&["claude"]) {
        200_000
    } else if starts(&["gemini"]) {
        128_000
    } else if starts(&["deepseek", "qwen", "glm", "kimi", "doubao", "grok"]) {
        64_000
    } else {
        32_000
    }
}

/// A Codex entry for a standard-list id with no known metadata: conservative
/// tool settings and the [`family_budget`] (or the list's own window).
pub(super) fn synthesized(id: &str, metadata: &Value, priority: i64) -> Value {
    let window = ["context_window", "context_length", "max_input_tokens"]
        .into_iter()
        .find_map(|k| {
            metadata[k]
                .as_u64()
                .filter(|w| (16_000..=10_000_000).contains(w))
        })
        .unwrap_or_else(|| family_budget(id));
    json!({
        "slug": id, "display_name": metadata["display_name"].as_str().unwrap_or(id),
        "description": "Standard model list · Superba router",
        "supported_reasoning_levels": [], "shell_type": "unified_exec",
        "visibility": "list", "supported_in_api": true, "priority": priority,
        "support_verbosity": false, "truncation_policy": {"mode":"bytes","limit":10000},
        "experimental_supported_tools": [],
        "base_instructions": "You are a coding assistant. Use the available tools to inspect and edit the workspace. Follow the user's instructions and report what you verified.",
        "apply_patch_tool_type": "freeform", "input_modalities": ["text"],
        "context_window": window, "auto_compact_token_limit": window * 90 / 100,
        "supports_reasoning_summary_parameter": false, "default_reasoning_summary": "none",
        "service_tiers": [], "additional_speed_tiers": [], "node_repl_disabled": true,
        "include_apps_usage_instructions": false, "use_responses_lite": false,
        "supports_search_tool": false, "supports_experimental_context": false,
        "supports_reasoning_effort_updates": false
    })
}

fn builtin(config: &CatalogConfig) -> Vec<Value> {
    if !config.bundled.is_empty() {
        return config
            .bundled
            .iter()
            .filter(|v| cache::valid_codex(v))
            .cloned()
            .collect();
    }
    // The repository feed is the shipped fallback when no installed consumer
    // can be queried yet. L1 replaces these objects, never just selected fields.
    let feed: Value = serde_json::from_str(include_str!("../../../../catalog/feeds/models.json"))
        .expect("bundled feed");
    feed["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let id = v["slug"].as_str().unwrap_or_default();
            let mut entry = synthesized(id, v, v["priority"].as_i64().unwrap_or(50));
            for key in [
                "display_name",
                "description",
                "visibility",
                "supported_in_api",
                "context_window",
                "max_context_window",
                "default_reasoning_level",
                "service_tiers",
                "input_modalities",
            ] {
                if let Some(value) = v.get(key) {
                    entry[key] = value.clone();
                }
            }
            if let Some(levels) = v.get("reasoning_levels") {
                entry["supported_reasoning_levels"] = levels.clone();
            }
            entry
        })
        .collect()
}

fn apply(entry: &mut Value, ov: Option<&ModelOverride>, defaults: &superba_core::ModelDefaults) {
    let empty = ModelOverride::default();
    let ov = ov.unwrap_or(&empty);
    if let Some(window) = ov.context_window.or(defaults.context_window) {
        let maximum = entry["max_context_window"].as_u64().unwrap_or(window);
        entry["context_window"] = json!(window.min(maximum));
    }
    if let Some(compact) = ov.auto_compact.or(defaults.auto_compact) {
        let ceiling = entry["context_window"]
            .as_u64()
            .map(|w| w * 90 / 100)
            .unwrap_or(compact);
        entry["auto_compact_token_limit"] = json!(compact.min(ceiling));
    }
    if let Some(name) = &ov.display_name {
        entry["display_name"] = json!(name);
    }
    if ov.hidden {
        entry["visibility"] = json!("hide");
    }
}

/// A probe result still counts: a pass for a day, a failure for ten minutes.
fn probe_state(g: &Generation, member: usize, id: &str) -> (CatalogReadiness, Option<String>) {
    if cache::responses_protocol(g, member).is_none() {
        return (CatalogReadiness::NeedsTranslation, None);
    }
    let result = {
        let slot = lock(&g.merged_catalog.probes)
            .get(&probe_key(member, id))
            .cloned();
        match slot {
            Some(slot) => slot.try_lock().ok().and_then(|p| p.clone()),
            None => None,
        }
    }
    .filter(|r| {
        unix_ms().saturating_sub(r.tested_at)
            < Duration::from_secs(if r.passed { 86400 } else { 600 }).as_millis() as u64
    });
    match result {
        Some(r) if r.passed => (CatalogReadiness::Ready, None),
        Some(r) => (CatalogReadiness::Failed, Some(r.detail)),
        None => (CatalogReadiness::Pending, None),
    }
}

/// The best readiness of a model's providers.
fn best(states: impl IntoIterator<Item = CatalogReadiness>) -> CatalogReadiness {
    let rank = |r: &CatalogReadiness| match r {
        CatalogReadiness::Ready => 0,
        CatalogReadiness::Pending => 1,
        CatalogReadiness::Failed => 2,
        CatalogReadiness::NeedsTranslation => 3,
    };
    states
        .into_iter()
        .min_by_key(rank)
        .unwrap_or(CatalogReadiness::Pending)
}

/// One provider's offer of one model.
struct Found {
    target: Target,
    /// The provider's metadata, with its own per-model settings applied.
    entry: Value,
    proof: CatalogProvenance,
    readiness: CatalogReadiness,
    detail: Option<String>,
    /// The provider's display name (alias display names).
    name: String,
}

/// The canonical settings: overrides and picker visibility of the model id.
fn canonical(settings: &superba_core::RouterCatalogSettings, id: &str, entry: &mut Value) {
    apply(
        entry,
        settings.overrides.get(id),
        &superba_core::ModelDefaults::default(),
    );
    if let Some(visible) = settings.visibility.get(id) {
        entry["visibility"] = json!(if *visible { "list" } else { "hide" });
    }
}

fn by_priority(a: &Value, b: &Value) -> std::cmp::Ordering {
    a["priority"]
        .as_i64()
        .cmp(&b["priority"].as_i64())
        .then_with(|| a["slug"].as_str().cmp(&b["slug"].as_str()))
}

/// Merge the layers into one row per model id (the management view) and the
/// mode's published list. Metadata: a provider's own Codex catalog entry (L1),
/// else an exact L0/L1 match ([`CatalogLayer::StandardKnown`]), else family
/// defaults ([`CatalogLayer::Synthesized`]). Routing: a bare id goes to every
/// ready provider in member order (failover); `slug/id` pins one provider.
pub(super) async fn build(g: &Arc<Generation>, query: &str) -> Snapshot {
    let Some(config) = &g.config.catalog else {
        return Snapshot::default();
    };
    let settings = &config.settings;
    let slugs = config.slugs(&g.config);
    let serving: Vec<_> = g
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| g.config.serves_codex(&m.cfg.id))
        .map(|(i, _)| i)
        .collect();
    let mut fetched: Vec<_> = stream::iter(serving.iter().copied().map(|member| async move {
        let (codex, standard) = tokio::join!(
            cache::fetch(g, member, false, query),
            cache::fetch(g, member, true, query)
        );
        // A Codex-shaped list cannot establish native Responses support.
        let mut codex = codex;
        if !cache::native_responses(g, member) {
            codex.entries.clear();
        }
        (member, codex, standard)
    }))
    .buffer_unordered(6)
    .collect()
    .await;
    fetched.sort_by_key(|(i, _, _)| *i);
    let bundled = builtin(config);
    // The installed Codex's own identities: the whole list in Codex mode.
    let official: BTreeSet<String> = bundled
        .iter()
        .filter_map(|v| v["slug"].as_str().map(str::to_owned))
        .collect();
    // Complete metadata by exact id: L0, replaced whole by L1 (the first
    // member's catalog wins), unknown fields included.
    let mut known: BTreeMap<String, Value> = bundled
        .iter()
        .filter_map(|v| Some((v["slug"].as_str()?.to_owned(), v.clone())))
        .collect();
    for (_, codex, _) in fetched.iter().rev() {
        for entry in &codex.entries {
            let id = entry["slug"].as_str().expect("validated id");
            known.insert(id.to_owned(), entry.clone());
        }
    }
    let priority = known
        .values()
        .filter_map(|v| v["priority"].as_i64())
        .max()
        .unwrap_or(100)
        .saturating_add(100);
    // A generated alias never shadows a real upstream id with a slash.
    let discovered: BTreeSet<String> = known
        .keys()
        .cloned()
        .chain(
            fetched
                .iter()
                .flat_map(|(_, _, standard)| &standard.entries)
                .filter_map(|v| v["id"].as_str().map(str::to_owned)),
        )
        .collect();

    let mut found: BTreeMap<String, Vec<Found>> = BTreeMap::new();
    let mut statuses = Vec::new();
    for (member, codex, standard) in fetched {
        let m = &g.members[member];
        let slug = &slugs[&m.cfg.id];
        let choices = config.members.get(&m.cfg.id).cloned().unwrap_or_default();
        let name = config.member_name(&m.cfg).to_owned();
        let native = cache::native_responses(g, member);
        let scope = hex::encode(Sha256::digest(m.auth.as_bytes()))[..16].to_owned();
        let protocol = match cache::responses_protocol(g, member) {
            Some(Protocol::Responses) => "responses",
            Some(Protocol::Chat) => "chat_completions",
            Some(Protocol::Messages) => "anthropic_messages",
            Some(Protocol::Gemini) => "gemini",
            None => "unavailable",
        };
        let source = |id: &str,
                      layer,
                      status: &superba_core::CatalogSourceStatus,
                      evidence: &str| CatalogProvenance {
            provider: m.cfg.id.clone(),
            provider_name: name.clone(),
            provider_slug: slug.clone(),
            key_scope: scope.clone(),
            upstream_id: id.into(),
            endpoint: status.endpoint.clone(),
            protocol: protocol.into(),
            layer,
            evidence: evidence.into(),
            observed_at: status.observed_at.unwrap_or_default(),
            readiness: None,
            alias: None,
        };
        let mut local: BTreeMap<String, Found> = BTreeMap::new();
        let mut add =
            |id: &str,
             entry: Value,
             proof: CatalogProvenance,
             standard: bool,
             (readiness, detail): (CatalogReadiness, Option<String>)| {
                local.entry(id.to_owned()).or_insert(Found {
                    target: Target {
                        member,
                        upstream: id.to_owned(),
                        standard,
                    },
                    entry,
                    proof,
                    readiness,
                    detail,
                    name: name.clone(),
                });
            };
        // L1: the provider's own complete catalog.
        for entry in &codex.entries {
            let id = entry["slug"].as_str().expect("validated id");
            let proof = source(
                id,
                CatalogLayer::Codex,
                &codex.status,
                "upstream Codex catalog",
            );
            add(
                id,
                entry.clone(),
                proof,
                false,
                (CatalogReadiness::Ready, None),
            );
        }
        // L2: the standard list. The installed Codex's own models stay
        // available on a native endpoint (the a8catalog fallback); anything
        // else needs this provider's own tool probe, whatever metadata it
        // borrows from another layer.
        for metadata in &standard.entries {
            let id = metadata["id"].as_str().expect("validated id");
            let (entry, layer, evidence) = match known.get(id) {
                Some(entry) => (
                    entry.clone(),
                    CatalogLayer::StandardKnown,
                    "standard list; complete metadata of the known model",
                ),
                None => (
                    synthesized(id, metadata, priority),
                    CatalogLayer::Synthesized,
                    "standard list; family defaults until the tool probe passes",
                ),
            };
            let proof = source(id, layer, &standard.status, evidence);
            if native && official.contains(id) {
                add(id, entry, proof, false, (CatalogReadiness::Ready, None));
            } else {
                add(id, entry, proof, true, probe_state(g, member, id));
            }
        }
        // L0 fallback on a native endpoint when a list is unavailable or
        // incomplete; it keeps the historical routing, capability unknown.
        if native {
            for entry in &bundled {
                let id = entry["slug"].as_str().expect("bundled id");
                let proof = source(
                    id,
                    CatalogLayer::Builtin,
                    &codex.status,
                    "bundled fallback; upstream capability unknown",
                );
                add(
                    id,
                    known[id].clone(),
                    proof,
                    false,
                    (CatalogReadiness::Ready, None),
                );
            }
        }
        for (id, mut f) in local {
            apply(&mut f.entry, choices.overrides.get(&id), &choices.defaults);
            let alias = format!("{slug}/{id}");
            f.proof.alias = (!discovered.contains(&alias)).then_some(alias);
            f.proof.readiness = Some(f.readiness);
            found.entry(id).or_default().push(f);
        }
        statuses.push(codex.status);
        statuses.push(standard.status);
    }

    let mode = settings.mode;
    let mut snap = Snapshot::default();
    let mut aliases = Vec::new();
    for (id, providers) in found {
        let ready: Vec<&Found> = providers
            .iter()
            .filter(|f| f.readiness == CatalogReadiness::Ready)
            .collect();
        let readiness = best(providers.iter().map(|f| f.readiness));
        // The canonical entry: the first ready provider's (member order),
        // then the model's own settings on top.
        let base = ready.first().copied().unwrap_or(&providers[0]);
        let mut entry = base.entry.clone();
        canonical(settings, &id, &mut entry);
        let listed = mode.extends() || official.contains(&id);
        if !ready.is_empty() {
            snap.routes
                .insert(id.clone(), ready.iter().map(|f| f.target.clone()).collect());
            if listed {
                snap.published.push(entry.clone());
            }
        }
        snap.targets.insert(
            id.clone(),
            providers.iter().map(|f| f.target.clone()).collect(),
        );
        for f in &providers {
            let Some(alias) = f.proof.alias.clone() else {
                continue;
            };
            let mut pinned = f.entry.clone();
            canonical(settings, &id, &mut pinned);
            pinned["slug"] = json!(alias);
            pinned["display_name"] = json!(format!(
                "{} · {}",
                pinned["display_name"].as_str().unwrap_or(&id),
                f.name
            ));
            if f.readiness == CatalogReadiness::Ready {
                snap.routes.insert(alias.clone(), vec![f.target.clone()]);
                if listed && mode.prefixes() {
                    snap.view.aliases.insert(alias.clone(), id.clone());
                    aliases.push(pinned.clone());
                }
            }
            snap.targets.insert(alias.clone(), vec![f.target.clone()]);
            snap.all.insert(
                alias,
                RouterCatalogModel {
                    entry: pinned,
                    sources: vec![f.proof.clone()],
                    readiness: f.readiness,
                    detail: f.detail.clone(),
                },
            );
        }
        let row = RouterCatalogModel {
            entry,
            readiness,
            detail: (readiness != CatalogReadiness::Ready)
                .then(|| providers.iter().find_map(|f| f.detail.clone()))
                .flatten(),
            sources: providers.into_iter().map(|f| f.proof).collect(),
        };
        if listed {
            snap.view.models.push(row.clone());
        }
        snap.all.insert(id, row);
    }
    snap.view
        .models
        .sort_by(|a, b| by_priority(&a.entry, &b.entry));
    snap.published.sort_by(by_priority);
    // Each alias follows its canonical model; Codex sorts by priority, and an
    // alias shares its model's.
    aliases.sort_by(by_priority);
    snap.published.extend(aliases);
    let mut view_settings = settings.clone();
    view_settings.provider_slugs = slugs;
    snap.view.settings = view_settings;
    snap.view.sources = statuses;
    snap
}
