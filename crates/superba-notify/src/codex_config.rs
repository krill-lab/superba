//! Superba's hooks in Codex's `config.toml` (pure text edits with
//! `toml_edit`, so comments and the user's layout stay as they are).
//!
//! - `notify`: Superba's program, chaining the user's own program when there
//!   was one ([`crate::hooks::codex_notify_argv`]); removing it puts the user's
//!   program back.
//! - `[[hooks.PermissionRequest]]` with one handler running `spb notify hook
//!   codex` (opt-in): Codex runs it only after the user trusted it in Codex;
//!   [`CodexHookFacts::permission_trusted`] reports whether Codex recorded a
//!   trust (`hooks.state."<file>:permission_request:<i>:<j>".trusted_hash`).

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value, value};

use crate::hooks::{
    NotifyShape, classify_notify, codex_notify_argv, hook_command, is_our_hook_command,
};

/// What Codex's `config.toml` holds of Superba's hooks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexHookFacts {
    /// The `notify` value.
    pub notify: NotifyShape,
    /// Superba's `PermissionRequest` handler is there.
    pub permission_hook: bool,
    /// Codex recorded a trust for it.
    pub permission_trusted: bool,
}

/// A `config.toml` that cannot be edited (it does not parse, or `notify` /
/// `hooks` have an unexpected type).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

fn parse(text: &str) -> Result<DocumentMut, ConfigError> {
    text.parse::<DocumentMut>()
        .map_err(|e| ConfigError(format!("config.toml does not parse: {e}")))
}

fn notify_value(doc: &DocumentMut) -> Result<Vec<String>, ConfigError> {
    match doc.get("notify") {
        None => Ok(Vec::new()),
        Some(item) => {
            let arr = item
                .as_array()
                .ok_or_else(|| ConfigError("notify is not a list of strings".into()))?;
            arr.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ConfigError("notify is not a list of strings".into()))
                })
                .collect()
        }
    }
}

/// Read the facts. `config_path` is the file's path as Codex names it in hook
/// keys (the absolute path of `config.toml`).
pub fn read(text: &str, config_path: &str) -> Result<CodexHookFacts, ConfigError> {
    let doc = parse(text)?;
    let notify = classify_notify(&notify_value(&doc)?);
    let positions = our_permission_handlers(&doc);
    let state = doc
        .get("hooks")
        .and_then(Item::as_table_like)
        .and_then(|h| h.get("state"))
        .and_then(Item::as_table_like);
    let permission_trusted = positions.iter().any(|(i, j)| {
        let key = format!("{config_path}:permission_request:{i}:{j}");
        state
            .and_then(|s| s.get(&key))
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("trusted_hash"))
            .and_then(Item::as_str)
            .is_some_and(|h| !h.is_empty())
    });
    Ok(CodexHookFacts {
        notify,
        permission_hook: !positions.is_empty(),
        permission_trusted,
    })
}

/// Install (or remove) Superba's `notify`. `Ok(None)` = nothing to change.
pub fn set_notify(text: &str, program: &str, install: bool) -> Result<Option<String>, ConfigError> {
    let mut doc = parse(text)?;
    let current = notify_value(&doc)?;
    let desired: Option<Vec<String>> = match (install, classify_notify(&current)) {
        (true, NotifyShape::Missing) => Some(codex_notify_argv(program, &[])),
        (true, NotifyShape::Foreign(v)) => Some(codex_notify_argv(program, &v)),
        (true, NotifyShape::Ours { chained, .. }) => Some(codex_notify_argv(program, &chained)),
        (false, NotifyShape::Ours { chained, .. }) => (!chained.is_empty()).then_some(chained),
        (false, _) => return Ok(None),
    };
    match desired {
        Some(v) if v == current => return Ok(None),
        Some(v) => {
            let mut arr = toml_edit::Array::new();
            for s in v {
                arr.push(s);
            }
            // Keep the key where it is when it exists (with its comments).
            match doc.get_mut("notify") {
                Some(item) => *item = value(arr),
                None => {
                    doc.insert("notify", value(arr));
                }
            }
        }
        None => {
            doc.remove("notify");
        }
    }
    Ok(Some(doc.to_string()))
}

