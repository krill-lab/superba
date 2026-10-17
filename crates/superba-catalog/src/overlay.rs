//! Curated text layer: `catalog/overlay/*.toml` (official English copied by the
//! generator; Chinese written per entry from the codex source).
//!
//! Each overlay file maps entry ids to an [`OverlayEntry`]:
//!
//! - `feature:<key>`: a feature of the registry,
//! - `config:<path>`: a `config.toml` key (`<name>` for map segments),
//! - `config:<path>=<value>`: one value of an enum key.
//!
//! The generator (`superba-catalog-gen overlay-sync`) owns the `en*` fields: `en` and
//! `en_name` are copied verbatim from upstream (schema descriptions and the menu text
//! of experimental features), `en_source` records where they came from and `en_hash`
//! fingerprints them. Entries without upstream text keep `en = ""` and count as
//! "English missing" in the coverage report. People own `zh`, `zh_name`, `en_note`
//! and `zh_basis`; the generator never writes Chinese text. A Chinese text whose
//! `zh_basis` differs from the current `en_hash` is stale: the English it translated
//! has changed since.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CatalogError, Result};
use crate::integrity::sha256_hex;

/// Version of the overlay file format.
pub const OVERLAY_FORMAT: u32 = 1;

/// Identifier of an explainable catalog entry.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryId {
    /// `feature:<key>`.
    Feature(String),
    /// `config:<path>`.
    Config(String),
    /// `config:<path>=<value>`.
    Value {
        /// Key path.
        path: String,
        /// Enum value.
        value: String,
    },
}

impl EntryId {
    /// Parse `feature:..`, `config:..` or `config:..=..`.
    pub fn parse(s: &str) -> Result<EntryId> {
        if let Some(key) = s.strip_prefix("feature:") {
            if key.is_empty() {
                return Err(CatalogError::Invalid(format!("empty feature id `{s}`")));
            }
            return Ok(EntryId::Feature(key.to_owned()));
        }
        if let Some(rest) = s.strip_prefix("config:") {
            if rest.is_empty() {
                return Err(CatalogError::Invalid(format!("empty config id `{s}`")));
            }
            return Ok(match rest.split_once('=') {
                Some((path, value)) => EntryId::Value {
                    path: path.to_owned(),
                    value: value.to_owned(),
                },
                None => EntryId::Config(rest.to_owned()),
            });
        }
        Err(CatalogError::Invalid(format!(
            "overlay id `{s}` must start with `feature:` or `config:`"
        )))
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryId::Feature(k) => write!(f, "feature:{k}"),
            EntryId::Config(p) => write!(f, "config:{p}"),
            EntryId::Value { path, value } => write!(f, "config:{path}={value}"),
        }
    }
}

/// One overlay entry. See the module documentation for who owns which field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayEntry {
    /// Official name (experimental features only). Generator-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en_name: Option<String>,
    /// Official upstream English text, verbatim; empty when upstream has none.
    /// Generator-owned.
    #[serde(default)]
    pub en: String,
    /// Where `en` / `en_name` come from (Codex version and source file).
    /// Generator-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en_source: Option<String>,
    /// Fingerprint of `en_name` + `en` ([`text_hash`]); empty when both are empty.
    /// Generator-owned.
    #[serde(default)]
    pub en_hash: String,
    /// Maintainer-written English note, used only when there is no official
    /// text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en_note: Option<String>,
    /// Chinese name (every entry), written per entry from the codex source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh_name: Option<String>,
    /// Chinese explanation, written per entry from the codex source.
    #[serde(default)]
    pub zh: String,
    /// The `en_hash` the Chinese text was written against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh_basis: Option<String>,
    /// Set when the id no longer exists in any catalog version: the last version
    /// that had it. Kept only while it holds maintainer-written text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<String>,
}

impl OverlayEntry {
    /// True if the entry has Chinese text.
    pub fn has_zh(&self) -> bool {
        !self.zh.trim().is_empty()
            || self
                .zh_name
                .as_deref()
                .is_some_and(|n| !n.trim().is_empty())
    }

    /// True if a person wrote an English note.
    pub fn has_en_note(&self) -> bool {
        self.en_note
            .as_deref()
            .is_some_and(|n| !n.trim().is_empty())
    }

    /// True if upstream provides English text.
    pub fn has_official_en(&self) -> bool {
        !self.en.trim().is_empty()
            || self
                .en_name
                .as_deref()
                .is_some_and(|n| !n.trim().is_empty())
    }

