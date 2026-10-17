//! `build_catalog` against real Codex dumps.
//!
//! Fixtures (`tests/fixtures/`): `bundled-<version>.json` is the output of
//! `codex debug models --bundled` of the public npm builds of Codex 0.140.0, 0.150.1
//! and 0.162.0-alpha.17.2 (the desktop app's bundled build; 0.163.0-alpha.10.2
//! prints the same bytes), with every string longer than 240 characters (the
//! model instructions) replaced by a placeholder. `native-upstream.json` is a
//! Codex-native `/models` answer built from the 0.162 dump the way relays answer
//! (listed models only, extra back-compat fields, `ultrafast` on gpt-6-astra).
//! `golden/` holds generated entries; `SUPERBA_BLESS=1` rewrites them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::Value;
use superba_core::{
    CodexVersion, Error, ModelOverride, ModelSpec, ReasoningLevel, ServiceTierSpec, SpeedTier,
};
use superba_models::*;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).unwrap()
}

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

fn bundled(version: &str) -> BundledCatalog {
    let mut b = parse_bundled(&fixture(&format!("bundled-{version}.json"))).unwrap();
    b.codex_version = Some(v(version));
    b
}

fn native() -> Vec<ModelSpec> {
    let value: Value = serde_json::from_slice(&fixture("native-upstream.json")).unwrap();
    value["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| ModelSpec::from_codex_entry(e).unwrap())
        .collect()
}

const PROMPT: &str = "You are a coding agent (generic instructions for tests).";

struct Case {
    consumer: CodexVersion,
    template: BundledCatalog,
    source: CatalogSource,
    feed: Vec<ModelSpec>,
    overrides: BTreeMap<String, ModelOverride>,
    speed: SpeedTier,
    suffix: bool,
    model: Option<String>,
    prompt: Option<&'static str>,
}

impl Case {
    fn new(version: &str, source: CatalogSource) -> Self {
        Self {
            consumer: v(version),
            template: bundled(version),
            source,
            feed: Vec::new(),
            overrides: BTreeMap::new(),
            speed: SpeedTier::Standard,
            suffix: false,
            model: None,
            prompt: Some(PROMPT),
        }
    }

    fn build(&self) -> superba_core::Result<CatalogFile> {
        build_catalog(&CatalogInput {
            consumer: self.consumer.clone(),
            template: &self.template,
            source: self.source.clone(),
            feed: &self.feed,
            prompt: self.prompt,
            overrides: &self.overrides,
            speed: self.speed,
            tier_model_suffix: self.suffix,
            model: self.model.as_deref(),
        })
    }
}

fn models(file: &CatalogFile) -> Vec<Value> {
    let v: Value = serde_json::from_slice(&file.bytes).unwrap();
    v["models"].as_array().unwrap().clone()
}

fn entry(file: &CatalogFile, slug: &str) -> Value {
    models(file)
        .into_iter()
        .find(|m| m["slug"] == slug)
        .unwrap_or_else(|| panic!("{slug} not in {:?}", file.slugs))
}

fn tier_ids(e: &Value) -> Vec<String> {
    e["service_tiers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect()
}

fn note_keys(file: &CatalogFile) -> Vec<String> {
    file.notes.iter().map(|n| n.text.key.clone()).collect()
}

fn acme() -> ModelSpec {
    ModelSpec {
        context_window: Some(128_000),
        reasoning_levels: vec![
            ReasoningLevel {
                effort: "low".into(),
                description: None,
            },
            ReasoningLevel {
                effort: "high".into(),
                description: Some("Deeper".into()),
            },
        ],
        default_reasoning_level: Some("high".into()),
        ..ModelSpec::new("acme-coder")
    }
}

// ---------------------------------------------------------------------------

#[test]
fn parse_bundled_reads_every_supported_version() {
    for (ver, count) in [("0.140.0", 6), ("0.150.1", 10), ("0.162.0-alpha.17.2", 11)] {
        let b = bundled(ver);
        assert_eq!(b.entries.len(), count, "{ver}");
        for f in required_fields(&v(ver)) {
            assert!(b.required_fields.iter().any(|x| x == f), "{ver}: {f}");
        }
    }
    let old = bundled("0.140.0");
    assert!(
        old.required_fields
            .contains(&"supports_reasoning_summaries".to_owned())
    );
    let new = bundled("0.162.0-alpha.17.2");
    assert!(
        !new.required_fields
            .contains(&"supports_reasoning_summaries".to_owned())
    );
}

#[test]
fn parse_bundled_skips_a_warning_line_and_rejects_empty_or_garbage() {
    let out =
        b"WARNING: something\n{\"models\":[{\"slug\":\"a\",\"x\":1},{\"slug\":\"a\"},{\"y\":2}]}\n";
    let b = parse_bundled(out).unwrap();
    assert_eq!(b.entries.len(), 1);
    assert_eq!(b.required_fields, ["slug", "x"]);
    assert!(parse_bundled(b"{\"models\":[]}").is_err());
    assert!(parse_bundled(b"Error: no").is_err());
    assert!(parse_bundled(b"{\"other\":1}").is_err());
    assert_eq!(
        parse_bundled(b"[{\"slug\":\"b\"}]").unwrap().entries.len(),
        1
    );
}

#[test]
fn required_fields_follow_the_codex_source() {
    let has = |ver: &str, f: &str| required_fields(&v(ver)).contains(&f);
    assert!(has("0.140.0", "supports_reasoning_summaries"));
    assert!(has("0.140.0", "base_instructions"));
    assert!(has("0.140.0", "supports_parallel_tool_calls"));
    assert!(!has("0.145.0", "supports_reasoning_summaries"));
    assert!(has("0.146.0", "base_instructions"));
    assert!(!has("0.147.0", "base_instructions"));
    assert!(has("0.147.0", "supports_parallel_tool_calls"));
    assert!(!has("0.148.0", "supports_parallel_tool_calls"));
    for f in [
        "slug",
        "display_name",
        "truncation_policy",
        "experimental_supported_tools",
    ] {
        assert!(has("0.163.0", f));
    }
}

#[test]
fn a_template_from_another_version_is_rejected_before_anything_is_built() {
    // The 0.162 dump as the template of a 0.140 consumer: refused by version.
    let mut case = Case::new("0.140.0", CatalogSource::Bundled);
    case.template = bundled("0.162.0-alpha.17.2");
    let err = case.build().unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(err.to_string().contains("0.162.0-alpha.17.2"), "{err}");

    // The same dump without a version: refused because its entries lack the
    // fields 0.140 requires.
    case.template.codex_version = None;
    let err = case.build().unwrap_err().to_string();
    assert!(err.contains("supports_reasoning_summaries"), "{err}");
    assert!(err.contains("base_instructions") || err.contains("supports_parallel_tool_calls"));

    // An older dump for a newer consumer, versions known: refused.
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Bundled);
    case.template = bundled("0.140.0");
    assert!(case.build().is_err());
}

#[test]
fn bundled_source_reproduces_the_dump_and_is_deterministic() {
    let case = Case::new("0.162.0-alpha.17.2", CatalogSource::Bundled);
    let a = case.build().unwrap();
    let b = case.build().unwrap();
    assert_eq!(a, b);
    assert_eq!(a.sha8.len(), 8);
    assert_eq!(a.file_name(), format!("models-{}.json", a.sha8));
    assert_eq!(content_name(&a.bytes).unwrap().0, a.sha8);
    let dump: Value = serde_json::from_slice(&fixture("bundled-0.162.0-alpha.17.2.json")).unwrap();
    assert_eq!(Value::Array(models(&a)), dump["models"]);
    assert!(a.notes.is_empty(), "{:?}", a.notes);
}

#[test]
fn overrides_patch_window_max_compaction_name_and_visibility() {
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Bundled);
    case.overrides.insert(
        "gpt-6-luna".into(),
        ModelOverride {
            context_window: Some(123_456),
            auto_compact: Some(100_000),
            display_name: Some("Luna (short)".into()),
            ..Default::default()
        },
    );
    case.overrides.insert(
        "gpt-5.5".into(),
        ModelOverride {
            context_window: Some(400_000),
            auto_compact: Some(390_000),
            hidden: true,
            ..Default::default()
        },
    );
    case.overrides.insert(
        "no-such-model".into(),
        ModelOverride {
            context_window: Some(64_000),
            ..Default::default()
        },
    );
    let f = case.build().unwrap();
    let luna = entry(&f, "gpt-6-luna");
    assert_eq!(luna["context_window"], 123_456);
    assert_eq!(luna["max_context_window"], 872_000);
    assert_eq!(luna["auto_compact_token_limit"], 100_000);
    assert_eq!(luna["display_name"], "Luna (short)");
    assert_eq!(luna["visibility"], "list");
    let g55 = entry(&f, "gpt-5.5");
    assert_eq!(g55["context_window"], 400_000);
    assert_eq!(g55["max_context_window"], 400_000, "max = max(max, window)");
    assert_eq!(g55["visibility"], "hide");
    let keys = note_keys(&f);
    assert!(
        keys.contains(&"models_note.max_raised".to_owned()),
        "{keys:?}"
    );
    assert!(
        keys.contains(&"models_note.compact_clamped".to_owned()),
        "{keys:?}"
    );
    assert!(keys.contains(&"models_note.override_unknown_model".to_owned()));
    assert!(!f.slugs.contains(&"no-such-model".to_owned()));
}

