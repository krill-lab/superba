//! OpenAI Responses codec. Item ids and function call ids are distinct.

#[path = "responses_tools.rs"]
mod tools;
pub(crate) use tools::Bindings;

use super::ir::*;
use super::{Invalid, array, kind, number, optional_string, role, role_name, string};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn content(v: &Value) -> Result<Vec<Block>, Invalid> {
    if let Some(s) = v.as_str() {
        return Ok(vec![Block::text(s)]);
    }
    if v.is_null() {
        return Ok(vec![]);
    }
    v.as_array()
        .ok_or_else(|| Invalid::new("Responses content must be an array"))?
        .iter()
        .map(|p| {
            Ok(Block::new(match kind(p) {
                "input_text" | "output_text" => Content::Text(string(p, "text")),
                "refusal" => Content::Text(string(p, "refusal")),
                "input_image" => {
                    let mut m = Media::from_url(&string(p, "image_url"));
                    m.detail = optional_string(p, "detail");
                    Content::Image(m)
                }
                "input_file" => Content::File(Media::from_url(
                    p.get("file_data")
                        .or_else(|| p.get("file_url"))
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )),
                _ => return Err(Invalid::new("unsupported Responses content part")),
            }))
        })
        .collect()
}

fn reasoning(v: &Value) -> Block {
    let text = array(v, "summary")
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    Block {
        content: Content::Reasoning {
            text,
            family: Family::OpenAi,
            id: optional_string(v, "id"),
        },
        opaque: optional_string(v, "encrypted_content")
            .filter(|s| !s.is_empty())
            .map(|value| Opaque {
                family: Family::OpenAi,
                kind: OpaqueKind::Encrypted,
                value,
            }),
    }
}

fn input_item(v: &Value) -> Result<Message, Invalid> {
    let (role, content) = match kind(v) {
        "" | "message" => (role(&string(v, "role"))?, content(&v["content"])?),
        "function_call" | "custom_tool_call" => (
            Role::Assistant,
            vec![Block::new(Content::ToolCall {
                id: string(v, "call_id"),
                name: tools::name(
                    &string(v, "name"),
                    v["namespace"].as_str(),
                    kind(v) == "custom_tool_call",
                ),
                arguments: if kind(v) == "custom_tool_call" {
                    json!({"input":v["input"]}).to_string()
                } else {
                    string(v, "arguments")
                },
            })],
        ),
        "function_call_output" | "custom_tool_call_output" => (
            Role::User,
            vec![Block::new(Content::ToolResult {
                call_id: string(v, "call_id"),
                name: None,
                content: content(&v["output"])?,
                is_error: false,
            })],
        ),
        "reasoning" => (Role::Assistant, vec![reasoning(v)]),
        "compaction" => {
            return Err(Invalid::new(
                "remote compaction is unsupported by protocol conversion",
            ));
        }
        "item_reference" => {
            return Err(Invalid::new(
                "stored item references cannot be translated; send the conversation history",
            ));
        }
        _ => return Err(Invalid::new("unsupported Responses input item")),
    };
    Ok(Message { role, content })
}

