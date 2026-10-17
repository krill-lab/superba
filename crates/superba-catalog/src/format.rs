//! On-disk formats: per-version catalogs (`catalog/dist/codex-<version>.json`), the
//! signed index (`catalog/dist/index.json` + `index.json.sig`).
//!
//! Every type here is produced by `superba-catalog-gen` and read by the app. Fields
//! carry upstream data only (`openai/codex` at a release tag): the schema
//! (`codex-rs/core/config.schema.json`), the feature registry
//! (`codex-rs/features/src/lib.rs`) and the legacy tables
//! (`codex-rs/features/src/legacy.rs`). Human-written text lives in the overlays
//! ([`crate::overlay`]), hand-maintained metadata in the curation file
//! ([`crate::curation`]).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version of the per-version catalog format this crate reads and writes.
pub const CATALOG_FORMAT: u32 = 1;

/// Version of the index format.
pub const INDEX_FORMAT: u32 = 1;

/// Version of the signature file format.
pub const SIGNATURE_FORMAT: u32 = 1;

/// Upstream source files, relative to the root of an `openai/codex` checkout.
pub mod sources {
    /// Generated JSON schema of `config.toml` (schemars, draft-07).
    pub const CONFIG_SCHEMA: &str = "codex-rs/core/config.schema.json";
    /// Feature registry (`FEATURES`, `enum Feature`, `apply_map`).
    pub const FEATURES_LIB: &str = "codex-rs/features/src/lib.rs";
    /// Legacy aliases (`ALIASES`, `LegacyFeatureToggles`).
    pub const FEATURES_LEGACY: &str = "codex-rs/features/src/legacy.rs";
    /// All three, in a fixed order.
    pub const ALL: [&str; 3] = [CONFIG_SCHEMA, FEATURES_LIB, FEATURES_LEGACY];
}

/// Operating systems Superba supports. Feature stages and defaults may differ per OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    /// macOS.
    Macos,
    /// Linux.
    Linux,
    /// Windows.
    Windows,
}

impl Os {
    /// All supported operating systems, in a fixed order.
    pub const ALL: [Os; 3] = [Os::Macos, Os::Linux, Os::Windows];

    /// The OS this binary was built for (Linux for any other Unix).
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    /// `macos`, `linux` or `windows`.
    pub fn as_str(self) -> &'static str {
        match self {
            Os::Macos => "macos",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }

    /// The catalog OS of a remote host, if it is one Codex runs on.
    pub fn from_remote(os: &superba_core::RemoteOs) -> Option<Os> {
        match os {
            superba_core::RemoteOs::Linux => Some(Os::Linux),
            superba_core::RemoteOs::Darwin => Some(Os::Macos),
            _ => None,
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Os {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "macos" => Ok(Os::Macos),
            "linux" => Ok(Os::Linux),
            "windows" => Ok(Os::Windows),
            other => Err(format!(
                "unknown OS `{other}` (expected macos, linux or windows)"
            )),
        }
    }
}

/// One Codex version's catalog: `catalog/dist/codex-<version>.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionCatalog {
    /// [`CATALOG_FORMAT`].
    pub catalog_format: u32,
    /// Codex version (`0.163.0`, `0.162.0-alpha.17.2`).
    pub codex_version: String,
    /// Where the data came from.
    pub source: SourceRef,
    /// Every `config.toml` path of the schema, parents before children.
    pub config_keys: Vec<ConfigKey>,
    /// Every entry of the feature registry, sorted by key.
    pub features: Vec<Feature>,
    /// Legacy keys and how Codex treats them.
    pub legacy: Legacy,
    /// Documentation of schema definitions (types) referenced by
    /// [`ConfigKey::definition`]. Shared by every key of that type and sometimes
    /// developer-facing, so it is kept apart from the key's own description.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub type_docs: BTreeMap<String, String>,
}

impl VersionCatalog {
    /// The config key at `path` (`<name>` for map entries).
    pub fn key(&self, path: &str) -> Option<&ConfigKey> {
        self.config_keys.iter().find(|k| k.path == path)
    }

