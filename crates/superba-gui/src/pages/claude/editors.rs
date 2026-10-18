//! Value editors of the Claude Code settings page: scalars by kind (switch,
//! choice, text, number, list), and real editors for structured keys:
//! `env` (variables with catalog help), `hooks` (event → matcher →
//! commands), maps of switches (`enabledPlugins`), `modelSettings` (per-model
//! effort and compaction), objects with known fields (`permissions`,
//! `statusLine`, `sandbox`, `autoMode`, ...) and JSON for everything else.
//!
//! Every editor gets the current value and returns the new one when the user
//! changed something: `Some(Some(v))` = set, `Some(None)` = remove the key.

use std::collections::HashMap;

use egui::{RichText, TextEdit, Ui, vec2};
use serde_json::{Map, Value, json};
use superba_catalog::claude::{
    ClaudeCatalog, ClaudeEditor, ClaudeEntry, ClaudeKind, kind_of_schema,
};

use crate::design::fonts::{Weight, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Button, Dropdown, IconButton, Segmented, Text};
use crate::pages::Cx;

/// A change: `Some(Some(v))` set, `Some(None)` remove, `None` nothing.
pub type Change = Option<Option<Value>>;

/// Text buffers of the editors, by an id built from the path.
pub type Buffers = HashMap<String, String>;

/// The editor for `entry` with `current` (its value after pending edits).
pub fn editor(
    ui: &mut Ui,
    cx: &Cx<'_>,
    entry: &ClaudeEntry,
    current: Option<&Value>,
    buffers: &mut Buffers,
) -> Change {
    match entry.editor {
        ClaudeEditor::Value => scalar(
            ui,
            cx,
            &entry.path,
            &entry.kind,
            current,
            buffers,
            entry.env,
        ),
        ClaudeEditor::Env => env_editor(ui, cx, current, buffers),
        ClaudeEditor::Hooks => hooks_editor(ui, cx, current, buffers),
        ClaudeEditor::BoolMap => bool_map(ui, cx, &entry.path, current, buffers),
        ClaudeEditor::ModelSettings => model_settings(ui, cx, current, buffers),
        ClaudeEditor::Object => match entry.schema.as_ref() {
            Some(s) => object(ui, cx, &entry.path, s, current, buffers, 0),
            None => json_editor(ui, cx, &entry.path, current, buffers),
        },
        ClaudeEditor::Json => json_editor(ui, cx, &entry.path, current, buffers),
    }
}

fn unset_label(cx: &Cx<'_>) -> String {
    cx.tr.t("claude.settings.value.unset")
}

/// A value for people (one line).
pub fn shown(cx: &Cx<'_>, v: &Value) -> String {
    match v {
        Value::Bool(true) => cx.tr.t("claude.settings.value.on"),
        Value::Bool(false) => cx.tr.t("claude.settings.value.off"),
        Value::String(s) => s.clone(),
        Value::Array(a) => cx.tr.f("claude.settings.value.items", &[("n", &a.len())]),
        Value::Object(o) => cx.tr.f("claude.settings.value.fields", &[("n", &o.len())]),
        other => other.to_string(),
    }
}

