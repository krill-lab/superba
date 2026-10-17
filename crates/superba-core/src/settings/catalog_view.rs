//! What the settings engine needs from the Codex settings catalog, as plain data.
//!
//! The catalog itself (generator, merged per-version JSON, overlays) lives in its
//! own crate, which implements [`CatalogSource`] by building one
//! [`CatalogSnapshot`] per Codex version. Lookups use the exact Codex version:
//! pre-release builds bundled by the desktop app differ from their release, so
//! [`crate::CodexVersion::meets`] must not be used here.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::codex::CodexVersion;
use crate::i18n::LocalizedText;
use crate::settings::value::SettingValue;

/// Lifecycle stage of a Codex feature flag (`[features]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeatureStage {
    /// Not finished upstream (Advanced only).
    UnderDevelopment,
    /// Offered in Codex's experimental menu.
    Experimental,
    /// Generally available.
    Stable,
    /// Still accepted, going away (Advanced only).
    Deprecated,
    /// No effect any more (Advanced only).
    Removed,
}

impl FeatureStage {
    /// Shown in the default view (decision 4: stable + experimental).
    pub fn in_default_view(self) -> bool {
        matches!(self, FeatureStage::Stable | FeatureStage::Experimental)
    }

    /// Parse the stage column of `codex features list` (`under development`,
    /// `experimental`, ...) or a catalog / app-server spelling
    /// (`under_development`, `underDevelopment`, `beta`).
    pub fn parse(text: &str) -> Option<Self> {
        let norm: String = text
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        Some(match norm.as_str() {
            "underdevelopment" | "development" | "dev" => FeatureStage::UnderDevelopment,
            "experimental" | "beta" => FeatureStage::Experimental,
            "stable" | "ga" => FeatureStage::Stable,
            "deprecated" => FeatureStage::Deprecated,
            "removed" => FeatureStage::Removed,
            _ => return None,
        })
    }

    /// Badge text.
    pub fn text(self) -> LocalizedText {
        match self {
            FeatureStage::UnderDevelopment => {
                LocalizedText::new("feature_stage.under_development", "Under development")
            }
            FeatureStage::Experimental => {
                LocalizedText::new("feature_stage.experimental", "Experimental")
            }
            FeatureStage::Stable => LocalizedText::new("feature_stage.stable", "Stable"),
            FeatureStage::Deprecated => {
                LocalizedText::new("feature_stage.deprecated", "Deprecated")
            }
            FeatureStage::Removed => LocalizedText::new("feature_stage.removed", "Removed"),
        }
    }
}

/// Shape of a config key in the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingKind {
    /// `true` / `false`.
    Boolean,
    /// Free string.
    String,
    /// Integer.
    Integer,
    /// Number.
    Number,
    /// Array.
    Array,
    /// Table with known keys.
    Table,
    /// Table with user-chosen keys (`mcp_servers.<name>`).
    Map,
    /// One of `enum_values` (strings), maybe also a table form.
    Enum,
    /// Several shapes.
    Union,
    /// `true` / `false` or a table with `enabled` (structured features).
    BoolOrTable,
    /// Anything.
    Any,
    /// Self-referential type.
    Recursive,
    /// A kind this build does not know (newer catalog).
    #[serde(other)]
    Unknown,
}

/// One allowed value of an enum key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingEnumValue {
    /// The string written to the file.
    pub value: String,
    /// Official upstream description, if any.
    #[serde(default)]
    pub description: Option<String>,
}

/// One config key of one Codex version (catalog format 1, the fields the engine
/// and the settings page use).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogKey {
    /// Dotted path; map segments are `<name>` placeholders
    /// (`mcp_servers.<name>.command`).
    pub path: String,
    /// Shape.
    pub kind: SettingKind,
    /// JSON schema types (`string`, `object`, ...).
    #[serde(default)]
    pub types: Vec<String>,
    /// Allowed string values ([`SettingKind::Enum`]).
    #[serde(default)]
    pub enum_values: Vec<SettingEnumValue>,
    /// The enum also accepts a table.
    #[serde(default)]
    pub enum_has_table_form: bool,
    /// Inclusive minimum (numbers).
    #[serde(default)]
    pub minimum: Option<f64>,
    /// Inclusive maximum (numbers).
    #[serde(default)]
    pub maximum: Option<f64>,
    /// Minimum string length.
    #[serde(default)]
    pub min_length: Option<u64>,
    /// Maximum string length.
    #[serde(default)]
    pub max_length: Option<u64>,
    /// Display default; `None` = Codex decides at run time.
    #[serde(default)]
    pub default: Option<SettingValue>,
    /// Official upstream field description (English).
    #[serde(default)]
    pub description: Option<String>,
    /// Upstream text says the key is deprecated or ignored.
    #[serde(default)]
    pub deprecated_hint: bool,
    /// Operating system the key applies to, when known (`windows`, `macos`).
    #[serde(default)]
    pub platform_hint: Option<String>,
    /// The path contains a map segment.
    #[serde(default)]
    pub under_map: bool,
    /// First Codex version that has the key, when the catalog knows it.
    #[serde(default)]
    pub since: Option<CodexVersion>,
}

