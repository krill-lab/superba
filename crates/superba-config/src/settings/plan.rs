//! Per-home filtering and validation of a profile's Codex settings.

use std::collections::BTreeSet;
use std::sync::Arc;

use superba_core::{
    CatalogKey, CatalogMatch, CatalogSnapshot, CatalogSource, CodexSettings, ConsumerSupport,
    FeatureStage, KeySupport, LegacyKind, LocalizedText, Notice, SettingEntry, SettingKind,
    SettingValue, SettingsConsumer, SettingsPlan, SkipReason, SkippedSetting, TargetId,
    is_valid_setting_path, reserved_owner,
};

use super::migrate;
use super::values::read_config_values;

/// One Codex home and what reads it.
#[derive(Clone, Copy)]
pub struct HomeInput<'a> {
    /// The home (`local` or `ssh:<host>`).
    pub home: &'a TargetId,
    /// Codex installs reading this home (app-bundled Codex and CLI for the local
    /// home; the remote Codex for a host). Empty = nothing known: nothing is
    /// filtered by version.
    pub consumers: &'a [SettingsConsumer],
    /// Current `config.toml` (shape conflicts, legacy keys); `None` = absent.
    pub config_toml: Option<&'a [u8]>,
    /// Paths Superba wrote last time in this home. They are released or rewritten
    /// by the next apply, so a value there never counts as a shape conflict.
    pub owned_paths: &'a [String],
    /// The catalog.
    pub catalog: &'a dyn CatalogSource,
}

impl<'a> HomeInput<'a> {
    /// A home with nothing known but its consumers and the catalog.
    pub fn new(
        home: &'a TargetId,
        consumers: &'a [SettingsConsumer],
        catalog: &'a dyn CatalogSource,
    ) -> Self {
        Self {
            home,
            consumers,
            config_toml: None,
            owned_paths: &[],
            catalog,
        }
    }

    /// Set the current `config.toml`.
    pub fn config_toml(mut self, bytes: Option<&'a [u8]>) -> Self {
        self.config_toml = bytes;
        self
    }

    /// Set the paths Superba owns in this home.
    pub fn owned_paths(mut self, paths: &'a [String]) -> Self {
        self.owned_paths = paths;
        self
    }

    /// Each consumer with its catalog snapshot.
    pub(crate) fn snapshots(&self) -> Vec<(&'a SettingsConsumer, Option<Arc<CatalogSnapshot>>)> {
        self.consumers
            .iter()
            .map(|c| (c, self.catalog.snapshot(c.version.as_ref())))
            .collect()
    }
}

impl std::fmt::Debug for HomeInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeInput")
            .field("home", self.home)
            .field("consumers", &self.consumers)
            .field("owned_paths", &self.owned_paths)
            .finish_non_exhaustive()
    }
}

/// A consumer with its catalog snapshot.
type Seen<'a> = (&'a SettingsConsumer, Option<Arc<CatalogSnapshot>>);

fn consumer_label(c: &SettingsConsumer) -> String {
    match &c.version {
        Some(v) => format!("{} {v}", c.label),
        None => c.label.clone(),
    }
}

/// Raw support of `path` in one snapshot (before accounting for inexact matches).
fn raw_support(snapshot: &CatalogSnapshot, path: &str) -> KeySupport {
    if let Some(rule) = snapshot.legacy_rule(path) {
        return match rule.kind {
            LegacyKind::Ignored => KeySupport::Removed,
            _ => KeySupport::Legacy,
        };
    }
    let key = snapshot.key(path);
    if let Some(feature) = snapshot.feature_for_path(path)
        && (key.is_some() || path == feature.config_path)
    {
        return match feature.stage {
            FeatureStage::Removed => KeySupport::Removed,
            FeatureStage::Deprecated => KeySupport::Deprecated,
            _ if key.is_some_and(|k| k.deprecated_hint) => KeySupport::Deprecated,
            _ => KeySupport::Supported,
        };
    }
    match key {
        Some(k) if k.deprecated_hint => KeySupport::Deprecated,
        Some(_) => KeySupport::Supported,
        None if snapshot.open_ancestor(path).is_some() => KeySupport::Supported,
        None => KeySupport::Missing,
    }
}

