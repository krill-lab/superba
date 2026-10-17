//! Cross-check of `codex features list` against the catalog.

use std::collections::{BTreeMap, BTreeSet};

use superba_core::{
    CatalogSnapshot, FeatureCrossCheck, FeatureMismatch, FeaturesList, LegacyKind, SettingsConsumer,
};

use super::values::read_config_values;

/// Compare the features a consumer's Codex printed with its catalog snapshot.
///
/// Rows the catalog does not know, or whose stage differs, mean the catalog is
/// older than this Codex ([`FeatureCrossCheck::catalog_outdated`]); the runtime
/// stage and effective state win either way. A feature that is not configured in
/// `config_toml` (directly, under its table, or through a legacy alias) but whose
/// effective state differs from the catalog default is reported as
/// [`FeatureMismatch::Default`] (dependencies or managed configuration can cause
/// this too, so it does not mark the catalog outdated). Catalog features the
/// output lacks are reported only when the output has rows at all.
pub fn cross_check_features(
    consumer: &SettingsConsumer,
    list: &FeaturesList,
    snapshot: Option<&CatalogSnapshot>,
    config_toml: Option<&[u8]>,
) -> FeatureCrossCheck {
    let effective: BTreeMap<String, bool> = list
        .entries
        .iter()
        .map(|e| (e.key.clone(), e.enabled))
        .collect();
    let stages = list
        .entries
        .iter()
        .filter_map(|e| Some((e.key.clone(), e.stage?)))
        .collect();
    let mut check = FeatureCrossCheck {
        consumer: consumer.clone(),
        mismatches: Vec::new(),
        effective,
        stages,
        catalog_outdated: false,
    };
    let Some(snapshot) = snapshot else {
        return check;
    };
    let configured = configured_features(snapshot, config_toml);
    for e in &list.entries {
        let Some(f) = snapshot.features.get(&e.key) else {
            check.mismatches.push(FeatureMismatch::UnknownToCatalog {
                key: e.key.clone(),
                stage_text: e.stage_text.clone(),
            });
            check.catalog_outdated = true;
            continue;
        };
        if let Some(stage) = e.stage
            && stage != f.stage
        {
            check.mismatches.push(FeatureMismatch::Stage {
                key: e.key.clone(),
                catalog: f.stage,
                runtime: e.stage_text.clone(),
            });
            check.catalog_outdated = true;
        }
        if !configured.contains(e.key.as_str()) && e.enabled != f.default_enabled {
            check.mismatches.push(FeatureMismatch::Default {
                key: e.key.clone(),
                catalog: f.default_enabled,
                runtime: e.enabled,
            });
        }
    }
    if !list.entries.is_empty() {
        for key in snapshot.features.keys() {
            if list.get(key).is_none() {
                check
                    .mismatches
                    .push(FeatureMismatch::MissingAtRuntime { key: key.clone() });
            }
        }
    }
    check
}

/// Feature keys the configuration sets, directly (`features.x`, `features.x.*`)
/// or through a legacy alias. An unparseable file counts as configuring nothing.
fn configured_features<'s>(
    snapshot: &'s CatalogSnapshot,
    config_toml: Option<&[u8]>,
) -> BTreeSet<&'s str> {
    let values = read_config_values(config_toml).unwrap_or_default();
    let mut out = BTreeSet::new();
    for f in snapshot.features.values() {
        let prefix = format!("{}.", f.config_path);
        if values
            .keys()
            .any(|p| p == &f.config_path || p.starts_with(&prefix))
        {
            out.insert(f.key.as_str());
        }
    }
    for rule in &snapshot.legacy {
        if let LegacyKind::Alias { canonical } = &rule.kind
            && values.contains_key(&rule.path)
            && let Some(f) = snapshot.feature_for_path(canonical)
        {
            out.insert(f.key.as_str());
        }
    }
    out
}
