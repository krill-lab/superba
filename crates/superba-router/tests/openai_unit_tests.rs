//! Units of the OpenAI ingress: the Chat Completions tap (any chunking, the
//! line before `stop`, usage chunks, tool calls, several choices, errors,
//! streams without `[DONE]`), body shaping, refused-field detection, request
//! facts per tool, configuration and Chat usage.

mod common;

use std::time::{Duration, Instant};

use bytes::Bytes;
use common::openai::{
    chat_accumulate, chat_stream, chat_tool_stream, chat_usage as usage, chunk, data,
};
use common::responses::line_of;
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::codex::STATS_LINE_MARKER;
use superba_core::{AgentKind, Multiplier, RequestHarness as H, RequestKind, StatsLineStyle};
use superba_router::openai::meta::{of_request, peek, peek_value};
use superba_router::openai::shape::{drop_private, rejected_field, shape_top};
use superba_router::openai::tap::{ChatSse, chat_usage};
use superba_router::openai::{OpenAiConfig, OpenAiEndpoint, OpenAiMember, OpenAiProtocol};
use superba_router::stats::tap::{LineSetup, Outcome, TurnSoFar};

fn setup() -> LineSetup {
    LineSetup {
        template: "⚡ {input} in / {output} out".into(),
        style: StatsLineStyle::Quote,
        turn: TurnSoFar::default(),
        price: None,
        ratio: Multiplier::ONE,
    }
}

/// Feed `stream` in pieces of `size` bytes (0 = one piece per event).
fn run(stream: &[String], size: usize, line: bool) -> (String, ChatSse) {
    let t0 = Instant::now();
    let mut tap = ChatSse::new(t0, line.then(setup));
    let all: String = stream.concat();
    let pieces: Vec<Bytes> = if size == 0 {
        stream.iter().map(|s| Bytes::from(s.clone())).collect()
    } else {
        all.as_bytes()
            .chunks(size)
            .map(Bytes::copy_from_slice)
            .collect()
    };
    let mut out = Vec::new();
    let mut tick = 0u64;
    let mut clock = || {
        tick += 1;
        t0 + Duration::from_millis(tick * 100)
    };
    for p in pieces {
        tap.push_at(p, &mut clock, &mut out);
    }
    tap.finish(&mut out);
    let text: String = out
        .iter()
        .map(|b| String::from_utf8(b.to_vec()).unwrap())
        .collect();
    (text, tap)
}

#[test]
fn the_line_goes_before_stop_with_the_usage_whatever_the_chunking() {
    let stream = chat_stream(&["Hel", "lo ", "wörld"], "stop", Some(usage(120, 100, 9)));
    for size in [0, 1, 3, 7, 64] {
        let (out, tap) = run(&stream, size, true);
        let (text, finish, u) = chat_accumulate(&out);
        assert_eq!(finish.as_deref(), Some("stop"), "size {size}");
        assert!(text.starts_with("Hello wörld\n\n> "), "size {size}: {text}");
        assert_eq!(
            line_of(&text).unwrap(),
            format!("> {STATS_LINE_MARKER} 120 in / 9 out")
        );
        assert_eq!(u.unwrap()["prompt_tokens"], 120);
        let s = tap.summary();
        assert!(s.injected);
        assert_eq!(s.outcome, Some(Outcome::Completed));
        assert_eq!(
            s.usage.map(|u| (u.input, u.cached, u.output)),
            Some((120, Some(100), 9))
        );
        assert_eq!(s.response_id.as_deref(), Some("chatcmpl-up-1"));
        assert_eq!(s.response_model.as_deref(), Some("deepseek-chat"));
        assert_eq!(s.deltas, 3);
    }
}

#[test]
fn without_the_line_the_stream_passes_byte_for_byte() {
    let stream = chat_stream(&["a", "b"], "stop", Some(usage(10, 0, 2)));
    for size in [0, 5] {
        let (out, tap) = run(&stream, size, false);
        assert_eq!(out, stream.concat());
        assert!(!tap.summary().injected);
    }
}

#[test]
fn text_in_the_finishing_chunk_comes_before_the_line() {
    let stream = vec![
        data(&chunk(
            json!({"role": "assistant", "content": "One "}),
            None,
        )),
        data(&chunk(json!({"content": "two."}), Some("stop"))),
        "data: [DONE]\n\n".to_owned(),
    ];
    let (out, tap) = run(&stream, 0, true);
    let (text, _, _) = chat_accumulate(&out);
    assert!(text.starts_with("One two.\n\n> "), "{text}");
    assert!(tap.summary().injected);
}

