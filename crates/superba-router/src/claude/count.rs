//! `POST /claude/v1/messages/count_tokens`: the upstream's own count when the
//! model is passed through to an Anthropic endpoint that counts, else a local
//! estimate ([`super::estimate`]). A member whose endpoint has no
//! `count_tokens` (404 / 405 / 501) is estimated for a day. Not recorded in
//! the request log (no model request).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::Response;
use http::StatusCode;
use http::request::Parts;
use serde_json::{Value, json};

use super::error::json_response;
use super::estimate;
use super::{line, passthrough, route};
use crate::config::join_path;
use crate::state::{RespBody, Shared, lock};

/// How long a missing `count_tokens` is remembered.
const NO_COUNT_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// One count request may take this long.
const COUNT_TIMEOUT: Duration = Duration::from_secs(20);

fn estimated(req: &Value) -> Response<RespBody> {
    json_response(
        StatusCode::OK,
        &json!({"input_tokens": estimate::input_tokens(req)}),
    )
}

/// Answer a count request.
pub(super) async fn count(shared: &Arc<Shared>, parts: &Parts, raw: Bytes) -> Response<RespBody> {
    let generation = shared.current();
    let limit = generation.config.tuning.max_body_bytes;
    let mut req = match super::parse_body(&parts.headers, &raw, limit) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    line::strip_request(&mut req);
    let Some(requested) = req.get("model").and_then(Value::as_str).map(str::to_owned) else {
        return estimated(&req);
    };
    let Ok(resolution) = route::resolve(
        shared,
        &generation,
        &requested,
        crate::forward::bound_member(&parts.headers),
    )
    .await
    else {
        return estimated(&req);
    };
    let target = if generation.config.tuning.failover_enabled {
        resolution.targets.first()
    } else {
        resolution.targets.iter().find(|t| {
            generation
                .fixed_exit()
                .is_some_and(|c| c.member == t.member)
        })
    };
    let Some(target) = target else {
        return estimated(&req);
    };
    let member = &generation.members[target.member];
    let Some(mc) = generation
        .claude
        .as_ref()
        .and_then(|c| c.member(target.member))
    else {
        return estimated(&req);
    };
    let id = member.cfg.id.clone();
    let remembered = lock(&shared.claude.no_count)
        .get(&id)
        .is_some_and(|until| Instant::now() < *until);
    if mc.protocol != crate::translate::ir::Protocol::Messages
        || mc.cfg.count_tokens == Some(false)
        || remembered
    {
        return estimated(&req);
    }
    let (body, _) = passthrough::body(&req, mc, &target.model);
    let body = body.unwrap_or_else(|| Bytes::from(serde_json::to_vec(&req).unwrap_or_default()));
    for (li, line) in member.lines.iter().enumerate() {
        if !generation.config.tuning.failover_enabled {
            if generation.fixed_exit().is_none_or(|c| c.line != li) {
                continue;
            }
        } else if !lock(&line.health).is_available(Instant::now()) {
            continue;
        }
        let base = &mc.base[li];
        let url = join_path(base, "/v1/messages/count_tokens", parts.uri.query());
        let sent = generation
            .http
            .post(url)
            .headers(passthrough::headers(&parts.headers, mc))
            .timeout(COUNT_TIMEOUT)
            .body(body.clone())
            .send()
            .await;
        let Ok(resp) = sent else {
            if !generation.config.tuning.failover_enabled {
                return estimated(&req);
            }
            continue;
        };
        let status = resp.status();
        if status.is_success() {
            if let Ok(bytes) = resp.bytes().await
                && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
                && v.get("input_tokens").is_some()
            {
                return json_response(StatusCode::OK, &v);
            }
            return estimated(&req);
        }
        if matches!(status.as_u16(), 404 | 405 | 501) {
            lock(&shared.claude.no_count).insert(id, Instant::now() + NO_COUNT_FOR);
        }
        return estimated(&req);
    }
    estimated(&req)
}
