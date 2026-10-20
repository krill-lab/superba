//! Discovery, probing and execution must agree on the actual upstream protocol.
mod common;

use common::claude::{Anthropic, Fake, Handler, anthropic};
use common::*;
use serde_json::{Value, json};
use std::sync::Arc;
use superba_core::{CatalogReadiness, RouterCatalogView};
use superba_router::{
    MemberKind, RouterConfig,
    claude::{ClaudeConfig, ClaudeMember, Endpoint},
    merged_catalog::{CatalogConfig, MemberCatalog},
    openai::{OpenAiConfig, OpenAiEndpoint, OpenAiMember},
    translate::{
        ir::Protocol,
        routing::{Conversion, GeminiConfig, GeminiMember},
    },
};

const MODEL: &str = "converted-test";

fn native_entry(id: &str) -> Value {
    json!({"slug":id,"display_name":id,"priority":1,"shell_type":"unified_exec","visibility":"list",
        "supported_reasoning_levels":[],"experimental_supported_tools":[],"supported_in_api":true,
        "support_verbosity":false,"truncation_policy":{"mode":"bytes","limit":10000},"base_instructions":"Use tools."})
}

fn handler(protocol: Protocol, passes: bool, sse: bool) -> Handler {
    Arc::new(move |seen| {
        if seen.method == "GET" {
            return if seen.path == "/v1/models" {
                Fake::json(200, json!({"data":[{"id":MODEL},{"id":"gpt-test"}]}))
            } else {
                // A Codex-shaped entry must not bypass conversion verification.
                Fake::json(200, json!({"models":[native_entry(MODEL)]}))
            };
        }
        let v: Value = serde_json::from_slice(&seen.body).unwrap();
        let (forced, result) = match protocol {
            Protocol::Chat => {
                assert_eq!(seen.path, "/inference/chat/completions");
                assert!(seen.headers.contains_key("authorization"));
                assert_eq!(v["model"], MODEL);
                assert!(v["input"].is_null());
                let result = v["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["role"] == "tool");
                if let Some(result) = result {
                    assert_eq!(result["tool_call_id"], "probe_call");
                }
                (
                    v["tool_choice"]["function"]["name"] == "get_probe_value",
                    result.map(|r| r["content"].as_str().unwrap().to_owned()),
                )
            }
            Protocol::Messages => {
                assert_eq!(seen.path, "/inference/v1/messages");
                assert!(seen.headers.contains_key("x-api-key"));
                assert!(!seen.headers.contains_key("authorization"));
                assert_eq!(v["model"], MODEL);
                assert!(v["input"].is_null());
                let result = v["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["content"].as_array().into_iter().flatten())
                    .find(|p| p["type"] == "tool_result");
                if let Some(result) = result {
                    assert_eq!(result["tool_use_id"], "probe_call");
                }
                (
                    v["tool_choice"]["name"] == "get_probe_value",
                    result.map(|r| match &r["content"] {
                        Value::String(s) => s.clone(),
                        v => v.to_string(),
                    }),
                )
            }
            Protocol::Gemini => {
                assert!(seen.path.starts_with("/inference/models/converted-test:"));
                assert!(seen.headers.contains_key("x-goog-api-key"));
                assert!(!seen.headers.contains_key("authorization"));
                assert!(v["model"].is_null() && v["stream"].is_null() && v["input"].is_null());
                let result = v["contents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["parts"].as_array().into_iter().flatten())
                    .find(|p| p["functionResponse"].is_object());
                if let Some(result) = result {
                    assert_eq!(result["functionResponse"]["name"], "get_probe_value");
                }
                (
                    v["toolConfig"]["functionCallingConfig"]["mode"] == "ANY",
                    result.map(|r| r["functionResponse"]["response"].to_string()),
                )
            }
            Protocol::Responses => unreachable!(),
        };
        let text = if passes {
            result.unwrap_or_else(|| "ORCHID-731 cobalt".into())
        } else {
            "wrong tool result".into()
        };
        let response = match protocol {
            Protocol::Chat => {
                json!({"id":"chat_probe","model":MODEL,"choices":[{"index":0,"message":if forced {json!({"role":"assistant","content":null,"tool_calls":[{"id":"probe_call","type":"function","function":{"name":"get_probe_value","arguments":"{}"}}]})} else {json!({"role":"assistant","content":text})},"finish_reason":if forced {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":40,"completion_tokens":10}})
            }
            Protocol::Messages => {
                json!({"id":"msg_probe","type":"message","role":"assistant","model":MODEL,"content":if forced {json!([{"type":"tool_use","id":"probe_call","name":"get_probe_value","input":{}}])} else {json!([{"type":"text","text":text}])},"stop_reason":if forced {"tool_use"} else {"end_turn"},"usage":{"input_tokens":40,"output_tokens":10}})
            }
            Protocol::Gemini => {
                json!({"responseId":"gemini_probe","modelVersion":MODEL,"candidates":[{"index":0,"content":{"role":"model","parts":if forced {json!([{"functionCall":{"id":"probe_call","name":"get_probe_value","args":{}}}])} else {json!([{"text":text}])}},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":40,"candidatesTokenCount":10}})
            }
            Protocol::Responses => unreachable!(),
        };
        if !sse {
            return Fake::json(200, response);
        }
        let events = match protocol {
            Protocol::Chat => {
                let mut delta = response["choices"][0]["message"].clone();
                if let Some(calls) = delta["tool_calls"].as_array_mut() {
                    calls[0]["index"] = json!(0);
                }
                format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"chat_probe","model":MODEL,"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                    json!({"id":"chat_probe","choices":[{"index":0,"delta":{},"finish_reason":response["choices"][0]["finish_reason"]}],"usage":response["usage"]})
                )
            }
            Protocol::Messages => {
                let mut start = response.clone();
                start["content"] = json!([]);
                start["stop_reason"] = Value::Null;
                let mut block = response["content"][0].clone();
                if !forced {
                    block["text"] = json!("");
                }
                let delta = if forced {
                    json!({"type":"input_json_delta","partial_json":"{}"})
                } else {
                    json!({"type":"text_delta","text":response["content"][0]["text"]})
                };
                [json!({"type":"message_start","message":start}),json!({"type":"content_block_start","index":0,"content_block":block}),json!({"type":"content_block_delta","index":0,"delta":delta}),json!({"type":"content_block_stop","index":0}),json!({"type":"message_delta","delta":{"stop_reason":response["stop_reason"]},"usage":{"output_tokens":10}}),json!({"type":"message_stop"})].into_iter().map(|e| format!("event: {}\ndata: {e}\n\n",e["type"].as_str().unwrap())).collect()
            }
            Protocol::Gemini => format!("data: {response}\n\n"),
            Protocol::Responses => unreachable!(),
        };
        Fake::sse(vec![events])
    })
}

fn config(ups: &[(&str, &Anthropic, Protocol)]) -> RouterConfig {
    let mut cfg = test_config(
        ups.iter()
            .map(|(id, up, _)| {
                member(
                    id,
                    MemberKind::Compatible,
                    false,
                    vec![line("one", up.base("/v1"))],
                )
            })
            .collect(),
    );
    cfg.catalog = Some(CatalogConfig {
        bundled: vec![native_entry("gpt-test")],
        members: ups
            .iter()
            .map(|(id, _, _)| {
                (
                    id.to_string(),
                    MemberCatalog {
                        name: id.to_string(),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.openai = Some(OpenAiConfig {
        members: ups
            .iter()
            .map(|(id, up, p)| {
                (
                    id.to_string(),
                    OpenAiMember {
                        chat: (*p == Protocol::Chat).then(|| OpenAiEndpoint {
                            lines: [("one".into(), up.base("/inference"))].into(),
                            ..OpenAiEndpoint::line_base()
                        }),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.claude = Some(ClaudeConfig {
        members: ups
            .iter()
            .map(|(id, up, p)| {
                (
                    id.to_string(),
                    ClaudeMember {
                        anthropic: (*p == Protocol::Messages).then(|| Endpoint {
                            lines: [("one".into(), up.base("/inference"))].into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.gemini = Some(GeminiConfig {
        members: ups
            .iter()
            .map(|(id, up, p)| {
                (
                    id.to_string(),
                    GeminiMember {
                        endpoint: (*p == Protocol::Gemini).then(|| OpenAiEndpoint {
                            lines: [("one".into(), up.base("/inference"))].into(),
                            ..OpenAiEndpoint::line_base()
                        }),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg
}

async fn view(r: &Router) -> RouterCatalogView {
    r.http
        .get(r.url("/_superba/v1/catalog"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn probe(r: &Router, model: &str) -> superba_core::RouterModelTest {
    r.http
        .post(r.url("/_superba/v1/catalog/test"))
        .bearer_auth(&r.admin_token)
        .json(&json!({"model":model}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn published(r: &Router, model: &str) -> bool {
    let value: Value = r
        .http
        .get(r.url("/models"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    value["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["slug"] == model)
}
fn completed(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| {
        text.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .find(|e| e["type"] == "response.completed")
            .unwrap()["response"]
            .clone()
    })
}

#[tokio::test]
async fn converted_catalog_probes_and_compacts_through_each_real_protocol_adapter() {
    for protocol in [Protocol::Chat, Protocol::Messages, Protocol::Gemini] {
        for sse in [false, true] {
            let up = anthropic(handler(protocol, true, sse)).await;
            let r = start(config(&[("provider", &up, protocol)])).await;
            let before = view(&r).await;
            let row = before
                .models
                .iter()
                .find(|m| m.entry["slug"] == MODEL)
                .unwrap();
            assert_eq!(row.readiness, CatalogReadiness::Pending);
            assert!(row.sources.iter().all(|s| s.translated()));
            assert!(!published(&r, MODEL).await);
            // L0 names listed by this converted provider also need their own probe.
            assert!(!published(&r, "gpt-test").await);
            assert_eq!(
                r.post(None, r#"{"model":"gpt-test","input":"private"}"#)
                    .await
                    .status(),
                409
            );
            let result = probe(&r, &format!("provider/{MODEL}")).await;
            assert!(result.passed, "{protocol:?} sse={sse}: {}", result.detail);
            assert_eq!(up.seen().iter().filter(|s| s.method == "POST").count(), 2);
            assert!(probe(&r, MODEL).await.passed);
            assert_eq!(
                up.seen().iter().filter(|s| s.method == "POST").count(),
                2,
                "cached probe"
            );
            for model in [MODEL, &format!("provider/{MODEL}")] {
                assert!(published(&r, model).await);
                let response = r
                    .post(
                        None,
                        &json!({"model":model,"input":"hello","stream":sse}).to_string(),
                    )
                    .await;
                assert_eq!(response.status(), 200);
                assert_eq!(
                    completed(&response.text().await.unwrap())["status"],
                    "completed"
                );
                let response=r.http.post(r.url("/backend-api/codex/responses/compact")).bearer_auth(ROUTER_TOKEN).json(&json!({"model":model,"input":[{"role":"user","content":"Remember ORCHID-731 cobalt"}],"stream":sse})).send().await.unwrap();
                assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
                let answer = completed(&response.text().await.unwrap());
                let capsule = answer["output"][0].clone();
                assert!(
                    capsule["encrypted_content"]
                        .as_str()
                        .unwrap()
                        .starts_with(superba_router::compaction_proxy::PREFIX)
                );
                let resumed=r.post(None,&json!({"model":model,"input":[capsule,{"role":"user","content":"Recall"}],"stream":sse}).to_string()).await;
                assert_eq!(resumed.status(), 200, "{}", resumed.text().await.unwrap());
                assert!(
                    completed(&resumed.text().await.unwrap())
                        .to_string()
                        .contains("ORCHID-731")
                );
                let seen = up.seen().last().unwrap().clone();
                let body = String::from_utf8_lossy(&seen.body);
                assert!(
                    body.contains("ORCHID-731")
                        && !body.contains("encrypted_content")
                        && !body.contains("compaction_trigger")
                );
            }
            r.handle.shutdown();
        }
    }
}

#[tokio::test]
async fn failed_converted_probe_never_publishes_or_executes_even_with_client_bypass_headers() {
    let up = anthropic(handler(Protocol::Chat, false, true)).await;
    let r = start(config(&[("provider", &up, Protocol::Chat)])).await;
    let result = probe(&r, MODEL).await;
    assert!(!result.passed);
    assert!(result.detail.contains("tool's result"));
    let before = up.seen().len();
    for model in [MODEL, &format!("provider/{MODEL}")] {
        assert!(!published(&r, model).await);
        let reply = r
            .http
            .post(r.url("/backend-api/codex/responses"))
            .bearer_auth(ROUTER_TOKEN)
            .header("x-superba-probe", "true")
            .json(&json!({"model":model,"input":"private"}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 409);
    }
    assert_eq!(up.seen().len(), before);
    assert_eq!(
        view(&r)
            .await
            .models
            .iter()
            .find(|m| m.entry["slug"] == MODEL)
            .unwrap()
            .readiness,
        CatalogReadiness::Failed
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn converted_namespace_stays_pinned_and_bare_model_fails_over_in_member_order() {
    let a = anthropic(handler(Protocol::Chat, true, false)).await;
    let b = anthropic(handler(Protocol::Messages, true, false)).await;
    let r = start(config(&[
        ("first", &a, Protocol::Chat),
        ("second", &b, Protocol::Messages),
    ]))
    .await;
    assert!(probe(&r, &format!("first/{MODEL}")).await.passed);
    assert!(probe(&r, &format!("second/{MODEL}")).await.passed);
    let b_before = b.seen().len();
    a.set(Arc::new(|_| {
        Fake::json(503, json!({"error":"unavailable"}))
    }));
    assert_eq!(
        r.post(
            None,
            &json!({"model":format!("first/{MODEL}"),"input":"hello"}).to_string()
        )
        .await
        .status(),
        502
    );
    assert_eq!(b.seen().len(), b_before);
    assert_eq!(
        r.post(None, &json!({"model":MODEL,"input":"hello"}).to_string())
            .await
            .status(),
        200
    );
    assert_eq!(b.seen().len(), b_before + 1);
    r.handle.shutdown();
}

#[tokio::test]
async fn conversion_switch_and_endpoint_reload_invalidate_successful_evidence() {
    for protocol in [Protocol::Chat, Protocol::Messages, Protocol::Gemini] {
        let up = anthropic(handler(protocol, true, false)).await;
        let mut cfg = config(&[("provider", &up, protocol)]);
        let temp = tempfile::tempdir().unwrap();
        let paths = superba_router::RouterPaths::from_dir(temp.path().join("router"));
        cfg.save(&paths).unwrap();
        secrets_for(&cfg).save(&paths).unwrap();
        let server = superba_router::RouterServer::bind_from_files(paths.clone())
            .await
            .unwrap();
        let r = Router {
            addr: server.local_addr(),
            admin_token: server.admin_token().expose().into(),
            handle: server.shutdown_handle(),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        };
        let task = tokio::spawn(server.serve());
        assert!(probe(&r, MODEL).await.passed);
        cfg.translation.disabled.insert(Conversion {
            from: Protocol::Responses,
            to: protocol,
        });
        reload(&r, &cfg, &paths).await;
        assert!(!published(&r, MODEL).await);
        assert!(!probe(&r, MODEL).await.passed);
        assert_eq!(
            r.post(None, &json!({"model":MODEL,"input":"private"}).to_string())
                .await
                .status(),
            409
        );
        cfg.translation.disabled.clear();
        reload(&r, &cfg, &paths).await;
        assert!(
            !published(&r, MODEL).await,
            "reenabling does not resurrect stale proof"
        );
        assert!(probe(&r, MODEL).await.passed);
        match protocol {
            Protocol::Chat => {
                cfg.openai
                    .as_mut()
                    .unwrap()
                    .members
                    .get_mut("provider")
                    .unwrap()
                    .chat
                    .as_mut()
                    .unwrap()
                    .lines
                    .insert("one".into(), up.base("/changed"));
            }
            Protocol::Messages => {
                cfg.claude
                    .as_mut()
                    .unwrap()
                    .members
                    .get_mut("provider")
                    .unwrap()
                    .anthropic
                    .as_mut()
                    .unwrap()
                    .lines
                    .insert("one".into(), up.base("/changed"));
            }
            Protocol::Gemini => {
                cfg.gemini
                    .as_mut()
                    .unwrap()
                    .members
                    .get_mut("provider")
                    .unwrap()
                    .endpoint
                    .as_mut()
                    .unwrap()
                    .lines
                    .insert("one".into(), up.base("/changed"));
            }
            _ => unreachable!(),
        }
        reload(&r, &cfg, &paths).await;
        assert!(!published(&r, MODEL).await);
        r.handle.shutdown();
        task.await.unwrap().unwrap();
    }
}
async fn reload(r: &Router, cfg: &RouterConfig, paths: &superba_router::RouterPaths) {
    cfg.save(paths).unwrap();
    assert_eq!(
        r.http
            .post(r.url("/_superba/v1/reload"))
            .bearer_auth(&r.admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn unverified_converted_provider_keeps_native_bundled_fallback_available() {
    let converted = anthropic(handler(Protocol::Chat, false, false)).await;
    let native = anthropic(Arc::new(|seen| {
        if seen.method == "GET" {
            return Fake::json(200, json!({"data":[{"id":"gpt-test"}]}));
        }
        assert_eq!(seen.path, "/v1/responses");
        Fake::json(200, json!({"id":"native","status":"completed","output":[]}))
    }))
    .await;
    for converted_first in [false, true] {
        let mut cfg = config(&[
            ("converted", &converted, Protocol::Chat),
            ("native", &native, Protocol::Responses),
        ]);
        cfg.openai
            .as_mut()
            .unwrap()
            .members
            .get_mut("native")
            .unwrap()
            .responses = Some(OpenAiEndpoint::line_base());
        if !converted_first {
            cfg.members.reverse();
        }
        let r = start(cfg).await;
        assert!(published(&r, "gpt-test").await);
        assert!(published(&r, "native/gpt-test").await);
        assert!(!published(&r, "converted/gpt-test").await);
        assert_eq!(
            r.post(None, r#"{"model":"gpt-test","input":"hello"}"#)
                .await
                .status(),
            200
        );
        assert_eq!(
            r.post(None, r#"{"model":"converted/gpt-test","input":"hello"}"#)
                .await
                .status(),
            409
        );
        assert!(converted.seen().iter().all(|s| s.method == "GET"));
        r.handle.shutdown();
    }
}

#[tokio::test]
async fn probe_uses_model_mapping_and_stays_on_each_selected_provider_line() {
    let up = anthropic(handler(Protocol::Chat, true, false)).await;
    let bad = anthropic(Arc::new(|_| {
        Fake::json(503, json!({"error":"unavailable"}))
    }))
    .await;
    let mut cfg = config(&[("provider", &bad, Protocol::Chat)]);
    cfg.members[0]
        .model_map
        .insert("alias".into(), MODEL.into());
    cfg.members[0].lines.push(line("two", up.base("/v1")));
    cfg.openai
        .as_mut()
        .unwrap()
        .members
        .get_mut("provider")
        .unwrap()
        .chat
        .as_mut()
        .unwrap()
        .lines
        .insert("two".into(), up.base("/inference"));
    let real = handler(Protocol::Chat, true, false);
    up.set(Arc::new(move |seen| {
        if seen.method == "GET" {
            Fake::json(200, json!({"data":[{"id":"alias"}]}))
        } else {
            real(seen)
        }
    }));
    let r = start(cfg).await;
    assert!(probe(&r, "provider/alias").await.passed);
    assert_eq!(up.seen().iter().filter(|s| s.method == "POST").count(), 2);
    assert_eq!(
        r.post(None, r#"{"model":"provider/alias","input":"hello"}"#)
            .await
            .status(),
        200
    );
    r.handle.shutdown();
}