/// A scalar or list editor by kind. `env` values are always text.
pub fn scalar(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: &str,
    kind: &ClaudeKind,
    current: Option<&Value>,
    buffers: &mut Buffers,
    env: bool,
) -> Change {
    match kind {
        ClaudeKind::Bool => {
            let now = current.and_then(Value::as_bool);
            let mut v = now;
            Segmented::new(("claude-bool", id), &mut v)
                .label(id.to_owned())
                .small()
                .option(None, unset_label(cx))
                .option(Some(true), cx.tr.t("claude.settings.value.on"))
                .option(Some(false), cx.tr.t("claude.settings.value.off"))
                .show(ui);
            (v != now).then(|| v.map(Value::Bool))
        }
        ClaudeKind::Flag => {
            let now = current.map(|v| match v {
                Value::String(s) => s.trim() != "0" && !s.trim().is_empty(),
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_i64() != Some(0),
                _ => true,
            });
            let mut v = now;
            Segmented::new(("claude-flag", id), &mut v)
                .label(id.to_owned())
                .small()
                .option(None, unset_label(cx))
                .option(Some(true), cx.tr.t("claude.settings.value.on_1"))
                .option(Some(false), cx.tr.t("claude.settings.value.off_0"))
                .show(ui);
            (v != now).then(|| v.map(|on| Value::String(if on { "1" } else { "0" }.into())))
        }
        ClaudeKind::Enum { values, open } => {
            let now = current.and_then(Value::as_str).map(str::to_owned);
            let mut pick: Option<Option<String>> = None;
            Dropdown::new(("claude-enum", id))
                .label(id.to_owned())
                .selected_text(now.clone().unwrap_or_else(|| unset_label(cx)))
                .width(220.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(now.is_none(), unset_label(cx))
                        .clicked()
                    {
                        pick = Some(None);
                    }
                    for v in values {
                        if ui
                            .selectable_label(now.as_deref() == Some(v.as_str()), v)
                            .clicked()
                        {
                            pick = Some(Some(v.clone()));
                        }
                    }
                });
            if *open {
                let key = format!("{id}#open");
                let buf = buffers
                    .entry(key)
                    .or_insert_with(|| now.clone().unwrap_or_default());
                let r = ui
                    .add(
                        TextEdit::singleline(buf)
                            .hint_text(cx.tr.t("claude.settings.value.other_hint"))
                            .desired_width(220.0),
                    )
                    .accessible_name(id.to_owned());
                if r.changed() {
                    let t = buf.trim().to_owned();
                    return Some((!t.is_empty()).then_some(Value::String(t)));
                }
            }
            pick.map(|p| p.map(Value::String))
        }
        ClaudeKind::Number { integer, min, max } if !env => {
            let now = current.map(Value::to_string).unwrap_or_default();
            let buf = buffers.entry(id.to_owned()).or_insert(now);
            let r = ui
                .add(
                    TextEdit::singleline(buf)
                        .hint_text(unset_label(cx))
                        .font(mono_font(text::SMALL, Weight::Regular))
                        .desired_width(160.0),
                )
                .accessible_name(id.to_owned());
            let t = buf.trim().to_owned();
            let parsed: Option<serde_json::Number> = if *integer {
                t.parse::<i64>().ok().map(Into::into)
            } else {
                t.parse::<f64>().ok().and_then(serde_json::Number::from_f64)
            };
            let in_range = parsed
                .as_ref()
                .and_then(|n| n.as_f64())
                .is_some_and(|n| min.is_none_or(|m| n >= m) && max.is_none_or(|m| n <= m));
            if !t.is_empty() && (parsed.is_none() || !in_range) {
                ui.label(Text::caption(
                    cx.tr.t(if *integer {
                        "claude.settings.invalid.integer"
                    } else {
                        "claude.settings.invalid.number"
                    }),
                    cx.pal,
                ));
            }
            if r.changed() {
                if t.is_empty() {
                    return Some(None);
                }
                if let Some(n) = parsed.filter(|_| in_range) {
                    return Some(Some(Value::Number(n)));
                }
            }
            None
        }
        ClaudeKind::List => list_editor(ui, cx, id, current, buffers),
        ClaudeKind::Json => json_editor(ui, cx, id, current, buffers),
        // Text, and numbers of environment variables (text too).
        _ => {
            let now = match current {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let buf = buffers.entry(id.to_owned()).or_insert(now);
            let r = ui
                .add(
                    TextEdit::singleline(buf)
                        .hint_text(unset_label(cx))
                        .font(mono_font(text::SMALL, Weight::Regular))
                        .desired_width(f32::INFINITY),
                )
                .accessible_name(id.to_owned());
            if r.changed() {
                let t = buf.clone();
                return Some((!t.trim().is_empty()).then_some(Value::String(t)));
            }
            None
        }
    }
}

