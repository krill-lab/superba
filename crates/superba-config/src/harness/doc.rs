//! One harness configuration file: its format, how it is read into a
//! document Superba can edit in place, and its state for people.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use superba_core::Result;
use superba_core::harness::{HarnessFileStatus, HarnessFileView};

use super::dotenv::DotenvDoc;
use super::jsonc::{JsonDoc, JsonFlavor};
use super::slot::SlotPath;
use super::yaml::YamlDoc;
use crate::fsx;

/// The format of a harness file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// JSON with the harness's syntax rules.
    Json(JsonFlavor),
    /// YAML (dsh patch files).
    Yaml,
    /// `NAME=value` lines.
    Dotenv,
}

/// A file a harness writer may write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessFile {
    /// Stable key within the harness.
    pub key: String,
    /// Path.
    pub path: PathBuf,
    /// Format.
    pub format: Format,
    /// It holds a key once Superba wrote it (written with mode 0600).
    pub holds_key: bool,
}

impl HarnessFile {
    /// A file.
    pub fn new(key: impl Into<String>, path: PathBuf, format: Format, holds_key: bool) -> Self {
        Self {
            key: key.into(),
            path,
            format,
            holds_key,
        }
    }
}

/// A document of one of the formats.
#[derive(Clone, Debug)]
pub enum Doc {
    /// JSON / JSONC.
    Json(JsonDoc),
    /// YAML.
    Yaml(YamlDoc),
    /// `.env`.
    Dotenv(DotenvDoc),
}

impl Doc {
    /// Parse `text` as `format`.
    pub fn parse(format: Format, text: &str) -> std::result::Result<Self, HarnessFileStatus> {
        match format {
            Format::Json(f) => {
                JsonDoc::parse(text, f)
                    .map(Doc::Json)
                    .map_err(|e| HarnessFileStatus::Invalid {
                        line: e.line,
                        column: e.column,
                        message: e.message,
                    })
            }
            Format::Yaml => {
                YamlDoc::parse(text)
                    .map(Doc::Yaml)
                    .map_err(|e| HarnessFileStatus::Invalid {
                        line: e.line,
                        column: e.column,
                        message: e.message,
                    })
            }
            Format::Dotenv => Ok(Doc::Dotenv(DotenvDoc::parse(text))),
        }
    }

    /// What it means now (`null` for an empty document).
    pub fn value(&self) -> &Value {
        match self {
            Doc::Json(d) => d.value(),
            Doc::Yaml(d) => d.value(),
            Doc::Dotenv(d) => d.value(),
        }
    }

    /// The text now.
    pub fn text(&self) -> &str {
        match self {
            Doc::Json(d) => d.text(),
            Doc::Yaml(d) => d.text(),
            Doc::Dotenv(d) => d.text(),
        }
    }

    /// Set `path` (see each editor).
    pub fn set(
        &mut self,
        path: &SlotPath,
        value: Option<&Value>,
    ) -> std::result::Result<(), String> {
        match self {
            Doc::Json(d) => d.set(path, value),
            Doc::Yaml(d) => d.set(path, value),
            Doc::Dotenv(d) => d.set(path, value),
        }
    }
}

/// A harness file as read now.
#[derive(Clone, Debug)]
pub struct ReadFile {
    /// The file.
    pub file: HarnessFile,
    /// Its bytes (`None` = absent).
    pub bytes: Option<Vec<u8>>,
    /// sha256 of the bytes.
    pub sha256: Option<String>,
    /// The document (`None` when it cannot be read in its format).
    pub doc: Option<Doc>,
    /// Its state.
    pub status: HarnessFileStatus,
}

impl ReadFile {
    /// Read `file`.
    pub fn read(file: &HarnessFile) -> Result<Self> {
        let bytes = match fsx::read_optional(&file.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(Self {
                    file: file.clone(),
                    bytes: None,
                    sha256: None,
                    doc: None,
                    status: HarnessFileStatus::Unreadable {
                        message: error.to_string(),
                    },
                });
            }
        };
        Ok(Self::from_bytes(file.clone(), bytes))
    }

    /// A file with these bytes.
    pub fn from_bytes(file: HarnessFile, bytes: Option<Vec<u8>>) -> Self {
        let sha256 = bytes.as_deref().map(fsx::sha256_hex);
        let (doc, status) = match &bytes {
            None => (Doc::parse(file.format, "").ok(), HarnessFileStatus::Absent),
            Some(b) => match std::str::from_utf8(b) {
                Err(_) => (
                    None,
                    HarnessFileStatus::Unreadable {
                        message: "not UTF-8 text".into(),
                    },
                ),
                Ok(text) => match Doc::parse(file.format, text) {
                    Ok(d) => (Some(d), HarnessFileStatus::Valid),
                    Err(status) => (None, status),
                },
            },
        };
        Self {
            file,
            bytes,
            sha256,
            doc,
            status,
        }
    }

    /// What the file means now (`null` = absent or empty).
    pub fn value(&self) -> Value {
        self.doc
            .as_ref()
            .map(|d| d.value().clone())
            .unwrap_or(Value::Null)
    }

    /// For people.
    pub fn view(&self) -> HarnessFileView {
        HarnessFileView {
            key: self.file.key.clone(),
            path: self.file.path.clone(),
            status: self.status.clone(),
            holds_key: self.file.holds_key,
            backups: 0,
        }
    }
}

/// `path` relative to `base` with `/` separators, for file keys.
pub fn rel_key(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
