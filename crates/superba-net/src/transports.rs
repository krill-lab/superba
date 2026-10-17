//! Transport checks of a Responses endpoint ([`superba_core::LineProber::probe_transports`]):
//! the Responses WebSocket and zstd request bodies, without a billed turn.
//!
//! - **WebSocket**: Codex's handshake (`GET …/responses`, `Upgrade: websocket`,
//!   `OpenAI-Beta: responses_websockets=…`, the key). 101 = supported, and the
//!   socket is closed at once with a close frame (nothing is sent on it); 426,
//!   another 4xx or an answer without the upgrade = not supported; 401 / 403 /
//!   429 / 5xx / no answer = unknown (they say nothing about the transport).
//! - **zstd**: three tiny `POST …/responses` with an invalid body ([`PROBE_BODY`]:
//!   an empty model, no input, `max_output_tokens: 0`): plain (A),
//!   zstd-compressed with `Content-Encoding: zstd`
//!   (B), and the compressed bytes declared as plain JSON (C). A server that
//!   decodes zstd rejects B exactly like A (the same validation error); one
//!   that does not answers 415 or rejects B like the unreadable C. When A and C
//!   get the same answer the server's errors do not tell the bodies apart and
//!   the result stays unknown. No request can start a turn: none names a
//!   model, and each is invalid twice more (no input, no output allowed), so a
//!   server that fills in a default model still rejects it.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use superba_core::{
    Error, ResolvedEndpoints, Result, Secret, TransportCheck, TransportProbe, TransportSupport,
};

use crate::HttpLineProber;
use crate::classify::{error_message, short, transport_detail};
use crate::http::{Answer, Handshake, Request, Transport, display_url, ensure_key_target};
use crate::keys::join_base;

/// `OpenAI-Beta` of Codex's Responses WebSocket handshake.
pub(crate) const RESPONSES_WEBSOCKETS_BETA: &str = "responses_websockets=2026-02-06";

/// Body of the zstd probe: a Responses request with an empty model, no input
/// and no output allowed, which every server rejects in validation once it has
/// read it (and none can run, even with a default model filled in).
pub(crate) const PROBE_BODY: &[u8] = br#"{"model":"","input":[],"max_output_tokens":0}"#;

/// Answer bytes kept per probe request.
const BODY_LIMIT: usize = 8 * 1024;

/// A zstd frame (RFC 8878) holding `plain` in one raw block: single segment,
/// one-byte content size, no checksum, no dictionary. Every zstd decoder
/// accepts it; compression is pointless for the tiny probe body. `plain`
/// must be shorter than 256 bytes.
pub(crate) fn zstd_frame(plain: &[u8]) -> Vec<u8> {
    debug_assert!(plain.len() < 256, "one-byte frame content size");
    let len = u8::try_from(plain.len()).unwrap_or(u8::MAX);
    let plain = &plain[..usize::from(len)];
    // Block header: Last_Block = 1, Block_Type = Raw (0), Block_Size = len.
    let block = 1u32 | (u32::from(len) << 3);
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, len];
    out.extend_from_slice(&block.to_le_bytes()[..3]);
    out.extend_from_slice(plain);
    out
}

/// What an error answer says, reduced to what tells two answers apart:
/// status, the OpenAI error fields and the message with ids and numbers
/// blanked (request ids differ between otherwise equal answers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    status: u16,
    code: Option<String>,
    kind: Option<String>,
    param: Option<String>,
    message: String,
}

pub(crate) fn signature(a: &Answer) -> Signature {
    let field = |v: &Value, p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_owned);
    let (code, kind, param) = match serde_json::from_slice::<Value>(&a.body) {
        Ok(v) => (
            field(&v, "/error/code"),
            field(&v, "/error/type"),
            field(&v, "/error/param"),
        ),
        Err(_) => (None, None, None),
    };
    let message = error_message(&a.body).unwrap_or_default();
    Signature {
        status: a.status,
        code,
        kind,
        param,
        message: blank_ids(&message),
    }
}

