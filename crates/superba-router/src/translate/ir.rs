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
    /// Anthropic Messages.
    Messages,
    /// OpenAI Responses.
    Responses,
    /// OpenAI Chat Completions.
    Chat,
    /// Google Gemini generateContent.
    Gemini,
}

impl Protocol {
    /// Every supported wire protocol in a stable order.
    pub const ALL: [Self; 4] = [Self::Messages, Self::Responses, Self::Chat, Self::Gemini];

    /// The family allowed to receive this protocol's opaque model state.
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
    /// The Anthropic model family.
    Anthropic,
    /// The OpenAI model family.
    OpenAi,
    /// The Google model family.
    Google,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The protocol representation of opaque model state.
pub enum OpaqueKind {
    /// An unmodified signature over vendor-owned model state.
    Signature,
    /// Encrypted state emitted by the model.
    Encrypted,
    /// Opaque redacted reasoning emitted by the model.
    Redacted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Vendor-owned state whose bytes must never cross vendor families.
pub struct Opaque {
    /// The model family that produced these bytes.
    pub family: Family,
    /// The protocol form of this opaque state.
    pub kind: OpaqueKind,
    /// Unmodified signature, ciphertext or redacted payload.
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Conversation speaker, normalized across the four protocols.
pub enum Role {
    /// The user or a function result.
    User,
    /// The model's response.
    Assistant,
    /// System or developer instructions.
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Inline base64 content or an externally referenced media resource.
pub enum MediaSource {
    /// Base64-encoded bytes.
    Inline(String),
    /// An external media resource reference.
    Uri(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Media content with its MIME type and optional image detail hint.
pub struct Media {
    /// Empty when the wire protocol only supplied a URL.
    pub mime_type: String,
    /// The media bytes or resource reference.
    pub source: MediaSource,
    /// The requested image processing detail, when supplied.
    pub detail: Option<String>,
}

impl Media {
    /// Parse an inline data URL or retain an external URL as a reference.
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

    /// Encode the media source as a data URL or its original reference.
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
    /// Ordered content blocks, or the payload of a single block.
    pub content: Content,
    /// Optional vendor-owned state attached to this block.
    pub opaque: Option<Opaque>,
}

impl Block {
    /// Construct a content block with no opaque state.
    pub fn new(content: Content) -> Self {
        Self {
            content,
            opaque: None,
        }
    }

    /// Construct a plain text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(Content::Text(text.into()))
    }

    /// Return opaque state only to the family that produced it.
    pub fn opaque_for(&self, family: Family) -> Option<&Opaque> {
        self.opaque.as_ref().filter(|o| o.family == family)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// One protocol-independent conversation content block.
pub enum Content {
    /// Plain text.
    Text(String),
    /// An image input or output.
    Image(Media),
    /// A non-image document or other media file.
    File(Media),
    /// Visible reasoning with its model family.
    Reasoning {
        /// Visible reasoning text.
        text: String,
        /// The family that produced the reasoning.
        family: Family,
        /// Responses reasoning item id (not a function's call id).
        id: Option<String>,
    },
    /// A function invocation with a stable call id.
    ToolCall {
        /// Stable identity of this response, reasoning item or function call.
        id: String,
        /// The function name.
        name: String,
        /// JSON text; stream deltas may be incomplete until BlockEnd.
        arguments: String,
    },
    /// The result of a previous function invocation.
    ToolResult {
        /// Identity of the function call being answered.
        call_id: String,
        /// Required by Gemini; recovered from the matching call when absent.
        name: Option<String>,
        /// The result payload.
        content: Vec<Block>,
        /// Whether the function execution failed.
        is_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// A speaker and the ordered blocks in that conversation message.
pub struct Message {
    /// The speaker of this message.
    pub role: Role,
    /// Ordered content blocks, or the payload of a single block.
    pub content: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// A callable function with a JSON Schema input contract.
pub struct Tool {
    /// The callable function name.
    pub name: String,
    /// Human-readable function documentation.
    pub description: Option<String>,
    /// JSON Schema for the function's argument object.
    pub parameters: Value,
    /// Whether the client requires strict schema enforcement.
    pub strict: Option<bool>,
    /// Native name and text-input format when normalized as a JSON function.
    pub native: Option<NativeTool>,
}

/// Original identity of a namespaced or text-input function. Other codecs
/// expose its normalized name and JSON parameters; Responses restores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTool {
    /// Name before namespace flattening.
    pub name: String,
    /// Optional namespace of the original function.
    pub namespace: Option<String>,
    /// Text-input format; absent for a function with JSON arguments.
    pub format: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The client's constraint on whether and which function to call.
pub enum ToolChoice {
    /// The model chooses whether to call a function.
    Auto,
    /// The model must call a function.
    Required,
    /// No function calls are allowed.
    None,
    /// The model must call the named function.
    Named(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Reasoning generation preferences, without any opaque model state.
pub struct Thinking {
    /// Whether the model should generate reasoning.
    pub enabled: bool,
    /// Requested thinking token budget; vendor sentinel values are retained.
    pub budget: Option<i64>,
    /// Named reasoning effort, when requested.
    pub effort: Option<String>,
    /// Whether the client requests visible reasoning or a summary.
    pub visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
/// A complete conversation request, normalized for codec conversion.
pub struct Request {
    /// The model identifier selected for this request or response.
    pub model: String,
    /// Whether the client expects incremental events.
    pub stream: bool,
    /// System and developer instructions in their original order.
    pub system: Vec<Block>,
    /// Conversation history after the system instructions.
    pub messages: Vec<Message>,
    /// Functions exposed to the model.
    pub tools: Vec<Tool>,
    /// Optional function calling constraint.
    pub tool_choice: Option<ToolChoice>,
    /// Whether the model may issue parallel function calls.
    pub parallel_tools: Option<bool>,
    /// Maximum number of generated tokens.
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature, if explicitly set.
    pub temperature: Option<Number>,
    /// Nucleus sampling threshold, if explicitly set.
    pub top_p: Option<Number>,
    /// Stop sequences for a request, or the completion reason of a response.
    pub stop: Vec<String>,
    /// Reasoning generation preferences.
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
/// Cumulative token counts; input includes cache reads and writes.
pub struct Usage {
    /// Total input tokens, including both cache reads and writes.
    pub input: Option<u64>,
    /// Total output tokens, including reasoning tokens.
    pub output: Option<u64>,
    /// Input tokens served from a previously populated cache.
    pub cache_read: Option<u64>,
    /// Input tokens written into a cache by this request.
    pub cache_write: Option<u64>,
    /// Output tokens used for reasoning.
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

    /// Input tokens excluding cache reads and writes, saturating at zero.
    pub fn fresh_input(self) -> Option<u64> {
        self.input.map(|n| {
            n.saturating_sub(self.cache_read.unwrap_or(0))
                .saturating_sub(self.cache_write.unwrap_or(0))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Why generation stopped, preserving an unrecognized native reason.
pub enum StopReason {
    /// The assistant completed its turn.
    EndTurn,
    /// The assistant needs one or more function results.
    ToolCall,
    /// Generation reached its output limit.
    MaxTokens,
    /// A configured sequence stopped generation.
    StopSequence(String),
    /// Generation was refused or blocked.
    Refusal,
    /// An unrecognized native completion reason.
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// An upstream failure translated into the client's error envelope.
pub struct ApiError {
    /// HTTP status, including the equivalent status for an in-stream error.
    pub status: u16,
    /// Upstream error category or code.
    pub code: String,
    /// Error description for the client.
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
/// A completed assistant response independent of its wire format.
pub struct Response {
    /// The upstream response identifier.
    pub id: String,
    /// The model identifier selected for this request or response.
    pub model: String,
    /// Ordered content blocks, or the payload of a single block.
    pub content: Vec<Block>,
    /// Cumulative token usage.
    pub usage: Usage,
    /// Stop sequences for a request, or the completion reason of a response.
    pub stop: Option<StopReason>,
    /// A terminal error, if generation failed.
    pub error: Option<ApiError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// An incremental update to an already opened content block.
pub enum Delta {
    /// Plain text.
    Text(String),
    /// Append visible reasoning text.
    Reasoning(String),
    /// Append a fragment of function argument JSON.
    Arguments(String),
    /// Append opaque state belonging to its recorded family.
    Opaque(Opaque),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Ordered response lifecycle events shared by every streaming codec.
pub enum Event {
    /// Begin a response and supply its metadata.
    Start {
        /// Native response identity.
        id: String,
        /// Served model identifier.
        model: String,
    },
    /// Open a content block with a stable stream index.
    BlockStart {
        /// Block identity for subsequent deltas.
        index: usize,
        /// Initial content and optional vendor state.
        block: Block,
    },
    /// Append to an open block.
    Delta {
        /// Identity of the open block.
        index: usize,
        /// Content or state fragment.
        delta: Delta,
    },
    /// Finish a content block.
    BlockEnd {
        /// Identity of the completed block.
        index: usize,
    },
    /// Update cumulative token usage.
    Usage(Usage),
    /// Record the generation stop reason.
    Stop(StopReason),
    /// Complete the response.
    End,
    /// Terminate the response with an error.
    Error(ApiError),
}

/// Accumulates one response for wire formats that include completed items.
/// It is also used by tests to compare streams independent of chunk boundaries.
#[derive(Clone, Debug, Default)]
pub struct Accumulator {
    /// The accumulated response metadata and completed content.
    pub response: Response,
    /// Content blocks indexed by their stable stream identity.
    pub blocks: std::collections::BTreeMap<usize, Block>,
    /// Whether a terminal event has been received.
    pub ended: bool,
}

impl Accumulator {
    /// Apply one event, validating block identity and delta type.
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
