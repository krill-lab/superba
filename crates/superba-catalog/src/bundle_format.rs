//! Container of the built-in snapshot (shared with `build.rs`, std only).
//!
//! Layout before compression: the 8-byte magic, then per file a little-endian `u32`
//! path length, the UTF-8 path, a little-endian `u64` data length and the data.
//! `build.rs` compresses it with zstd (long-distance matching: the per-version
//! files are near-duplicates of each other).

/// Magic header.
pub const MAGIC: &[u8; 8] = b"SBCATv1\0";

/// zstd window log used for compression (and the decoder's limit).
pub const WINDOW_LOG: u32 = 24;

/// Encode `(path, bytes)` pairs (used by `build.rs`).
#[allow(dead_code)]
pub fn encode(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let total: usize = entries.iter().map(|(p, d)| 12 + p.len() + d.len()).sum();
    let mut out = Vec::with_capacity(MAGIC.len() + total);
    out.extend_from_slice(MAGIC);
    for (path, data) in entries {
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// Decode what [`encode`] produced.
pub fn decode(mut bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let Some(rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return Err("bad magic".into());
    };
    bytes = rest;
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (len, rest) = take::<4>(bytes)?;
        let len = u32::from_le_bytes(len) as usize;
        let (path, rest) = split(rest, len)?;
        let path = std::str::from_utf8(path).map_err(|_| "path is not UTF-8".to_string())?;
        let (dlen, rest) = take::<8>(rest)?;
        let dlen = usize::try_from(u64::from_le_bytes(dlen)).map_err(|_| "entry too large")?;
        let (data, rest) = split(rest, dlen)?;
        out.push((path.to_owned(), data.to_vec()));
        bytes = rest;
    }
    Ok(out)
}

fn take<const N: usize>(b: &[u8]) -> Result<([u8; N], &[u8]), String> {
    let (head, rest) = split(b, N)?;
    let mut a = [0u8; N];
    a.copy_from_slice(head);
    Ok((a, rest))
}

fn split(b: &[u8], n: usize) -> Result<(&[u8], &[u8]), String> {
    if b.len() < n {
        return Err("truncated bundle".into());
    }
    Ok(b.split_at(n))
}
