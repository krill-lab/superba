//! The Claude Code ingress's pure parts: configuration rules, model lists,
//! the stats line on Anthropic messages and streams, request facts, harness
//! tokens and the local token estimate.

mod common;

use std::time::{Duration, Instant};

use bytes::Bytes;
use common::anthropic::accumulate;
use common::claude::{ev, text_stream, tool_stream};
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::{AgentKind, ClaudeIneligible, RequestHarness, RequestKind, StatsLineStyle};
use superba_core::{Multiplier, codex::STATS_LINE_MARKER};
use superba_router::claude::catalog::parse_list;
use superba_router::claude::config::matches;
use superba_router::claude::tap::AnthropicSse;
use superba_router::claude::{
    BetaPolicy, ClaudeConfig, ClaudeMember, ClaudeRoute, ClaudeTarget, Endpoint, ModelAlias,
    estimate, line, meta,
};
use superba_router::stats::tap::{LineSetup, Outcome, TurnSoFar};
use superba_router::token::{harness_token, presenter_of, verify_source_token};
use superba_router::{MemberKind, StatsLineConfig};

#[test]
fn model_patterns_match_exact_ids_and_globs_ignoring_case() {
    assert!(matches("gpt-5.5", "gpt-5.5"));
    assert!(matches("GPT-5.5", "gpt-5.5"));
    assert!(!matches("gpt-5.5", "gpt-5.5-mini"));
    assert!(matches("claude-haiku-*", "claude-haiku-4-5-20251001"));
    assert!(matches("*haiku*", "claude-haiku-4-5"));
    assert!(matches("claude-*-4-5", "claude-sonnet-4-5"));
    assert!(!matches("claude-*-4-5", "claude-sonnet-4-6"));
    assert!(matches("*", "anything"));
    assert!(!matches("a*a", "a"));
}

#[test]
fn beta_policies_keep_what_the_provider_takes() {
    let h = "claude-code-20250219, interleaved-thinking-2025-05-14,context-1m-2025-08-07";
    assert_eq!(
        BetaPolicy::Pass.filter(h).as_deref(),
        Some("claude-code-20250219,interleaved-thinking-2025-05-14,context-1m-2025-08-07")
    );
    assert_eq!(BetaPolicy::Drop.filter(h), None);
    assert_eq!(
        BetaPolicy::Allow(vec!["interleaved-thinking-2025-05-14".into()])
            .filter(h)
            .as_deref(),
        Some("interleaved-thinking-2025-05-14")
    );
    assert_eq!(
        BetaPolicy::Deny(vec!["context-1m-2025-08-07".into()])
            .filter(h)
            .as_deref(),
        Some("claude-code-20250219,interleaved-thinking-2025-05-14")
    );
    assert_eq!(BetaPolicy::Allow(vec!["x".into()]).filter(h), None);
    // The wire form of the policy.
    assert_eq!(
        serde_json::to_value(BetaPolicy::Allow(vec!["a".into()])).unwrap(),
        json!({"mode": "allow", "values": ["a"]})
    );
    assert_eq!(
        serde_json::to_value(BetaPolicy::Drop).unwrap(),
        json!({"mode": "drop"})
    );
}

fn router_with(claude: ClaudeConfig) -> superba_router::RouterConfig {
    let up = url::Url::parse("https://api.example.com/v1").unwrap();
    let mut cfg = common::test_config(vec![
        common::member(
            "ds",
            MemberKind::Compatible,
            false,
            vec![common::line("a", up.clone())],
        ),
        common::member(
            "oa",
            MemberKind::Compatible,
            false,
            vec![common::line("a", up)],
        ),
    ]);
    cfg.claude = Some(claude);
    cfg
}