/// One row per item of a list of strings.
fn list_editor(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: &str,
    current: Option<&Value>,
    buffers: &mut Buffers,
) -> Change {
    let items: Vec<String> = current
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let mut next = items.clone();
    let mut remove = None;
    for (i, item) in next.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(item)
                    .font(mono_font(text::SMALL, Weight::Regular))
                    .desired_width((ui.available_width() - 36.0).max(80.0)),
            )
            .accessible_name(id.to_owned());
            if ui
                .add(IconButton::new(
                    Icon::Close,
                    cx.tr.t("claude.settings.remove"),
                ))
                .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        next.remove(i);
    }
    let key = format!("{id}#add");
    let mut added = None;
    ui.horizontal(|ui| {
        let buf = buffers.entry(key.clone()).or_default();
        let r = ui
            .add(
                TextEdit::singleline(buf)
                    .hint_text(cx.tr.t("claude.settings.add_item"))
                    .font(mono_font(text::SMALL, Weight::Regular))
                    .desired_width((ui.available_width() - 70.0).max(80.0)),
            )
            .accessible_name(cx.tr.t("claude.settings.add_item"));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui
            .add(Button::ghost(cx.tr.t("claude.settings.add")).small())
            .clicked()
            || enter)
            && !buf.trim().is_empty()
        {
            added = Some(buf.trim().to_owned());
            buf.clear();
        }
    });
    if let Some(a) = added {
        next.push(a);
    }
    (next != items).then(|| {
        (!next.is_empty() || current.is_some())
            .then(|| Value::Array(next.into_iter().map(Value::String).collect()))
    })
}

/// JSON text, applied when it parses.
pub fn json_editor(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: &str,
    current: Option<&Value>,
    buffers: &mut Buffers,
) -> Change {
    let now = current
        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
        .unwrap_or_default();
    let key = format!("{id}#json");
    let buf = buffers.entry(key).or_insert(now);
    let r = ui
        .add(
            TextEdit::multiline(buf)
                .font(mono_font(text::SMALL, Weight::Regular))
                .code_editor()
                .desired_rows(6)
                .desired_width(f32::INFINITY),
        )
        .accessible_name(id.to_owned());
    let t = buf.trim().to_owned();
    let parsed = if t.is_empty() {
        Ok(None)
    } else {
        serde_json::from_str::<Value>(&t).map(Some)
    };
    if let Err(e) = &parsed {
        ui.label(Text::caption(
            cx.tr
                .f("claude.settings.invalid.json", &[("error", &e.to_string())]),
            cx.pal,
        ));
    }
    if r.changed()
        && let Ok(v) = parsed
    {
        return Some(v);
    }
    None
}

/// `env`: every variable of the block, with its catalog name.
fn env_editor(ui: &mut Ui, cx: &Cx<'_>, current: Option<&Value>, buffers: &mut Buffers) -> Change {
    let catalog = ClaudeCatalog::builtin();
    let zh = cx.tr.locale() == superba_core::Locale::ZhCn;
    let map = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut next = map.clone();
    let mut remove = None;
    egui::Grid::new("claude-env-editor")
        .num_columns(3)
        .spacing(vec2(8.0, 4.0))
        .show(ui, |ui| {
            for (k, v) in &map {
                let help = catalog
                    .entry(&format!("env.{k}"))
                    .and_then(|e| if zh { e.name_zh.clone() } else { e.en.clone() });
                let label =
                    ui.label(RichText::new(k).font(mono_font(text::CAPTION, Weight::Medium)));
                if let Some(h) = help {
                    label.on_hover_text(h);
                }
                let key = format!("env#{k}");
                let shown = v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string());
                let buf = buffers.entry(key).or_insert(shown);
                if ui
                    .add(
                        TextEdit::singleline(buf)
                            .font(mono_font(text::CAPTION, Weight::Regular))
                            .desired_width(160.0),
                    )
                    .accessible_name(k.clone())
                    .changed()
                {
                    next.insert(k.clone(), Value::String(buf.clone()));
                }
                if ui
                    .add(IconButton::new(
                        Icon::Close,
                        cx.tr.t("claude.settings.remove"),
                    ))
                    .clicked()
                {
                    remove = Some(k.clone());
                }
                ui.end_row();
            }
        });
    if let Some(k) = remove {
        next.shift_remove(&k);
    }
    // Add a variable.
    let mut add = None;
    ui.horizontal(|ui| {
        let name = buffers.entry("env#new-name".into()).or_default().clone();
        let mut name_buf = name;
        ui.add(
            TextEdit::singleline(&mut name_buf)
                .hint_text(cx.tr.t("claude.settings.env.name"))
                .font(mono_font(text::CAPTION, Weight::Regular))
                .desired_width(170.0),
        )
        .accessible_name(cx.tr.t("claude.settings.env.name"));
        buffers.insert("env#new-name".into(), name_buf.clone());
        let value_buf = buffers.entry("env#new-value".into()).or_default();
        ui.add(
            TextEdit::singleline(value_buf)
                .hint_text(cx.tr.t("claude.settings.env.value"))
                .font(mono_font(text::CAPTION, Weight::Regular))
                .desired_width(110.0),
        )
        .accessible_name(cx.tr.t("claude.settings.env.value"));
        let n = name_buf.trim().to_owned();
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.settings.add"))
                    .small()
                    .enabled(!n.is_empty()),
            )
            .clicked()
        {
            add = Some((n, value_buf.clone()));
        }
    });
    if let Some((n, v)) = add {
        next.insert(n, Value::String(v));
        buffers.remove("env#new-name");
        buffers.remove("env#new-value");
    }
    (next != map).then(|| (!next.is_empty() || current.is_some()).then_some(Value::Object(next)))
}

