//! Pure classification of HTTP answers: edge interception, relay answers, key
//! access, whoami and model catalogs. No IO here, so every rule is unit tested.
//!
//! Observed behaviour this module encodes (Krill relay and its edge, 2026-10):
//! - Unauthenticated requests to relay endpoints answer `401` with a JSON body
//!   `{"type":"error","error":{"type":"unauthorized_error","message":"missing Authorization"}}`.
//! - The edge of a line can intercept a path and answer `200` with an empty body
//!   and an `l-block` header instead of forwarding the request.
//! - The authenticated models request runs the relay's access checks in order:
//!   key (401), access point / line allowed by the plan (403), plan quota or
//!   balance (403 / 429), Codex channel authorization (403), each with a message,
//!   and only then the per-minute request limit (429 "rate limit exceeded"). A
//!   per-minute 429 therefore proves the key passed every entitlement check.

use serde_json::Value;
use superba_core::{EndpointCheck, KeyAccess, ProbeEndpoint, SiteKind};

use crate::http::{Answer, Transport, TransportKind};

/// Longest error excerpt kept from a response body (characters).
const MAX_MESSAGE_CHARS: usize = 240;

/// True if the body is valid JSON.
pub(crate) fn is_json(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok()
}

/// True if the body is empty or only whitespace.
pub(crate) fn is_blank(body: &[u8]) -> bool {
    body.iter().all(u8::is_ascii_whitespace)
}

/// True if the answer body is JSON. A body cut at the read limit cannot be parsed,
/// so it counts as JSON when it starts like a JSON object or array.
pub(crate) fn json_like(a: &Answer) -> bool {
    if a.truncated {
        matches!(
            a.body.iter().find(|b| !b.is_ascii_whitespace()),
            Some(b'{' | b'[')
        )
    } else {
        is_json(&a.body)
    }
}

/// Edge interception of a JSON endpoint: the `l-block` header, or a 2xx answer
/// whose body is empty or not JSON (the relay always answers these paths with
/// JSON; a 2xx without it comes from something in front of the relay).
pub(crate) fn edge_intercepted(a: &Answer) -> bool {
    a.edge_header.is_some() || (a.is_success() && !json_like(a))
}

/// Edge interception of a streamed endpoint (Responses SSE is not JSON, so only
/// the header and an empty 2xx body count).
pub(crate) fn edge_intercepted_stream(a: &Answer) -> bool {
    a.edge_header.is_some() || (a.is_success() && is_blank(&a.body))
}

/// Human description of an interception (never includes secrets).
pub(crate) fn edge_detail(a: &Answer) -> String {
    match &a.edge_header {
        Some(_) => format!(
            "intercepted by the line's edge (HTTP {} with an l-block header)",
            a.status
        ),
        None if is_blank(&a.body) => format!(
            "intercepted by the line's edge (HTTP {} with an empty body)",
            a.status
        ),
        None => format!(
            "intercepted by the line's edge (HTTP {} with a non-JSON body)",
            a.status
        ),
    }
}

/// Collapse whitespace and cut to [`MAX_MESSAGE_CHARS`] on a char boundary.
pub(crate) fn short(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_MESSAGE_CHARS {
        collapsed
    } else {
        let mut s: String = collapsed.chars().take(MAX_MESSAGE_CHARS).collect();
        s.push('…');
        s
    }
}

/// The error message of a JSON error body (`error.message`, `error` as a string,
/// `message`, `detail`), else a short plain-text excerpt. HTML pages are
/// summarised instead of quoted.
pub(crate) fn error_message(body: &[u8]) -> Option<String> {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let pick = |v: &Value| -> Option<String> {
            match v {
                Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
                _ => None,
            }
        };
        let candidates = [
            v.pointer("/error/message"),
            v.get("error"),
            v.get("message"),
            v.get("detail"),
            v.pointer("/detail/message"),
            v.pointer("/error/code"),
            v.pointer("/error/type"),
        ];
        return candidates
            .into_iter()
            .flatten()
            .find_map(pick)
            .map(|s| short(&s));
    }
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.starts_with("<html") || lower.starts_with('<') {
        return Some("HTML page".to_owned());
    }
    Some(short(trimmed))
}

