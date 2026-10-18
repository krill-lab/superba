//! View model of the Claude Code settings page: every catalog entry (official
//! keys, the installed Claude Code's own keys, documented environment
//! variables) plus the keys of the file that no source knows, with the value
//! each has in `settings.json`, the keys Superba manages there, rail filters,
//! search and the pending edits that are written back into the file.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use superba_catalog::claude::{COMMON, ClaudeCatalog, ClaudeEntry, ClaudeGroup};
use superba_core::{CodexFileDoc, ProfileId};

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The catalog entry (or a file-only key).
    pub entry: ClaudeEntry,
    /// The value in the file now.
    pub value: Option<Value>,
    /// Superba manages the key (written by "Apply" on the connection page).
    pub owned: bool,
    /// Position in the curated "Common" list.
    pub common: Option<usize>,
}

impl Row {
    /// The path.
    pub fn path(&self) -> &str {
        &self.entry.path
    }

    /// The list title: the Chinese name in Chinese, else the key.
    pub fn title(&self, zh: bool) -> &str {
        if zh {
            self.entry.name_zh.as_deref().unwrap_or(self.entry.name())
        } else {
            self.entry.name()
        }
    }
}

/// Rail item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rail {
    /// The curated common keys.
    #[default]
    Common,
    /// Keys set in the file (or edited).
    Changed,
    /// One group.
    Group(ClaudeGroup),
}

/// Filter of the list.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Filter {
    /// Rail item.
    pub rail: Rail,
    /// Search text (searches every row).
    pub query: String,
    /// Show deprecated keys.
    pub deprecated: bool,
}

/// Pending edits: path -> new value (`None` = remove the key).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending(pub BTreeMap<String, Option<Value>>);

impl Pending {
    /// Number of edits.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// No edits.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The edit of `path`.
    pub fn get(&self, path: &str) -> Option<&Option<Value>> {
        self.0.get(path)
    }
}

/// The page model.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsModel {
    /// Rows: catalog order, then keys only the file has.
    pub rows: Vec<Row>,
    /// The parsed file (an object; empty when absent).
    pub root: Value,
    /// Why the file cannot be used (not JSON, not an object).
    pub error: Option<String>,
    /// sha256 of the file when it was read.
    pub sha256: Option<String>,
    /// The provider Superba applied (owner of the managed keys).
    pub owner: Option<ProfileId>,
}

impl SettingsModel {
    /// Build from the file as the service returned it.
    pub fn build(doc: &CodexFileDoc) -> Self {
        let catalog = ClaudeCatalog::builtin();
        let text = doc.text().unwrap_or_default();
        let (root, error) = if text.trim().is_empty() {
            (Value::Object(Default::default()), None)
        } else {
            match serde_json::from_str::<Value>(&text) {
                Ok(v @ Value::Object(_)) => (v, None),
                Ok(_) => (
                    Value::Object(Default::default()),
                    Some("settings.json is not a JSON object".to_owned()),
                ),
                Err(e) => (Value::Object(Default::default()), Some(e.to_string())),
            }
        };
        let owned: BTreeSet<&str> = doc.owned.iter().map(|o| o.path.as_str()).collect();
        let mut rows: Vec<Row> = catalog
            .entries()
            .iter()
            .map(|e| Row {
                value: value_at(&root, &e.path),
                owned: owned.contains(e.path.as_str()),
                common: COMMON.iter().position(|c| *c == e.path),
                entry: e.clone(),
            })
            .collect();
        if let Some(obj) = root.as_object() {
            for (k, v) in obj {
                if catalog.entry(k).is_none() {
                    rows.push(Row {
                        entry: ClaudeCatalog::file_entry(k),
                        value: Some(v.clone()),
                        owned: owned.contains(k.as_str()),
                        common: None,
                    });
                }
            }
            if let Some(env) = obj.get("env").and_then(Value::as_object) {
                for (k, v) in env {
                    let path = format!("env.{k}");
                    if catalog.entry(&path).is_none() {
                        rows.push(Row {
                            value: Some(v.clone()),
                            owned: owned.contains(path.as_str()),
                            entry: ClaudeCatalog::file_entry(&path),
                            common: None,
                        });
                    }
                }
            }
        }
        Self {
            rows,
            root,
            error,
            sha256: doc.sha256.clone(),
            owner: doc.profile.clone(),
        }
    }

