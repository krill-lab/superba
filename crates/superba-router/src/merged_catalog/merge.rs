use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use superba_core::{
    CatalogLayer, CatalogProvenance, CatalogReadiness, ModelOverride, RouterCatalogModel,
    RouterCatalogView,
};

use super::{CatalogConfig, Snapshot, Target, cache, probe_key};
use crate::state::{Generation, lock, unix_ms};

/// Conservative tool/metadata defaults. Context sizes are operating budgets,
/// not claims of the provider's maximum; upstream metadata and L4 take priority.
pub(super) fn synthesized(id: &str, metadata: &Value, priority: i64) -> Value {
    let family = id.to_ascii_lowercase();
    let budget = if family.starts_with("deepseek") || family.starts_with("qwen") {
        64_000
    } else {
        32_000
    };
    let window = ["context_window", "context_length", "max_input_tokens"]
        .into_iter()
        .find_map(|k| metadata[k].as_u64())
        .filter(|w| (16_000..=10_000_000).contains(w))
        .unwrap_or(budget);
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

pub(super) async fn build(g: &Arc<Generation>, query: &str) -> Snapshot {
    let Some(config) = &g.config.catalog else {
        return Snapshot::default();
    };
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
        (member, codex, standard)
    }))
    .buffer_unordered(6)
    .collect()
    .await;
    fetched.sort_by_key(|(i, _, _)| *i);
    let bundled = builtin(config);
    let known: BTreeSet<String> = bundled
        .iter()
        .filter_map(|v| v["slug"].as_str().map(str::to_owned))
        .collect();
    let mut rows: BTreeMap<String, RouterCatalogModel> = bundled
        .iter()
        .filter_map(|entry| {
            let id = entry["slug"].as_str()?.to_owned();
            Some((
                id.clone(),
                RouterCatalogModel {
                    entry: entry.clone(),
                    readiness: CatalogReadiness::Ready,
                    detail: None,
                    sources: vec![CatalogProvenance {
                        provider: "builtin".into(),
                        provider_name: "Codex".into(),
                        provider_slug: String::new(),
                        key_scope: "bundled".into(),
                        upstream_id: id,
                        endpoint: "bundled".into(),
                        protocol: "responses".into(),
                        layer: CatalogLayer::Builtin,
                        evidence: "bundled catalog; upstream access not inferred".into(),
                        observed_at: 0,
                    }],
                },
            ))
        })
        .collect();
    for (_, codex, _) in &fetched {
        for entry in &codex.entries {
            let id = entry["slug"].as_str().expect("validated id").to_owned();
            rows.entry(id).or_insert_with(|| RouterCatalogModel {
                entry: entry.clone(),
                sources: Vec::new(),
                readiness: CatalogReadiness::Ready,
                detail: None,
            });
        }
    }
    let mut authoritative = BTreeSet::new();
    let mut routes: BTreeMap<String, Vec<Target>> = BTreeMap::new();
    let mut prefixed = Vec::new();
    let mut statuses = Vec::new();
    let mut max_priority = bundled
        .iter()
        .filter_map(|v| v["priority"].as_i64())
        .max()
        .unwrap_or(100);
    for (_, codex, _) in &fetched {
        max_priority = max_priority.max(
            codex
                .entries
                .iter()
                .filter_map(|v| v["priority"].as_i64())
                .max()
                .unwrap_or(0),
        );
    }

    for (member, codex, standard) in fetched {
        let m = &g.members[member];
        let slug = &slugs[&m.cfg.id];
        let choices = config.members.get(&m.cfg.id).cloned().unwrap_or_default();
        let name = if choices.name.is_empty() {
            &m.cfg.name
        } else {
            &choices.name
        };
        let scope = hex::encode(Sha256::digest(m.auth.as_bytes()))[..16].to_owned();
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
            protocol: if layer == CatalogLayer::Codex || cache::native_responses(g, member) {
                "responses"
            } else if g.openai.members[member].chat.is_some() {
                "chat_completions"
            } else if g.claude.as_ref().and_then(|c| c.member(member)).is_some() {
                "anthropic_messages"
            } else {
                "unknown"
            }
            .into(),
            layer,
            evidence: evidence.into(),
            observed_at: status.observed_at.unwrap_or_default(),
        };
        let mut local: BTreeMap<String, RouterCatalogModel> = BTreeMap::new();
        for entry in &codex.entries {
            let id = entry["slug"].as_str().expect("validated id").to_owned();
            let proof = source(
                &id,
                CatalogLayer::Codex,
                &codex.status,
                "upstream Codex catalog",
            );
            let mut entry = entry.clone();
            apply(&mut entry, choices.overrides.get(&id), &choices.defaults);
            let row = RouterCatalogModel {
                entry,
                sources: vec![proof],
                readiness: CatalogReadiness::Ready,
                detail: None,
            };
            if authoritative.insert(id.clone()) {
                rows.insert(id.clone(), row.clone());
            } else if let Some(bare) = rows.get_mut(&id) {
                bare.sources.extend(row.sources.clone());
            }
            local.insert(id.clone(), row);
            routes.entry(id.clone()).or_default().push(Target {
                member,
                upstream: id,
                standard: false,
            });
        }
        for metadata in &standard.entries {
            let id = metadata["id"].as_str().expect("validated id").to_owned();
            let proof = source(
                &id,
                CatalogLayer::Standard,
                &standard.status,
                "credential-scoped standard list",
            );
            if let Some(row) = local.get_mut(&id) {
                row.sources.push(proof.clone());
                if let Some(bare) = rows.get_mut(&id) {
                    bare.sources.push(proof);
                }
                continue;
            }
            if known.contains(&id) {
                // L0 models keep their native fallback. Another member's L1
                // metadata does not prove this member has a native endpoint;
                // its standard-only target still needs its own L2 probe.
                if let Some(row) = rows.get(&id) {
                    let mut row = row.clone();
                    row.sources = vec![proof];
                    apply(
                        &mut row.entry,
                        choices.overrides.get(&id),
                        &choices.defaults,
                    );
                    local.insert(id.clone(), row);
                    routes.entry(id.clone()).or_default().push(Target {
                        member,
                        upstream: id,
                        standard: false,
                    });
                }
                continue;
            }
            let result = {
                let slot = lock(&g.merged_catalog.probes)
                    .get(&probe_key(member, &id))
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
            let readiness = if !cache::native_responses(g, member) {
                CatalogReadiness::NeedsTranslation
            } else {
                match &result {
                    Some(r) if r.passed => CatalogReadiness::Ready,
                    Some(_) => CatalogReadiness::Failed,
                    None => CatalogReadiness::Pending,
                }
            };
            let mut synthesized_proof = source(
                &id,
                CatalogLayer::Synthesized,
                &standard.status,
                if readiness == CatalogReadiness::Ready {
                    "two-turn function call passed"
                } else {
                    "conservative family defaults; tool probe required"
                },
            );
            if let Some(r) = &result {
                synthesized_proof.observed_at = r.tested_at;
            }
            let mut entry = synthesized(&id, metadata, max_priority.saturating_add(100));
            apply(&mut entry, choices.overrides.get(&id), &choices.defaults);
            let row = RouterCatalogModel {
                entry,
                sources: vec![proof, synthesized_proof],
                readiness,
                detail: result.map(|r| r.detail),
            };
            match rows.get_mut(&id) {
                Some(bare) => {
                    if bare.readiness != CatalogReadiness::Ready
                        && row.readiness == CatalogReadiness::Ready
                    {
                        bare.entry = row.entry.clone();
                        bare.readiness = row.readiness;
                        bare.detail = row.detail.clone();
                    }
                    bare.sources.extend(row.sources.clone());
                }
                None => {
                    rows.insert(id.clone(), row.clone());
                }
            }
            local.insert(id.clone(), row);
            if readiness == CatalogReadiness::Ready {
                routes.entry(id.clone()).or_default().push(Target {
                    member,
                    upstream: id,
                    standard: true,
                });
            }
        }
        // L0 fallback remains available with the same failover order, and each
        // member gets a namespace even when an upstream list is unavailable.
        for entry in &bundled {
            let id = entry["slug"].as_str().expect("bundled id").to_owned();
            if local.contains_key(&id) {
                continue;
            }
            let mut entry = entry.clone();
            apply(&mut entry, choices.overrides.get(&id), &choices.defaults);
            let row = RouterCatalogModel {
                entry,
                readiness: CatalogReadiness::Ready,
                detail: None,
                sources: vec![source(
                    &id,
                    CatalogLayer::Builtin,
                    &codex.status,
                    "bundled fallback; upstream capability unknown",
                )],
            };
            local.insert(id.clone(), row);
            routes.entry(id.clone()).or_default().push(Target {
                member,
                upstream: id,
                standard: false,
            });
        }
        for (id, mut row) in local {
            let prefixed_id = format!("{slug}/{id}");
            let targets: Vec<_> = routes
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|t| t.member == member)
                .cloned()
                .collect();
            routes.insert(prefixed_id.clone(), targets);
            if config.settings.show_prefixed {
                row.entry["slug"] = json!(prefixed_id);
                row.entry["display_name"] = json!(format!(
                    "{name} · {}",
                    row.entry["display_name"].as_str().unwrap_or(&id)
                ));
                apply(
                    &mut row.entry,
                    choices.overrides.get(&prefixed_id),
                    &superba_core::ModelDefaults::default(),
                );
                prefixed.push(row);
            }
        }
        statuses.push(codex.status);
        statuses.push(standard.status);
    }
    // A bare name takes overrides from its first actual target (the normal
    // router order); prefixed names retain the selected provider's choices.
    for (id, row) in &mut rows {
        if let Some(target) = routes.get(id).and_then(|r| r.first())
            && let Some(choices) = config.members.get(&g.members[target.member].cfg.id)
        {
            apply(&mut row.entry, choices.overrides.get(id), &choices.defaults);
        }
    }
    for targets in routes.values_mut() {
        targets.sort_by_key(|t| t.member);
        targets.dedup_by_key(|t| t.member);
    }
    let mut models: Vec<_> = rows.into_values().chain(prefixed).collect();
    for row in &mut models {
        let id = row.entry["slug"].as_str().unwrap_or_default().to_owned();
        apply(
            &mut row.entry,
            config.settings.overrides.get(&id),
            &superba_core::ModelDefaults::default(),
        );
        if let Some(visible) = config.settings.visibility.get(&id) {
            row.entry["visibility"] = json!(if *visible { "list" } else { "hide" });
        }
    }
    models.sort_by(|a, b| {
        a.entry["priority"]
            .as_i64()
            .cmp(&b.entry["priority"].as_i64())
            .then_with(|| a.entry["slug"].as_str().cmp(&b.entry["slug"].as_str()))
    });
    let mut settings = config.settings.clone();
    settings.provider_slugs = slugs;
    Snapshot {
        view: RouterCatalogView {
            settings,
            models,
            sources: statuses,
        },
        routes,
    }
}
