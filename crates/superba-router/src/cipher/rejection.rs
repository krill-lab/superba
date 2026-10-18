//! Recognising an upstream's refusal to read a ciphertext.
//!
//! Wordings seen (2026-10-18, the Krill relay in front of the ChatGPT Codex
//! backend; GitHub issue 11):
//!
//! - HTTP 400 `{"error":{"message":"The encrypted content for item amsg_… could
//!   not be verified. Reason: encrypted content hydration failed: Encrypted
//!   content could not be decrypted or parsed.","type":"invalid_request_error",
//!   "param":"input","code":"invalid_encrypted_content"}}`; also `for item rs_…`,
//!   `fco_…`, `fc_…`, or the ciphertext's ends instead of an item (`The
//!   encrypted content gAAA...AA== could not be verified`);
//! - HTTP 400 with `"message":"Encrypted function output content could not be
//!   decrypted or decoded."` (no item named);
//! - the relay's own refusal of a ciphertext it already saw refused:
//!   `{"type":"error","error":{…same fields…},"status":400}`, which is also the
//!   shape of a WebSocket `error` event.
//!
//! Recognised: `error.code` (or `response.error.code` of `response.failed`)
//! `invalid_encrypted_content`, or one of these wordings in its message.

use memchr::memmem;
use serde_json::Value;

use super::REJECTION_CODE;
use super::scan::{Cipher, Place};

const VERIFY_PHRASE: &str = "The encrypted content ";
const NOT_VERIFIED: &str = " could not be verified";
const FUNCTION_OUTPUT_PHRASE: &str = "Encrypted function output content could not be decrypted";
const HYDRATION_PHRASE: &str = "encrypted content hydration failed";

/// What a refusal names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    /// The item with this id.
    Item(String),
    /// The ciphertext with these first and last four characters.
    Ends(String, String),
    /// An encrypted tool output or agent-message payload (not which).
    Parts,
    /// Nothing in particular.
    Unnamed,
}

/// An upstream refused a ciphertext of the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    /// What it names.
    pub named: Named,
    /// Its message (names at most an item id or a ciphertext's ends).
    pub message: String,
}

impl Rejection {
    /// True when the refusal names `c`.
    pub fn names(&self, c: &Cipher) -> bool {
        match &self.named {
            Named::Item(id) => c.item_id.as_deref() == Some(id.as_str()),
            Named::Ends(head, tail) => c.has_ends(head, tail),
            Named::Parts => matches!(c.place, Place::Part { .. }),
            Named::Unnamed => false,
        }
    }

    fn from_error(error: &Value) -> Option<Self> {
        let obj = error.as_object()?;
        let message = obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let coded = obj.get("code").and_then(Value::as_str) == Some(REJECTION_CODE);
        let worded = message.starts_with(VERIFY_PHRASE) && message.contains(NOT_VERIFIED)
            || message.contains(FUNCTION_OUTPUT_PHRASE)
            || message.contains(HYDRATION_PHRASE);
        if !coded && !worded {
            return None;
        }
        Some(Self {
            named: named(message),
            message: message.chars().take(300).collect(),
        })
    }
}

/// What a message names.
fn named(message: &str) -> Named {
    if let Some(rest) = message
        .find(VERIFY_PHRASE)
        .map(|at| &message[at + VERIFY_PHRASE.len()..])
        && let Some(end) = rest.find(NOT_VERIFIED)
    {
        let what = &rest[..end];
        if let Some(id) = what.strip_prefix("for item ") {
            return Named::Item(id.trim().to_owned());
        }
        if let Some((head, tail)) = what.split_once("...")
            && !what.contains(' ')
        {
            return Named::Ends(head.to_owned(), tail.to_owned());
        }
    }
    if message.contains(FUNCTION_OUTPUT_PHRASE) {
        return Named::Parts;
    }
    Named::Unnamed
}

/// A refusal in an HTTP error body (`{"error":{…}}` or `{"type":"error",
/// "error":{…}}`).
pub fn from_body(body: &[u8]) -> Option<Rejection> {
    memmem::find(body, b"ncrypted")?;
    let v: Value = serde_json::from_slice(body).ok()?;
    Rejection::from_error(v.get("error")?)
}

/// A refusal in a stream event: `error` (`{"type":"error","error":{…}}`, or
/// the error fields at the top level) or `response.failed`
/// (`response.error`).
pub fn from_event(json: &[u8]) -> Option<Rejection> {
    memmem::find(json, b"ncrypted")?;
    let v: Value = serde_json::from_slice(json).ok()?;
    match v.get("type").and_then(Value::as_str)? {
        "error" => match v.get("error") {
            Some(e @ Value::Object(_)) => Rejection::from_error(e),
            _ => Rejection::from_error(&v),
        },
        "response.failed" => Rejection::from_error(v.get("response")?.get("error")?),
        _ => None,
    }
}

/// True when an answer with `status` and `content_type` may be such a refusal
/// (worth reading before it is passed on).
pub fn may_be(status: u16, content_type: Option<&str>) -> bool {
    matches!(status, 400 | 422 | 502)
        && content_type.is_some_and(|c| c.to_ascii_lowercase().contains("json"))
}
