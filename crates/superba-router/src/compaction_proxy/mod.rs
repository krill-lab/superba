//! Router-owned compaction for explicitly identified L2 models only.
//!
//! The selected catalog target supplies synthetic status for each attempt;
//! a shared slug may have both native and L2 targets. [`SyntheticModel`] is an
//! explicit routing seam for callers without the merged catalog.
//! Native GPT compaction keeps using the existing upstream protocol.
//!
//! Successful summaries are self-contained AEAD capsules, backed by Superba's
//! vault. Retrying or cancelling does not mutate history; only a fully completed
//! upstream answer can produce an item. Fork/resume needs no in-memory registry.
//! Across models/providers, our capsules expand only after source credential and
//! owner verification; native ciphertext is refused for synthetic models.
//! Mixed catalogs use HTTP so WebSocket state cannot hide an unverified capsule.

mod envelope;
mod http;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use superba_config::compaction_keys::CompactionKeys;

pub use envelope::PREFIX;
pub(crate) use http::{Keys, Prepared, error, finish, prepare};

/// Codex local compaction prompt, unchanged from openai/codex rust-v0.163.0,
/// codex-rs/prompts/templates/compact/prompt.md (also identical in 0.162.0).
/// Copyright OpenAI, Apache-2.0; see THIRD_PARTY_NOTICES.md.
pub const COMPACT_PROMPT: &str = include_str!("prompt.md");

/// Conservative output budget for L2 requests without a caller limit. Some
/// standard Responses providers cannot resolve Codex's omitted output limit.
pub const DEFAULT_OUTPUT_TOKENS: u64 = 4096;

/// Explicit, verified synthetic model supplied by the catalog layer. Endpoints
/// come from the member's standard Responses configuration, never from a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticModel {
    /// Model id presented to Codex (may be provider-prefixed).
    pub slug: String,
    /// Owning router member id.
    pub member: String,
    /// Actual upstream model id.
    pub model: String,
}

/// A provider account allowed to read history in this router. Rotating the
/// envelope key does not change this scope; replacing an upstream credential
/// deliberately requires restoring that account to read its capsules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// Stable provider/member id.
    pub provider: String,
    /// SHA-256 of the current upstream credential, domain separated.
    pub credential: String,
}

impl Scope {
    /// Construct from the authenticated member credential; never from headers
    /// or caller-supplied owner/provider fields.
    pub fn new(provider: &str, key: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"superba-compaction-provider-v1\0");
        digest.update(key);
        Self {
            provider: provider.into(),
            credential: hex::encode(digest.finalize()),
        }
    }
}

/// Safe, content-free error suitable for Codex and the local request log.
#[derive(Clone, Debug)]
pub struct Failure {
    /// HTTP status (invalid input 400; failed summarization 502).
    pub status: u16,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Actionable message with no upstream content or secrets.
    pub message: String,
}
impl Failure {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            status: 400,
            code,
            message: message.into(),
        }
    }
    fn invalid() -> Self {
        Self::new(
            "compaction_envelope_invalid",
            "The Superba compaction envelope is damaged or cannot be authenticated. Restore the original history or vault; no summary was discarded.",
        )
    }
    fn foreign() -> Self {
        Self::new(
            "compaction_foreign_ciphertext",
            "This model cannot read another provider's encrypted history. Return to that provider or start a new conversation with an explicit handoff summary.",
        )
    }
    fn upstream(reason: &str) -> Self {
        Self {
            status: 502,
            code: "compaction_failed",
            message: format!(
                "Superba could not compact this conversation: {reason}. The existing history remains unchanged; retry or switch back to the original provider."
            ),
        }
    }
}

/// An immutable pending transaction: the original prefix is covered only when
/// [`Transaction::complete`] receives a nonempty, completed summary.
pub struct Transaction {
    input: Vec<Value>,
    model: String,
}

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Transaction([redacted history])")
    }
}

