//! Gemini v1beta generateContent and streamGenerateContent (alt=sse).
//! Accepts both protobuf JSON camelCase and the documented snake_case aliases.

use super::ir::*;
use super::{Invalid, array, number, optional_string, string, strings};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn field<'a>(v: &'a Value, camel: &str, snake: &str) -> &'a Value {
    v.get(camel)
        .or_else(|| v.get(snake))
        .unwrap_or(&Value::Null)
}

#[derive(Default)]
struct Calls {
    next: usize,
    pending: BTreeMap<String, Vec<String>>,
}

impl Calls {
    fn call(&mut self, v: &Value) -> (String, String) {
        let name = string(v, "name");
        let id = optional_string(v, "id")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let id = format!("gemini_call_{}", self.next);
                self.next += 1;
                id
            });
        self.pending
            .entry(name.clone())
            .or_default()
            .push(id.clone());
        (id, name)
    }
    fn result(&mut self, v: &Value) -> Result<(String, String), Invalid> {
        let name = string(v, "name");
        let ids = self.pending.entry(name.clone()).or_default();
        if let Some(id) = optional_string(v, "id").filter(|s| !s.is_empty()) {
            ids.retain(|i| i != &id);
            return Ok((id, name));
        }
        if ids.len() != 1 {
            return Err(Invalid::new(
                "Gemini function response needs an id when calls with the same name are ambiguous",
            ));
        }
        Ok((ids.remove(0), name))
    }
}

fn decode_part(v: &Value, calls: &mut Calls) -> Result<Block, Invalid> {
    let inline = field(v, "inlineData", "inline_data");
    let file = field(v, "fileData", "file_data");
    let fc = field(v, "functionCall", "function_call");
    let fr = field(v, "functionResponse", "function_response");
    let content = if let Some(text) = v["text"].as_str() {
        if v["thought"].as_bool() == Some(true) {
            Content::Reasoning {
                text: text.into(),
                family: Family::Google,
                id: None,
            }
        } else {
            Content::Text(text.into())
        }
    } else if !inline.is_null() || !file.is_null() {
        let src = if inline.is_null() { file } else { inline };
        let mime_type = field(src, "mimeType", "mime_type")
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let source = if inline.is_null() {
            MediaSource::Uri(
                field(file, "fileUri", "file_uri")
                    .as_str()
                    .unwrap_or_default()
                    .into(),
            )
        } else {
            MediaSource::Inline(string(inline, "data"))
        };
        let image = mime_type.starts_with("image/");
        let media = Media {
            mime_type,
            source,
            detail: None,
        };
        if image {
            Content::Image(media)
        } else {
            Content::File(media)
        }
    } else if !fc.is_null() {
        let (id, name) = calls.call(fc);
        Content::ToolCall {
            id,
            name,
            arguments: fc
                .get("args")
                .map(Value::to_string)
                .unwrap_or_else(|| "{}".into()),
        }
    } else if !fr.is_null() {
        let (call_id, name) = calls.result(fr)?;
        let response = &fr["response"];
        let text = if let Some(s) = response["output"].as_str() {
            s.to_owned()
        } else if let Some(s) = response["error"].as_str() {
            s.to_owned()
        } else {
            response.to_string()
        };
        let mut content = vec![Block::text(text)];
        for p in array(fr, "parts") {
            content.push(decode_part(p, calls)?);
        }
        Content::ToolResult {
            call_id,
            name: Some(name),
            content,
            is_error: response.get("error").is_some(),
        }
    } else if !field(v, "thoughtSignature", "thought_signature").is_null() {
        Content::Reasoning {
            text: String::new(),
            family: Family::Google,
            id: None,
        }
    } else {
        return Err(Invalid::new("unsupported Gemini part"));
    };
    let opaque = field(v, "thoughtSignature", "thought_signature")
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| Opaque {
            family: Family::Google,
            kind: OpaqueKind::Signature,
            value: s.into(),
        });
    Ok(Block { content, opaque })
}