/// `hooks`: event → matcher → commands. Hooks other than commands are kept
/// as they are and shown as JSON.
fn hooks_editor(
    ui: &mut Ui,
    cx: &Cx<'_>,
    current: Option<&Value>,
    buffers: &mut Buffers,
) -> Change {
    let catalog = ClaudeCatalog::builtin();
    let root = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut next = root.clone();
    let mut events: Vec<String> = root.keys().cloned().collect();
    for e in catalog.hook_events() {
        if !events.contains(&e) {
            events.push(e);
        }
    }
    let used: Vec<String> = events
        .iter()
        .filter(|e| root.contains_key(*e))
        .cloned()
        .collect();
    for event in &used {
        let matchers = root
            .get(event)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut list = matchers.clone();
        egui::CollapsingHeader::new(
            RichText::new(format!("{event} · {}", matchers.len()))
                .font(mono_font(text::SMALL, Weight::Medium)),
        )
        .id_salt(("claude-hook", event))
        .default_open(true)
        .show(ui, |ui| {
            if let Some(t) = catalog.hook_event_text(event) {
                ui.label(Text::caption(t, cx.pal));
            }
            let mut remove_matcher = None;
            for (mi, m) in list.iter_mut().enumerate() {
                let Some(obj) = m.as_object_mut() else {
                    continue;
                };
                ui.horizontal(|ui| {
                    ui.label(Text::small(
                        cx.tr.t("claude.settings.hooks.matcher"),
                        cx.pal,
                    ));
                    let key = format!("hooks#{event}#{mi}#matcher");
                    let now = obj
                        .get("matcher")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let buf = buffers.entry(key).or_insert(now);
                    if ui
                        .add(
                            TextEdit::singleline(buf)
                                .hint_text(cx.tr.t("claude.settings.hooks.matcher_hint"))
                                .font(mono_font(text::CAPTION, Weight::Regular))
                                .desired_width(160.0),
                        )
                        .accessible_name(format!("{event} matcher"))
                        .changed()
                    {
                        if buf.is_empty() {
                            obj.shift_remove("matcher");
                        } else {
                            obj.insert("matcher".into(), Value::String(buf.clone()));
                        }
                    }
                    if ui
                        .add(IconButton::new(
                            Icon::Close,
                            cx.tr.t("claude.settings.hooks.remove_matcher"),
                        ))
                        .clicked()
                    {
                        remove_matcher = Some(mi);
                    }
                });
                let hooks = obj
                    .entry("hooks")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(arr) = hooks.as_array_mut() {
                    let mut remove_cmd = None;
                    for (ci, h) in arr.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            let is_command =
                                h.get("type").and_then(Value::as_str) == Some("command");
                            if is_command && let Some(ho) = h.as_object_mut() {
                                let key = format!("hooks#{event}#{mi}#{ci}#cmd");
                                let now = ho
                                    .get("command")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned();
                                let buf = buffers.entry(key).or_insert(now);
                                if ui
                                    .add(
                                        TextEdit::singleline(buf)
                                            .hint_text(
                                                cx.tr.t("claude.settings.hooks.command_hint"),
                                            )
                                            .font(mono_font(text::CAPTION, Weight::Regular))
                                            .desired_width(
                                                (ui.available_width() - 110.0).max(100.0),
                                            ),
                                    )
                                    .accessible_name(format!("{event} command"))
                                    .changed()
                                {
                                    ho.insert("command".into(), Value::String(buf.clone()));
                                }
                                let tkey = format!("hooks#{event}#{mi}#{ci}#timeout");
                                let tnow =
                                    ho.get("timeout").map(Value::to_string).unwrap_or_default();
                                let tbuf = buffers.entry(tkey).or_insert(tnow);
                                if ui
                                    .add(
                                        TextEdit::singleline(tbuf)
                                            .hint_text(
                                                cx.tr.t("claude.settings.hooks.timeout_hint"),
                                            )
                                            .desired_width(56.0),
                                    )
                                    .accessible_name(cx.tr.t("claude.settings.hooks.timeout_tip"))
                                    .on_hover_text(cx.tr.t("claude.settings.hooks.timeout_tip"))
                                    .changed()
                                {
                                    match tbuf
                                        .trim()
                                        .parse::<f64>()
                                        .ok()
                                        .and_then(serde_json::Number::from_f64)
                                    {
                                        Some(n) => {
                                            ho.insert("timeout".into(), Value::Number(n));
                                        }
                                        None if tbuf.trim().is_empty() => {
                                            ho.shift_remove("timeout");
                                        }
                                        None => {}
                                    }
                                }
                            } else {
                                ui.label(
                                    RichText::new(h.to_string())
                                        .font(mono_font(text::CAPTION, Weight::Regular)),
                                );
                            }
                            if ui
                                .add(IconButton::new(
                                    Icon::Close,
                                    cx.tr.t("claude.settings.remove"),
                                ))
                                .clicked()
                            {
                                remove_cmd = Some(ci);
                            }
                        });
                    }
                    if let Some(ci) = remove_cmd {
                        arr.remove(ci);
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        if ui
                            .add(
                                Button::ghost(cx.tr.t("claude.settings.hooks.add_command")).small(),
                            )
                            .clicked()
                        {
                            arr.push(json!({"type": "command", "command": ""}));
                        }
                    });
                }
            }
            if let Some(mi) = remove_matcher {
                list.remove(mi);
            }
            if ui
                .add(Button::ghost(cx.tr.t("claude.settings.hooks.add_matcher")).small())
                .clicked()
            {
                list.push(json!({"matcher": "", "hooks": [{"type": "command", "command": ""}]}));
            }
        });
        if list != matchers {
            if list.is_empty() {
                next.shift_remove(event);
            } else {
                next.insert(event.clone(), Value::Array(list));
            }
        }
    }
    // Add an event.
    let unused: Vec<String> = events
        .iter()
        .filter(|e| !root.contains_key(*e))
        .cloned()
        .collect();
    if !unused.is_empty() {
        let mut pick = None;
        Dropdown::new("claude-hooks-add")
            .label(cx.tr.t("claude.settings.hooks.add_event"))
            .selected_text(cx.tr.t("claude.settings.hooks.add_event"))
            .width(220.0)
            .show_ui(ui, |ui| {
                for e in &unused {
                    let r = ui.selectable_label(false, e);
                    let r = match catalog.hook_event_text(e) {
                        Some(t) => r.on_hover_text(t),
                        None => r,
                    };
                    if r.clicked() {
                        pick = Some(e.clone());
                    }
                }
            });
        if let Some(e) = pick {
            next.insert(
                e,
                json!([{"matcher": "", "hooks": [{"type": "command", "command": ""}]}]),
            );
        }
    }
    (next != root).then(|| (!next.is_empty() || current.is_some()).then_some(Value::Object(next)))
}

