//! Anthropic Messages codec. The only Anthropic-specific wire knowledge in
//! the translation hub lives here.

use super::ir::*;
use super::{Invalid, array, kind, number, optional_string, role, role_name, string, strings};
use serde_json::{Value, json};

pub(crate) fn blocks(value: &Value) -> Result<Vec<Block>, Invalid> {
    if let Some(text) = value.as_str() {
        return Ok(vec![Block::text(text)]);
    }
    if value.is_null() {
        return Ok(vec![]);
    }
    value
        .as_array()
        .ok_or_else(|| Invalid::new("content must be text or an array"))?
        .iter()
        .map(decode_block)
        .collect()
}

pub(crate) fn decode_block(v: &Value) -> Result<Block, Invalid> {
    let mut opaque = None;
    let content = match kind(v) {
        "text" => Content::Text(string(v, "text")),
        "image" | "document" => {
            let s = &v["source"];
            let media = Media {
                mime_type: string(s, "media_type"),
                source: match kind(s) {
                    "base64" => MediaSource::Inline(string(s, "data")),
                    "url" => MediaSource::Uri(string(s, "url")),
                    _ => return Err(Invalid::new("unsupported image/document source")),
                },
                detail: None,
            };
            if kind(v) == "image" {
                Content::Image(media)
            } else {
                Content::File(media)
            }
        }
        "thinking" | "redacted_thinking" => {
            let (field, k) = if kind(v) == "thinking" {
                ("signature", OpaqueKind::Signature)
            } else {
                ("data", OpaqueKind::Redacted)
            };
            opaque = optional_string(v, field)
                .filter(|s| !s.is_empty())
                .map(|value| Opaque {
                    family: Family::Anthropic,
                    kind: k,
                    value,
                });
            Content::Reasoning {
                text: string(v, "thinking"),
                family: Family::Anthropic,
                id: None,
            }
        }
        "tool_use" => Content::ToolCall {
            id: string(v, "id"),
            name: string(v, "name"),
            arguments: v
                .get("input")
                .map(Value::to_string)
                .unwrap_or_else(|| "{}".into()),
        },
        "tool_result" => Content::ToolResult {
            call_id: string(v, "tool_use_id"),
            name: None,
            content: blocks(&v["content"])?,
            is_error: v["is_error"].as_bool().unwrap_or(false),
        },
        _ => return Err(Invalid::new("unsupported Anthropic content block")),
    };
    Ok(Block { content, opaque })
}

/// Request histories may only replay signed Anthropic thinking. Unsigned or
/// foreign summaries remain ordinary text; ciphertext never becomes text.
pub(crate) fn encode_block(b: &Block, history: bool) -> Result<Option<Value>, Invalid> {
    let opaque = b.opaque_for(Family::Anthropic);
    Ok(Some(match &b.content {
        Content::Text(text) => json!({"type":"text","text":text}),
        Content::Image(m) | Content::File(m) => {
            let source = match &m.source {
                MediaSource::Inline(data) => {
                    json!({"type":"base64","media_type":m.mime_type,"data":data})
                }
                MediaSource::Uri(url) => json!({"type":"url","url":url}),
            };
            json!({"type":if matches!(&b.content, Content::Image(_)) {"image"} else {"document"},"source":source})
        }
        Content::Reasoning { text, .. } => match opaque {
            Some(o) if o.kind == OpaqueKind::Redacted => {
                json!({"type":"redacted_thinking","data":o.value})
            }
            Some(o) if o.kind == OpaqueKind::Signature => {
                json!({"type":"thinking","thinking":text,"signature":o.value})
            }
            _ if text.is_empty() => return Ok(None),
            _ if history => json!({"type":"text","text":text}),
            _ => json!({"type":"thinking","thinking":text}),
        },
        Content::ToolCall {
            id,
            name,
            arguments,
        } => {
            json!({"type":"tool_use","id":id,"name":name,"input":super::json_arguments(arguments)?})
        }
        Content::ToolResult {
            call_id,
            content,
            is_error,
            ..
        } => {
            json!({"type":"tool_result","tool_use_id":call_id,"content":encode_blocks(content, history)?,"is_error":is_error})
        }
    }))
}

fn encode_blocks(content: &[Block], history: bool) -> Result<Vec<Value>, Invalid> {
    content
        .iter()
        .filter_map(|b| encode_block(b, history).transpose())
        .collect()
}

