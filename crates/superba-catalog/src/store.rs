//! Loading a verified catalog tree and looking up the catalog of a Codex version.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use superba_core::{CodexVersion, LocalizedText, Notice};

use crate::curation::Curation;
use crate::error::{CatalogError, Result};
use crate::format::{
    CATALOG_FORMAT, CURATION_PATH, INDEX_PATH, Index, OVERLAY_DIR, Os, SIGNATURE_PATH,
    VersionCatalog,
};
use crate::integrity::{
    REVOKED_KEY_IDS, SignatureStatus, TRUSTED_KEYS, TrustedKey, parse_index, verify_files,
    verify_signature,
};
use crate::overlay::{EntryId, Explanation, Overlay};

/// Where a loaded catalog came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreOrigin {
    /// The snapshot compiled into this binary.
    BuiltIn,
    /// A verified download in the cache directory.
    Cache(PathBuf),
    /// A catalog directory (the repository's `catalog/`, tests, the generator).
    Directory(PathBuf),
}

/// Signature requirement when loading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustPolicy {
    /// A valid signature from a trusted key is required (downloaded catalogs).
    RequireSignature,
    /// The manifest must match; a missing or invalid signature is reported in
    /// [`CatalogStore::signature`] but accepted (the built-in snapshot, which is
    /// covered by the application's own code signature, and local development).
    ManifestOnly,
}

/// A verified catalog tree.
#[derive(Debug)]
pub struct CatalogStore {
    origin: StoreOrigin,
    index: Index,
    signature: SignatureStatus,
    files: BTreeMap<String, Vec<u8>>,
    curation: Curation,
    overlay: Overlay,
    /// `(version, position in index.versions)`, ascending.
    versions: Vec<(CodexVersion, usize)>,
    parsed: Mutex<BTreeMap<usize, Arc<VersionCatalog>>>,
}

impl CatalogStore {
    /// Verify and load a catalog from its files (`catalog-relative path -> bytes`).
    pub fn from_files(
        origin: StoreOrigin,
        files: BTreeMap<String, Vec<u8>>,
        policy: TrustPolicy,
    ) -> Result<CatalogStore> {
        Self::from_files_with_keys(origin, files, policy, TRUSTED_KEYS, REVOKED_KEY_IDS)
    }

    /// [`CatalogStore::from_files`] with explicit trusted and revoked keys (tests,
    /// key rotation checks).
    pub fn from_files_with_keys(
        origin: StoreOrigin,
        files: BTreeMap<String, Vec<u8>>,
        policy: TrustPolicy,
        trusted: &[TrustedKey],
        revoked: &[&str],
    ) -> Result<CatalogStore> {
        let index_bytes = files
            .get(INDEX_PATH)
            .ok_or_else(|| CatalogError::integrity(INDEX_PATH, "missing"))?;
        let signature = verify_signature(
            index_bytes,
            files.get(SIGNATURE_PATH).map(Vec::as_slice),
            trusted,
            revoked,
        );
        if policy == TrustPolicy::RequireSignature && !signature.is_valid() {
            return Err(CatalogError::Signature(match &signature {
                SignatureStatus::Unsigned => "the index is not signed".into(),
                SignatureStatus::Invalid { reason } => reason.clone(),
                SignatureStatus::Valid { .. } => String::new(),
            }));
        }
        let index = parse_index(index_bytes)?;
        verify_files(&index, &files)?;

        let text = |path: &str| -> Result<&str> {
            std::str::from_utf8(&files[path]).map_err(|_| CatalogError::parse(path, "not UTF-8"))
        };
        let curation = match index.file(CURATION_PATH) {
            Some(_) => Curation::parse(text(CURATION_PATH)?)?,
            None => Curation::default(),
        };
        let overlay_prefix = format!("{OVERLAY_DIR}/");
        let mut overlay_files = Vec::new();
        for f in &index.files {
            if f.path.starts_with(&overlay_prefix) && f.path.ends_with(".toml") {
                overlay_files.push((f.path.as_str(), text(&f.path)?));
            }
        }
        let overlay = Overlay::from_files(overlay_files)?;

        let mut versions = Vec::with_capacity(index.versions.len());
        for (i, v) in index.versions.iter().enumerate() {
            let parsed = CodexVersion::parse(&v.version)
                .map_err(|e| CatalogError::parse(INDEX_PATH, e.detail()))?;
            if parsed.to_string() != v.version {
                return Err(CatalogError::parse(
                    INDEX_PATH,
                    format!("`{}` is not a canonical version", v.version),
                ));
            }
            versions.push((parsed, i));
        }
        if versions.is_empty() {
            return Err(CatalogError::Empty);
        }
        versions.sort_by(|a, b| a.0.cmp(&b.0));
        if versions.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(CatalogError::parse(INDEX_PATH, "duplicate version"));
        }

