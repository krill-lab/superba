//! Fake-upstream coverage of the real HTTP ingress and WS/HTTP bridge.
mod common;
use common::claude::{Anthropic, Fake, anthropic};
use common::*;
use serde_json::{Value, json};
use std::sync::Arc;
use superba_core::{CompactionSupport, RequestHarness};
use superba_router::{
    MemberKind, RouterConfig,
    claude::{ClaudeConfig, ClaudeMember, ClaudeModel, Endpoint},
    openai::{OpenAiConfig, OpenAiEndpoint, OpenAiMember, OpenAiModel},
    translate::{
        self,
        ir::*,
        routing::{Conversion, GeminiConfig, GeminiMember},
    },
};

fn config(up: &Anthropic, protocol: Protocol) -> RouterConfig {
    let mut c = test_config(vec![member(
        "site",
        MemberKind::Compatible,
        false,
        vec![line("main", up.base("/native"))],
    )]);
    c.claude = Some(ClaudeConfig {
        members: [(
            "site".into(),
            ClaudeMember {
                anthropic: (protocol == Protocol::Messages).then(Endpoint::default),
                models: vec![ClaudeModel {
                    id: "model".into(),
                    display_name: None,
                }],
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    });
    c.openai = Some(OpenAiConfig {
        members: [(
            "site".into(),
            OpenAiMember {
                responses: (protocol == Protocol::Responses).then(OpenAiEndpoint::line_base),
                chat: (protocol == Protocol::Chat).then(OpenAiEndpoint::line_base),
                models: vec![OpenAiModel {
                    id: "model".into(),
                    display_name: None,
                }],
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    });
    c.gemini = Some(GeminiConfig {
        members: [(
            "site".into(),
            GeminiMember {
                endpoint: (protocol == Protocol::Gemini).then(|| OpenAiEndpoint::at("/v1beta")),
                models: vec![OpenAiModel {
                    id: "model".into(),
                    display_name: None,
                }],
            },
        )]
        .into(),
        ..Default::default()
    });
    c
}

fn path(p: Protocol, stream: bool) -> &'static str {
    match p {
        Protocol::Messages => "/claude/v1/messages",
        Protocol::Responses => "/openai/v1/responses",
        Protocol::Chat => "/openai/v1/chat/completions",
        Protocol::Gemini => {
            if stream {
                "/gemini/v1beta/models/model:streamGenerateContent?alt=sse"
            } else {
                "/gemini/v1beta/models/model:generateContent"
            }
        }
    }
}

fn request(p: Protocol, stream: bool) -> Value {
    let r = Request {
        model: "model".into(),
        stream,
        max_output_tokens: Some(128),
        messages: vec![Message {
            role: Role::User,
            content: vec![Block::text("Hello")],
        }],
        ..Default::default()
    };
    let mut v = translate::encode_request(p, &r).unwrap();
    if p == Protocol::Gemini {
        v.as_object_mut().unwrap().remove("model");
        v.as_object_mut().unwrap().remove("stream");
    }
    v
}

fn stream(p: Protocol) -> &'static str {
    match p {
        Protocol::Messages => include_str!("fixtures/translate/messages.sse"),
        Protocol::Responses => include_str!("fixtures/translate/responses.sse"),
        Protocol::Chat => include_str!("fixtures/translate/chat.sse"),
        Protocol::Gemini => include_str!("fixtures/translate/gemini.sse"),
    }
}

async fn pair(client: Protocol, upstream: Protocol) {
    let up = anthropic(Arc::new(move |_| Fake::sse(vec![stream(upstream).into()]))).await;
    let r = start(config(&up, upstream)).await;
    let response = r
        .http
        .post(r.url(path(client, true)))
        .bearer_auth(ROUTER_TOKEN)
        .json(&request(client, true))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "{client:?}->{upstream:?}: {}",
        response.text().await.unwrap()
    );
    let body = response.text().await.unwrap();
    assert!(
        body.contains("call_a") && body.contains("read"),
        "{client:?}->{upstream:?}: {body}"
    );
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    let expected = match upstream {
        Protocol::Messages => "/v1/messages",
        Protocol::Responses => "/native/responses",
        Protocol::Chat => "/native/chat/completions",
        Protocol::Gemini => "/v1beta/models/model:streamGenerateContent?alt=sse",
    };
    assert_eq!(seen[0].path, expected);
    let credential = match upstream {
        Protocol::Messages => seen[0].headers["x-api-key"].to_str().unwrap().to_owned(),
        Protocol::Gemini => seen[0].headers["x-goog-api-key"]
            .to_str()
            .unwrap()
            .to_owned(),
        _ => seen[0].headers["authorization"]
            .to_str()
            .unwrap()
            .trim_start_matches("Bearer ")
            .to_owned(),
    };
    assert_eq!(credential, key_of("site"));
    assert!(!seen[0].path.contains(ROUTER_TOKEN));
    assert!(!seen[0].headers.contains_key("x-superba-harness"));
    let wire: Value = serde_json::from_slice(&seen[0].body).unwrap();
    if upstream == Protocol::Gemini {
        assert!(wire.get("model").is_none());
        assert!(wire.get("stream").is_none());
    }
    r.handle.shutdown();
}

macro_rules! integration {
    ($name:ident,$from:ident,$to:ident) => {
        #[tokio::test]
        async fn $name() {
            pair(Protocol::$from, Protocol::$to).await;
        }
    };
}
integration!(messages_responses, Messages, Responses);
integration!(messages_chat, Messages, Chat);
integration!(messages_gemini, Messages, Gemini);
integration!(responses_messages, Responses, Messages);
integration!(responses_chat, Responses, Chat);
integration!(responses_gemini, Responses, Gemini);
integration!(chat_messages, Chat, Messages);
integration!(chat_responses, Chat, Responses);
integration!(chat_gemini, Chat, Gemini);
integration!(gemini_messages, Gemini, Messages);
integration!(gemini_responses, Gemini, Responses);
integration!(gemini_chat, Gemini, Chat);

#[tokio::test]
async fn native_gemini_is_byte_preserving_and_query_key_is_not_forwarded() {
    let body = " { \"contents\" : [{\"role\":\"user\",\"parts\":[{\"text\":\"hello\"}]}], \"futureField\":true } ";
    let answer = " { \"candidates\" : [] , \"futureAnswer\" : true } ";
    let up = anthropic(Arc::new(move |_| Fake {
        status: 200,
        content_type: "application/json",
        chunks: vec![answer.into()],
        headers: vec![],
    }))
    .await;
    let r = start(config(&up, Protocol::Gemini)).await;
    let response = r
        .http
        .post(r.url(&format!(
            "{}?key={ROUTER_TOKEN}",
            path(Protocol::Gemini, false)
        )))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), answer);
    assert_eq!(&up.seen()[0].body[..], body.as_bytes());
    assert!(!up.seen()[0].path.contains("key="));
    r.handle.shutdown();
}

#[tokio::test]
async fn gemini_models_auth_and_harness_tokens() {
    let up = anthropic(Arc::new(|_| {
        Fake::json(200, json!({"data":[{"id":"model"}]}))
    }))
    .await;
    let r = start(config(&up, Protocol::Chat)).await;
    let token =
        superba_router::token::harness_token(ROUTER_TOKEN.as_bytes(), RequestHarness::Gemini, None)
            .unwrap();
    assert_eq!(
        r.http
            .get(r.url("/gemini/v1beta/models"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = r
        .http
        .get(r.url("/gemini/v1beta/models"))
        .header("x-goog-api-key", token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let v: Value = response.json().await.unwrap();
    assert_eq!(v["models"][0]["name"], "models/model");
    r.handle.shutdown();
}

#[tokio::test]
async fn all_directional_switches_are_enforced_and_native_always_wins() {
    let up = anthropic(Arc::new(|_| {
        Fake::json(500, json!({"error":{"message":"must not be called"}}))
    }))
    .await;
    for from in Protocol::ALL {
        for to in Protocol::ALL {
            if from == to {
                continue;
            }
            let mut c = config(&up, to);
            assert_eq!(c.upstream_protocol("site", from), Some(to));
            c.translation.disabled.insert(Conversion { from, to });
            assert_eq!(c.upstream_protocol("site", from), None);
            assert_eq!(c.upstream_protocol("site", to), Some(to));
        }
    }
    let mut c = config(&up, Protocol::Chat);
    c.translation.disabled.insert(Conversion {
        from: Protocol::Responses,
        to: Protocol::Chat,
    });
    let r = start(c).await;
    let resp = r
        .http
        .post(r.url(path(Protocol::Responses, false)))
        .bearer_auth(ROUTER_TOKEN)
        .json(&request(Protocol::Responses, false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert!(up.seen().is_empty());
    r.handle.shutdown();
}

#[tokio::test]
async fn compact_never_uses_conversion_even_if_member_claims_supported() {
    let up = anthropic(Arc::new(|_| Fake::json(500, json!({"error":{}})))).await;
    for protocol in [Protocol::Messages, Protocol::Chat, Protocol::Gemini] {
        let mut c = config(&up, protocol);
        c.members[0].compaction = CompactionSupport::Native;
        assert_eq!(c.compaction_support("site"), CompactionSupport::Unsupported);
        let r = start(c).await;
        let response = r
            .http
            .post(r.url("/backend-api/codex/responses/compact"))
            .bearer_auth(ROUTER_TOKEN)
            .json(&json!({"model":"model","input":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("compaction_unsupported")
        );
        r.handle.shutdown();
    }
    assert!(up.seen().is_empty());
}

#[tokio::test]
async fn codex_http_reaches_chat_only_member() {
    let up = anthropic(Arc::new(|_| Fake::sse(vec![stream(Protocol::Chat).into()]))).await;
    let r = start(config(&up, Protocol::Chat)).await;
    let resp = r
        .http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .json(&request(Protocol::Responses, true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("response.completed"));
    assert_eq!(up.seen()[0].path, "/native/chat/completions");
    r.handle.shutdown();
}

#[tokio::test]
async fn codex_websocket_uses_http_and_replays_local_continuations() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message as WsMessage, client::IntoClientRequest};
    let up = anthropic(Arc::new(|_| Fake::sse(vec![stream(Protocol::Chat).into()]))).await;
    let r = start(config(&up, Protocol::Chat)).await;
    let mut upgrade = r
        .url("/backend-api/codex/responses")
        .replacen("http:", "ws:", 1)
        .into_client_request()
        .unwrap();
    upgrade.headers_mut().insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    let (mut socket, response) = tokio_tungstenite::connect_async(upgrade).await.unwrap();
    assert_eq!(response.status(), 101);
    let mut first = request(Protocol::Responses, true);
    first["type"] = "response.create".into();
    socket
        .send(WsMessage::Text(first.to_string().into()))
        .await
        .unwrap();
    let id: String;
    let mut saw_args = false;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let WsMessage::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            saw_args |= v["type"] == "response.function_call_arguments.delta";
            if v["type"] == "response.completed" {
                id = v["response"]["id"].as_str().unwrap().into();
                break;
            }
        }
    }
    assert!(saw_args);
    assert!(!id.is_empty());
    let second = json!({"type":"response.create","model":"model","previous_response_id":id,"input":[{"type":"function_call_output","call_id":"call_a","output":"done"}]});
    socket
        .send(WsMessage::Text(second.to_string().into()))
        .await
        .unwrap();
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let WsMessage::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            assert_ne!(v["type"], "error", "{v}");
            if v["type"] == "response.completed" {
                break;
            }
        }
    }
    assert_eq!(up.seen().len(), 2);
    let second: Value = serde_json::from_slice(&up.seen()[1].body).unwrap();
    assert!(
        second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_a")
    );
    socket.close(None).await.unwrap();
    r.handle.shutdown();
}