/// Decode a native request into the shared conversation representation.
pub fn decode_request(v: &Value) -> Result<Request, Invalid> {
    if v.get("previous_response_id").is_some_and(|v| !v.is_null())
        || v.get("conversation").is_some_and(|v| !v.is_null())
    {
        return Err(Invalid::new(
            "stored conversations cannot be translated; send the conversation history",
        ));
    }
    let mut r = Request {
        model: string(v, "model"),
        stream: v["stream"].as_bool().unwrap_or(false),
        max_output_tokens: v["max_output_tokens"].as_u64(),
        temperature: number(v, "temperature"),
        top_p: number(v, "top_p"),
        parallel_tools: v["parallel_tool_calls"].as_bool(),
        output_schema: v.pointer("/text/format/schema").cloned(),
        ..Request::default()
    };
    if let Some(s) = v["instructions"].as_str() {
        r.system.push(Block::text(s));
    }
    if let Some(s) = v["input"].as_str() {
        r.messages.push(Message {
            role: Role::User,
            content: vec![Block::text(s)],
        });
    } else {
        let input = v["input"]
            .as_array()
            .ok_or_else(|| Invalid::new("input must be text or an array"))?;
        for item in input {
            if kind(item) == "additional_tools" {
                tools::decode(array(item, "tools"), None, &mut r.tools)?;
                continue;
            }
            let m = input_item(item)?;
            if m.role == Role::System {
                r.system.extend(m.content);
            } else {
                r.messages.push(m);
            }
        }
    }
    tools::decode(array(v, "tools"), None, &mut r.tools)?;
    r.tool_choice = match v["tool_choice"].as_str() {
        Some("auto") => Some(ToolChoice::Auto),
        Some("required") => Some(ToolChoice::Required),
        Some("none") => Some(ToolChoice::None),
        _ => optional_string(&v["tool_choice"], "name").map(ToolChoice::Named),
    };
    r.thinking = Thinking {
        enabled: v.get("reasoning").is_some() && v["reasoning"]["effort"] != "none",
        effort: optional_string(&v["reasoning"], "effort"),
        visible: v["reasoning"]["summary"]
            .as_str()
            .is_some_and(|s| s != "none"),
        budget: None,
    };
    Ok(r)
}

fn encode_content(blocks: &[Block], assistant: bool) -> Result<Value, Invalid> {
    let out: Result<Vec<_>, Invalid> = blocks
        .iter()
        .map(|b| {
            Ok(match &b.content {
                Content::Text(t) => {
                    if assistant {
                        json!({"type":"output_text","text":t,"annotations":[]})
                    } else {
                        json!({"type":"input_text","text":t})
                    }
                }
                Content::Image(m) => {
                    let mut v = json!({"type":"input_image","image_url":m.url()});
                    if let Some(d) = &m.detail {
                        v["detail"] = d.clone().into();
                    }
                    v
                }
                Content::File(m) => match &m.source {
                    MediaSource::Inline(_) => {
                        json!({"type":"input_file","file_data":m.url(),"filename":"attachment"})
                    }
                    MediaSource::Uri(u) => json!({"type":"input_file","file_url":u}),
                },
                _ => return Err(Invalid::new("non-content block in Responses message")),
            })
        })
        .collect();
    Ok(Value::Array(out?))
}

pub(crate) fn encode_item(
    block: &Block,
    index: usize,
    history: bool,
) -> Result<Option<Value>, Invalid> {
    let prefix = match &block.content {
        Content::ToolCall { .. } => "fc",
        Content::Reasoning { .. } => "rs",
        _ => "msg",
    };
    let id = format!("{prefix}_{index}");
    Ok(Some(match &block.content {
        Content::ToolCall {
            id: call,
            name,
            arguments,
        } => {
            json!({"type":"function_call","id":id,"call_id":call,"name":name,"arguments":arguments,"status":"completed"})
        }
        Content::ToolResult {
            call_id, content, ..
        } => {
            json!({"type":"function_call_output","call_id":call_id,"output":encode_content(content,false)?})
        }
        Content::Reasoning {
            text,
            id: reasoning_id,
            ..
        } => {
            let opaque = block
                .opaque_for(Family::OpenAi)
                .filter(|o| o.kind == OpaqueKind::Encrypted);
            if opaque.is_none() && text.is_empty() {
                return Ok(None);
            }
            // Only native encrypted state is replayed as a reasoning item. A
            // visible foreign summary is an ordinary assistant message.
            if history && opaque.is_none() {
                return Ok(Some(
                    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]}),
                ));
            }
            let mut v = json!({"type":"reasoning","id":reasoning_id.as_deref().unwrap_or(&id),"summary":if text.is_empty() {vec![]} else {vec![json!({"type":"summary_text","text":text})]}});
            if let Some(o) = opaque {
                v["encrypted_content"] = o.value.clone().into();
            }
            v
        }
        _ => {
            json!({"type":"message","id":id,"role":"assistant","status":"completed","content":encode_content(std::slice::from_ref(block),true)?})
        }
    }))
}

