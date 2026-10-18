//! Neutralising chosen ciphertexts of a request, byte-preservingly: only the
//! items holding one change, every other item and member keeps its bytes.

use std::borrow::Cow;

use memchr::memmem;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::scan::{
    encrypted_arguments, is_compaction_kind, item_type, looks_like_ciphertext, part_text, parts_key,
};
use crate::compaction::{Fingerprint, fingerprint};
use crate::stats::strip::{assemble, split_object};

/// Text that replaces an encrypted part or argument that is a real ciphertext.
pub const OMITTED: &str =
    "[Encrypted content from another provider was left out: this provider cannot read it.]";

/// Text of the developer message that replaces a compaction item.
pub const COMPACTION_NOTE: &str = "[An earlier part of this conversation was compacted by another provider; that summary cannot be read here and was left out.]";

/// What [`neutralise`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    /// Reasoning items left out.
    pub reasoning: u32,
    /// `encrypted_content` fields dropped from other items.
    pub fields: u32,
    /// Encrypted parts that held plain text, kept as `input_text`.
    pub parts_as_text: u32,
    /// Encrypted parts replaced by [`OMITTED`].
    pub parts_omitted: u32,
    /// Encrypted function-call arguments replaced by [`OMITTED`].
    pub arguments: u32,
    /// Compaction items replaced by [`COMPACTION_NOTE`].
    pub compactions: u32,
}

impl Removed {
    /// Everything changed.
    pub fn total(&self) -> u32 {
        self.reasoning
            + self.fields
            + self.parts_as_text
            + self.parts_omitted
            + self.arguments
            + self.compactions
    }

    /// Add `other`.
    pub fn add(&mut self, other: &Removed) {
        self.reasoning += other.reasoning;
        self.fields += other.fields;
        self.parts_as_text += other.parts_as_text;
        self.parts_omitted += other.parts_omitted;
        self.arguments += other.arguments;
        self.compactions += other.compactions;
    }

    /// For logs and events: `2 reasoning items, 1 encrypted payload kept as
    /// plain text`.
    pub fn describe(&self) -> String {
        let mut out = Vec::new();
        let mut put = |n: u32, one: &str, many: &str| {
            if n > 0 {
                out.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        };
        put(self.reasoning, "reasoning item", "reasoning items");
        put(
            self.parts_as_text,
            "encrypted payload that held plain text (kept as text)",
            "encrypted payloads that held plain text (kept as text)",
        );
        put(
            self.parts_omitted,
            "encrypted payload (replaced by a note)",
            "encrypted payloads (replaced by a note)",
        );
        put(
            self.arguments,
            "encrypted tool-call argument",
            "encrypted tool-call arguments",
        );
        put(
            self.fields,
            "encrypted copy of a message or tool call",
            "encrypted copies of messages or tool calls",
        );
        put(
            self.compactions,
            "compacted history (replaced by a note)",
            "compacted histories (replaced by a note)",
        );
        out.join(", ")
    }
}

enum Outcome {
    Keep,
    Drop,
    Changed,
}

/// The request `body` with the ciphertexts whose fingerprint `pick` chooses
/// neutralised, and what changed; `None` when nothing was chosen (the body
/// stays as it is).
pub fn neutralise(body: &[u8], pick: &dyn Fn(&Fingerprint) -> bool) -> Option<(Vec<u8>, Removed)> {
    memmem::find(body, b"encrypted")?;
    let top = split_object(body)?;
    let input_at = top.iter().position(|(k, _)| k == "input")?;
    let items: Vec<&RawValue> = serde_json::from_str(top[input_at].1.get()).ok()?;
    let mut removed = Removed::default();
    let mut new_items: Vec<Cow<'_, str>> = Vec::with_capacity(items.len());
    for raw in &items {
        let text = raw.get();
        if memmem::find(text.as_bytes(), b"encrypted").is_none() {
            new_items.push(Cow::Borrowed(text));
            continue;
        }
        let Ok(mut item) = serde_json::from_str::<Value>(text) else {
            new_items.push(Cow::Borrowed(text));
            continue;
        };
        match rewrite_item(&mut item, pick, &mut removed) {
            Outcome::Keep => new_items.push(Cow::Borrowed(text)),
            Outcome::Drop => {}
            Outcome::Changed => new_items.push(Cow::Owned(serde_json::to_string(&item).ok()?)),
        }
    }
    if removed.total() == 0 {
        return None;
    }
    Some((assemble(&top, input_at, &new_items)?, removed))
}

fn rewrite_item(
    item: &mut Value,
    pick: &dyn Fn(&Fingerprint) -> bool,
    removed: &mut Removed,
) -> Outcome {
    let Some(obj) = item.as_object_mut() else {
        return Outcome::Keep;
    };
    let kind = item_type(obj).to_owned();
    let own_picked = obj
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|t| !t.is_empty() && pick(&fingerprint(t)));
    if kind == "reasoning" {
        if own_picked {
            removed.reasoning += 1;
            return Outcome::Drop;
        }
        return Outcome::Keep;
    }
    if is_compaction_kind(&kind) {
        if !own_picked {
            return Outcome::Keep;
        }
        removed.compactions += 1;
        *item = json!({
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": COMPACTION_NOTE}],
        });
        return Outcome::Changed;
    }
    let mut changed = false;
    if let Some(key) = parts_key(&kind) {
        for part in obj
            .get_mut(key)
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let Some(t) = part_text(part).filter(|t| pick(&fingerprint(t))) else {
                continue;
            };
            let text = if looks_like_ciphertext(t) {
                removed.parts_omitted += 1;
                OMITTED.to_owned()
            } else {
                removed.parts_as_text += 1;
                t.to_owned()
            };
            *part = json!({"type": "input_text", "text": text});
            changed = true;
        }
        return if changed {
            Outcome::Changed
        } else {
            Outcome::Keep
        };
    }
    if own_picked {
        obj.shift_remove("encrypted_content");
        removed.fields += 1;
        changed = true;
    }
    if kind == "function_call" && neutralise_arguments(obj, pick, removed) {
        changed = true;
    }
    if changed {
        Outcome::Changed
    } else {
        Outcome::Keep
    }
}

/// Replace the chosen encrypted arguments of a `function_call` by [`OMITTED`]
/// and stop listing them in `encrypted_function_args` (dropped when empty).
fn neutralise_arguments(
    obj: &mut serde_json::Map<String, Value>,
    pick: &dyn Fn(&Fingerprint) -> bool,
    removed: &mut Removed,
) -> bool {
    let chosen: Vec<String> = encrypted_arguments(obj)
        .into_iter()
        .filter(|(_, v)| pick(&fingerprint(v)))
        .map(|(n, _)| n)
        .collect();
    if chosen.is_empty() {
        return false;
    }
    let Some(Value::Object(mut args)) = obj
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|a| serde_json::from_str::<Value>(a).ok())
    else {
        return false;
    };
    for name in &chosen {
        args.insert(name.clone(), Value::String(OMITTED.to_owned()));
        removed.arguments += 1;
    }
    let Ok(text) = serde_json::to_string(&Value::Object(args)) else {
        return false;
    };
    obj.insert("arguments".into(), Value::String(text));
    let left: Vec<Value> = obj
        .get("encrypted_function_args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| n.as_str().is_none_or(|n| !chosen.iter().any(|c| c == n)))
        .cloned()
        .collect();
    if left.is_empty() {
        obj.shift_remove("encrypted_function_args");
    } else {
        obj.insert("encrypted_function_args".into(), Value::Array(left));
    }
    true
}
