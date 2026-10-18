//! Random local tokens, per-consumer source tokens and constant-time
//! comparison.
//!
//! Source tokens (user bug 45): every Codex host that is not this computer (a
//! WSL distribution) authenticates with its own token
//! `sbs_<hex(source)>_<hex(HMAC-SHA256(router token, "superba-router-source-v1\0" source))>`,
//! derived from the router token. The router needs no list of sources: it
//! recomputes the MAC of the source the token names, and request-log rows carry
//! that source. Replacing the router token invalidates every source token.

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Prefix of a per-consumer source token.
pub const SOURCE_TOKEN_PREFIX: &str = "sbs_";

/// Domain separation of the source token MAC.
const SOURCE_MAC_CONTEXT: &[u8] = b"superba-router-source-v1\0";

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

/// A source id usable in a token: 1 to 80 characters of ASCII letters, digits
/// and `: . _ -` (`wsl:Ubuntu-24.04`).
pub fn valid_source(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 80
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'))
}

/// The token a Codex host `source` presents (see the module docs). `None` for an
/// invalid source id.
pub fn source_token(router_token: &[u8], source: &str) -> Option<String> {
    if !valid_source(source) {
        return None;
    }
    let mac = hmac_sha256(router_token, &[SOURCE_MAC_CONTEXT, source.as_bytes()]);
    Some(format!(
        "{SOURCE_TOKEN_PREFIX}{}_{}",
        hex::encode(source.as_bytes()),
        hex::encode(mac)
    ))
}

/// The source a presented token was made for, when its MAC matches.
pub fn verify_source_token(router_token: &[u8], presented: &[u8]) -> Option<String> {
    let rest = presented.strip_prefix(SOURCE_TOKEN_PREFIX.as_bytes())?;
    let sep = rest.iter().rposition(|b| *b == b'_')?;
    let source = hex::decode(&rest[..sep]).ok()?;
    let source = String::from_utf8(source).ok()?;
    let expected = source_token(router_token, &source)?;
    constant_time_eq(expected.as_bytes(), presented).then_some(source)
}

/// Prefix of the token Codex presents to the router (its `OPENAI_API_KEY`).
pub const ROUTER_TOKEN_PREFIX: &str = "sbr_";
/// Prefix of the per-instance admin token.
pub const ADMIN_TOKEN_PREFIX: &str = "sba_";

/// `prefix` + 64 hex characters from two random v4 UUIDs (244 random bits, from
/// the OS generator).
pub fn generate(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

/// Compare two byte strings in time independent of where they differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// The bearer token of an `Authorization` header value, if well formed.
pub fn bearer(value: &[u8]) -> Option<&[u8]> {
    let s = value
        .strip_prefix(b"Bearer ")
        .or_else(|| value.strip_prefix(b"bearer "))?;
    let s = s.trim_ascii();
    (!s.is_empty()).then_some(s)
}
