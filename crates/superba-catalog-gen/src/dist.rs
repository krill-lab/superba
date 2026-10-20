//! `catalog/dist/`: version selection, per-version files, change summaries, index.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use superba_catalog::VersionCatalog;
use superba_catalog::format::{
    self, CATALOG_FORMAT, CURATION_PATH, INDEX_FORMAT, INDEX_PATH, Index, IndexFile, IndexVersion,
    OVERLAY_DIR, SIGNATURE_PATH, to_pretty_json,
};
use superba_catalog::integrity::sha256_hex;
use superba_core::CodexVersion;

use crate::diff;
use crate::extract::{self, REPO};
use crate::git::GitRepo;

/// `catalog/sources.toml`: which upstream versions the catalog contains.
#[derive(Clone, Debug, Deserialize)]
pub struct Sources {
    pub format: u32,
    pub repo: String,
    /// Oldest version included.
    pub floor: String,
    /// Include every prerelease tag >= floor.
    #[serde(default)]
    pub prereleases: bool,
    /// Include the newest prerelease when it is newer than every stable release
    /// (it rolls forward: the previous alpha drops out, and the stable release
    /// replaces it once tagged).
    #[serde(default)]
    pub latest_prerelease: bool,
    /// Prereleases included anyway (for example the ones the desktop app bundles).
    #[serde(default)]
    pub extra_versions: Vec<String>,
    /// Versions never included.
    #[serde(default)]
    pub exclude_versions: Vec<String>,
}

impl Sources {
    pub fn load(catalog: &Path) -> Result<Sources> {
        let p = catalog.join("sources.toml");
        let s: Sources = toml::from_str(
            &std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?,
        )
        .with_context(|| format!("parse {}", p.display()))?;
        if s.format != 1 {
            bail!("{}: unsupported format {}", p.display(), s.format);
        }
        if s.repo != REPO {
            bail!("{}: repo must be {REPO}", p.display());
        }
        Ok(s)
    }

    /// Resolve `latest_prerelease` against the repository's release versions: the
    /// newest prerelease above every stable release becomes an extra version.
    pub fn resolve(mut self, all: &[CodexVersion]) -> Result<Sources> {
        if !self.latest_prerelease {
            return Ok(self);
        }
        let floor = CodexVersion::parse(&self.floor).map_err(|e| anyhow::anyhow!("floor: {e}"))?;
        let newest_stable = all.iter().filter(|v| v.pre.is_none()).max();
        let newest = all
            .iter()
            .filter(|v| !self.exclude_versions.contains(&v.to_string()))
            .max();
        if let Some(n) = newest
            && n.pre.is_some()
            && *n >= floor
            && newest_stable.is_none_or(|s| n > s)
            && !self.extra_versions.contains(&n.to_string())
        {
            self.extra_versions.push(n.to_string());
        }
        Ok(self)
    }

    /// Whether `v` belongs in the catalog.
    pub fn selects(&self, v: &CodexVersion) -> Result<bool> {
        let floor = CodexVersion::parse(&self.floor).map_err(|e| anyhow::anyhow!("floor: {e}"))?;
        let s = v.to_string();
        if self.exclude_versions.contains(&s) {
            return Ok(false);
        }
        if self.extra_versions.contains(&s) {
            return Ok(true);
        }
        Ok(*v >= floor && (v.pre.is_none() || self.prereleases))
    }
}

/// The selected `(version, tag)` pairs of a repository, ascending.
pub fn selected_tags(sources: &Sources, repo: &GitRepo) -> Result<Vec<(CodexVersion, String)>> {
    let all = repo.release_tags()?;
    let versions: Vec<CodexVersion> = all.iter().map(|(v, _)| v.clone()).collect();
    let sources = &sources.clone().resolve(&versions)?;
    let mut out = Vec::new();
    for (v, tag) in all {
        if sources.selects(&v)? {
            out.push((v, tag));
        }
    }
    for extra in &sources.extra_versions {
        if !out.iter().any(|(v, _)| v.to_string() == *extra) {
            bail!("extra version {extra} has no tag in the repository");
        }
    }
    Ok(out)
}

fn dist_dir(catalog: &Path) -> PathBuf {
    catalog.join("dist")
}