impl CatalogKey {
    /// A key with only a path and a kind (tests, hand-made catalogs).
    pub fn new(path: impl Into<String>, kind: SettingKind) -> Self {
        Self {
            path: path.into(),
            kind,
            types: Vec::new(),
            enum_values: Vec::new(),
            enum_has_table_form: false,
            minimum: None,
            maximum: None,
            min_length: None,
            max_length: None,
            default: None,
            description: None,
            deprecated_hint: false,
            platform_hint: None,
            under_map: false,
            since: None,
        }
    }
}

/// One `[features]` flag of one Codex version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogFeature {
    /// Registry key (`fast_mode`, `guardianv2.thread_context`).
    pub key: String,
    /// Config path (`features.fast_mode`).
    pub config_path: String,
    /// Stage on this operating system.
    pub stage: FeatureStage,
    /// Default on this operating system.
    pub default_enabled: bool,
    /// Official experimental-menu name.
    #[serde(default)]
    pub name: Option<String>,
    /// Official experimental-menu description.
    #[serde(default)]
    pub menu_description: Option<String>,
}

/// What Codex does with a legacy key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LegacyKind {
    /// Renamed: the same value belongs at `canonical` (`features.collab` ->
    /// `features.multi_agent`).
    Alias {
        /// Canonical path.
        canonical: String,
    },
    /// Replaced by a different key and value (`features.web_search_request = true`
    /// -> `web_search = "live"`); applies when the legacy value equals `when`
    /// (`None` = any value).
    Replace {
        /// Canonical path.
        canonical: String,
        /// Legacy value the rule applies to.
        #[serde(default)]
        when: Option<SettingValue>,
        /// Value to write at `canonical`.
        value: SettingValue,
    },
    /// Ignored by this Codex, with no replacement: removing it changes nothing.
    Ignored,
}

/// A legacy config path and its migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyRule {
    /// Legacy dotted path (`features.collab`, `experimental_use_unified_exec_tool`).
    pub path: String,
    /// Migration.
    #[serde(flatten)]
    pub kind: LegacyKind,
}

/// How well a snapshot matches the Codex version asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CatalogMatch {
    /// The catalog has this exact version.
    Exact,
    /// Not listed; the nearest older version is used.
    Approximate,
    /// Newer than the catalog; its newest version is used.
    Outdated,
    /// Older than the oldest version the catalog covers.
    BelowFloor,
    /// The Codex version is unknown; the newest version is used.
    VersionUnknown,
}

impl CatalogMatch {
    /// True unless [`CatalogMatch::Exact`].
    pub fn is_inexact(&self) -> bool {
        !matches!(self, CatalogMatch::Exact)
    }
}

/// The catalog as seen by one Codex version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    /// Catalog version this snapshot describes.
    pub codex_version: CodexVersion,
    /// The version that was asked for, if known.
    #[serde(default)]
    pub requested: Option<CodexVersion>,
    /// How [`Self::codex_version`] relates to the requested one.
    pub matched: CatalogMatch,
    /// Config keys by path (map segments as `<name>`).
    #[serde(default)]
    pub keys: BTreeMap<String, CatalogKey>,
    /// Feature flags by registry key.
    #[serde(default)]
    pub features: BTreeMap<String, CatalogFeature>,
    /// Legacy paths.
    #[serde(default)]
    pub legacy: Vec<LegacyRule>,
}

impl CatalogSnapshot {
    /// An empty snapshot for `version` (builders and tests).
    pub fn new(version: CodexVersion) -> Self {
        Self {
            requested: Some(version.clone()),
            codex_version: version,
            matched: CatalogMatch::Exact,
            keys: BTreeMap::new(),
            features: BTreeMap::new(),
            legacy: Vec::new(),
        }
    }

    /// Add a key (builder).
    pub fn with_key(mut self, key: CatalogKey) -> Self {
        self.keys.insert(key.path.clone(), key);
        self
    }

    /// Add a feature and its `features.<key>` boolean key (builder).
    pub fn with_feature(mut self, feature: CatalogFeature) -> Self {
        self.keys
            .entry(feature.config_path.clone())
            .or_insert_with(|| {
                let mut k = CatalogKey::new(feature.config_path.clone(), SettingKind::Boolean);
                k.default = Some(SettingValue::Bool(feature.default_enabled));
                k
            });
        self.features.insert(feature.key.clone(), feature);
        self
    }

