//! `models.json`, `prompts.json` and `prompts/<sha8>.md` from release tags.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use superba_catalog::feeds::format::{
    FEEDS_FORMAT, MODELS_FILE, ModelsFeed, PROMPTS_DIR, PROMPTS_FILE, PromptsFeed, to_pretty_json,
};
use superba_catalog::integrity::sha256_hex;
use superba_core::{CodexVersion, ModelSpec};

use crate::dist::{Sources, write_if_changed};
use crate::git::GitRepo;

/// Release file with the bundled model list.
pub const MODELS_JSON: &str = "codex-rs/models-manager/models.json";
/// Release file with the generic model instructions.
pub const PROMPT_MD: &str = "codex-rs/models-manager/prompt.md";
/// Hand-written entries (third-party models), people-maintained.
pub const MANUAL_MODELS: &str = "models-extra.json";

#[derive(Deserialize)]
struct Manual {
    format: u32,
    #[serde(default)]
    models: Vec<ModelSpec>,
}

/// Versions whose prompt the feed carries: every stable release from `floor`
/// plus the catalog's extra versions (app-bundled builds).
fn prompt_tags(
    sources: &Sources,
    repo: &GitRepo,
    floor: &CodexVersion,
) -> Result<Vec<(CodexVersion, String)>> {
    let mut out = Vec::new();
    for (v, tag) in repo.release_tags()? {
        let s = v.to_string();
        if sources.exclude_versions.contains(&s) {
            continue;
        }
        if (v.pre.is_none() && v >= *floor) || sources.extra_versions.contains(&s) {
            out.push((v, tag));
        }
    }
    Ok(out)
}

/// The OpenAI entries of a release's `models.json` (metadata only).
fn release_models(repo: &GitRepo, tag: &str) -> Result<Vec<ModelSpec>> {
    let Some(bytes) = repo.read_file(tag, MODELS_JSON)? else {
        return Ok(Vec::new());
    };
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("{MODELS_JSON} at {tag}"))?;
    let items = v["models"]
        .as_array()
        .with_context(|| format!("{MODELS_JSON} at {tag} has no models"))?;
    Ok(items
        .iter()
        .filter_map(ModelSpec::from_codex_entry)
        .map(|mut m| {
            m.raw = None;
            m
        })
        .collect())
}

/// Report of [`write`].
pub struct ModelsReport {
    pub codex_version: CodexVersion,
    pub models: usize,
    pub prompts: usize,
    pub prompt_files: usize,
}

/// Regenerate `models.json` (newest stable release first, then models only older
/// selected releases list, then the hand-written entries) and the prompts.
pub fn write(catalog: &Path, repo: &GitRepo, prompt_floor: &CodexVersion) -> Result<ModelsReport> {
    let feeds = catalog.join("feeds");
    let sources = Sources::load(catalog)?;
    let mut tags = crate::dist::selected_tags(&sources, repo)?;
    tags.reverse();
    let Some((newest, _)) = tags.iter().find(|(v, _)| v.pre.is_none()).cloned() else {
        bail!("no stable release selected");
    };
    let mut models: Vec<ModelSpec> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut used = Vec::new();
    for (v, tag) in tags.iter().filter(|(v, _)| v.pre.is_none()) {
        let before = models.len();
        for m in release_models(repo, tag)? {
            if seen.insert(m.slug.clone()) {
                models.push(m);
            }
        }
        if models.len() > before || *v == newest {
            used.push(format!("openai/codex {tag}: {MODELS_JSON}"));
        }
    }
    let manual_path = catalog.join(MANUAL_MODELS);
    if manual_path.is_file() {
        let manual: Manual = serde_json::from_slice(&std::fs::read(&manual_path)?)
            .with_context(|| format!("parse {}", manual_path.display()))?;
        if manual.format != FEEDS_FORMAT {
            bail!(
                "{}: unsupported format {}",
                manual_path.display(),
                manual.format
            );
        }
        for mut m in manual.models {
            m.raw = None;
            if seen.insert(m.slug.clone()) {
                models.push(m);
            } else {
                eprintln!(
                    "note: {MANUAL_MODELS}: {} is an OpenAI model; skipped",
                    m.slug
                );
            }
        }
        used.push(format!("catalog/{MANUAL_MODELS}"));
    }
    let feed = ModelsFeed {
        format: FEEDS_FORMAT,
        codex_version: newest.clone(),
        sources: used,
        models,
    };
    std::fs::create_dir_all(&feeds)?;
    write_if_changed(&feeds.join(MODELS_FILE), to_pretty_json(&feed)?.as_bytes())?;

    // Prompts, deduplicated by content.
    let mut prompts = BTreeMap::new();
    let mut files = BTreeMap::new();
    for (v, tag) in prompt_tags(&sources, repo, prompt_floor)? {
        let Some(bytes) = repo.read_file(&tag, PROMPT_MD)? else {
            continue;
        };
        std::str::from_utf8(&bytes).with_context(|| format!("{PROMPT_MD} at {tag}"))?;
        let name = format!("{PROMPTS_DIR}/{}.md", &sha256_hex(&bytes)[..8]);
        prompts.insert(v.to_string(), name.clone());
        files.insert(name, bytes);
    }
    let dir = feeds.join(PROMPTS_DIR);
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in &files {
        write_if_changed(&feeds.join(name), bytes)?;
    }
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let rel = format!("{PROMPTS_DIR}/{}", e.file_name().to_string_lossy());
        if !files.contains_key(&rel) {
            std::fs::remove_file(e.path())?;
        }
    }
    let pf = PromptsFeed {
        format: FEEDS_FORMAT,
        prompts,
    };
    write_if_changed(&feeds.join(PROMPTS_FILE), to_pretty_json(&pf)?.as_bytes())?;
    Ok(ModelsReport {
        codex_version: newest,
        models: feed.models.len(),
        prompts: pf.prompts.len(),
        prompt_files: files.len(),
    })
}
