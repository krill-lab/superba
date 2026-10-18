//! What Superba applied to a harness's files and planning an apply or a
//! restore of the entries it owns (pure; the same rules as Claude Code's
//! `settings.json` writer on the `cc1` branch, generalised to several files
//! and to entries inside lists):
//!
//! - **Apply**: every wanted entry gets its value. An entry Superba wrote
//!   before that someone changed since is *drift*: written only when the
//!   user takes it over, else it stays theirs (and stays drift). Entries
//!   Superba owned that the settings no longer want (a model taken out, a
//!   file no longer targeted) get their value from before Superba back.
//! - **Restore**: every owned entry gets back its value from before Superba
//!   first took it over (absent when it was). Drift stays as it is.
//!
//! After both, containers Superba created that hold nothing any more are
//! taken out again, and a file Superba created that holds nothing is
//! removed (a file that was an empty `[]` / `{}` gets that text back).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use superba_core::harness::{HarnessChange, HarnessProtocol};
use superba_core::{ProfileId, Timestamp};

use super::doc::{Format, HarnessFile};
use super::slot::{self, SlotPath};

/// One entry Superba owns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnedSlot {
    /// Where it is.
    pub path: SlotPath,
    /// The value Superba wrote (`None` = it removed it).
    #[serde(default)]
    pub written: Option<Value>,
    /// Its value before Superba first took it over.
    #[serde(default)]
    pub baseline: Option<Value>,
}

/// What Superba owns in one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileState {
    /// The file's path.
    pub path: PathBuf,
    /// Its format.
    pub format: Format,
    /// It holds a key.
    #[serde(default)]
    pub holds_key: bool,
    /// The file did not exist before Superba's first write.
    #[serde(default)]
    pub created_file: bool,
    /// The file existed but held nothing (`[]`, `{}`): its text, written back
    /// when Superba's entries leave it empty again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_original: Option<String>,
    /// Owned entries by [`SlotPath::display`].
    #[serde(default)]
    pub slots: BTreeMap<String, OwnedSlot>,
    /// Containers Superba created on the way to its entries.
    #[serde(default)]
    pub created: BTreeMap<String, SlotPath>,
    /// Directories Superba created for the file (deepest first): removed
    /// again with the file when they are empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_dirs: Vec<PathBuf>,
}

impl FileState {
    /// The file this state is about.
    pub fn file(&self, key: &str) -> HarnessFile {
        HarnessFile::new(key, self.path.clone(), self.format, self.holds_key)
    }
}

/// What Superba applied to one harness (stored encrypted: it holds the key
/// Superba wrote and the values it replaced).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessState {
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
    /// The protocol applied.
    #[serde(default)]
    pub protocol: Option<HarnessProtocol>,
    /// Per file key.
    #[serde(default)]
    pub files: BTreeMap<String, FileState>,
}

impl HarnessState {
    /// Owned entries (`<file> <slot>`) whose value in `current` (file key ->
    /// meaning) is not what Superba wrote.
    pub fn drift(&self, current: &BTreeMap<String, Value>) -> Vec<String> {
        let mut out = Vec::new();
        for (key, fs) in &self.files {
            let doc = current.get(key).cloned().unwrap_or(Value::Null);
            for (name, s) in &fs.slots {
                if slot::get(&doc, &s.path).cloned() != s.written {
                    out.push(format!("{key} {name}"));
                }
            }
        }
        out
    }

    /// Every owned entry (`<file> <slot>`).
    pub fn owned(&self) -> Vec<String> {
        self.files
            .iter()
            .flat_map(|(k, fs)| fs.slots.keys().map(move |s| format!("{k} {s}")))
            .collect()
    }
}

/// The entries one file should hold (`None` = absent), in order.
#[derive(Clone, Debug)]
pub struct DesiredFile {
    /// The file.
    pub file: HarnessFile,
    /// Entries and their values.
    pub slots: Vec<(SlotPath, Option<Value>)>,
}

/// One planned change of an owned entry (with values).
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedChange {
    /// File key.
    pub file: String,
    /// Where.
    pub path: SlotPath,
    /// The value now.
    pub before: Option<Value>,
    /// The value after.
    pub after: Option<Value>,
    /// Changed by someone after Superba wrote it.
    pub drift: bool,
}

impl PlannedChange {
    /// `<file> <slot>`.
    pub fn id(&self) -> String {
        format!("{} {}", self.file, self.path.display())
    }

    /// For people: values on one line, keys masked. `secret_slot` = the
    /// entry itself is a key (a `.env` variable).
    pub fn display(&self, secret_slot: bool) -> HarnessChange {
        let show = |v: &Option<Value>| v.as_ref().map(|v| slot::display_value(v, secret_slot));
        let before = show(&self.before);
        let after = show(&self.after);
        let secret = secret_slot
            || before.as_ref().is_some_and(|(_, s)| *s)
            || after.as_ref().is_some_and(|(_, s)| *s);
        HarnessChange {
            file: self.file.clone(),
            slot: self.path.display(),
            before: before.map(|(t, _)| t),
            after: after.map(|(t, _)| t),
            drift: self.drift,
            secret,
        }
    }
}

fn owned<'a>(
    state: Option<&'a HarnessState>,
    file: &str,
    path: &SlotPath,
) -> Option<&'a OwnedSlot> {
    state?.files.get(file)?.slots.get(&path.display())
}