/// Version files present in `dist/`, ascending.
pub fn dist_versions(catalog: &Path) -> Result<Vec<(CodexVersion, PathBuf)>> {
    let mut out = Vec::new();
    let dir = dist_dir(catalog);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(v) = name
            .strip_prefix("codex-")
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };
        let parsed = CodexVersion::parse(v).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        if parsed.to_string() != v {
            bail!("{name}: not a canonical version");
        }
        out.push((parsed, e.path()));
    }
    out.sort();
    Ok(out)
}

pub fn load_version(path: &Path) -> Result<VersionCatalog> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let vc: VersionCatalog =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    if vc.catalog_format != CATALOG_FORMAT {
        bail!("{}: catalog format {}", path.display(), vc.catalog_format);
    }
    Ok(vc)
}

/// Write `contents` to `path` only if it differs; returns true when written.
pub fn write_if_changed(path: &Path, contents: &[u8]) -> Result<bool> {
    if std::fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

pub struct BuildReport {
    pub written: Vec<String>,
    pub unchanged: Vec<String>,
    pub removed: Vec<String>,
}

/// Regenerate `dist/` from `repo`: every selected version (or only versions whose
/// file is missing or was built from another commit, with `only_new`), then change
/// summaries and the index.
pub fn build(catalog: &Path, repo: &GitRepo, only_new: bool) -> Result<BuildReport> {
    let sources = Sources::load(catalog)?;
    let tags = selected_tags(&sources, repo)?;
    if tags.is_empty() {
        bail!(
            "no release tag >= {} in {}",
            sources.floor,
            repo.path.display()
        );
    }
    let mut report = BuildReport {
        written: Vec::new(),
        unchanged: Vec::new(),
        removed: Vec::new(),
    };
    let existing: BTreeMap<CodexVersion, PathBuf> = dist_versions(catalog)?.into_iter().collect();
    for (v, tag) in &tags {
        let path = dist_dir(catalog).join(format::version_file_name(&v.to_string()));
        if only_new
            && let Some(p) = existing.get(v)
            && let Ok(old) = load_version(p)
            && old.source.commit == repo.peel(tag)?
        {
            report.unchanged.push(v.to_string());
            continue;
        }
        let src = repo.read_sources(tag)?;
        let vc = extract::extract(v, &src).with_context(|| format!("extract {tag}"))?;
        if write_if_changed(&path, to_pretty_json(&vc)?.as_bytes())? {
            report.written.push(v.to_string());
        } else {
            report.unchanged.push(v.to_string());
        }
    }
    for (v, p) in &existing {
        if !tags.iter().any(|(t, _)| t == v) {
            std::fs::remove_file(p)?;
            report.removed.push(v.to_string());
        }
    }
    write_changes(catalog)?;
    Ok(report)
}

/// The version a change summary compares against: the highest stable version below
/// `v` (release branches make "the previous prerelease" meaningless).
pub fn base_of<'a>(
    v: &CodexVersion,
    all: &'a [(CodexVersion, PathBuf)],
) -> Option<&'a (CodexVersion, PathBuf)> {
    all.iter().rev().find(|(x, _)| x < v && x.pre.is_none())
}