#[test]
fn invalid_override_values_refuse_the_build() {
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Bundled);
    case.overrides.insert(
        "gpt-6-luna".into(),
        ModelOverride {
            context_window: Some(8_000),
            ..Default::default()
        },
    );
    assert!(case.build().is_err());
    case.overrides.insert(
        "gpt-6-luna".into(),
        ModelOverride {
            auto_compact: Some(0),
            ..Default::default()
        },
    );
    assert!(case.build().is_err());
}

#[test]
fn native_entries_are_kept_verbatim_including_unknown_fields() {
    let specs = native();
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Native(specs.clone()));
    case.model = Some("gpt-6-astra".into());
    let f = case.build().unwrap();
    assert_eq!(
        f.slugs,
        specs.iter().map(|s| s.slug.clone()).collect::<Vec<_>>()
    );
    for s in &specs {
        assert_eq!(&entry(&f, &s.slug), s.raw.as_ref().unwrap());
    }
    let astra = entry(&f, "gpt-6-astra");
    assert_eq!(astra["prefer_websockets"], true);
    assert_eq!(tier_ids(&astra), ["priority", "ultrafast"]);
    assert!(f.notes.is_empty(), "{:?}", f.notes);
}

#[test]
fn native_entries_missing_a_required_field_get_it_from_the_consumer() {
    let mut specs = native();
    for s in &mut specs {
        if let Some(Value::Object(o)) = s.raw.as_mut() {
            o.shift_remove("truncation_policy");
        }
    }
    // 0.140 also requires base_instructions etc.: filled from its generic base.
    let case = Case::new("0.140.0", CatalogSource::Native(specs.clone()));
    let f = case.build().unwrap();
    for slug in &f.slugs {
        let e = entry(&f, slug);
        for field in required_fields(&v("0.140.0")) {
            assert!(e.get(field).is_some(), "{slug}: {field}");
        }
    }
    assert!(note_keys(&f).contains(&"models_note.filled_fields".to_owned()));
    // The verbatim part stays.
    assert_eq!(entry(&f, "gpt-6-astra")["prefer_websockets"], true);
}

