//! Service-level views of a profile's Codex settings (the settings page and
//! `spb settings`): catalog rows with the current, default and managed values,
//! edits of the managed settings, and the settings part of a computed plan.

use serde::{Deserialize, Serialize};

use crate::CodexVersion;
use crate::i18n::Notice;
use crate::ids::ProfileId;
use crate::profile::Profile;
use crate::settings::{
    CatalogFeature, CatalogKey, CatalogMatch, ConsumerSupport, SettingChange, SettingEntry,
    SettingValue, SettingsConsumer, SettingsPlan,
};

/// The settings part of a computed plan ([`crate::SwitchService::plan_settings`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSettings {
    /// One settings plan per Codex home written (local first, then remote hosts).
    #[serde(default)]
    pub targets: Vec<SettingsPlan>,
    /// The diff preview: every owned key that changes, with target and reason.
    #[serde(default)]
    pub changes: Vec<SettingChange>,
}

/// What [`crate::SwitchService::settings_view`] lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsViewRequest {
    /// Profile whose managed settings are shown (`None` = the applied profile,
    /// else the last selected one).
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// Also list keys of features under development, deprecated or removed.
    #[serde(default)]
    pub advanced: bool,
    /// Only these paths (exact or prefix match on a `.` boundary); empty = all.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// One setting row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingRow {
    /// Dotted path.
    pub path: String,
    /// Catalog data of the key (kind, official English description, default).
    #[serde(default)]
    pub key: Option<CatalogKey>,
    /// The feature behind a `features.*` key (stage, default).
    #[serde(default)]
    pub feature: Option<CatalogFeature>,
    /// Human-written Chinese explanation, when one exists.
    #[serde(default)]
    pub zh: Option<String>,
    /// Value in the local `config.toml` (TOML representation), if set.
    #[serde(default)]
    pub current: Option<String>,
    /// What the profile manages for this key.
    #[serde(default)]
    pub managed: Option<SettingEntry>,
    /// Availability per local consumer.
    #[serde(default)]
    pub support: Vec<ConsumerSupport>,
    /// Shown only in the Advanced view (stage or deprecation).
    #[serde(default)]
    pub advanced: bool,
    /// In the curated "Common" list.
    #[serde(default)]
    pub common: bool,
    /// Availability per remote host the app is connected to, from each host's
    /// probed Codex version ([`crate::ConsumerKind::Remote`] consumers): what a
    /// sync to that host would skip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_support: Vec<ConsumerSupport>,
}

/// Rows of the settings page for one profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    /// Profile.
    pub profile_id: ProfileId,
    /// Local consumers the availability refers to.
    #[serde(default)]
    pub consumers: Vec<SettingsConsumer>,
    /// Codex version the rows come from.
    #[serde(default)]
    pub catalog_version: Option<CodexVersion>,
    /// How that version matched the installed Codex.
    #[serde(default)]
    pub catalog_match: Option<CatalogMatch>,
    /// Rows, sorted by path.
    #[serde(default)]
    pub rows: Vec<SettingRow>,
    /// Notes (catalog older than the installed Codex, versions unknown).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// An edit of a profile's managed settings ([`crate::SwitchService::edit_settings`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsEdit {
    /// Profile (`None` = the applied profile, else the last selected one).
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// Keys to manage with a value.
    #[serde(default)]
    pub set: Vec<(String, SettingValue)>,
    /// Keys to manage as removed (Codex's default applies).
    #[serde(default)]
    pub reset: Vec<String>,
    /// Keys to stop managing (the value from before Superba comes back).
    #[serde(default)]
    pub forget: Vec<String>,
    /// Legacy paths whose offered migration is accepted.
    #[serde(default)]
    pub accept_migrations: Vec<String>,
    /// Store a value even when the catalog does not know the key or rejects
    /// the value.
    #[serde(default)]
    pub force: bool,
}

/// Result of [`crate::SwitchService::edit_settings`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsEditReport {
    /// The saved profile.
    pub profile: Profile,
    /// Notes (unknown keys stored with `force`, keys some consumer lacks).
    #[serde(default)]
    pub notes: Vec<Notice>,
}
