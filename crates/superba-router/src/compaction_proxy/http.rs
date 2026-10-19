//! HTTP adapter. Summary bytes stay private until a terminal success is read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::{Response, StatusCode, request::Parts};
use http_body_util::{BodyExt, Limited};
use serde_json::{Value, json};
use superba_config::compaction_keys::{CompactionKeyStore, CompactionKeys};
use superba_core::{DataDir, LocalContext, ProcessEnv};

use super::{Failure, PREFIX, Scope, Transaction, expand};
use crate::forward::{UpBody, UpResponse, router_error};
use crate::state::{Generation, RespBody, Shared};

/// Lifecycle of the vault-backed keyring; no real HOME is consulted.
#[derive(Debug)]
pub(crate) struct Keys {
    store: Option<CompactionKeyStore>,
    memory: CompactionKeys,
    loaded: AtomicBool,
}

impl Keys {
    pub fn new(paths: Option<&crate::RouterPaths>) -> Self {
        let store = paths.and_then(|p| p.dir().parent()).map(|root| {
            CompactionKeyStore::new(LocalContext::new(
                root,
                DataDir::new(root),
                Arc::new(ProcessEnv),
            ))
        });
        Self {
            store,
            memory: CompactionKeys::ephemeral(),
            loaded: AtomicBool::new(false),
        }
    }
    pub fn used(&self) -> bool {
        self.loaded.load(Ordering::Relaxed)
            || self.store.as_ref().is_some_and(CompactionKeyStore::exists)
    }
    async fn load(&self) -> Result<CompactionKeys, Failure> {
        self.loaded.store(true, Ordering::Relaxed);
        let Some(store) = self.store.clone() else {
            return Ok(self.memory.clone());
        };
        tokio::task::spawn_blocking(move || store.load()).await
            .map_err(|_| Failure::upstream("compaction vault task failed"))?
            .map_err(|_| Failure::new("compaction_vault_unavailable", "The Superba compaction vault is unavailable. Restore the vault before continuing."))
    }
}

pub(crate) struct Prepared {
    pub body: Bytes,
    pub transaction: Option<Transaction>,
    pub keys: Option<CompactionKeys>,
    pub stream: bool,
}

impl Prepared {
    pub fn unchanged(body: Bytes) -> Self {
        Self {
            body,
            transaction: None,
            keys: None,
            stream: true,
        }
    }
}

pub(crate) fn scopes(g: &Generation) -> Vec<Scope> {
    g.members
        .iter()
        .map(|m| Scope::new(&m.cfg.id, m.auth.as_bytes()))
        .collect()
}

pub(crate) fn error(f: Failure) -> Response<RespBody> {
    router_error(
        StatusCode::from_u16(f.status).unwrap_or(StatusCode::BAD_REQUEST),
        f.code,
        &f.message,
    )
}

pub(crate) async fn prepare(
    shared: &Shared,
    g: &Generation,
    parts: &mut Parts,
    body: Bytes,
    synthetic: bool,
    limit: usize,
) -> Result<Prepared, Box<Response<RespBody>>> {
    // Decode before deciding: compressed capsules must not bypass verification.
    let mut headers = parts.headers.clone();
    let plain = match crate::openai::body::decode(&mut headers, body.clone(), limit) {
        Ok(plain) => plain,
        Err(_) if !synthetic && !shared.compaction_proxy.used() => {
            return Ok(Prepared::unchanged(body));
        }
        Err(e) => return Err(e),
    };
    let mut req: Value = match serde_json::from_slice(&plain) {
        Ok(v) => v,
        Err(_) => return Ok(Prepared::unchanged(body)),
    };
    let owned = req["input"].as_array().is_some_and(|a| {
        a.iter().any(|i| {
            i["encrypted_content"]
                .as_str()
                .is_some_and(|s| s.starts_with(PREFIX))
        })
    });
    if !synthetic && !owned {
        return Ok(Prepared::unchanged(body));
    }
    let keys = shared
        .compaction_proxy
        .load()
        .await
        .map_err(|e| Box::new(error(e)))?;
    let stream = req["stream"].as_bool().unwrap_or(false);
    if synthetic
        && parts
            .uri
            .path()
            .trim_end_matches('/')
            .ends_with("/responses/compact")
        && let Some(input) = req["input"].as_array_mut()
        && !input.iter().any(|i| i["type"] == "compaction_trigger")
    {
        input.push(json!({"type":"compaction_trigger"}));
    }
    let trigger = req["input"]
        .as_array()
        .is_some_and(|a| a.iter().any(|i| i["type"] == "compaction_trigger"));
    let transaction = if trigger && synthetic {
        Some(Transaction::begin(&mut req, &keys, &scopes(g)).map_err(|e| Box::new(error(e)))?)
    } else {
        expand(&mut req, &keys, &scopes(g), synthetic).map_err(|e| Box::new(error(e)))?;
        None
    };
    if synthetic {
        if req["max_output_tokens"].is_null() {
            req["max_output_tokens"] = json!(super::DEFAULT_OUTPUT_TOKENS);
        }
        if let Some(input) = req["input"].as_array_mut() {
            super::sanitize_input(input);
        }
        crate::openai::shape::drop_private(&mut req);
        req["store"] = json!(false);
        if let Some(obj) = req.as_object_mut() {
            obj.remove("previous_response_id");
        }
    }
    headers.remove(crate::forward::TURN_STATE_HEADER);
    headers.remove(http::header::ACCEPT_ENCODING);
    parts.headers = headers;
    Ok(Prepared {
        body: Bytes::from(serde_json::to_vec(&req).expect("JSON value")),
        transaction,
        keys: Some(keys),
        stream,
    })
}