/// `HTTP <status>: <message>` or just `HTTP <status>`.
pub(crate) fn status_detail(a: &Answer) -> String {
    match error_message(&a.body) {
        Some(m) => format!("HTTP {}: {m}", a.status),
        None => format!("HTTP {}", a.status),
    }
}

/// Description of a transport failure.
pub(crate) fn transport_detail(t: &Transport) -> String {
    match t.kind {
        TransportKind::Timeout => "no answer before the timeout".to_owned(),
        _ => short(&t.message),
    }
}

/// Classify an unauthenticated probe of a relay endpoint. `expect_relay` = the
/// site promises relay-style answers (status in `expected` with a JSON body);
/// otherwise any non-intercepted answer below 500 counts as healthy.
pub(crate) fn unauth_check(
    endpoint: ProbeEndpoint,
    result: &Result<Answer, Transport>,
    expected: &[u16],
    expect_relay: bool,
) -> EndpointCheck {
    match result {
        Err(t) => EndpointCheck {
            endpoint,
            status: None,
            latency_ms: None,
            ok: false,
            edge_blocked: false,
            detail: Some(transport_detail(t)),
        },
        Ok(a) => {
            let edge = if expect_relay {
                edge_intercepted(a)
            } else {
                a.edge_header.is_some()
            };
            let ok = !edge
                && if expect_relay {
                    expected.contains(&a.status) && json_like(a)
                } else {
                    a.status < 500 && !a.is_redirect()
                };
            let detail = if edge {
                Some(edge_detail(a))
            } else if a.is_redirect() {
                Some(format!("HTTP {} redirect (not followed)", a.status))
            } else if !ok {
                Some(status_detail(a))
            } else {
                None
            };
            EndpointCheck {
                endpoint,
                status: Some(a.status),
                latency_ms: Some(a.elapsed_ms()),
                ok,
                edge_blocked: edge,
                detail,
            }
        }
    }
}

/// Classify the authenticated models request of a key check.
pub(crate) fn key_access(kind: SiteKind, result: &Result<Answer, Transport>) -> KeyAccess {
    let a = match result {
        Err(_) => return KeyAccess::Unreachable,
        Ok(a) => a,
    };
    if edge_intercepted(a) {
        return KeyAccess::EdgeBlocked;
    }
    if a.is_success() {
        return KeyAccess::Ok;
    }
    let message = error_message(&a.body).unwrap_or_default();
    let unknown = || KeyAccess::Unknown {
        status: Some(a.status),
        message: if message.is_empty() {
            format!("HTTP {}", a.status)
        } else {
            message.clone()
        },
    };
    if a.is_redirect() {
        return KeyAccess::Unknown {
            status: Some(a.status),
            message: "redirect (not followed)".to_owned(),
        };
    }
    match a.status {
        401 => KeyAccess::InvalidKey,
        402 => KeyAccess::QuotaExhausted,
        400 | 403 | 429 => classify_denial(kind, &message.to_lowercase()).unwrap_or_else(unknown),
        _ => unknown(),
    }
}

/// Map a denial message to a [`KeyAccess`] reason. The relay's English messages
/// (and the Chinese line message) are matched by stable keywords; other sites
/// only get the generic OpenAI quota and key wording.
fn classify_denial(kind: SiteKind, msg: &str) -> Option<KeyAccess> {
    let has = |needles: &[&str]| needles.iter().any(|n| msg.contains(n));
    if has(&[
        "invalid token",
        "invalid api key",
        "incorrect api key",
        "invalid_api_key",
    ]) {
        return Some(KeyAccess::InvalidKey);
    }
    if kind == SiteKind::Krill {
        // Per-minute limit: enforced after every entitlement check, so the key is
        // usable on this line (the limit is transient and must not block a switch).
        if msg.contains("rate limit") && !has(&["quota", "balance", "额度", "余额"]) {
            return Some(KeyAccess::Ok);
        }
        if has(&["access point", "entry route", "当前套餐不包含线路"]) {
            return Some(KeyAccess::AccessPointDenied);
        }
        if has(&["codex channel"]) || (msg.contains("channel") && msg.contains("not authorized")) {
            return Some(KeyAccess::ChannelDenied);
        }
    }
    if has(&[
        "quota",
        "balance",
        "recharge",
        "insufficient",
        "no active plan",
        "redeem",
        "credit",
        "额度",
        "余额",
    ]) {
        return Some(KeyAccess::QuotaExhausted);
    }
    None
}

