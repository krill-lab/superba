//! Planning an apply or a restore of the keys Superba owns (pure).
//!
//! - **Apply**: every owned path gets the value the configuration wants. A
//!   path Superba wrote before that someone changed since is *drift*: it is
//!   written only when the user takes it over, else it stays theirs (and
//!   stays drift: the next apply asks again).
//! - **Restore**: every path Superba owns gets back the value it had before
//!   Superba first took it over (absent when it was). Drift stays as it is.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use superba_core::claude::{ClaudeDesired, display_value, is_secret_path};
use superba_core::{ClaudeChange, ProfileId, Timestamp};

use super::json;

/// What Superba applied to `settings.json` (stored encrypted: it holds the
/// key Superba wrote and the values it replaced).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeState {
    /// Format.
    #[serde(default)]
    pub schema: u32,
    /// Counter of applies.
    #[serde(default)]
    pub generation: u64,
    /// When Superba last wrote.
    #[serde(default)]
    pub applied_at: Option<Timestamp>,
    /// The provider applied.
    #[serde(default)]
    pub provider: Option<ProfileId>,
    /// Owned path -> the value Superba wrote (`None` = it removed the key).
    #[serde(default)]
    pub written: BTreeMap<String, Option<Value>>,
    /// Owned path -> its value before Superba first took it over.
    #[serde(default)]
    pub baseline: BTreeMap<String, Option<Value>>,
    /// The file did not exist before Superba's first write.
    #[serde(default)]
    pub created_file: bool,
    /// The `env` block did not exist before Superba's first write.
    #[serde(default)]
    pub created_env: bool,
}

impl ClaudeState {
    /// Owned paths whose value in `current` is not what Superba wrote.
    pub fn drift(&self, current: &Value) -> Vec<String> {
        self.written
            .iter()
            .filter(|(p, w)| json::value_at(current, p) != **w)
            .map(|(p, _)| p.clone())
            .collect()
    }
}

/// One planned change of an owned path (with values).
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedChange {
    /// Dotted path.
    pub path: String,
    /// The value now.
    pub before: Option<Value>,
    /// The value after.
    pub after: Option<Value>,
    /// Changed by someone after Superba wrote it.
    pub drift: bool,
}

impl PlannedChange {
    /// For people: values on one line, keys masked.
    pub fn display(&self) -> ClaudeChange {
        ClaudeChange {
            path: self.path.clone(),
            before: self.before.as_ref().map(|v| display_value(&self.path, v)),
            after: self.after.as_ref().map(|v| display_value(&self.path, v)),
            drift: self.drift,
            secret: is_secret_path(&self.path),
        }
    }
}

/// The changes an apply of `desired` makes to `current`.
pub fn plan_apply(
    current: &Value,
    desired: &ClaudeDesired,
    state: Option<&ClaudeState>,
) -> Vec<PlannedChange> {
    let mut out = Vec::new();
    for (path, want) in desired.iter() {
        let before = json::value_at(current, path);
        let after = want.cloned();
        if before == after {
            continue;
        }
        let drift = state
            .and_then(|s| s.written.get(path))
            .is_some_and(|w| *w != before);
        out.push(PlannedChange {
            path: path.to_owned(),
            before,
            after,
            drift,
        });
    }
    out
}

/// The changes a restore makes to `current`.
pub fn plan_restore(current: &Value, state: &ClaudeState) -> Vec<PlannedChange> {
    let mut out = Vec::new();
    for (path, written) in &state.written {
        let before = json::value_at(current, path);
        let after = state.baseline.get(path).cloned().flatten();
        if before == after {
            continue;
        }
        out.push(PlannedChange {
            path: path.clone(),
            drift: before != *written,
            before,
            after,
        });
    }
    out
}

/// The result of applying planned changes.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The new object.
    pub value: Value,
    /// Paths written or removed.
    pub changed: Vec<String>,
    /// Drift left as it was.
    pub kept: Vec<String>,
    /// `env` was created.
    pub created_env: bool,
}

/// Apply `changes` to `current`; drift only with `take_over`.
pub fn apply_changes(
    current: &Value,
    changes: &[PlannedChange],
    take_over: bool,
) -> superba_core::Result<Applied> {
    let mut value = current.clone();
    let mut changed = Vec::new();
    let mut kept = Vec::new();
    let mut created_env = false;
    for c in changes {
        if c.drift && !take_over {
            kept.push(c.path.clone());
            continue;
        }
        created_env |= json::set_path(&mut value, &c.path, c.after.clone())?;
        changed.push(c.path.clone());
    }
    Ok(Applied {
        value,
        changed,
        kept,
        created_env,
    })
}

/// The state after an apply: owned paths and what they hold, baselines of
/// newly owned paths. Drift that was kept keeps its record, so it stays
/// drift (the next apply asks again, a restore leaves it).
pub fn state_after_apply(
    previous: Option<&ClaudeState>,
    before: &Value,
    after: &Value,
    desired: &ClaudeDesired,
    kept: &[String],
) -> ClaudeState {
    let mut s = previous.cloned().unwrap_or_default();
    s.schema = 1;
    for path in desired.paths() {
        if kept.iter().any(|k| k == path) {
            continue;
        }
        if !s.baseline.contains_key(path) {
            s.baseline
                .insert(path.to_owned(), json::value_at(before, path));
        }
        s.written
            .insert(path.to_owned(), json::value_at(after, path));
    }
    s
}