        Ok(CatalogStore {
            origin,
            index,
            signature,
            files,
            curation,
            overlay,
            versions,
            parsed: Mutex::new(BTreeMap::new()),
        })
    }

    /// The built-in snapshot (unpacked and verified once per process).
    pub fn builtin() -> Result<Arc<CatalogStore>> {
        static BUILTIN: OnceLock<std::result::Result<Arc<CatalogStore>, String>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                crate::bundle::builtin_files()
                    .and_then(|files| {
                        CatalogStore::from_files(
                            StoreOrigin::BuiltIn,
                            files,
                            TrustPolicy::ManifestOnly,
                        )
                    })
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            })
            .clone()
            .map_err(|m| CatalogError::Invalid(format!("built-in catalog: {m}")))
    }

    /// Load a catalog directory laid out like the repository's `catalog/`: reads the
    /// index, its signature and every file the index lists.
    pub fn from_dir(root: &Path, policy: TrustPolicy) -> Result<CatalogStore> {
        Self::from_dir_with_origin(root, policy, StoreOrigin::Directory(root.to_path_buf()))
    }

    pub(crate) fn from_dir_with_origin(
        root: &Path,
        policy: TrustPolicy,
        origin: StoreOrigin,
    ) -> Result<CatalogStore> {
        let files = read_tree(root)?;
        Self::from_files(origin, files, policy)
    }

    /// Origin.
    pub fn origin(&self) -> &StoreOrigin {
        &self.origin
    }

    /// The index.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Signature state of the index.
    pub fn signature(&self) -> &SignatureStatus {
        &self.signature
    }

    /// Curation (categories, platform hints).
    pub fn curation(&self) -> &Curation {
        &self.curation
    }

    /// Merged overlays.
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// Raw bytes of a listed file.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// All files (catalog-relative path -> bytes).
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    /// Versions in the catalog, ascending.
    pub fn versions(&self) -> impl Iterator<Item = &CodexVersion> {
        self.versions.iter().map(|(v, _)| v)
    }

    /// Exact catalog of `version`, if listed.
    pub fn exact(&self, version: &CodexVersion) -> Result<Option<Arc<VersionCatalog>>> {
        match self.versions.binary_search_by(|(v, _)| v.cmp(version)) {
            Ok(pos) => self.load_version(self.versions[pos].1).map(Some),
            Err(_) => Ok(None),
        }
    }

    /// The highest stable version (or the highest version if all are prereleases).
    pub fn latest(&self) -> Result<Arc<VersionCatalog>> {
        let pos = self
            .versions
            .iter()
            .rev()
            .find(|(v, _)| v.pre.is_none())
            .or_else(|| self.versions.last())
            .map(|(_, i)| *i)
            .ok_or(CatalogError::Empty)?;
        self.load_version(pos)
    }

    /// Catalog for an installed Codex: the exact version, else the nearest listed
    /// version below it, else the oldest one. The result says which case applied.
    pub fn for_version(&self, installed: &CodexVersion) -> Result<VersionMatch> {
        let below = self.versions.partition_point(|(v, _)| v <= installed);
        let (used, pos, kind) = if below == 0 {
            let (v, i) = &self.versions[0];
            (v.clone(), *i, MatchKind::OlderThanCatalog)
        } else {
            let (v, i) = &self.versions[below - 1];
            let kind = if v == installed {
                MatchKind::Exact
            } else if below == self.versions.len() {
                MatchKind::NewerThanCatalog
            } else {
                MatchKind::Nearest
            };
            (v.clone(), *i, kind)
        };
        Ok(VersionMatch {
            installed: installed.clone(),
            used,
            kind,
            catalog: self.load_version(pos)?,
        })
    }

    fn load_version(&self, pos: usize) -> Result<Arc<VersionCatalog>> {
        if let Some(c) = self.parsed.lock().map_err(poisoned)?.get(&pos) {
            return Ok(c.clone());
        }
        let entry = &self.index.versions[pos];
        let bytes = self
            .files
            .get(&entry.path)
            .ok_or_else(|| CatalogError::integrity(&entry.path, "missing"))?;
        let vc: VersionCatalog = serde_json::from_slice(bytes)
            .map_err(|e| CatalogError::parse(entry.path.clone(), e))?;
        if vc.catalog_format != CATALOG_FORMAT {
            return Err(CatalogError::UnsupportedFormat {
                what: "catalog",
                found: vc.catalog_format,
                supported: CATALOG_FORMAT,
            });
        }
        if vc.codex_version != entry.version {
            return Err(CatalogError::integrity(
                &entry.path,
                format!("holds {} instead of {}", vc.codex_version, entry.version),
            ));
        }
        let vc = Arc::new(vc);
        self.parsed
            .lock()
            .map_err(poisoned)?
            .insert(pos, vc.clone());
        Ok(vc)
    }

    /// Explanation of a feature in `catalog`.
    pub fn explain_feature(&self, catalog: &VersionCatalog, key: &str) -> Explanation {
        let f = catalog.feature(key);
        let exp = f.and_then(|f| f.experimental.as_ref());
        Explanation::new(
            exp.map(|e| e.name.as_str()),
            exp.map(|e| e.menu_description.as_str()),
            self.overlay.get(&EntryId::Feature(key.to_owned())),
        )
    }

    /// Explanation of a config key in `catalog`.
    pub fn explain_key(&self, catalog: &VersionCatalog, path: &str) -> Explanation {
        Explanation::new(
            None,
            catalog.key(path).and_then(|k| k.description.as_deref()),
            self.overlay.get(&EntryId::Config(path.to_owned())),
        )
    }

    /// Explanation of one value of an enum key in `catalog`.
    pub fn explain_value(&self, catalog: &VersionCatalog, path: &str, value: &str) -> Explanation {
        let desc = catalog.key(path).and_then(|k| {
            k.enum_values
                .iter()
                .find(|v| v.value == value)
                .and_then(|v| v.description.as_deref())
        });
        Explanation::new(
            None,
            desc,
            self.overlay.get(&EntryId::Value {
                path: path.to_owned(),
                value: value.to_owned(),
            }),
        )
    }

    /// Coverage of the overlays against the latest version (stages as on `os`).
    pub fn coverage(&self, os: Os) -> Result<crate::coverage::Coverage> {
        let latest = self.latest()?;
        Ok(crate::coverage::Coverage::compute(
            &self.overlay,
            Some(&latest),
            os,
        ))
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> CatalogError {
    CatalogError::Invalid("catalog cache lock poisoned".into())
}

/// Read `dist/index.json`, its signature and every file the index lists below `root`.
pub fn read_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let read = |rel: &str| -> Result<Vec<u8>> {
        let p = root.join(rel);
        std::fs::read(&p).map_err(|e| CatalogError::io(&p, &e))
    };
    let mut files = BTreeMap::new();
    let index_bytes = read(INDEX_PATH)?;
    let index = parse_index(&index_bytes)?;
    files.insert(INDEX_PATH.to_owned(), index_bytes);
    match std::fs::read(root.join(SIGNATURE_PATH)) {
        Ok(b) => {
            files.insert(SIGNATURE_PATH.to_owned(), b);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(CatalogError::io(root.join(SIGNATURE_PATH), &e)),
    }
    for f in &index.files {
        crate::integrity::validate_relative_path(&f.path)?;
        files.insert(f.path.clone(), read(&f.path)?);
    }
    Ok(files)
}

/// How the catalog version relates to the installed Codex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MatchKind {
    /// The catalog has exactly this version.
    Exact,
    /// The nearest older version of the catalog is used; newer catalog versions exist
    /// (an unlisted prerelease or patch release).
    Nearest,
    /// The installed Codex is newer than every catalog version: the latest is used
    /// and new settings may be missing.
    NewerThanCatalog,
    /// The installed Codex is older than every catalog version: the oldest is used.
    OlderThanCatalog,
}