/// The changes an apply of `desired` makes to `current` (file key ->
/// meaning): wanted entries, and owned entries no longer wanted going back
/// to their value from before Superba.
pub fn plan_apply(
    current: &BTreeMap<String, Value>,
    desired: &[DesiredFile],
    state: Option<&HarnessState>,
) -> Vec<PlannedChange> {
    let mut out = Vec::new();
    let null = Value::Null;
    for d in desired {
        let key = &d.file.key;
        let doc = current.get(key).unwrap_or(&null);
        for (path, want) in &d.slots {
            let before = slot::get(doc, path).cloned();
            if before == *want {
                continue;
            }
            let drift = owned(state, key, path).is_some_and(|o| o.written != before);
            out.push(PlannedChange {
                file: key.clone(),
                path: path.clone(),
                before,
                after: want.clone(),
                drift,
            });
        }
    }
    if let Some(s) = state {
        for (key, fs) in &s.files {
            let wanted = desired.iter().find(|d| d.file.key == *key);
            let doc = current.get(key).unwrap_or(&null);
            for o in fs.slots.values() {
                if wanted.is_some_and(|d| d.slots.iter().any(|(p, _)| *p == o.path)) {
                    continue;
                }
                let before = slot::get(doc, &o.path).cloned();
                if before == o.baseline {
                    continue;
                }
                out.push(PlannedChange {
                    file: key.clone(),
                    path: o.path.clone(),
                    drift: before != o.written,
                    before,
                    after: o.baseline.clone(),
                });
            }
        }
    }
    out
}

/// The changes a restore makes to `current`.
pub fn plan_restore(current: &BTreeMap<String, Value>, state: &HarnessState) -> Vec<PlannedChange> {
    let mut out = Vec::new();
    let null = Value::Null;
    for (key, fs) in &state.files {
        let doc = current.get(key).unwrap_or(&null);
        for o in fs.slots.values() {
            let before = slot::get(doc, &o.path).cloned();
            if before == o.baseline {
                continue;
            }
            out.push(PlannedChange {
                file: key.clone(),
                path: o.path.clone(),
                drift: before != o.written,
                before,
                after: o.baseline.clone(),
            });
        }
    }
    out
}

/// The state after an apply. `before` / `after` are the meanings of every
/// file before and after (file key -> meaning), `existed` the files that
/// existed before, `kept` the drift left as it was (`<file> <slot>`).
pub fn state_after_apply(
    previous: Option<&HarnessState>,
    desired: &[DesiredFile],
    before: &BTreeMap<String, Value>,
    after: &BTreeMap<String, Value>,
    before_text: &BTreeMap<String, Option<String>>,
    created_dirs: &BTreeMap<String, Vec<PathBuf>>,
    kept: &[String],
) -> HarnessState {
    let mut s = previous.cloned().unwrap_or_default();
    s.schema = 1;
    let null = Value::Null;
    for d in desired {
        let key = &d.file.key;
        let doc_before = before.get(key).unwrap_or(&null);
        let doc_after = after.get(key).unwrap_or(&null);
        let fs = s.files.entry(key.clone()).or_insert_with(|| {
            let text = before_text.get(key).cloned().flatten();
            FileState {
                path: d.file.path.clone(),
                format: d.file.format,
                holds_key: d.file.holds_key,
                created_file: text.is_none(),
                empty_original: text.filter(|_| slot::is_empty_doc(doc_before)),
                slots: BTreeMap::new(),
                created: BTreeMap::new(),
                created_dirs: created_dirs.get(key).cloned().unwrap_or_default(),
            }
        });
        fs.path = d.file.path.clone();
        fs.holds_key |= d.file.holds_key;
        for (path, want) in &d.slots {
            let name = path.display();
            if kept.iter().any(|k| *k == format!("{key} {name}")) {
                continue;
            }
            if want.is_some() {
                for c in slot::missing_containers(doc_before, path) {
                    fs.created.entry(c.display()).or_insert(c);
                }
            }
            let entry = fs.slots.entry(name).or_insert_with(|| OwnedSlot {
                path: path.clone(),
                written: None,
                baseline: slot::get(doc_before, path).cloned(),
            });
            entry.written = slot::get(doc_after, path).cloned();
        }
    }
    // Owned entries no longer wanted went back to their baseline: released.
    for (key, fs) in s.files.iter_mut() {
        let wanted = desired.iter().find(|d| d.file.key == *key);
        fs.slots.retain(|name, o| {
            wanted.is_some_and(|d| d.slots.iter().any(|(p, _)| *p == o.path))
                || kept.iter().any(|k| *k == format!("{key} {name}"))
        });
    }
    for (key, fs) in s.files.iter_mut() {
        let doc = after.get(key).unwrap_or(&null);
        fs.created.retain(|_, p| slot::get(doc, p).is_some());
    }
    s.files.retain(|_, fs| !fs.slots.is_empty());
    s
}

/// The containers Superba created in a file, deepest first (the order to
/// take empty ones out in).
pub fn created_deepest_first(fs: &FileState) -> Vec<SlotPath> {
    let mut paths: Vec<SlotPath> = fs.created.values().cloned().collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
    paths
}