#[test]
fn only_members_with_an_anthropic_endpoint_are_eligible() {
    let mut c = ClaudeConfig::default();
    c.members.insert(
        "ds".into(),
        ClaudeMember {
            anthropic: Some(Endpoint {
                path: "/anthropic".into(),
                ..Endpoint::default()
            }),
            ..ClaudeMember::default()
        },
    );
    c.members.insert("oa".into(), ClaudeMember::default());
    assert_eq!(c.eligibility("ds"), Ok(()));
    assert_eq!(
        c.eligibility("oa"),
        Err(ClaudeIneligible::NoAnthropicEndpoint)
    );
    assert_eq!(
        c.eligibility("unknown"),
        Err(ClaudeIneligible::NoAnthropicEndpoint)
    );
    assert_eq!(
        ClaudeIneligible::NoAnthropicEndpoint.text().key,
        "claude_routing.no_anthropic_endpoint"
    );
    router_with(c.clone()).validate().unwrap();

    // A route to an ineligible member is refused.
    let mut bad = c.clone();
    bad.routes.push(ClaudeRoute {
        model: "gpt-*".into(),
        targets: vec![ClaudeTarget {
            member: "oa".into(),
            model: None,
        }],
    });
    let e = router_with(bad).validate().unwrap_err().to_string();
    assert!(e.contains("no Anthropic endpoint"), "{e}");

    // Members, paths, lines and aliases are checked.
    let mut bad = c.clone();
    bad.members.insert("nope".into(), ClaudeMember::default());
    assert!(router_with(bad).validate().is_err());
    let mut bad = c.clone();
    bad.members.get_mut("ds").unwrap().anthropic = Some(Endpoint {
        path: "anthropic".into(),
        ..Endpoint::default()
    });
    assert!(router_with(bad).validate().is_err());
    let mut bad = c.clone();
    bad.members
        .get_mut("ds")
        .unwrap()
        .anthropic
        .as_mut()
        .unwrap()
        .lines
        .insert("zz".into(), url::Url::parse("https://x.example").unwrap());
    assert!(router_with(bad).validate().is_err());
    let mut bad = c.clone();
    bad.aliases.push(ModelAlias {
        from: "claude-*".into(),
        to: " ".into(),
    });
    assert!(router_with(bad).validate().is_err());
    let mut bad = c;
    bad.routes.push(ClaudeRoute {
        model: "x".into(),
        targets: vec![],
    });
    assert!(router_with(bad).validate().is_err());
}

#[test]
fn the_claude_section_has_a_stable_wire_form() {
    let mut c = ClaudeConfig::default();
    c.members.insert(
        "ds".into(),
        ClaudeMember {
            anthropic: Some(Endpoint {
                path: "/anthropic".into(),
                ..Endpoint::default()
            }),
            betas: BetaPolicy::Drop,
            system_messages: false,
            claude_only: true,
            ..ClaudeMember::default()
        },
    );
    c.aliases.push(ModelAlias {
        from: "claude-haiku-*".into(),
        to: "deepseek-chat".into(),
    });
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(
        v,
        json!({
            "members": {"ds": {
                "anthropic": {"path": "/anthropic"},
                "auth": "x_api_key",
                "betas": {"mode": "drop"},
                "system_messages": false,
                "claude_only": true,
            }},
            "aliases": [{"from": "claude-haiku-*", "to": "deepseek-chat"}],
        })
    );
    let back: ClaudeConfig = serde_json::from_value(v).unwrap();
    assert_eq!(back, c);
    // Defaults: an empty member takes Anthropic system messages and passes betas.
    let m: ClaudeMember = serde_json::from_value(json!({})).unwrap();
    assert!(m.system_messages);
    assert_eq!(m.betas, BetaPolicy::Pass);
}