    /// The row of `path`.
    pub fn row(&self, path: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.path() == path)
    }

    /// The value of `row` after `pending`.
    pub fn value_with<'a>(&self, row: &'a Row, pending: &'a Pending) -> Option<&'a Value> {
        match pending.get(row.path()) {
            Some(v) => v.as_ref(),
            None => row.value.as_ref(),
        }
    }

    /// Rows that pass `filter`, in rail order (Common: curated order;
    /// search: best matches first).
    pub fn listed<'a>(&'a self, filter: &Filter, pending: &Pending) -> Vec<&'a Row> {
        let q = filter.query.trim().to_lowercase();
        let mut out: Vec<(&Row, u8)> = self
            .rows
            .iter()
            .filter(|r| filter.deprecated || !r.entry.deprecated || r.value.is_some())
            .filter_map(|r| {
                if !q.is_empty() {
                    return rank(r, &q).map(|k| (r, k));
                }
                let set = r.value.is_some() || pending.get(r.path()).is_some();
                let pass = match filter.rail {
                    Rail::Common => r.common.is_some(),
                    Rail::Changed => set,
                    Rail::Group(g) => r.entry.group == g,
                };
                pass.then_some((r, 0))
            })
            .collect();
        if !q.is_empty() {
            out.sort_by_key(|(_, k)| *k);
        } else if filter.rail == Rail::Common {
            out.sort_by_key(|(r, _)| r.common);
        }
        out.into_iter().map(|(r, _)| r).collect()
    }

    /// Row counts per rail item (search ignored).
    pub fn counts(&self, filter: &Filter, pending: &Pending) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        let visible = |r: &&Row| filter.deprecated || !r.entry.deprecated || r.value.is_some();
        out.insert(
            "common".to_owned(),
            self.rows
                .iter()
                .filter(visible)
                .filter(|r| r.common.is_some())
                .count(),
        );
        out.insert(
            "changed".to_owned(),
            self.rows
                .iter()
                .filter(visible)
                .filter(|r| r.value.is_some() || pending.get(r.path()).is_some())
                .count(),
        );
        for g in ClaudeGroup::ALL {
            out.insert(
                g.id().to_owned(),
                self.rows
                    .iter()
                    .filter(visible)
                    .filter(|r| r.entry.group == g)
                    .count(),
            );
        }
        out
    }

    /// The file text with `pending` applied: every other key keeps its value
    /// and place; two-space indentation.
    pub fn render(&self, pending: &Pending) -> Result<String, String> {
        let mut root = self.root.clone();
        for (path, value) in &pending.0 {
            superba_switch::claude_files::json::set_path(&mut root, path, value.clone())
                .map_err(|e| e.to_string())?;
        }
        superba_switch::claude_files::json::drop_empty_env(&mut root);
        String::from_utf8(superba_switch::claude_files::json::print(&root))
            .map_err(|e| e.to_string())
    }
}

/// The value at a dotted path (`env.X` or a top-level key).
pub fn value_at(root: &Value, path: &str) -> Option<Value> {
    superba_core::claude::get_path(root, path).cloned()
}

/// Search rank (lower first): exact key, key prefix, name, key contains,
/// explanation only; `None` = no match.
fn rank(r: &Row, q: &str) -> Option<u8> {
    let path = r.path().to_lowercase();
    let name = r.entry.name().to_lowercase();
    if path == q || name == q {
        Some(0)
    } else if name.starts_with(q) || path.starts_with(q) {
        Some(1)
    } else if r
        .entry
        .name_zh
        .as_deref()
        .is_some_and(|n| n.to_lowercase().contains(q))
    {
        Some(2)
    } else if path.contains(q) {
        Some(3)
    } else if r.entry.haystack().any(|t| t.to_lowercase().contains(q)) {
        Some(4)
    } else {
        None
    }
}