    /// Add a legacy rule (builder).
    pub fn with_legacy(mut self, rule: LegacyRule) -> Self {
        self.legacy.push(rule);
        self
    }

    /// The key describing a concrete `path`: the exact entry, else the
    /// placeholder pattern with the fewest `<name>` segments that matches
    /// (`mcp_servers.docs.command` -> `mcp_servers.<name>.command`).
    pub fn key(&self, path: &str) -> Option<&CatalogKey> {
        if let Some(k) = self.keys.get(path) {
            return Some(k);
        }
        let segs: Vec<&str> = path.split('.').collect();
        self.keys
            .iter()
            .filter(|(p, _)| p.contains('<'))
            .filter_map(|(p, k)| {
                let pat: Vec<&str> = p.split('.').collect();
                if pat.len() != segs.len() {
                    return None;
                }
                let mut wild = 0usize;
                for (a, b) in pat.iter().zip(&segs) {
                    if a.starts_with('<') && a.ends_with('>') {
                        wild += 1;
                    } else if a != b {
                        return None;
                    }
                }
                Some((wild, k))
            })
            .min_by_key(|(wild, _)| *wild)
            .map(|(_, k)| k)
    }

    /// The nearest ancestor key of `path` that accepts free-form content
    /// ([`SettingKind::Any`], [`SettingKind::Recursive`], [`SettingKind::Map`] of
    /// unknown shape), if any.
    pub fn open_ancestor(&self, path: &str) -> Option<&CatalogKey> {
        let mut cut = path.len();
        while let Some(i) = path[..cut].rfind('.') {
            cut = i;
            if let Some(k) = self.key(&path[..cut])
                && matches!(k.kind, SettingKind::Any | SettingKind::Recursive)
            {
                return Some(k);
            }
        }
        None
    }

    /// The feature whose config path is `path` or a parent of it
    /// (`features.token_budget.enabled` -> `token_budget`).
    pub fn feature_for_path(&self, path: &str) -> Option<&CatalogFeature> {
        self.features
            .values()
            .filter(|f| {
                path == f.config_path
                    || path
                        .strip_prefix(f.config_path.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            .max_by_key(|f| f.config_path.len())
    }

    /// The legacy rule for `path`, if it is a legacy key.
    pub fn legacy_rule(&self, path: &str) -> Option<&LegacyRule> {
        self.legacy.iter().find(|r| r.path == path)
    }
}

/// Source of catalog snapshots (implemented by the catalog crate; tests use
/// [`StaticCatalog`]).
pub trait CatalogSource: Send + Sync {
    /// The snapshot for a Codex version (`None` = version unknown: the newest
    /// snapshot, marked [`CatalogMatch::VersionUnknown`]). `None` when the catalog
    /// has no data at all.
    fn snapshot(&self, version: Option<&CodexVersion>) -> Option<Arc<CatalogSnapshot>>;
}

/// A [`CatalogSource`] over a fixed list of snapshots (tests, offline fallback).
/// Lookup: exact version, else the greatest older version (`Approximate`), else
/// the newest (`Outdated` when newer than all, `BelowFloor` when older).
#[derive(Clone, Debug, Default)]
pub struct StaticCatalog {
    snapshots: Vec<Arc<CatalogSnapshot>>,
}

impl StaticCatalog {
    /// A catalog of these snapshots (any order).
    pub fn new(snapshots: impl IntoIterator<Item = CatalogSnapshot>) -> Self {
        let mut snapshots: Vec<Arc<CatalogSnapshot>> =
            snapshots.into_iter().map(Arc::new).collect();
        snapshots.sort_by(|a, b| a.codex_version.cmp(&b.codex_version));
        Self { snapshots }
    }
}

impl CatalogSource for StaticCatalog {
    fn snapshot(&self, version: Option<&CodexVersion>) -> Option<Arc<CatalogSnapshot>> {
        let newest = self.snapshots.last()?;
        let rebind = |s: &Arc<CatalogSnapshot>, matched: CatalogMatch| {
            if matched == CatalogMatch::Exact {
                return s.clone();
            }
            let mut copy = (**s).clone();
            copy.matched = matched;
            copy.requested = version.cloned();
            Arc::new(copy)
        };
        let Some(v) = version else {
            return Some(rebind(newest, CatalogMatch::VersionUnknown));
        };
        if let Some(s) = self.snapshots.iter().find(|s| &s.codex_version == v) {
            return Some(rebind(s, CatalogMatch::Exact));
        }
        if v > &newest.codex_version {
            return Some(rebind(newest, CatalogMatch::Outdated));
        }
        match self.snapshots.iter().rev().find(|s| &s.codex_version < v) {
            Some(s) => Some(rebind(s, CatalogMatch::Approximate)),
            None => Some(rebind(&self.snapshots[0], CatalogMatch::BelowFloor)),
        }
    }
}