#[test]
fn openai_list_uses_the_consumer_entry_then_feed_then_generic_and_only_line_tiers() {
    let feed_spec = ModelSpec {
        display_name: Some("Feed Model".into()),
        context_window: Some(500_000),
        max_context_window: Some(900_000),
        service_tiers: vec![ServiceTierSpec {
            id: "priority".into(),
            name: "Fast".into(),
            description: String::new(),
        }],
        ..ModelSpec::new("feed-model")
    };
    let mut case = Case::new(
        "0.162.0-alpha.17.2",
        CatalogSource::OpenAiList(vec![
            ModelSpec::new("gpt-6-luna"),
            ModelSpec::new("feed-model"),
            acme(),
        ]),
    );
    case.feed = vec![feed_spec];
    let f = case.build().unwrap();
    assert_eq!(f.slugs, ["gpt-6-luna", "feed-model", "acme-coder"]);
    let luna = entry(&f, "gpt-6-luna");
    // The consumer's own entry, but without tiers the line did not declare.
    assert_eq!(tier_ids(&luna), Vec::<String>::new());
    assert!(luna.get("default_service_tier").is_none());
    assert_eq!(luna["display_name"], "GPT-6-Luna");
    let feed = entry(&f, "feed-model");
    assert_eq!(feed["display_name"], "Feed Model");
    assert_eq!(feed["context_window"], 500_000);
    assert_eq!(feed["max_context_window"], 900_000);
    assert_eq!(tier_ids(&feed), Vec::<String>::new());
    let acme = entry(&f, "acme-coder");
    assert_eq!(acme["context_window"], 128_000);
    assert_eq!(acme["model_messages"]["instructions_template"], PROMPT);
    assert_eq!(acme["base_instructions"], PROMPT);
    assert_eq!(acme["default_reasoning_level"], "high");
    assert!(note_keys(&f).contains(&"models_note.generic_template".to_owned()));
}