pub(crate) fn encode_part(b: &Block) -> Result<Option<Value>, Invalid> {
    let mut v = match &b.content {
        Content::Text(t) => json!({"text":t}),
        Content::Reasoning { text, .. } => {
            if text.is_empty() && b.opaque_for(Family::Google).is_none() {
                return Ok(None);
            }
            json!({"text":text,"thought":true})
        }
        Content::Image(m) | Content::File(m) => {
            let mime = if m.mime_type.is_empty() && matches!(b.content, Content::Image(_)) {
                "image/png"
            } else {
                &m.mime_type
            };
            match &m.source {
                MediaSource::Inline(data) => json!({"inlineData":{"mimeType":mime,"data":data}}),
                MediaSource::Uri(uri) => json!({"fileData":{"mimeType":mime,"fileUri":uri}}),
            }
        }
        Content::ToolCall {
            id,
            name,
            arguments,
        } => json!({"functionCall":{"id":id,"name":name,"args":super::json_arguments(arguments)?}}),
        Content::ToolResult {
            call_id,
            name,
            content,
            is_error,
        } => {
            let name = name.as_ref().ok_or_else(|| {
                Invalid::new("Gemini function response has no matching function name")
            })?;
            let text = super::text_content(content);
            let response = if *is_error {
                json!({"error":text})
            } else {
                json!({"output":text})
            };
            let mut v = json!({"functionResponse":{"id":call_id,"name":name,"response":response}});
            let media: Result<Vec<_>, _> = content
                .iter()
                .filter(|b| !matches!(b.content, Content::Text(_)))
                .filter_map(|b| encode_part(b).transpose())
                .collect();
            let media = media?;
            if !media.is_empty() {
                v["functionResponse"]["parts"] = media.into();
            }
            v
        }
    };
    if let Some(o) = b
        .opaque_for(Family::Google)
        .filter(|o| o.kind == OpaqueKind::Signature)
    {
        v["thoughtSignature"] = o.value.clone().into();
    }
    Ok(Some(v))
}

fn encode_parts(blocks: &[Block]) -> Result<Vec<Value>, Invalid> {
    blocks
        .iter()
        .filter_map(|b| encode_part(b).transpose())
        .collect()
}

pub fn decode_request(v: &Value) -> Result<Request, Invalid> {
    let contents = v["contents"]
        .as_array()
        .ok_or_else(|| Invalid::new("contents must be an array"))?;
    let config = field(v, "generationConfig", "generation_config");
    if field(config, "candidateCount", "candidate_count")
        .as_u64()
        .is_some_and(|n| n != 1)
    {
        return Err(Invalid::new(
            "translation supports one candidate per request",
        ));
    }
    let mut r = Request {
        model: string(v, "model").trim_start_matches("models/").to_owned(),
        stream: v["stream"].as_bool().unwrap_or(false),
        max_output_tokens: field(config, "maxOutputTokens", "max_output_tokens").as_u64(),
        temperature: number(config, "temperature"),
        top_p: field(config, "topP", "top_p").as_number().cloned(),
        stop: strings(field(config, "stopSequences", "stop_sequences")),
        output_schema: config
            .get("responseJsonSchema")
            .or_else(|| config.get("response_json_schema"))
            .or_else(|| config.get("responseSchema"))
            .or_else(|| config.get("response_schema"))
            .cloned(),
        ..Request::default()
    };
    let mut calls = Calls::default();
    for p in array(field(v, "systemInstruction", "system_instruction"), "parts") {
        r.system.push(decode_part(p, &mut calls)?);
    }
    for m in contents {
        let role = match m["role"].as_str().unwrap_or("user") {
            "user" => Role::User,
            "model" => Role::Assistant,
            _ => return Err(Invalid::new("unsupported Gemini content role")),
        };
        let content = array(m, "parts")
            .iter()
            .map(|p| decode_part(p, &mut calls))
            .collect::<Result<_, _>>()?;
        r.messages.push(Message { role, content });
    }
    for group in array(v, "tools") {
        let definitions = field(group, "functionDeclarations", "function_declarations").as_array();
        if definitions.is_none() {
            return Err(Invalid::new(
                "only Gemini function declarations can be translated",
            ));
        }
        for t in definitions.into_iter().flatten() {
            r.tools.push(Tool {
                name: string(t, "name"),
                description: optional_string(t, "description"),
                parameters: t
                    .get("parametersJsonSchema")
                    .or_else(|| t.get("parameters_json_schema"))
                    .or_else(|| t.get("parameters"))
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object","properties":{}})),
                strict: None,
            });
        }
    }
    let tc = field(
        field(v, "toolConfig", "tool_config"),
        "functionCallingConfig",
        "function_calling_config",
    );
    r.tool_choice = match tc["mode"].as_str() {
        Some("AUTO") => Some(ToolChoice::Auto),
        Some("NONE") => Some(ToolChoice::None),
        Some("ANY") => {
            let allowed = strings(field(tc, "allowedFunctionNames", "allowed_function_names"));
            if allowed.len() > 1 {
                return Err(Invalid::new(
                    "multiple allowed Gemini functions cannot be translated",
                ));
            }
            Some(
                allowed
                    .into_iter()
                    .next()
                    .map(ToolChoice::Named)
                    .unwrap_or(ToolChoice::Required),
            )
        }
        _ => None,
    };
    let thinking = field(config, "thinkingConfig", "thinking_config");
    let budget = field(thinking, "thinkingBudget", "thinking_budget").as_i64();
    r.thinking = Thinking {
        enabled: !thinking.is_null() && budget != Some(0),
        budget,
        effort: field(thinking, "thinkingLevel", "thinking_level")
            .as_str()
            .map(str::to_ascii_lowercase),
        visible: field(thinking, "includeThoughts", "include_thoughts")
            .as_bool()
            .unwrap_or(false),
    };
    Ok(r)
}

