//! Differences between two version catalogs, rendered as Markdown (change files in
//! `dist/changes/` and pull request bodies).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use superba_catalog::format::{ConfigKey, Feature};
use superba_catalog::{Os, VersionCatalog};

#[derive(Debug, Default)]
pub struct Diff {
    pub keys_added: Vec<String>,
    pub keys_removed: Vec<String>,
    pub keys_changed: Vec<(String, Vec<String>)>,
    pub descriptions_changed: Vec<String>,
    pub features_added: Vec<String>,
    pub features_removed: Vec<String>,
    pub features_changed: Vec<(String, Vec<String>)>,
    pub feature_text_changed: Vec<String>,
    pub legacy: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.keys_added.is_empty()
            && self.keys_removed.is_empty()
            && self.keys_changed.is_empty()
            && self.descriptions_changed.is_empty()
            && self.features_added.is_empty()
            && self.features_removed.is_empty()
            && self.features_changed.is_empty()
            && self.feature_text_changed.is_empty()
            && self.legacy.is_empty()
    }
}

fn json(v: &impl serde::Serialize) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn stage_summary(f: &Feature) -> String {
    let base = f.stage.cli_str().to_owned();
    let diffs: Vec<String> = Os::ALL
        .iter()
        .filter(|os| f.stage_on(**os) != f.stage)
        .map(|os| format!("{os}: {}", f.stage_on(*os).cli_str()))
        .collect();
    if diffs.is_empty() {
        base
    } else {
        format!("{base} ({})", diffs.join(", "))
    }
}

fn default_summary(f: &Feature) -> String {
    let base = f.default_enabled.to_string();
    let diffs: Vec<String> = Os::ALL
        .iter()
        .filter(|os| f.default_on(**os) != f.default_enabled)
        .map(|os| format!("{os}: {}", f.default_on(*os)))
        .collect();
    if diffs.is_empty() {
        base
    } else {
        format!("{base} ({})", diffs.join(", "))
    }
}

fn key_changes(a: &ConfigKey, b: &ConfigKey) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = |name: &str, x: String, y: String| {
        if x != y {
            out.push(format!("{name} {x} → {y}"));
        }
    };
    field("kind", json(&a.kind), json(&b.kind));
    field("types", json(&a.types), json(&b.types));
    field("items", json(&a.items), json(&b.items));
    field("format", json(&a.format), json(&b.format));
    field("minimum", json(&a.minimum), json(&b.minimum));
    field("maximum", json(&a.maximum), json(&b.maximum));
    field("default", json(&a.default), json(&b.default));
    let ea: BTreeSet<&str> = a.enum_values.iter().map(|v| v.value.as_str()).collect();
    let eb: BTreeSet<&str> = b.enum_values.iter().map(|v| v.value.as_str()).collect();
    let added: Vec<&str> = eb.difference(&ea).copied().collect();
    let removed: Vec<&str> = ea.difference(&eb).copied().collect();
    if !added.is_empty() {
        out.push(format!("values added {}", added.join(", ")));
    }
    if !removed.is_empty() {
        out.push(format!("values removed {}", removed.join(", ")));
    }
    out
}