/// Decode a native request into the shared conversation representation.
pub fn decode_request(v: &Value) -> Result<Request, Invalid> {
    let messages = v["messages"]
        .as_array()
        .ok_or_else(|| Invalid::new("messages must be an array"))?;
    let mut r = Request {
        model: string(v, "model"),
        stream: v["stream"].as_bool().unwrap_or(false),
        system: blocks(&v["system"])?,
        max_output_tokens: v["max_tokens"].as_u64(),
        temperature: number(v, "temperature"),
        top_p: number(v, "top_p"),
        stop: strings(&v["stop_sequences"]),
        output_schema: v.pointer("/output_config/format/schema").cloned(),
        ..Request::default()
    };
    for m in messages {
        let role = role(&string(m, "role"))?;
        let content = blocks(&m["content"])?;
        if role == Role::System {
            r.system.extend(content);
        } else {
            r.messages.push(Message { role, content });
        }
    }
    for t in array(v, "tools") {
        if !matches!(kind(t), "" | "custom") {
            return Err(Invalid::new(
                "server-hosted Anthropic tools cannot be translated",
            ));
        }
        r.tools.push(Tool {
            native: None,
            name: string(t, "name"),
            description: optional_string(t, "description"),
            parameters: t
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}})),
            strict: t["strict"].as_bool(),
        });
    }
    r.tool_choice = match kind(&v["tool_choice"]) {
        "auto" => Some(ToolChoice::Auto),
        "any" => Some(ToolChoice::Required),
        "none" => Some(ToolChoice::None),
        "tool" => Some(ToolChoice::Named(string(&v["tool_choice"], "name"))),
        _ => None,
    };
    r.parallel_tools = v
        .pointer("/tool_choice/disable_parallel_tool_use")
        .and_then(Value::as_bool)
        .map(|disabled| !disabled);
    r.thinking = Thinking {
        enabled: matches!(kind(&v["thinking"]), "enabled" | "adaptive"),
        budget: v.pointer("/thinking/budget_tokens").and_then(Value::as_i64),
        effort: optional_string(&v["output_config"], "effort"),
        visible: v.get("thinking").is_some()
            && v.pointer("/thinking/display").and_then(Value::as_str) != Some("omitted"),
    };
    Ok(r)
}

/// Encode a request, dropping opaque state from foreign model families.
pub fn encode_request(r: &Request) -> Result<Value, Invalid> {
    let mut messages: Vec<Value> = Vec::new();
    let mut system = encode_blocks(&r.system, true)?;
    for m in &r.messages {
        if m.role == Role::System {
            system.extend(encode_blocks(&m.content, true)?);
            continue;
        }
        let content = encode_blocks(&m.content, true)?;
        if !content.is_empty() {
            messages.push(json!({"role":role_name(m.role),"content":content}));
        }
    }
    let mut v = json!({"model":r.model,"stream":r.stream,"messages":messages,"max_tokens":r.max_output_tokens.unwrap_or(8192)});
    if !system.is_empty() {
        v["system"] = system.into();
    }
    if let Some(t) = &r.temperature {
        v["temperature"] = t.clone().into();
    }
    if let Some(t) = &r.top_p {
        v["top_p"] = t.clone().into();
    }
    if !r.stop.is_empty() {
        v["stop_sequences"] = json!(r.stop);
    }
    if !r.tools.is_empty() {
        v["tools"] = Value::Array(
            r.tools
                .iter()
                .map(|t| {
                    let mut v = json!({"name":t.name,"input_schema":t.parameters});
                    if let Some(d) = &t.description {
                        v["description"] = d.clone().into();
                    }
                    if let Some(s) = t.strict {
                        v["strict"] = s.into();
                    }
                    v
                })
                .collect(),
        );
    }
    if let Some(c) = &r.tool_choice {
        v["tool_choice"] = match c {
            ToolChoice::Auto => json!({"type":"auto"}),
            ToolChoice::Required => json!({"type":"any"}),
            ToolChoice::None => json!({"type":"none"}),
            ToolChoice::Named(n) => json!({"type":"tool","name":n}),
        };
    }
    if let Some(p) = r.parallel_tools {
        if v["tool_choice"].is_null() {
            v["tool_choice"] = json!({"type":"auto"});
        }
        v["tool_choice"]["disable_parallel_tool_use"] = (!p).into();
    }
    if r.thinking.enabled {
        v["thinking"] = match r.thinking.budget {
            Some(n) if n > 0 => json!({"type":"enabled","budget_tokens":n}),
            _ => json!({"type":"adaptive"}),
        };
        if !r.thinking.visible {
            v["thinking"]["display"] = "omitted".into();
        }
    }
    if let Some(e) = &r.thinking.effort {
        v["output_config"]["effort"] = e.clone().into();
    }
    if let Some(schema) = &r.output_schema {
        v["output_config"]["format"] = json!({"type":"json_schema","schema":schema});
    }
    Ok(v)
}