/// Encode a request, dropping opaque state from foreign model families.
pub fn encode_request(r: &Request) -> Result<Value, Invalid> {
    let mut input = Vec::new();
    if !r.system.is_empty() {
        input.push(
            json!({"type":"message","role":"system","content":encode_content(&r.system,false)?}),
        );
    }
    for m in &r.messages {
        let mut normal = Vec::new();
        for b in &m.content {
            if matches!(
                b.content,
                Content::ToolCall { .. } | Content::ToolResult { .. } | Content::Reasoning { .. }
            ) {
                if !normal.is_empty() {
                    input.push(json!({"type":"message","role":role_name(m.role),"content":encode_content(&normal,m.role==Role::Assistant)?}));
                    normal.clear();
                }
                if let Some(i) = encode_item(b, input.len(), true)? {
                    input.push(i);
                }
            } else {
                normal.push(b.clone());
            }
        }
        if !normal.is_empty() {
            input.push(json!({"type":"message","role":role_name(m.role),"content":encode_content(&normal,m.role==Role::Assistant)?}));
        }
    }
    let mut v = json!({"model":r.model,"stream":r.stream,"input":input,"store":false});
    if let Some(n) = r.max_output_tokens {
        v["max_output_tokens"] = n.into();
    }
    if let Some(n) = &r.temperature {
        v["temperature"] = n.clone().into();
    }
    if let Some(n) = &r.top_p {
        v["top_p"] = n.clone().into();
    }
    if let Some(p) = r.parallel_tools {
        v["parallel_tool_calls"] = p.into();
    }
    if !r.tools.is_empty() {
        v["tools"] = Value::Array(tools::encode(&r.tools));
    }
    let mut bindings = Bindings::new(&r.tools);
    for item in v["input"].as_array_mut().expect("request input") {
        bindings.item(item)?;
    }
    if let Some(c) = &r.tool_choice {
        v["tool_choice"] = match c {
            ToolChoice::Auto => "auto".into(),
            ToolChoice::Required => "required".into(),
            ToolChoice::None => "none".into(),
            ToolChoice::Named(n) => json!({"type":"function","name":n}),
        };
    }
    if r.thinking.enabled || r.thinking.effort.is_some() {
        v["reasoning"] = json!({});
        if let Some(e) = &r.thinking.effort {
            v["reasoning"]["effort"] = if e == "max" {
                "xhigh".into()
            } else {
                e.clone().into()
            };
        }
        if r.thinking.visible {
            v["reasoning"]["summary"] = "auto".into();
        }
        v["include"] = json!(["reasoning.encrypted_content"]);
    }
    if let Some(schema) = &r.output_schema {
        v["text"]["format"] =
            json!({"type":"json_schema","name":"output","schema":schema,"strict":false});
    }
    Ok(v)
}

pub(crate) fn usage(v: &Value) -> Usage {
    Usage {
        input: v["input_tokens"].as_u64(),
        output: v["output_tokens"].as_u64(),
        cache_read: v
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        cache_write: v
            .pointer("/input_tokens_details/cache_write_tokens")
            .or_else(|| v.pointer("/input_tokens_details/cache_creation_tokens"))
            .and_then(Value::as_u64),
        reasoning: v
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
    }
}

pub(crate) fn encode_usage(u: Usage) -> Value {
    let mut v = json!({});
    if let Some(n) = u.input {
        v["input_tokens"] = n.into();
    }
    if let Some(n) = u.output {
        v["output_tokens"] = n.into();
    }
    if let (Some(i), Some(o)) = (u.input, u.output) {
        v["total_tokens"] = i.saturating_add(o).into();
    }
    if let Some(n) = u.cache_read {
        v["input_tokens_details"]["cached_tokens"] = n.into();
    }
    if let Some(n) = u.cache_write {
        v["input_tokens_details"]["cache_write_tokens"] = n.into();
    }
    if let Some(n) = u.reasoning {
        v["output_tokens_details"]["reasoning_tokens"] = n.into();
    }
    v
}

