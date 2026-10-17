//! Hand-maintained metadata the upstream sources do not carry:
//! `catalog/curation.toml` (categories, platform hints and the "Common" list).
//!
//! The schema sorts keys alphabetically and has no grouping, and platform-specific
//! keys have no marker, so both are curated by people. Changing this file does not
//! need a regenerated `dist/`; the loader applies it at run time.

use serde::{Deserialize, Serialize};

use superba_core::LocalizedText;

use crate::error::{CatalogError, Result};
use crate::format::Os;

/// Version of the curation file format.
pub const CURATION_FORMAT: u32 = 1;

/// Category of keys that match no rule.
pub const OTHER_CATEGORY: &str = "other";

/// Category of features that are not assigned elsewhere.
pub const FEATURES_CATEGORY: &str = "features";

/// `catalog/curation.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Curation {
    /// [`CURATION_FORMAT`].
    pub format: u32,
    /// Categories in display order.
    #[serde(default, rename = "category")]
    pub categories: Vec<Category>,
    /// Platform hints.
    #[serde(default, rename = "platform")]
    pub platforms: Vec<PlatformHint>,
    /// Semantic migrations of legacy keys that upstream only describes in prose or
    /// code (aliases and ignored keys come from the generated catalog).
    #[serde(default, rename = "migration")]
    pub migrations: Vec<Migration>,
    /// The settings page's "Common" list (`[common] paths = [...]`).
    #[serde(default, skip_serializing_if = "Common::is_empty")]
    pub common: Common,
}

/// The curated "Common" settings, in display order (people maintain it; entries a
/// consumer does not know are hidden from the list).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Common {
    /// Dotted config paths.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Common {
    /// True when no path is listed.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// A legacy key whose replacement is a different key and value
/// (`features.web_search_request = true` -> `web_search = "live"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    /// Legacy dotted path.
    pub path: String,
    /// Replacement path.
    pub canonical: String,
    /// Legacy value the rule applies to (absent = any value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<toml::Value>,
    /// Value to write at `canonical`.
    pub value: toml::Value,
    /// Where the rule comes from (upstream file, function and release).
    pub source: String,
}

/// A settings category.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Stable id; the UI label is the i18n key `catalog.category.<id>`.
    pub id: String,
    /// English label (fallback when the UI catalog has no translation).
    pub label: String,
    /// Config path prefixes (whole segments: `tui` matches `tui.theme`, not `tui_x`).
    #[serde(default)]
    pub keys: Vec<String>,
    /// Feature keys shown in this category instead of [`FEATURES_CATEGORY`].
    #[serde(default)]
    pub features: Vec<String>,
}

impl Category {
    /// Label as localizable text.
    pub fn label_text(&self) -> LocalizedText {
        LocalizedText::new(format!("catalog.category.{}", self.id), self.label.clone())
    }
}

/// Keys that only apply on some operating systems.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformHint {
    /// Config path prefix (whole segments).
    pub prefix: String,
    /// Operating systems where the keys apply.
    pub os: Vec<Os>,
}

/// True if `path` is `prefix` or below it.
pub fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'.')
}

impl Curation {
    /// Parse `curation.toml`.
    pub fn parse(src: &str) -> Result<Curation> {
        let c: Curation =
            toml::from_str(src).map_err(|e| CatalogError::parse("curation.toml", e))?;
        if c.format != CURATION_FORMAT {
            return Err(CatalogError::UnsupportedFormat {
                what: "curation",
                found: c.format,
                supported: CURATION_FORMAT,
            });
        }
        let mut paths = std::collections::BTreeSet::new();
        for m in &c.migrations {
            if !paths.insert(m.path.as_str()) {
                return Err(CatalogError::Invalid(format!(
                    "curation.toml: migration of `{}` is defined twice",
                    m.path
                )));
            }
            if m.source.trim().is_empty() {
                return Err(CatalogError::Invalid(format!(
                    "curation.toml: migration of `{}` has no source",
                    m.path
                )));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for cat in &c.categories {
            if !seen.insert(cat.id.as_str()) {
                return Err(CatalogError::Invalid(format!(
                    "curation.toml: category `{}` is defined twice",
                    cat.id
                )));
            }
        }
        Ok(c)
    }

    /// The curated "Common" paths in display order.
    pub fn common(&self) -> &[String] {
        &self.common.paths
    }

    /// The category definition of `id`.
    pub fn category(&self, id: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Category id of a config path: the category with the longest matching prefix;
    /// `features.<key>...` paths follow their feature; otherwise [`OTHER_CATEGORY`].
    pub fn category_of_key(&self, path: &str) -> &str {
        if let Some(rest) = path.strip_prefix("features.") {
            let feature = rest.split('.').next().unwrap_or(rest);
            return self.category_of_feature(feature);
        }
        let mut best: Option<(&str, usize)> = None;
        for c in &self.categories {
            for p in &c.keys {
                if path_has_prefix(path, p) && best.is_none_or(|(_, len)| p.len() > len) {
                    best = Some((c.id.as_str(), p.len()));
                }
            }
        }
        best.map_or(OTHER_CATEGORY, |(id, _)| id)
    }

    /// Category id of a feature key ([`FEATURES_CATEGORY`] unless assigned).
    pub fn category_of_feature(&self, key: &str) -> &str {
        let base = key.split('.').next().unwrap_or(key);
        self.categories
            .iter()
            .find(|c| c.features.iter().any(|f| f == key || f == base))
            .map_or(FEATURES_CATEGORY, |c| c.id.as_str())
    }

    /// Operating systems a config path applies to, if it is platform specific
    /// (longest matching prefix).
    pub fn platforms_of(&self, path: &str) -> Option<&[Os]> {
        self.platforms
            .iter()
            .filter(|h| path_has_prefix(path, &h.prefix))
            .max_by_key(|h| h.prefix.len())
            .map(|h| h.os.as_slice())
    }
}
