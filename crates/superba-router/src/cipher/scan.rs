//! Where a Responses request carries ciphertexts.
//!
//! Item shapes (Codex 0.150 – 0.163, `codex-rs/protocol/src/models.rs`):
//!
//! | Item | Ciphertext | [`Place`] |
//! |---|---|---|
//! | `reasoning` | `encrypted_content` | `Reasoning` |
//! | `compaction` / `compaction_summary` / `context_compaction` | `encrypted_content` | `Compaction` |
//! | `agent_message` | `content[]` parts `{"type":"encrypted_content","encrypted_content":…}` | `Part` |
//! | `function_call_output` / `custom_tool_call_output` | `output[]` parts of the same shape | `Part` |
//! | `function_call` | the `arguments` named by `encrypted_function_args` | `Argument` |
//! | `message`, tool calls, `web_search_call`, … | `encrypted_content` (output-token replay) | `Field` |
//!
//! A part's "ciphertext" is plain text when the provider that made it was not
//! an OpenAI backend: Codex stores a sub-agent's task as encrypted unless the
//! provider said otherwise (`encrypted_function_args: []`).

use memchr::memmem;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::compaction::{Fingerprint, fingerprint, is_compaction_item};
use crate::stats::strip::split_object;

/// How a ciphertext is held (decides how it is neutralised).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// `reasoning.encrypted_content`: the item goes.
    Reasoning,
    /// An item's own `encrypted_content` field: the field goes.
    Field,
    /// An `encrypted_content` part of an agent message or a tool output;
    /// `plaintext`: it holds readable text, not a ciphertext.
    Part {
        /// Readable text rather than a ciphertext.
        plaintext: bool,
    },
    /// A function-call argument named by `encrypted_function_args`.
    Argument,
    /// A compaction item: the whole earlier conversation.
    Compaction,
}

/// One ciphertext of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cipher {
    /// Index of its item in `input`.
    pub item: usize,
    /// The item's `id`.
    pub item_id: Option<String>,
    /// Where it sits.
    pub place: Place,
    /// SHA-256 fingerprint of the text.
    pub fp: Fingerprint,
    head: String,
    tail: String,
}

impl Cipher {
    fn new(item: usize, item_id: Option<&str>, place: Place, text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let head: String = chars.iter().take(4).collect();
        let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
        Self {
            item,
            item_id: item_id.map(str::to_owned),
            place,
            fp: fingerprint(text),
            head,
            tail,
        }
    }

    /// True when its first and last four characters are `head` and `tail`
    /// (how upstreams name a ciphertext: `gAAA...AA==`).
    pub fn has_ends(&self, head: &str, tail: &str) -> bool {
        self.head == head && self.tail == tail
    }
}

/// The ciphertexts of a request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// In input order.
    pub ciphers: Vec<Cipher>,
    /// The request's own new input when it is an `agent_message` (a
    /// sub-agent's task): its index in `input`.
    pub task_item: Option<usize>,
}

impl Scan {
    /// No ciphertext.
    pub fn is_empty(&self) -> bool {
        self.ciphers.is_empty()
    }
}

/// True when `text` looks like a ciphertext (64 or more base64 / base64url
/// characters) rather than readable text.
pub fn looks_like_ciphertext(text: &str) -> bool {
    text.len() >= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'_' | b'-' | b'='))
}

/// The ciphertexts of a Responses body or `response.create` frame (plain
/// JSON). Bodies without the bytes `encrypted` are not parsed.
pub fn scan(body: &[u8]) -> Scan {
    let mut out = Scan::default();
    if memmem::find(body, b"encrypted").is_none() {
        return out;
    }
    let Some(top) = split_object(body) else {
        return out;
    };
    let Some((_, input)) = top.iter().find(|(k, _)| k == "input") else {
        return out;
    };
    let Ok(items) = serde_json::from_str::<Vec<&RawValue>>(input.get()) else {
        return out;
    };
    for (i, raw) in items.iter().enumerate() {
        let text = raw.get();
        if memmem::find(text.as_bytes(), b"encrypted").is_none() {
            continue;
        }
        if let Ok(Value::Object(item)) = serde_json::from_str::<Value>(text) {
            item_ciphers(i, &item, &mut out.ciphers);
        }
    }
    if !out.ciphers.is_empty() {
        out.task_item = task_item(&items);
    }
    out
}

