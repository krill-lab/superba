//! v0.3 fakes: the version store, feeds and the model catalog generator.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use superba_core::{
    AppDownloadReport, AppPackage, AppRelease, CodexRelease, CodexVersion, Error, FeedSnapshot,
    FeedSource, FeedsStatus, InstallReport, InstallTarget, ModelList, ModelOverride, ModelSpec,
    PriceTable, Result, StandaloneLayout, StoredCodex, VersionStore, async_trait,
};
use superba_models::{BundledCatalog, CatalogFile, CatalogInput, CatalogSource, OverrideCheck};
use superba_switch::models::CatalogBuilder;

use super::{Log, push};

// ---------------------------------------------------------------------------
// Version store
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeVersions {
    pub log: Log,
    pub store_dir: PathBuf,
    pub stored: Mutex<Vec<StoredCodex>>,
    pub releases: Mutex<Vec<CodexRelease>>,
    pub fail_releases: Mutex<Option<Error>>,
    pub app_latest: Mutex<Option<AppRelease>>,
    pub fail_ensure: Mutex<Option<Error>>,
}

impl FakeVersions {
    pub fn new(log: Log, store_dir: PathBuf) -> Self {
        Self {
            log,
            store_dir,
            stored: Mutex::new(Vec::new()),
            releases: Mutex::new(Vec::new()),
            fail_releases: Mutex::new(None),
            app_latest: Mutex::new(None),
            fail_ensure: Mutex::new(None),
        }
    }

    pub fn executable(&self, v: &CodexVersion) -> PathBuf {
        self.store_dir
            .join(format!("{v}-x86_64-unknown-linux-musl"))
            .join("bin")
            .join("codex")
    }

    pub fn add_stored(&self, v: CodexVersion) -> PathBuf {
        let exe = self.executable(&v);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        self.stored.lock().unwrap().push(StoredCodex {
            version: v,
            target: "x86_64-unknown-linux-musl".into(),
            executable: exe.clone(),
            size_bytes: None,
            installed_at: None,
            in_use: false,
            pinned: false,
        });
        exe
    }
}

#[async_trait]
impl VersionStore for FakeVersions {
    async fn codex_releases(&self, _refresh: bool) -> Result<Vec<CodexRelease>> {
        if let Some(error) = self.fail_releases.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.releases.lock().unwrap().clone())
    }

    async fn app_latest(&self, _refresh: bool) -> Result<Option<AppRelease>> {
        Ok(self.app_latest.lock().unwrap().clone())
    }

    async fn stored(&self) -> Result<Vec<StoredCodex>> {
        Ok(self.stored.lock().unwrap().clone())
    }

    async fn ensure(&self, version: &CodexVersion) -> Result<StoredCodex> {
        push(&self.log, format!("versions.ensure {version}"));
        if let Some(e) = self.fail_ensure.lock().unwrap().clone() {
            return Err(e);
        }
        if let Some(s) = self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|s| &s.version == version)
        {
            return Ok(s.clone());
        }
        self.add_stored(version.clone());
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|s| &s.version == version)
            .cloned()
            .unwrap())
    }

    async fn package_path(&self, version: &CodexVersion, target: &str) -> Result<PathBuf> {
        push(&self.log, format!("versions.package {version} {target}"));
        Ok(self
            .store_dir
            .join(format!("codex-package-{version}-{target}.tar.gz")))
    }

    async fn download_app(&self, package: &AppPackage) -> Result<AppDownloadReport> {
        push(&self.log, format!("versions.download_app {}", package.url));
        Ok(AppDownloadReport {
            package: package.clone(),
            path: self.store_dir.join("chatgpt.deb"),
            sha256: "00".repeat(32),
            command: None,
            windows: None,
        })
    }

    async fn install_standalone(
        &self,
        version: &CodexVersion,
        layout: &StandaloneLayout,
    ) -> Result<InstallReport> {
        push(&self.log, format!("versions.install_standalone {version}"));
        Ok(InstallReport {
            version: version.clone(),
            target: InstallTarget::Local,
            executable: Some(layout.command().display().to_string()),
            previous: None,
            notes: Vec::new(),
        })
    }

    async fn rollback_standalone(&self, layout: &StandaloneLayout) -> Result<InstallReport> {
        push(&self.log, "versions.rollback_standalone".to_owned());
        Ok(InstallReport {
            version: CodexVersion::new(0, 150, 1),
            target: InstallTarget::Local,
            executable: Some(layout.command().display().to_string()),
            previous: Some(CodexVersion::new(0, 162, 0)),
            notes: Vec::new(),
        })
    }

    async fn prune(
        &self,
        _: &[superba_core::CodexVersion],
    ) -> superba_core::Result<Vec<superba_core::CodexVersion>> {
        Err(superba_core::Error::not_implemented("VersionStore::prune"))
    }
}

// ---------------------------------------------------------------------------
// Feeds
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct FakeFeeds {
    pub models: Mutex<Vec<ModelSpec>>,
    pub prices: Mutex<PriceTable>,
    pub refreshes: Mutex<u32>,
}