pub(crate) async fn finish(
    prepared: &Prepared,
    g: &Generation,
    member: usize,
    response: UpResponse,
) -> Result<UpResponse, Failure> {
    let Some(tx) = &prepared.transaction else {
        return Ok(response);
    };
    if !response.status().is_success() {
        return Ok(response);
    }
    let (mut parts, body) = response.into_parts();
    let collected = tokio::time::timeout(
        Duration::from_secs(300),
        Limited::new(body, 2 * 1024 * 1024).collect(),
    )
    .await
    .map_err(|_| Failure::upstream("summary generation timed out"))?
    .map_err(|_| Failure::upstream("summary stream broke or exceeded its size limit"))?;
    let bytes = collected.to_bytes();
    let (summary, usage) = summary(
        &bytes,
        parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
    )?;
    let scope = Scope::new(&g.members[member].cfg.id, g.members[member].auth.as_bytes());
    let item = tx.complete(
        prepared.keys.as_ref().expect("transaction keys"),
        &scope,
        summary,
    )?;
    let id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let completed = json!({"id":id,"object":"response","status":"completed","model":tx.model,"output":[item],"usage":usage});
    let body = if prepared.stream {
        let events = [
            json!({"type":"response.created","response":{"id":id,"object":"response","status":"in_progress","output":[]}}),
            json!({"type":"response.output_item.added","output_index":0,"item":item}),
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.completed","response":completed}),
        ];
        events
            .into_iter()
            .enumerate()
            .map(|(n, mut e)| {
                e["sequence_number"] = json!(n);
                format!(
                    "event: {}\ndata: {e}\n\n",
                    e["type"].as_str().expect("event type")
                )
            })
            .collect::<String>()
    } else {
        completed.to_string()
    };
    parts.headers.remove(http::header::CONTENT_ENCODING);
    parts.headers.remove(http::header::CONTENT_LENGTH);
    parts.headers.remove(crate::forward::TURN_STATE_HEADER);
    parts.headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static(if prepared.stream {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );
    Ok(Response::from_parts(
        parts,
        UpBody::replay(Bytes::from(body)),
    ))
}

fn summary(bytes: &[u8], content_type: &str) -> Result<(String, Value), Failure> {
    let mut done = vec![];
    let terminal = if content_type.contains("text/event-stream") {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Failure::upstream("summary stream was not UTF-8"))?
            .replace("\r\n", "\n");
        let mut completed = None;
        for frame in text.split("\n\n") {
            let data = frame
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let event: Value = serde_json::from_str(&data)
                .map_err(|_| Failure::upstream("summary stream contained invalid JSON"))?;
            match event["type"].as_str() {
                Some("response.failed" | "response.incomplete" | "error") => {
                    return Err(Failure::upstream(
                        "the upstream reported a failed or incomplete summary",
                    ));
                }
                Some("response.output_item.done") => done.push(event["item"].clone()),
                Some("response.completed") => {
                    if completed.is_some() {
                        return Err(Failure::upstream("duplicate summary completion"));
                    }
                    completed = Some(event["response"].clone());
                }
                _ => {}
            }
        }
        completed
            .ok_or_else(|| Failure::upstream("summary stream ended before response.completed"))?
    } else {
        serde_json::from_slice(bytes)
            .map_err(|_| Failure::upstream("the upstream did not return a Responses answer"))?
    };
    if terminal["status"] != "completed" || !terminal["error"].is_null() {
        return Err(Failure::upstream("the summary did not complete"));
    }
    let output = terminal
        .get("output")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .unwrap_or(&done);
    let mut text = String::new();
    for item in output {
        match item["type"].as_str() {
            Some("message") if item["role"] == "assistant" => {
                for part in item["content"].as_array().into_iter().flatten() {
                    if part["type"] != "output_text" {
                        return Err(Failure::upstream(
                            "the model refused or returned a non-text summary",
                        ));
                    }
                    text.push_str(part["text"].as_str().unwrap_or_default());
                }
            }
            Some("reasoning") => {}
            _ => {
                return Err(Failure::upstream(
                    "the model returned a tool call instead of a summary",
                ));
            }
        }
    }
    if text.trim().is_empty() {
        return Err(Failure::upstream("the model returned an empty summary"));
    }
    Ok((text, terminal["usage"].clone()))
}