/// Model and stream live in the URL, not in generateContent's body. The hub
/// supplies those two fields to the decoder from the authenticated ingress.
pub fn encode_request(r: &Request) -> Result<Value, Invalid> {
    let mut contents = Vec::new();
    let mut system = r.system.clone();
    for m in &r.messages {
        if m.role == Role::System {
            system.extend(m.content.clone());
            continue;
        }
        let parts = encode_parts(&m.content)?;
        if !parts.is_empty() {
            contents.push(
                json!({"role":if m.role==Role::Assistant{"model"}else{"user"},"parts":parts}),
            );
        }
    }
    let mut v = json!({"contents":contents,"model":r.model,"stream":r.stream});
    if !system.is_empty() {
        v["systemInstruction"] = json!({"parts":encode_parts(&system)?});
    }
    let mut config = json!({});
    if let Some(n) = r.max_output_tokens {
        config["maxOutputTokens"] = n.into();
    }
    if let Some(n) = &r.temperature {
        config["temperature"] = n.clone().into();
    }
    if let Some(n) = &r.top_p {
        config["topP"] = n.clone().into();
    }
    if !r.stop.is_empty() {
        config["stopSequences"] = json!(r.stop);
    }
    if let Some(schema) = &r.output_schema {
        config["responseMimeType"] = "application/json".into();
        config["responseJsonSchema"] = schema.clone();
    }
    if r.thinking.enabled || r.thinking.budget.is_some() || r.thinking.effort.is_some() {
        let mut t = json!({"includeThoughts":r.thinking.visible});
        if let Some(n) = r.thinking.budget {
            t["thinkingBudget"] = n.into();
        } else if let Some(e) = &r.thinking.effort {
            t["thinkingLevel"] = e.to_ascii_uppercase().into();
        } else {
            t["thinkingBudget"] = (-1).into();
        }
        config["thinkingConfig"] = t;
    }
    if config.as_object().is_some_and(|o| !o.is_empty()) {
        v["generationConfig"] = config;
    }
    if !r.tools.is_empty() {
        v["tools"] = json!([{"functionDeclarations":r.tools.iter().map(|t|{let mut v=json!({"name":t.name,"parametersJsonSchema":t.parameters});if let Some(d)=&t.description{v["description"]=d.clone().into();}v}).collect::<Vec<_>>()}]);
    }
    if let Some(c) = &r.tool_choice {
        v["toolConfig"]["functionCallingConfig"] = match c {
            ToolChoice::Auto => json!({"mode":"AUTO"}),
            ToolChoice::Required => json!({"mode":"ANY"}),
            ToolChoice::None => json!({"mode":"NONE"}),
            ToolChoice::Named(n) => json!({"mode":"ANY","allowedFunctionNames":[n]}),
        };
    }
    Ok(v)
}

pub(crate) fn usage(v: &Value) -> Usage {
    let reasoning = field(v, "thoughtsTokenCount", "thoughts_token_count").as_u64();
    Usage {
        input: field(v, "promptTokenCount", "prompt_token_count").as_u64(),
        output: field(v, "candidatesTokenCount", "candidates_token_count")
            .as_u64()
            .map(|n| n.saturating_add(reasoning.unwrap_or(0))),
        cache_read: field(v, "cachedContentTokenCount", "cached_content_token_count").as_u64(),
        cache_write: None,
        reasoning,
    }
}