/// Decode a completed native response and normalize token usage.
pub fn decode_response(v: &Value) -> Result<Response, Invalid> {
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return Ok(Response {
            error: Some(super::decode_error(500, v)),
            ..Response::default()
        });
    }
    let mut content = Vec::new();
    for item in array(v, "output") {
        content.extend(input_item(item)?.content);
    }
    let stop = match v["status"].as_str() {
        Some("incomplete") => Some(
            if v["incomplete_details"]["reason"] == "max_output_tokens" {
                StopReason::MaxTokens
            } else {
                StopReason::Refusal
            },
        ),
        Some("completed") => Some(
            if content
                .iter()
                .any(|b| matches!(b.content, Content::ToolCall { .. }))
            {
                StopReason::ToolCall
            } else {
                StopReason::EndTurn
            },
        ),
        _ => None,
    };
    Ok(Response {
        id: string(v, "id"),
        model: string(v, "model"),
        content,
        usage: usage(&v["usage"]),
        stop,
        error: None,
    })
}

/// Encode a completed response in this protocol.
pub fn encode_response(r: &Response) -> Result<Value, Invalid> {
    if let Some(e) = &r.error {
        return Ok(
            json!({"id":r.id,"object":"response","status":"failed","output":[],"error":{"code":e.code,"message":e.message}}),
        );
    }
    let output: Result<Vec<_>, _> = r
        .content
        .iter()
        .enumerate()
        .filter_map(|(i, b)| encode_item(b, i, false).transpose())
        .collect();
    let incomplete = match r.stop {
        Some(StopReason::MaxTokens) => Some("max_output_tokens"),
        Some(StopReason::Refusal) => Some("content_filter"),
        _ => None,
    };
    Ok(
        json!({"id":r.id,"object":"response","created_at":0,"status":if incomplete.is_some(){"incomplete"}else{"completed"},"model":r.model,"output":output?,"usage":encode_usage(r.usage),"error":null,"incomplete_details":incomplete.map(|r|json!({"reason":r}))}),
    )
}

#[derive(Default)]
/// Incremental decoder for this protocol's response events.
pub struct Decoder {
    blocks: BTreeMap<(usize, usize), usize>,
    ended_blocks: BTreeSet<usize>,
    started: bool,
    ended: bool,
    seen_tools: bool,
    opaque: BTreeSet<usize>,
}

impl Decoder {
    fn start(&mut self, key: (usize, usize), block: Block, out: &mut Vec<Event>) -> usize {
        if let Some(i) = self.blocks.get(&key) {
            return *i;
        }
        let index = self.blocks.len();
        self.blocks.insert(key, index);
        if block.opaque.is_some() {
            self.opaque.insert(index);
        }
        out.push(Event::BlockStart { index, block });
        index
    }
    fn end(&mut self, index: usize, out: &mut Vec<Event>) {
        if self.ended_blocks.insert(index) {
            out.push(Event::BlockEnd { index });
        }
    }