/// A map of names to switches (`enabledPlugins`).
fn bool_map(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: &str,
    current: Option<&Value>,
    buffers: &mut Buffers,
) -> Change {
    let map = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut next = map.clone();
    let mut remove = None;
    for (k, v) in &map {
        ui.horizontal(|ui| {
            let mut on = v.as_bool();
            let before = on;
            Segmented::new(("claude-map", id, k), &mut on)
                .label(k.clone())
                .small()
                .option(Some(true), cx.tr.t("claude.settings.value.on"))
                .option(Some(false), cx.tr.t("claude.settings.value.off"))
                .show(ui);
            ui.label(RichText::new(k).font(mono_font(text::CAPTION, Weight::Medium)));
            if on != before
                && let Some(b) = on
            {
                next.insert(k.clone(), Value::Bool(b));
            }
            if ui
                .add(IconButton::new(
                    Icon::Close,
                    cx.tr.t("claude.settings.remove"),
                ))
                .clicked()
            {
                remove = Some(k.clone());
            }
        });
    }
    if let Some(k) = remove {
        next.shift_remove(&k);
    }
    let mut add = None;
    ui.horizontal(|ui| {
        let buf = buffers.entry(format!("{id}#add")).or_default();
        ui.add(
            TextEdit::singleline(buf)
                .hint_text(cx.tr.t("claude.settings.plugin_hint"))
                .font(mono_font(text::CAPTION, Weight::Regular))
                .desired_width(220.0),
        )
        .accessible_name(cx.tr.t("claude.settings.plugin_hint"));
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.settings.add"))
                    .small()
                    .enabled(!buf.trim().is_empty()),
            )
            .clicked()
        {
            add = Some(buf.trim().to_owned());
            buf.clear();
        }
    });
    if let Some(k) = add {
        next.insert(k, Value::Bool(true));
    }
    (next != map).then(|| (!next.is_empty() || current.is_some()).then_some(Value::Object(next)))
}

