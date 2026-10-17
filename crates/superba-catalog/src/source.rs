//! [`superba_core::CatalogSource`] over a verified catalog: converts a
//! [`VersionCatalog`] into the settings engine's [`CatalogSnapshot`] for one
//! operating system.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use superba_core::{
    CatalogFeature, CatalogKey, CatalogMatch, CatalogSnapshot, CatalogSource, CodexVersion,
    FeatureStage, LegacyKind, LegacyRule, SettingEnumValue, SettingKind, SettingValue,
};

use crate::error::Result;
use crate::format::{ConfigKey, KeyKind, Os, Stage, VersionCatalog};
use crate::store::{CatalogStore, MatchKind};

/// Cache key: (catalog version used, version requested).
type SnapshotKey = (String, Option<String>);

/// Catalog snapshots for the settings engine. Feature stages and defaults are
/// evaluated for one operating system: the local one for local targets, the
/// remote host's for SSH targets.
pub struct SnapshotSource {
    store: Arc<CatalogStore>,
    os: Os,
    cache: Mutex<BTreeMap<SnapshotKey, Arc<CatalogSnapshot>>>,
    since: OnceLock<BTreeMap<String, CodexVersion>>,
}

impl std::fmt::Debug for SnapshotSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotSource")
            .field("origin", self.store.origin())
            .field("os", &self.os)
            .finish_non_exhaustive()
    }
}

impl SnapshotSource {
    /// Snapshots of `store` for `os`.
    pub fn new(store: Arc<CatalogStore>, os: Os) -> SnapshotSource {
        SnapshotSource {
            store,
            os,
            cache: Mutex::new(BTreeMap::new()),
            since: OnceLock::new(),
        }
    }

    /// The built-in catalog for the operating system this binary runs on.
    pub fn builtin() -> Result<SnapshotSource> {
        Ok(Self::new(CatalogStore::builtin()?, Os::current()))
    }

    /// The same catalog for another operating system (remote hosts).
    pub fn for_os(&self, os: Os) -> SnapshotSource {
        Self::new(self.store.clone(), os)
    }

    /// The underlying store (explanations, curation, coverage).
    pub fn store(&self) -> &Arc<CatalogStore> {
        &self.store
    }

    /// Operating system of feature stages and defaults.
    pub fn os(&self) -> Os {
        self.os
    }

    /// First catalog version of every config path that is not in the oldest one.
    fn since(&self) -> &BTreeMap<String, CodexVersion> {
        self.since.get_or_init(|| {
            let mut first: BTreeMap<String, CodexVersion> = BTreeMap::new();
            let versions: Vec<CodexVersion> = self.store.versions().cloned().collect();
            for (i, v) in versions.iter().enumerate() {
                let Ok(Some(c)) = self.store.exact(v) else {
                    continue;
                };
                for k in &c.config_keys {
                    first.entry(k.path.clone()).or_insert_with(|| {
                        // Present in the oldest version: introduced before the catalog.
                        if i == 0 {
                            CodexVersion::new(0, 0, 0)
                        } else {
                            v.clone()
                        }
                    });
                }
            }
            first.retain(|_, v| v.triple() != (0, 0, 0));
            first
        })
    }

    fn build(
        &self,
        catalog: &VersionCatalog,
        matched: CatalogMatch,
        requested: Option<&CodexVersion>,
    ) -> Option<CatalogSnapshot> {
        let version = CodexVersion::parse(&catalog.codex_version).ok()?;
        let curation = self.store.curation();
        let since = self.since();
        let mut snap = CatalogSnapshot::new(version);
        snap.matched = matched;
        snap.requested = requested.cloned();

        for k in &catalog.config_keys {
            let mut key = convert_key(k);
            key.platform_hint = curation
                .platforms_of(&k.path)
                .map(|os| os.iter().map(|o| o.as_str()).collect::<Vec<_>>().join(","));
            key.since = since.get(&k.path).cloned();
            snap.keys.insert(key.path.clone(), key);
        }
        for f in &catalog.features {
            let default = f.default_on(self.os);
            let key = snap
                .keys
                .entry(f.config_path.clone())
                .or_insert_with(|| CatalogKey::new(f.config_path.clone(), SettingKind::Boolean));
            if matches!(key.kind, SettingKind::Boolean | SettingKind::BoolOrTable) {
                key.default = Some(SettingValue::Bool(default));
            }
            if f.stage_on(self.os) == Stage::Deprecated {
                key.deprecated_hint = true;
            }
            let exp = f.experimental.as_ref();
            snap.features.insert(
                f.key.clone(),
                CatalogFeature {
                    key: f.key.clone(),
                    config_path: f.config_path.clone(),
                    stage: feature_stage(f.stage_on(self.os)),
                    default_enabled: default,
                    name: exp.map(|e| e.name.clone()),
                    menu_description: exp.map(|e| e.menu_description.clone()),
                },
            );
        }

        // Hand-written semantic migrations win over generated rules for a path.
        let mut legacy: BTreeMap<String, LegacyKind> = BTreeMap::new();
        for m in &curation.migrations {
            let Some(value) = SettingValue::from_toml(&m.value) else {
                continue;
            };
            legacy.insert(
                m.path.clone(),
                LegacyKind::Replace {
                    canonical: m.canonical.clone(),
                    when: m.when.as_ref().and_then(SettingValue::from_toml),
                    value,
                },
            );
        }
        let l = &catalog.legacy;
        for a in &l.feature_aliases {
            legacy
                .entry(format!("features.{}", a.legacy_key))
                .or_insert_with(|| LegacyKind::Alias {
                    canonical: format!("features.{}", a.canonical),
                });
        }
        for a in &l.top_level_aliases {
            legacy
                .entry(a.legacy_key.clone())
                .or_insert_with(|| LegacyKind::Alias {
                    canonical: format!("features.{}", a.canonical),
                });
        }
        for k in &l.ignored_feature_keys {
            legacy
                .entry(format!("features.{k}"))
                .or_insert(LegacyKind::Ignored);
        }
        snap.legacy = legacy
            .into_iter()
            .map(|(path, kind)| LegacyRule { path, kind })
            .collect();
        Some(snap)
    }
}

