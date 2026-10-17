//! Providers as the other tools describe them, after reading and before mapping.

use std::collections::BTreeMap;

use superba_core::{ForeignApp, ImportSource, ImportStore, LocalizedText, Secret};

/// Where a foreign provider's key is.
#[derive(Clone, PartialEq, Eq)]
pub enum ForeignKey {
    /// In the tool's file (read).
    Secret(Secret),
    /// In the OS keychain; never read.
    Keychain,
    /// In an environment variable.
    Env(String),
    /// None.
    Missing,
}

impl std::fmt::Debug for ForeignKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForeignKey::Secret(s) => write!(f, "Secret({s:?})"),
            ForeignKey::Keychain => f.write_str("Keychain"),
            ForeignKey::Env(v) => write!(f, "Env({v})"),
            ForeignKey::Missing => f.write_str("Missing"),
        }
    }
}

impl ForeignKey {
    /// From an optional key text (empty = missing).
    pub fn from_text(text: Option<&str>) -> Self {
        match text.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => ForeignKey::Secret(Secret::new(k)),
            None => ForeignKey::Missing,
        }
    }

    /// The key, when one was read.
    pub fn secret(&self) -> Option<&Secret> {
        match self {
            ForeignKey::Secret(s) => Some(s),
            _ => None,
        }
    }
}

/// One provider of another tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignProvider {
    /// The tool.
    pub source: ImportSource,
    /// Its id there (unique within the source; part of the candidate id).
    pub id: String,
    /// Its name there.
    pub name: String,
    /// The coding tool it configures.
    pub app: ForeignApp,
    /// The tool's current provider for that coding tool.
    pub current: bool,
    /// Base URL as stored.
    pub base_url: Option<String>,
    /// Further addresses of the same provider (alternative endpoints).
    pub extra_urls: Vec<String>,
    /// Model.
    pub model: Option<String>,
    /// Reasoning effort.
    pub reasoning_effort: Option<String>,
    /// Codex model name -> the provider's model name.
    pub model_map: BTreeMap<String, String>,
    /// Wire API (`responses`, `chat`), when the tool says.
    pub wire_api: Option<String>,
    /// The key.
    pub key: ForeignKey,
    /// The official login (ChatGPT account), not a key-based provider.
    pub official: bool,
    /// Notes the reader adds (settings not imported, ...).
    pub notes: Vec<LocalizedText>,
    /// Set when the reader already knows it cannot be imported.
    pub blocked: Option<LocalizedText>,
    /// Importable, but not selected by default (disabled in the tool, ...).
    pub deselect: bool,
}

impl ForeignProvider {
    /// A provider with only the identity filled in.
    pub fn new(
        source: ImportSource,
        id: impl Into<String>,
        name: impl Into<String>,
        app: ForeignApp,
    ) -> Self {
        Self {
            source,
            id: id.into(),
            name: name.into(),
            app,
            current: false,
            base_url: None,
            extra_urls: Vec::new(),
            model: None,
            reasoning_effort: None,
            model_map: BTreeMap::new(),
            wire_api: None,
            key: ForeignKey::Missing,
            official: false,
            notes: Vec::new(),
            blocked: None,
            deselect: false,
        }
    }

    /// The candidate id (`<source>:<id>`).
    pub fn candidate_id(&self) -> String {
        format!("{}:{}", self.source.id(), self.id)
    }
}

/// What reading one source produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRead {
    /// The tool.
    pub source: ImportSource,
    /// Every store looked at.
    pub stores: Vec<ImportStore>,
    /// Providers of the stores that were read, in the tool's order.
    pub providers: Vec<ForeignProvider>,
}