pub fn diff(a: &VersionCatalog, b: &VersionCatalog) -> Diff {
    let mut d = Diff::default();
    let ka: BTreeMap<&str, &ConfigKey> =
        a.config_keys.iter().map(|k| (k.path.as_str(), k)).collect();
    let kb: BTreeMap<&str, &ConfigKey> =
        b.config_keys.iter().map(|k| (k.path.as_str(), k)).collect();
    for (p, kb_) in &kb {
        match ka.get(p) {
            None => d.keys_added.push((*p).to_owned()),
            Some(ka_) => {
                let ch = key_changes(ka_, kb_);
                if !ch.is_empty() {
                    d.keys_changed.push(((*p).to_owned(), ch));
                }
                let descs = |k: &ConfigKey| {
                    (
                        k.description.clone(),
                        k.enum_values
                            .iter()
                            .map(|v| v.description.clone())
                            .collect::<Vec<_>>(),
                    )
                };
                if descs(ka_) != descs(kb_) {
                    d.descriptions_changed.push((*p).to_owned());
                }
            }
        }
    }
    d.keys_removed = ka
        .keys()
        .filter(|p| !kb.contains_key(*p))
        .map(|p| (*p).to_owned())
        .collect();

    let fa: BTreeMap<&str, &Feature> = a.features.iter().map(|f| (f.key.as_str(), f)).collect();
    let fb: BTreeMap<&str, &Feature> = b.features.iter().map(|f| (f.key.as_str(), f)).collect();
    for (k, fb_) in &fb {
        match fa.get(k) {
            None => d.features_added.push(format!(
                "{k} [{}, default {}]",
                stage_summary(fb_),
                default_summary(fb_)
            )),
            Some(fa_) => {
                let mut ch = Vec::new();
                if stage_summary(fa_) != stage_summary(fb_) {
                    ch.push(format!(
                        "stage {} → {}",
                        stage_summary(fa_),
                        stage_summary(fb_)
                    ));
                }
                if default_summary(fa_) != default_summary(fb_) {
                    ch.push(format!(
                        "default {} → {}",
                        default_summary(fa_),
                        default_summary(fb_)
                    ));
                }
                if !ch.is_empty() {
                    d.features_changed.push(((*k).to_owned(), ch));
                }
                if fa_.experimental != fb_.experimental {
                    d.feature_text_changed.push((*k).to_owned());
                }
            }
        }
    }
    d.features_removed = fa
        .keys()
        .filter(|k| !fb.contains_key(*k))
        .map(|k| (*k).to_owned())
        .collect();

    let set = |c: &VersionCatalog| -> BTreeSet<String> {
        let l = &c.legacy;
        l.feature_aliases
            .iter()
            .map(|a| format!("alias [features].{} → {}", a.legacy_key, a.canonical))
            .chain(
                l.top_level_aliases
                    .iter()
                    .map(|a| format!("alias {} → features.{}", a.legacy_key, a.canonical)),
            )
            .chain(
                l.ignored_feature_keys
                    .iter()
                    .map(|k| format!("ignored [features].{k}")),
            )
            .chain(
                l.notice_feature_keys
                    .iter()
                    .map(|k| format!("notice-only [features].{k}")),
            )
            .collect()
    };
    let (la, lb) = (set(a), set(b));
    for x in lb.difference(&la) {
        d.legacy.push(format!("added: {x}"));
    }
    for x in la.difference(&lb) {
        d.legacy.push(format!("removed: {x}"));
    }
    d
}

/// Markdown summary; `title` is the heading line.
pub fn markdown(d: &Diff, title: &str) -> String {
    let mut s = format!("### {title}\n\n");
    let list = |s: &mut String, name: &str, items: &[String]| {
        if !items.is_empty() {
            let _ = writeln!(s, "- **{name}** ({}):", items.len());
            for i in items {
                let _ = writeln!(s, "  - `{}`", i.replace('`', "'"));
            }
        }
    };
    let pairs = |v: &[(String, Vec<String>)]| -> Vec<String> {
        v.iter()
            .map(|(k, ch)| format!("{k}: {}", ch.join("; ")))
            .collect()
    };
    list(&mut s, "Features added", &d.features_added);
    list(&mut s, "Features removed", &d.features_removed);
    list(&mut s, "Features changed", &pairs(&d.features_changed));
    list(
        &mut s,
        "Experimental menu text changed",
        &d.feature_text_changed,
    );
    list(&mut s, "Config keys added", &d.keys_added);
    list(&mut s, "Config keys removed", &d.keys_removed);
    list(&mut s, "Config keys changed", &pairs(&d.keys_changed));
    list(
        &mut s,
        "Official descriptions changed",
        &d.descriptions_changed,
    );
    list(&mut s, "Legacy handling", &d.legacy);
    if d.is_empty() {
        s.push_str("- No catalog changes.\n");
    }
    s
}
