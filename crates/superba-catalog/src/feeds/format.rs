//! File formats of the feeds (`catalog/feeds/`), shared by the client and
//! `superba-catalog-gen`.
//!
//! ```text
//! index.json            {"format":1,"serial":N,"generated":"YYYY-MM-DD","files":[{path,size,sha256}]}
//! index.json.sig        ed25519 over FEEDS_SIGNATURE_CONTEXT + index.json (same file format as the
//!                       settings catalog's signature)
//! models.json           {"format":1,"codex_version":"0.163.0","sources":[..],"models":[ModelSpec]}
//! prices.json           {"format":1,"rev":"YYYY-MM-DD","entries":{"<model>":PriceEntry}}
//! codex-versions.json   {"format":1,"releases":[CodexRelease]}   newest first
//! app-versions.json     {"format":1,"releases":[AppRelease]}     newest first
//! prompts.json          {"format":1,"prompts":{"<codex version>":"prompts/<sha8>.md"}}
//! prompts/<sha8>.md     codex-rs/models-manager/prompt.md of those versions
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use superba_core::{AppRelease, CodexRelease, CodexVersion, ModelSpec, PriceEntry, PriceTable};

use crate::error::{CatalogError, Result};

/// Version of every feed file format.
pub const FEEDS_FORMAT: u32 = 1;
/// The feed index.
pub const INDEX_FILE: &str = "index.json";
/// Its signature.
pub const SIGNATURE_FILE: &str = "index.json.sig";
/// Model list.
pub const MODELS_FILE: &str = "models.json";
/// Price table.
pub const PRICES_FILE: &str = "prices.json";
/// Codex releases.
pub const CODEX_VERSIONS_FILE: &str = "codex-versions.json";
/// Desktop app builds.
pub const APP_VERSIONS_FILE: &str = "app-versions.json";
/// Codex version -> prompt file.
pub const PROMPTS_FILE: &str = "prompts.json";
/// Directory of the prompt files.
pub const PROMPTS_DIR: &str = "prompts";

/// `index.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedIndex {
    /// [`FEEDS_FORMAT`].
    pub format: u32,
    /// Monotonic serial (a client never goes back to a lower one).
    pub serial: u64,
    /// Generation date (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    /// Every published file except the index and its signature.
    pub files: Vec<FeedFile>,
}

impl FeedIndex {
    /// The entry of `path`.
    pub fn file(&self, path: &str) -> Option<&FeedFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// One file of the index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedFile {
    /// Path relative to the feeds root (`/`-separated).
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256.
    pub sha256: String,
}

/// `models.json`: the built-in model list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsFeed {
    /// [`FEEDS_FORMAT`].
    pub format: u32,
    /// Newest Codex release the OpenAI entries come from.
    pub codex_version: CodexVersion,
    /// Where the entries come from (release tags, the hand-written file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Models (no `raw` entries: metadata only).
    pub models: Vec<ModelSpec>,
}

/// `prices.json`: USD per 1M tokens from public price pages only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricesFeed {
    /// [`FEEDS_FORMAT`].
    pub format: u32,
    /// Revision of the table.
    #[serde(default)]
    pub rev: String,
    /// Entries by model slug; each carries its `source` page and `rev`.
    #[serde(default)]
    pub entries: BTreeMap<String, PriceEntry>,
}

impl PricesFeed {
    /// As a [`PriceTable`].
    pub fn table(&self) -> PriceTable {
        PriceTable {
            rev: self.rev.clone(),
            entries: self.entries.clone(),
        }
    }
}

/// `codex-versions.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexVersionsFeed {
    /// [`FEEDS_FORMAT`].
    pub format: u32,
    /// Releases, newest first.
    #[serde(default)]
    pub releases: Vec<CodexRelease>,
}

/// `app-versions.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppVersionsFeed {
    /// [`FEEDS_FORMAT`].
    pub format: u32,
    /// Builds, newest first.
    #[serde(default)]
    pub releases: Vec<AppRelease>,
}

/// `prompts.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptsFeed {
    /// [`FEEDS_FORMAT`].
    pub format: u32,
    /// Codex version (as written) -> prompt file path.
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
}

/// Parse a feed JSON file and check its `format`.
pub fn parse_feed<T: serde::de::DeserializeOwned>(
    path: &str,
    bytes: &[u8],
    format: impl Fn(&T) -> u32,
) -> Result<T> {
    let v: T = serde_json::from_slice(bytes).map_err(|e| CatalogError::parse(path, e))?;
    let found = format(&v);
    if found != FEEDS_FORMAT {
        return Err(CatalogError::UnsupportedFormat {
            what: "feed",
            found,
            supported: FEEDS_FORMAT,
        });
    }
    Ok(v)
}

/// Parse and check `index.json`.
pub fn parse_index(bytes: &[u8]) -> Result<FeedIndex> {
    let index = parse_feed::<FeedIndex>(INDEX_FILE, bytes, |i| i.format)?;
    for f in &index.files {
        crate::integrity::validate_relative_path(&f.path)?;
        if f.path == INDEX_FILE || f.path == SIGNATURE_FILE {
            return Err(CatalogError::integrity(&f.path, "the index lists itself"));
        }
    }
    Ok(index)
}

/// Pretty JSON with a trailing newline (the generator's output format).
pub fn to_pretty_json<T: Serialize>(v: &T) -> Result<String> {
    let mut s =
        serde_json::to_string_pretty(v).map_err(|e| CatalogError::Invalid(e.to_string()))?;
    s.push('\n');
    Ok(s)
}