    /// State of the Chinese text.
    pub fn zh_state(&self) -> ZhState {
        if !self.has_zh() {
            ZhState::Missing
        } else if self.zh_basis.as_deref().unwrap_or("") == self.en_hash {
            ZhState::Current
        } else {
            ZhState::Stale
        }
    }
}

/// State of an entry's Chinese text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZhState {
    /// Nobody has written it yet (the UI shows "no Chinese description yet").
    #[default]
    Missing,
    /// Written against the current English text.
    Current,
    /// Written against an older English text.
    Stale,
}

/// Fingerprint of an official English text: the first 16 hex digits of SHA-256 of
/// `name`, a unit separator and `text`; empty when both are empty.
pub fn text_hash(name: Option<&str>, text: &str) -> String {
    let name = name.unwrap_or("");
    if name.is_empty() && text.is_empty() {
        return String::new();
    }
    let mut buf = String::with_capacity(name.len() + text.len() + 1);
    buf.push_str(name);
    buf.push('\u{1f}');
    buf.push_str(text);
    sha256_hex(buf.as_bytes())[..16].to_owned()
}

/// One parsed overlay file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayFile {
    /// [`OVERLAY_FORMAT`].
    pub format: u32,
    /// Entries by id.
    #[serde(flatten)]
    pub entries: BTreeMap<String, OverlayEntry>,
}

impl OverlayFile {
    /// Parse an overlay TOML file; `name` is used in errors.
    pub fn parse(name: &str, src: &str) -> Result<OverlayFile> {
        let file: OverlayFile =
            toml::from_str(src).map_err(|e| CatalogError::parse(format!("overlay {name}"), e))?;
        if file.format != OVERLAY_FORMAT {
            return Err(CatalogError::UnsupportedFormat {
                what: "overlay",
                found: file.format,
                supported: OVERLAY_FORMAT,
            });
        }
        for id in file.entries.keys() {
            EntryId::parse(id)?;
        }
        Ok(file)
    }
}

/// All overlay files merged.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    entries: BTreeMap<String, (String, OverlayEntry)>,
}

impl Overlay {
    /// Merge files given as `(file name, contents)`. An id defined in two files is an
    /// error.
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Overlay> {
        let mut entries = BTreeMap::new();
        for (name, src) in files {
            let file = OverlayFile::parse(name, src)?;
            for (id, entry) in file.entries {
                if let Some((other, _)) = entries.get(&id) {
                    return Err(CatalogError::Invalid(format!(
                        "overlay id `{id}` is defined in both {other} and {name}"
                    )));
                }
                entries.insert(id, (name.to_owned(), entry));
            }
        }
        Ok(Overlay { entries })
    }

    /// The entry of `id`.
    pub fn get(&self, id: &EntryId) -> Option<&OverlayEntry> {
        self.entries.get(&id.to_string()).map(|(_, e)| e)
    }

    /// Iterate `(id, file name, entry)` in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, &OverlayEntry)> {
        self.entries
            .iter()
            .map(|(id, (file, e))| (id.as_str(), file.as_str(), e))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What the UI shows for one entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Explanation {
    /// Official name (experimental features).
    pub official_name: Option<String>,
    /// Official upstream English text for the matched Codex version.
    pub official_en: Option<String>,
    /// Maintainer-written English note (only offered when there is no official
    /// text).
    pub en_note: Option<String>,
    /// Chinese name (written per entry from the codex source).
    pub zh_name: Option<String>,
    /// Chinese explanation (written per entry from the codex source).
    pub zh: Option<String>,
    /// State of the Chinese text (`Missing` -> show "no Chinese description yet").
    pub zh_state: ZhState,
}

impl Explanation {
    /// True when upstream has no English text for this entry (flagged in coverage).
    pub fn en_missing(&self) -> bool {
        self.official_en.is_none() && self.official_name.is_none()
    }

    /// Build from the official text of the matched version and the overlay entry.
    pub fn new(
        official_name: Option<&str>,
        official_en: Option<&str>,
        entry: Option<&OverlayEntry>,
    ) -> Explanation {
        let nonempty = |s: Option<&str>| {
            s.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let official_name = nonempty(official_name);
        let official_en = nonempty(official_en);
        let mut ex = Explanation {
            official_name,
            official_en,
            ..Explanation::default()
        };
        if let Some(e) = entry {
            if ex.official_en.is_none() && ex.official_name.is_none() {
                ex.en_note = nonempty(e.en_note.as_deref());
            }
            ex.zh = nonempty(Some(e.zh.as_str()));
            ex.zh_name = nonempty(e.zh_name.as_deref());
            ex.zh_state = e.zh_state();
        }
        ex
    }
}
