//! Integrity of a catalog tree: a SHA-256 manifest (`dist/index.json`) signed with
//! ed25519 (`dist/index.json.sig`).
//!
//! Only public keys are compiled in ([`TRUSTED_KEYS`]); the signing key never enters
//! the repository. The built-in snapshot is part of the (signed) application binary,
//! so it is accepted with a valid manifest even when its index is not signed yet;
//! a catalog fetched from the network must carry a valid signature from a trusted,
//! non-revoked key. Key rotation is described in `catalog/CONTRIBUTING.md`.

use std::collections::BTreeMap;

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::error::{CatalogError, Result};
use crate::format::{INDEX_PATH, Index, SIGNATURE_FORMAT, SIGNATURE_PATH, SignatureFile};

/// Domain separation prefix of the signed message.
pub const SIGNATURE_CONTEXT: &[u8] = b"superba-catalog-index-v1\n";

/// A public key allowed to sign catalog indexes.
#[derive(Clone, Copy, Debug)]
pub struct TrustedKey {
    /// [`key_id`] of [`TrustedKey::public_key`].
    pub id: &'static str,
    /// Raw ed25519 public key.
    pub public_key: [u8; 32],
    /// What the key is for (shown by `superba-catalog-gen verify`).
    pub note: &'static str,
}

/// Keys whose signatures are accepted. Add the new key here first when rotating;
/// see `catalog/CONTRIBUTING.md`.
pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    id: "044ecd9ca3481d4f",
    public_key: [
        0xe2, 0x2f, 0x98, 0x04, 0xf3, 0x11, 0xf2, 0x6e, 0x56, 0x94, 0x17, 0x49, 0x06, 0x1a, 0x2c,
        0x5c, 0xf2, 0xa9, 0xdf, 0xec, 0xb8, 0x91, 0xe7, 0xf8, 0x38, 0xcc, 0x6f, 0x51, 0x3d, 0x97,
        0xe4, 0xe9,
    ],
    note: "catalog signing key 1 (generated 2026-10-16)",
}];

/// Key ids that must never be accepted again (compromised or retired keys).
pub const REVOKED_KEY_IDS: &[&str] = &[];

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Key id of a public key: the first 16 hex digits of its SHA-256.
pub fn key_id(public_key: &[u8; 32]) -> String {
    sha256_hex(public_key)[..16].to_owned()
}

/// The exact bytes that are signed: [`SIGNATURE_CONTEXT`] followed by `index.json`.
pub fn signed_message(index_bytes: &[u8]) -> Vec<u8> {
    signed_message_with_context(SIGNATURE_CONTEXT, index_bytes)
}

/// `context` followed by the index bytes (the settings catalog and the feeds use
/// different contexts, so one index can never pass as the other).
pub fn signed_message_with_context(context: &[u8], index_bytes: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(context.len() + index_bytes.len());
    m.extend_from_slice(context);
    m.extend_from_slice(index_bytes);
    m
}

/// Outcome of a signature check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    /// Signed by the trusted key with this id.
    Valid {
        /// Key id.
        key_id: String,
    },
    /// No signature file.
    Unsigned,
    /// A signature file exists but no entry verifies with a trusted key.
    Invalid {
        /// Why.
        reason: String,
    },
}

impl SignatureStatus {
    /// True for [`SignatureStatus::Valid`].
    pub fn is_valid(&self) -> bool {
        matches!(self, SignatureStatus::Valid { .. })
    }
}

/// Check `sig_bytes` (the signature file) over `index_bytes` against `trusted`.
pub fn verify_signature(
    index_bytes: &[u8],
    sig_bytes: Option<&[u8]>,
    trusted: &[TrustedKey],
    revoked: &[&str],
) -> SignatureStatus {
    verify_signature_with_context(SIGNATURE_CONTEXT, index_bytes, sig_bytes, trusted, revoked)
}