#[async_trait]
impl FeedSource for FakeFeeds {
    async fn snapshot(&self, refresh: bool) -> Result<FeedSnapshot> {
        if refresh {
            *self.refreshes.lock().unwrap() += 1;
        }
        Ok(FeedSnapshot {
            models: self.models.lock().unwrap().clone(),
            prices: self.prices.lock().unwrap().clone(),
            serial: 7,
            ..FeedSnapshot::default()
        })
    }

    async fn prompt(&self, _version: &CodexVersion) -> Result<Option<String>> {
        Ok(None)
    }

    async fn status(&self) -> Result<FeedsStatus> {
        Ok(FeedsStatus {
            serial: 7,
            builtin_serial: 7,
            online_checks: true,
            ..FeedsStatus::default()
        })
    }
}

// ---------------------------------------------------------------------------
// Catalog generator
// ---------------------------------------------------------------------------

/// What the fake generator was asked to build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildCall {
    pub consumer: CodexVersion,
    pub source: &'static str,
    pub slugs: Vec<String>,
    pub overrides: BTreeMap<String, ModelOverride>,
    pub speed: superba_core::SpeedTier,
    pub suffix: bool,
    pub model: Option<String>,
}

#[derive(Debug, Default)]
pub struct FakeBuilder {
    pub calls: Mutex<Vec<BuildCall>>,
    pub fail_build: Mutex<Option<Error>>,
}

impl CatalogBuilder for FakeBuilder {
    fn parse_bundled(&self, output: &[u8]) -> Result<BundledCatalog> {
        let v: serde_json::Value =
            serde_json::from_slice(output).map_err(|e| Error::parse("bundled", e.to_string()))?;
        Ok(BundledCatalog {
            codex_version: None,
            entries: v
                .get("models")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default(),
            required_fields: vec!["slug".into()],
        })
    }

    fn build(&self, input: &CatalogInput<'_>) -> Result<CatalogFile> {
        if let Some(e) = self.fail_build.lock().unwrap().clone() {
            return Err(e);
        }
        let (source, mut slugs): (&'static str, Vec<String>) = match &input.source {
            CatalogSource::Native(m) => ("native", m.iter().map(|s| s.slug.clone()).collect()),
            CatalogSource::OpenAiList(m) => ("openai", m.iter().map(|s| s.slug.clone()).collect()),
            CatalogSource::Feed(m) => ("feed", m.iter().map(|s| s.slug.clone()).collect()),
            _ => (
                "bundled",
                input
                    .template
                    .entries
                    .iter()
                    .filter_map(|e| e.get("slug").and_then(|s| s.as_str()).map(str::to_owned))
                    .collect(),
            ),
        };
        if let Some(m) = input.model
            && !slugs.iter().any(|s| s == m)
        {
            slugs.push(m.to_owned());
        }
        self.calls.lock().unwrap().push(BuildCall {
            consumer: input.consumer.clone(),
            source,
            slugs: slugs.clone(),
            overrides: input.overrides.clone(),
            speed: input.speed,
            suffix: input.tier_model_suffix,
            model: input.model.map(str::to_owned),
        });
        let bytes = serde_json::to_vec(&serde_json::json!({
            "models": slugs.iter().map(|s| serde_json::json!({"slug": s})).collect::<Vec<_>>(),
            "overrides": input.overrides,
            "speed": input.speed,
            // Like the real builder: the consumer's template shapes the file.
            "consumer": input.consumer.to_string(),
        }))
        .unwrap();
        let sha8 = hex::encode(Sha256::digest(&bytes))[..8].to_owned();
        Ok(CatalogFile {
            bytes,
            sha8,
            slugs,
            notes: Vec::new(),
        })
    }

    fn validate_override(
        &self,
        spec: &ModelSpec,
        model_override: &ModelOverride,
    ) -> Result<OverrideCheck> {
        superba_switch::models::check_override(Some(spec), model_override)
    }

    fn union(&self, lists: &[(String, ModelList)]) -> Result<ModelList> {
        let mut first = lists
            .first()
            .map(|(_, l)| l.clone())
            .ok_or_else(|| Error::invalid("no lists"))?;
        for (_, l) in lists.iter().skip(1) {
            for m in &l.models {
                if first.model(&m.slug).is_none() {
                    first.models.push(m.clone());
                }
            }
        }
        Ok(first)
    }
}

/// A bundled dump with these slugs (each offering Fast; `astra` also Ultrafast
/// when `ultra`).
pub fn bundled_dump(slugs: &[&str], ultra: bool) -> String {
    serde_json::json!({
        "models": slugs.iter().map(|s| {
            let mut tiers = vec![serde_json::json!({"id": "priority", "name": "Fast", "description": ""})];
            if ultra && s.contains("astra") {
                tiers.push(serde_json::json!({"id": "ultrafast", "name": "Ultrafast", "description": ""}));
            }
            serde_json::json!({
                "slug": s, "display_name": s, "context_window": 272000,
                "max_context_window": 872000, "service_tiers": tiers,
                "visibility": "list", "supported_in_api": true
            })
        }).collect::<Vec<_>>()
    })
    .to_string()
}

/// The path of a file under `dir` (helper for assertions).
pub fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Shareable fake set (cloned into the factories).
#[derive(Clone)]
pub struct V3 {
    pub versions: Arc<FakeVersions>,
    pub feeds: Arc<FakeFeeds>,
    pub builder: Arc<FakeBuilder>,
}
