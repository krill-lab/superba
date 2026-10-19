//! Protocol-independent requests, responses and incremental events.
//!
//! Token counts include cache reads/writes in input and reasoning in output.
//! Opaque model state has a producer family; codecs may only return it to that
//! family. No codec invents a signature or wraps another vendor's ciphertext.

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

/// A wire protocol, independent of the model or coding tool using it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Messages,
    Responses,
    Chat,
    Gemini,
}

impl Protocol {
    pub const ALL: [Self; 4] = [Self::Messages, Self::Responses, Self::Chat, Self::Gemini];

    pub fn family(self) -> Family {
        match self {
            Self::Messages => Family::Anthropic,
            Self::Responses | Self::Chat => Family::OpenAi,
            Self::Gemini => Family::Google,
        }
    }
}

/// Producer of reasoning state, including plaintext reasoning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Anthropic,
    OpenAi,
    Google,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpaqueKind {
    Signature,
    Encrypted,
    Redacted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opaque {
    pub family: Family,
    pub kind: OpaqueKind,
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSource {
    Inline(String),
    Uri(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    /// Empty when the wire protocol only supplied a URL.
    pub mime_type: String,
    pub source: MediaSource,
    pub detail: Option<String>,
}

impl Media {
    pub fn from_url(url: &str) -> Self {
        if let Some(data) = url.strip_prefix("data:")
            && let Some((mime, data)) = data.split_once(";base64,")
        {
            return Self {
                mime_type: mime.to_owned(),
                source: MediaSource::Inline(data.to_owned()),
                detail: None,
            };
        }
        Self {
            mime_type: String::new(),
            source: MediaSource::Uri(url.to_owned()),
            detail: None,
        }
    }

    pub fn url(&self) -> String {
        match &self.source {
            MediaSource::Uri(u) => u.clone(),
            MediaSource::Inline(data) => format!("data:{};base64,{data}", self.mime_type),
        }
    }
}

/// A block's signature is independent of its content: Gemini attaches thought
/// signatures to text and function calls as well as to thoughts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub content: Content,
    pub opaque: Option<Opaque>,
}

impl Block {
    pub fn new(content: Content) -> Self {
        Self {
            content,
            opaque: None,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::new(Content::Text(text.into()))
    }

    pub fn opaque_for(&self, family: Family) -> Option<&Opaque> {
        self.opaque.as_ref().filter(|o| o.family == family)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Text(String),
    Image(Media),
    File(Media),
    Reasoning {
        text: String,
        family: Family,
        /// Responses reasoning item id (not a function's call id).
        id: Option<String>,
    },
    ToolCall {
        id: String,
        name: String,
        /// JSON text; stream deltas may be incomplete until BlockEnd.
        arguments: String,
    },
    ToolResult {
        call_id: String,
        /// Required by Gemini; recovered from the matching call when absent.
        name: Option<String>,
        content: Vec<Block>,
        is_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Value,
    pub strict: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Required,
    None,
    Named(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thinking {
    pub enabled: bool,
    pub budget: Option<i64>,
    pub effort: Option<String>,
    pub visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub model: String,
    pub stream: bool,
    pub system: Vec<Block>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub tool_choice: Option<ToolChoice>,
    pub parallel_tools: Option<bool>,
    pub max_output_tokens: Option<u64>,
    pub temperature: Option<Number>,
    pub top_p: Option<Number>,
    pub stop: Vec<String>,
    pub thinking: Thinking,
    /// JSON schema of a structured answer (None = ordinary text).
    pub output_schema: Option<Value>,
}

impl Request {
    /// A tool result carries the id of its call, even on protocols where a
    /// function name alone is legal. Do not guess among parallel same-name calls.
    pub fn resolve_tools(&mut self) -> Result<(), super::Invalid> {
        let mut calls = std::collections::BTreeMap::<String, String>::new();
        for message in &mut self.messages {
            for block in &mut message.content {
                match &mut block.content {
                    Content::ToolCall { id, name, .. } => {
                        if id.is_empty() || name.is_empty() {
                            return Err(super::Invalid::new("tool call needs an id and a name"));
                        }
                        calls.insert(id.clone(), name.clone());
                    }
                    Content::ToolResult { call_id, name, .. } => {
                        if let Some(n) = calls.get(call_id) {
                            *name = Some(n.clone());
                        } else if call_id.is_empty() {
                            return Err(super::Invalid::new("tool result has no matching call id"));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
}

impl Usage {
    /// Usage events are cumulative snapshots. Missing fields never erase an
    /// earlier count (Anthropic sends input and output at different times).
    pub fn merge(&mut self, new: Self) {
        macro_rules! field {
            ($($f:ident),*) => { $(if new.$f.is_some() { self.$f = new.$f; })* };
        }
        field!(input, output, cache_read, cache_write, reasoning);
    }

    pub fn fresh_input(self) -> Option<u64> {
        self.input.map(|n| {
            n.saturating_sub(self.cache_read.unwrap_or(0))
                .saturating_sub(self.cache_write.unwrap_or(0))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolCall,
    MaxTokens,
    StopSequence(String),
    Refusal,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    pub model: String,
    pub content: Vec<Block>,
    pub usage: Usage,
    pub stop: Option<StopReason>,
    pub error: Option<ApiError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delta {
    Text(String),
    Reasoning(String),
    Arguments(String),
    Opaque(Opaque),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Start { id: String, model: String },
    BlockStart { index: usize, block: Block },
    Delta { index: usize, delta: Delta },
    BlockEnd { index: usize },
    Usage(Usage),
    Stop(StopReason),
    End,
    Error(ApiError),
}

/// Accumulates one response for wire formats that include completed items.
/// It is also used by tests to compare streams independent of chunk boundaries.
#[derive(Clone, Debug, Default)]
pub struct Accumulator {
    pub response: Response,
    pub blocks: std::collections::BTreeMap<usize, Block>,
    pub ended: bool,
}

impl Accumulator {
    pub fn push(&mut self, event: &Event) -> Result<(), super::Invalid> {
        match event {
            Event::Start { id, model } => {
                self.response.id.clone_from(id);
                self.response.model.clone_from(model);
            }
            Event::BlockStart { index, block } => {
                if self.blocks.insert(*index, block.clone()).is_some() {
                    return Err(super::Invalid::new("duplicate stream block index"));
                }
            }
            Event::Delta { index, delta } => {
                let b = self
                    .blocks
                    .get_mut(index)
                    .ok_or_else(|| super::Invalid::new("delta before block start"))?;
                match (delta, &mut b.content) {
                    (Delta::Text(d), Content::Text(t))
                    | (Delta::Reasoning(d), Content::Reasoning { text: t, .. })
                    | (Delta::Arguments(d), Content::ToolCall { arguments: t, .. }) => {
                        t.push_str(d)
                    }
                    (Delta::Opaque(o), _) => match &mut b.opaque {
                        Some(old) if old.family == o.family && old.kind == o.kind => {
                            old.value.push_str(&o.value)
                        }
                        _ => b.opaque = Some(o.clone()),
                    },
                    _ => return Err(super::Invalid::new("stream delta does not match its block")),
                }
            }
            Event::Usage(usage) => self.response.usage.merge(*usage),
            Event::Stop(reason) => self.response.stop = Some(reason.clone()),
            Event::End => self.ended = true,
            Event::Error(e) => {
                self.response.error = Some(e.clone());
                self.ended = true;
            }
            Event::BlockEnd { .. } => {}
        }
        self.response.content = self.blocks.values().cloned().collect();
        Ok(())
    }
}
