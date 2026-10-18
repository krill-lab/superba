//! Signatures of the channels that sign their webhooks.
//!
//! - **Feishu / Lark** custom bots: `string_to_sign = "<timestamp>\n<secret>"`
//!   is the HMAC-SHA256 *key* and the message is empty; the signature is the
//!   base64 of the MAC, sent with the timestamp (seconds) in the JSON body.
//! - **DingTalk** custom bots: HMAC-SHA256 keyed with the secret over
//!   `"<timestamp_ms>\n<secret>"`, base64, then URL-encoded into the query
//!   (`&timestamp=..&sign=..`).
//! - **Webhook** (Superba's own): `X-Superba-Signature: sha256=<hex>` over
//!   `"<timestamp>.<body>"` keyed with the secret, so a receiver can check the
//!   sender and reject replays by the timestamp.

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256 of `message` keyed with `key`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    // HMAC accepts keys of any length.
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// Feishu / Lark signature for `timestamp` (seconds).
pub fn feishu(secret: &str, timestamp_secs: i64) -> String {
    let string_to_sign = format!("{timestamp_secs}\n{secret}");
    let mac = hmac_sha256(string_to_sign.as_bytes(), b"");
    base64::engine::general_purpose::STANDARD.encode(mac)
}

/// DingTalk signature for `timestamp` (milliseconds), not yet URL-encoded.
pub fn dingtalk(secret: &str, timestamp_ms: i64) -> String {
    let string_to_sign = format!("{timestamp_ms}\n{secret}");
    let mac = hmac_sha256(secret.as_bytes(), string_to_sign.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac)
}

/// Superba webhook signature header value (`sha256=<hex>`).
pub fn webhook(secret: &str, timestamp_secs: i64, body: &[u8]) -> String {
    let mut message = format!("{timestamp_secs}.").into_bytes();
    message.extend_from_slice(body);
    format!(
        "sha256={}",
        hex::encode(hmac_sha256(secret.as_bytes(), &message))
    )
}