/// `modelSettings`: per model, effort, maximum effort and the compaction
/// window.
fn model_settings(
    ui: &mut Ui,
    cx: &Cx<'_>,
    current: Option<&Value>,
    buffers: &mut Buffers,
) -> Change {
    let map = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut next = map.clone();
    let mut remove = None;
    let levels = ["low", "medium", "high", "xhigh"];
    let max_levels = ["low", "medium", "high", "xhigh", "max"];
    for (model, v) in &map {
        let mut obj = v.as_object().cloned().unwrap_or_default();
        let before = obj.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new(model).font(mono_font(text::SMALL, Weight::Medium)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(IconButton::new(
                        Icon::Close,
                        cx.tr.t("claude.settings.remove"),
                    ))
                    .clicked()
                {
                    remove = Some(model.clone());
                }
            });
        });
        egui::Grid::new(("claude-model-settings", model))
            .num_columns(2)
            .spacing(vec2(8.0, 4.0))
            .show(ui, |ui| {
                for (field, options) in [
                    ("effortLevel", &levels[..]),
                    ("maxEffortLevel", &max_levels[..]),
                ] {
                    ui.label(Text::small(field, cx.pal));
                    let now = obj.get(field).and_then(Value::as_str).map(str::to_owned);
                    let mut pick: Option<Option<String>> = None;
                    Dropdown::new(("claude-ms", model.clone(), field))
                        .label(field.to_owned())
                        .selected_text(now.clone().unwrap_or_else(|| unset_label(cx)))
                        .width(140.0)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(now.is_none(), unset_label(cx))
                                .clicked()
                            {
                                pick = Some(None);
                            }
                            for o in options {
                                if ui
                                    .selectable_label(now.as_deref() == Some(*o), *o)
                                    .clicked()
                                {
                                    pick = Some(Some((*o).to_owned()));
                                }
                            }
                        });
                    if let Some(p) = pick {
                        match p {
                            Some(s) => {
                                obj.insert(field.into(), Value::String(s));
                            }
                            None => {
                                obj.shift_remove(field);
                            }
                        }
                    }
                    ui.end_row();
                }
                ui.label(Text::small("autoCompactWindow", cx.pal));
                let key = format!("modelSettings#{model}#window");
                let now = obj.get("autoCompactWindow").map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
                let buf = buffers.entry(key).or_insert(now.unwrap_or_default());
                if ui
                    .add(
                        TextEdit::singleline(buf)
                            .hint_text(cx.tr.t("claude.settings.window_hint"))
                            .desired_width(140.0),
                    )
                    .accessible_name(format!("{model} autoCompactWindow"))
                    .changed()
                {
                    let t = buf.trim();
                    if t.is_empty() {
                        obj.shift_remove("autoCompactWindow");
                    } else if t == "auto" {
                        obj.insert("autoCompactWindow".into(), json!("auto"));
                    } else if let Ok(n) = t.parse::<u64>() {
                        obj.insert("autoCompactWindow".into(), json!(n));
                    }
                }
                ui.end_row();
            });
        if obj != before {
            next.insert(model.clone(), Value::Object(obj));
        }
        ui.add_space(4.0);
    }
    if let Some(m) = remove {
        next.shift_remove(&m);
    }
    let mut add = None;
    ui.horizontal(|ui| {
        let buf = buffers.entry("modelSettings#add".into()).or_default();
        ui.add(
            TextEdit::singleline(buf)
                .hint_text(cx.tr.t("claude.settings.model_hint"))
                .font(mono_font(text::CAPTION, Weight::Regular))
                .desired_width(220.0),
        )
        .accessible_name(cx.tr.t("claude.settings.model_hint"));
        if ui
            .add(
                Button::ghost(cx.tr.t("claude.settings.add"))
                    .small()
                    .enabled(!buf.trim().is_empty()),
            )
            .clicked()
        {
            add = Some(buf.trim().to_owned());
            buf.clear();
        }
    });
    if let Some(m) = add {
        next.insert(m, Value::Object(Map::new()));
    }
    (next != map).then(|| (!next.is_empty() || current.is_some()).then_some(Value::Object(next)))
}

