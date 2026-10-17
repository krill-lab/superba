//! Feature registry extraction from `codex-rs/features/src/{lib,legacy}.rs` with `syn`.
//!
//! Reads `enum Feature` (variant docs), `const FEATURES: &[FeatureSpec]` (key, stage,
//! default per OS), `const ALIASES` and `set_if_some(..)` calls (legacy aliases) and
//! the string arms of `apply_map` (ignored and notice-only `[features]` keys).
//! Unsupported expressions are errors: the generator never guesses.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use superba_catalog::Os;
use syn::visit::Visit;
use syn::{Expr, ExprStruct, Item, Lit, Member};

use crate::platform::{PerOs, block_tail, else_tail, eval_bool_expr};

#[derive(Clone, Debug, PartialEq)]
pub struct Experimental {
    pub name: String,
    pub menu_description: String,
    pub announcement: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StageKind {
    UnderDevelopment,
    Experimental,
    Stable,
    Deprecated,
    Removed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub kind: StageKind,
    pub experimental: Option<Experimental>,
}

#[derive(Clone, Debug)]
pub struct FeatureEntry {
    pub key: String,
    /// Rust enum variant (`Feature::X`); legacy aliases point at variants.
    pub variant: String,
    pub stage: PerOs<Stage>,
    pub default_enabled: PerOs<bool>,
    /// Rust doc comment on the `Feature` variant (developer-facing upstream text).
    pub rustdoc: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasScope {
    /// `[features].<legacy_key>`.
    Features,
    /// A top-level key (`LegacyFeatureToggles`).
    TopLevel,
}

#[derive(Clone, Debug)]
pub struct LegacyAlias {
    pub legacy_key: String,
    pub scope: AliasScope,
    pub canonical: String,
}

#[derive(Clone, Debug, Default)]
pub struct ApplyMapRules {
    /// `[features]` keys that are accepted and silently ignored (`continue` arms).
    pub ignored_keys: Vec<String>,
    /// Keys that still apply but always record a deprecation notice.
    pub forced_notice_keys: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FeatureRegistry {
    pub features: Vec<FeatureEntry>,
    pub legacy_aliases: Vec<LegacyAlias>,
    pub apply_map: ApplyMapRules,
}

fn lit_str(e: &Expr) -> Result<String> {
    match e {
        Expr::Lit(l) => match &l.lit {
            Lit::Str(s) => Ok(s.value()),
            _ => bail!("expected a string literal"),
        },
        // `concat!("a", "b")` of literals.
        Expr::Macro(m) if m.mac.path.is_ident("concat") => {
            let parts: syn::punctuated::Punctuated<syn::LitStr, syn::Token![,]> = m
                .mac
                .parse_body_with(syn::punctuated::Punctuated::parse_terminated)?;
            Ok(parts.iter().map(|p| p.value()).collect())
        }
        Expr::Group(g) => lit_str(&g.expr),
        other => bail!("expected a string literal, got `{}`", quote::quote!(#other)),
    }
}

fn field<'a>(s: &'a ExprStruct, name: &str) -> Result<&'a Expr> {
    s.fields
        .iter()
        .find(|f| matches!(&f.member, Member::Named(i) if i == name))
        .map(|f| &f.expr)
        .ok_or_else(|| anyhow!("missing field `{name}`"))
}

fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

fn eval_stage(expr: &Expr, os: Os) -> Result<Stage> {
    match expr {
        Expr::Path(p) => {
            let kind = match last_segment(&p.path).as_str() {
                "UnderDevelopment" => StageKind::UnderDevelopment,
                "Stable" => StageKind::Stable,
                "Deprecated" => StageKind::Deprecated,
                "Removed" => StageKind::Removed,
                other => bail!("unknown stage `{other}`"),
            };
            Ok(Stage {
                kind,
                experimental: None,
            })
        }
        Expr::Struct(s) if last_segment(&s.path) == "Experimental" => {
            let announcement = lit_str(field(s, "announcement")?)?;
            Ok(Stage {
                kind: StageKind::Experimental,
                experimental: Some(Experimental {
                    name: lit_str(field(s, "name")?)?,
                    menu_description: lit_str(field(s, "menu_description")?)?,
                    // codex treats "" as "no announcement" (Stage::experimental_announcement).
                    announcement: (!announcement.is_empty()).then_some(announcement),
                }),
            })
        }
        Expr::If(i) => {
            let cond = eval_bool_expr(&i.cond, os)?;
            let branch = if cond {
                block_tail(&i.then_branch)?
            } else {
                else_tail(
                    &i.else_branch
                        .as_ref()
                        .ok_or_else(|| anyhow!("if without else"))?
                        .1,
                )?
            };
            eval_stage(branch, os)
        }
        Expr::Paren(p) => eval_stage(&p.expr, os),
        Expr::Group(g) => eval_stage(&g.expr, os),
        Expr::Block(b) => eval_stage(block_tail(&b.block)?, os),
        other => bail!("unsupported stage expression `{}`", quote::quote!(#other)),
    }
}

fn doc_of(attrs: &[syn::Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(l) => match &l.lit {
                    Lit::Str(s) => Some(s.value().trim().to_string()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect();
    let text = lines.join(" ").trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn array_elems(expr: &Expr) -> Result<&syn::punctuated::Punctuated<Expr, syn::Token![,]>> {
    match expr {
        Expr::Reference(r) => array_elems(&r.expr),
        Expr::Array(a) => Ok(&a.elems),
        Expr::Group(g) => array_elems(&g.expr),
        _ => bail!("expected `&[ .. ]`"),
    }
}

fn find_const<'a>(file: &'a syn::File, name: &str) -> Option<&'a syn::ItemConst> {
    file.items.iter().find_map(|i| match i {
        Item::Const(c) if c.ident == name => Some(c),
        _ => None,
    })
}

struct ApplyMapVisitor {
    rules: ApplyMapRules,
    in_apply_map: bool,
}

fn contains_continue(e: &Expr) -> bool {
    struct V(bool);
    impl<'ast> Visit<'ast> for V {
        fn visit_expr_continue(&mut self, _: &'ast syn::ExprContinue) {
            self.0 = true;
        }
    }
    let mut v = V(false);
    v.visit_expr(e);
    v.0
}

fn pat_strs(p: &syn::Pat, out: &mut Vec<String>) {
    match p {
        syn::Pat::Lit(l) => {
            if let Lit::Str(s) = &l.lit {
                out.push(s.value());
            }
        }
        syn::Pat::Or(o) => o.cases.iter().for_each(|c| pat_strs(c, out)),
        _ => {}
    }
}

impl<'ast> Visit<'ast> for ApplyMapVisitor {
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let was = self.in_apply_map;
        self.in_apply_map = f.sig.ident == "apply_map";
        syn::visit::visit_impl_item_fn(self, f);
        self.in_apply_map = was;
    }
    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        if self.in_apply_map {
            for arm in &m.arms {
                let mut keys = Vec::new();
                pat_strs(&arm.pat, &mut keys);
                if keys.is_empty() {
                    continue;
                }
                if contains_continue(&arm.body) {
                    self.rules.ignored_keys.extend(keys);
                } else {
                    self.rules.forced_notice_keys.extend(keys);
                }
            }
        }
        syn::visit::visit_expr_match(self, m);
    }
}

pub fn parse_registry(lib_rs: &str, legacy_rs: Option<&str>) -> Result<FeatureRegistry> {
    let file = syn::parse_file(lib_rs).context("parse lib.rs")?;

    // Feature enum: variant -> rustdoc.
    let mut docs: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut variant_order = Vec::new();
    for item in &file.items {
        if let Item::Enum(e) = item
            && e.ident == "Feature"
        {
            for v in &e.variants {
                variant_order.push(v.ident.to_string());
                docs.insert(v.ident.to_string(), doc_of(&v.attrs));
            }
        }
    }
    if variant_order.is_empty() {
        bail!("`enum Feature` not found");
    }

    let features_const =
        find_const(&file, "FEATURES").ok_or_else(|| anyhow!("`FEATURES` not found"))?;
    let mut features = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut keys = std::collections::BTreeSet::new();
    for elem in array_elems(&features_const.expr)? {
        let Expr::Struct(s) = elem else {
            bail!("FEATURES element is not a struct literal");
        };
        let variant = match field(s, "id")? {
            Expr::Path(p) => last_segment(&p.path),
            _ => bail!("FeatureSpec.id is not a path"),
        };
        let key = lit_str(field(s, "key")?).with_context(|| format!("key of {variant}"))?;
        if !keys.insert(key.clone()) {
            bail!("duplicate feature key `{key}`");
        }
        let stage_expr = field(s, "stage")?;
        let stage = PerOs::from_fn(|os| eval_stage(stage_expr, os))
            .with_context(|| format!("stage of `{key}`"))?;
        let def_expr = field(s, "default_enabled")?;
        let default_enabled = PerOs::from_fn(|os| eval_bool_expr(def_expr, os))
            .with_context(|| format!("default_enabled of `{key}`"))?;
        seen.insert(variant.clone());
        features.push(FeatureEntry {
            key,
            rustdoc: docs.get(&variant).cloned().flatten(),
            variant,
            stage,
            default_enabled,
        });
    }
    for v in &variant_order {
        if !seen.contains(v) {
            bail!("Feature::{v} has no FeatureSpec row");
        }
    }

    let by_variant: BTreeMap<&str, &str> = features
        .iter()
        .map(|f| (f.variant.as_str(), f.key.as_str()))
        .collect();

    let mut legacy_aliases = Vec::new();
    if let Some(src) = legacy_rs {
        let lf = syn::parse_file(src).context("parse legacy.rs")?;
        if let Some(c) = find_const(&lf, "ALIASES") {
            for elem in array_elems(&c.expr)? {
                let Expr::Struct(s) = elem else {
                    bail!("ALIASES element is not a struct literal");
                };
                let legacy_key = lit_str(field(s, "legacy_key")?)?;
                let variant = match field(s, "feature")? {
                    Expr::Path(p) => last_segment(&p.path),
                    _ => bail!("Alias.feature is not a path"),
                };
                let canonical = by_variant
                    .get(variant.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| anyhow!("alias `{legacy_key}` -> unknown Feature::{variant}"))?;
                legacy_aliases.push(LegacyAlias {
                    legacy_key,
                    scope: AliasScope::Features,
                    canonical,
                });
            }
        }
        // Top-level legacy toggles: set_if_some(features, Feature::X, self.field, "key").
        struct SetIfSome<'m>(Vec<LegacyAlias>, &'m BTreeMap<&'m str, &'m str>);
        impl<'ast, 'm> Visit<'ast> for SetIfSome<'m> {
            fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
                if let Expr::Path(p) = &*c.func
                    && p.path.is_ident("set_if_some")
                    && c.args.len() == 4
                    && let Expr::Path(fp) = &c.args[1]
                    && let Ok(key) = lit_str(&c.args[3])
                {
                    let variant = last_segment(&fp.path);
                    if let Some(canon) = self.1.get(variant.as_str()) {
                        self.0.push(LegacyAlias {
                            legacy_key: key,
                            scope: AliasScope::TopLevel,
                            canonical: canon.to_string(),
                        });
                    }
                }
                syn::visit::visit_expr_call(self, c);
            }
        }
        let mut v = SetIfSome(Vec::new(), &by_variant);
        v.visit_file(&lf);
        legacy_aliases.extend(v.0);
    }

    let mut amv = ApplyMapVisitor {
        rules: ApplyMapRules::default(),
        in_apply_map: false,
    };
    amv.visit_file(&file);

    Ok(FeatureRegistry {
        features,
        legacy_aliases,
        apply_map: amv.rules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"
pub enum Feature {
    /// Fast mode.
    FastMode,
    /// Keeps the
    /// machine awake.
    PreventIdleSleep,
    SecretAuthStorage,
    MultiAgent,
}

pub const FEATURES: &[FeatureSpec] = &[
    FeatureSpec { id: Feature::FastMode, key: "fast_mode", stage: Stage::Stable, default_enabled: true },
    FeatureSpec {
        id: Feature::PreventIdleSleep,
        key: "prevent_idle_sleep",
        stage: if cfg!(any(target_os = "macos", target_os = "windows")) {
            Stage::Experimental {
                name: "Prevent sleep",
                menu_description: concat!("Keep the computer ", "awake."),
                announcement: "",
            }
        } else {
            Stage::UnderDevelopment
        },
        default_enabled: false,
    },
    FeatureSpec { id: Feature::SecretAuthStorage, key: "secret_auth_storage", stage: Stage::Stable, default_enabled: cfg!(windows) },
    FeatureSpec { id: Feature::MultiAgent, key: "multi_agent", stage: Stage::Removed, default_enabled: false },
];

impl Features {
    fn apply_map(&mut self, m: &BTreeMap<String, bool>) {
        for (k, v) in m {
            match k.as_str() {
                "undo" | "js_repl" => continue,
                "web_search_request" => { self.notice(k); }
                _ => {}
            }
        }
    }
}
"#;

    const LEGACY: &str = r#"
const ALIASES: &[Alias] = &[Alias { legacy_key: "collab", feature: Feature::MultiAgent }];
impl LegacyFeatureToggles {
    fn apply(self, features: &mut Features) {
        set_if_some(features, Feature::FastMode, self.old_fast, "experimental_fast");
    }
}
"#;

    #[test]
    fn registry_with_per_os_stage_and_default() {
        let reg = parse_registry(LIB, Some(LEGACY)).unwrap();
        assert_eq!(reg.features.len(), 4);
        let pis = &reg.features[1];
        assert_eq!(pis.key, "prevent_idle_sleep");
        assert_eq!(pis.rustdoc.as_deref(), Some("Keeps the machine awake."));
        assert_eq!(pis.stage.get(Os::Linux).kind, StageKind::UnderDevelopment);
        let mac = pis.stage.get(Os::Macos);
        assert_eq!(mac.kind, StageKind::Experimental);
        let exp = mac.experimental.as_ref().unwrap();
        assert_eq!(exp.menu_description, "Keep the computer awake.");
        assert_eq!(exp.announcement, None);
        let secret = &reg.features[2];
        assert!(*secret.default_enabled.get(Os::Windows));
        assert!(!*secret.default_enabled.get(Os::Linux));
        assert_eq!(reg.legacy_aliases.len(), 2);
        assert_eq!(reg.legacy_aliases[0].canonical, "multi_agent");
        assert_eq!(reg.legacy_aliases[1].scope, AliasScope::TopLevel);
        assert_eq!(reg.legacy_aliases[1].legacy_key, "experimental_fast");
        assert_eq!(reg.apply_map.ignored_keys, ["undo", "js_repl"]);
        assert_eq!(reg.apply_map.forced_notice_keys, ["web_search_request"]);
    }

    #[test]
    fn unsupported_shapes_are_errors() {
        let bad_stage = LIB.replace("Stage::Removed", "Stage::Sunset");
        assert!(parse_registry(&bad_stage, None).is_err());
        let orphan = LIB.replace("    MultiAgent,\n}", "    MultiAgent,\n    Orphan,\n}");
        assert!(parse_registry(&orphan, None).is_err());
        let dup = LIB.replace("key: \"multi_agent\"", "key: \"fast_mode\"");
        assert!(parse_registry(&dup, None).is_err());
        let bad_alias = LEGACY.replace("Feature::MultiAgent", "Feature::Nope");
        assert!(parse_registry(LIB, Some(&bad_alias)).is_err());
    }
}