#[test]
fn usage_in_the_finishing_chunk_and_streams_without_done() {
    // DeepSeek-style: usage rides on the finishing chunk; no [DONE].
    let mut finish = chunk(json!({}), Some("stop"));
    finish["usage"] =
        json!({"prompt_tokens": 30, "completion_tokens": 3, "prompt_cache_hit_tokens": 20});
    let stream = vec![data(&chunk(json!({"content": "ok"}), None)), data(&finish)];
    let (out, tap) = run(&stream, 0, true);
    assert!(out.contains(STATS_LINE_MARKER));
    assert!(out.find(STATS_LINE_MARKER) < out.find("\"finish_reason\":\"stop\""));
    let s = tap.summary();
    assert_eq!(s.outcome, Some(Outcome::Completed));
    assert_eq!(s.usage.map(|u| (u.input, u.cached)), Some((30, Some(20))));
}

#[test]
fn no_line_on_tool_calls_several_choices_length_or_errors() {
    let (out, tap) = run(&chat_tool_stream(), 0, true);
    assert!(!out.contains(STATS_LINE_MARKER));
    assert!(!tap.summary().injected);
    let mut two = chat_stream(&["a"], "stop", None);
    let mut other = chunk(json!({"content": "b"}), Some("stop"));
    other["choices"][0]["index"] = json!(1);
    two.insert(1, data(&other));
    let (out, _) = run(&two, 0, true);
    assert!(!out.contains(STATS_LINE_MARKER));
    let (out, tap) = run(&chat_stream(&["a"], "length", None), 0, true);
    assert!(!out.contains(STATS_LINE_MARKER));
    assert_eq!(
        tap.summary().outcome,
        Some(Outcome::Incomplete {
            reason: Some("length".into())
        })
    );
    let err = vec![
        data(&chunk(json!({"content": "a"}), None)),
        data(
            &json!({"error": {"message": "overloaded", "type": "server_error", "code": "overloaded"}}),
        ),
    ];
    let (out, tap) = run(&err, 0, true);
    assert!(!out.contains(STATS_LINE_MARKER));
    assert_eq!(
        tap.summary().outcome,
        Some(Outcome::Error {
            status: None,
            code: Some("overloaded".into())
        })
    );
}

#[test]
fn a_cut_stream_has_no_outcome() {
    let stream = vec![data(&chunk(json!({"content": "a"}), None))];
    let (_, tap) = run(&stream, 0, true);
    assert_eq!(tap.summary().outcome, None);
}

#[test]
fn chat_usage_follows_the_routers_semantics() {
    let u = |v: Value| chat_usage(&v).map(|u| (u.input, u.cached, u.output, u.reasoning));
    assert_eq!(
        u(
            json!({"prompt_tokens": 100, "completion_tokens": 9, "prompt_tokens_details": {"cached_tokens": 80}, "completion_tokens_details": {"reasoning_tokens": 4}})
        ),
        Some((100, Some(80), 9, Some(4)))
    );
    // DeepSeek, Moonshot.
    assert_eq!(
        u(
            json!({"prompt_tokens": 100, "completion_tokens": 9, "prompt_cache_hit_tokens": 70, "prompt_cache_miss_tokens": 30})
        ),
        Some((100, Some(70), 9, None))
    );
    assert_eq!(
        u(json!({"prompt_tokens": 100, "completion_tokens": 9, "cached_tokens": 60})),
        Some((100, Some(60), 9, None))
    );
    // A provider that counts cache reads apart from prompt tokens.
    assert_eq!(
        u(
            json!({"prompt_tokens": 20, "completion_tokens": 1, "prompt_tokens_details": {"cached_tokens": 100}})
        ),
        Some((120, Some(100), 1, None))
    );
    assert_eq!(u(json!(null)), None);
    assert_eq!(u(json!({})), None);
}