pub(crate) fn encode_usage(u: Usage) -> Value {
    let mut v = json!({});
    if let Some(n) = u.input {
        v["promptTokenCount"] = n.into();
    }
    if let Some(n) = u.output {
        v["candidatesTokenCount"] = n.saturating_sub(u.reasoning.unwrap_or(0)).into();
    }
    if let Some(n) = u.cache_read {
        v["cachedContentTokenCount"] = n.into();
    }
    if let Some(n) = u.reasoning {
        v["thoughtsTokenCount"] = n.into();
    }
    if let (Some(i), Some(o)) = (u.input, u.output) {
        v["totalTokenCount"] = i.saturating_add(o).into();
    }
    v
}

fn stop(s: &str, tools: bool) -> StopReason {
    match s {
        "STOP" => {
            if tools {
                StopReason::ToolCall
            } else {
                StopReason::EndTurn
            }
        }
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            StopReason::Refusal
        }
        s => StopReason::Other(s.into()),
    }
}
fn encode_stop(s: Option<&StopReason>) -> Value {
    match s {
        Some(StopReason::EndTurn | StopReason::ToolCall | StopReason::StopSequence(_)) => {
            "STOP".into()
        }
        Some(StopReason::MaxTokens) => "MAX_TOKENS".into(),
        Some(StopReason::Refusal) => "SAFETY".into(),
        Some(StopReason::Other(s)) => s.clone().into(),
        None => Value::Null,
    }
}

pub fn decode_response(v: &Value) -> Result<Response, Invalid> {
    if v.get("error").is_some() {
        return Ok(Response {
            error: Some(super::decode_error(
                v["error"]["code"].as_u64().unwrap_or(500) as u16,
                v,
            )),
            ..Response::default()
        });
    }
    let mut calls = Calls::default();
    let mut content = Vec::new();
    let c = array(v, "candidates").first();
    if let Some(c) = c {
        for p in array(&c["content"], "parts") {
            content.push(decode_part(p, &mut calls)?);
        }
    }
    let tools = content
        .iter()
        .any(|b| matches!(b.content, Content::ToolCall { .. }));
    let stop = c
        .and_then(|c| field(c, "finishReason", "finish_reason").as_str())
        .map(|s| stop(s, tools))
        .or_else(|| {
            field(v, "promptFeedback", "prompt_feedback")
                .get("blockReason")
                .map(|_| StopReason::Refusal)
        });
    Ok(Response {
        id: field(v, "responseId", "response_id")
            .as_str()
            .unwrap_or_default()
            .into(),
        model: field(v, "modelVersion", "model_version")
            .as_str()
            .unwrap_or_default()
            .into(),
        content,
        usage: usage(field(v, "usageMetadata", "usage_metadata")),
        stop,
        error: None,
    })
}

pub fn encode_response(r: &Response) -> Result<Value, Invalid> {
    if let Some(e) = &r.error {
        return Ok(super::encode_error(Protocol::Gemini, e));
    }
    Ok(
        json!({"responseId":r.id,"modelVersion":r.model,"candidates":[{"index":0,"content":{"role":"model","parts":encode_parts(&r.content)?},"finishReason":encode_stop(r.stop.as_ref())}],"usageMetadata":encode_usage(r.usage)}),
    )
}

#[derive(Default)]
pub struct Decoder {
    started: bool,
    next: usize,
    last: Option<(usize, bool)>,
    calls: Calls,
    tools: bool,
    ended: bool,
}

