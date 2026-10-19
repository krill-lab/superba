//! The OpenAI ingress (`/openai/v1`) with fake upstreams: Responses and Chat
//! Completions passed through to the members that serve them (Krill at
//! `/codex/v1`, compatible sites at their base), failover and stickiness,
//! eligibility and its reasons, model lists, the tools' request shaping
//! (dsh's private fields, `store: false`, refused and dropped fields),
//! tokens, `/models`, WebSocket refusal and the harness routes.

mod common;

use std::io::Write;
use std::sync::Arc;

use common::claude::{Anthropic, Fake, anthropic};
use common::openai::*;
use common::responses::{request_body, sse, text_answer};
use common::*;
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::{OpenAiIneligible, RequestHarness as H, RequestProtocol};
use superba_router::openai::{OpenAiMember, OpenAiModel, OpenAiProtocol};
use superba_router::translate::{ir::Protocol, routing::Conversion};

async fn post(
    r: &Router,
    path: &str,
    token: &str,
    headers: HeaderMap,
    body: &Value,
) -> reqwest::Response {
    r.http
        .post(r.url(path))
        .headers(headers)
        .bearer_auth(token)
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .unwrap()
}

fn responses_ok() -> Fake {
    Fake::sse(vec![sse(&text_answer(), false)])
}

fn chat_ok() -> Fake {
    Fake::sse(chat_stream(
        &["Hel", "lo"],
        "stop",
        Some(chat_usage(100, 80, 5)),
    ))
}

/// Requests a fake received.
trait Calls {
    fn hits(&self) -> usize;
    fn hits_of(&self, path: &str) -> usize;
}

impl Calls for Anthropic {
    fn hits(&self) -> usize {
        self.seen().len()
    }
    fn hits_of(&self, path: &str) -> usize {
        self.seen().iter().filter(|s| s.path.contains(path)).count()
    }
}

fn sent(up: &Anthropic, i: usize) -> Value {
    serde_json::from_slice(&up.seen()[i].body).unwrap()
}

fn encoded(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding {
        "gzip" => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(body).unwrap();
            encoder.finish().unwrap()
        }
        "deflate" => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(body).unwrap();
            encoder.finish().unwrap()
        }
        "br" => {
            let mut out = Vec::new();
            {
                let mut encoder = brotli::CompressorWriter::new(&mut out, 4096, 3, 22);
                encoder.write_all(body).unwrap();
            }
            out
        }
        "zstd" => zstd::stream::encode_all(body, 3).unwrap(),
        "identity" => body.to_vec(),
        _ => panic!("unknown test encoding"),
    }
}

async fn encoded_private_fields_are_removed(path: &str, encoding: &str) {
    let chat = path.ends_with("/chat/completions");
    let up = anthropic(Arc::new(
        move |_| if chat { chat_ok() } else { responses_ok() },
    ))
    .await;
    let mut member = krill("k", &up);
    member
        .model_map
        .insert("public-model".into(), "upstream-model".into());
    let r = start(config(vec![member], vec![])).await;
    let mut body = if chat {
        chat_body("public-model", "keep this history", true)
    } else {
        request_body("public-model", "keep this history", json!({"store": false}))
    };
    body["dsh_session_log"] = json!({"events": ["private conversation"]});
    body["dsh_plugin_packages"] = json!(["private package"]);
    body["dsh_future_field"] = json!("also private");
    let resp = r
        .http
        .post(r.url(path))
        .bearer_auth(token_of(H::Dsh))
        .header("content-encoding", encoding)
        .body(encoded(&serde_json::to_vec(&body).unwrap(), encoding))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{path}, {encoding}");
    let _ = resp.text().await.unwrap();
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].headers.contains_key("content-encoding"));
    assert_eq!(
        seen[0].headers["content-length"],
        seen[0].body.len().to_string()
    );
    let actual = sent(&up, 0);
    assert!(
        actual
            .as_object()
            .unwrap()
            .keys()
            .all(|k| !k.starts_with("dsh_"))
    );
    body.as_object_mut()
        .unwrap()
        .retain(|k, _| !k.starts_with("dsh_"));
    body["model"] = json!("upstream-model");
    assert_eq!(
        actual, body,
        "all public fields survive; model mapping sees decoded JSON"
    );
}

