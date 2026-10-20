//! Small explicit capability probes, cached per generation/member/model.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use superba_core::{CatalogLayer, RouterModelTest};

use super::{cache, probe_key, snapshot};
use crate::state::{Generation, Shared, lock, unix_ms};

pub(crate) async fn test(shared: &Arc<Shared>, model: &str) -> RouterModelTest {
    let g = shared.current();
    let snap = snapshot(&g, "client_version=0.163.0").await;
    let failure = |detail: &str| RouterModelTest {
        model: model.into(),
        passed: false,
        detail: detail.into(),
        tested_at: unix_ms(),
    };
    let Some(row) = snap
        .view
        .models
        .iter()
        .find(|m| m.entry["slug"].as_str() == Some(model))
    else {
        return failure("model is absent from the current catalog");
    };
    let Some(proof) = row
        .sources
        .iter()
        .find(|s| s.layer == CatalogLayer::Synthesized)
        .or_else(|| row.sources.iter().find(|s| s.provider != "builtin"))
    else {
        return failure("no provider listed this model");
    };
    let Some(member) = g.members.iter().position(|m| m.cfg.id == proof.provider) else {
        return failure("provider is no longer configured");
    };
    let standard = proof.layer == CatalogLayer::Synthesized;
    if standard && cache::responses_protocol(&g, member).is_none() {
        return failure("no enabled protocol route for Responses; enable protocol conversion");
    }
    let key = probe_key(member, &proof.upstream_id);
    let slot = {
        lock(&g.merged_catalog.probes)
            .entry(key)
            .or_default()
            .clone()
    };
    let mut result = slot.lock().await;
    if standard
        && let Some(previous) = result.as_ref()
        && unix_ms().saturating_sub(previous.tested_at)
            < if previous.passed { 86_400_000 } else { 60_000 }
    {
        let mut previous = previous.clone();
        previous.model = model.into();
        return previous;
    }
    let attempted = if standard {
        two_turns(shared, &g, member, &proof.upstream_id).await
    } else {
        // Native models test the same public ingress used by Codex, including
        // namespace selection, per-line failover and request recording.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build();
        match client {
            Ok(client) => {
                let req = client.post(format!("http://127.0.0.1:{}/backend-api/codex/responses", shared.listen.port()))
                    .bearer_auth(g.router_token.expose()).json(&json!({"model": model, "input": [{"role":"user","content":"Reply with OK."}],
                        "instructions":"Reply briefly.", "stream":true,"store":false}));
                send(req).await.map(|_| ())
            }
            Err(_) => Err("could not initialize the local test client".into()),
        }
    };
    let report = RouterModelTest {
        model: model.into(),
        passed: attempted.is_ok(),
        detail: match attempted {
            Ok(()) if standard => "Two-turn function call passed".into(),
            Ok(()) => "Response completed through the local router".into(),
            Err(e) => e,
        },
        tested_at: unix_ms(),
    };
    *result = Some(report.clone());
    drop(result);
    // Publish the successful evidence before another client refreshes /models.
    snapshot(&g, "client_version=0.163.0").await;
    report
}

async fn two_turns(
    shared: &Arc<Shared>,
    g: &Arc<Generation>,
    member: usize,
    model: &str,
) -> Result<(), String> {
    let m = &g.members[member];
    let mut error = "no provider line available".to_string();
    for line in 0..m.lines.len() {
        let tool = json!({"type":"function","name":"get_probe_value","description":"Read the probe value.",
            "parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false},"strict":true});
        let input = json!([{"role":"user","content":"Call get_probe_value, then return only its exact result."}]);
        let request = json!({"model":model,"input":input,"tools":[tool],"tool_choice":{"type":"function","name":"get_probe_value"},
            "max_output_tokens":512,"stream":true,"store":false});
        let first = routed(shared, g, member, line, model, &request).await;
        let first = match first {
            Ok(v) => v,
            Err(e) => {
                error = e;
                continue;
            }
        };
        let Some(output) = first["output"].as_array() else {
            error = "probe returned no output".into();
            continue;
        };
        let Some(call) = output
            .iter()
            .find(|v| v["type"] == "function_call" && v["name"] == "get_probe_value")
        else {
            error = "probe did not call the requested tool".into();
            continue;
        };
        let Some(call_id) = call["call_id"].as_str().filter(|s| !s.is_empty()) else {
            error = "probe tool call has no call_id".into();
            continue;
        };
        if !call["arguments"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .is_some_and(|v| v.is_object())
        {
            error = "probe tool arguments are not valid JSON".into();
            continue;
        }
        let token = format!("SUPERBA_PROBE_{}", uuid::Uuid::new_v4().simple());
        let mut conversation = input.as_array().expect("input array").clone();
        conversation.extend(output.iter().cloned());
        conversation.push(json!({"type":"function_call_output","call_id":call_id,"output":token}));
        let second = json!({"model":model,"input":conversation,"tools":[tool],"tool_choice":"auto","max_output_tokens":512,"stream":true,"store":false});
        match routed(shared, g, member, line, model, &second).await {
            Ok(answer) if output_text(&answer).contains(&token) => return Ok(()),
            Ok(_) => error = "probe response did not contain the tool's result".into(),
            Err(e) => error = e,
        }
    }
    Err(error)
}

async fn send(request: reqwest::RequestBuilder) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let response = request
            .send()
            .await
            .map_err(|_| "probe connection failed".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "probe returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let bytes = cache::read_bounded(response).await?;
        completed(&bytes)
    })
    .await
    .map_err(|_| "probe timed out".to_string())?
}

fn output_text(value: &Value) -> String {
    value["output"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|v| v["content"].as_array().into_iter().flatten())
        .filter_map(|v| v["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

async fn routed(
    shared: &Arc<Shared>,
    g: &Arc<Generation>,
    member: usize,
    line: usize,
    model: &str,
    request: &Value,
) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let response = crate::proxy::probe_http(
            shared.clone(),
            g.clone(),
            member,
            line,
            model,
            Bytes::from(request.to_string()),
        )
        .await;
        if !response.status().is_success() {
            return Err(format!(
                "probe returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| "probe stream failed".to_string())?;
            if let Ok(data) = frame.into_data() {
                if bytes.len().saturating_add(data.len()) > cache::MAX_BYTES {
                    return Err("probe exceeded its size limit".into());
                }
                bytes.extend_from_slice(&data);
            }
        }
        completed(&bytes)
    })
    .await
    .map_err(|_| "probe timed out".to_string())?
}

fn completed(bytes: &[u8]) -> Result<Value, String> {
    let parsed = serde_json::from_slice::<Value>(bytes)
        .ok()
        .or_else(|| {
            String::from_utf8_lossy(bytes)
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|s| serde_json::from_str::<Value>(s.trim()).ok())
                .find(|v| v["type"] == "response.completed")
                .map(|v| v["response"].clone())
        })
        .ok_or_else(|| "probe returned no completed Responses object".to_string())?;
    if parsed["status"] != "completed" || !parsed["output"].is_array() {
        return Err("probe did not complete".into());
    }
    Ok(parsed)
}
