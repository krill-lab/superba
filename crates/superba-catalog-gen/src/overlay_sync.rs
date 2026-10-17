//! `overlay-sync`: keep `catalog/overlay/*.toml` in step with `dist/`.
//!
//! Generator-owned fields (`en`, `en_name`, `en_source`, `en_hash`) are rewritten
//! from the newest catalog version that has the id; human fields (`zh`, `zh_name`,
//! `en_note`, `zh_basis`) are only ever copied from the existing files. The Chinese
//! text digest is checked before and after so a bug here can never create or alter
//! Chinese text.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use superba_catalog::curation::Curation;
use superba_catalog::format::{CURATION_PATH, OVERLAY_DIR, sources};
use superba_catalog::integrity::sha256_hex;
use superba_catalog::overlay::{OVERLAY_FORMAT, OverlayEntry, OverlayFile, text_hash};

use crate::dist::{dist_versions, load_version, write_if_changed};

/// File of feature entries.
pub const FEATURES_FILE: &str = "features.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Desired {
    file: String,
    en_name: Option<String>,
    en: String,
    /// Tag of the first catalog version with the current text, and the source file.
    since: Option<(String, &'static str)>,
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Desired entries (id -> generator fields) from every version in `dist/`.
fn desired(catalog: &Path, curation: &Curation) -> Result<(BTreeMap<String, Desired>, String)> {
    let mut out: BTreeMap<String, Desired> = BTreeMap::new();
    let versions = dist_versions(catalog)?;
    let latest = versions
        .iter()
        .rev()
        .find(|(v, _)| v.pre.is_none())
        .or(versions.last())
        .map(|(v, _)| v.to_string())
        .context("dist/ has no versions")?;
    let mut put = |id: String,
                   file: String,
                   name: Option<String>,
                   en: String,
                   tag: &str,
                   src: &'static str| {
        let since = (!en.is_empty() || name.is_some()).then(|| (tag.to_owned(), src));
        match out.get_mut(&id) {
            Some(d) => {
                let changed = d.en != en || d.en_name != name;
                d.file = file;
                if changed {
                    d.en = en;
                    d.en_name = name;
                    d.since = since;
                }
            }
            None => {
                out.insert(
                    id,
                    Desired {
                        file,
                        en_name: name,
                        en,
                        since,
                    },
                );
            }
        }
    };
    const FEATURE_SRC: &str = "codex-rs/features/src/lib.rs (experimental menu text)";
    for (_, path) in &versions {
        let vc = load_version(path)?;
        let tag = vc.source.tag.as_str();
        for f in &vc.features {
            let exp = f.experimental.as_ref();
            put(
                format!("feature:{}", f.key),
                FEATURES_FILE.to_owned(),
                exp.and_then(|e| nonempty(Some(&e.name))),
                exp.and_then(|e| nonempty(Some(&e.menu_description)))
                    .unwrap_or_default(),
                tag,
                FEATURE_SRC,
            );
        }
        for k in &vc.config_keys {
            if vc.features.iter().any(|f| f.config_path == k.path) {
                continue; // explained by its feature entry
            }
            let file = format!("config-{}.toml", curation.category_of_key(&k.path));
            put(
                format!("config:{}", k.path),
                file.clone(),
                None,
                nonempty(k.description.as_deref()).unwrap_or_default(),
                tag,
                sources::CONFIG_SCHEMA,
            );
            for ev in &k.enum_values {
                if let Some(d) = nonempty(ev.description.as_deref()) {
                    put(
                        format!("config:{}={}", k.path, ev.value),
                        file.clone(),
                        None,
                        d,
                        tag,
                        sources::CONFIG_SCHEMA,
                    );
                }
            }
        }
    }
    Ok((out, latest))
}

/// Existing overlay files: file name -> parsed file.
pub fn read_overlays(catalog: &Path) -> Result<BTreeMap<String, OverlayFile>> {
    let mut out = BTreeMap::new();
    let dir = catalog.join(OVERLAY_DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".toml") {
            continue;
        }
        let text = std::fs::read_to_string(e.path())?;
        let file = OverlayFile::parse(&name, &text).map_err(|e| anyhow::anyhow!("{e}"))?;
        out.insert(name, file);
    }
    Ok(out)
}

