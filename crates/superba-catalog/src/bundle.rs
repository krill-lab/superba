//! The built-in snapshot: the repository's `catalog/` tree packed by `build.rs`.

use std::collections::BTreeMap;
use std::io::Read as _;

use crate::bundle_format;
use crate::error::{CatalogError, Result};

static BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/catalog.bundle.zst"));
static FEEDS_BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/feeds.bundle.zst"));

/// Upper bound of the unpacked snapshot (sanity limit).
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;

/// Unpack the built-in snapshot into `catalog-relative path -> bytes`.
pub fn builtin_files() -> Result<BTreeMap<String, Vec<u8>>> {
    unpack(BUNDLE)
}

/// Unpack the built-in feeds (`catalog/feeds/`) into `feeds-relative path -> bytes`.
pub fn builtin_feed_files() -> Result<BTreeMap<String, Vec<u8>>> {
    unpack(FEEDS_BUNDLE)
}

/// Size of the compressed built-in snapshot in bytes.
pub fn builtin_compressed_size() -> usize {
    BUNDLE.len()
}

pub(crate) fn unpack(compressed: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let bad = |m: String| CatalogError::parse("built-in catalog bundle", m);
    let mut dec = zstd::stream::Decoder::new(compressed).map_err(|e| bad(e.to_string()))?;
    dec.window_log_max(bundle_format::WINDOW_LOG)
        .map_err(|e| bad(e.to_string()))?;
    let mut raw = Vec::new();
    dec.take(MAX_UNPACKED)
        .read_to_end(&mut raw)
        .map_err(|e| bad(e.to_string()))?;
    Ok(bundle_format::decode(&raw)
        .map_err(bad)?
        .into_iter()
        .collect())
}
