//! Signing keys and index signatures.
//!
//! Key file format (never committed; `.secrets/` is git-ignored): comment lines
//! starting with `#` and one line with the 32-byte ed25519 seed in hex.

use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use superba_catalog::format::{
    INDEX_PATH, SIGNATURE_FORMAT, SIGNATURE_PATH, SignatureEntry, SignatureFile, to_pretty_json,
};
use superba_catalog::integrity::{key_id, signed_message};
use zeroize::Zeroizing;

/// Create a new key file at `path` (refuses to overwrite) and return the public key.
pub fn keygen(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        bail!(
            "{} exists; refusing to overwrite a signing key",
            path.display()
        );
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    let sk = SigningKey::from_bytes(&seed);
    let pk = sk.verifying_key().to_bytes();
    let body = Zeroizing::new(format!(
        "# Superba catalog signing key (ed25519 seed, hex). Private: never commit or share.\n\
         # key id: {}\n{}\n",
        key_id(&pk),
        hex_encode(seed.as_ref())
    ));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_private(path, body.as_bytes())?;
    Ok(pk)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Parse a key file's contents.
pub fn parse_key(text: &str) -> Result<SigningKey> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .context("key file has no key line")?;
    if line.len() != 64 || !line.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("key line must be 64 hex digits");
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    for (i, chunk) in line.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk)?;
        seed[i] = u8::from_str_radix(s, 16)?;
    }
    Ok(SigningKey::from_bytes(&seed))
}

/// Load a key from a file or, with `env:<NAME>`, from an environment variable (CI
/// secrets).
pub fn load_key(spec: &str) -> Result<SigningKey> {
    if let Some(name) = spec.strip_prefix("env:") {
        let v = Zeroizing::new(std::env::var(name).with_context(|| format!("${name} is not set"))?);
        return parse_key(&v);
    }
    let text =
        Zeroizing::new(std::fs::read_to_string(spec).with_context(|| format!("read {spec}"))?);
    parse_key(&text)
}

/// Sign `dist/index.json` with every key, writing `dist/index.json.sig`.
pub fn sign(catalog: &Path, keys: &[SigningKey]) -> Result<Vec<String>> {
    if keys.is_empty() {
        bail!("no signing key");
    }
    let index = std::fs::read(catalog.join(INDEX_PATH)).context("read dist/index.json")?;
    let message = signed_message(&index);
    let mut ids = Vec::new();
    let signatures = keys
        .iter()
        .map(|k| {
            let id = key_id(&k.verifying_key().to_bytes());
            ids.push(id.clone());
            SignatureEntry {
                key_id: id,
                algorithm: "ed25519".into(),
                signature: base64::engine::general_purpose::STANDARD
                    .encode(k.sign(&message).to_bytes()),
            }
        })
        .collect();
    let file = SignatureFile {
        format: SIGNATURE_FORMAT,
        signatures,
    };
    std::fs::write(catalog.join(SIGNATURE_PATH), to_pretty_json(&file)?)?;
    Ok(ids)
}
