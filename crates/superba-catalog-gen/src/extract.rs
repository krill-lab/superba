//! Assemble one [`VersionCatalog`] from the three upstream sources.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use superba_catalog::format::{
    Alias, CATALOG_FORMAT, ConfigKey, DefaultSource, ExperimentalText, Feature, FeatureOsOverride,
    KeyKind, Legacy, SourceRef, Stage,
};
use superba_catalog::{Os, VersionCatalog};
use superba_core::CodexVersion;

use crate::features::{self, AliasScope, StageKind};
use crate::git::SourceFiles;
use crate::schema;

/// Maps whose value type repeats top-level keys: recorded as a map node only.
const NO_EXPAND: &[&str] = &["profiles"];

/// Upstream repository name recorded in the catalog.
pub const REPO: &str = "openai/codex";

/// Extract the catalog of `version` from its sources.
pub fn extract(version: &CodexVersion, src: &SourceFiles) -> Result<VersionCatalog> {
    let schema_json: Value =
        serde_json::from_str(&src.schema).context("parse config.schema.json")?;
    let raw_keys =
        schema::flatten(&schema_json, NO_EXPAND).context("flatten config.schema.json")?;
    let reg = features::parse_registry(&src.lib_rs, Some(&src.legacy_rs))
        .context("parse the feature registry")?;

    let mut type_docs: BTreeMap<String, String> = BTreeMap::new();
    let mut config_keys = Vec::with_capacity(raw_keys.len());
    let mut seen = BTreeSet::new();
    for k in raw_keys {
        if !seen.insert(k.path.clone()) {
            bail!("duplicate config path `{}`", k.path);
        }
        let kind: KeyKind = serde_json::from_value(Value::String(k.kind.clone()))
            .with_context(|| format!("kind of `{}`", k.path))?;
        if kind == KeyKind::Unknown {
            bail!("unsupported kind `{}` of `{}`", k.kind, k.path);
        }
        match (&k.definition, &k.type_description) {
            (Some(def), Some(doc)) => {
                if let Some(prev) = type_docs.get(def)
                    && prev != doc
                {
                    bail!("definition `{def}` has two different docs");
                }
                type_docs.insert(def.clone(), doc.clone());
            }
            (None, Some(_)) => bail!("`{}` has a type doc but no definition", k.path),
            _ => {}
        }
        let default_source = match k.default_source {
            Some("schema") => Some(DefaultSource::Schema),
            Some("parent") => Some(DefaultSource::Parent),
            Some(other) => bail!("unknown default source `{other}`"),
            None => None,
        };
        // Lossless raw default next to the pruned display default.
        let schema_default = match (&k.raw_default, default_source) {
            (Some(raw), Some(DefaultSource::Schema)) if Some(raw) == k.default.as_ref() => None,
            (raw, _) => raw.clone(),
        };
        config_keys.push(ConfigKey {
            path: k.path,
            kind,
            types: k.types,
            enum_values: k.enum_values.unwrap_or_default(),
            enum_has_table_form: k.enum_has_table_form,
            items: k.items,
            format: k.format,
            minimum: k.minimum,
            maximum: k.maximum,
            min_length: k.min_length,
            max_length: k.max_length,
            default: k.default,
            default_source,
            schema_default,
            description: k.description.filter(|d| !d.is_empty()),
            definition: k.definition,
            deprecated_hint: k.deprecated_hint,
            under_map: k.under_map,
            discriminator: k.discriminator,
            variant_of: k.variant_of,
        });
    }

    let mut feats = Vec::with_capacity(reg.features.len());
    for f in &reg.features {
        let base_stage = f.stage.get(Os::Linux);
        let base_default = *f.default_enabled.get(Os::Linux);
        let mut os_overrides = BTreeMap::new();
        let mut experimental: Option<ExperimentalText> = None;
        for os in Os::ALL {
            let st = f.stage.get(os);
            let def = *f.default_enabled.get(os);
            let o = FeatureOsOverride {
                stage: (st.kind != base_stage.kind).then(|| stage_of(st.kind)),
                default_enabled: (def != base_default).then_some(def),
            };
            if o != FeatureOsOverride::default() {
                os_overrides.insert(os, o);
            }
            if let Some(e) = &st.experimental {
                let text = ExperimentalText {
                    name: e.name.clone(),
                    menu_description: e.menu_description.clone(),
                    announcement: e.announcement.clone(),
                };
                match &experimental {
                    Some(prev) if *prev != text => {
                        bail!("feature `{}` has different experimental text per OS", f.key)
                    }
                    _ => experimental = Some(text),
                }
            }
        }
        let config_path = format!("features.{}", f.key);
        feats.push(Feature {
            not_in_schema: !seen.contains(&config_path),
            key: f.key.clone(),
            config_path,
            variant: f.variant.clone(),
            stage: stage_of(base_stage.kind),
            default_enabled: base_default,
            os_overrides,
            experimental,
            rustdoc: f.rustdoc.clone(),
        });
    }
    feats.sort_by(|a, b| a.key.cmp(&b.key));

    let mut legacy = Legacy::default();
    for a in &reg.legacy_aliases {
        let alias = Alias {
            legacy_key: a.legacy_key.clone(),
            canonical: a.canonical.clone(),
        };
        match a.scope {
            AliasScope::Features => legacy.feature_aliases.push(alias),
            AliasScope::TopLevel => legacy.top_level_aliases.push(alias),
        }
    }
    legacy
        .feature_aliases
        .sort_by(|a, b| a.legacy_key.cmp(&b.legacy_key));
    legacy.feature_aliases.dedup();
    legacy
        .top_level_aliases
        .sort_by(|a, b| a.legacy_key.cmp(&b.legacy_key));
    legacy.top_level_aliases.dedup();
    legacy.ignored_feature_keys = sorted_unique(&reg.apply_map.ignored_keys);
    legacy.notice_feature_keys = sorted_unique(&reg.apply_map.forced_notice_keys);

    Ok(VersionCatalog {
        catalog_format: CATALOG_FORMAT,
        codex_version: version.to_string(),
        source: SourceRef {
            repo: REPO.to_owned(),
            tag: src.tag.clone(),
            commit: src.commit.clone(),
            prerelease: version.pre.is_some(),
            files: src.blobs.iter().cloned().collect(),
        },
        config_keys,
        features: feats,
        legacy,
        type_docs,
    })
}

fn stage_of(k: StageKind) -> Stage {
    match k {
        StageKind::UnderDevelopment => Stage::UnderDevelopment,
        StageKind::Experimental => Stage::Experimental,
        StageKind::Stable => Stage::Stable,
        StageKind::Deprecated => Stage::Deprecated,
        StageKind::Removed => Stage::Removed,
    }
}

fn sorted_unique(v: &[String]) -> Vec<String> {
    v.iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