#[test]
fn model_lists_parse_in_every_shape() {
    let (m, more) = parse_list(
        br#"{"data":[{"type":"model","id":"claude-x","display_name":"Claude X","created_at":"2025-01-01T00:00:00Z"},{"type":"model","id":"deepseek-chat"}],"has_more":true,"first_id":"claude-x","last_id":"deepseek-chat"}"#,
    )
    .unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].display_name.as_deref(), Some("Claude X"));
    assert_eq!(more.as_deref(), Some("deepseek-chat"));
    let (m, more) =
        parse_list(br#"{"object":"list","data":[{"id":"gpt-5.5","object":"model"}]}"#).unwrap();
    assert_eq!(m[0].id, "gpt-5.5");
    assert_eq!(more, None);
    let (m, _) = parse_list(br#"{"models":[{"slug":"gpt-6","display_name":"GPT-6"}]}"#).unwrap();
    assert_eq!(
        (m[0].id.as_str(), m[0].display_name.as_deref()),
        ("gpt-6", Some("GPT-6"))
    );
    let (m, _) = parse_list(br#"["a", {"id": "b", "name": "b"}, 3]"#).unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[1].display_name, None);
    assert!(parse_list(b"<html>").is_none());
    assert!(parse_list(b"{}").is_none());
}

fn stats_line(harnesses: Vec<RequestHarness>) -> StatsLineConfig {
    StatsLineConfig {
        template: "⚡ {input} in / {output} out · {requests} req".into(),
        style: StatsLineStyle::Quote,
        originators: vec![],
        harnesses,
    }
}

#[test]
fn earlier_lines_are_stripped_from_assistant_text_only() {
    let line = format!("\n\n> {STATS_LINE_MARKER} 1,234 in / 56 out");
    let mut req = json!({
        "messages": [
            {"role": "user", "content": format!("user text{line}")},
            {"role": "assistant", "content": format!("answer{line}")},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": format!("t{line}"), "signature": "s"},
                {"type": "text", "text": format!("first{line}")},
                {"type": "text", "text": line.clone()},
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {"x": line.clone()}},
            ]},
            {"role": "assistant", "content": [{"type": "text", "text": "plain"}]},
        ]
    });
    assert_eq!(line::strip_request(&mut req), 3);
    let m = &req["messages"];
    assert_eq!(
        m[0]["content"],
        format!("user text{line}"),
        "user text kept"
    );
    assert_eq!(m[1]["content"], "answer");
    let blocks = m[2]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 3, "the block holding only the line is gone");
    assert_eq!(
        blocks[0]["thinking"],
        format!("t{line}"),
        "thinking untouched"
    );
    assert_eq!(blocks[1]["text"], "first");
    assert_eq!(blocks[2]["input"]["x"], line, "tool input untouched");
    assert_eq!(m[3]["content"][0]["text"], "plain");
    assert_eq!(line::strip_request(&mut req), 0);
}

#[test]
fn whole_answers_get_the_line_only_on_end_turn() {
    let mut a = json!({"stop_reason": "end_turn", "content": [
        {"type": "thinking", "thinking": "x", "signature": "s"},
        {"type": "text", "text": "Hello"},
    ]});
    assert!(line::inject_json(&mut a, "\n\n> ⚡ x"));
    assert_eq!(a["content"][1]["text"], "Hello\n\n> ⚡ x");
    let mut tool = json!({"stop_reason": "tool_use", "content": [{"type": "text", "text": "a"}, {"type": "tool_use"}]});
    assert!(!line::inject_json(&mut tool, "\n\n> ⚡ x"));
    let mut no_text = json!({"stop_reason": "end_turn", "content": [{"type": "thinking", "thinking": "", "signature": "s"}]});
    assert!(line::inject_json(&mut no_text, "\n\n> ⚡ x"));
    assert_eq!(
        no_text["content"][1],
        json!({"type": "text", "text": "\n\n> ⚡ x"})
    );
}

fn setup() -> LineSetup {
    LineSetup {
        template: "⚡ {input} in / {output} out · {cached} cached · {requests} req".into(),
        style: StatsLineStyle::Quote,
        turn: TurnSoFar {
            input: 1000,
            output: 10,
            cached: 0,
            requests: 1,
            cost: None,
        },
        price: None,
        ratio: Multiplier::ONE,
    }
}