fn support_in(seen: &Seen<'_>, path: &str) -> ConsumerSupport {
    let (consumer, snapshot) = seen;
    let (support, matched) = match snapshot {
        None => (KeySupport::Unknown, None),
        Some(s) => {
            let raw = raw_support(s, path);
            let support = match (&s.matched, raw) {
                (CatalogMatch::VersionUnknown, _) => KeySupport::Unknown,
                // A newer Codex than the snapshot may know keys the snapshot lacks.
                (CatalogMatch::Approximate | CatalogMatch::Outdated, KeySupport::Missing) => {
                    KeySupport::Unknown
                }
                // An older Codex than the catalog floor may lack keys the floor has.
                (CatalogMatch::BelowFloor, KeySupport::Missing) => KeySupport::Missing,
                (CatalogMatch::BelowFloor, _) => KeySupport::Unknown,
                (_, raw) => raw,
            };
            (support, Some(s.matched.clone()))
        }
    };
    ConsumerSupport {
        consumer: consumer.consumer.clone(),
        label: consumer.label.clone(),
        version: consumer.version.clone(),
        support,
        catalog: matched,
    }
}

/// Availability of `path` for each consumer (settings page badges).
pub fn key_support(
    path: &str,
    consumers: &[SettingsConsumer],
    catalog: &dyn CatalogSource,
) -> Vec<ConsumerSupport> {
    consumers
        .iter()
        .map(|c| support_in(&(c, catalog.snapshot(c.version.as_ref())), path))
        .collect()
}

/// Check `value` against a catalog key; `Err` carries what is expected
/// (`boolean`, `one of: "a", "b"`, `integer >= 1`).
pub fn validate_value(key: &CatalogKey, value: &SettingValue) -> Result<(), String> {
    let range = |n: f64, what: &str| -> Result<(), String> {
        let below = key.minimum.is_some_and(|m| n < m);
        let above = key.maximum.is_some_and(|m| n > m);
        if below || above {
            let mut bounds = Vec::new();
            if let Some(m) = key.minimum {
                bounds.push(format!(">= {m}"));
            }
            if let Some(m) = key.maximum {
                bounds.push(format!("<= {m}"));
            }
            return Err(format!("{what} {}", bounds.join(" and ")));
        }
        Ok(())
    };
    match key.kind {
        SettingKind::Boolean => match value {
            SettingValue::Bool(_) => Ok(()),
            _ => Err("true or false".into()),
        },
        SettingKind::String => match value {
            SettingValue::String(s) => {
                let n = s.chars().count() as u64;
                if key.min_length.is_some_and(|m| n < m) || key.max_length.is_some_and(|m| n > m) {
                    let min = key.min_length.unwrap_or(0);
                    return Err(match key.max_length {
                        Some(max) => format!("a string of {min} to {max} characters"),
                        None => format!("a string of at least {min} characters"),
                    });
                }
                Ok(())
            }
            _ => Err("a string".into()),
        },
        SettingKind::Integer => match value {
            SettingValue::Integer(i) => range(*i as f64, "an integer"),
            _ => Err("an integer".into()),
        },
        SettingKind::Number => match value.as_f64() {
            Some(n) => range(n, "a number"),
            None => Err("a number".into()),
        },
        SettingKind::Enum => match value {
            SettingValue::String(s) => {
                if key.enum_values.is_empty() || key.enum_values.iter().any(|e| &e.value == s) {
                    Ok(())
                } else {
                    let options: Vec<String> = key
                        .enum_values
                        .iter()
                        .map(|e| format!("{:?}", e.value))
                        .collect();
                    Err(format!("one of: {}", options.join(", ")))
                }
            }
            SettingValue::Table(_) if key.enum_has_table_form => Ok(()),
            _ => Err("one of the listed values".into()),
        },
        SettingKind::BoolOrTable => match value {
            SettingValue::Bool(_) | SettingValue::Table(_) => Ok(()),
            _ => Err("true, false or a table".into()),
        },
        SettingKind::Array => match value {
            SettingValue::Array(_) => Ok(()),
            _ => Err("an array".into()),
        },
        SettingKind::Table | SettingKind::Map => match value {
            SettingValue::Table(_) => Ok(()),
            _ => Err("a table".into()),
        },
        _ => Ok(()),
    }
}