#[test]
fn shaping_changes_only_the_top_level_members_it_names() {
    let body = br#"{"model":"m","input":[{"type":"message","content":"x  y"}],"dsh_session_log":{"a":[1,2]},"dsh_plugin_packages":[]}"#;
    let shaped = shape_top(body, &superba_router::openai::shape::is_private_field, true).unwrap();
    assert_eq!(
        String::from_utf8(shaped.body).unwrap(),
        r#"{"model":"m","input":[{"type":"message","content":"x  y"}],"store":false}"#
    );
    assert_eq!(shaped.removed, ["dsh_session_log", "dsh_plugin_packages"]);
    assert!(shaped.store_added);
    let with_store = br#"{"store":true,"model":"m"}"#;
    assert!(shape_top(with_store, &|_| false, true).is_none());
    assert!(shape_top(b"[1,2]", &|_| true, true).is_none());
    let mut v = json!({"model": "m", "dsh_session_log": 1, "keep": 2});
    assert_eq!(drop_private(&mut v), ["dsh_session_log"]);
    assert_eq!(v, json!({"model": "m", "keep": 2}));
}

#[test]
fn refusals_name_the_field_they_refuse() {
    let c = [
        "thinking",
        "enable_thinking",
        "reasoning",
        "reasoning_effort",
    ];
    let r = |v: Value| rejected_field(v.to_string().as_bytes(), &c);
    assert_eq!(
        r(
            json!({"error": {"message": "Unrecognized request argument supplied: thinking", "type": "invalid_request_error"}})
        ),
        Some("thinking".into())
    );
    assert_eq!(
        r(
            json!({"error": {"message": "Unknown parameter: 'enable_thinking'.", "param": "enable_thinking", "code": "unknown_parameter"}})
        ),
        Some("enable_thinking".into())
    );
    assert_eq!(
        r(
            json!({"detail": [{"loc": ["body", "reasoning"], "msg": "Extra inputs are not permitted", "type": "extra_forbidden"}]})
        ),
        Some("reasoning".into())
    );
    assert_eq!(
        r(
            json!({"error": {"message": "Unsupported parameter: 'reasoning_effort' is not supported with this model."}})
        ),
        Some("reasoning_effort".into()),
        "not `reasoning` inside `reasoning_effort`"
    );
    assert_eq!(
        r(json!({"error": {"message": "messages: must not be empty"}})),
        None
    );
    assert_eq!(
        r(json!({"error": {"message": "thinking is great"}})),
        None,
        "no refusal word"
    );
    assert_eq!(rejected_field(b"not json", &c), None);
}

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, v.parse().unwrap());
    }
    h
}

