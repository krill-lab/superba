//! Small explicit capability probes, cached per generation/member/model.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use superba_core::RouterModelTest;

use super::{cache, probe_key, snapshot};
use crate::state::{Generation, Shared, lock, unix_ms};

/// Test a model through the router: each of its providers on its own (a
/// pinned alias: that provider only), with an explicit summary. Standard-list
/// targets run the two-turn tool probe whose pass publishes them; native
/// targets answer one request.
pub(crate) async fn test(shared: &Arc<Shared>, model: &str) -> RouterModelTest {
    let g = shared.current();
    let snap = snapshot(&g, "client_version=0.163.0").await;
    let failure = |detail: &str| RouterModelTest {
        model: model.into(),
        passed: false,
        detail: detail.into(),
        tested_at: unix_ms(),
    };
    let (Some(row), Some(targets)) = (snap.all.get(model), snap.targets.get(model)) else {
        return failure("model is absent from the current catalog");
    };
    if targets.is_empty() {
        return failure("no provider listed this model");
    }
    let runs = targets.iter().map(|target| {
        let name = row
            .sources
            .iter()
            .find(|s| {
                g.members
                    .get(target.member)
                    .is_some_and(|m| m.cfg.id == s.provider)
            })
            .map_or_else(
                || g.members[target.member].cfg.name.clone(),
                |s| s.provider_name.clone(),
            );
        let g = g.clone();
        async move { (name, test_target(shared, &g, target).await) }
    });
    let reports = futures_util::future::join_all(runs).await;
    // Publish passing evidence before another client refreshes /models.
    snapshot(&g, "client_version=0.163.0").await;
    let detail = if reports.len() == 1 {
        reports[0].1.detail.clone()
    } else {
        reports
            .iter()
            .map(|(name, r)| format!("{name}: {}", r.detail))
            .collect::<Vec<_>>()
            .join("; ")
    };
    RouterModelTest {
        model: model.into(),
        passed: reports.iter().all(|(_, r)| r.passed),
        detail,
        tested_at: unix_ms(),
    }
}

/// One provider's test. Results are kept per member and upstream id, never
/// per alias, so the bare id and its alias share one piece of evidence.
async fn test_target(
    shared: &Arc<Shared>,
    g: &Arc<Generation>,
    target: &super::Target,
) -> RouterModelTest {
    let model = target.upstream.as_str();
    let member = target.member;
    let failure = |detail: &str| RouterModelTest {
        model: model.into(),
        passed: false,
        detail: detail.into(),
        tested_at: unix_ms(),
    };
    if target.standard && cache::responses_protocol(g, member).is_none() {
        return failure("no enabled protocol route for Responses; enable protocol conversion");
    }
    let key = probe_key(member, model);
    let slot = {
        lock(&g.merged_catalog.probes)
            .entry(key)
            .or_default()
            .clone()
    };
    let mut result = slot.lock().await;
    if target.standard
        && let Some(previous) = result.as_ref()
        && unix_ms().saturating_sub(previous.tested_at)
            < if previous.passed { 86_400_000 } else { 60_000 }
    {
        return previous.clone();
    }
    let attempted = if target.standard {
        two_turns(shared, g, member, model).await
    } else {
        // A native target answers one request on its own lines, through the
        // same proxy path (conversion and response handling included).
        let request = json!({"model": model, "input": [{"role":"user","content":"Reply with OK."}],
            "instructions":"Reply briefly.", "stream":true, "store":false});
        let mut outcome = Err("no provider line available".to_string());
        for line in 0..g.members[member].lines.len() {
            outcome = routed(shared, g, member, line, model, false, &request)
                .await
                .map(|_| ());
            if outcome.is_ok() {
                break;
            }
        }
        outcome
    };
    let report = RouterModelTest {
        model: model.into(),
        passed: attempted.is_ok(),
        detail: match attempted {
            Ok(()) if target.standard => "Two-turn function call passed".into(),
            Ok(()) => "Response completed through the local router".into(),
            Err(e) => e,
        },
        tested_at: unix_ms(),
    };
    *result = Some(report.clone());
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
        let first = routed(shared, g, member, line, model, true, &request).await;
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
        match routed(shared, g, member, line, model, true, &second).await {
            Ok(answer) if output_text(&answer).contains(&token) => return Ok(()),
            Ok(_) => error = "probe response did not contain the tool's result".into(),
            Err(e) => error = e,
        }
    }
    Err(error)
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
    standard: bool,
    request: &Value,
) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let response = crate::proxy::probe_http(
            shared.clone(),
            g.clone(),
            member,
            line,
            model,
            standard,
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