/// Check `value` against the catalog key of `path` of every Codex reading the
/// home that knows the key, newest first: a value only a newer Codex accepts
/// would make an older one refuse the whole `config.toml`. The error is the
/// expected form; a consumer other than the newest one is named in it.
fn validate_for_all(seen: &[Seen<'_>], path: &str, value: &SettingValue) -> Result<(), String> {
    let mut known: Vec<(&SettingsConsumer, &CatalogSnapshot, &CatalogKey)> = seen
        .iter()
        .filter_map(|(c, s)| {
            let s = s.as_deref()?;
            Some((*c, s, s.key(path)?))
        })
        .collect();
    known.sort_by(|a, b| b.1.codex_version.cmp(&a.1.codex_version));
    for (i, (consumer, _, key)) in known.iter().enumerate() {
        if let Err(expected) = validate_value(key, value) {
            return Err(if i == 0 {
                expected
            } else {
                format!("{expected} (for {})", consumer_label(consumer))
            });
        }
    }
    Ok(())
}

/// Proper ancestors of `path`, nearest first (`a.b.c` -> `a.b`, `a`).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('.').rev().map(move |(i, _)| &path[..i])
}

/// Filter and validate `settings` for one Codex home.
///
/// Every managed path ends up either in [`SettingsPlan::apply`] or in
/// [`SettingsPlan::skipped`] with a reason. A key is skipped when the path is
/// malformed or reserved, when it overlaps a managed parent, when no Codex
/// reading the home knows it (or every one dropped it), when the value does not
/// fit the key, or when `config.toml` holds a different kind of value there.
/// Removals ([`SettingEntry::Unset`]) are never filtered by version: removing a
/// key cannot hurt. Unknown versions never filter anything.
pub fn plan_settings(settings: &CodexSettings, input: &HomeInput<'_>) -> SettingsPlan {
    let seen = input.snapshots();
    let mut plan = SettingsPlan {
        home: Some(input.home.clone()),
        consumers: input.consumers.to_vec(),
        ..SettingsPlan::default()
    };
    let wanted = settings;
    let config = read_config_values(input.config_toml).ok();
    let config_tables = config_tables(input.config_toml);
    let managed: BTreeSet<&str> = wanted.paths().collect();
    let owned: BTreeSet<&str> = input.owned_paths.iter().map(String::as_str).collect();

    for (path, entry) in wanted.iter() {
        let skip = |reason: SkipReason| SkippedSetting {
            path: path.to_owned(),
            entry: entry.clone(),
            reason,
        };
        if !is_valid_setting_path(path) {
            plan.skipped.push(skip(SkipReason::InvalidPath));
            continue;
        }
        if let Some(owner) = reserved_owner(path) {
            plan.skipped.push(skip(SkipReason::Reserved { owner }));
            continue;
        }
        if let Some(parent) = ancestors(path).find(|a| managed.contains(a)) {
            plan.skipped.push(skip(SkipReason::Overlap {
                other: parent.to_owned(),
            }));
            continue;
        }
        let support: Vec<ConsumerSupport> = seen.iter().map(|s| support_in(s, path)).collect();
        if !support.is_empty() {
            plan.support.insert(path.to_owned(), support.clone());
        }
        let value = match entry {
            SettingEntry::Unset => {
                plan.apply.insert_unchecked(path, entry.clone());
                continue;
            }
            SettingEntry::Set(v) => v,
        };
        if !support.is_empty() && !support.iter().any(|s| s.support.may_apply()) {
            let reason = if support.iter().any(|s| s.support == KeySupport::Removed) {
                SkipReason::Removed
            } else {
                SkipReason::NotSupported {
                    consumers: input.consumers.iter().map(consumer_label).collect(),
                }
            };
            plan.skipped.push(skip(reason));
            continue;
        }
        if let Err(expected) = validate_for_all(&seen, path, value) {
            plan.skipped
                .push(skip(SkipReason::InvalidValue { expected }));
            continue;
        }
        // A plain value at a parent (or a table where a value goes) belongs to
        // the user unless Superba wrote it.
        let parent_value = config
            .as_ref()
            .and_then(|c| ancestors(path).find(|a| c.contains_key(*a) && !owned.contains(a)));
        let table_here = !matches!(value, SettingValue::Table(_))
            && config_tables.contains(path)
            && !owned.contains(path);
        if let Some(parent) = parent_value {
            plan.skipped.push(skip(SkipReason::ShapeConflict {
                parent: parent.to_owned(),
            }));
            continue;
        }
        if table_here {
            plan.skipped.push(skip(SkipReason::ShapeConflict {
                parent: path.to_owned(),
            }));
            continue;
        }
        let missing: Vec<String> = support
            .iter()
            .filter(|s| !s.support.may_apply())
            .map(|s| match &s.version {
                Some(v) => format!("{} {v}", s.label),
                None => s.label.clone(),
            })
            .collect();
        if !missing.is_empty() {
            plan.notes.push(Notice::info(
                LocalizedText::new(
                    "config.settings.partial_support",
                    "{key} has no effect for {consumers}; it is written for the others.",
                )
                .arg("key", path)
                .arg("consumers", missing.join(", ")),
            ));
        }
        plan.apply.insert_unchecked(path, entry.clone());
    }

    plan.notes.extend(catalog_notes(&seen));
    let doc = input
        .config_toml
        .and_then(|b| crate::toml_keys::ConfigDoc::parse(b).ok());
    plan.migrations = migrate::find_in(wanted, doc.as_ref(), &seen);
    plan.migrated = migrate::applied_in(&plan.apply, &seen);
    plan
}

