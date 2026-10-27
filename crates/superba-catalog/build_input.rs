//! Canonicalize repository text before embedding it, then check the original
//! manifest. Shared with integrity tests; downloaded catalogs remain byte exact.

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct Manifest {
    files: Vec<File>,
}

#[derive(Deserialize)]
struct File {
    path: String,
    size: u64,
    sha256: String,
}

/// Git can leave CRLF files in an older Windows checkout even after `.gitattributes`
/// asks for LF. Restore only those newline bytes, without changing either manifest
/// hashes or signatures, and refuse to ship any other mismatch.
pub fn prepare_entries(index_path: &str, entries: &mut [(String, Vec<u8>)]) -> Result<(), String> {
    for (path, data) in entries.iter_mut() {
        if path.ends_with(".json") || path.ends_with(".toml") || path.ends_with(".md") {
            if data.windows(2).any(|pair| pair == b"\r\n") {
                *data = data
                    .iter()
                    .copied()
                    .enumerate()
                    .filter_map(|(i, byte)| {
                        (byte != b'\r' || data.get(i + 1) != Some(&b'\n')).then_some(byte)
                    })
                    .collect();
            }
        }
    }
    let index = entries
        .iter()
        .find(|(path, _)| path == index_path)
        .ok_or_else(|| format!("{index_path}: missing bundle index"))?;
    let manifest: Manifest =
        serde_json::from_slice(&index.1).map_err(|error| format!("{index_path}: {error}"))?;
    for file in manifest.files {
        let (_, bytes) = entries
            .iter()
            .find(|(path, _)| path == &file.path)
            .ok_or_else(|| format!("{}: listed in {index_path} but missing", file.path))?;
        if bytes.len() as u64 != file.size {
            return Err(format!(
                "{}: size {} differs from {index_path} ({}) after LF normalization",
                file.path,
                bytes.len(),
                file.size
            ));
        }
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if !digest.eq_ignore_ascii_case(&file.sha256) {
            return Err(format!(
                "{}: SHA-256 differs from {index_path} after LF normalization",
                file.path
            ));
        }
    }
    Ok(())
}
