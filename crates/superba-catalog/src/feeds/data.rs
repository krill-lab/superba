//! A verified set of feed files.

use std::collections::BTreeMap;

use superba_core::{CodexVersion, FeedOrigin, FeedSnapshot, Timestamp};

use super::FEEDS_SIGNATURE_CONTEXT;
use super::format::{
    APP_VERSIONS_FILE, AppVersionsFeed, CODEX_VERSIONS_FILE, CodexVersionsFeed, FeedIndex,
    INDEX_FILE, MODELS_FILE, ModelsFeed, PRICES_FILE, PROMPTS_FILE, PricesFeed, PromptsFeed,
    SIGNATURE_FILE, parse_feed, parse_index,
};
use crate::error::{CatalogError, Result};
use crate::integrity::{SignatureStatus, TrustedKey, sha256_hex, verify_signature_with_context};
use crate::store::TrustPolicy;

/// Largest feed file accepted (the index lists sizes; this bounds a lying index).
pub const MAX_FEED_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Largest index or signature accepted.
pub const MAX_FEED_INDEX_BYTES: u64 = 512 * 1024;

/// Feed files whose index, digests and (when required) signature were checked,
/// with every known file parsed.
#[derive(Clone, Debug)]
pub struct FeedData {
    index: FeedIndex,
    files: BTreeMap<String, Vec<u8>>,
    signature: SignatureStatus,
    models: Option<ModelsFeed>,
    prices: PricesFeed,
    codex: CodexVersionsFeed,
    app: AppVersionsFeed,
    prompts: PromptsFeed,
}

impl FeedData {
    /// Verify `files` (feeds-relative path -> bytes, including `index.json` and,
    /// when present, `index.json.sig`). `RequireSignature` needs a valid signature
    /// from `trusted` over [`FEEDS_SIGNATURE_CONTEXT`] + the index; `ManifestOnly`
    /// (the compiled-in snapshot) records the signature state but accepts a
    /// missing or invalid one. Every listed file must match its size and SHA-256
    /// and the known files must parse.
    pub fn from_files(
        files: BTreeMap<String, Vec<u8>>,
        policy: TrustPolicy,
        trusted: &[TrustedKey],
        revoked: &[&str],
    ) -> Result<FeedData> {
        let index_bytes = files
            .get(INDEX_FILE)
            .ok_or_else(|| CatalogError::integrity(INDEX_FILE, "missing"))?;
        let signature = verify_signature_with_context(
            FEEDS_SIGNATURE_CONTEXT,
            index_bytes,
            files.get(SIGNATURE_FILE).map(Vec::as_slice),
            trusted,
            revoked,
        );
        if policy == TrustPolicy::RequireSignature && !signature.is_valid() {
            return Err(CatalogError::Signature(match &signature {
                SignatureStatus::Invalid { reason } => reason.clone(),
                _ => "the feed index is not signed".into(),
            }));
        }
        let index = parse_index(index_bytes)?;
        for f in &index.files {
            if f.size > MAX_FEED_FILE_BYTES {
                return Err(CatalogError::integrity(&f.path, "file is too large"));
            }
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
            if !sha256_hex(bytes).eq_ignore_ascii_case(&f.sha256) {
                return Err(CatalogError::integrity(
                    &f.path,
                    "SHA-256 differs from the index",
                ));
            }
        }
        let listed = |path: &str| index.file(path).and_then(|_| files.get(path));
        let models = listed(MODELS_FILE)
            .map(|b| parse_feed::<ModelsFeed>(MODELS_FILE, b, |m| m.format))
            .transpose()?;
        let prices = listed(PRICES_FILE)
            .map(|b| parse_feed::<PricesFeed>(PRICES_FILE, b, |m| m.format))
            .transpose()?
            .unwrap_or_default();
        let codex = listed(CODEX_VERSIONS_FILE)
            .map(|b| parse_feed::<CodexVersionsFeed>(CODEX_VERSIONS_FILE, b, |m| m.format))
            .transpose()?
            .unwrap_or_default();
        let app = listed(APP_VERSIONS_FILE)
            .map(|b| parse_feed::<AppVersionsFeed>(APP_VERSIONS_FILE, b, |m| m.format))
            .transpose()?
            .unwrap_or_default();
        let prompts = listed(PROMPTS_FILE)
            .map(|b| parse_feed::<PromptsFeed>(PROMPTS_FILE, b, |m| m.format))
            .transpose()?
            .unwrap_or_default();
        for (version, path) in &prompts.prompts {
            CodexVersion::parse(version).map_err(|e| CatalogError::parse(PROMPTS_FILE, e))?;
            if index.file(path).is_none() {
                return Err(CatalogError::integrity(
                    path,
                    "prompt file is not listed in the index",
                ));
            }
            if std::str::from_utf8(&files[path]).is_err() {
                return Err(CatalogError::parse(path, "not UTF-8"));
            }
        }
        // Only what the index lists is kept.
        let mut kept = BTreeMap::new();
        for (path, bytes) in files {
            if path == INDEX_FILE || path == SIGNATURE_FILE || index.file(&path).is_some() {
                kept.insert(path, bytes);
            }
        }
        Ok(FeedData {
            index,
            files: kept,
            signature,
            models,
            prices,
            codex,
            app,
            prompts,
        })
    }

    /// The index.
    pub fn index(&self) -> &FeedIndex {
        &self.index
    }

    /// Index serial.
    pub fn serial(&self) -> u64 {
        self.index.serial
    }

    /// Signature state.
    pub fn signature(&self) -> &SignatureStatus {
        &self.signature
    }

    /// Every verified file (with the index and its signature).
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    /// The bytes of one file.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// The model list feed, when published.
    pub fn models(&self) -> Option<&ModelsFeed> {
        self.models.as_ref()
    }

    /// The data as a [`FeedSnapshot`].
    pub fn snapshot(&self, source: FeedOrigin, fetched_at: Option<Timestamp>) -> FeedSnapshot {
        FeedSnapshot {
            models: self
                .models
                .as_ref()
                .map(|m| m.models.clone())
                .unwrap_or_default(),
            prices: self.prices.table(),
            codex_versions: self.codex.releases.clone(),
            app_versions: self.app.releases.clone(),
            serial: self.index.serial,
            source,
            fetched_at,
        }
    }

    /// `prompt.md` of `version`: the exact version, else the nearest older one the
    /// feed has (prompts change rarely; Codex alphas carry their release's prompt).
    pub fn prompt(&self, version: &CodexVersion) -> Option<String> {
        let mut best: Option<(CodexVersion, &String)> = None;
        for (v, path) in &self.prompts.prompts {
            let Ok(v) = CodexVersion::parse(v) else {
                continue;
            };
            if &v == version {
                best = Some((v, path));
                break;
            }
            if v < *version && best.as_ref().is_none_or(|(b, _)| v > *b) {
                best = Some((v, path));
            }
        }
        let (_, path) = best?;
        self.files
            .get(path)
            .and_then(|b| String::from_utf8(b.clone()).ok())
    }
}