    /// The feature `key` (registry key, e.g. `fast_mode` or `guardianv2.thread_context`).
    pub fn feature(&self, key: &str) -> Option<&Feature> {
        self.features.iter().find(|f| f.key == key)
    }

    /// The type documentation of `key`'s definition, if any.
    pub fn type_doc(&self, key: &ConfigKey) -> Option<&str> {
        key.definition
            .as_deref()
            .and_then(|d| self.type_docs.get(d))
            .map(String::as_str)
    }

    /// Canonical feature key of a legacy `[features]` alias (`collab` -> `multi_agent`).
    pub fn canonical_feature(&self, legacy_key: &str) -> Option<&str> {
        self.legacy
            .feature_aliases
            .iter()
            .find(|a| a.legacy_key == legacy_key)
            .map(|a| a.canonical.as_str())
    }
}

/// Upstream reference of a [`VersionCatalog`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// `openai/codex`.
    pub repo: String,
    /// Release tag (`rust-v0.163.0`).
    pub tag: String,
    /// Commit the (annotated) tag points to.
    pub commit: String,
    /// True for alpha/beta tags.
    pub prerelease: bool,
    /// Git blob ids of the source files (path -> sha1), for provenance.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
}

/// Shape of a config value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// `true` / `false`.
    Boolean,
    /// Free string.
    String,
    /// Integer.
    Integer,
    /// Number.
    Number,
    /// Array ([`ConfigKey::items`] names the element type).
    Array,
    /// Table with fixed keys (children follow as their own keys).
    Table,
    /// Table with free-form keys (`<name>` children).
    Map,
    /// One of [`ConfigKey::enum_values`] (plus a table form when
    /// [`ConfigKey::enum_has_table_form`]).
    Enum,
    /// Several unrelated shapes; tagged unions carry a
    /// [`ConfigKey::discriminator`].
    Union,
    /// `true`/`false` or a table with `enabled` (structured features).
    BoolOrTable,
    /// Anything (no schema constraint).
    Any,
    /// A type that contains itself; children are not expanded.
    Recursive,
    /// A kind this version of Superba does not know (newer catalog).
    #[serde(other)]
    Unknown,
}

impl KeyKind {
    /// True for values (as opposed to tables and maps that only hold children).
    pub fn is_leaf(self) -> bool {
        !matches!(self, KeyKind::Table | KeyKind::Map)
    }
}

/// Where a display default comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSource {
    /// The property's own `default`.
    Schema,
    /// The parent table's `default` object.
    Parent,
}

/// One allowed value of an enum key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnumValue {
    /// The value as written in `config.toml`.
    pub value: String,
    /// Upstream description of the value (schema `oneOf` branch doc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One `config.toml` path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigKey {
    /// Dotted path; map entries are `<name>` (`mcp_servers.<name>.command`).
    pub path: String,
    /// Value shape.
    pub kind: KeyKind,
    /// JSON types accepted (from `type` and from union branches).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// Allowed values of an enum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<EnumValue>,
    /// The enum also accepts a table (`approval_policy = { granular = .. }`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub enum_has_table_form: bool,
    /// Element type of an array (definition name or JSON type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// JSON schema `format` (`uint64`, `double`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Inclusive minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<serde_json::Number>,
    /// Inclusive maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<serde_json::Number>,
    /// Minimum string length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    /// Maximum string length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    /// Display default, derived from the schema `default`: `null` values are dropped
    /// (schemars writes `null` for values Codex computes at run time), then objects
    /// left empty are dropped too, recursively (a `{}` default of a map or table says
    /// nothing a reader can use); a key without a default of its own takes its member
    /// of the parent table's display default ([`DefaultSource::Parent`]). Absent when
    /// nothing is left; the raw value is then in [`ConfigKey::schema_default`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub default: Option<Value>,
    /// Origin of [`ConfigKey::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_source: Option<DefaultSource>,
    /// The raw schema `default` of this property whenever the display default is not
    /// exactly it (for example `null`, `{}`, or an object with `null` members):
    /// lossless. A present JSON `null` means the raw default was `null`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub schema_default: Option<Value>,
    /// Upstream field documentation (official English text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Schema definition of the value (`AskForApproval`); its documentation is in
    /// [`VersionCatalog::type_docs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// Heuristic: the description says deprecated / legacy / ignored. The schema has
    /// no `deprecated` keyword.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deprecated_hint: bool,
    /// The path is below a map (`<name>` segment).
    #[serde(default, skip_serializing_if = "is_false")]
    pub under_map: bool,
    /// Tagged unions: the tag property (`kind`), `<external>` for externally tagged
    /// enums (`{otlp-http = {..}}`) or `<branch>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<String>,
    /// Keys that exist only in some variants of a tagged union, as
    /// `<union path>:<label>`. Keys of different labels of one union are mutually
    /// exclusive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variant_of: Vec<String>,
}