#[test]
fn feed_source_keeps_bundled_entries_and_feed_tiers() {
    let feed = vec![
        ModelSpec::new("gpt-6-luna"),
        ModelSpec {
            service_tiers: vec![ServiceTierSpec {
                id: "priority".into(),
                name: "Fast".into(),
                description: String::new(),
            }],
            ..ModelSpec::new("gpt-7-next")
        },
    ];
    let case = Case::new("0.162.0-alpha.17.2", CatalogSource::Feed(feed));
    let f = case.build().unwrap();
    assert_eq!(
        entry(&f, "gpt-6-luna"),
        bundled("0.162.0-alpha.17.2").entries[3]
    );
    assert_eq!(tier_ids(&entry(&f, "gpt-7-next")), ["priority"]);
}

#[test]
fn generic_entries_match_the_golden_files_per_version() {
    for ver in ["0.140.0", "0.150.1", "0.162.0-alpha.17.2"] {
        let case = Case::new(ver, CatalogSource::OpenAiList(vec![acme()]));
        let f = case.build().unwrap();
        let got = serde_json::to_string_pretty(&entry(&f, "acme-coder")).unwrap() + "\n";
        let path = fixture_path(&format!("golden/generic-{ver}.json"));
        if std::env::var_os("SUPERBA_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &got).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(got, want, "{ver}: run with SUPERBA_BLESS=1 after reviewing");
    }
}

#[test]
fn old_consumers_get_legacy_instructions_only() {
    let case = Case::new("0.140.0", CatalogSource::OpenAiList(vec![acme()]));
    let f = case.build().unwrap();
    let e = entry(&f, "acme-coder");
    assert_eq!(e["base_instructions"], PROMPT);
    assert!(e.get("model_messages").is_none());
    assert_eq!(e["supports_reasoning_summaries"], false);
    assert_eq!(e["supports_parallel_tool_calls"], false);
}

#[test]
fn without_a_prompt_a_generic_entry_keeps_template_instructions_with_a_warning() {
    let mut case = Case::new(
        "0.162.0-alpha.17.2",
        CatalogSource::OpenAiList(vec![acme()]),
    );
    case.prompt = None;
    let f = case.build().unwrap();
    assert!(note_keys(&f).contains(&"models_note.generic_without_prompt".to_owned()));
    let e = entry(&f, "acme-coder");
    assert!(e["model_messages"]["instructions_template"].is_string());
}

#[test]
fn the_configured_model_is_always_listed() {
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Native(native()));
    case.model = Some("gpt-5.5".into());
    let f = case.build().unwrap();
    assert!(
        f.slugs.contains(&"gpt-5.5".to_owned()),
        "taken from the template"
    );
    case.model = Some("my-local-model".into());
    let f = case.build().unwrap();
    assert_eq!(f.slugs.last().unwrap(), "my-local-model");
    assert!(note_keys(&f).contains(&"models_note.model_added".to_owned()));
}