/// `(group index, handler index)` of Superba's `PermissionRequest` handlers.
fn our_permission_handlers(doc: &DocumentMut) -> Vec<(usize, usize)> {
    let Some(groups) = doc
        .get("hooks")
        .and_then(Item::as_table_like)
        .and_then(|h| h.get("PermissionRequest"))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut visit = |i: usize, handlers: Vec<Option<String>>| {
        for (j, cmd) in handlers.into_iter().enumerate() {
            if cmd.as_deref().is_some_and(is_our_hook_command) {
                out.push((i, j));
            }
        }
    };
    if let Some(aot) = groups.as_array_of_tables() {
        for (i, group) in aot.iter().enumerate() {
            visit(i, handler_commands(group.get("hooks")));
        }
    } else if let Some(arr) = groups.as_array() {
        for (i, group) in arr.iter().enumerate() {
            let hooks = group.as_inline_table().and_then(|t| t.get("hooks"));
            let cmds = hooks
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|h| {
                            h.as_inline_table()
                                .and_then(|t| t.get("command"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            visit(i, cmds);
        }
    }
    out
}

fn handler_commands(hooks: Option<&Item>) -> Vec<Option<String>> {
    match hooks {
        Some(Item::ArrayOfTables(aot)) => aot
            .iter()
            .map(|t| t.get("command").and_then(Item::as_str).map(str::to_owned))
            .collect(),
        Some(Item::Value(Value::Array(arr))) => arr
            .iter()
            .map(|h| {
                h.as_inline_table()
                    .and_then(|t| t.get("command"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Install (or remove) Superba's `PermissionRequest` hook. `Ok(None)` =
/// nothing to change. Removal only touches Superba's handlers (and the groups
/// they leave empty).
pub fn set_permission_hook(
    text: &str,
    program: &str,
    install: bool,
    asynchronous: bool,
) -> Result<Option<String>, ConfigError> {
    let mut doc = parse(text)?;
    let ours = our_permission_handlers(&doc);
    if install {
        if !ours.is_empty() {
            return Ok(None);
        }
        let hooks = doc
            .entry("hooks")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| ConfigError("hooks is not a table".into()))?;
        let groups = hooks
            .entry("PermissionRequest")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| {
                ConfigError("hooks.PermissionRequest is not an array of tables".into())
            })?;
        let mut handler = Table::new();
        handler.insert("type", value("command"));
        handler.insert(
            "command",
            value(hook_command(program, superba_core::notify::harness::CODEX)),
        );
        handler.insert("timeout", value(30));
        // In the background, so the approval prompt is never held up by a
        // send: Codex 0.150+ runs async PermissionRequest hooks (checked with
        // 0.150.1 and 0.163); 0.140 skips them ("async hooks are not
        // supported yet"), so the caller asks for it only when every local
        // Codex is new enough.
        if asynchronous {
            handler.insert("async", value(true));
        }
        let mut handlers = ArrayOfTables::new();
        handlers.push(handler);
        let mut group = Table::new();
        group.insert("hooks", Item::ArrayOfTables(handlers));
        groups.push(group);
        return Ok(Some(doc.to_string()));
    }
    if ours.is_empty() {
        return Ok(None);
    }
    let hooks = doc
        .get_mut("hooks")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ConfigError("hooks is not a table".into()))?;
    let Some(groups) = hooks
        .get_mut("PermissionRequest")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return Err(ConfigError(
            "hooks.PermissionRequest is written inline; edit it in config.toml".into(),
        ));
    };
    let mut empty_groups = Vec::new();
    for (i, group) in groups.iter_mut().enumerate() {
        if let Some(Item::ArrayOfTables(handlers)) = group.get_mut("hooks") {
            let keep: Vec<Table> = handlers
                .iter()
                .filter(|t| {
                    !t.get("command")
                        .and_then(Item::as_str)
                        .is_some_and(is_our_hook_command)
                })
                .cloned()
                .collect();
            let mut fresh = ArrayOfTables::new();
            for t in keep {
                fresh.push(t);
            }
            if fresh.is_empty() {
                empty_groups.push(i);
            }
            *handlers = fresh;
        }
    }
    for i in empty_groups.into_iter().rev() {
        groups.remove(i);
    }
    if groups.is_empty() {
        hooks.remove("PermissionRequest");
    }
    if hooks.is_empty() {
        doc.remove("hooks");
    }
    Ok(Some(doc.to_string()))
}

/// Record Codex's trust for hooks: `hooks.state."<key>".trusted_hash =
/// "<hash>"` for each `(key, hash)` (the key and the hash as Codex's
/// `hooks/list` reports them; this is what Codex's own `/hooks` review
/// writes). `Ok(None)` = every entry is already recorded.
pub fn set_trusted(
    text: &str,
    entries: &[(String, String)],
) -> Result<Option<String>, ConfigError> {
    let mut doc = parse(text)?;
    let mut changed = false;
    for (key, hash) in entries {
        let hooks = doc
            .entry("hooks")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| ConfigError("hooks is not a table".into()))?;
        let state = hooks
            .entry("state")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| ConfigError("hooks.state is not a table".into()))?;
        let entry = state
            .entry(key)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigError(format!("hooks.state.{key} is not a table")))?;
        if entry.get("trusted_hash").and_then(Item::as_str) != Some(hash.as_str()) {
            entry.insert("trusted_hash", value(hash.as_str()));
            changed = true;
        }
    }
    Ok(changed.then(|| doc.to_string()))
}