/// Lifecycle stage of a feature (`Stage` in `codex-rs/features`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Not ready; Codex warns when it is enabled.
    UnderDevelopment,
    /// Opt-in, listed in `/experimental`.
    Experimental,
    /// Supported.
    Stable,
    /// Still accepted, scheduled for removal.
    Deprecated,
    /// No longer has an effect.
    Removed,
    /// A stage this version of Superba does not know (newer catalog).
    #[serde(other)]
    Unknown,
}

impl Stage {
    /// The string `codex features list` prints for this stage.
    pub fn cli_str(self) -> &'static str {
        match self {
            Stage::UnderDevelopment => "under development",
            Stage::Experimental => "experimental",
            Stage::Stable => "stable",
            Stage::Deprecated => "deprecated",
            Stage::Removed => "removed",
            Stage::Unknown => "unknown",
        }
    }

    /// Stages shown without "Advanced" (decision: stable and experimental).
    pub fn is_default_visible(self) -> bool {
        matches!(self, Stage::Stable | Stage::Experimental)
    }

    /// Stable key for i18n (`catalog.stage.<id>`).
    pub fn id(self) -> &'static str {
        match self {
            Stage::UnderDevelopment => "under_development",
            Stage::Experimental => "experimental",
            Stage::Stable => "stable",
            Stage::Deprecated => "deprecated",
            Stage::Removed => "removed",
            Stage::Unknown => "unknown",
        }
    }
}

/// Text of an experimental feature (`Stage::Experimental { .. }`), official English.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentalText {
    /// Menu name.
    pub name: String,
    /// Menu description.
    pub menu_description: String,
    /// Announcement shown once, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement: Option<String>,
}

/// Per-OS difference of a feature.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureOsOverride {
    /// Stage on this OS, when it differs from [`Feature::stage`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// Default on this OS, when it differs from [`Feature::default_enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_enabled: Option<bool>,
}

/// One entry of the feature registry (`FEATURES` in `codex-rs/features/src/lib.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    /// Registry key (`fast_mode`; dotted keys such as `guardianv2.thread_context` are
    /// nested tables in `config.toml`).
    pub key: String,
    /// `config.toml` path (`features.<key>`).
    pub config_path: String,
    /// Rust variant (`Feature::FastMode`); legacy aliases point at variants.
    pub variant: String,
    /// Stage on Linux (base value; see [`Feature::os_overrides`]).
    pub stage: Stage,
    /// Default on Linux (base value; see [`Feature::os_overrides`]).
    pub default_enabled: bool,
    /// Differences on other operating systems.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub os_overrides: BTreeMap<Os, FeatureOsOverride>,
    /// Official menu text, for features that are experimental on some OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<ExperimentalText>,
    /// Rust doc comment of the variant: upstream, developer-facing. Kept for
    /// reviewers; the UI does not show it as an explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rustdoc: Option<String>,
    /// The schema has no `features.<key>` property (e.g. `artifact`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub not_in_schema: bool,
}

impl Feature {
    /// Stage on `os`.
    pub fn stage_on(&self, os: Os) -> Stage {
        self.os_overrides
            .get(&os)
            .and_then(|o| o.stage)
            .unwrap_or(self.stage)
    }

    /// Default on `os`.
    pub fn default_on(&self, os: Os) -> bool {
        self.os_overrides
            .get(&os)
            .and_then(|o| o.default_enabled)
            .unwrap_or(self.default_enabled)
    }
}