#[test]
fn ultrafast_lists_the_tier_only_where_the_line_declares_it() {
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Native(native()));
    case.speed = SpeedTier::Ultrafast;
    case.model = Some("gpt-6-luna".into());
    let f = case.build().unwrap();
    let with_ultra: Vec<String> = models(&f)
        .iter()
        .filter(|m| tier_ids(m).contains(&"ultrafast".to_owned()))
        .map(|m| m["slug"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(with_ultra, ["gpt-6-astra"]);
    assert!(!f.slugs.iter().any(|s| s.ends_with("-ultrafast")));
    assert!(note_keys(&f).contains(&"models_note.tier_not_offered_by_model".to_owned()));
}

#[test]
fn tier_suffix_lines_get_suffix_entries_without_tiers() {
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Native(native()));
    case.suffix = true;
    case.speed = SpeedTier::Ultrafast;
    case.model = Some("gpt-6-astra-ultrafast".into());
    case.overrides.insert(
        "gpt-6-astra".into(),
        ModelOverride {
            context_window: Some(300_000),
            display_name: Some("Astra".into()),
            ..Default::default()
        },
    );
    let f = case.build().unwrap();
    let ultra: Vec<&String> = f
        .slugs
        .iter()
        .filter(|s| s.ends_with("-ultrafast"))
        .collect();
    assert_eq!(ultra, ["gpt-6-astra-ultrafast"]);
    let e = entry(&f, "gpt-6-astra-ultrafast");
    assert_eq!(tier_ids(&e), Vec::<String>::new());
    assert!(e.get("default_service_tier").is_none());
    assert_eq!(e["display_name"], "Astra Ultrafast");
    assert_eq!(
        e["context_window"], 300_000,
        "overrides follow the base model"
    );
    assert_eq!(entry(&f, "gpt-6-astra")["display_name"], "Astra");

    case.speed = SpeedTier::Fast;
    case.model = Some("gpt-6-luna-fast".into());
    let f = case.build().unwrap();
    let fast: Vec<&String> = f.slugs.iter().filter(|s| s.ends_with("-fast")).collect();
    assert_eq!(
        fast.len(),
        8,
        "every listed model offers priority: {fast:?}"
    );
    assert_eq!(
        entry(&f, "gpt-6-luna-fast")["display_name"],
        "GPT-6-Luna Fast"
    );
    assert!(!f.slugs.iter().any(|s| s.ends_with("-ultrafast")));
}

#[test]
fn a_stale_suffix_model_is_still_listed_and_standard_adds_no_suffix_entries() {
    let mut case = Case::new("0.162.0-alpha.17.2", CatalogSource::Native(native()));
    case.suffix = true;
    case.speed = SpeedTier::Standard;
    let f = case.build().unwrap();
    assert!(!f.slugs.iter().any(|s| s.ends_with("-fast")));
    case.model = Some("gpt-6-luna-ultrafast".into());
    let f = case.build().unwrap();
    assert!(f.slugs.contains(&"gpt-6-luna-ultrafast".to_owned()));
    assert!(note_keys(&f).contains(&"models_note.tier_not_offered_by_model".to_owned()));
}

#[test]
fn duplicate_slugs_keep_the_first_entry() {
    let case = Case::new(
        "0.162.0-alpha.17.2",
        CatalogSource::OpenAiList(vec![acme(), ModelSpec::new("acme-coder")]),
    );
    let f = case.build().unwrap();
    assert_eq!(f.slugs, ["acme-coder"]);
    assert!(note_keys(&f).contains(&"models_note.duplicate".to_owned()));
}

#[test]
fn an_empty_list_without_a_configured_model_is_refused() {
    let case = Case::new("0.162.0-alpha.17.2", CatalogSource::OpenAiList(Vec::new()));
    assert!(case.build().is_err());
}

#[test]
fn every_note_key_exists_in_the_core_catalog() {
    let mut case = Case::new("0.140.0", CatalogSource::OpenAiList(vec![acme()]));
    case.prompt = None;
    case.speed = SpeedTier::Ultrafast;
    case.model = Some("x".into());
    case.overrides.insert(
        "zzz".into(),
        ModelOverride {
            context_window: Some(20_000),
            ..Default::default()
        },
    );
    let f = case.build().unwrap();
    // en/zh-CN parity of the core locale files is checked by superba-core.
    let cat = superba_core::Catalog::new(superba_core::Locale::En);
    assert!(f.notes.len() >= 4, "{:?}", f.notes);
    for n in &f.notes {
        assert!(cat.get(&n.text.key).is_some(), "{}", n.text.key);
    }
}