#[tokio::test]
async fn gzip_responses_strip_private_fields_before_forwarding() {
    encoded_private_fields_are_removed("/openai/v1/responses", "gzip").await;
}

#[tokio::test]
async fn deflate_responses_strip_private_fields_before_forwarding() {
    encoded_private_fields_are_removed("/openai/v1/responses", "deflate").await;
}

#[tokio::test]
async fn brotli_responses_strip_private_fields_before_forwarding() {
    encoded_private_fields_are_removed("/openai/v1/responses", "br").await;
}

#[tokio::test]
async fn zstd_responses_strip_private_fields_before_forwarding() {
    encoded_private_fields_are_removed("/openai/v1/responses", "zstd").await;
}

#[tokio::test]
async fn chat_strips_private_fields_for_identity_and_every_supported_encoding() {
    for encoding in ["identity", "gzip", "deflate", "br", "zstd"] {
        encoded_private_fields_are_removed("/openai/v1/chat/completions", encoding).await;
    }
}

#[tokio::test]
async fn unsupported_encodings_return_415_on_both_protocols_without_contacting_upstream() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    for path in ["/openai/v1/responses", "/openai/v1/chat/completions"] {
        for values in [
            vec![http::HeaderValue::from_static("unknown")],
            vec![http::HeaderValue::from_static("gzip, br")],
            vec![
                http::HeaderValue::from_static("identity"),
                http::HeaderValue::from_static("gzip"),
            ],
            vec![http::HeaderValue::from_bytes(b"\xff").unwrap()],
        ] {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append("content-encoding", value);
            }
            let resp = post(
                &r,
                path,
                &token_of(H::Dsh),
                headers,
                &json!({"model": "m", "dsh_session_log": "private"}),
            )
            .await;
            assert_eq!(resp.status(), 415, "{path}");
            let error: Value = resp.json().await.unwrap();
            assert_eq!(error["error"]["code"], "bad_request");
        }
    }
    assert_eq!(up.hits(), 0);
}

#[tokio::test]
async fn malformed_encoded_or_json_bodies_never_reach_upstream() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    for path in ["/openai/v1/responses", "/openai/v1/chat/completions"] {
        for encoding in ["gzip", "deflate", "br", "zstd"] {
            let resp = r
                .http
                .post(r.url(path))
                .bearer_auth(token_of(H::Dsh))
                .header("content-encoding", encoding)
                .body(vec![0xff; 8])
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 400, "{path}, {encoding}");
        }
        for body in [br#"{"dsh_private": "x""#.as_slice(), b"{} trailing", b"[]"] {
            let resp = r
                .http
                .post(r.url(path))
                .bearer_auth(token_of(H::Dsh))
                .header("content-encoding", "gzip")
                .body(encoded(body, "gzip"))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 400, "{path}");
        }
    }
    assert_eq!(up.hits(), 0);
}

#[tokio::test]
async fn decoded_body_limit_applies_to_both_protocols_before_forwarding() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let mut cfg = config(vec![krill("k", &up)], vec![]);
    cfg.tuning.max_body_bytes = 1024;
    let r = start(cfg).await;
    let body = serde_json::to_vec(&json!({"model": "m", "dsh_private": "x".repeat(8192)})).unwrap();
    for path in ["/openai/v1/responses", "/openai/v1/chat/completions"] {
        for encoding in ["gzip", "deflate", "br", "zstd"] {
            let compressed = encoded(&body, encoding);
            assert!(compressed.len() < 1024);
            let resp = r
                .http
                .post(r.url(path))
                .bearer_auth(token_of(H::Dsh))
                .header("content-encoding", encoding)
                .body(compressed)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 413, "{path}, {encoding}");
        }
    }
    assert_eq!(up.hits(), 0);
}