/// Result of [`CatalogStore::for_version`].
#[derive(Clone, Debug)]
pub struct VersionMatch {
    /// Installed Codex version.
    pub installed: CodexVersion,
    /// Catalog version used.
    pub used: CodexVersion,
    /// Relation between both.
    pub kind: MatchKind,
    /// The catalog of [`VersionMatch::used`].
    pub catalog: Arc<VersionCatalog>,
}

impl VersionMatch {
    /// What to tell the user, if anything (`catalog.match.*`).
    pub fn notice(&self) -> Option<Notice> {
        let text = |key: &str, fallback: &str| {
            LocalizedText::new(format!("catalog.match.{key}"), fallback)
                .arg("installed", &self.installed)
                .arg("used", &self.used)
        };
        match self.kind {
            MatchKind::Exact => None,
            MatchKind::Nearest => Some(Notice::info(text(
                "nearest",
                "Settings for Codex {installed} come from the catalog of Codex {used}.",
            ))),
            MatchKind::NewerThanCatalog => Some(Notice::warning(text(
                "newer",
                "Codex {installed} is newer than the settings catalog (latest: {used}); \
                 settings added since then are not listed.",
            ))),
            MatchKind::OlderThanCatalog => Some(Notice::warning(text(
                "older",
                "Codex {installed} is older than the settings catalog; showing Codex \
                 {used}, so some settings may not apply.",
            ))),
        }
    }
}

