//! Random local tokens and constant-time comparison.

use uuid::Uuid;

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