/// Digest of every human-written field (`zh`, `zh_name`, `en_note`) of the overlays.
pub fn human_digest(files: &BTreeMap<String, OverlayFile>) -> String {
    let mut all: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    for f in files.values() {
        for (id, e) in &f.entries {
            let t = (
                e.zh.trim().to_owned(),
                e.zh_name.as_deref().unwrap_or("").trim().to_owned(),
                e.en_note.as_deref().unwrap_or("").trim().to_owned(),
            );
            if !(t.0.is_empty() && t.1.is_empty() && t.2.is_empty()) {
                all.insert(id.clone(), t);
            }
        }
    }
    sha256_hex(serde_json::to_string(&all).unwrap_or_default().as_bytes())
}

const HEADER: &str = "\
# Superba Codex settings catalog: text overlay.
#
# Each table is one catalog entry: `feature:<key>`, `config:<path>` or
# `config:<path>=<value>`. `superba-catalog-gen overlay-sync` maintains the
# generator-owned fields and adds or retires entries:
#   en, en_name  official upstream English, copied verbatim (empty = upstream has none)
#   en_source    where that text comes from (first release with this wording)
#   en_hash      fingerprint of en_name + en
# Maintainers own the other fields (see catalog/CONTRIBUTING.md):
#   zh, zh_name  Chinese name / explanation, written per entry from the codex source
#   zh_basis     the en_hash the Chinese text was written against
#   en_note      English note, only where upstream has no text
# Comments in this file are not preserved; put remarks in the pull request.
";

/// Render one overlay file.
pub fn render(entries: &BTreeMap<String, OverlayEntry>) -> Result<String> {
    let file = OverlayFile {
        format: OVERLAY_FORMAT,
        entries: entries.clone(),
    };
    let body = toml::to_string(&file).context("serialize overlay")?;
    Ok(format!("{HEADER}\n{body}"))
}

pub struct SyncReport {
    pub added: usize,
    pub updated: usize,
    pub retired: usize,
    pub dropped: usize,
    pub files_written: Vec<String>,
}

