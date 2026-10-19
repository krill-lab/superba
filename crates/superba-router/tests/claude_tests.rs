//! The Claude Code ingress through a running router with fake Anthropic
//! upstreams: pass-through, auth, failover and stickiness between two
//! Anthropic members, routes and aliases, per-member adjustments, model
//! lists, count_tokens, Codex unaffected.

mod common;

use std::sync::Arc;

use common::claude::*;
use common::*;
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::RequestHarness;
use superba_router::claude::{
    AnthropicAuth, BetaPolicy, ClaudeModel, ClaudeRoute, ClaudeTarget, Endpoint, ModelAlias,
};
use superba_router::token::harness_token;

async fn post(r: &Router, path: &str, headers: HeaderMap, body: &Value) -> reqwest::Response {
    r.http
        .post(r.url(path))
        .headers(headers)
        .bearer_auth(ROUTER_TOKEN)
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .unwrap()
}

fn listed(models: &[&str]) -> Vec<ClaudeModel> {
    models
        .iter()
        .map(|m| ClaudeModel {
            id: (*m).to_owned(),
            display_name: None,
        })
        .collect()
}

fn streaming(text: &'static [&'static str]) -> Handler {
    Arc::new(move |s: &Seen| {
        let model = serde_json::from_slice::<Value>(&s.body)
            .ok()
            .and_then(|v| v["model"].as_str().map(str::to_owned))
            .unwrap_or_default();
        Fake::sse(text_stream(
            &model,
            text,
            "end_turn",
            json!({"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 20}),
        ))
    })
}

#[tokio::test]
async fn messages_pass_through_with_the_members_own_key() {
    let up = anthropic(streaming(&["Hello", " world"])).await;
    let (m, mut c) = claude_member("deepseek", &up);
    c.models = listed(&["deepseek-chat"]);
    let r = start(claude_config(vec![(m, c)])).await;
    let req = body("deepseek-chat", true);
    let resp = post(
        &r,
        "/claude/v1/messages?beta=true",
        claude_headers("s-1", None),
        &req,
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "deepseek/main");
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let text = resp.text().await.unwrap();
    let expected = text_stream(
        "deepseek-chat",
        &["Hello", " world"],
        "end_turn",
        json!({"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 20}),
    )
    .concat();
    assert_eq!(text, expected, "the stream passes byte for byte");
    let seen = up.messages();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!(s.path, "/v1/messages?beta=true");
    assert_eq!(s.headers["x-api-key"], key_of("deepseek").as_str());
    assert!(!s.headers.contains_key("authorization"));
    assert_eq!(
        s.headers["anthropic-beta"],
        "claude-code-20250219,interleaved-thinking-2025-05-14"
    );
    assert_eq!(s.headers["anthropic-version"], "2023-06-01");
    assert!(!s.headers.contains_key("accept-encoding"));
    for (name, value) in &s.headers {
        assert!(!name.as_str().starts_with("x-superba"), "{name} leaked");
        assert!(
            !value.to_str().unwrap_or_default().contains(ROUTER_TOKEN),
            "router token leaked in {name}"
        );
    }
    assert_eq!(
        s.body,
        serde_json::to_vec(&req).unwrap(),
        "an unchanged body keeps its bytes"
    );

    // Not streamed: the JSON answer passes as it is.
    let answer = json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "deepseek-chat", "content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn", "stop_sequence": null, "usage": {"input_tokens": 3, "output_tokens": 1}});
    let a2 = answer.clone();
    up.set(Arc::new(move |_| Fake::json(200, a2.clone())));
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s-1", None),
        &body("deepseek-chat", false),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap(), answer);
}

#[tokio::test]
async fn the_router_token_is_taken_as_a_bearer_or_an_api_key() {
    let up = anthropic(streaming(&["ok"])).await;
    let (m, mut c) = claude_member("krill", &up);
    c.models = listed(&["gpt-5.5"]);
    let r = start(claude_config(vec![(m, c)])).await;
    let url = r.url("/claude/v1/messages");
    let send = |auth: Option<(&'static str, String)>| {
        let mut rb = r
            .http
            .post(&url)
            .headers(claude_headers("s-2", None))
            .body(serde_json::to_vec(&body("gpt-5.5", true)).unwrap());
        if let Some((k, v)) = auth {
            rb = rb.header(k, v);
        }
        rb.send()
    };
    let none = send(None).await.unwrap();
    assert_eq!(none.status(), 401);
    let e: Value = none.json().await.unwrap();
    assert_eq!(e["type"], "error");
    assert_eq!(e["error"]["type"], "authentication_error");
    assert_eq!(
        send(Some(("x-api-key", "sbr_wrong".into())))
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        send(Some(("x-api-key", ROUTER_TOKEN.into())))
            .await
            .unwrap()
            .status(),
        200
    );
    let token = harness_token(ROUTER_TOKEN.as_bytes(), RequestHarness::ClaudeCode, None).unwrap();
    assert_eq!(
        send(Some(("authorization", format!("Bearer {token}"))))
            .await
            .unwrap()
            .status(),
        200
    );
    // The router's credentials never reach the member.
    for s in up.messages() {
        assert_eq!(s.headers["x-api-key"], key_of("krill").as_str());
    }
}

#[tokio::test]
async fn failover_between_two_anthropic_members_sticks_to_the_one_that_answered() {
    let busy = anthropic(Arc::new(|_| {
        Fake::json(
            529,
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        )
    }))
    .await;
    let good = anthropic(streaming(&["from b"])).await;
    let (ma, mut ca) = claude_member("a", &busy);
    let (mb, mut cb) = claude_member("b", &good);
    ca.models = listed(&["m"]);
    cb.models = listed(&["m"]);
    let r = start(claude_config(vec![(ma, ca), (mb, cb)])).await;

    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s-a", None),
        &body("m", true),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "b/main");
    assert!(resp.text().await.unwrap().contains("from b"));
    assert_eq!((busy.messages().len(), good.messages().len()), (1, 1));

    // The same conversation stays on b.
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s-a", None),
        &body("m", true),
    )
    .await;
    assert_eq!(route(&resp), "b/main");
    let _ = resp.text().await;
    assert_eq!((busy.messages().len(), good.messages().len()), (1, 2));

    // Another conversation starts at a again (one soft failure does not open
    // its breaker); a second failure does.
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s-b", None),
        &body("m", true),
    )
    .await;
    assert_eq!(route(&resp), "b/main");
    let _ = resp.text().await;
    assert_eq!((busy.messages().len(), good.messages().len()), (2, 3));
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s-c", None),
        &body("m", true),
    )
    .await;
    let _ = resp.text().await;
    assert_eq!(busy.messages().len(), 2, "a's breaker is open");

    // A member that cannot be reached at all: the next one answers.
    let (mut md, mut cd) = claude_member("dead", &good);
    md.lines = vec![line("main", dead_base())];
    cd.models = listed(&["m"]);
    let (mb, mut cb) = claude_member("b", &good);
    cb.models = listed(&["m"]);
    let r2 = start(claude_config(vec![(md, cd), (mb, cb)])).await;
    let resp = post(
        &r2,
        "/claude/v1/messages",
        claude_headers("s-d", None),
        &body("m", true),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "b/main");
    let status = r2.status().await;
    assert!(status.totals.failovers >= 1);
}