pub(crate) fn usage(v: &Value) -> Usage {
    let read = v["cache_read_input_tokens"].as_u64();
    let write = v["cache_creation_input_tokens"].as_u64();
    Usage {
        input: v["input_tokens"].as_u64().map(|n| {
            n.saturating_add(read.unwrap_or(0))
                .saturating_add(write.unwrap_or(0))
        }),
        output: v["output_tokens"].as_u64(),
        cache_read: read,
        cache_write: write,
        reasoning: None,
    }
}

pub(crate) fn encode_usage(u: Usage) -> Value {
    let mut v = json!({});
    for (key, count) in [
        ("input_tokens", u.fresh_input()),
        ("output_tokens", u.output),
        ("cache_read_input_tokens", u.cache_read),
        ("cache_creation_input_tokens", u.cache_write),
    ] {
        if let Some(n) = count {
            v[key] = n.into();
        }
    }
    v
}

pub(crate) fn stop(v: &Value) -> Option<StopReason> {
    Some(match v["stop_reason"].as_str()? {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolCall,
        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence(string(v, "stop_sequence")),
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.into()),
    })
}

pub(crate) fn encode_stop(stop: Option<&StopReason>) -> (Value, Value) {
    match stop {
        Some(StopReason::EndTurn) => ("end_turn".into(), Value::Null),
        Some(StopReason::ToolCall) => ("tool_use".into(), Value::Null),
        Some(StopReason::MaxTokens) => ("max_tokens".into(), Value::Null),
        Some(StopReason::Refusal) => ("refusal".into(), Value::Null),
        Some(StopReason::StopSequence(s)) => ("stop_sequence".into(), s.clone().into()),
        Some(StopReason::Other(s)) => (s.clone().into(), Value::Null),
        None => (Value::Null, Value::Null),
    }
}

/// Decode a completed native response and normalize token usage.
pub fn decode_response(v: &Value) -> Result<Response, Invalid> {
    if v.get("error").is_some() {
        return Ok(Response {
            error: Some(decode_error(v)),
            ..Response::default()
        });
    }
    Ok(Response {
        id: string(v, "id"),
        model: string(v, "model"),
        content: blocks(&v["content"])?,
        usage: usage(&v["usage"]),
        stop: stop(v),
        error: None,
    })
}

/// Encode a completed response in this protocol.
pub fn encode_response(r: &Response) -> Result<Value, Invalid> {
    if let Some(e) = &r.error {
        return Ok(super::encode_error(Protocol::Messages, e));
    }
    let (stop, sequence) = encode_stop(r.stop.as_ref());
    Ok(
        json!({"type":"message","role":"assistant","id":r.id,"model":r.model,"content":encode_blocks(&r.content,false)?,"stop_reason":stop,"stop_sequence":sequence,"usage":encode_usage(r.usage)}),
    )
}

#[derive(Default)]
/// Incremental decoder for this protocol's response events.
pub struct Decoder {
    usage: Usage,
}

impl Decoder {
    /// Decode one native event into zero or more IR events.
    pub fn decode(&mut self, v: &Value) -> Result<Vec<Event>, Invalid> {
        let index = v["index"].as_u64().unwrap_or(0) as usize;
        Ok(match kind(v) {
            "message_start" => {
                self.usage.merge(usage(&v["message"]["usage"]));
                vec![
                    Event::Start {
                        id: string(&v["message"], "id"),
                        model: string(&v["message"], "model"),
                    },
                    Event::Usage(self.usage),
                ]
            }
            "content_block_start" => {
                let mut block = decode_block(&v["content_block"])?;
                if let Content::ToolCall { arguments, .. } = &mut block.content
                    && arguments == "{}"
                {
                    arguments.clear();
                }
                vec![Event::BlockStart { index, block }]
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let delta = match kind(d) {
                    "text_delta" => Delta::Text(string(d, "text")),
                    "thinking_delta" => Delta::Reasoning(string(d, "thinking")),
                    "input_json_delta" => Delta::Arguments(string(d, "partial_json")),
                    "signature_delta" => Delta::Opaque(Opaque {
                        family: Family::Anthropic,
                        kind: OpaqueKind::Signature,
                        value: string(d, "signature"),
                    }),
                    _ => return Ok(vec![]),
                };
                vec![Event::Delta { index, delta }]
            }
            "content_block_stop" => vec![Event::BlockEnd { index }],
            "message_delta" => {
                // A delta may omit the cache counters supplied by message_start.
                let mut raw = v["usage"].clone();
                for (key, count) in [
                    ("cache_read_input_tokens", self.usage.cache_read),
                    ("cache_creation_input_tokens", self.usage.cache_write),
                ] {
                    if raw.get(key).is_none()
                        && let Some(n) = count
                    {
                        raw[key] = n.into();
                    }
                }
                self.usage.merge(usage(&raw));
                let mut out = vec![Event::Usage(self.usage)];
                if let Some(s) = stop(&v["delta"]) {
                    out.push(Event::Stop(s));
                }
                out
            }
            "message_stop" => vec![Event::End],
            "error" => vec![Event::Error(decode_error(v))],
            _ => vec![],
        })
    }
}

