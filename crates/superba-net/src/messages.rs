//! Claude Code's endpoint check ([`superba_core::LineProber::probe_messages`]):
//! one `POST <endpoint>/v1/messages` with `max_tokens: 1`, the key sent the
//! way Claude Code sends it (`Authorization: Bearer` for
//! `ANTHROPIC_AUTH_TOKEN`, `x-api-key` for `ANTHROPIC_API_KEY`) plus
//! `anthropic-version`.

use std::time::Instant;

use serde_json::json;
use superba_core::claude::ClaudeProbe;
use superba_core::{AnthropicAuth, MessagesProbeRequest, Result};

use crate::HttpLineProber;
use crate::classify::error_message;
use crate::http::{Request, display_url, ensure_key_target, redact};

/// Answer bytes kept.
const LIMIT: usize = 64 * 1024;

impl HttpLineProber {
    /// See [`superba_core::LineProber::probe_messages`].
    pub(crate) async fn probe_messages_impl(
        &self,
        req: MessagesProbeRequest<'_>,
    ) -> Result<ClaudeProbe> {
        Self::ensure_key(req.key)?;
        let url = req.endpoint.join("v1/messages");
        ensure_key_target(&url, req.key)?;
        let body = json!({
            "model": req.model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "Reply with OK."}],
        });
        let mut request = Request::get(url.clone(), self.config.request_timeout, LIMIT)
            .with_json(body.to_string().into_bytes(), "application/json")
            .with_key(req.key);
        request = match req.endpoint.auth {
            AnthropicAuth::Bearer => request.anthropic_bearer(),
            AnthropicAuth::ApiKey => request.anthropic_api_key(),
        };
        let started = Instant::now();
        let answer = self.http.send(request).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let shown = display_url(&url);
        let a = match answer {
            Ok(a) => a,
            Err(t) => {
                return Ok(ClaudeProbe {
                    model: req.model.to_owned(),
                    url: shown,
                    status: None,
                    latency_ms,
                    ok: false,
                    message: Some(redact(&t.into_error(&url).to_string(), Some(req.key))),
                });
            }
        };
        let parsed: Option<serde_json::Value> = serde_json::from_slice(&a.body).ok();
        let is_message = parsed.as_ref().is_some_and(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("message")
                || v.get("content").is_some_and(serde_json::Value::is_array)
        });
        let ok = (200..300).contains(&a.status) && is_message;
        let message = if ok {
            None
        } else {
            Some(
                error_message(&a.body)
                    .map(|m| redact(&m, Some(req.key)))
                    .unwrap_or_else(|| format!("HTTP {}", a.status)),
            )
        };
        Ok(ClaudeProbe {
            model: req.model.to_owned(),
            url: shown,
            status: Some(a.status),
            latency_ms,
            ok,
            message,
        })
    }
}