impl CatalogSource for SnapshotSource {
    fn snapshot(&self, version: Option<&CodexVersion>) -> Option<Arc<CatalogSnapshot>> {
        let (catalog, matched) = match version {
            None => (self.store.latest().ok()?, CatalogMatch::VersionUnknown),
            Some(v) => {
                let m = self.store.for_version(v).ok()?;
                let matched = match m.kind {
                    MatchKind::Exact => CatalogMatch::Exact,
                    MatchKind::Nearest => CatalogMatch::Approximate,
                    MatchKind::NewerThanCatalog => CatalogMatch::Outdated,
                    MatchKind::OlderThanCatalog => CatalogMatch::BelowFloor,
                };
                (m.catalog, matched)
            }
        };
        let cache_key = (
            catalog.codex_version.clone(),
            version.map(ToString::to_string),
        );
        if let Ok(cache) = self.cache.lock()
            && let Some(s) = cache.get(&cache_key)
        {
            return Some(s.clone());
        }
        let snap = Arc::new(self.build(&catalog, matched, version)?);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key, snap.clone());
        }
        Some(snap)
    }
}

fn feature_stage(s: Stage) -> FeatureStage {
    match s {
        Stage::Experimental => FeatureStage::Experimental,
        Stage::Stable => FeatureStage::Stable,
        Stage::Deprecated => FeatureStage::Deprecated,
        Stage::Removed => FeatureStage::Removed,
        // Unknown stages of a newer catalog stay out of the default view.
        Stage::UnderDevelopment | Stage::Unknown => FeatureStage::UnderDevelopment,
    }
}

fn setting_kind(k: KeyKind) -> SettingKind {
    match k {
        KeyKind::Boolean => SettingKind::Boolean,
        KeyKind::String => SettingKind::String,
        KeyKind::Integer => SettingKind::Integer,
        KeyKind::Number => SettingKind::Number,
        KeyKind::Array => SettingKind::Array,
        KeyKind::Table => SettingKind::Table,
        KeyKind::Map => SettingKind::Map,
        KeyKind::Enum => SettingKind::Enum,
        KeyKind::Union => SettingKind::Union,
        KeyKind::BoolOrTable => SettingKind::BoolOrTable,
        KeyKind::Any => SettingKind::Any,
        KeyKind::Recursive => SettingKind::Recursive,
        KeyKind::Unknown => SettingKind::Unknown,
    }
}

fn convert_key(k: &ConfigKey) -> CatalogKey {
    let mut key = CatalogKey::new(k.path.clone(), setting_kind(k.kind));
    key.types = k.types.clone();
    key.enum_values = k
        .enum_values
        .iter()
        .map(|e| SettingEnumValue {
            value: e.value.clone(),
            description: e.description.clone(),
        })
        .collect();
    key.enum_has_table_form = k.enum_has_table_form;
    key.minimum = k.minimum.as_ref().and_then(serde_json::Number::as_f64);
    key.maximum = k.maximum.as_ref().and_then(serde_json::Number::as_f64);
    key.min_length = k.min_length;
    key.max_length = k.max_length;
    // Display defaults have no nulls; anything SettingValue cannot hold is dropped.
    key.default = k
        .default
        .clone()
        .and_then(|d| serde_json::from_value::<SettingValue>(d).ok());
    key.description = k.description.clone();
    key.deprecated_hint = k.deprecated_hint;
    key.under_map = k.under_map;
    key
}