#[tokio::test]
async fn responses_reach_krill_at_codex_v1_with_the_members_key_and_without_private_fields() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("krill", &up)], vec![])).await;
    let body = request_body(
        "gpt-5.6-luna",
        "earlier answer",
        json!({"store": false, "dsh_session_log": {"events": ["secret conversation"]}, "dsh_plugin_packages": ["x"]}),
    );
    let mut headers = tool_headers(H::Dsh, "session-abc");
    headers.insert("x-api-key", token_of(H::Dsh).parse().unwrap());
    let resp = post(
        &r,
        "/openai/v1/responses",
        &token_of(H::Dsh),
        headers,
        &body,
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/main");
    assert!(resp.text().await.unwrap().contains("Hello"));
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/codex/v1/responses");
    assert_eq!(
        seen[0].headers["authorization"],
        format!("Bearer {}", key_of("krill"))
    );
    assert!(
        !seen[0].headers.contains_key("x-api-key"),
        "the token never leaves"
    );
    assert!(!seen[0].headers.contains_key("x-superba-harness"));
    assert!(!seen[0].headers.contains_key("accept-encoding"));
    assert_eq!(seen[0].headers["session_id"], "session-abc");
    let up_body = sent(&up, 0);
    assert!(up_body.get("dsh_session_log").is_none());
    assert!(up_body.get("dsh_plugin_packages").is_none());
    assert_eq!(up_body["model"], "gpt-5.6-luna");
    assert_eq!(up_body["input"], body["input"]);
    assert_eq!(up_body["store"], false);
}

#[tokio::test]
async fn compatible_members_serve_responses_at_their_line_base() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![compatible("site", &up)], vec![])).await;
    let body = request_body("gpt-x", "a", json!({"store": false}));
    let resp = post(
        &r,
        "/openai/v1/responses",
        &token_of(H::Pi),
        tool_headers(H::Pi, "s"),
        &body,
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(up.seen()[0].path, "/v1/responses");
    // Unchanged bodies keep their bytes.
    assert_eq!(up.seen()[0].body, serde_json::to_vec(&body).unwrap());
}

#[tokio::test]
async fn store_false_is_added_only_when_the_tool_does_not_say() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("krill", &up)], vec![])).await;
    let token = token_of(H::ZCode);
    let mut zcode = request_body("gpt-5.6-luna", "a", json!({}));
    zcode.as_object_mut().unwrap().remove("store");
    for (extra, _) in [
        (json!({}), Some(json!(false))),
        (json!({"store": true}), Some(json!(true))),
        (json!({"previous_response_id": "resp_prev"}), None),
    ] {
        let mut b = zcode.clone();
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        let resp = post(
            &r,
            "/openai/v1/responses",
            &token,
            tool_headers(H::ZCode, "s"),
            &b,
        )
        .await;
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await;
    }
    let stores: Vec<Option<Value>> = (0..3).map(|i| sent(&up, i).get("store").cloned()).collect();
    assert_eq!(stores, vec![Some(json!(false)), Some(json!(true)), None]);
}

#[tokio::test]
async fn disabled_translation_limits_chat_to_native_members() {
    let a = anthropic(Arc::new(|_| responses_ok())).await;
    let b = anthropic(Arc::new(|_| chat_ok())).await;
    let mut cfg = config(
        vec![compatible("a", &a), compatible("b", &b)],
        vec![("b", chat_entry(false))],
    );
    cfg.translation.disabled.insert(Conversion {
        from: Protocol::Chat,
        to: Protocol::Responses,
    });
    let r = start(cfg).await;
    let token = token_of(H::WorkBuddy);
    let resp = post(
        &r,
        "/openai/v1/chat/completions",
        &token,
        tool_headers(H::WorkBuddy, "conv-1"),
        &chat_body("deepseek-chat", "earlier", true),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "b/main");
    let (text, finish, usage) = chat_accumulate(&resp.text().await.unwrap());
    assert_eq!((text.as_str(), finish.as_deref()), ("Hello", Some("stop")));
    assert_eq!(usage.unwrap()["prompt_tokens"], 100);
    assert_eq!(a.hits_of("/chat/completions"), 0);
    assert_eq!(b.seen()[0].path, "/v1/chat/completions");
    // Responses: b's entry serves no Responses, so a.
    let resp = post(
        &r,
        "/openai/v1/responses",
        &token_of(H::Dsh),
        tool_headers(H::Dsh, "s"),
        &request_body("gpt-x", "a", json!({})),
    )
    .await;
    assert_eq!(route(&resp), "a/main");
}

