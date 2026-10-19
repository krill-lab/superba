//! Authenticated, self-contained history capsules. Wire headers and payload
//! versions are authenticated; plaintext is never exposed before verification.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use superba_config::compaction_keys::CompactionKeys;
use zeroize::Zeroizing;

use super::{Failure, Scope};

/// Wire marker for router-owned authenticated summaries.
pub const PREFIX: &str = "spb-compaction:";
const FORMAT: u32 = 1;
const MAX_ENVELOPE: usize = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub format: u32,
    pub owner: String,
    pub provider: String,
    pub credential: String,
    pub model: String,
    pub summary: String,
    /// Ordered hashes of the exact prefix before the trigger, including earlier
    /// capsules. No session id: retries, forks and resumes are independent.
    pub covered: Vec<String>,
    pub prefix_sha256: String,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Envelope([redacted history])")
    }
}

pub fn digest(value: &Value) -> String {
    let mut v = value.clone();
    // IDs/status/annotations are not history content and Codex drops some of
    // them when reserializing retained messages after remote compaction.
    if let Some(o) = v.as_object_mut() {
        for k in ["id", "status", "phase", "codex_metadata"] {
            o.remove(k);
        }
        o.retain(|key, _| !key.starts_with("internal_") && !key.starts_with("codex_"));
        o.entry("type").or_insert(Value::String("message".into()));
    }
    v.sort_all_objects();
    hex::encode(Sha256::digest(serde_json::to_vec(&v).expect("JSON value")))
}

pub fn seal(
    keys: &CompactionKeys,
    scope: &Scope,
    model: &str,
    input: &[Value],
    summary: String,
) -> Result<String, Failure> {
    if summary.trim().is_empty() {
        return Err(Failure::upstream("the model returned an empty summary"));
    }
    let covered: Vec<_> = input.iter().map(digest).collect();
    let env = Envelope {
        format: FORMAT,
        owner: keys.owner().into(),
        provider: scope.provider.clone(),
        credential: scope.credential.clone(),
        model: model.into(),
        summary,
        prefix_sha256: digest(&serde_json::json!(covered)),
        covered,
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&env).map_err(|_| Failure::invalid())?);
    let (kid, key) = keys.active();
    let header = format!("{PREFIX}{FORMAT}:{kid}:");
    let nonce = Nonce::generate();
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| Failure::invalid())?;
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &bytes,
                aad: header.as_bytes(),
            },
        )
        .map_err(|_| Failure::invalid())?;
    let mut payload = nonce.to_vec();
    payload.extend(ct);
    let wire = format!("{header}{}", URL_SAFE_NO_PAD.encode(payload));
    if wire.len() > MAX_ENVELOPE {
        return Err(Failure::upstream(
            "the generated compaction envelope is too large",
        ));
    }
    Ok(wire)
}

pub fn open(keys: &CompactionKeys, scopes: &[Scope], wire: &str) -> Result<Envelope, Failure> {
    if wire.len() > MAX_ENVELOPE {
        return Err(Failure::invalid());
    }
    let rest = wire.strip_prefix(PREFIX).ok_or_else(Failure::foreign)?;
    let mut parts = rest.splitn(3, ':');
    let version = parts.next().ok_or_else(Failure::invalid)?;
    if version != "1" {
        return Err(Failure::new(
            "compaction_version_unsupported",
            "This summary uses an unsupported Superba envelope version.",
        ));
    }
    let kid = parts.next().ok_or_else(Failure::invalid)?;
    let encoded = parts.next().ok_or_else(Failure::invalid)?;
    let key = keys.key(kid).ok_or_else(|| Failure::new("compaction_key_unavailable", "The key for this summary is unavailable or retired. Restore its Superba vault, or return to the original installation."))?;
    let blob = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| Failure::invalid())?;
    if blob.len() < 28 {
        return Err(Failure::invalid());
    }
    let nonce = Nonce::try_from(&blob[..12]).map_err(|_| Failure::invalid())?;
    let header = format!("{PREFIX}{version}:{kid}:");
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| Failure::invalid())?;
    let plain = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &blob[12..],
                    aad: header.as_bytes(),
                },
            )
            .map_err(|_| Failure::invalid())?,
    );
    let env: Envelope = serde_json::from_slice(&plain).map_err(|_| Failure::invalid())?;
    if env.format != FORMAT
        || env.summary.trim().is_empty()
        || env.prefix_sha256 != digest(&serde_json::json!(env.covered))
    {
        return Err(Failure::invalid());
    }
    if env.owner != keys.owner()
        || !scopes
            .iter()
            .any(|s| s.provider == env.provider && s.credential == env.credential)
    {
        return Err(Failure::new(
            "compaction_scope_mismatch",
            "This summary belongs to another owner or provider credential. Restore the original provider account before continuing.",
        ));
    }
    Ok(env)
}
