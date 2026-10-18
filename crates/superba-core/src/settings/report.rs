//! Results of the settings engine: which settings are written for a Codex home,
//! which are skipped and why, availability per consumer, legacy migrations, the
//! diff preview, and the `codex features list` cross-check.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::codex::CodexVersion;
use crate::i18n::{LocalizedText, Notice};
use crate::ids::TargetId;
use crate::plan::ConsumerKind;
use crate::settings::catalog_view::{CatalogMatch, FeatureStage};
use crate::settings::value::{CodexSettings, ReservedOwner, SettingEntry, SettingValue};

/// A Codex install that reads a configuration: the app's bundled Codex and the
/// CLI share the local home; each remote host has its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsConsumer {
    /// Consumer group.
    pub consumer: ConsumerKind,
    /// Display label ("ChatGPT app", "CLI", host alias).
    pub label: String,
    /// `codex --version`, when known.
    #[serde(default)]
    pub version: Option<CodexVersion>,
}

impl SettingsConsumer {
    /// A consumer.
    pub fn new(
        consumer: ConsumerKind,
        label: impl Into<String>,
        version: Option<CodexVersion>,
    ) -> Self {
        Self {
            consumer,
            label: label.into(),
            version,
        }
    }
}

/// Whether one consumer's Codex knows a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeySupport {
    /// The key exists in this version.
    Supported,
    /// The key exists but is deprecated (or its feature is).
    Deprecated,
    /// The feature exists but was removed (no effect).
    Removed,
    /// The key is a legacy alias this version still understands.
    Legacy,
    /// The key does not exist in this version (ignored; an error with
    /// `--strict-config`).
    Missing,
    /// The version is unknown or the catalog has no data.
    Unknown,
}

impl KeySupport {
    /// Writing the key has an effect for this consumer (or might).
    pub fn may_apply(self) -> bool {
        matches!(
            self,
            KeySupport::Supported
                | KeySupport::Deprecated
                | KeySupport::Legacy
                | KeySupport::Unknown
        )
    }

    /// Badge text.
    pub fn text(self) -> LocalizedText {
        match self {
            KeySupport::Supported => LocalizedText::new("settings.support.supported", "Supported"),
            KeySupport::Deprecated => {
                LocalizedText::new("settings.support.deprecated", "Deprecated")
            }
            KeySupport::Removed => LocalizedText::new("settings.support.removed", "No effect"),
            KeySupport::Legacy => LocalizedText::new("settings.support.legacy", "Old name"),
            KeySupport::Missing => {
                LocalizedText::new("settings.support.missing", "Not in this version")
            }
            KeySupport::Unknown => LocalizedText::new("settings.support.unknown", "Unknown"),
        }
    }
}

/// [`KeySupport`] of one key for one consumer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerSupport {
    /// Consumer group.
    pub consumer: ConsumerKind,
    /// Display label.
    pub label: String,
    /// Codex version, when known.
    #[serde(default)]
    pub version: Option<CodexVersion>,
    /// Support.
    pub support: KeySupport,
    /// How the catalog snapshot matched the version (`None` = no catalog data).
    #[serde(default)]
    pub catalog: Option<CatalogMatch>,
}

/// Why a setting is not written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SkipReason {
    /// Not a dotted path of bare TOML keys.
    InvalidPath,
    /// Managed elsewhere (connection, model defaults, Superba).
    Reserved {
        /// Who manages it.
        owner: ReservedOwner,
    },
    /// No Codex reading this home knows the key.
    NotSupported {
        /// `label version` of each consumer, for the message.
        consumers: Vec<String>,
    },
    /// The feature was removed in every Codex reading this home.
    Removed,
    /// The value does not fit the key.
    InvalidValue {
        /// What the catalog expects (`boolean`, `one of: a, b`, `>= 1`).
        expected: String,
    },
    /// `config.toml` holds a different kind of value at `parent` (a plain value
    /// where a table is needed, or a table where a value would go), so the key
    /// cannot be written without rewriting the user's own value.
    ShapeConflict {
        /// The conflicting path (the key itself or one of its parents).
        parent: String,
    },
    /// Another managed path is a parent or child of this one.
    Overlap {
        /// The other path.
        other: String,
    },
}

impl SkipReason {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            SkipReason::InvalidPath => LocalizedText::new(
                "settings.skip.invalid_path",
                "Not a valid config.toml key path.",
            ),
            SkipReason::Reserved { owner } => match owner {
                ReservedOwner::Connection => LocalizedText::new(
                    "settings.skip.reserved_connection",
                    "Set from the profile's site, line and auth instead.",
                ),
                ReservedOwner::ModelDefaults => LocalizedText::new(
                    "settings.skip.reserved_model",
                    "Set from the profile's model instead.",
                ),
                _ => LocalizedText::new(
                    "settings.skip.reserved_superba",
                    "Superba cannot manage this key without breaking switching.",
                ),
            },
            SkipReason::NotSupported { consumers } => {
                LocalizedText::new("settings.skip.not_supported", "Not known to {consumers}.")
                    .arg("consumers", consumers.join(", "))
            }
            SkipReason::Removed => LocalizedText::new(
                "settings.skip.removed",
                "Removed from Codex; it has no effect.",
            ),
            SkipReason::InvalidValue { expected } => LocalizedText::new(
                "settings.skip.invalid_value",
                "Invalid value; expected {expected}.",
            )
            .arg("expected", expected),
            SkipReason::ShapeConflict { parent } => LocalizedText::new(
                "settings.skip.shape_conflict",
                "config.toml has a different kind of value at {parent}; change it there first.",
            )
            .arg("parent", parent),
            SkipReason::Overlap { other } => LocalizedText::new(
                "settings.skip.overlap",
                "Overlaps with the managed key {other}.",
            )
            .arg("other", other),
        }
    }
}