    /// Decode one native event into zero or more IR events.
    pub fn decode(&mut self, v: &Value) -> Result<Vec<Event>, Invalid> {
        let mut out = Vec::new();
        let oi = v["output_index"].as_u64().unwrap_or(0) as usize;
        let ci = v["content_index"].as_u64().unwrap_or(0) as usize;
        match kind(v) {
            "response.created" | "response.in_progress" => {
                if !self.started {
                    self.started = true;
                    out.push(Event::Start {
                        id: string(&v["response"], "id"),
                        model: string(&v["response"], "model"),
                    });
                }
            }
            "response.output_item.added" => {
                let item = &v["item"];
                match kind(item) {
                    "function_call" => {
                        self.seen_tools = true;
                        self.start(
                            (oi, 0),
                            Block::new(Content::ToolCall {
                                id: string(item, "call_id"),
                                name: string(item, "name"),
                                arguments: string(item, "arguments"),
                            }),
                            &mut out,
                        );
                    }
                    "reasoning" => {
                        self.start((oi, 0), reasoning(item), &mut out);
                    }
                    _ => {}
                }
            }
            "response.content_part.added" => {
                for b in content(&json!([v["part"]]))? {
                    self.start((oi, ci), b, &mut out);
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let index = self.start((oi, ci), Block::text(""), &mut out);
                out.push(Event::Delta {
                    index,
                    delta: Delta::Text(string(v, "delta")),
                });
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let index = self.start(
                    (oi, 0),
                    Block::new(Content::Reasoning {
                        text: String::new(),
                        family: Family::OpenAi,
                        id: optional_string(v, "item_id"),
                    }),
                    &mut out,
                );
                out.push(Event::Delta {
                    index,
                    delta: Delta::Reasoning(string(v, "delta")),
                });
            }
            "response.function_call_arguments.delta" => {
                let index = *self
                    .blocks
                    .get(&(oi, 0))
                    .ok_or_else(|| Invalid::new("Responses arguments before function call"))?;
                out.push(Event::Delta {
                    index,
                    delta: Delta::Arguments(string(v, "delta")),
                });
            }
            "response.output_item.done" => {
                let item = &v["item"];
                if !self.blocks.keys().any(|(i, _)| *i == oi) {
                    for (ci, b) in input_item(item)?.content.into_iter().enumerate() {
                        self.start((oi, ci), b, &mut out);
                    }
                }
                let indexes: Vec<_> = self
                    .blocks
                    .iter()
                    .filter(|((i, _), _)| *i == oi)
                    .map(|(_, i)| *i)
                    .collect();
                for index in indexes {
                    if let Some(value) =
                        optional_string(item, "encrypted_content").filter(|s| !s.is_empty())
                        && self.opaque.insert(index)
                    {
                        out.push(Event::Delta {
                            index,
                            delta: Delta::Opaque(Opaque {
                                family: Family::OpenAi,
                                kind: OpaqueKind::Encrypted,
                                value,
                            }),
                        });
                    }
                    self.end(index, &mut out);
                }
            }
            "response.completed" | "response.incomplete" => {
                let r = decode_response(&v["response"])?;
                if !self.started {
                    self.started = true;
                    out.push(Event::Start {
                        id: r.id.clone(),
                        model: r.model.clone(),
                    });
                }
                // Some relays send only the final snapshot.
                if self.blocks.is_empty() {
                    for (i, b) in r.content.into_iter().enumerate() {
                        self.start((i, 0), b, &mut out);
                    }
                }
                for index in 0..self.blocks.len() {
                    self.end(index, &mut out);
                }
                out.push(Event::Usage(r.usage));
                out.push(Event::Stop(r.stop.unwrap_or(if self.seen_tools {
                    StopReason::ToolCall
                } else {
                    StopReason::EndTurn
                })));
                out.push(Event::End);
                self.ended = true;
            }
            "error" | "response.failed" => {
                self.ended = true;
                out.push(Event::Error(super::decode_error(
                    500,
                    v.get("response").unwrap_or(v),
                )));
            }
            _ => {}
        }
        Ok(out)
    }

    /// Whether the stream has already received a terminal event.
    pub fn ended(&self) -> bool {
        self.ended
    }
}

#[derive(Default)]
/// Incremental encoder for this protocol's response events.
pub struct Encoder {
    state: Accumulator,
    sequence: u64,
    items: BTreeMap<usize, Value>,
    closed: BTreeSet<usize>,
    bindings: Bindings,
}

impl Encoder {
    pub(crate) fn with_tools(mut self, tools: &[Tool]) -> Self {
        self.bindings = Bindings::new(tools);
        self
    }