/// Rewrite `dist/changes/codex-<v>.md` for every version and drop stale ones.
pub fn write_changes(catalog: &Path) -> Result<()> {
    let all = dist_versions(catalog)?;
    let dir = dist_dir(catalog).join("changes");
    std::fs::create_dir_all(&dir)?;
    let mut keep = Vec::new();
    for (v, p) in &all {
        let vc = load_version(p)?;
        let body = match base_of(v, &all) {
            Some((bv, bp)) => {
                let base = load_version(bp)?;
                diff::markdown(&diff::diff(&base, &vc), &format!("Codex {bv} → {v}"))
            }
            None => format!(
                "### Codex {v}\n\n- First version in the catalog: {} config keys, {} features.\n",
                vc.config_keys.len(),
                vc.features.len()
            ),
        };
        let name = format!("codex-{v}.md");
        let header = format!(
            "<!-- Generated by superba-catalog-gen from {} ({}). Do not edit. -->\n\n",
            vc.source.tag, vc.source.commit
        );
        write_if_changed(&dir.join(&name), format!("{header}{body}").as_bytes())?;
        keep.push(name);
    }
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".md") && !keep.contains(&n) {
            std::fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// Files listed in the index (catalog-relative), sorted.
pub fn listed_files(catalog: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    if catalog.join(CURATION_PATH).is_file() {
        files.push(CURATION_PATH.to_owned());
    }
    if let Ok(rd) = std::fs::read_dir(catalog.join(OVERLAY_DIR)) {
        for e in rd {
            let n = e?.file_name().to_string_lossy().into_owned();
            if n.ends_with(".toml") {
                files.push(format!("{OVERLAY_DIR}/{n}"));
            }
        }
    }
    for (v, _) in dist_versions(catalog)? {
        files.push(format::version_path(&v.to_string()));
    }
    files.sort();
    Ok(files)
}

/// Compute the index of the tree at `catalog`; the serial increases when the
/// files or versions differ from `previous`.
pub fn compute_index(catalog: &Path, previous: Option<&Index>) -> Result<Index> {
    let mut files = Vec::new();
    for rel in listed_files(catalog)? {
        let bytes = std::fs::read(catalog.join(&rel)).with_context(|| format!("read {rel}"))?;
        files.push(IndexFile {
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
            path: rel,
        });
    }
    let mut versions = Vec::new();
    let mut latest: Option<CodexVersion> = None;
    for (v, p) in dist_versions(catalog)? {
        let vc = load_version(&p)?;
        if vc.codex_version != v.to_string() {
            bail!("{} holds {}", p.display(), vc.codex_version);
        }
        if v.pre.is_none() {
            latest = Some(v.clone());
        }
        versions.push(IndexVersion {
            version: v.to_string(),
            tag: vc.source.tag.clone(),
            commit: vc.source.commit.clone(),
            prerelease: v.pre.is_some(),
            path: format::version_path(&v.to_string()),
            changes: format::changes_path(&v.to_string()),
        });
    }
    let Some(latest) = latest.or_else(|| {
        versions
            .last()
            .and_then(|v| CodexVersion::parse(&v.version).ok())
    }) else {
        bail!("dist/ has no version files");
    };
    let mut index = Index {
        index_format: INDEX_FORMAT,
        catalog_format: CATALOG_FORMAT,
        serial: previous.map_or(1, |p| p.serial),
        repo: REPO.to_owned(),
        latest: latest.to_string(),
        versions,
        files,
    };
    if let Some(p) = previous {
        let same =
            p.files == index.files && p.versions == index.versions && p.latest == index.latest;
        if !same {
            index.serial = p.serial + 1;
        }
    }
    Ok(index)
}

pub fn read_index(catalog: &Path) -> Result<Option<Index>> {
    match std::fs::read(catalog.join(INDEX_PATH)) {
        Ok(b) => Ok(Some(
            superba_catalog::integrity::parse_index(&b).map_err(|e| anyhow::anyhow!("{e}"))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Rewrite `dist/index.json`; when it changes, a now-stale signature is removed.
/// Returns true when the index changed.
pub fn write_index(catalog: &Path) -> Result<bool> {
    let previous = read_index(catalog)?;
    let index = compute_index(catalog, previous.as_ref())?;
    let changed = write_if_changed(
        &catalog.join(INDEX_PATH),
        to_pretty_json(&index)?.as_bytes(),
    )?;
    let sig = catalog.join(SIGNATURE_PATH);
    if changed && sig.exists() {
        std::fs::remove_file(&sig)?;
        eprintln!(
            "note: dist/index.json changed; removed the stale signature (run `superba-catalog-gen sign`)"
        );
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(latest: bool) -> Sources {
        Sources {
            format: 1,
            repo: REPO.into(),
            floor: "0.155.0".into(),
            prereleases: false,
            latest_prerelease: latest,
            extra_versions: vec![],
            exclude_versions: vec![],
        }
    }

    fn vs(v: &[&str]) -> Vec<CodexVersion> {
        v.iter().map(|s| CodexVersion::parse(s).unwrap()).collect()
    }

    #[test]
    fn latest_prerelease_rolls_forward() {
        let all = vs(&["0.163.0", "0.164.0-alpha.6", "0.164.0-alpha.7"]);
        let s = src(true).resolve(&all).unwrap();
        assert!(s.selects(&all[2]).unwrap());
        assert!(!s.selects(&all[1]).unwrap());
        assert!(!src(false).resolve(&all).unwrap().selects(&all[2]).unwrap());
        // Once the stable release is tagged, no prerelease is added.
        let all = vs(&["0.163.0", "0.164.0-alpha.7", "0.164.0"]);
        let s = src(true).resolve(&all).unwrap();
        assert!(!s.selects(&all[1]).unwrap());
        assert!(s.extra_versions.is_empty());
    }
}