#[tokio::test]
async fn without_a_chat_member_the_router_names_each_members_reason() {
    let a = anthropic(Arc::new(|_| chat_ok())).await;
    let mut cfg = config(vec![compatible("site", &a)], vec![]);
    cfg.translation.disabled.insert(Conversion {
        from: Protocol::Chat,
        to: Protocol::Responses,
    });
    let r = start(cfg).await;
    let resp = post(
        &r,
        "/openai/v1/chat/completions",
        &token_of(H::WorkBuddy),
        tool_headers(H::WorkBuddy, "c"),
        &chat_body("m", "x", true),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("no provider of the local router serves Chat Completions"),
        "{msg}"
    );
    assert!(
        msg.contains("SITE: This provider has no Chat Completions endpoint"),
        "{msg}"
    );
    assert_eq!(v["error"]["code"], "no_provider");
    assert_eq!(a.hits(), 0);
}

#[test]
fn eligibility_follows_the_members_kind_and_entries() {
    let up_base = url::Url::parse("http://127.0.0.1:9/v1").unwrap();
    let k = member(
        "k",
        superba_router::MemberKind::Krill,
        false,
        vec![line("l", up_base.clone())],
    );
    let c = member(
        "c",
        superba_router::MemberKind::Compatible,
        false,
        vec![line("l", up_base.clone())],
    );
    let d = member(
        "d",
        superba_router::MemberKind::Compatible,
        false,
        vec![line("l", up_base)],
    );
    let mut cfg = test_config(vec![k, c, d]);
    let mut claude = superba_router::ClaudeConfig::default();
    claude.members.insert(
        "d".into(),
        superba_router::claude::ClaudeMember {
            anthropic: Some(Default::default()),
            claude_only: true,
            ..Default::default()
        },
    );
    cfg.claude = Some(claude);
    let e = |id: &str, p| cfg.openai_eligibility(id, p);
    assert_eq!(e("k", OpenAiProtocol::Responses), Ok(()));
    assert_eq!(e("k", OpenAiProtocol::Chat), Ok(()));
    assert_eq!(e("c", OpenAiProtocol::Responses), Ok(()));
    assert_eq!(e("c", OpenAiProtocol::Chat), Ok(()));
    assert_eq!(
        e("d", OpenAiProtocol::Responses),
        Ok(()),
        "Anthropic is available through conversion"
    );
    assert_eq!(
        e("nobody", OpenAiProtocol::Chat),
        Err(OpenAiIneligible::NoChatEndpoint)
    );
    cfg.validate().unwrap();
}

#[tokio::test]
async fn chat_fails_over_before_the_first_byte_and_the_session_stays_on_the_new_member() {
    let a = anthropic(Arc::new(|_| {
        Fake::json(502, json!({"error": {"message": "bad gateway"}}))
    }))
    .await;
    let b = anthropic(Arc::new(|_| chat_ok())).await;
    let r = start(config(vec![krill("a", &a), krill("b", &b)], vec![])).await;
    let token = token_of(H::Pi);
    for _ in 0..2 {
        let resp = post(
            &r,
            "/openai/v1/chat/completions",
            &token,
            tool_headers(H::Pi, "pi-session"),
            &chat_body("gpt-5.6-luna", "x", true),
        )
        .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(route(&resp), "b/main");
        let _ = resp.text().await;
    }
    assert_eq!(
        a.hits_of("/chat/completions"),
        1,
        "the second request went straight to the pinned member"
    );
    assert!(
        b.seen()
            .iter()
            .any(|s| s.path == "/codex/v1/chat/completions")
    );
}

#[tokio::test]
async fn responses_fail_over_on_connect_errors_and_503() {
    let b = anthropic(Arc::new(|_| responses_ok())).await;
    let c = anthropic(Arc::new(|_| {
        Fake::json(503, json!({"error": {"message": "busy"}}))
    }))
    .await;
    let dead = member(
        "dead",
        superba_router::MemberKind::Compatible,
        false,
        vec![line("main", dead_base())],
    );
    let r = start(config(
        vec![dead, compatible("busy", &c), compatible("b", &b)],
        vec![],
    ))
    .await;
    let resp = post(
        &r,
        "/openai/v1/responses",
        &token_of(H::OpenCode),
        tool_headers(H::OpenCode, "ses_1"),
        &request_body("gpt-x", "a", json!({"store": false})),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "b/main");
    assert_eq!(c.hits_of("/responses"), 1);
}