/// The item's type (`message` for the typeless `{role, content}` form).
pub(crate) fn item_type(item: &Map<String, Value>) -> &str {
    match item.get("type").and_then(Value::as_str) {
        Some(t) => t,
        None if item.contains_key("role") => "message",
        None => "",
    }
}

/// Item types whose parts may be encrypted, with the key holding the parts.
pub(crate) fn parts_key(kind: &str) -> Option<&'static str> {
    match kind {
        "agent_message" => Some("content"),
        "function_call_output" | "custom_tool_call_output" => Some("output"),
        _ => None,
    }
}

/// Compaction item types (`context_compaction` included).
pub(crate) fn is_compaction_kind(kind: &str) -> bool {
    is_compaction_item(kind) || kind == "context_compaction"
}

/// A part's ciphertext.
pub(crate) fn part_text(part: &Value) -> Option<&str> {
    let p = part.as_object()?;
    (p.get("type").and_then(Value::as_str) == Some("encrypted_content"))
        .then(|| p.get("encrypted_content").and_then(Value::as_str))
        .flatten()
        .filter(|t| !t.is_empty())
}

/// The encrypted arguments of a `function_call`: `(name, value)`.
pub(crate) fn encrypted_arguments(item: &Map<String, Value>) -> Vec<(String, String)> {
    let Some(names) = item
        .get("encrypted_function_args")
        .and_then(Value::as_array)
        .filter(|n| !n.is_empty())
    else {
        return Vec::new();
    };
    let Some(Value::Object(args)) = item
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|a| serde_json::from_str::<Value>(a).ok())
    else {
        return Vec::new();
    };
    names
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|n| {
            let v = args.get(n)?.as_str()?;
            (!v.is_empty()).then(|| (n.to_owned(), v.to_owned()))
        })
        .collect()
}

fn item_ciphers(i: usize, item: &Map<String, Value>, out: &mut Vec<Cipher>) {
    let kind = item_type(item);
    let id = item.get("id").and_then(Value::as_str);
    let own = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty());
    if kind == "reasoning" {
        if let Some(t) = own {
            out.push(Cipher::new(i, id, Place::Reasoning, t));
        }
        return;
    }
    if is_compaction_kind(kind) {
        if let Some(t) = own {
            out.push(Cipher::new(i, id, Place::Compaction, t));
        }
        return;
    }
    if let Some(key) = parts_key(kind) {
        for part in item
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(t) = part_text(part) {
                let plaintext = !looks_like_ciphertext(t);
                out.push(Cipher::new(i, id, Place::Part { plaintext }, t));
            }
        }
        return;
    }
    if let Some(t) = own {
        out.push(Cipher::new(i, id, Place::Field, t));
    }
    if kind == "function_call" {
        for (_, v) in encrypted_arguments(item) {
            out.push(Cipher::new(i, id, Place::Argument, &v));
        }
    }
}

/// Index of the request's own new input when it is an `agent_message`: the
/// last item that is not a request control (`compaction_trigger`,
/// `additional_tools`).
fn task_item(items: &[&RawValue]) -> Option<usize> {
    #[derive(serde::Deserialize)]
    struct Kind<'a> {
        #[serde(rename = "type", borrow, default)]
        kind: Option<std::borrow::Cow<'a, str>>,
    }
    for (i, raw) in items.iter().enumerate().rev() {
        let kind = serde_json::from_str::<Kind<'_>>(raw.get())
            .ok()
            .and_then(|k| k.kind);
        match kind.as_deref() {
            Some("compaction_trigger" | "additional_tools") => continue,
            Some("agent_message") => return Some(i),
            _ => return None,
        }
    }
    None
}