/// Lowercase `text` with every run of digits and every hex-looking word of
/// six or more characters replaced by `#`.
fn blank_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        let (core, tail) = match word.char_indices().last() {
            Some((i, c)) if !c.is_ascii_alphanumeric() && c != '-' => (&word[..i], &word[i..]),
            _ => (word, ""),
        };
        let hexish = core.len() >= 6
            && core.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            && core.chars().any(|c| c.is_ascii_digit());
        if hexish {
            out.push('#');
        } else {
            let mut digits = false;
            for c in core.chars() {
                if c.is_ascii_digit() {
                    if !digits {
                        out.push('#');
                    }
                    digits = true;
                } else {
                    digits = false;
                    out.push(c.to_ascii_lowercase());
                }
            }
        }
        out.push_str(&tail.to_ascii_lowercase());
    }
    out
}

/// Statuses that say something about the key or the server, not the transport.
fn says_nothing(status: u16) -> bool {
    matches!(status, 401 | 403 | 407 | 408 | 429) || status >= 500
}

fn unknown_for(a: &Answer) -> TransportCheck {
    let why = match a.status {
        401 | 403 => "the key was rejected",
        429 => "rate limited",
        _ => "",
    };
    let detail = match (why, error_message(&a.body)) {
        ("", Some(m)) => format!("HTTP {}: {m}", a.status),
        ("", None) => format!("HTTP {}", a.status),
        (why, _) => format!("HTTP {}: {why}", a.status),
    };
    TransportCheck::new(TransportSupport::Unknown, Some(a.status), detail)
}

/// Classify the WebSocket handshake.
pub(crate) fn classify_websocket(r: &std::result::Result<Handshake, Transport>) -> TransportCheck {
    let h = match r {
        Err(t) => return TransportCheck::unknown(transport_detail(t)),
        Ok(h) => h,
    };
    let status = Some(h.status);
    if h.edge_blocked {
        return TransportCheck::new(
            TransportSupport::Unknown,
            status,
            format!("intercepted by the line's edge (HTTP {})", h.status),
        );
    }
    match h.status {
        101 => TransportCheck::new(TransportSupport::Supported, status, "HTTP 101"),
        426 => TransportCheck::new(
            TransportSupport::Unsupported,
            status,
            "HTTP 426: the line serves Responses over HTTP only",
        ),
        s if says_nothing(s) || (300..400).contains(&s) => {
            let a = Answer {
                status: s,
                edge_header: None,
                content_type: None,
                cors: false,
                location: None,
                body: h.body.clone(),
                truncated: false,
                elapsed: std::time::Duration::ZERO,
                retry_after: None,
                etag: None,
            };
            unknown_for(&a)
        }
        s => {
            let detail = match error_message(&h.body) {
                Some(m) => format!("HTTP {s} without an upgrade: {m}"),
                None => format!("HTTP {s} without an upgrade"),
            };
            TransportCheck::new(TransportSupport::Unsupported, status, detail)
        }
    }
}

/// Classify the zstd probe from the plain (A), compressed (B) and
/// compressed-as-plain (C) answers.
pub(crate) fn classify_zstd(
    plain: &std::result::Result<Answer, Transport>,
    zstd: &std::result::Result<Answer, Transport>,
    garbled: &std::result::Result<Answer, Transport>,
) -> TransportCheck {
    let b = match zstd {
        Err(t) => return TransportCheck::unknown(transport_detail(t)),
        Ok(b) => b,
    };
    let status = Some(b.status);
    if b.edge_header.is_some() {
        return TransportCheck::new(
            TransportSupport::Unknown,
            status,
            format!("intercepted by the line's edge (HTTP {})", b.status),
        );
    }
    if b.status == 415 {
        return TransportCheck::new(
            TransportSupport::Unsupported,
            status,
            match error_message(&b.body) {
                Some(m) => format!("HTTP 415: {m}"),
                None => "HTTP 415: zstd request bodies are not accepted".to_owned(),
            },
        );
    }
    if says_nothing(b.status) {
        return unknown_for(b);
    }
    let a = match plain {
        Err(t) => return TransportCheck::unknown(transport_detail(t)),
        Ok(a) if says_nothing(a.status) => return unknown_for(a),
        Ok(a) => a,
    };
    let (sa, sb) = (signature(a), signature(b));
    let sc = garbled.as_ref().ok().map(signature);
    let detail = |verdict: &str| match error_message(&b.body) {
        Some(m) => format!("HTTP {} ({verdict}): {m}", b.status),
        None => format!("HTTP {} ({verdict})", b.status),
    };
    if sc.as_ref() == Some(&sa) {
        // The server answers a readable and an unreadable body alike.
        return TransportCheck::new(
            TransportSupport::Unknown,
            status,
            detail("the server's errors do not tell the bodies apart"),
        );
    }
    if sb == sa {
        return TransportCheck::new(
            TransportSupport::Supported,
            status,
            detail("decoded, then rejected like the plain body"),
        );
    }
    if b.is_success() && a.is_success() {
        return TransportCheck::new(TransportSupport::Supported, status, detail("decoded"));
    }
    if sc.as_ref() == Some(&sb) || (400..500).contains(&b.status) {
        return TransportCheck::new(
            TransportSupport::Unsupported,
            status,
            detail("the compressed body was not decoded"),
        );
    }
    TransportCheck::new(TransportSupport::Unknown, status, detail("inconclusive"))
}

