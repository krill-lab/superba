//! Unified profile building blocks (v2 decision 3) and the settings engine's
//! contract types.
//!
//! - [`CodexSettings`]: the catalog keys a profile manages.
//! - [`AuthChoice`] (Auto / PAT / API key) and its resolution per line with a
//!   reason ([`AuthDecision`]).
//! - The catalog data the engine needs ([`CatalogSnapshot`]) and
//!   [`CatalogSource`], implemented by the catalog crate.
//! - Settings plans per Codex home ([`SettingsPlan`]), skip reasons, availability
//!   per consumer, legacy migrations, the diff preview ([`SettingChange`]) and the
//!   `codex features list` cross-check ([`FeaturesList`], [`FeatureCrossCheck`]).
//!
//! The engine itself (filtering, validation, migrations, preview) lives in
//! `superba-config::settings`; `superba-config`'s renderer applies
//! [`crate::ResolvedProfile::settings`] as owned keys.

mod auth_choice;
mod catalog_view;
mod report;
mod value;

pub use auth_choice::{AuthChoice, AuthContext, AuthDecision, AuthReason, pat_unavailable_summary};
pub use catalog_view::{
    CatalogFeature, CatalogKey, CatalogMatch, CatalogSnapshot, CatalogSource, FeatureStage,
    LegacyKind, LegacyRule, SettingEnumValue, SettingKind, StaticCatalog,
};
pub use report::{
    ChangeReason, ConsumerSupport, FeatureCrossCheck, FeatureListEntry, FeatureMismatch,
    FeaturesList, KeySupport, LegacyMigration, MigrationSource, SettingChange, SettingsConsumer,
    SettingsPlan, SkipReason, SkippedSetting,
};
pub use value::{
    CodexSettings, ReservedOwner, SettingEntry, SettingValue, is_valid_setting_path, reserved_owner,
};