/// Fields of a whoami answer (`/v1/user-auth-credential/whoami`). Codex requires
/// the four `chatgpt_*` fields; the plan type is constant on Krill and must never
/// be shown as the user's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Whoami {
    /// Optional email.
    pub email: Option<String>,
    /// `chatgpt_user_id`.
    pub user_id: String,
    /// `chatgpt_account_id`.
    pub account_id: String,
}

/// Parse and validate a whoami body. `Err` names the first missing or mistyped
/// required field.
pub(crate) fn parse_whoami(body: &[u8]) -> Result<Whoami, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| "whoami answer is not JSON".to_owned())?;
    let obj = v
        .as_object()
        .ok_or_else(|| "whoami answer is not a JSON object".to_owned())?;
    let string = |name: &str| -> Result<String, String> {
        match obj.get(name) {
            Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
            Some(_) => Err(format!("whoami field {name} is not a non-empty string")),
            None => Err(format!("whoami answer is missing {name}")),
        }
    };
    let user_id = string("chatgpt_user_id")?;
    let account_id = string("chatgpt_account_id")?;
    string("chatgpt_plan_type")?;
    match obj.get("chatgpt_account_is_fedramp") {
        Some(Value::Bool(_)) => {}
        Some(_) => return Err("whoami field chatgpt_account_is_fedramp is not a boolean".into()),
        None => return Err("whoami answer is missing chatgpt_account_is_fedramp".into()),
    }
    let email = match obj.get("email") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    };
    Ok(Whoami {
        email,
        user_id,
        account_id,
    })
}

