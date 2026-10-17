//! The settings engine (v2 decision 3): which of a profile's Codex settings are
//! written into a Codex home, checked against the settings catalog for every Codex
//! that reads that home.
//!
//! - [`plan_settings`]: filter and validate a profile's [`superba_core::CodexSettings`] for one
//!   home (the app's bundled Codex and the CLI share the local home; a remote host
//!   has its own) and explain every skipped key. The result's
//!   [`superba_core::SettingsPlan::apply`] goes into [`superba_core::ResolvedProfile::settings`],
//!   which the renderer writes as owned keys: Superba owns exactly the keys a
//!   profile manages and restores the original value of a key it stops managing.
//! - [`key_support`]: availability of any catalog key per consumer (settings page).
//! - [`find_migrations`] / [`apply_migrations`]: legacy keys (aliases, ignored
//!   keys, hand-written replacements) found in `config.toml` or in the profile,
//!   offered for preview and applied as profile edits.
//! - [`preview_changes`]: the diff preview `(key, from, to, target, reason)` of a
//!   computed [`superba_core::ApplyPlan`].
//! - [`cross_check_features`]: `codex features list` of a consumer against the
//!   catalog; runtime data wins on a mismatch.
//! - [`read_config_values`]: current values of `config.toml` by dotted path.
//!
//! Everything here is pure: callers pass the catalog
//! ([`superba_core::CatalogSource`]), the
//! consumers' versions and the current file bytes.

mod features;
mod migrate;
mod plan;
mod preview;
mod values;

pub use features::cross_check_features;
pub use migrate::{apply_migrations, find_migrations};
pub use plan::{HomeInput, key_support, plan_settings, validate_value};
pub use preview::{PreviewTarget, preview_changes};
pub use values::read_config_values;