#[tokio::test]
async fn client_errors_and_rate_limits_pass_through_without_failover() {
    let limited = anthropic(Arc::new(|_| {
        let mut f = Fake::json(
            429,
            json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}}),
        );
        f.headers.push(("retry-after", "7".into()));
        f
    }))
    .await;
    let other = anthropic(streaming(&["x"])).await;
    let (ma, mut ca) = claude_member("a", &limited);
    let (mb, mut cb) = claude_member("b", &other);
    ca.models = listed(&["m"]);
    cb.models = listed(&["m"]);
    let r = start(claude_config(vec![(ma, ca), (mb, cb)])).await;
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("m", true),
    )
    .await;
    assert_eq!(resp.status(), 429);
    assert_eq!(resp.headers()["retry-after"], "7");
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "rate_limit_error");
    assert!(other.messages().is_empty(), "no failover on 429");

    limited.set(Arc::new(|_| {
        Fake::json(
            400,
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}),
        )
    }));
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("m", true),
    )
    .await;
    assert_eq!(resp.status(), 400);
    assert!(other.messages().is_empty(), "no failover on 400");
}

#[tokio::test]
async fn routes_aliases_and_provider_adjustments_shape_the_request() {
    let up = anthropic(streaming(&["ok"])).await;
    let (m, mut c) = claude_member("glm", &up);
    c.anthropic = Some(Endpoint {
        path: "/api/anthropic".into(),
        ..Endpoint::default()
    });
    c.auth = AnthropicAuth::Bearer;
    c.betas = BetaPolicy::Drop;
    c.drop_fields = vec!["context_management".into()];
    c.system_messages = false;
    let mut cfg = claude_config(vec![(m, c)]);
    let claude = cfg.claude.as_mut().unwrap();
    claude.aliases.push(ModelAlias {
        from: "claude-haiku-*".into(),
        to: "glm-4.6".into(),
    });
    claude.routes.push(ClaudeRoute {
        model: "glm-*".into(),
        targets: vec![ClaudeTarget {
            member: "glm".into(),
            model: Some("GLM-4.6-upstream".into()),
        }],
    });
    let r = start(cfg).await;
    let mut req = body("claude-haiku-4-5-20251001", true);
    req["context_management"] =
        json!({"edits": [{"type": "clear_thinking_20251015", "keep": "all"}]});
    req["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "system", "content": [{"type": "text", "text": "env info"}]}));
    let resp = post(&r, "/claude/v1/messages", claude_headers("s", None), &req).await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let s = &up.messages()[0];
    assert_eq!(s.path, "/api/anthropic/v1/messages");
    assert_eq!(
        s.headers["authorization"],
        format!("Bearer {}", key_of("glm")).as_str()
    );
    assert!(!s.headers.contains_key("x-api-key"));
    assert!(!s.headers.contains_key("anthropic-beta"));
    let sent: Value = serde_json::from_slice(&s.body).unwrap();
    assert_eq!(sent["model"], "GLM-4.6-upstream");
    assert!(sent.get("context_management").is_none());
    let msgs = sent["messages"].as_array().unwrap();
    assert_eq!(
        msgs.len(),
        1,
        "the system message is folded into the user's"
    );
    let blocks = msgs[0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "hello");
    assert_eq!(
        blocks[1]["text"],
        "<system-reminder>\nenv info\n</system-reminder>"
    );
}

#[tokio::test]
async fn members_without_an_enabled_protocol_route_are_refused() {
    let up = anthropic(streaming(&["x"])).await;
    let (m, mut c) = claude_member("openai-only", &up);
    c.anthropic = None;
    c.models = listed(&["gpt-5.5"]);
    let mut cfg = claude_config(vec![(m, c)]);
    use superba_router::translate::{ir::Protocol, routing::Conversion};
    for to in [Protocol::Responses, Protocol::Chat, Protocol::Gemini] {
        cfg.translation.disabled.insert(Conversion {
            from: Protocol::Messages,
            to,
        });
    }
    let r = start(cfg).await;
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("gpt-5.5", true),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "not_found_error");
    assert!(up.seen().is_empty());
    let models: Value = r
        .http
        .get(r.url("/claude/v1/models"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"], json!([]), "ineligible members list nothing");

    // Without a Claude Code section every model is refused.
    let plain = start(test_config(vec![member(
        "k",
        superba_router::MemberKind::Krill,
        false,
        vec![line("a", up.base("/v1"))],
    )]))
    .await;
    let resp = post(
        &plain,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("m", true),
    )
    .await;
    assert_eq!(resp.status(), 404);
    // Unknown paths below the ingress answer in the Anthropic shape too.
    let resp = plain
        .http
        .get(plain.url("/claude/v1/nothing"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.json::<Value>().await.unwrap()["type"], "error");
}

#[tokio::test]
async fn models_are_the_routes_and_the_members_lists() {
    let listing = anthropic(Arc::new(|s: &Seen| {
        if s.path.starts_with("/v1/models") {
            if s.path.contains("after_id=b1") {
                return Fake::json(
                    200,
                    json!({"data": [{"type": "model", "id": "b2", "display_name": "Model B2"}], "has_more": false, "first_id": "b2", "last_id": "b2"}),
                );
            }
            return Fake::json(
                200,
                json!({"data": [{"type": "model", "id": "b1"}], "has_more": true, "first_id": "b1", "last_id": "b1"}),
            );
        }
        Fake::sse(text_stream("b2", &["from b"], "end_turn", json!({"input_tokens": 1, "output_tokens": 1})))
    }))
    .await;
    let other = anthropic(streaming(&["x"])).await;
    let (ma, mut ca) = claude_member("a", &other);
    ca.models = vec![
        ClaudeModel {
            id: "a1".into(),
            display_name: Some("Model A1".into()),
        },
        ClaudeModel {
            id: "a2".into(),
            display_name: None,
        },
    ];
    let (mb, cb) = claude_member("b", &listing);
    let mut cfg = claude_config(vec![(ma, ca), (mb, cb)]);
    cfg.claude.as_mut().unwrap().routes.push(ClaudeRoute {
        model: "fixed-model".into(),
        targets: vec![ClaudeTarget {
            member: "a".into(),
            model: None,
        }],
    });
    let r = start(cfg).await;
    let get = |path: &str| {
        r.http
            .get(r.url(path))
            .header("x-api-key", ROUTER_TOKEN)
            .send()
    };
    let all: Value = get("/claude/v1/models")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = all["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["fixed-model", "a1", "a2", "b1", "b2"]);
    assert_eq!(all["data"][1]["display_name"], "Model A1");
    assert_eq!(all["data"][4]["display_name"], "Model B2");
    assert_eq!(all["data"][0]["type"], "model");
    assert_eq!(all["has_more"], false);
    assert_eq!(all["first_id"], "fixed-model");
    assert_eq!(all["last_id"], "b2");
    let page: Value = get("/claude/v1/models?limit=2&after_id=a1")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["data"][0]["id"], "a2");
    assert_eq!(page["data"][1]["id"], "b1");
    assert_eq!(page["has_more"], true);
    let one: Value = get("/claude/v1/models/b2")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["id"], "b2");
    assert_eq!(get("/claude/v1/models/zzz").await.unwrap().status(), 404);
    // The member's list was read with its key and Anthropic's version header.
    let lists: Vec<Seen> = listing
        .seen()
        .into_iter()
        .filter(|s| s.path.starts_with("/v1/models"))
        .collect();
    assert_eq!(lists.len(), 2, "two pages, read once and kept");
    assert_eq!(lists[0].headers["x-api-key"], key_of("b").as_str());
    assert_eq!(lists[0].headers["anthropic-version"], "2023-06-01");
    // A model only b lists goes to b.
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("b2", true),
    )
    .await;
    assert_eq!(route(&resp), "b/main");
    assert!(resp.text().await.unwrap().contains("from b"));
}

#[tokio::test]
async fn count_tokens_is_passed_through_or_estimated() {
    let counting = anthropic(Arc::new(|s: &Seen| {
        if s.path.contains("count_tokens") {
            return Fake::json(200, json!({"input_tokens": 1234}));
        }
        Fake::sse(vec![])
    }))
    .await;
    let not_counting = anthropic(Arc::new(|s: &Seen| {
        if s.path.contains("count_tokens") {
            return Fake::json(404, json!({"error": "not found"}));
        }
        Fake::sse(vec![])
    }))
    .await;
    let (ma, mut ca) = claude_member("a", &counting);
    ca.models = listed(&["counted"]);
    let (mb, mut cb) = claude_member("b", &not_counting);
    cb.models = listed(&["estimated"]);
    let r = start(claude_config(vec![(ma, ca), (mb, cb)])).await;
    let count = |model: &str| {
        let r = &r;
        let model = model.to_owned();
        async move {
            let resp = post(
                r,
                "/claude/v1/messages/count_tokens?beta=true",
                claude_headers("s", None),
                &json!({"model": model, "messages": [{"role": "user", "content": "a".repeat(400)}]}),
            )
            .await;
            assert_eq!(resp.status(), 200);
            resp.json::<Value>().await.unwrap()["input_tokens"]
                .as_u64()
                .unwrap()
        }
    };
    assert_eq!(count("counted").await, 1234);
    let c = &counting.seen()[0];
    assert_eq!(c.path, "/v1/messages/count_tokens?beta=true");
    assert_eq!(c.headers["x-api-key"], key_of("a").as_str());
    assert_eq!(count("estimated").await, 104);
    assert_eq!(count("estimated").await, 104);
    let asked = not_counting
        .seen()
        .iter()
        .filter(|s| s.path.contains("count_tokens"))
        .count();
    assert_eq!(asked, 1, "a missing count_tokens is remembered");
}

#[tokio::test]
async fn codex_is_unaffected_and_never_uses_claude_only_members() {
    let claude_up = anthropic(streaming(&["c"])).await;
    let codex_up = upstream(Mode::Json(200, r#"{"id":"resp_1"}"#)).await;
    let (mc, mut cc) = claude_member("claude-only", &claude_up);
    cc.models = listed(&["m"]);
    cc.claude_only = true;
    let codex_member = member(
        "codex",
        superba_router::MemberKind::Compatible,
        false,
        vec![line("main", codex_up.base("/v1"))],
    );
    let mut cfg = claude_config(vec![(mc, cc)]);
    cfg.members.push(codex_member);
    let r = start(cfg).await;
    let resp = r
        .post(Some("sess"), r#"{"model":"gpt-x","input":[]}"#)
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "codex/main");
    assert!(claude_up.seen().is_empty());
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("m", true),
    )
    .await;
    assert_eq!(route(&resp), "claude-only/main");
}

#[tokio::test]
async fn claude_codes_own_member_order_comes_before_the_routers() {
    let a = anthropic(streaming(&["from a"])).await;
    let b = anthropic(streaming(&["from b"])).await;
    let (ma, mut ca) = claude_member("a", &a);
    let (mb, mut cb) = claude_member("b", &b);
    ca.models = listed(&["m"]);
    cb.models = listed(&["m"]);
    let mut cfg = claude_config(vec![(ma, ca), (mb, cb)]);
    cfg.claude.as_mut().unwrap().order = vec!["b".into()];
    let r = start(cfg).await;
    let resp = post(
        &r,
        "/claude/v1/messages",
        claude_headers("s", None),
        &body("m", true),
    )
    .await;
    assert_eq!(route(&resp), "b/main");
    let models: Value = r
        .http
        .get(r.url("/claude/v1/models"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"].as_array().unwrap().len(), 1);
}