/// True if a `wham/accounts/check` body lists at least one account.
pub(crate) fn wham_lists_accounts(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("accounts")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn answer(status: u16, body: &str) -> Answer {
        Answer {
            status,
            edge_header: None,
            content_type: Some("application/json".into()),
            cors: false,
            location: None,
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::from_millis(42),
            retry_after: None,
            etag: None,
        }
    }

    const RELAY_401: &str = r#"{"type":"error","error":{"type":"unauthorized_error","message":"missing Authorization"}}"#;

    fn err403(msg: &str) -> Answer {
        answer(
            403,
            &serde_json::json!({"type":"error","error":{"type":"forbidden_error","message":msg}})
                .to_string(),
        )
    }

    #[test]
    fn relay_401_is_healthy_and_edge_empty_200_is_blocked() {
        let ok = unauth_check(
            ProbeEndpoint::Whoami,
            &Ok(answer(401, RELAY_401)),
            &[401],
            true,
        );
        assert!(ok.ok && !ok.edge_blocked && ok.detail.is_none());
        assert_eq!(ok.status, Some(401));
        assert_eq!(ok.latency_ms, Some(42));

        let mut blocked = answer(200, "");
        blocked.edge_header = Some("Filter|Site-1".into());
        let c = unauth_check(ProbeEndpoint::WhamAccountsCheck, &Ok(blocked), &[401], true);
        assert!(!c.ok && c.edge_blocked);
        assert!(c.detail.unwrap().contains("l-block"));

        // Empty 200 without the header is still an interception.
        let c = unauth_check(
            ProbeEndpoint::WhamAccountsCheck,
            &Ok(answer(200, "  ")),
            &[401],
            true,
        );
        assert!(!c.ok && c.edge_blocked);
        // HTML 200 too.
        let c = unauth_check(
            ProbeEndpoint::BackendCodexModels,
            &Ok(answer(200, "<html>challenge</html>")),
            &[401],
            true,
        );
        assert!(c.edge_blocked);
    }

    #[test]
    fn unexpected_statuses_and_transport_failures_are_not_ok() {
        let c = unauth_check(
            ProbeEndpoint::CodexV1Models,
            &Ok(answer(404, r#"{"error":"not found"}"#)),
            &[401],
            true,
        );
        assert!(!c.ok && !c.edge_blocked);
        assert_eq!(c.detail.as_deref(), Some("HTTP 404: not found"));

        let c = unauth_check(
            ProbeEndpoint::ModelsUnauth,
            &Err(Transport {
                kind: TransportKind::Timeout,
                message: "timed out".into(),
            }),
            &[401],
            true,
        );
        assert!(!c.ok && c.status.is_none() && c.latency_ms.is_none());
        assert_eq!(c.detail.as_deref(), Some("no answer before the timeout"));

        // A redirect is never healthy.
        let mut r = answer(302, "");
        r.location = Some("https://elsewhere.example/".into());
        let c = unauth_check(ProbeEndpoint::ModelsUnauth, &Ok(r), &[401], false);
        assert!(!c.ok);
    }

    #[test]
    fn non_relay_sites_accept_any_answer_below_500() {
        let c = unauth_check(
            ProbeEndpoint::ModelsUnauth,
            &Ok(answer(404, "nope")),
            &[401],
            false,
        );
        assert!(c.ok);
        let c = unauth_check(
            ProbeEndpoint::ModelsUnauth,
            &Ok(answer(200, "")),
            &[401],
            false,
        );
        assert!(c.ok, "empty 200 is not an interception for generic sites");
        let c = unauth_check(
            ProbeEndpoint::ModelsUnauth,
            &Ok(answer(502, "bad gateway")),
            &[401],
            false,
        );
        assert!(!c.ok);
    }

    #[test]
    fn key_access_maps_relay_messages() {
        let k = |a: Answer| key_access(SiteKind::Krill, &Ok(a));
        assert_eq!(k(answer(200, r#"{"models":[]}"#)), KeyAccess::Ok);
        assert_eq!(k(answer(401, RELAY_401)), KeyAccess::InvalidKey);
        assert_eq!(
            k(err403(
                "当前套餐不包含线路「三网优化cdn」。套餐可用线路：海外线路 https://api.example. | Your plan does not include the entry route"
            )),
            KeyAccess::AccessPointDenied
        );
        assert_eq!(
            k(err403("unknown access point")),
            KeyAccess::AccessPointDenied
        );
        assert_eq!(
            k(err403(
                "your API key is not authorized for the Codex channel"
            )),
            KeyAccess::ChannelDenied
        );
        for m in [
            "account balance is negative, please recharge first",
            "no active plan, please redeem a plan code",
            "insufficient balance, please redeem a plan code or recharge credits",
            "balance billing is disabled for this key; redeem or upgrade a plan to continue",
        ] {
            assert_eq!(k(err403(m)), KeyAccess::QuotaExhausted, "{m}");
        }
        let daily = answer(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"daily quota exceeded, please try again tomorrow"}}"#,
        );
        assert_eq!(k(daily), KeyAccess::QuotaExhausted);
        let qpm = answer(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded, please retry after the current minute window"}}"#,
        );
        assert_eq!(k(qpm.clone()), KeyAccess::Ok, "QPM comes after entitlement");
        assert_eq!(
            key_access(SiteKind::Compatible, &Ok(qpm)),
            KeyAccess::Unknown {
                status: Some(429),
                message: "rate limit exceeded, please retry after the current minute window".into()
            },
            "other sites give no such ordering guarantee"
        );
        assert_eq!(
            k(err403("user disabled")),
            KeyAccess::Unknown {
                status: Some(403),
                message: "user disabled".into()
            }
        );
        assert_eq!(
            k(answer(500, "")),
            KeyAccess::Unknown {
                status: Some(500),
                message: "HTTP 500".into()
            }
        );
        let mut edge = answer(200, "");
        edge.edge_header = Some("x".into());
        assert_eq!(k(edge), KeyAccess::EdgeBlocked);
        assert_eq!(
            key_access(
                SiteKind::Krill,
                &Err(Transport {
                    kind: TransportKind::Connect,
                    message: "refused".into()
                })
            ),
            KeyAccess::Unreachable
        );
    }

    #[test]
    fn compatible_sites_do_not_get_relay_specific_reasons() {
        let a = err403("your API key is not authorized for the Codex channel");
        assert!(matches!(
            key_access(SiteKind::Compatible, &Ok(a)),
            KeyAccess::Unknown { .. }
        ));
        let quota = answer(
            429,
            r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#,
        );
        assert_eq!(
            key_access(SiteKind::Compatible, &Ok(quota)),
            KeyAccess::QuotaExhausted
        );
        let bad = answer(
            401,
            r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#,
        );
        assert_eq!(
            key_access(SiteKind::Compatible, &Ok(bad)),
            KeyAccess::InvalidKey
        );
    }

    #[test]
    fn whoami_requires_the_four_codex_fields() {
        let good = r#"{"email":"a@example.com","chatgpt_user_id":"user-1","chatgpt_account_id":"acc","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":false}"#;
        let w = parse_whoami(good.as_bytes()).unwrap();
        assert_eq!(w.email.as_deref(), Some("a@example.com"));
        assert_eq!(w.user_id, "user-1");
        assert_eq!(w.account_id, "acc");

        let no_email = r#"{"chatgpt_user_id":"u","chatgpt_account_id":"a","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":true}"#;
        assert_eq!(parse_whoami(no_email.as_bytes()).unwrap().email, None);

        for (body, needle) in [
            (
                r#"{"chatgpt_account_id":"a","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":false}"#,
                "chatgpt_user_id",
            ),
            (
                r#"{"chatgpt_user_id":"u","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":false}"#,
                "chatgpt_account_id",
            ),
            (
                r#"{"chatgpt_user_id":"u","chatgpt_account_id":"a","chatgpt_account_is_fedramp":false}"#,
                "chatgpt_plan_type",
            ),
            (
                r#"{"chatgpt_user_id":"u","chatgpt_account_id":"a","chatgpt_plan_type":"pro"}"#,
                "chatgpt_account_is_fedramp",
            ),
            (
                r#"{"chatgpt_user_id":"u","chatgpt_account_id":"a","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":"no"}"#,
                "chatgpt_account_is_fedramp",
            ),
            (
                r#"{"chatgpt_user_id":7,"chatgpt_account_id":"a","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":false}"#,
                "chatgpt_user_id",
            ),
            ("", "not JSON"),
            ("[]", "not a JSON object"),
        ] {
            let e = parse_whoami(body.as_bytes()).unwrap_err();
            assert!(e.contains(needle), "{body}: {e}");
        }
    }

    #[test]
    fn error_messages_are_extracted_and_shortened() {
        assert_eq!(
            error_message(RELAY_401.as_bytes()).as_deref(),
            Some("missing Authorization")
        );
        assert_eq!(
            error_message(br#"{"detail":"Not Found"}"#).as_deref(),
            Some("Not Found")
        );
        assert_eq!(
            error_message(br#"{"error":"boom"}"#).as_deref(),
            Some("boom")
        );
        assert_eq!(
            error_message(b"<!DOCTYPE html><html>..</html>").as_deref(),
            Some("HTML page")
        );
        assert_eq!(error_message(b"  "), None);
        let long = "x".repeat(1000);
        let s = error_message(long.as_bytes()).unwrap();
        assert_eq!(s.chars().count(), MAX_MESSAGE_CHARS + 1);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn truncated_json_is_not_mistaken_for_an_interception() {
        let mut a = answer(200, r#"{"models":[{"slug":"a"#);
        assert!(
            edge_intercepted(&a),
            "a complete but invalid body is not JSON"
        );
        a.truncated = true;
        assert!(!edge_intercepted(&a));
        assert_eq!(key_access(SiteKind::Krill, &Ok(a)), KeyAccess::Ok);
        let mut html = answer(200, "<html>");
        html.truncated = true;
        assert!(edge_intercepted(&html));
    }

    #[test]
    fn wham_answer_must_list_an_account() {
        assert!(wham_lists_accounts(
            br#"{"accounts":[{"id":"a"}],"default_account_id":"a"}"#
        ));
        assert!(!wham_lists_accounts(br#"{"accounts":[]}"#));
        assert!(!wham_lists_accounts(b""));
    }
}