/// A setting the engine does not write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedSetting {
    /// Path.
    pub path: String,
    /// What the profile asked for.
    pub entry: SettingEntry,
    /// Why it is skipped.
    pub reason: SkipReason,
}

/// Where a legacy key was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationSource {
    /// In the live `config.toml` (the user's own key).
    Config,
    /// In the profile's settings.
    Profile,
}

/// An offered legacy-key migration (preview first, applied only when accepted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyMigration {
    /// Legacy path.
    pub legacy_path: String,
    /// Its current value (TOML representation).
    #[serde(default)]
    pub legacy_value: Option<String>,
    /// Replacement path; `None` when the key is simply ignored by Codex.
    #[serde(default)]
    pub canonical: Option<String>,
    /// Value to write at `canonical`.
    #[serde(default)]
    pub canonical_value: Option<SettingValue>,
    /// Where it was found.
    pub source: MigrationSource,
    /// `canonical` is already set (in the config or the profile) to a different
    /// value; applying keeps that value and only removes the legacy key.
    #[serde(default)]
    pub conflict: bool,
}

impl LegacyMigration {
    /// The settings edits that apply this migration: remove the legacy key and,
    /// unless it conflicts, set the replacement.
    pub fn edits(&self) -> Vec<(String, SettingEntry)> {
        let mut out = vec![(self.legacy_path.clone(), SettingEntry::Unset)];
        if !self.conflict
            && let (Some(path), Some(value)) = (&self.canonical, &self.canonical_value)
        {
            out.push((path.clone(), SettingEntry::Set(value.clone())));
        }
        out
    }

    /// Display text ("features.collab -> features.multi_agent").
    pub fn text(&self) -> LocalizedText {
        match &self.canonical {
            Some(c) => LocalizedText::new("settings.migration.rename", "{legacy} -> {canonical}")
                .arg("legacy", &self.legacy_path)
                .arg("canonical", c),
            None => LocalizedText::new(
                "settings.migration.remove",
                "{legacy} is ignored by Codex and can be removed",
            )
            .arg("legacy", &self.legacy_path),
        }
    }
}

/// The settings to write into one Codex home (input of
/// [`crate::ResolvedProfile::settings`]) and everything the preview shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsPlan {
    /// The home (`local` or `ssh:<host>`).
    #[serde(default)]
    pub home: Option<TargetId>,
    /// Consumers the plan was filtered for.
    #[serde(default)]
    pub consumers: Vec<SettingsConsumer>,
    /// Settings to write.
    #[serde(default)]
    pub apply: CodexSettings,
    /// Settings not written, with reasons.
    #[serde(default)]
    pub skipped: Vec<SkippedSetting>,
    /// Availability per path and consumer (for written and skipped paths).
    #[serde(default)]
    pub support: BTreeMap<String, Vec<ConsumerSupport>>,
    /// Legacy migrations on offer (found in `config.toml` or the profile, not yet
    /// accepted).
    #[serde(default)]
    pub migrations: Vec<LegacyMigration>,
    /// Entries of [`Self::apply`] that carry out an accepted migration: path ->
    /// legacy path (both the removal of the legacy key and the replacement).
    #[serde(default)]
    pub migrated: BTreeMap<String, String>,
    /// Notes (catalog older than a Codex, versions unknown, partial support).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// Why a key changes in the diff preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeReason {
    /// Routing for the profile's site, line and auth.
    Connection,
    /// The profile's model defaults.
    Model,
    /// The model Codex runs with now, written so that a change of
    /// authentication or the router does not change it (user bug 58).
    KeptModel,
    /// A setting of the profile.
    Setting,
    /// A setting reset to Codex's default (key removed).
    Reset,
    /// No longer managed by the profile: the value from before Superba took the
    /// key over comes back.
    Released,
    /// A legacy key migration.
    Migration {
        /// Legacy path.
        legacy: String,
    },
    /// Restoring the original configuration or a point in time.
    Restore,
}