/// Run `chunks` through a tap, one byte at a time when `bytewise`.
fn tap_through(
    chunks: &[String],
    line: Option<LineSetup>,
    bytewise: bool,
) -> (String, AnthropicSse) {
    let t0 = Instant::now();
    let mut tap = AnthropicSse::new(t0, line);
    let mut out = Vec::new();
    let mut tick = 0u64;
    let mut clock = || {
        tick += 1;
        t0 + Duration::from_millis(tick * 10)
    };
    let all: String = chunks.concat();
    if bytewise {
        for b in all.as_bytes() {
            tap.push_at(Bytes::copy_from_slice(&[*b]), &mut clock, &mut out);
        }
    } else {
        for c in chunks {
            tap.push_at(Bytes::from(c.clone()), &mut clock, &mut out);
        }
    }
    tap.finish(&mut out);
    let text = String::from_utf8(out.concat()).unwrap();
    (text, tap)
}

#[test]
fn the_tap_passes_streams_unchanged_and_measures_them() {
    let usage = json!({"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 20});
    let chunks = text_stream("m", &["Hel", "lo"], "end_turn", usage);
    for bytewise in [false, true] {
        let (text, tap) = tap_through(&chunks, None, bytewise);
        assert_eq!(text, chunks.concat(), "byte for byte");
        let s = tap.summary();
        let u = s.usage.unwrap();
        // Anthropic usage in the router's semantics: input includes the cache.
        assert_eq!(u.input, 115);
        assert_eq!(u.cached, Some(100));
        assert_eq!(u.cache_write, Some(5));
        assert_eq!(u.output, 20);
        assert_eq!(s.outcome, Some(Outcome::Completed));
        assert_eq!(s.deltas, 2);
        assert!(s.first_token.is_some() && s.end.is_some());
        assert!(!s.injected);
    }
    let (_, tap) = tap_through(
        &text_stream(
            "m",
            &["x"],
            "max_tokens",
            json!({"input_tokens": 1, "output_tokens": 1}),
        ),
        None,
        false,
    );
    assert!(matches!(
        tap.summary().outcome,
        Some(Outcome::Incomplete { .. })
    ));
    let (_, tap) = tap_through(
        &[ev(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}}),
        )],
        None,
        false,
    );
    assert_eq!(
        tap.summary().outcome,
        Some(Outcome::Error {
            status: None,
            code: Some("overloaded_error".into())
        })
    );
}

#[test]
fn the_line_is_appended_to_the_last_text_block_of_a_final_answer() {
    let usage = json!({"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 20});
    let chunks = text_stream("m", &["Hel", "lo"], "end_turn", usage);
    for bytewise in [false, true] {
        let (text, tap) = tap_through(&chunks, Some(setup()), bytewise);
        let msg = accumulate(&text);
        let t = msg["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            t,
            format!("Hello\n\n> {STATS_LINE_MARKER} 1,115 in / 30 out · 100 cached · 2 req")
        );
        assert!(tap.summary().injected);
        assert_eq!(msg["stop_reason"], "end_turn");
    }
    // A tool call is not the end of the turn.
    let (text, tap) = tap_through(&tool_stream("m"), Some(setup()), false);
    assert_eq!(text, tool_stream("m").concat());
    assert!(!tap.summary().injected);
    // An answer that ends with a thinking block gets a text block of its own.
    let chunks = vec![
        ev(
            "message_start",
            json!({"type": "message_start", "message": {"id": "m1", "type": "message", "role": "assistant", "model": "m", "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 3, "output_tokens": 1}}}),
        ),
        ev(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        ),
        ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "s"}}),
        ),
        ev(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        ev(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 4}}),
        ),
        ev("message_stop", json!({"type": "message_stop"})),
    ];
    let (text, tap) = tap_through(&chunks, Some(setup()), false);
    let msg = accumulate(&text);
    assert_eq!(msg["content"].as_array().unwrap().len(), 2);
    assert_eq!(msg["content"][0]["signature"], "s");
    let t = msg["content"][1]["text"].as_str().unwrap();
    assert!(
        t.starts_with("\n\n> ") && t.contains(STATS_LINE_MARKER),
        "{t}"
    );
    assert!(tap.summary().injected);
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    h
}