impl HttpLineProber {
    pub(crate) async fn probe_transports_impl(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
    ) -> Result<TransportProbe> {
        let base = &endpoints.openai_base_url;
        if key.is_empty() {
            return Err(Error::Blocked {
                reasons: vec![superba_core::BlockReason::MissingKey],
            });
        }
        ensure_key_target(base, key)?;
        if base.origin() != endpoints.origin.url().origin() {
            return Err(Error::invalid(format!(
                "openai_base_url {} is not on the endpoints' origin {}",
                display_url(base),
                endpoints.origin
            )));
        }
        let url = join_base(base, "responses");
        let timeout = self.config.request_timeout;
        let mut extra = HeaderMap::new();
        extra.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static(RESPONSES_WEBSOCKETS_BETA),
        );
        let post = |body: Vec<u8>, encoding: Option<&'static str>| {
            let req = Request::get(url.clone(), timeout, BODY_LIMIT)
                .with_key(key)
                .with_json(body, crate::http::ACCEPT_JSON);
            match encoding {
                Some(e) => req.with_content_encoding(e),
                None => req,
            }
        };
        let frame = zstd_frame(PROBE_BODY);
        let (ws, plain, zstd, garbled) = tokio::join!(
            self.http
                .websocket_handshake(&url, key, extra, timeout, BODY_LIMIT),
            self.http.send(post(PROBE_BODY.to_vec(), None)),
            self.http.send(post(frame.clone(), Some("zstd"))),
            self.http.send(post(frame, None)),
        );
        let redact = |mut c: TransportCheck| {
            c.detail = c.detail.map(|d| short(&crate::http::redact(&d, Some(key))));
            c
        };
        let probe = TransportProbe {
            websocket: redact(classify_websocket(&ws)),
            zstd: redact(classify_zstd(&plain, &zstd, &garbled)),
        };
        tracing::debug!(
            url = %display_url(&url),
            websocket = ?probe.websocket.support,
            ws_status = ?probe.websocket.status,
            zstd = ?probe.zstd.support,
            zstd_status = ?probe.zstd.status,
            "transport probe"
        );
        Ok(probe)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn answer(status: u16, body: &str) -> std::result::Result<Answer, Transport> {
        Ok(Answer {
            status,
            edge_header: None,
            content_type: Some("application/json".into()),
            cors: false,
            location: None,
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::from_millis(3),
            retry_after: None,
            etag: None,
        })
    }

    fn handshake(status: u16, body: &str) -> std::result::Result<Handshake, Transport> {
        Ok(Handshake {
            status,
            edge_blocked: false,
            body: body.as_bytes().to_vec(),
        })
    }

    const MISSING_MODEL: &str = r#"{"error":{"message":"Missing required parameter: 'model'. (request id req_8f3a9c2b11)","type":"invalid_request_error","param":"model","code":"missing_required_parameter"}}"#;
    const MISSING_MODEL_2: &str = r#"{"error":{"message":"Missing required parameter: 'model'. (request id req_00ab77de42)","type":"invalid_request_error","param":"model","code":"missing_required_parameter"}}"#;
    const BAD_JSON: &str = r#"{"error":{"message":"We could not parse the JSON body of your request.","type":"invalid_request_error","param":null,"code":null}}"#;

    #[test]
    fn the_zstd_frame_is_a_raw_single_segment_frame() {
        assert_eq!(
            zstd_frame(b"{}"),
            [
                0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x02, 0x11, 0x00, 0x00, b'{', b'}'
            ]
        );
        let f = zstd_frame(b"");
        assert_eq!(&f[4..], [0x20, 0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn ids_and_numbers_do_not_change_a_signature() {
        let a = signature(&answer(400, MISSING_MODEL).unwrap());
        let b = signature(&answer(400, MISSING_MODEL_2).unwrap());
        assert_eq!(a, b);
        assert_ne!(a, signature(&answer(400, BAD_JSON).unwrap()));
        assert_ne!(a, signature(&answer(422, MISSING_MODEL).unwrap()));
        assert_eq!(blank_ids("Trace 0f3a9c2b-11 at 12:30"), "trace # at #:#");
        assert_eq!(blank_ids("bad model gpt-5"), "bad model gpt-#");
    }

    #[test]
    fn websocket_answers_are_classified() {
        let s = |r| classify_websocket(&r).support;
        assert_eq!(s(handshake(101, "")), TransportSupport::Supported);
        assert_eq!(s(handshake(426, "")), TransportSupport::Unsupported);
        assert_eq!(s(handshake(404, "")), TransportSupport::Unsupported);
        assert_eq!(s(handshake(405, "")), TransportSupport::Unsupported);
        assert_eq!(s(handshake(200, "{}")), TransportSupport::Unsupported);
        for status in [401, 403, 429, 500, 502, 503, 301] {
            assert_eq!(
                s(handshake(status, "")),
                TransportSupport::Unknown,
                "{status}"
            );
        }
        let edge = Ok(Handshake {
            status: 200,
            edge_blocked: true,
            body: Vec::new(),
        });
        assert_eq!(s(edge), TransportSupport::Unknown);
        let down = Err(Transport {
            kind: crate::http::TransportKind::Connect,
            message: "connection refused".into(),
        });
        let c = classify_websocket(&down);
        assert_eq!(c.support, TransportSupport::Unknown);
        assert_eq!(c.status, None);
        assert_eq!(c.detail.as_deref(), Some("connection refused"));
        assert_eq!(classify_websocket(&handshake(426, "")).status, Some(426));
    }

    #[test]
    fn zstd_answers_are_classified() {
        let s = |a, b, c| classify_zstd(&a, &b, &c).support;
        // Decoded: rejected exactly like the plain body.
        assert_eq!(
            s(
                answer(400, MISSING_MODEL),
                answer(400, MISSING_MODEL_2),
                answer(400, BAD_JSON)
            ),
            TransportSupport::Supported
        );
        // Not decoded: 415, or rejected like unreadable bytes.
        assert_eq!(
            s(
                answer(400, MISSING_MODEL),
                answer(415, ""),
                answer(400, BAD_JSON)
            ),
            TransportSupport::Unsupported
        );
        assert_eq!(
            s(
                answer(400, MISSING_MODEL),
                answer(400, BAD_JSON),
                answer(400, BAD_JSON)
            ),
            TransportSupport::Unsupported
        );
        assert_eq!(
            s(
                answer(422, MISSING_MODEL),
                answer(400, r#"{"error":"unsupported content encoding"}"#),
                answer(422, BAD_JSON)
            ),
            TransportSupport::Unsupported
        );
        // The server's errors do not tell readable from unreadable bodies.
        assert_eq!(
            s(
                answer(400, r#"{"error":"bad request"}"#),
                answer(400, r#"{"error":"bad request"}"#),
                answer(400, r#"{"error":"bad request"}"#)
            ),
            TransportSupport::Unknown
        );
        // Key, rate limit and server errors say nothing about zstd.
        for status in [401, 403, 429, 500, 503] {
            assert_eq!(
                s(
                    answer(400, MISSING_MODEL),
                    answer(status, ""),
                    answer(400, BAD_JSON)
                ),
                TransportSupport::Unknown,
                "{status}"
            );
            assert_eq!(
                s(
                    answer(status, ""),
                    answer(400, MISSING_MODEL),
                    answer(400, BAD_JSON)
                ),
                TransportSupport::Unknown,
                "plain {status}"
            );
        }
        let c = classify_zstd(
            &answer(400, MISSING_MODEL),
            &answer(415, ""),
            &answer(400, BAD_JSON),
        );
        assert_eq!(c.status, Some(415));
    }
}