/// An object with known fields: one editor per field of its schema (nested
/// objects one level deeper), unknown fields as JSON.
pub fn object(
    ui: &mut Ui,
    cx: &Cx<'_>,
    id: &str,
    schema: &Value,
    current: Option<&Value>,
    buffers: &mut Buffers,
    depth: usize,
) -> Change {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return json_editor(ui, cx, id, current, buffers);
    };
    let map = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut next = map.clone();
    for (field, fs) in props {
        let path = format!("{id}.{field}");
        let value = map.get(field);
        let kind = kind_of_schema(fs);
        let nested = kind == ClaudeKind::Json
            && fs
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|p| !p.is_empty());
        let desc = fs
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let change = if nested && depth < 2 {
            let mut c = None;
            egui::CollapsingHeader::new(
                RichText::new(field).font(mono_font(text::SMALL, Weight::Medium)),
            )
            .id_salt(("claude-obj", path.clone()))
            .default_open(value.is_some())
            .show(ui, |ui| {
                if let Some(d) = &desc {
                    ui.label(Text::caption(first_line(d), cx.pal));
                }
                c = object(ui, cx, &path, fs, value, buffers, depth + 1);
            });
            c
        } else {
            let mut c = None;
            ui.horizontal_top(|ui| {
                let label =
                    ui.label(RichText::new(field).font(mono_font(text::SMALL, Weight::Medium)));
                if let Some(d) = &desc {
                    label.on_hover_text(d);
                }
            });
            if matches!(kind, ClaudeKind::List | ClaudeKind::Json) {
                c = scalar(ui, cx, &path, &kind, value, buffers, false);
            } else {
                ui.horizontal_wrapped(|ui| {
                    c = scalar(ui, cx, &path, &kind, value, buffers, false);
                });
            }
            c
        };
        if let Some(v) = change {
            match v {
                Some(v) => {
                    next.insert(field.clone(), v);
                }
                None => {
                    next.shift_remove(field);
                }
            }
        }
    }
    // Fields the schema does not describe.
    for (k, v) in &map {
        if props.contains_key(k) {
            continue;
        }
        ui.label(RichText::new(k).font(mono_font(text::SMALL, Weight::Medium)));
        if let Some(c) = json_editor(ui, cx, &format!("{id}.{k}"), Some(v), buffers) {
            match c {
                Some(v) => {
                    next.insert(k.clone(), v);
                }
                None => {
                    next.shift_remove(k);
                }
            }
        }
    }
    (next != map).then(|| (!next.is_empty() || current.is_some()).then_some(Value::Object(next)))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().to_owned()
}