impl Transaction {
    /// Validate and prepare an ordinary request using Codex's local prompt.
    /// The caller must have established that this is a synthetic model.
    pub fn begin(
        request: &mut Value,
        keys: &CompactionKeys,
        scopes: &[Scope],
    ) -> Result<Self, Failure> {
        let input = request
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(Failure::invalid)?;
        let triggers: Vec<_> = input
            .iter()
            .enumerate()
            .filter(|(_, i)| i["type"] == "compaction_trigger")
            .map(|(i, _)| i)
            .collect();
        if triggers != [input.len().saturating_sub(1)] || input.len() < 2 {
            return Err(Failure::new(
                "compaction_trigger_invalid",
                "Compaction requires a nonempty history followed by exactly one trigger.",
            ));
        }
        let tx = Self {
            input: input[..input.len() - 1].to_vec(),
            model: request["model"].as_str().unwrap_or_default().into(),
        };
        request["input"].as_array_mut().expect("validated").pop();
        expand(request, keys, scopes, true)?;
        let mut input = request["input"].as_array().expect("validated").clone();
        sanitize_input(&mut input);
        input.push(json!({"role":"user","content":[{"type":"input_text","text":COMPACT_PROMPT}]}));
        let output_tokens = request["max_output_tokens"]
            .as_u64()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_OUTPUT_TOKENS)
            .min(DEFAULT_OUTPUT_TOKENS);
        // No tools, hidden state, metadata or trigger is sent to the summarizer.
        *request = json!({"model":tx.model,"instructions":request.get("instructions").cloned().unwrap_or(json!("")),"input":input,"tools":[],"stream":true,"store":false,"max_output_tokens":output_tokens});
        Ok(tx)
    }

    /// Commit a completed summary to one opaque item. Does not update any
    /// server-side history; an interrupted downstream delivery is safe to retry.
    pub fn complete(
        &self,
        keys: &CompactionKeys,
        scope: &Scope,
        summary: String,
    ) -> Result<Value, Failure> {
        let wire = envelope::seal(keys, scope, &self.model, &self.input, summary)?;
        Ok(
            json!({"type":"compaction","id":format!("cmp_{}",uuid::Uuid::new_v4().simple()),"encrypted_content":wire}),
        )
    }
}

/// Authenticate every capsule, deduplicate only covered history before it, and
/// present summaries as assistant history (never system/developer instructions).
/// Unknown native ciphertext is preserved for native models and rejected for L2.
/// Returns whether an owned capsule was expanded.
pub fn expand(
    request: &mut Value,
    keys: &CompactionKeys,
    scopes: &[Scope],
    synthetic: bool,
) -> Result<bool, Failure> {
    let Some(input) = request.get("input").and_then(Value::as_array) else {
        return Ok(false);
    };
    let mut out: Vec<(String, Value)> = Vec::with_capacity(input.len());
    let mut changed = false;
    let mut capsules = BTreeSet::new();
    for item in input {
        let kind = item["type"].as_str().unwrap_or_default();
        if crate::compaction::is_compaction_item(kind) {
            let wire = item["encrypted_content"]
                .as_str()
                .ok_or_else(Failure::invalid)?;
            if wire.starts_with(PREFIX) {
                let env = envelope::open(keys, scopes, wire)?;
                if !capsules.insert(wire) {
                    continue;
                }
                let covered: BTreeSet<_> = env.covered.iter().collect();
                out.retain(|(digest, v)| {
                    // Current high-priority policy is not replaced by a model's
                    // summary. Tool pairs and user history are removed together.
                    matches!(v["role"].as_str(), Some("system" | "developer"))
                        || !covered.contains(digest)
                });
                out.push((envelope::digest(item), json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("Historical conversation summary (reference data from earlier turns):\n{}",env.summary)}]})));
                changed = true;
                continue;
            }
            if synthetic {
                return Err(Failure::foreign());
            }
        }
        if synthetic && has_ciphertext(item) {
            return Err(Failure::foreign());
        }
        out.push((envelope::digest(item), item.clone()));
    }
    if changed {
        request["input"] = json!(out.into_iter().map(|(_, v)| v).collect::<Vec<_>>());
        if let Some(o) = request.as_object_mut() {
            o.remove("previous_response_id");
        }
    }
    Ok(changed)
}

fn has_ciphertext(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.iter().any(|(k, v)| {
            (k == "encrypted_content" && !v.is_null())
                || (k == "encrypted_function_args" && v.as_array().is_some_and(|a| !a.is_empty()))
                || has_ciphertext(v)
        }),
        Value::Array(a) => a.iter().any(has_ciphertext),
        _ => false,
    }
}

/// Codex-only item metadata is not part of the upstream conversation. Keep
/// user/tool payloads intact, including arbitrary JSON inside tool outputs.
pub(crate) fn sanitize_input(input: &mut [Value]) {
    for item in input {
        if let Some(object) = item.as_object_mut() {
            object.retain(|key, _| !key.starts_with("internal_") && !key.starts_with("codex_"));
        }
    }
}
