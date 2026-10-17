//! Legacy keys: find them in `config.toml` and in a profile, offer their
//! migration, apply accepted ones as profile edits.
//!
//! A migration never edits `config.toml` directly. Accepting one adds two entries
//! to the profile's settings (remove the legacy key, set its replacement), so both
//! keys become owned by Superba and come back as they were when the profile stops
//! managing them or the original configuration is restored.

use std::collections::BTreeMap;
use std::sync::Arc;

use superba_core::{
    CatalogSnapshot, CodexSettings, LegacyKind, LegacyMigration, LegacyRule, MigrationSource,
    Result, SettingEntry, SettingValue, SettingsConsumer,
};

use super::plan::HomeInput;
use crate::toml_keys::ConfigDoc;

/// Legacy rules of every consumer's snapshot, the newest snapshot's rule winning
/// for a path.
fn rules(snaps: &[(&SettingsConsumer, Option<Arc<CatalogSnapshot>>)]) -> Vec<LegacyRule> {
    let mut known: Vec<&CatalogSnapshot> = snaps.iter().filter_map(|(_, s)| s.as_deref()).collect();
    known.sort_by(|a, b| b.codex_version.cmp(&a.codex_version));
    let mut out: BTreeMap<&str, &LegacyRule> = BTreeMap::new();
    for s in known {
        for r in &s.legacy {
            out.entry(r.path.as_str()).or_insert(r);
        }
    }
    out.into_values().cloned().collect()
}

/// The replacement of a legacy value: `Some((canonical, value))`, `Some((None,
/// None))` for an ignored key, `None` when the rule does not apply to this value.
fn replacement(
    rule: &LegacyRule,
    legacy: &SettingValue,
) -> Option<(Option<String>, Option<SettingValue>)> {
    match &rule.kind {
        LegacyKind::Alias { canonical } => Some((Some(canonical.clone()), Some(legacy.clone()))),
        LegacyKind::Replace {
            canonical,
            when,
            value,
        } => (when.as_ref().is_none_or(|w| w == legacy))
            .then(|| (Some(canonical.clone()), Some(value.clone()))),
        LegacyKind::Ignored => Some((None, None)),
        _ => None,
    }
}

pub(crate) fn find_in(
    settings: &CodexSettings,
    doc: Option<&ConfigDoc>,
    snaps: &[(&SettingsConsumer, Option<Arc<CatalogSnapshot>>)],
) -> Vec<LegacyMigration> {
    let config_value = |path: &str| -> Option<SettingValue> {
        doc.and_then(|d| d.get_value(path))
            .and_then(|v| SettingValue::from_toml(&v))
    };
    let mut out = Vec::new();
    for rule in rules(snaps) {
        let (legacy, source) = match settings.get(&rule.path) {
            Some(SettingEntry::Set(v)) => (v.clone(), MigrationSource::Profile),
            // Already being removed by the profile.
            Some(SettingEntry::Unset) => continue,
            None => match config_value(&rule.path) {
                Some(v) => (v, MigrationSource::Config),
                None => continue,
            },
        };
        let Some((canonical, canonical_value)) = replacement(&rule, &legacy) else {
            continue;
        };
        let conflict = match (&canonical, &canonical_value) {
            (Some(c), Some(want)) => {
                let current = match settings.get(c) {
                    Some(SettingEntry::Set(v)) => Some(v.clone()),
                    Some(SettingEntry::Unset) => None,
                    None => config_value(c),
                };
                current.is_some_and(|cur| &cur != want)
            }
            _ => false,
        };
        out.push(LegacyMigration {
            legacy_path: rule.path.clone(),
            legacy_value: Some(legacy.to_toml_repr()),
            canonical,
            canonical_value,
            source,
            conflict,
        });
    }
    out
}

/// Entries of `apply` that carry out a migration: the removal of a legacy key and
/// the setting of its replacement (path -> legacy path).
pub(crate) fn applied_in(
    apply: &CodexSettings,
    snaps: &[(&SettingsConsumer, Option<Arc<CatalogSnapshot>>)],
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for rule in rules(snaps) {
        if apply.get(&rule.path) != Some(&SettingEntry::Unset) {
            continue;
        }
        out.insert(rule.path.clone(), rule.path.clone());
        let canonical = match &rule.kind {
            LegacyKind::Alias { canonical } | LegacyKind::Replace { canonical, .. } => canonical,
            _ => continue,
        };
        if matches!(apply.get(canonical), Some(SettingEntry::Set(_))) {
            out.insert(canonical.clone(), rule.path.clone());
        }
    }
    out
}

/// Legacy keys in the home's `config.toml` or in `settings` that a migration can
/// replace or remove, using the legacy rules of every consumer's catalog snapshot.
/// Nothing is changed; see [`apply_migrations`].
pub fn find_migrations(settings: &CodexSettings, input: &HomeInput<'_>) -> Vec<LegacyMigration> {
    let doc = input.config_toml.and_then(|b| ConfigDoc::parse(b).ok());
    find_in(settings, doc.as_ref(), &input.snapshots())
}

/// Add the edits of accepted migrations to a profile's settings
/// ([`LegacyMigration::edits`]). The next switch writes them and shows them in the
/// preview as migrations.
pub fn apply_migrations(settings: &mut CodexSettings, accepted: &[LegacyMigration]) -> Result<()> {
    let mut next = settings.clone();
    for m in accepted {
        for (path, entry) in m.edits() {
            next.insert(&path, entry)?;
        }
    }
    *settings = next;
    Ok(())
}