/// A catalog chosen by [`load`], with notices for the user.
#[derive(Debug)]
pub struct Loaded {
    /// The catalog to use.
    pub store: Arc<CatalogStore>,
    /// Why a downloaded catalog was not used, and similar.
    pub notices: Vec<Notice>,
}

/// The catalog to use: a verified, signed download in `cache_dir` when its serial is
/// higher than the built-in snapshot's, else the built-in snapshot.
pub fn load(cache_dir: Option<&Path>) -> Result<Loaded> {
    let builtin = CatalogStore::builtin()?;
    let mut notices = Vec::new();
    if let Some(dir) = cache_dir {
        let tree = crate::remote::cache_tree(dir);
        if tree.join(INDEX_PATH).is_file() {
            match CatalogStore::from_dir_with_origin(
                &tree,
                TrustPolicy::RequireSignature,
                StoreOrigin::Cache(tree.clone()),
            ) {
                Ok(cached) if cached.index.serial > builtin.index.serial => {
                    return Ok(Loaded {
                        store: Arc::new(cached),
                        notices,
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "ignoring the downloaded settings catalog");
                    notices.push(Notice::warning(
                        LocalizedText::new(
                            "catalog.cache.rejected",
                            "The downloaded settings catalog failed verification and is \
                             not used: {error}",
                        )
                        .arg("error", e),
                    ));
                }
            }
        }
    }
    Ok(Loaded {
        store: builtin,
        notices,
    })
}