    /// Encode one IR event into zero or more native events.
    pub fn encode(&mut self, e: &Event) -> Result<Vec<Value>, Invalid> {
        self.state.push(e)?;
        let mut out = Vec::new();
        match e {
            Event::Start {id,model}=>out.push(json!({"type":"response.created","response":{"id":id,"object":"response","created_at":0,"status":"in_progress","model":model,"output":[],"error":null}})),
            Event::BlockStart {index,block}=>{
                let mut initial=block.clone();
                let delta=match &mut initial.content {Content::Text(t)=>Some(Delta::Text(std::mem::take(t))),Content::Reasoning {text,..}=>Some(Delta::Reasoning(std::mem::take(text))),Content::ToolCall {arguments,..}=>Some(Delta::Arguments(std::mem::take(arguments))),_=>None};
                // Empty reasoning items still have a lifecycle and can receive
                // summary or encrypted content later.
                let mut item=encode_item(&initial,*index,false)?.unwrap_or_else(||json!({"type":"reasoning","id":format!("rs_{index}"),"summary":[]}));
                item["status"]="in_progress".into();
                if item["type"]=="message" {item["content"]=json!([]);}
                self.items.insert(*index,item.clone());
                out.push(json!({"type":"response.output_item.added","output_index":index,"item":item}));
                if matches!(initial.content,Content::Text(_)) {out.push(json!({"type":"response.content_part.added","output_index":index,"item_id":item["id"],"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}));}
                if matches!(initial.content,Content::Reasoning {..}) {out.push(json!({"type":"response.reasoning_summary_part.added","output_index":index,"item_id":item["id"],"summary_index":0,"part":{"type":"summary_text","text":""}}));}
                if matches!(initial.content,Content::Image(_)|Content::File(_)) {out.push(json!({"type":"response.content_part.added","output_index":index,"item_id":item["id"],"content_index":0,"part":encode_content(std::slice::from_ref(block),true)?[0]}));}
                if let Some(d)=delta {out.extend(self.delta(*index,&d));}
            }
            Event::Delta {index,delta}=>out.extend(self.delta(*index,delta)),
            Event::BlockEnd {index}=>out.extend(self.close(*index)?),
            Event::End=>{
                let indexes:Vec<_>=self.items.keys().copied().collect();for i in indexes {out.extend(self.close(i)?);}
                let response=encode_response(&self.state.response)?;
                out.push(json!({"type":if response["status"]=="incomplete" {"response.incomplete"}else{"response.completed"},"response":response}));
            }
            Event::Error(e)=>out.push(json!({"type":"response.failed","response":{"id":self.state.response.id,"object":"response","status":"failed","output":[],"error":{"code":e.code,"message":e.message}}})),
            Event::Usage(_) | Event::Stop(_)=>{}
        }
        let mut out = self.bindings.events(out)?;
        for v in &mut out {
            v["sequence_number"] = self.sequence.into();
            self.sequence += 1;
        }
        Ok(out)
    }

    fn delta(&self, index: usize, d: &Delta) -> Vec<Value> {
        let Some(item) = self.items.get(&index) else {
            return vec![];
        };
        let (kind, text) = match d {
            Delta::Text(t) => ("response.output_text.delta", t),
            Delta::Reasoning(t) => ("response.reasoning_summary_text.delta", t),
            Delta::Arguments(t) => ("response.function_call_arguments.delta", t),
            Delta::Opaque(_) => return vec![],
        };
        if text.is_empty() {
            return vec![];
        }
        let mut v = json!({"type":kind,"output_index":index,"item_id":item["id"],"delta":text});
        if matches!(d, Delta::Text(_)) {
            v["content_index"] = 0.into();
        }
        if matches!(d, Delta::Reasoning(_)) {
            v["summary_index"] = 0.into();
        }
        vec![v]
    }

    fn close(&mut self, index: usize) -> Result<Vec<Value>, Invalid> {
        if !self.closed.insert(index) {
            return Ok(vec![]);
        }
        let Some(block) = self.state.blocks.get(&index) else {
            return Ok(vec![]);
        };
        let Some(mut item) = encode_item(block, index, false)? else {
            return Ok(vec![]);
        };
        item["status"] = "completed".into();
        let mut out = Vec::new();
        match &block.content {
            Content::Text(text)=>{
                out.push(json!({"type":"response.output_text.done","output_index":index,"item_id":item["id"],"content_index":0,"text":text}));
                out.push(json!({"type":"response.content_part.done","output_index":index,"item_id":item["id"],"content_index":0,"part":item["content"][0]}));
            }
            Content::ToolCall {arguments,..}=>out.push(json!({"type":"response.function_call_arguments.done","output_index":index,"item_id":item["id"],"arguments":arguments})),
            Content::Reasoning {text,..}=>out.push(json!({"type":"response.reasoning_summary_text.done","output_index":index,"item_id":item["id"],"summary_index":0,"text":text})),
            _=>{}
        }
        out.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
        Ok(out)
    }
}