impl Decoder {
    pub fn decode(&mut self, v: &Value) -> Result<Vec<Event>, Invalid> {
        if v.get("error").is_some() {
            self.ended = true;
            return Ok(vec![Event::Error(super::decode_error(
                v["error"]["code"].as_u64().unwrap_or(500) as u16,
                v,
            ))]);
        }
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(Event::Start {
                id: field(v, "responseId", "response_id")
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                model: field(v, "modelVersion", "model_version")
                    .as_str()
                    .unwrap_or_default()
                    .into(),
            });
        }
        for c in array(v, "candidates") {
            if c["index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            for p in array(&c["content"], "parts") {
                let mut b = decode_part(p, &mut self.calls)?;
                let thinking = matches!(b.content, Content::Reasoning { .. });
                let text = match &mut b.content {
                    Content::Text(t) | Content::Reasoning { text: t, .. } => {
                        Some(std::mem::take(t))
                    }
                    _ => None,
                };
                if let Some(text) = text {
                    let signature = b.opaque.take();
                    let index = if let Some((i, th)) = self.last.filter(|(_, th)| *th == thinking) {
                        let _ = th;
                        i
                    } else {
                        let i = self.next;
                        self.next += 1;
                        out.push(Event::BlockStart { index: i, block: b });
                        self.last = Some((i, thinking));
                        i
                    };
                    if !text.is_empty() {
                        out.push(Event::Delta {
                            index,
                            delta: if thinking {
                                Delta::Reasoning(text)
                            } else {
                                Delta::Text(text)
                            },
                        });
                    }
                    if let Some(o) = signature {
                        out.push(Event::Delta {
                            index,
                            delta: Delta::Opaque(o),
                        });
                    }
                } else {
                    self.last = None;
                    let index = self.next;
                    self.next += 1;
                    self.tools |= matches!(b.content, Content::ToolCall { .. });
                    out.push(Event::BlockStart { index, block: b });
                }
            }
            if let Some(reason) = field(c, "finishReason", "finish_reason").as_str() {
                out.push(Event::Stop(stop(reason, self.tools)));
            }
        }
        let u = usage(field(v, "usageMetadata", "usage_metadata"));
        if u != Usage::default() {
            out.push(Event::Usage(u));
        }
        if field(v, "promptFeedback", "prompt_feedback")
            .get("blockReason")
            .is_some()
        {
            out.push(Event::Stop(StopReason::Refusal));
        }
        Ok(out)
    }
    pub fn finish(&mut self) -> Vec<Event> {
        if std::mem::replace(&mut self.ended, true) {
            vec![]
        } else {
            (0..self.next)
                .map(|index| Event::BlockEnd { index })
                .chain(std::iter::once(Event::End))
                .collect()
        }
    }
}

#[derive(Default)]
pub struct Encoder {
    state: Accumulator,
    sent: BTreeMap<usize, bool>,
}

impl Encoder {
    fn chunk(&self, parts: Vec<Value>) -> Value {
        json!({"responseId":self.state.response.id,"modelVersion":self.state.response.model,"candidates":[{"index":0,"content":{"role":"model","parts":parts}}]})
    }
    pub fn encode(&mut self, e: &Event) -> Result<Vec<Value>, Invalid> {
        self.state.push(e)?;
        let mut out = Vec::new();
        match e {
            Event::Start { .. } => {}
            Event::BlockStart { index, block } => {
                if matches!(block.content, Content::ToolCall { .. }) {
                    self.sent.insert(*index, false);
                } else if let Some(p) = encode_part(block)? {
                    out.push(self.chunk(vec![p]));
                    self.sent.insert(*index, true);
                }
            }
            Event::Delta { index, delta } => match delta {
                Delta::Text(t) => out.push(self.chunk(vec![json!({"text":t})])),
                Delta::Reasoning(t) => out.push(self.chunk(vec![json!({"text":t,"thought":true})])),
                Delta::Opaque(o)
                    if o.family == Family::Google
                        && o.kind == OpaqueKind::Signature
                        && self.sent.get(index) == Some(&true) =>
                {
                    out.push(self.chunk(vec![json!({"thoughtSignature":o.value})]))
                }
                _ => {}
            },
            Event::BlockEnd { index } => {
                if self.sent.get(index) == Some(&false) {
                    if let Some(p) = self
                        .state
                        .blocks
                        .get(index)
                        .map(encode_part)
                        .transpose()?
                        .flatten()
                    {
                        out.push(self.chunk(vec![p]));
                    }
                    self.sent.insert(*index, true);
                }
            }
            Event::End => {
                let mut v = self.chunk(vec![]);
                v["candidates"][0]["finishReason"] = encode_stop(self.state.response.stop.as_ref());
                v["usageMetadata"] = encode_usage(self.state.response.usage);
                out.push(v);
            }
            Event::Error(e) => out.push(super::encode_error(Protocol::Gemini, e)),
            Event::Usage(_) | Event::Stop(_) => {}
        }
        Ok(out)
    }
}