impl ChangeReason {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            ChangeReason::Connection => {
                LocalizedText::new("settings.change.connection", "Site, line and auth")
            }
            ChangeReason::Model => LocalizedText::new("settings.change.model", "Profile model"),
            ChangeReason::KeptModel => {
                LocalizedText::new("settings.change.kept_model", "Keeps the model in use")
            }
            ChangeReason::Setting => {
                LocalizedText::new("settings.change.setting", "Profile setting")
            }
            ChangeReason::Reset => {
                LocalizedText::new("settings.change.reset", "Reset to Codex default")
            }
            ChangeReason::Released => LocalizedText::new(
                "settings.change.released",
                "No longer managed: original value restored",
            ),
            ChangeReason::Migration { legacy } => {
                LocalizedText::new("settings.change.migration", "Replaces old key {legacy}")
                    .arg("legacy", legacy)
            }
            ChangeReason::Restore => LocalizedText::new("settings.change.restore", "Restore"),
        }
    }
}

/// One row of the diff preview: `(key, from, to, target, reason)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChange {
    /// Dotted path.
    pub path: String,
    /// Value now (TOML representation, `None` = absent).
    #[serde(default)]
    pub from: Option<String>,
    /// Value after (TOML representation, `None` = removed).
    #[serde(default)]
    pub to: Option<String>,
    /// The Codex home.
    pub target: TargetId,
    /// Display label of the target ("This computer", host alias).
    pub target_label: String,
    /// Why.
    pub reason: ChangeReason,
}

// ---------------------------------------------------------------------------
// `codex features list`
// ---------------------------------------------------------------------------

/// One row of `codex features list` (`key  stage  enabled`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureListEntry {
    /// Registry key.
    pub key: String,
    /// Parsed stage, `None` when the text is not a known stage.
    #[serde(default)]
    pub stage: Option<FeatureStage>,
    /// Stage column as printed (`under development`).
    pub stage_text: String,
    /// Effective state after config, aliases and dependencies.
    pub enabled: bool,
}

/// Parsed `codex features list` output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturesList {
    /// Rows, in output order.
    pub entries: Vec<FeatureListEntry>,
    /// Non-empty lines that are not rows (headers, warnings), kept for diagnostics.
    #[serde(default)]
    pub other_lines: Vec<String>,
}

impl FeaturesList {
    /// Parse the output tolerantly: rows are `<key> <stage...> <enabled>`, where
    /// the stage may contain spaces (`under development`) and `enabled` is
    /// `true`/`false` (also `on`/`off`, `yes`/`no`, `enabled`/`disabled`). ANSI
    /// colour codes, header lines and other text are skipped.
    pub fn parse(output: &str) -> Self {
        let mut list = FeaturesList::default();
        for raw in output.lines() {
            let line = strip_ansi(raw);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_row(line) {
                Some(e) => list.entries.push(e),
                None => list.other_lines.push(line.to_owned()),
            }
        }
        list
    }

    /// The row for `key`.
    pub fn get(&self, key: &str) -> Option<&FeatureListEntry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

fn parse_row(line: &str) -> Option<FeatureListEntry> {
    let (rest, last) = line.rsplit_once(char::is_whitespace)?;
    let enabled = match last.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "enabled" => true,
        "false" | "off" | "no" | "disabled" => false,
        _ => return None,
    };
    let rest = rest.trim();
    let (key, stage_text) = match rest.split_once(char::is_whitespace) {
        Some((k, s)) => (k, s.trim()),
        None => (rest, ""),
    };
    let valid_key = !key.is_empty()
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        });
    if !valid_key {
        return None;
    }
    Some(FeatureListEntry {
        key: key.to_owned(),
        stage: FeatureStage::parse(stage_text),
        stage_text: stage_text.to_owned(),
        enabled,
    })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A disagreement between the catalog and a running Codex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeatureMismatch {
    /// Codex has a feature the catalog does not know (catalog older than Codex).
    UnknownToCatalog {
        /// Key.
        key: String,
        /// Runtime stage text.
        stage_text: String,
    },
    /// The catalog lists a feature this Codex does not print.
    MissingAtRuntime {
        /// Key.
        key: String,
    },
    /// Different stage.
    Stage {
        /// Key.
        key: String,
        /// Catalog stage.
        catalog: FeatureStage,
        /// Runtime stage text.
        runtime: String,
    },
    /// Not configured, yet the effective state differs from the catalog default.
    Default {
        /// Key.
        key: String,
        /// Catalog default.
        catalog: bool,
        /// Effective state.
        runtime: bool,
    },
}

impl FeatureMismatch {
    /// The feature key.
    pub fn key(&self) -> &str {
        match self {
            FeatureMismatch::UnknownToCatalog { key, .. }
            | FeatureMismatch::MissingAtRuntime { key }
            | FeatureMismatch::Stage { key, .. }
            | FeatureMismatch::Default { key, .. } => key,
        }
    }
}

/// Result of comparing `codex features list` of one consumer with the catalog.
/// On a mismatch the runtime data wins and the UI says the catalog is older than
/// this Codex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureCrossCheck {
    /// Consumer.
    pub consumer: SettingsConsumer,
    /// Disagreements.
    pub mismatches: Vec<FeatureMismatch>,
    /// Effective state of every feature Codex printed (runtime wins).
    pub effective: BTreeMap<String, bool>,
    /// Stage of every feature Codex printed, where parseable (runtime wins).
    pub stages: BTreeMap<String, FeatureStage>,
    /// Codex knows features the catalog does not: show "catalog older than this
    /// Codex".
    pub catalog_outdated: bool,
}