#[tokio::test]
async fn a_member_that_refuses_an_extra_reasoning_field_gets_the_request_again_without_it() {
    let up = anthropic(Arc::new(|s| {
        let v: Value = serde_json::from_slice(&s.body).unwrap();
        if v.get("thinking").is_some() {
            Fake::json(
                400,
                json!({"error": {"message": "Unrecognized request argument supplied: thinking", "type": "invalid_request_error", "param": null, "code": null}}),
            )
        } else {
            chat_ok()
        }
    }))
    .await;
    let r = start(config(vec![krill("strict", &up)], vec![])).await;
    let mut body = chat_body("glm-5.3", "x", true);
    body["thinking"] = json!({"type": "enabled"});
    body["enable_thinking"] = json!(true);
    body["reasoning_effort"] = json!("high");
    body["reasoning"] = json!({"effort": "high"});
    let token = token_of(H::ZCode);
    for _ in 0..2 {
        let resp = post(
            &r,
            "/openai/v1/chat/completions",
            &token,
            tool_headers(H::ZCode, "z"),
            &body,
        )
        .await;
        assert_eq!(resp.status(), 200);
        let (text, _, _) = chat_accumulate(&resp.text().await.unwrap());
        assert_eq!(text, "Hello");
    }
    let bodies: Vec<Value> = (0..up.hits()).map(|i| sent(&up, i)).collect();
    assert_eq!(
        bodies.len(),
        3,
        "refused once, sent again, then left out from the start"
    );
    assert!(bodies[0].get("thinking").is_some());
    for b in &bodies[1..] {
        assert!(b.get("thinking").is_none());
        assert_eq!(b["enable_thinking"], true, "only the refused field goes");
        assert_eq!(b["reasoning_effort"], "high");
    }
}

#[tokio::test]
async fn a_genuine_400_is_passed_on_unchanged() {
    let up = anthropic(Arc::new(|_| {
        Fake::json(400, json!({"error": {"message": "messages: must not be empty", "type": "invalid_request_error"}}))
    }))
    .await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    let mut body = chat_body("glm-5.3", "x", false);
    body["thinking"] = json!({"type": "enabled"});
    let resp = post(
        &r,
        "/openai/v1/chat/completions",
        &token_of(H::ZCode),
        tool_headers(H::ZCode, "z"),
        &body,
    )
    .await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["message"], "messages: must not be empty");
    assert_eq!(up.hits(), 1);
}

#[tokio::test]
async fn configured_fields_are_left_out_on_both_protocols() {
    let up = anthropic(Arc::new(|s| {
        if s.path.ends_with("/chat/completions") {
            chat_ok()
        } else {
            responses_ok()
        }
    }))
    .await;
    let entry = OpenAiMember {
        drop_fields: vec!["metadata".into(), "service_tier".into()],
        ..superba_router::openai::OpenAiMember::default_for(&krill("k", &up), true)
    };
    let r = start(config(vec![krill("k", &up)], vec![("k", entry)])).await;
    let mut chat = chat_body("m", "x", true);
    chat["metadata"] = json!({"a": 1});
    let resp = post(
        &r,
        "/openai/v1/chat/completions",
        &token_of(H::Pi),
        tool_headers(H::Pi, "p"),
        &chat,
    )
    .await;
    let _ = resp.text().await;
    let resp = post(
        &r,
        "/openai/v1/responses",
        &token_of(H::Pi),
        tool_headers(H::Pi, "p"),
        &request_body(
            "m",
            "x",
            json!({"metadata": {"a": 1}, "service_tier": "priority"}),
        ),
    )
    .await;
    let _ = resp.text().await;
    for i in 0..2 {
        let b = sent(&up, i);
        assert!(b.get("metadata").is_none(), "{b}");
        assert!(b.get("service_tier").is_none(), "{b}");
        assert_eq!(b["model"], "m");
    }
}

