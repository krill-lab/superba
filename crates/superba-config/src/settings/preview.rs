//! The diff preview: `(key, from, to, target, reason)` rows for a computed plan.

use superba_core::codex::config_keys as k;
use superba_core::{ChangeReason, KeyChange, SettingChange, SettingEntry, SettingsPlan, TargetId};

/// The Codex home a preview is for.
#[derive(Clone, Copy, Debug)]
pub struct PreviewTarget<'a> {
    /// `local` or `ssh:<host>`.
    pub id: &'a TargetId,
    /// Display label ("This computer", host alias).
    pub label: &'a str,
}

/// Annotate the owned-key changes of an [`superba_core::ApplyPlan`] (or a
/// remote plan) with the reason for each: the connection (routing keys), the
/// profile's model, one of its settings (written, reset to the default, or part of
/// an accepted legacy migration), or a key the profile no longer manages, whose
/// original value comes back. `restore` marks restores of the original
/// configuration or of a point in time, where every change is a restore.
pub fn preview_changes(
    key_changes: &[KeyChange],
    settings: Option<&SettingsPlan>,
    restore: bool,
    target: PreviewTarget<'_>,
) -> Vec<SettingChange> {
    key_changes
        .iter()
        .map(|c| SettingChange {
            path: c.path.clone(),
            from: c.before.clone(),
            to: c.after.clone(),
            target: target.id.clone(),
            target_label: target.label.to_owned(),
            reason: reason(&c.path, c.after.is_some(), settings, restore),
        })
        .collect()
}

fn reason(path: &str, set: bool, settings: Option<&SettingsPlan>, restore: bool) -> ChangeReason {
    if restore {
        return ChangeReason::Restore;
    }
    if [k::MODEL_PROVIDER, k::OPENAI_BASE_URL, k::CHATGPT_BASE_URL].contains(&path) {
        return ChangeReason::Connection;
    }
    if let Some(legacy) = settings.and_then(|s| s.migrated.get(path)) {
        return ChangeReason::Migration {
            legacy: legacy.clone(),
        };
    }
    match settings.and_then(|s| s.apply.get(path)) {
        Some(SettingEntry::Set(_)) => ChangeReason::Setting,
        Some(SettingEntry::Unset) => ChangeReason::Reset,
        None if [
            k::MODEL,
            k::MODEL_REASONING_EFFORT,
            k::MODEL_CATALOG_JSON,
            k::MODEL_CONTEXT_WINDOW,
            k::MODEL_AUTO_COMPACT_TOKEN_LIMIT,
            k::SERVICE_TIER,
        ]
        .contains(&path)
            && set =>
        {
            ChangeReason::Model
        }
        // v1 recommended keys written without a settings plan.
        None if set => ChangeReason::Setting,
        None => ChangeReason::Released,
    }
}