/// A legacy key and the canonical feature it maps to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    /// Legacy key.
    pub legacy_key: String,
    /// Canonical feature key.
    pub canonical: String,
}

/// Legacy handling of one version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Legacy {
    /// `[features].<legacy_key>` aliases (`ALIASES`).
    #[serde(default)]
    pub feature_aliases: Vec<Alias>,
    /// Top-level legacy toggles (`experimental_use_unified_exec_tool`).
    #[serde(default)]
    pub top_level_aliases: Vec<Alias>,
    /// `[features]` keys accepted and ignored (`apply_map` arms with `continue`).
    #[serde(default)]
    pub ignored_feature_keys: Vec<String>,
    /// `[features]` keys that still apply but always log a deprecation notice.
    #[serde(default)]
    pub notice_feature_keys: Vec<String>,
}

/// `catalog/dist/index.json`: the manifest of a catalog tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// [`INDEX_FORMAT`].
    pub index_format: u32,
    /// [`CATALOG_FORMAT`] of every version file.
    pub catalog_format: u32,
    /// Increases whenever the listed files change; clients refuse an index whose
    /// serial is lower than one they already have (rollback protection).
    pub serial: u64,
    /// Upstream repository (`openai/codex`).
    pub repo: String,
    /// Highest stable version in [`Index::versions`].
    pub latest: String,
    /// Versions, sorted ascending (semver).
    pub versions: Vec<IndexVersion>,
    /// Every file a client loads, sorted by path, with its digest.
    pub files: Vec<IndexFile>,
}

impl Index {
    /// The manifest entry of `path`.
    pub fn file(&self, path: &str) -> Option<&IndexFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// A version listed in the index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexVersion {
    /// Codex version.
    pub version: String,
    /// Release tag.
    pub tag: String,
    /// Commit of the tag.
    pub commit: String,
    /// Alpha/beta.
    pub prerelease: bool,
    /// Catalog-relative path of the version file (`dist/codex-<version>.json`).
    pub path: String,
    /// Catalog-relative path of the change summary against the previous stable
    /// version (`dist/changes/codex-<version>.md`).
    pub changes: String,
}

/// A file listed in the index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexFile {
    /// Catalog-relative path with `/` separators.
    pub path: String,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// `catalog/dist/index.json.sig`: ed25519 signatures over the exact bytes of
/// `index.json` (domain-separated, see [`crate::integrity::signed_message`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureFile {
    /// [`SIGNATURE_FORMAT`].
    pub format: u32,
    /// One entry per signing key (several during a key rotation).
    pub signatures: Vec<SignatureEntry>,
}

/// One signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureEntry {
    /// Key id: first 16 hex digits of SHA-256 of the public key.
    pub key_id: String,
    /// Always `ed25519`.
    pub algorithm: String,
    /// Base64 (standard, padded) signature.
    pub signature: String,
}

/// File name of a version catalog inside `dist/`.
pub fn version_file_name(version: &str) -> String {
    format!("codex-{version}.json")
}

/// Catalog-relative path of a version catalog.
pub fn version_path(version: &str) -> String {
    format!("dist/{}", version_file_name(version))
}

/// Catalog-relative path of a version's change summary.
pub fn changes_path(version: &str) -> String {
    format!("dist/changes/codex-{version}.md")
}

/// Catalog-relative path of the index.
pub const INDEX_PATH: &str = "dist/index.json";

/// Catalog-relative path of the index signature.
pub const SIGNATURE_PATH: &str = "dist/index.json.sig";

/// Catalog-relative path of the curation file.
pub const CURATION_PATH: &str = "curation.toml";

/// Catalog-relative directory of the overlays.
pub const OVERLAY_DIR: &str = "overlay";

fn is_false(b: &bool) -> bool {
    !*b
}

/// A present field is `Some`, even when it is `null` (a missing field is `None`
/// through `#[serde(default)]`), so a raw `null` default survives a round trip.
fn present_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// Serialize a value as the generator writes catalog JSON: two-space indentation and
/// a trailing newline.
pub fn to_pretty_json<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut s = serde_json::to_string_pretty(value)?;
    s.push('\n');
    Ok(s)
}
