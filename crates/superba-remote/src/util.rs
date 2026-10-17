//! Small helpers: hashing, ids, remote POSIX paths.

use sha2::{Digest, Sha256};
use superba_core::TargetId;

/// sha256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Join a remote POSIX directory and a relative path. Remote paths are always
/// handled as strings with `/`: `Path::join` would use `\` on a Windows client.
pub fn rjoin(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        return rel.to_owned();
    }
    format!(
        "{}/{}",
        dir.trim_end_matches('/'),
        rel.trim_start_matches('/')
    )
}

/// File-system safe directory name for a target id (readable prefix + short hash,
/// so distinct ids never collide).
pub fn target_dir_name(id: &TargetId) -> String {
    let readable: String = id
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    format!("{readable}-{}", &sha256_hex(id.as_str().as_bytes())[..10])
}

/// A unique id for an apply (`<millis>-<8 hex>`).
pub fn new_apply_id(now_ms: i64) -> String {
    let nonce = crate::script::new_nonce();
    format!("{now_ms}-{}", &nonce[..8])
}