#[tokio::test]
async fn tokens_are_checked_and_never_forwarded() {
    let up = anthropic(Arc::new(|_| chat_ok())).await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    let body = chat_body("m", "x", true);
    let resp = post(
        &r,
        "/openai/v1/chat/completions",
        "sbr_wrong",
        tool_headers(H::WorkBuddy, "c"),
        &body,
    )
    .await;
    assert_eq!(resp.status(), 401);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "router_auth");
    // WorkBuddy sends its key as a bearer and as X-API-Key.
    let token = token_of(H::WorkBuddy);
    let resp = r
        .http
        .post(r.url("/openai/v1/chat/completions"))
        .headers(tool_headers(H::WorkBuddy, "c"))
        .header("x-api-key", &token)
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].headers.contains_key("x-api-key"));
    assert_eq!(
        seen[0].headers["authorization"],
        format!("Bearer {}", key_of("k"))
    );
    for (_, v) in seen[0].headers.iter() {
        assert!(!v.to_str().unwrap_or_default().contains(&token));
    }
}

#[tokio::test]
async fn model_lists_choose_between_members() {
    let a = anthropic(Arc::new(|_| chat_ok())).await;
    let b = anthropic(Arc::new(|_| chat_ok())).await;
    let with_models = |ids: &[&str]| OpenAiMember {
        models: ids
            .iter()
            .map(|i| OpenAiModel {
                id: (*i).into(),
                display_name: None,
            })
            .collect(),
        ..chat_entry(false)
    };
    let r = start(config(
        vec![compatible("a", &a), compatible("b", &b)],
        vec![
            ("a", with_models(&["deepseek-chat"])),
            ("b", with_models(&["kimi-k3"])),
        ],
    ))
    .await;
    let mut routes = Vec::new();
    for model in ["kimi-k3", "deepseek-chat", "not-listed"] {
        let resp = post(
            &r,
            "/openai/v1/chat/completions",
            &token_of(H::OpenCode),
            tool_headers(H::OpenCode, model),
            &chat_body(model, "x", true),
        )
        .await;
        routes.push(route(&resp));
        let _ = resp.text().await;
    }
    assert_eq!(routes, ["b/main", "a/main", "a/main"]);
}

#[tokio::test]
async fn models_lists_every_members_models() {
    let a = anthropic(Arc::new(|_| chat_ok())).await;
    let b = anthropic(Arc::new(|s| {
        if s.path.starts_with("/v1/models") {
            Fake::json(
                200,
                json!({"object": "list", "data": [{"id": "m2"}, {"id": "m3"}]}),
            )
        } else {
            chat_ok()
        }
    }))
    .await;
    let a_entry = OpenAiMember {
        models: vec![
            OpenAiModel {
                id: "m1".into(),
                display_name: None,
            },
            OpenAiModel {
                id: "m2".into(),
                display_name: None,
            },
        ],
        ..chat_entry(false)
    };
    let r = start(config(
        vec![compatible("a", &a), compatible("b", &b)],
        vec![("a", a_entry)],
    ))
    .await;
    let get = |p: &str| r.http.get(r.url(p)).bearer_auth(token_of(H::Dsh)).send();
    let v: Value = get("/openai/v1/models")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["object"], "list");
    let ids: Vec<(&str, &str)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["id"].as_str().unwrap(), m["owned_by"].as_str().unwrap()))
        .collect();
    assert_eq!(ids, [("m1", "A"), ("m2", "A"), ("m3", "B")]);
    let one: Value = get("/openai/v1/models/m3")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["id"], "m3");
    assert_eq!(get("/openai/v1/models/zzz").await.unwrap().status(), 404);
    assert!(
        b.seen()
            .iter()
            .all(|s| s.headers["authorization"] == format!("Bearer {}", key_of("b")))
    );
}

#[tokio::test]
async fn a_responses_websocket_is_answered_426_and_unknown_paths_404() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    let resp = r
        .http
        .get(r.url("/openai/v1/responses"))
        .bearer_auth(token_of(H::Pi))
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 426);
    let resp = r
        .http
        .post(r.url("/openai/v1/embeddings"))
        .bearer_auth(token_of(H::Pi))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(up.hits(), 0);
}