#[test]
fn request_facts_follow_each_tool() {
    let tools = br#"{"model":"m","tools":[{"type":"function"}],"prompt_cache_key":"pck"}"#;
    let bare = br#"{"model":"m","prompt_cache_key":"pck"}"#;
    let facts = |h: &HeaderMap, body: &[u8]| of_request(h, &peek(body), OpenAiProtocol::Chat);
    let dsh = facts(
        &headers(&[("x-superba-harness", "dsh"), ("session_id", "session-1")]),
        tools,
    );
    assert_eq!(
        (dsh.harness, dsh.session_id.as_deref()),
        (H::Dsh, Some("session-1"))
    );
    assert_eq!(dsh.agent_kind, Some(AgentKind::Main));
    let title = facts(&headers(&[("x-superba-harness", "dsh")]), bare);
    assert_eq!(title.agent_kind, Some(AgentKind::Background));
    assert_eq!(
        title.session_id.as_deref(),
        Some("pck"),
        "the prompt cache key stands in"
    );
    let zc = facts(
        &headers(&[
            ("x-superba-harness", "zcode"),
            ("x-session-id", "z1"),
            ("x-zcode-trace-id", "t1"),
            ("x-zcode-session-type", "side_chat"),
        ]),
        tools,
    );
    assert_eq!(
        (zc.session_id.as_deref(), zc.turn_id.as_deref()),
        (Some("z1"), Some("t1"))
    );
    assert_eq!(zc.agent_kind, Some(AgentKind::Background));
    let probe = facts(
        &headers(&[
            ("x-superba-harness", "zcode"),
            ("x-zcode-session-type", "main"),
            ("x-zcode-query-source", "provider_settings_connectivity"),
        ]),
        tools,
    );
    assert_eq!(
        (probe.agent_kind, probe.request_kind),
        (Some(AgentKind::Background), Some(RequestKind::Other))
    );
    let wb = facts(
        &headers(&[
            ("user-agent", "CLI/2.161.0 WorkBuddy/2.161.0"),
            ("x-conversation-id", "conv"),
            ("x-root-request-id", "root"),
            ("x-agent-type", "explorer"),
        ]),
        tools,
    );
    assert_eq!(
        (wb.harness, wb.harness_version.as_deref()),
        (H::WorkBuddy, Some("2.161.0"))
    );
    assert_eq!(
        (wb.session_id.as_deref(), wb.turn_id.as_deref()),
        (Some("conv"), Some("root"))
    );
    assert_eq!(wb.agent_kind, Some(AgentKind::Subagent));
    // Codex's session header name from pi: the token says pi.
    let pi = facts(
        &headers(&[("x-superba-harness", "pi"), ("session_id", "p")]),
        tools,
    );
    assert_eq!((pi.harness, pi.session_id.as_deref()), (H::Pi, Some("p")));
    let unknown = facts(&headers(&[("x-session-affinity", "aff")]), tools);
    assert_eq!(
        (unknown.harness, unknown.session_id.as_deref()),
        (H::Unknown, Some("aff"))
    );
    // Structured output, both protocols.
    let structured = of_request(
        &HeaderMap::new(),
        &peek_value(&json!({"model": "m", "text": {"format": {"type": "json_schema"}}})),
        OpenAiProtocol::Responses,
    );
    assert!(structured.structured_output);
    assert!(
        of_request(
            &HeaderMap::new(),
            &peek(br#"{"response_format":{"type":"json_object"}}"#),
            OpenAiProtocol::Chat
        )
        .structured_output
    );
}

#[test]
fn configuration_round_trips_and_is_checked() {
    let mut cfg = common::test_config(vec![common::member(
        "k",
        superba_router::MemberKind::Krill,
        false,
        vec![common::line(
            "main",
            url::Url::parse("https://a.example/backend-api/codex").unwrap(),
        )],
    )]);
    let mut o = OpenAiConfig::default();
    o.members.insert(
        "k".into(),
        OpenAiMember {
            responses: Some(OpenAiEndpoint::at("/codex/v1")),
            chat: Some(OpenAiEndpoint {
                path: None,
                lines: [(
                    "main".to_owned(),
                    url::Url::parse("https://b.example/v1").unwrap(),
                )]
                .into(),
            }),
            drop_fields: vec!["thinking".into()],
            models: Vec::new(),
        },
    );
    o.order = vec!["k".into()];
    cfg.openai = Some(o.clone());
    cfg.validate().unwrap();
    let back: superba_router::RouterConfig =
        serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
    assert_eq!(back.openai, Some(o.clone()));
    let bad = |f: &dyn Fn(&mut OpenAiConfig)| {
        let mut c = cfg.clone();
        f(c.openai.as_mut().unwrap());
        c.validate().is_err()
    };
    assert!(bad(&|o| {
        o.members.insert("nobody".into(), OpenAiMember::default());
    }));
    assert!(bad(
        &|o| o.members.get_mut("k").unwrap().responses = Some(OpenAiEndpoint::at("codex"))
    ));
    assert!(bad(&|o| {
        o.members.get_mut("k").unwrap().chat = Some(OpenAiEndpoint {
            path: None,
            lines: [(
                "other".to_owned(),
                url::Url::parse("https://b.example/v1").unwrap(),
            )]
            .into(),
        })
    }));
    assert!(bad(
        &|o| o.members.get_mut("k").unwrap().drop_fields = vec![" ".into()]
    ));
    assert!(bad(&|o| o.order = vec!["nobody".into()]));
    // Members not listed serve their defaults; an empty section is all defaults.
    let defaults: superba_router::RouterConfig =
        serde_json::from_value(json!({"format": 1, "listen_port": 0, "members": [
            {"id": "k", "name": "K", "kind": "krill", "lines": [{"id": "l", "name": "L", "base_url": "https://a.example/codex/v1"}]}
        ]}))
        .unwrap();
    assert_eq!(defaults.openai, None);
    let k = defaults.openai_member("k").unwrap();
    assert_eq!(k.responses, Some(OpenAiEndpoint::at("/codex/v1")));
    assert_eq!(k.chat, Some(OpenAiEndpoint::at("/codex/v1")));
}