/// Notes about consumers whose version the catalog does not match exactly.
fn catalog_notes(seen: &[Seen<'_>]) -> Vec<Notice> {
    let mut out = Vec::new();
    for (consumer, snapshot) in seen {
        let label = consumer_label(consumer);
        let Some(snapshot) = snapshot else {
            out.push(Notice::info(
                LocalizedText::new(
                    "config.settings.no_catalog",
                    "No settings catalog is available; settings for {consumer} are not checked.",
                )
                .arg("consumer", label),
            ));
            continue;
        };
        let catalog = snapshot.codex_version.to_string();
        let note = match snapshot.matched {
            CatalogMatch::Exact => continue,
            CatalogMatch::Approximate => LocalizedText::new(
                "config.settings.catalog_approximate",
                "The settings catalog has no entry for {consumer}; Codex {catalog} is used instead.",
            ),
            CatalogMatch::Outdated => LocalizedText::new(
                "config.settings.catalog_outdated",
                "{consumer} is newer than the settings catalog (Codex {catalog}); newer keys are not checked.",
            ),
            CatalogMatch::BelowFloor => LocalizedText::new(
                "config.settings.catalog_below_floor",
                "{consumer} is older than the settings catalog (from Codex {catalog}); keys are not checked.",
            ),
            _ => LocalizedText::new(
                "config.settings.version_unknown",
                "The Codex version of {consumer} is unknown; settings are written without a version check.",
            ),
        };
        out.push(Notice::info(
            note.arg("consumer", label).arg("catalog", catalog),
        ));
    }
    out
}

/// Dotted paths of every table in `config.toml` (standard, inline, dotted).
fn config_tables(config_toml: Option<&[u8]>) -> BTreeSet<String> {
    fn walk(prefix: &str, table: &toml::Table, out: &mut BTreeSet<String>) {
        for (k, v) in table {
            if let toml::Value::Table(t) = v {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                walk(&path, t, out);
                out.insert(path);
            }
        }
    }
    let mut out = BTreeSet::new();
    let parsed = config_toml
        .map(|b| b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b))
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|t| toml::from_str::<toml::Table>(t).ok());
    if let Some(t) = parsed {
        walk("", &t, &mut out);
    }
    out
}