/// [`verify_signature`] with another signature context (the feeds index).
pub fn verify_signature_with_context(
    context: &[u8],
    index_bytes: &[u8],
    sig_bytes: Option<&[u8]>,
    trusted: &[TrustedKey],
    revoked: &[&str],
) -> SignatureStatus {
    let Some(sig_bytes) = sig_bytes else {
        return SignatureStatus::Unsigned;
    };
    let file: SignatureFile = match serde_json::from_slice(sig_bytes) {
        Ok(f) => f,
        Err(e) => {
            return SignatureStatus::Invalid {
                reason: format!("{SIGNATURE_PATH} does not parse: {e}"),
            };
        }
    };
    if file.format != SIGNATURE_FORMAT {
        return SignatureStatus::Invalid {
            reason: format!("unsupported signature format {}", file.format),
        };
    }
    let message = signed_message_with_context(context, index_bytes);
    let mut reasons = Vec::new();
    for entry in &file.signatures {
        if entry.algorithm != "ed25519" {
            reasons.push(format!(
                "{}: unsupported algorithm {}",
                entry.key_id, entry.algorithm
            ));
            continue;
        }
        if revoked.contains(&entry.key_id.as_str()) {
            reasons.push(format!("{}: key revoked", entry.key_id));
            continue;
        }
        let Some(key) = trusted.iter().find(|k| k.id == entry.key_id) else {
            reasons.push(format!("{}: unknown key", entry.key_id));
            continue;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&key.public_key) else {
            reasons.push(format!("{}: invalid public key", entry.key_id));
            continue;
        };
        let raw = match base64::engine::general_purpose::STANDARD.decode(entry.signature.trim()) {
            Ok(r) => r,
            Err(_) => {
                reasons.push(format!("{}: signature is not base64", entry.key_id));
                continue;
            }
        };
        let Ok(raw) = <[u8; 64]>::try_from(raw.as_slice()) else {
            reasons.push(format!("{}: signature has the wrong length", entry.key_id));
            continue;
        };
        let sig = Signature::from_bytes(&raw);
        match vk.verify_strict(&message, &sig) {
            Ok(()) => {
                return SignatureStatus::Valid {
                    key_id: entry.key_id.clone(),
                };
            }
            Err(_) => reasons.push(format!("{}: signature does not match", entry.key_id)),
        }
    }
    if reasons.is_empty() {
        reasons.push("no signatures".into());
    }
    SignatureStatus::Invalid {
        reason: reasons.join("; "),
    }
}

/// Parse an index, checking its format.
pub fn parse_index(index_bytes: &[u8]) -> Result<Index> {
    let index: Index =
        serde_json::from_slice(index_bytes).map_err(|e| CatalogError::parse(INDEX_PATH, e))?;
    if index.index_format != crate::format::INDEX_FORMAT {
        return Err(CatalogError::UnsupportedFormat {
            what: "index",
            found: index.index_format,
            supported: crate::format::INDEX_FORMAT,
        });
    }
    if index.catalog_format != crate::format::CATALOG_FORMAT {
        return Err(CatalogError::UnsupportedFormat {
            what: "catalog",
            found: index.catalog_format,
            supported: crate::format::CATALOG_FORMAT,
        });
    }
    Ok(index)
}

/// Check that `files` holds exactly the bytes the index lists (path, size, SHA-256).
/// Extra files are ignored; a missing or different file is an error.
pub fn verify_files(index: &Index, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for f in &index.files {
        validate_relative_path(&f.path)?;
        let Some(bytes) = files.get(&f.path) else {
            return Err(CatalogError::integrity(
                &f.path,
                "listed in the index but missing",
            ));
        };
        if bytes.len() as u64 != f.size {
            return Err(CatalogError::integrity(
                &f.path,
                format!("size {} differs from the index ({})", bytes.len(), f.size),
            ));
        }
        let got = sha256_hex(bytes);
        if !got.eq_ignore_ascii_case(&f.sha256) {
            return Err(CatalogError::integrity(
                &f.path,
                "SHA-256 differs from the index",
            ));
        }
    }
    for v in &index.versions {
        if index.file(&v.path).is_none() {
            return Err(CatalogError::integrity(
                &v.path,
                "version file is not listed in the index files",
            ));
        }
    }
    Ok(())
}

/// Reject absolute paths, `..`, backslashes and empty segments: index paths name
/// files below the catalog root only (they become cache file names).
pub fn validate_relative_path(path: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..");
    if bad {
        return Err(CatalogError::integrity(path, "not a plain relative path"));
    }
    Ok(())
}
