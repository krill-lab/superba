//! Codec contracts and reviewed stream snapshots for every directed pair.
use serde_json::{Value, json};
use superba_router::translate::{
    self,
    ir::*,
    stream::{Decoder, Encoder, Sse, Transcoder},
};

fn native_request(p: Protocol) -> Value {
    let schema =
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]});
    match p {
        Protocol::Messages => {
            json!({"model":"test","stream":true,"max_tokens":2048,"system":[{"type":"text","text":"Be precise."}],"messages":[
            {"role":"user","content":[{"type":"text","text":"Read this"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"cGl4ZWw="}}]},
            {"role":"assistant","content":[{"type":"thinking","thinking":"Inspect it","signature":"anthropic-signature"},{"type":"tool_use","id":"call_a","name":"read","input":{"path":"a.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_a","content":[{"type":"text","text":"hello"}],"is_error":false}]}
        ],"tools":[{"name":"read","description":"Read a file","input_schema":schema}],"tool_choice":{"type":"auto","disable_parallel_tool_use":false},"temperature":0.4,"top_p":0.9,"stop_sequences":["DONE"],"thinking":{"type":"enabled","budget_tokens":1024}})
        }
        Protocol::Responses => {
            json!({"model":"test","stream":true,"max_output_tokens":2048,"instructions":"Be precise.","input":[
            {"role":"user","content":[{"type":"input_text","text":"Read this"},{"type":"input_image","image_url":"data:image/png;base64,cGl4ZWw="}]},
            {"type":"reasoning","id":"rs_a","summary":[{"type":"summary_text","text":"Inspect it"}],"encrypted_content":"openai-ciphertext"},
            {"type":"function_call","call_id":"call_a","name":"read","arguments":"{\"path\":\"a.txt\"}"},
            {"type":"function_call_output","call_id":"call_a","output":[{"type":"input_text","text":"hello"}]}
        ],"tools":[{"type":"function","name":"read","description":"Read a file","parameters":schema,"strict":false}],"tool_choice":"auto","parallel_tool_calls":true,"reasoning":{"effort":"high","summary":"auto"}})
        }
        Protocol::Chat => {
            json!({"model":"test","stream":true,"max_completion_tokens":2048,"messages":[
            {"role":"system","content":"Be precise."},
            {"role":"user","content":[{"type":"text","text":"Read this"},{"type":"image_url","image_url":{"url":"data:image/png;base64,cGl4ZWw="}}]},
            {"role":"assistant","content":null,"reasoning_content":"Inspect it","tool_calls":[{"type":"function","id":"call_a","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]},
            {"role":"tool","tool_call_id":"call_a","content":"hello"}
        ],"tools":[{"type":"function","function":{"name":"read","description":"Read a file","parameters":schema,"strict":false}}],"tool_choice":"auto","parallel_tool_calls":true,"temperature":0.4,"top_p":0.9,"stop":["DONE"],"reasoning_effort":"high"})
        }
        Protocol::Gemini => {
            json!({"model":"test","stream":true,"systemInstruction":{"parts":[{"text":"Be precise."}]},"contents":[
            {"role":"user","parts":[{"text":"Read this"},{"inlineData":{"mimeType":"image/png","data":"cGl4ZWw="}},{"fileData":{"mimeType":"application/pdf","fileUri":"https://example.test/document.pdf"}}]},
            {"role":"model","parts":[{"text":"Inspect it","thought":true,"thoughtSignature":"google-signature"},{"functionCall":{"id":"call_a","name":"read","args":{"path":"a.txt"}},"thoughtSignature":"google-tool-signature"}]},
            {"role":"user","parts":[{"functionResponse":{"id":"call_a","name":"read","response":{"output":"hello"}}}]}
        ],"tools":[{"functionDeclarations":[{"name":"read","description":"Read a file","parametersJsonSchema":schema}]}],"toolConfig":{"functionCallingConfig":{"mode":"AUTO"}},"generationConfig":{"maxOutputTokens":2048,"temperature":0.4,"topP":0.9,"stopSequences":["DONE"],"thinkingConfig":{"thinkingBudget":1024,"includeThoughts":true}}})
        }
    }
}

fn request_roundtrip(p: Protocol) {
    let ir = translate::decode_request(p, &native_request(p)).unwrap();
    let encoded = translate::encode_request(p, &ir).unwrap();
    let actual = translate::decode_request(p, &encoded).unwrap();
    assert_eq!(actual, ir, "{p:?}: {encoded}");
}
#[test]
fn anthropic_request_roundtrip() {
    request_roundtrip(Protocol::Messages);
}
#[test]
fn responses_request_roundtrip() {
    request_roundtrip(Protocol::Responses);
}
#[test]
fn chat_request_roundtrip() {
    request_roundtrip(Protocol::Chat);
}
#[test]
fn gemini_request_roundtrip() {
    request_roundtrip(Protocol::Gemini);
}

#[test]
fn requests_convert_all_twelve_directions_without_foreign_opaque_state() {
    for from in Protocol::ALL {
        for to in Protocol::ALL {
            if from == to {
                continue;
            }
            let ir = translate::decode_request(from, &native_request(from)).unwrap();
            let wire = translate::encode_request(to, &ir).unwrap();
            let text = wire.to_string();
            if from.family() != to.family() {
                for secret in [
                    "anthropic-signature",
                    "openai-ciphertext",
                    "google-signature",
                    "google-tool-signature",
                ] {
                    assert!(!text.contains(secret), "{from:?}->{to:?} leaked {secret}");
                }
            }
            let decoded = translate::decode_request(to, &wire).unwrap();
            let call = decoded
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .find_map(|b| {
                    if let Content::ToolCall {
                        id,
                        name,
                        arguments,
                    } = &b.content
                    {
                        Some((id, name, arguments))
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(call.0, "call_a");
            assert_eq!(call.1, "read");
            assert_eq!(
                serde_json::from_str::<Value>(call.2).unwrap(),
                json!({"path":"a.txt"})
            );
            assert!(
                decoded.messages.iter().flat_map(|m| &m.content).any(
                    |b| matches!(&b.content,Content::ToolResult{call_id,..}if call_id=="call_a")
                )
            );
            assert!(decoded.messages.iter().flat_map(|m|&m.content).any(|b|matches!(&b.content,Content::Image(m)if m.source==MediaSource::Inline("cGl4ZWw=".into()))));
        }
    }
}

fn wire_stream(p: Protocol) -> (&'static str, &'static str) {
    match p {
        Protocol::Messages => (
            include_str!("fixtures/translate/messages.sse"),
            include_str!("fixtures/translate/messages-error.sse"),
        ),
        Protocol::Responses => (
            include_str!("fixtures/translate/responses.sse"),
            include_str!("fixtures/translate/responses-error.sse"),
        ),
        Protocol::Chat => (
            include_str!("fixtures/translate/chat.sse"),
            include_str!("fixtures/translate/chat-error.sse"),
        ),
        Protocol::Gemini => (
            include_str!("fixtures/translate/gemini-partial.sse"),
            include_str!("fixtures/translate/gemini-error.sse"),
        ),
    }
}

fn decode_events(p: Protocol, wire: &str) -> Vec<Event> {
    let mut parser = Sse::default();
    let mut decoder = Decoder::new(p);
    let mut events = Vec::new();
    for data in parser
        .push(wire.as_bytes())
        .unwrap()
        .into_iter()
        .chain(parser.finish().unwrap())
    {
        if data == b"[DONE]" {
            events.extend(decoder.finish());
        } else {
            events.extend(
                decoder
                    .decode(&serde_json::from_slice(&data).unwrap())
                    .unwrap(),
            );
        }
    }
    events.extend(decoder.finish());
    events
}

fn accumulate(events: &[Event]) -> Response {
    let mut state = Accumulator::default();
    for event in events {
        state.push(event).unwrap();
    }
    assert!(state.ended, "stream must terminate");
    state.response
}

fn response_roundtrip(p: Protocol) {
    let (success, error) = wire_stream(p);
    for wire in [success, error] {
        let original = accumulate(&decode_events(p, wire));
        let encoded = translate::encode_response(p, &original).unwrap();
        let decoded = translate::decode_response(p, &encoded).unwrap();
        assert_eq!(decoded, original, "{p:?} complete response: {encoded}");
    }
    for stop in [
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::Refusal,
    ] {
        let original = Response {
            id: "text-response".into(),
            model: "model".into(),
            content: vec![Block::text("Hello 世界")],
            stop: Some(stop),
            ..Default::default()
        };
        let encoded = translate::encode_response(p, &original).unwrap();
        assert_eq!(translate::decode_response(p, &encoded).unwrap(), original);
    }
}

fn stream_roundtrip(p: Protocol) {
    let (success, error) = wire_stream(p);
    for wire in [success, error] {
        let original = decode_events(p, wire);
        let mut encoder = Encoder::new(p);
        let mut bytes = Vec::new();
        for event in &original {
            for native in encoder.encode(event).unwrap() {
                translate::stream::frame(p, &native, &mut bytes);
            }
        }
        let decoded = decode_events(p, std::str::from_utf8(&bytes).unwrap());
        // Protocols group their text/tool deltas differently; compare the
        // complete IR, including order, ids, opaque state, usage and errors.
        assert_eq!(accumulate(&decoded), accumulate(&original), "{p:?} stream");
    }
    let events = vec![
        Event::Start {
            id: "text-response".into(),
            model: "model".into(),
        },
        Event::BlockStart {
            index: 0,
            block: Block::text(""),
        },
        Event::Delta {
            index: 0,
            delta: Delta::Text("Hello ".into()),
        },
        Event::Delta {
            index: 0,
            delta: Delta::Text("世界".into()),
        },
        Event::BlockEnd { index: 0 },
        Event::Usage(Usage {
            input: Some(0),
            output: Some(0),
            ..Default::default()
        }),
        Event::Stop(StopReason::EndTurn),
        Event::End,
    ];
    let mut encoder = Encoder::new(p);
    let mut bytes = Vec::new();
    for event in &events {
        for native in encoder.encode(event).unwrap() {
            translate::stream::frame(p, &native, &mut bytes);
        }
    }
    assert_eq!(
        accumulate(&decode_events(p, std::str::from_utf8(&bytes).unwrap())),
        accumulate(&events)
    );
}

macro_rules! roundtrips {
    ($response:ident, $stream:ident, $p:ident) => {
        #[test]
        fn $response() {
            response_roundtrip(Protocol::$p);
        }
        #[test]
        fn $stream() {
            stream_roundtrip(Protocol::$p);
        }
    };
}
roundtrips!(
    anthropic_response_roundtrip,
    anthropic_stream_roundtrip,
    Messages
);
roundtrips!(
    responses_response_roundtrip,
    responses_stream_roundtrip,
    Responses
);
roundtrips!(chat_response_roundtrip, chat_stream_roundtrip, Chat);
roundtrips!(gemini_response_roundtrip, gemini_stream_roundtrip, Gemini);

#[test]
fn gemini_partial_arguments_and_complete_calls_have_the_same_ir() {
    let partial = decode_events(Protocol::Gemini, wire_stream(Protocol::Gemini).0);
    let complete = decode_events(
        Protocol::Gemini,
        include_str!("fixtures/translate/gemini.sse"),
    );
    assert_eq!(accumulate(&partial), accumulate(&complete));
    assert!(partial.iter().any(|e| matches!(
        e,
        Event::Delta {
            delta: Delta::Arguments(_),
            ..
        }
    )));
}

#[test]
fn gemini_partial_arguments_are_not_executed_before_the_last_fragment() {
    let mut decoder = Decoder::new(Protocol::Gemini);
    let mut state = Accumulator::default();
    let chunk = |call: Value| json!({"candidates":[{"content":{"parts":[{"functionCall":call}]}}]});
    let first = chunk(
        json!({"id":"a","name":"read","willContinue":true,"partialArgs":[
        {"jsonPath":"$.files[0].path","stringValue":"dir/","willContinue":true},
        {"jsonPath":"$.enabled","boolValue":false},{"jsonPath":"$.limit","numberValue":0},
        {"jsonPath":"$.optional","nullValue":"NULL_VALUE"}]}),
    );
    for event in decoder.decode(&first).unwrap() {
        state.push(&event).unwrap();
    }
    assert!(
        matches!(&state.response.content[0].content,Content::ToolCall{arguments,..} if arguments.is_empty())
    );
    let last = chunk(json!({"id":"a","will_continue":false,"partial_args":[
        {"json_path":"$.files[0].path","string_value":"file.txt","will_continue":false},
        {"json_path":"$[\"quoted.key\"]","number_value":2}]}));
    for event in decoder
        .decode(&last)
        .unwrap()
        .into_iter()
        .chain(decoder.finish())
    {
        state.push(&event).unwrap();
    }
    let Content::ToolCall {
        id,
        name,
        arguments,
    } = &state.response.content[0].content
    else {
        panic!("tool call");
    };
    assert_eq!((id.as_str(), name.as_str()), ("a", "read"));
    assert_eq!(
        serde_json::from_str::<Value>(arguments).unwrap(),
        json!({"files":[{"path":"dir/file.txt"}],"enabled":false,"limit":0,"optional":null,"quoted.key":2})
    );
    let mut truncated = Decoder::new(Protocol::Gemini);
    truncated.decode(&first).unwrap();
    assert!(matches!(truncated.finish().as_slice(), [Event::Error(_)]));
}

fn transcode(from: Protocol, to: Protocol, s: &str, chunk: usize) -> String {
    let mut codec = Transcoder::new(from, to);
    let mut out = Vec::new();
    for bytes in s.as_bytes().chunks(chunk) {
        out.extend(codec.push(bytes).unwrap());
    }
    out.extend(codec.finish().unwrap());
    String::from_utf8(out).unwrap()
}

fn pair(from: Protocol, to: Protocol, name: &str) {
    let (source, error) = wire_stream(from);
    let success = transcode(from, to, source, 1);
    let failure = transcode(from, to, error, 3);
    assert_eq!(
        success,
        transcode(from, to, source, 4096),
        "TCP chunk boundaries must not change events"
    );
    assert!(success.contains("call_a") && success.contains("read"));
    assert!(success.contains("Inspect"));
    assert!(success.contains("cGl4ZWw="), "multimodal block missing");
    if from.family() != to.family() {
        for secret in [
            "anthropic-signature",
            "openai-ciphertext",
            "google-signature",
        ] {
            assert!(!success.contains(secret));
        }
    }
    assert!(failure.contains("fixture overloaded"));
    let actual = json!({"success":success,"error":failure});
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/translate/golden")
        .join(format!("{name}.json"));
    if std::env::var_os("UPDATE_TRANSLATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
    }
    let expected: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

macro_rules! golden {
    ($name:ident,$from:ident,$to:ident) => {
        #[test]
        fn $name() {
            pair(Protocol::$from, Protocol::$to, stringify!($name));
        }
    };
}
golden!(messages_to_responses, Messages, Responses);
golden!(messages_to_chat, Messages, Chat);
golden!(messages_to_gemini, Messages, Gemini);
golden!(responses_to_messages, Responses, Messages);
golden!(responses_to_chat, Responses, Chat);
golden!(responses_to_gemini, Responses, Gemini);
golden!(chat_to_messages, Chat, Messages);
golden!(chat_to_responses, Chat, Responses);
golden!(chat_to_gemini, Chat, Gemini);
golden!(gemini_to_messages, Gemini, Messages);
golden!(gemini_to_responses, Gemini, Responses);
golden!(gemini_to_chat, Gemini, Chat);

#[test]
fn sse_parser_handles_crlf_comments_and_multiline_json() {
    let mut s = Sse::default();
    let mut output = vec![];
    for b in b":ping\r\nevent: ignored\r\ndata: {\r\ndata: \"text\": \"hello\"}\r\n\r\n" {
        output.extend(s.push(&[*b]).unwrap());
    }
    assert_eq!(output, vec![b"{\n\"text\": \"hello\"}".to_vec()]);
}

#[test]
fn truncated_anthropic_and_responses_are_errors() {
    for protocol in [Protocol::Messages, Protocol::Responses] {
        let mut c = Transcoder::new(protocol, Protocol::Chat);
        c.push(b":ping\n\n").unwrap();
        assert!(c.finish().is_err());
    }
}

#[test]
fn cache_read_and_write_are_distinct_and_input_is_inclusive() {
    let a=translate::decode_response(Protocol::Messages,&json!({"id":"a","content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":4}})).unwrap();
    assert_eq!(
        a.usage,
        Usage {
            cache_write_1h: None,
            input: Some(60),
            output: Some(4),
            cache_read: Some(20),
            cache_write: Some(30),
            reasoning: None
        }
    );
    for p in [Protocol::Responses, Protocol::Chat] {
        let v = translate::encode_response(p, &a).unwrap();
        let decoded = translate::decode_response(p, &v).unwrap();
        assert_eq!(decoded.usage, a.usage);
    }
}

#[test]
fn gemini_cache_read_is_subset_and_thought_tokens_are_output() {
    let r=translate::decode_response(Protocol::Gemini,&json!({"candidates":[],"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":40,"candidatesTokenCount":5,"thoughtsTokenCount":7}})).unwrap();
    assert_eq!(
        r.usage,
        Usage {
            cache_write_1h: None,
            input: Some(100),
            output: Some(12),
            cache_read: Some(40),
            cache_write: None,
            reasoning: Some(7)
        }
    );
}

#[test]
fn gemini_snake_case_media_and_tool_signatures_roundtrip() {
    let value = json!({"contents":[{"role":"model","parts":[{"function_call":{"id":"a","name":"read","args":{}},"thought_signature":"native"}]},{"role":"user","parts":[{"inline_data":{"mime_type":"image/png","data":"eA=="}},{"file_data":{"mime_type":"application/pdf","file_uri":"gs://bucket/doc"}},{"function_response":{"id":"a","name":"read","response":{"output":"ok"}}}]}]});
    let r = translate::decode_request(Protocol::Gemini, &value).unwrap();
    assert_eq!(
        r,
        translate::decode_request(
            Protocol::Gemini,
            &translate::encode_request(Protocol::Gemini, &r).unwrap()
        )
        .unwrap()
    );
}

#[test]
fn ambiguous_gemini_tool_results_are_rejected() {
    let v = json!({"contents":[{"role":"model","parts":[{"functionCall":{"name":"read","args":{}}},{"functionCall":{"name":"read","args":{}}}]},{"role":"user","parts":[{"functionResponse":{"name":"read","response":{}}}]}]});
    assert!(translate::decode_request(Protocol::Gemini, &v).is_err());
}

#[test]
fn compaction_items_and_stored_state_are_not_faked() {
    for v in [
        json!({"input":[{"type":"compaction","encrypted_content":"opaque"}]}),
        json!({"input":[],"previous_response_id":"stored"}),
    ] {
        assert!(translate::decode_request(Protocol::Responses, &v).is_err());
    }
}

#[test]
fn responses_embedded_namespaces_and_text_tools_roundtrip() {
    let wire = json!({"model":"m","input":[
        {"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"local","tools":[
            {"type":"custom","name":"run","description":"Run text","format":{"type":"text"}},
            {"type":"function","name":"read","parameters":{"type":"object","properties":{}},"strict":false}
        ]}]},
        {"role":"user","content":"Read a file"},
        {"type":"custom_tool_call","id":"ctc_a","call_id":"call_a","namespace":"local","name":"run","input":"read marker.txt"},
        {"type":"custom_tool_call_output","call_id":"call_a","output":"contents"},
        {"type":"function_call","id":"fc_b","call_id":"call_b","namespace":"local","name":"read","arguments":"{}"},
        {"type":"function_call_output","call_id":"call_b","output":"more"}
    ]});
    let request = translate::decode_request(Protocol::Responses, &wire).unwrap();
    let encoded = translate::encode_request(Protocol::Responses, &request).unwrap();
    assert_eq!(encoded["input"][1]["type"], "custom_tool_call");
    assert_eq!(encoded["input"][1]["input"], "read marker.txt");
    assert_eq!(encoded["input"][2]["type"], "custom_tool_call_output");
    assert_eq!(
        translate::decode_request(Protocol::Responses, &encoded).unwrap(),
        request
    );
    let chat = translate::encode_request(Protocol::Chat, &request).unwrap();
    assert_eq!(
        chat["tools"][0]["function"]["parameters"]["properties"]["input"]["type"],
        "string"
    );
    assert_eq!(
        chat["messages"][1]["tool_calls"][0]["function"]["name"],
        chat["tools"][0]["function"]["name"]
    );
}

#[test]
fn text_tool_stream_restores_native_events_after_split_json_arguments() {
    let request = translate::decode_request(Protocol::Responses, &json!({"input":[],"tools":[{"type":"namespace","name":"local","tools":[{"type":"custom","name":"run","format":{"type":"text"}}]}]})).unwrap();
    let tool = &request.tools[0].name;
    let source = [
        json!({"id":"reply","model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":tool,"arguments":"{\"input\":\"read"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":" marker.txt\"}"}}]},"finish_reason":"tool_calls"}]}),
    ].into_iter().map(|v| format!("data: {v}\n\n")).collect::<String>() + "data: [DONE]\n\n";
    let mut converter =
        Transcoder::new(Protocol::Chat, Protocol::Responses).with_response_tools(&request.tools);
    let mut bytes = Vec::new();
    for chunk in source.as_bytes().chunks(7) {
        bytes.extend(converter.push(chunk).unwrap());
    }
    bytes.extend(converter.finish().unwrap());
    let mut parser = Sse::default();
    let events: Vec<Value> = parser
        .push(&bytes)
        .unwrap()
        .into_iter()
        .map(|b| serde_json::from_slice(&b).unwrap())
        .collect();
    let delta = events
        .iter()
        .find(|v| v["type"] == "response.custom_tool_call_input.delta")
        .unwrap();
    assert_eq!(delta["delta"], "read marker.txt");
    assert!(delta["item_id"].as_str().unwrap().starts_with("ctc_"));
    let item = &events.last().unwrap()["response"]["output"][0];
    assert_eq!(item["type"], "custom_tool_call");
    assert_eq!(item["namespace"], "local");
    assert_eq!(item["name"], "run");
    assert_eq!(item["input"], "read marker.txt");
    assert!(
        events
            .iter()
            .all(|v| v["type"] != "response.function_call_arguments.delta")
    );
}

#[test]
fn generated_responses_item_ids_use_native_type_prefixes() {
    let r =
        translate::decode_request(Protocol::Messages, &native_request(Protocol::Messages)).unwrap();
    let encoded = translate::encode_request(Protocol::Responses, &r).unwrap();
    let call = encoded["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "function_call")
        .unwrap();
    assert!(call["id"].as_str().unwrap().starts_with("fc_"));
    assert_eq!(call["call_id"], "call_a");
}