fn decode_error(v: &Value) -> ApiError {
    let status = match v["error"]["type"].as_str() {
        Some("invalid_request_error") => 400,
        Some("authentication_error") => 401,
        Some("permission_error") => 403,
        Some("not_found_error") => 404,
        Some("request_too_large") => 413,
        Some("rate_limit_error") => 429,
        Some("overloaded_error") => 529,
        _ => 500,
    };
    super::decode_error(status, v)
}

#[derive(Default)]
/// Incremental encoder for this protocol's response events.
pub struct Encoder {
    state: Accumulator,
    /// Empty foreign encrypted blocks have no downstream index.
    indexes: std::collections::BTreeMap<usize, usize>,
}

impl Encoder {
    /// Encode one IR event into zero or more native events.
    pub fn encode(&mut self, e: &Event) -> Result<Vec<Value>, Invalid> {
        self.state.push(e)?;
        let mut out = Vec::new();
        match e {
            Event::Start { id, model } => out.push(json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}})),
            Event::BlockStart { index, block } => {
                let mut start = block.clone();
                let initial = match &mut start.content {
                    Content::Text(t) => Some(Delta::Text(std::mem::take(t))), Content::Reasoning { text, .. } => Some(Delta::Reasoning(std::mem::take(text))),
                    Content::ToolCall { arguments, .. } => Some(Delta::Arguments(std::mem::take(arguments))), _ => None,
                };
                // Keep visible thinking even while its initial delta is empty.
                let encoded = if matches!(&start.content, Content::Reasoning { .. }) && start.opaque_for(Family::Anthropic).is_none() {
                    Some(json!({"type":"thinking","thinking":""}))
                } else { encode_block(&start,false)? };
                if let Some(b) = encoded {
                    let wire_index = self.indexes.len(); self.indexes.insert(*index,wire_index);
                    out.push(json!({"type":"content_block_start","index":wire_index,"content_block":b}));
                    if let Some(d) = initial { out.extend(self.delta(*index,&d)); }
                }
            }
            Event::Delta { index, delta } => out.extend(self.delta(*index,delta)),
            Event::BlockEnd { index } => { if let Some(i) = self.indexes.get(index) { out.push(json!({"type":"content_block_stop","index":i})); } }
            Event::End => {
                let (stop,sequence) = encode_stop(self.state.response.stop.as_ref());
                out.push(json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":sequence},"usage":encode_usage(self.state.response.usage)}));
                out.push(json!({"type":"message_stop"}));
            }
            Event::Error(e) => out.push(super::encode_error(Protocol::Messages,e)),
            Event::Usage(_) | Event::Stop(_) => {}
        }
        Ok(out)
    }

    fn delta(&self, index: usize, d: &Delta) -> Vec<Value> {
        let Some(index) = self.indexes.get(&index) else {
            return vec![];
        };
        let delta = match d {
            Delta::Text(t) if !t.is_empty() => json!({"type":"text_delta","text":t}),
            Delta::Reasoning(t) if !t.is_empty() => json!({"type":"thinking_delta","thinking":t}),
            Delta::Arguments(t) if !t.is_empty() => {
                json!({"type":"input_json_delta","partial_json":t})
            }
            Delta::Opaque(o)
                if o.family == Family::Anthropic && o.kind == OpaqueKind::Signature =>
            {
                json!({"type":"signature_delta","signature":o.value})
            }
            _ => return vec![],
        };
        vec![json!({"type":"content_block_delta","index":index,"delta":delta})]
    }
}