/// Synchronize the overlays; with `dry_run` nothing is written (used by `check`).
pub fn sync(catalog: &Path, dry_run: bool) -> Result<SyncReport> {
    let curation = Curation::parse(
        &std::fs::read_to_string(catalog.join(CURATION_PATH)).context("read curation.toml")?,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (desired, latest) = desired(catalog, &curation)?;
    let existing = read_overlays(catalog)?;
    let before = human_digest(&existing);

    let mut old: BTreeMap<String, (String, OverlayEntry)> = BTreeMap::new();
    for (name, f) in &existing {
        for (id, e) in &f.entries {
            if old.insert(id.clone(), (name.clone(), e.clone())).is_some() {
                bail!("overlay id `{id}` appears in two files");
            }
        }
    }

    let mut files: BTreeMap<String, BTreeMap<String, OverlayEntry>> = BTreeMap::new();
    let mut report = SyncReport {
        added: 0,
        updated: 0,
        retired: 0,
        dropped: 0,
        files_written: Vec::new(),
    };
    for (id, d) in &desired {
        let prev = old.get(id).map(|(_, e)| e);
        let mut e = prev.cloned().unwrap_or_default();
        let prev_hash = e.en_hash.clone();
        e.en_name = d.en_name.clone();
        e.en = d.en.clone();
        e.en_source = d
            .since
            .as_ref()
            .map(|(tag, src)| format!("openai/codex {tag}: {src}"));
        e.en_hash = text_hash(e.en_name.as_deref(), &e.en);
        match (&e.en_name, &e.zh_name) {
            (Some(_), None) => e.zh_name = Some(String::new()),
            (None, Some(z)) if z.trim().is_empty() => e.zh_name = None,
            _ => {}
        }
        if e.has_zh() && e.zh_basis.is_none() {
            // Written by hand without a basis: assume the English it was written next to.
            e.zh_basis = Some(if prev.is_some() {
                prev_hash
            } else {
                e.en_hash.clone()
            });
        }
        e.retired = None;
        match prev {
            None => report.added += 1,
            Some(p) if *p != e => report.updated += 1,
            _ => {}
        }
        files
            .entry(d.file.clone())
            .or_default()
            .insert(id.clone(), e);
    }
    for (id, (file, e)) in &old {
        if desired.contains_key(id) {
            continue;
        }
        if e.has_zh() || e.has_en_note() {
            let mut e = e.clone();
            if e.retired.is_none() {
                e.retired = Some(latest.clone());
                report.retired += 1;
            }
            files.entry(file.clone()).or_default().insert(id.clone(), e);
        } else {
            report.dropped += 1;
        }
    }

    let rendered: BTreeMap<String, OverlayFile> = files
        .iter()
        .map(|(n, entries)| {
            (
                n.clone(),
                OverlayFile {
                    format: OVERLAY_FORMAT,
                    entries: entries.clone(),
                },
            )
        })
        .collect();
    let after = human_digest(&rendered);
    if before != after {
        bail!("internal error: overlay-sync would change human-written text; nothing written");
    }

    let dir = catalog.join(OVERLAY_DIR);
    for (name, entries) in &files {
        let text = render(entries)?;
        let path = dir.join(name);
        let same = std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str());
        if !same {
            report.files_written.push(name.clone());
            if !dry_run {
                write_if_changed(&path, text.as_bytes())?;
            }
        }
    }
    for name in existing.keys() {
        if !files.contains_key(name) {
            report.files_written.push(format!("{name} (removed)"));
            if !dry_run {
                std::fs::remove_file(dir.join(name))?;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use superba_catalog::VersionCatalog;
    use superba_catalog::format::{
        CATALOG_FORMAT, ConfigKey, EnumValue, ExperimentalText, Feature, KeyKind, Legacy,
        SourceRef, Stage, to_pretty_json,
    };
    use superba_catalog::overlay::ZhState;

    fn key(path: &str, desc: Option<&str>) -> ConfigKey {
        ConfigKey {
            path: path.into(),
            kind: KeyKind::String,
            types: vec!["string".into()],
            enum_values: vec![],
            enum_has_table_form: false,
            items: None,
            format: None,
            minimum: None,
            maximum: None,
            min_length: None,
            max_length: None,
            default: None,
            default_source: None,
            schema_default: None,
            description: desc.map(Into::into),
            definition: None,
            deprecated_hint: false,
            under_map: false,
            discriminator: None,
            variant_of: vec![],
        }
    }

    fn feature(k: &str, exp: Option<(&str, &str)>) -> Feature {
        Feature {
            key: k.into(),
            config_path: format!("features.{k}"),
            variant: "X".into(),
            stage: if exp.is_some() {
                Stage::Experimental
            } else {
                Stage::Stable
            },
            default_enabled: false,
            os_overrides: Default::default(),
            experimental: exp.map(|(n, d)| ExperimentalText {
                name: n.into(),
                menu_description: d.into(),
                announcement: None,
            }),
            rustdoc: Some("dev doc".into()),
            not_in_schema: false,
        }
    }

    fn write_version(dir: &Path, v: &str, keys: Vec<ConfigKey>, feats: Vec<Feature>) {
        let vc = VersionCatalog {
            catalog_format: CATALOG_FORMAT,
            codex_version: v.into(),
            source: SourceRef {
                repo: "openai/codex".into(),
                tag: format!("rust-v{v}"),
                commit: "c0ffee".into(),
                prerelease: false,
                files: Default::default(),
            },
            config_keys: keys,
            features: feats,
            legacy: Legacy::default(),
            type_docs: Default::default(),
        };
        let p = dir.join(format!("dist/codex-{v}.json"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, to_pretty_json(&vc).unwrap()).unwrap();
    }

    fn entry(dir: &Path, id: &str) -> Option<(String, OverlayEntry)> {
        read_overlays(dir)
            .unwrap()
            .into_iter()
            .find_map(|(n, f)| f.entries.get(id).cloned().map(|e| (n, e)))
    }

    fn set_zh(dir: &Path, id: &str, zh: &str) {
        let (name, mut file) = read_overlays(dir)
            .unwrap()
            .into_iter()
            .find(|(_, f)| f.entries.contains_key(id))
            .unwrap();
        file.entries.get_mut(id).unwrap().zh = zh.into();
        std::fs::write(
            dir.join(OVERLAY_DIR).join(name),
            render(&file.entries).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn sync_copies_official_text_and_never_writes_chinese() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join(CURATION_PATH),
            "format = 1\n[[category]]\nid = \"model\"\nlabel = \"Model\"\nkeys = [\"model\"]\n",
        )
        .unwrap();
        let mut effort = key("model", Some("Model override."));
        effort.kind = KeyKind::Enum;
        effort.enum_values = vec![
            EnumValue {
                value: "a".into(),
                description: Some("Value A.".into()),
            },
            EnumValue {
                value: "b".into(),
                description: None,
            },
        ];
        write_version(
            dir,
            "0.1.0",
            vec![
                effort.clone(),
                key("gone", None),
                key("gone_translated", Some("Old.")),
                key("features.fast", None),
            ],
            vec![
                feature("fast", None),
                feature("sleep", Some(("Sleep", "Keep awake."))),
            ],
        );

        let r = sync(dir, false).unwrap();
        assert_eq!(
            r.added, 6,
            "model, model=a, gone, gone_translated and two features"
        );
        let (file, e) = entry(dir, "config:model").unwrap();
        assert_eq!(file, "config-model.toml");
        assert_eq!(e.en, "Model override.");
        assert_eq!(
            e.en_source.as_deref(),
            Some("openai/codex rust-v0.1.0: codex-rs/core/config.schema.json")
        );
        assert!(e.zh.is_empty());
        assert!(entry(dir, "config:model=a").is_some());
        assert!(
            entry(dir, "config:model=b").is_none(),
            "values without upstream text get no entry"
        );
        assert!(
            entry(dir, "config:features.fast").is_none(),
            "feature toggles are explained by the feature"
        );
        let (file, e) = entry(dir, "feature:sleep").unwrap();
        assert_eq!(file, FEATURES_FILE);
        assert_eq!(
            (e.en_name.as_deref(), e.en.as_str()),
            (Some("Sleep"), "Keep awake.")
        );
        assert_eq!(e.zh_name.as_deref(), Some(""));
        let (_, e) = entry(dir, "feature:fast").unwrap();
        assert!(e.en.is_empty() && e.en_hash.is_empty() && e.en_source.is_none());
        assert_eq!(entry(dir, "config:gone").unwrap().0, "config-other.toml");

        // A person writes Chinese text (without a basis), then upstream changes the
        // English and drops two keys.
        set_zh(dir, "config:model", "人工翻译");
        set_zh(dir, "config:gone_translated", "保留");
        let digest = human_digest(&read_overlays(dir).unwrap());
        std::fs::remove_file(dir.join("dist/codex-0.1.0.json")).unwrap();
        let mut changed = effort;
        changed.description = Some("Model override (changed).".into());
        write_version(
            dir,
            "0.2.0",
            vec![changed],
            vec![
                feature("fast", None),
                feature("sleep", Some(("Sleep", "Keep awake."))),
            ],
        );

        let r = sync(dir, false).unwrap();
        assert_eq!((r.retired, r.dropped), (1, 1));
        assert_eq!(human_digest(&read_overlays(dir).unwrap()), digest);
        let (_, e) = entry(dir, "config:model").unwrap();
        assert_eq!(e.zh, "人工翻译");
        assert_eq!(e.en, "Model override (changed).");
        assert_ne!(e.zh_basis.as_deref(), Some(e.en_hash.as_str()));
        assert_eq!(e.zh_state(), ZhState::Stale);
        let (_, e) = entry(dir, "config:gone_translated").unwrap();
        assert_eq!(e.retired.as_deref(), Some("0.2.0"));
        assert!(entry(dir, "config:gone").is_none());

        // Idempotent.
        let r = sync(dir, true).unwrap();
        assert!(r.files_written.is_empty(), "{:?}", r.files_written);
    }
}
