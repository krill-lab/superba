//! Unified profile building blocks (v2 decision 3) and the settings engine's
//! contract types.
//!
//! - [`CodexSettings`]: the catalog keys a profile manages.
//! - [`key_auth`]: whether a key-based profile's API key can be used on a
//!   line, with a reason ([`AuthDecision`]).
//! - The catalog data the engine needs ([`CatalogSnapshot`]) and
//!   [`CatalogSource`], implemented by the catalog crate.
//! - Settings plans per Codex home ([`SettingsPlan`]), skip reasons, availability
//!   per consumer, legacy migrations, the diff preview ([`SettingChange`]) and the
//!   `codex features list` cross-check ([`FeaturesList`], [`FeatureCrossCheck`]).
//!
//! The engine itself (filtering, validation, migrations, preview) lives in
//! `superba-config::settings`; `superba-config`'s renderer applies
//! [`crate::ResolvedProfile::settings`] as owned keys.

mod catalog_view;
mod key_auth;
mod report;
mod value;

pub use catalog_view::{
    CatalogFeature, CatalogKey, CatalogMatch, CatalogSnapshot, CatalogSource, FeatureStage,
    LegacyKind, LegacyRule, SettingEnumValue, SettingKind, StaticCatalog,
};
pub use key_auth::{AuthDecision, AuthReason, key_auth};
pub use report::{
    ChangeReason, ConsumerSupport, FeatureCrossCheck, FeatureListEntry, FeatureMismatch,
    FeaturesList, KeySupport, LegacyMigration, MigrationSource, SettingChange, SettingsConsumer,
    SettingsPlan, SkipReason, SkippedSetting,
};
pub use value::{
    CodexSettings, ReservedOwner, SettingEntry, SettingValue, is_valid_setting_path, reserved_owner,
};