#[tokio::test]
async fn harness_routes_name_the_ingress_and_a_token_the_router_accepts() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    let route_of =
        |h, p| superba_router::harness::route(r.addr, ROUTER_TOKEN.as_bytes(), h, p, None).unwrap();
    let pi = route_of(H::Pi, RequestProtocol::Responses);
    assert_eq!(pi.root, format!("http://{}", r.addr));
    assert_eq!(pi.base_url, format!("http://{}/openai/v1", r.addr));
    assert_eq!(pi.base_url, superba_router::openai::base_url(r.addr));
    assert_eq!(
        route_of(H::WorkBuddy, RequestProtocol::ChatCompletions).base_url,
        pi.base_url
    );
    assert_eq!(
        route_of(H::Dsh, RequestProtocol::Messages).base_url,
        format!("http://{}/claude", r.addr)
    );
    assert_eq!(pi.token.expose(), token_of(H::Pi));
    let resp = post(
        &r,
        "/openai/v1/responses",
        pi.token.expose(),
        tool_headers(H::Pi, "s"),
        &request_body("m", "x", json!({})),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(
        superba_router::harness::route(
            r.addr,
            b"t",
            H::Pi,
            RequestProtocol::Responses,
            Some("bad host!")
        )
        .is_none()
    );
}

#[tokio::test]
async fn codex_paths_keep_their_native_endpoint_and_public_fields() {
    let up = anthropic(Arc::new(|_| responses_ok())).await;
    let r = start(config(vec![krill("k", &up)], vec![])).await;
    let body = request_body("gpt-5.6-luna", "a", json!({"public_extra": 1}));
    let resp = r
        .http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let seen = up.seen();
    assert_eq!(seen[0].path, "/backend-api/codex/responses");
    assert_eq!(
        seen[0].body,
        serde_json::to_vec(&body).unwrap(),
        "byte for byte"
    );
}