#[test]
fn request_facts_come_from_claude_codes_headers() {
    let with_tools = json!({"tools": [{"name": "Read"}], "metadata": {"user_id": "{\"session_id\":\"s-body\"}"}});
    let ua = ("user-agent", "claude-cli/2.1.302 (external, cli)");
    let m = meta::claude_meta(
        &headers(&[
            ua,
            ("x-claude-code-session-id", "s-head"),
            ("x-claude-code-prompt-id", "p-1"),
            ("x-claude-code-request-class", "main"),
        ]),
        &with_tools,
        "claude-haiku-4-5",
        "deepseek-chat",
    );
    assert_eq!(m.harness, RequestHarness::ClaudeCode);
    assert_eq!(m.harness_version.as_deref(), Some("2.1.302"));
    assert_eq!(m.protocol, superba_core::RequestProtocol::Messages);
    assert_eq!(m.session_id.as_deref(), Some("s-head"));
    assert_eq!(m.thread_id.as_deref(), Some("s-head"));
    assert_eq!(m.turn_id.as_deref(), Some("p-1"));
    assert_eq!(m.agent_kind, Some(AgentKind::Main));
    assert_eq!(m.request_kind, Some(RequestKind::Turn));
    assert_eq!(m.requested_model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(m.model.as_deref(), Some("deepseek-chat"));

    let kind = |pairs: &[(&str, &str)], body: &Value| {
        let m = meta::claude_meta(&headers(pairs), body, "m", "m");
        (m.agent_kind, m.request_kind)
    };
    let none = json!({});
    assert_eq!(
        kind(&[("x-claude-code-request-class", "subagent")], &none),
        (Some(AgentKind::Subagent), Some(RequestKind::Turn))
    );
    assert_eq!(
        kind(
            &[("x-claude-code-request-class", "compaction")],
            &with_tools
        ),
        (Some(AgentKind::Background), Some(RequestKind::Compaction))
    );
    assert_eq!(
        kind(&[("x-claude-code-request-class", "auxiliary")], &with_tools),
        (Some(AgentKind::Background), Some(RequestKind::Other))
    );
    assert_eq!(
        kind(&[("x-claude-code-agent-id", "a1")], &with_tools).0,
        Some(AgentKind::Subagent)
    );
    assert_eq!(
        kind(&[("x-app", "cli-bg")], &with_tools).0,
        Some(AgentKind::Background)
    );
    assert_eq!(kind(&[], &with_tools).0, Some(AgentKind::Main));
    assert_eq!(kind(&[], &none).0, Some(AgentKind::Background));

    // The session from the body: JSON user id, or the older `…_session_<id>`.
    let m = meta::claude_meta(&headers(&[ua]), &with_tools, "m", "m");
    assert_eq!(m.session_id.as_deref(), Some("s-body"));
    let legacy = json!({"metadata": {"user_id": "user_abc_account__session_1234-5678"}});
    let m = meta::claude_meta(&headers(&[]), &legacy, "m", "m");
    assert_eq!(m.session_id.as_deref(), Some("1234-5678"));
    // Neither token nor User-Agent: an unknown tool.
    assert_eq!(m.harness, RequestHarness::Unknown);
    // The token's harness wins over the User-Agent.
    let m = meta::claude_meta(
        &headers(&[
            ("user-agent", "curl/8.5.0"),
            ("x-superba-harness", "claude-code"),
        ]),
        &none,
        "m",
        "m",
    );
    assert_eq!(
        (m.harness, m.harness_version),
        (RequestHarness::ClaudeCode, None)
    );
    // Structured output never gets the line.
    let m = meta::claude_meta(
        &headers(&[ua]),
        &json!({"output_config": {"format": {"type": "json_schema", "schema": {}}}}),
        "m",
        "m",
    );
    assert!(m.structured_output);
}

#[test]
fn the_line_is_for_claude_codes_main_conversation_with_its_switch_on() {
    let both = stats_line(vec![RequestHarness::Codex, RequestHarness::ClaudeCode]);
    let codex_only = stats_line(vec![RequestHarness::Codex]);
    let ua = ("user-agent", "claude-cli/2.1.302 (external, cli)");
    let main = meta::claude_meta(
        &headers(&[ua, ("x-claude-code-request-class", "main")]),
        &json!({}),
        "m",
        "m",
    );
    assert!(line::allowed(&both, &main));
    assert!(!line::allowed(&codex_only, &main));
    let sub = meta::claude_meta(
        &headers(&[ua, ("x-claude-code-request-class", "subagent")]),
        &json!({}),
        "m",
        "m",
    );
    assert!(!line::allowed(&both, &sub));
    let other_tool = meta::claude_meta(
        &headers(&[
            ("user-agent", "opencode/1.0"),
            ("x-claude-code-request-class", "main"),
        ]),
        &json!({}),
        "m",
        "m",
    );
    assert!(!line::allowed(&both, &other_tool));
}

#[test]
fn harness_tokens_name_their_harness_and_host() {
    let router = b"sbr_router_token_for_tests";
    let local = harness_token(router, RequestHarness::ClaudeCode, None).unwrap();
    let source = verify_source_token(router, local.as_bytes()).unwrap();
    assert_eq!(source, "h:claude-code");
    let p = presenter_of(&source);
    assert_eq!(p.harness, Some(RequestHarness::ClaudeCode));
    assert_eq!(p.host, None);
    let wsl = harness_token(router, RequestHarness::ClaudeCode, Some("wsl:Ubuntu-24.04")).unwrap();
    let p = presenter_of(&verify_source_token(router, wsl.as_bytes()).unwrap());
    assert_eq!(p.harness, Some(RequestHarness::ClaudeCode));
    assert_eq!(p.host.as_deref(), Some("wsl:Ubuntu-24.04"));
    // A plain WSL source token names its host only.
    let p = presenter_of("wsl:Debian");
    assert_eq!((p.host.as_deref(), p.harness), (Some("wsl:Debian"), None));
    // Another router token does not verify it; an unknown harness names none.
    assert!(verify_source_token(b"other", local.as_bytes()).is_none());
    assert_eq!(presenter_of("h:nope").harness, None);
    assert!(harness_token(router, RequestHarness::Codex, Some("bad host!")).is_none());
}

#[test]
fn the_local_estimate_counts_text_images_and_tools() {
    let ascii = json!({"messages": [{"role": "user", "content": "a".repeat(400)}]});
    assert_eq!(estimate::input_tokens(&ascii), 100 + 4);
    let cjk = json!({"messages": [{"role": "user", "content": "你好世界"}]});
    assert_eq!(estimate::input_tokens(&cjk), 4 + 4);
    let rich = json!({
        "system": [{"type": "text", "text": "abcd"}],
        "messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "x"}},
            {"type": "tool_result", "tool_use_id": "t", "content": "abcdefgh"},
        ]}],
        "tools": [{"name": "Read", "input_schema": {}}],
    });
    assert!(estimate::input_tokens(&rich) > 1600 + 1 + 2);
    assert_eq!(estimate::input_tokens(&json!({})), 1);
}

#[test]
fn the_responses_tap_keeps_the_upstream_response_id() {
    use superba_router::stats::sse::EventKind;
    use superba_router::stats::tap::ResponseTap;
    let now = Instant::now();
    let mut tap = ResponseTap::new(now, None);
    tap.on_event(
        EventKind::Created,
        br#"{"type":"response.created","response":{"id":"resp_0123","status":"in_progress"}}"#,
        now,
    );
    assert_eq!(tap.summary().response_id.as_deref(), Some("resp_0123"));
    tap.on_event(
        EventKind::Completed,
        br#"{"type":"response.completed","response":{"id":"resp_0123","status":"completed","usage":{"input_tokens":3,"output_tokens":1}}}"#,
        now,
    );
    assert_eq!(tap.summary().response_id.as_deref(), Some("resp_0123"));
}