#[tokio::test]
async fn dsh_private_fields_never_leave_the_claude_ingress_either() {
    use common::claude::{body, claude_config, claude_headers, claude_member, text_stream};
    let up = anthropic(Arc::new(|_| {
        Fake::sse(text_stream(
            "deepseek-chat",
            &["ok"],
            "end_turn",
            json!({"input_tokens": 1, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "output_tokens": 1}),
        ))
    }))
    .await;
    let (m, mut c) = claude_member("deepseek", &up);
    c.models = vec![superba_router::claude::ClaudeModel {
        id: "deepseek-chat".into(),
        display_name: None,
    }];
    let r = start(claude_config(vec![(m, c)])).await;
    let mut b = body("deepseek-chat", true);
    b["dsh_session_log"] = json!({"events": ["the whole conversation"]});
    b["dsh_plugin_packages"] = json!(["p"]);
    let resp = r
        .http
        .post(r.url("/claude/v1/messages"))
        .headers(claude_headers("s", None))
        .bearer_auth(token_of(H::Dsh))
        .body(serde_json::to_vec(&b).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let sent: Value = serde_json::from_slice(&up.messages()[0].body).unwrap();
    assert!(sent.get("dsh_session_log").is_none());
    assert!(sent.get("dsh_plugin_packages").is_none());
    assert_eq!(sent["messages"], b["messages"]);
}

#[tokio::test]
async fn applied_provider_tokens_pin_openai_requests_and_cannot_be_spoofed_by_headers() {
    use superba_router::token::harness_member_token;
    for chat in [false, true] {
        let a = anthropic(Arc::new(
            move |_| if chat { chat_ok() } else { responses_ok() },
        ))
        .await;
        let b = anthropic(Arc::new(
            move |_| if chat { chat_ok() } else { responses_ok() },
        ))
        .await;
        let r = start(config(vec![krill("a", &a), krill("b", &b)], vec![])).await;
        let token = harness_member_token(ROUTER_TOKEN.as_bytes(), H::Pi, "b").unwrap();
        let mut headers = tool_headers(H::Pi, "same-session");
        headers.insert("x-superba-harness-member", "a".parse().unwrap());
        let (path, body) = if chat {
            ("/openai/v1/chat/completions", chat_body("m", "hi", true))
        } else {
            (
                "/openai/v1/responses",
                request_body("m", "hi", json!({"store": false})),
            )
        };
        let response = post(&r, path, &token, headers, &body).await;
        assert_eq!(response.status(), 200);
        let _ = response.text().await.unwrap();
        assert_eq!(a.hits(), 0);
        assert_eq!(b.hits(), 1);
        assert!(!b.seen()[0].headers.contains_key("x-superba-harness-member"));
        // Reapplying another provider also works for an already sticky conversation.
        let token = harness_member_token(ROUTER_TOKEN.as_bytes(), H::Pi, "a").unwrap();
        let response = post(&r, path, &token, tool_headers(H::Pi, "same-session"), &body).await;
        assert_eq!(response.status(), 200);
        let _ = response.text().await.unwrap();
        assert_eq!(a.hits(), 1);
        assert_eq!(b.hits(), 1);
    }
}

#[tokio::test]
async fn an_unavailable_applied_provider_never_falls_back_to_another_provider() {
    use superba_router::token::harness_member_token;
    for chat in [false, true] {
        let a = anthropic(Arc::new(|_| responses_ok())).await;
        let b = anthropic(Arc::new(|_| Fake::json(503, json!({"error": "offline"})))).await;
        let r = start(config(vec![krill("a", &a), krill("b", &b)], vec![])).await;
        let (path, body) = if chat {
            ("/openai/v1/chat/completions", chat_body("m", "hi", true))
        } else {
            (
                "/openai/v1/responses",
                request_body("m", "hi", json!({"store": false})),
            )
        };
        for provider in ["b", "deleted-provider"] {
            let token =
                harness_member_token(ROUTER_TOKEN.as_bytes(), H::OpenCode, provider).unwrap();
            let response = post(&r, path, &token, HeaderMap::new(), &body).await;
            assert!(response.status().is_server_error() || response.status() == 404);
            let _ = response.text().await.unwrap();
        }
        assert_eq!(a.hits(), 0);
        assert_eq!(b.hits(), 1);
    }
}

#[tokio::test]
async fn a_bound_tools_model_list_contains_only_its_applied_provider() {
    use superba_router::token::harness_member_token;
    let a = anthropic(Arc::new(|_| chat_ok())).await;
    let b = anthropic(Arc::new(|_| chat_ok())).await;
    let entry = |id: &str| OpenAiMember {
        models: vec![OpenAiModel {
            id: id.into(),
            display_name: None,
        }],
        ..chat_entry(true)
    };
    let r = start(config(
        vec![compatible("a", &a), compatible("b", &b)],
        vec![("a", entry("only-a")), ("b", entry("only-b"))],
    ))
    .await;
    let token = harness_member_token(ROUTER_TOKEN.as_bytes(), H::ZCode, "b").unwrap();
    let v: Value = r
        .http
        .get(r.url("/openai/v1/models"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"][0]["id"], "only-b");
    assert_eq!(
        r.http
            .get(r.url("/openai/v1/models/only-a"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn applied_provider_tokens_also_pin_anthropic_messages_and_hide_internal_headers() {
    use common::claude::{body, claude_config, claude_headers, claude_member, text_stream};
    use superba_router::token::harness_member_token;
    let answer = || {
        anthropic(Arc::new(|_| {
            Fake::sse(text_stream(
                "model",
                &["ok"],
                "end_turn",
                json!({"input_tokens": 1, "output_tokens": 1}),
            ))
        }))
    };
    let a = answer().await;
    let b = answer().await;
    let r = start(claude_config(vec![
        claude_member("a", &a),
        claude_member("b", &b),
    ]))
    .await;
    let token = harness_member_token(ROUTER_TOKEN.as_bytes(), H::Dsh, "b").unwrap();
    let mut headers = claude_headers("s", None);
    headers.insert("x-superba-harness-member", "a".parse().unwrap());
    let response = post(
        &r,
        "/claude/v1/messages",
        &token,
        headers,
        &body("model", true),
    )
    .await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(a.hits(), 0);
    assert_eq!(b.messages().len(), 1);
    assert!(
        !b.messages()[0]
            .headers
            .contains_key("x-superba-harness-member")
    );
    let token = harness_member_token(ROUTER_TOKEN.as_bytes(), H::Dsh, "deleted").unwrap();
    let response = post(
        &r,
        "/claude/v1/messages",
        &token,
        claude_headers("s", None),
        &body("model", true),
    )
    .await;
    assert_eq!(response.status(), 404);
    assert_eq!(a.hits(), 0);
    assert_eq!(b.messages().len(), 1);
}
