//! The settings engine (`superba_config::settings`): per-home filtering and
//! validation against a hand-made catalog, legacy migrations, the diff preview,
//! the `codex features list` cross-check and config value reading.

use std::collections::{BTreeMap, BTreeSet};

use pretty_assertions::assert_eq;
use superba_config::settings::{
    HomeInput, PreviewTarget, apply_migrations, cross_check_features, find_migrations, key_support,
    plan_settings, preview_changes, read_config_values, validate_value,
};
use superba_core::{
    CatalogFeature, CatalogKey, CatalogMatch, CatalogSnapshot, CatalogSource, ChangeReason,
    CodexSettings, CodexVersion, ConsumerKind, Error, FeatureMismatch, FeatureStage, FeaturesList,
    KeyChange, KeySupport, LegacyKind, LegacyRule, MigrationSource, ReservedOwner, SettingEntry,
    SettingEnumValue, SettingKind, SettingValue, SettingsConsumer, SettingsPlan, SkipReason,
    StaticCatalog, TargetId,
};

const APP: &str = "0.162.0-alpha.17.2";
const CLI: &str = "0.150.0";

fn v(s: &str) -> CodexVersion {
    s.parse().unwrap()
}

fn key(path: &str, kind: SettingKind) -> CatalogKey {
    CatalogKey::new(path, kind)
}

fn enum_key(path: &str, values: &[&str]) -> CatalogKey {
    let mut k = key(path, SettingKind::Enum);
    k.enum_values = values
        .iter()
        .map(|v| SettingEnumValue {
            value: (*v).into(),
            description: None,
        })
        .collect();
    k
}

fn feature(key: &str, stage: FeatureStage, default: bool) -> CatalogFeature {
    CatalogFeature {
        key: key.into(),
        config_path: format!("features.{key}"),
        stage,
        default_enabled: default,
        name: None,
        menu_description: None,
    }
}

fn legacy(path: &str, kind: LegacyKind) -> LegacyRule {
    LegacyRule {
        path: path.into(),
        kind,
    }
}

/// Keys and features both versions share, plus legacy rules.
fn base(version: &str) -> CatalogSnapshot {
    let mut max_bytes = key("history.max_bytes", SettingKind::Integer);
    max_bytes.minimum = Some(0.0);
    let mut token_budget = key("features.token_budget", SettingKind::BoolOrTable);
    token_budget.default = Some(false.into());
    let mut name = key("mcp_servers.<name>.command", SettingKind::String);
    name.min_length = Some(1);
    name.under_map = true;
    CatalogSnapshot::new(v(version))
        .with_key(enum_key(
            "web_search",
            &["live", "cached", "indexed", "disabled"],
        ))
        .with_key(max_bytes)
        .with_key(key("history", SettingKind::Table))
        .with_key(key("tui.notifications", SettingKind::Boolean))
        .with_key(key("notice", SettingKind::Union))
        .with_key(key("notify", SettingKind::Array))
        .with_key(name)
        .with_key(key("mcp_servers.<name>.env", SettingKind::Any))
        .with_key(token_budget)
        .with_key(key("features.token_budget.enabled", SettingKind::Boolean))
        .with_feature(feature("fast_mode", FeatureStage::Stable, true))
        .with_feature(feature(
            "token_budget",
            FeatureStage::UnderDevelopment,
            false,
        ))
        .with_feature(feature("multi_agent", FeatureStage::Stable, true))
        .with_feature(feature("old_thing", FeatureStage::Removed, false))
        .with_feature(feature("collab", FeatureStage::Deprecated, false))
        .with_legacy(legacy(
            "features.collab",
            LegacyKind::Alias {
                canonical: "features.multi_agent".into(),
            },
        ))
        .with_legacy(legacy(
            "features.web_search_request",
            LegacyKind::Replace {
                canonical: "web_search".into(),
                when: Some(true.into()),
                value: "live".into(),
            },
        ))
        .with_legacy(legacy("features.undo", LegacyKind::Ignored))
}

fn catalog() -> StaticCatalog {
    let app = base(APP)
        .with_key(enum_key("model_verbosity", &["low", "medium", "high"]))
        .with_feature(feature("ultrafast_mode", FeatureStage::Stable, true));
    StaticCatalog::new([base(CLI), app])
}

fn consumers(app: Option<&str>, cli: Option<&str>) -> Vec<SettingsConsumer> {
    vec![
        SettingsConsumer::new(ConsumerKind::DesktopApp, "ChatGPT", app.map(v)),
        SettingsConsumer::new(ConsumerKind::CliProcesses, "CLI", cli.map(v)),
    ]
}

fn settings(entries: &[(&str, Option<SettingValue>)]) -> CodexSettings {
    entries
        .iter()
        .map(|(p, v)| {
            (
                (*p).to_owned(),
                match v {
                    Some(v) => SettingEntry::Set(v.clone()),
                    None => SettingEntry::Unset,
                },
            )
        })
        .collect()
}

fn skipped(plan: &SettingsPlan) -> BTreeMap<String, SkipReason> {
    plan.skipped
        .iter()
        .map(|s| (s.path.clone(), s.reason.clone()))
        .collect()
}

fn applied(plan: &SettingsPlan) -> Vec<&str> {
    plan.apply.paths().collect()
}

fn note_keys(plan: &SettingsPlan) -> Vec<String> {
    plan.notes.iter().map(|n| n.text.key.clone()).collect()
}

fn plan_with(
    s: &CodexSettings,
    consumers: &[SettingsConsumer],
    cat: &dyn CatalogSource,
    config: Option<&str>,
    owned: &[String],
) -> SettingsPlan {
    let local = TargetId::local();
    let input = HomeInput::new(&local, consumers, cat)
        .config_toml(config.map(str::as_bytes))
        .owned_paths(owned);
    plan_settings(s, &input)
}

#[test]
fn every_managed_path_is_applied_or_skipped_with_a_reason() {
    let cat = catalog();
    let cons = consumers(Some(APP), Some(CLI));
    let mut s = settings(&[
        ("web_search", Some("live".into())),
        ("model_verbosity", Some("high".into())),
        ("features.ultrafast_mode", Some(true.into())),
        ("features.fast_mode", Some(false.into())),
        ("foo.bar", Some(true.into())),
        ("features.old_thing", Some(true.into())),
        ("approval_policy", None),
        ("tui.notifications", Some("yes".into())),
        ("history.max_bytes", Some(SettingValue::Integer(-1))),
        ("features.token_budget", Some(true.into())),
        ("features.token_budget.enabled", Some(true.into())),
        ("mcp_servers.docs.command", Some("".into())),
        ("mcp_servers.docs.env.TOKEN", Some("x".into())),
        ("features.collab", Some(true.into())),
    ]);
    s.insert_unchecked("openai_base_url", SettingEntry::Set("https://x".into()));
    s.insert_unchecked("model", SettingEntry::Set("gpt-5".into()));
    // The Models page owns these (`Profile::model`); a copy in the settings is
    // reported, never written.
    for path in [
        "model_context_window",
        "model_auto_compact_token_limit",
        "service_tier",
    ] {
        s.insert_unchecked(path, SettingEntry::Set(400_000_i64.into()));
    }
    s.insert_unchecked("a..b", SettingEntry::Set(true.into()));
    let plan = plan_with(&s, &cons, &cat, None, &[]);

    assert_eq!(
        applied(&plan),
        vec![
            "approval_policy",
            "features.collab",
            "features.fast_mode",
            "features.token_budget",
            "features.ultrafast_mode",
            "mcp_servers.docs.env.TOKEN",
            "model_verbosity",
            "web_search",
        ]
    );
    let sk = skipped(&plan);
    assert_eq!(sk["a..b"], SkipReason::InvalidPath);
    assert_eq!(
        sk["openai_base_url"],
        SkipReason::Reserved {
            owner: ReservedOwner::Connection
        }
    );
    for path in [
        "model",
        "model_context_window",
        "model_auto_compact_token_limit",
        "service_tier",
    ] {
        assert_eq!(
            sk[path],
            SkipReason::Reserved {
                owner: ReservedOwner::ModelDefaults
            },
            "{path}"
        );
    }
    assert_eq!(
        sk["foo.bar"],
        SkipReason::NotSupported {
            consumers: vec![format!("ChatGPT {APP}"), format!("CLI {CLI}")]
        }
    );
    assert_eq!(sk["features.old_thing"], SkipReason::Removed);
    assert!(
        matches!(&sk["tui.notifications"], SkipReason::InvalidValue { expected } if expected == "true or false")
    );
    assert!(
        matches!(&sk["history.max_bytes"], SkipReason::InvalidValue { expected } if expected == "an integer >= 0")
    );
    assert!(matches!(
        &sk["mcp_servers.docs.command"],
        SkipReason::InvalidValue { .. }
    ));
    assert_eq!(
        sk["features.token_budget.enabled"],
        SkipReason::Overlap {
            other: "features.token_budget".into()
        }
    );
    assert_eq!(plan.apply.len() + plan.skipped.len(), s.len());

    // Partial support: written, with a note naming the CLI.
    assert_eq!(
        plan.support["model_verbosity"]
            .iter()
            .map(|c| c.support)
            .collect::<Vec<_>>(),
        vec![KeySupport::Supported, KeySupport::Missing]
    );
    let partial: Vec<_> = plan
        .notes
        .iter()
        .filter(|n| n.text.key == "config.settings.partial_support")
        .map(|n| n.text.render_fallback())
        .collect();
    assert_eq!(
        partial,
        vec![
            format!(
                "features.ultrafast_mode has no effect for CLI {CLI}; it is written for the others."
            ),
            format!("model_verbosity has no effect for CLI {CLI}; it is written for the others."),
        ]
    );
    assert_eq!(
        plan.support["features.collab"][0].support,
        KeySupport::Legacy
    );
    // Exact matches add no catalog notes.
    assert!(!note_keys(&plan).iter().any(|k| k.contains("catalog")));
    assert_eq!(plan.home, Some(TargetId::local()));
    assert_eq!(plan.consumers, cons);
}

#[test]
fn inexact_catalog_matches_relax_checks_and_say_so() {
    let cat = catalog();
    let s = settings(&[
        ("web_search", Some("live".into())),
        ("model_verbosity", Some("high".into())),
        ("foo.bar", Some(true.into())),
    ]);
    // Approximate (an alpha that is not listed): missing keys may exist.
    let cons = vec![SettingsConsumer::new(
        ConsumerKind::CliProcesses,
        "CLI",
        Some(v("0.155.0")),
    )];
    let plan = plan_with(&s, &cons, &cat, None, &[]);
    assert_eq!(
        applied(&plan),
        vec!["foo.bar", "model_verbosity", "web_search"]
    );
    assert_eq!(
        plan.support["foo.bar"][0].catalog,
        Some(CatalogMatch::Approximate)
    );
    assert_eq!(plan.support["foo.bar"][0].support, KeySupport::Unknown);
    assert_eq!(
        note_keys(&plan),
        vec!["config.settings.catalog_approximate"]
    );
    assert_eq!(
        plan.notes[0].text.render_fallback(),
        "The settings catalog has no entry for CLI 0.155.0; Codex 0.150.0 is used instead."
    );

    // Newer than the catalog.
    let cons = vec![SettingsConsumer::new(
        ConsumerKind::DesktopApp,
        "ChatGPT",
        Some(v("0.170.0")),
    )];
    let plan = plan_with(&s, &cons, &cat, None, &[]);
    assert_eq!(plan.skipped, vec![]);
    assert_eq!(note_keys(&plan), vec!["config.settings.catalog_outdated"]);

    // Older than the floor: known keys become unknown, missing stay missing.
    let cons = vec![SettingsConsumer::new(
        ConsumerKind::CliProcesses,
        "CLI",
        Some(v("0.100.0")),
    )];
    let plan = plan_with(&s, &cons, &cat, None, &[]);
    assert_eq!(applied(&plan), vec!["web_search"]);
    assert_eq!(plan.support["web_search"][0].support, KeySupport::Unknown);
    assert_eq!(
        skipped(&plan).keys().collect::<Vec<_>>(),
        vec!["foo.bar", "model_verbosity"]
    );
    assert_eq!(
        note_keys(&plan),
        vec!["config.settings.catalog_below_floor"]
    );

    // Unknown version: nothing is filtered, but values are still validated.
    let cons = consumers(None, None);
    let mut s2 = s.clone();
    s2.set("web_search", "sometimes").unwrap();
    let plan = plan_with(&s2, &cons, &cat, None, &[]);
    assert_eq!(applied(&plan), vec!["foo.bar", "model_verbosity"]);
    assert!(matches!(
        &skipped(&plan)["web_search"],
        SkipReason::InvalidValue { expected } if expected.starts_with("one of: \"live\"")
    ));
    assert_eq!(
        note_keys(&plan),
        vec![
            "config.settings.version_unknown",
            "config.settings.version_unknown"
        ]
    );

    // No consumers known: nothing filtered, no notes.
    let plan = plan_with(&s, &[], &cat, None, &[]);
    assert_eq!(plan.apply, s);
    assert!(plan.notes.is_empty() && plan.support.is_empty());

    // No catalog data at all.
    let empty = StaticCatalog::default();
    let cons = consumers(Some(APP), None);
    let plan = plan_with(&s, &cons, &empty, None, &[]);
    assert_eq!(plan.apply, s);
    assert_eq!(
        note_keys(&plan),
        vec!["config.settings.no_catalog", "config.settings.no_catalog"]
    );
}

#[test]
fn values_are_checked_against_every_codex_that_knows_the_key() {
    // The app's Codex knows a new enum value; the CLI on the PATH does not.
    let newer = base(APP).with_key(enum_key(
        "web_search",
        &["live", "cached", "indexed", "disabled", "deep"],
    ));
    let cat = StaticCatalog::new([base(CLI), newer]);
    let s = settings(&[("web_search", Some("deep".into()))]);
    let plan = plan_with(&s, &consumers(Some(APP), Some(CLI)), &cat, None, &[]);
    assert!(
        applied(&plan).is_empty(),
        "the older CLI would refuse config.toml"
    );
    assert!(matches!(
        &skipped(&plan)["web_search"],
        SkipReason::InvalidValue { expected }
            if expected.starts_with("one of: \"live\"") && expected.ends_with("(for CLI 0.150.0)")
    ));
    // Only the newer Codex reads the home: the value is written.
    let plan = plan_with(&s, &consumers(Some(APP), None), &cat, None, &[]);
    assert_eq!(applied(&plan), vec!["web_search"]);
}

#[test]
fn removals_are_never_filtered_by_version() {
    let cat = catalog();
    let cons = consumers(Some(APP), Some(CLI));
    let s = settings(&[("foo.bar", None), ("features.old_thing", None)]);
    let plan = plan_with(&s, &cons, &cat, None, &[]);
    assert_eq!(plan.apply, s);
    assert!(plan.skipped.is_empty());
}

#[test]
fn user_values_of_another_shape_are_shape_conflicts_unless_superba_owns_them() {
    let cat = catalog();
    let cons = consumers(Some(APP), Some(CLI));
    let config = "[features]\ntoken_budget = true\n\n[notice]\nhide = true\n";
    let s = settings(&[
        ("features.token_budget.enabled", Some(true.into())),
        ("notice", Some("x".into())),
        ("web_search", Some("live".into())),
    ]);
    let plan = plan_with(&s, &cons, &cat, Some(config), &[]);
    assert_eq!(applied(&plan), vec!["web_search"]);
    assert_eq!(
        skipped(&plan),
        BTreeMap::from([
            (
                "features.token_budget.enabled".to_owned(),
                SkipReason::ShapeConflict {
                    parent: "features.token_budget".into()
                }
            ),
            (
                "notice".to_owned(),
                SkipReason::ShapeConflict {
                    parent: "notice".into()
                }
            ),
        ])
    );
    // Superba wrote them last time: the next apply releases or rewrites them.
    let owned = vec!["features.token_budget".to_owned(), "notice".to_owned()];
    let plan = plan_with(&s, &cons, &cat, Some(config), &owned);
    assert_eq!(plan.apply, s);
    // A table value may replace a table.
    let s = settings(&[(
        "notice",
        Some(SettingValue::Table(BTreeMap::from([(
            "hide".to_owned(),
            false.into(),
        )]))),
    )]);
    let plan = plan_with(&s, &cons, &cat, Some(config), &[]);
    assert_eq!(plan.apply, s);
    // An unparseable config.toml blocks nothing here (the renderer refuses it).
    let plan = plan_with(&s, &cons, &cat, Some("[[["), &[]);
    assert_eq!(plan.apply, s);
}

#[test]
fn legacy_keys_are_offered_applied_and_labelled() {
    let cat = catalog();
    let cons = consumers(Some(APP), Some(CLI));
    let local = TargetId::local();
    let config = "web_search = \"cached\"\n[features]\ncollab = true\nweb_search_request = true\nundo = true\n";
    let mut profile = CodexSettings::new();
    let input = HomeInput::new(&local, &cons, &cat).config_toml(Some(config.as_bytes()));
    let found = find_migrations(&profile, &input);
    let rows: Vec<_> = found
        .iter()
        .map(|m| {
            (
                m.legacy_path.as_str(),
                m.legacy_value.as_deref(),
                m.canonical.as_deref(),
                m.canonical_value.clone(),
                m.source,
                m.conflict,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "features.collab",
                Some("true"),
                Some("features.multi_agent"),
                Some(true.into()),
                MigrationSource::Config,
                false
            ),
            (
                "features.undo",
                Some("true"),
                None,
                None,
                MigrationSource::Config,
                false
            ),
            (
                "features.web_search_request",
                Some("true"),
                Some("web_search"),
                Some("live".into()),
                MigrationSource::Config,
                true
            ),
        ]
    );
    // The plan offers the same list.
    assert_eq!(plan_settings(&profile, &input).migrations, found);

    // Accepting adds the edits to the profile; the conflicting one only removes.
    apply_migrations(&mut profile, &found).unwrap();
    assert_eq!(
        profile,
        settings(&[
            ("features.collab", None),
            ("features.multi_agent", Some(true.into())),
            ("features.undo", None),
            ("features.web_search_request", None),
        ])
    );
    let plan = plan_settings(&profile, &input);
    assert!(plan.migrations.is_empty(), "{:?}", plan.migrations);
    assert_eq!(
        plan.migrated,
        BTreeMap::from([
            ("features.collab".to_owned(), "features.collab".to_owned()),
            (
                "features.multi_agent".to_owned(),
                "features.collab".to_owned()
            ),
            ("features.undo".to_owned(), "features.undo".to_owned()),
            (
                "features.web_search_request".to_owned(),
                "features.web_search_request".to_owned()
            ),
        ])
    );

    // A legacy key in the profile itself; a Replace rule only for its value.
    let profile = settings(&[
        ("features.collab", Some(false.into())),
        ("features.web_search_request", Some(false.into())),
    ]);
    let input = HomeInput::new(&local, &cons, &cat);
    let found = find_migrations(&profile, &input);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].source, MigrationSource::Profile);
    assert_eq!(found[0].canonical_value, Some(false.into()));
    assert_eq!(
        found[0].edits(),
        vec![
            ("features.collab".to_owned(), SettingEntry::Unset),
            (
                "features.multi_agent".to_owned(),
                SettingEntry::Set(false.into())
            ),
        ]
    );
    // Reserved canonical paths are refused and leave the profile unchanged.
    let mut bad = found[0].clone();
    bad.canonical = Some("model".into());
    let mut p = profile.clone();
    assert!(matches!(
        apply_migrations(&mut p, &[bad]),
        Err(Error::Invalid { .. })
    ));
    assert_eq!(p, profile);
}

#[test]
fn preview_rows_name_the_reason_for_every_change() {
    let change = |path: &str, before: Option<&str>, after: Option<&str>| KeyChange {
        path: path.into(),
        before: before.map(Into::into),
        after: after.map(Into::into),
    };
    let changes = vec![
        change("model_provider", Some("\"krill\""), Some("\"openai\"")),
        change(
            "openai_base_url",
            None,
            Some("\"https://x/backend-api/codex\""),
        ),
        change("chatgpt_base_url", Some("\"https://x/backend-api/\""), None),
        change("model", Some("\"a\""), Some("\"b\"")),
        change("web_search", None, Some("\"live\"")),
        change("approval_policy", Some("\"never\""), None),
        change("features.multi_agent", None, Some("true")),
        change("features.collab", Some("true"), None),
        change("tui.animations", Some("true"), Some("false")),
        change("features.context_management", Some("true"), None),
    ];
    let sp = SettingsPlan {
        apply: settings(&[
            ("web_search", Some("live".into())),
            ("approval_policy", None),
            ("features.multi_agent", Some(true.into())),
            ("features.collab", None),
        ]),
        migrated: BTreeMap::from([
            (
                "features.multi_agent".to_owned(),
                "features.collab".to_owned(),
            ),
            ("features.collab".to_owned(), "features.collab".to_owned()),
        ]),
        ..SettingsPlan::default()
    };
    let target = TargetId::remote("devbox");
    let rows = preview_changes(
        &changes,
        Some(&sp),
        false,
        PreviewTarget {
            id: &target,
            label: "devbox",
        },
    );
    let reasons: Vec<_> = rows
        .iter()
        .map(|r| (r.path.as_str(), r.reason.clone()))
        .collect();
    let migration = ChangeReason::Migration {
        legacy: "features.collab".into(),
    };
    assert_eq!(
        reasons,
        vec![
            ("model_provider", ChangeReason::Connection),
            ("openai_base_url", ChangeReason::Connection),
            ("chatgpt_base_url", ChangeReason::Connection),
            ("model", ChangeReason::Model),
            ("web_search", ChangeReason::Setting),
            ("approval_policy", ChangeReason::Reset),
            ("features.multi_agent", migration.clone()),
            ("features.collab", migration),
            // Not managed any more but written: a v1 recommended key.
            ("tui.animations", ChangeReason::Setting),
            ("features.context_management", ChangeReason::Released),
        ]
    );
    assert!(
        rows.iter()
            .all(|r| r.target == target && r.target_label == "devbox")
    );
    assert_eq!(rows[0].from.as_deref(), Some("\"krill\""));
    assert_eq!(rows[2].to, None);

    let restore = preview_changes(
        &changes,
        None,
        true,
        PreviewTarget {
            id: &TargetId::local(),
            label: "This computer",
        },
    );
    assert!(restore.iter().all(|r| r.reason == ChangeReason::Restore));
}

#[test]
fn features_cross_check_prefers_the_runtime() {
    let snap = base(APP);
    let consumer = SettingsConsumer::new(ConsumerKind::DesktopApp, "ChatGPT", Some(v(APP)));
    let out = "\
collab             deprecated         false
fast_mode          stable             false
multi_agent        stable             false
new_feature        experimental       false
old_thing          stable             false
token_budget       under development  true
";
    let list = FeaturesList::parse(out);
    // fast_mode is configured (so its state is no mismatch); multi_agent is
    // configured through the legacy alias features.collab.
    let config = "[features]\nfast_mode = false\ncollab = false\n";
    let check = cross_check_features(&consumer, &list, Some(&snap), Some(config.as_bytes()));
    assert_eq!(
        check.mismatches,
        vec![
            FeatureMismatch::UnknownToCatalog {
                key: "new_feature".into(),
                stage_text: "experimental".into()
            },
            FeatureMismatch::Stage {
                key: "old_thing".into(),
                catalog: FeatureStage::Removed,
                runtime: "stable".into()
            },
            FeatureMismatch::Default {
                key: "token_budget".into(),
                catalog: false,
                runtime: true
            },
        ]
    );
    assert!(check.catalog_outdated);
    assert_eq!(check.effective["token_budget"], true);
    assert_eq!(check.stages["old_thing"], FeatureStage::Stable);
    assert_eq!(check.consumer, consumer);

    // A catalog feature Codex does not print.
    let short = FeaturesList::parse("fast_mode stable true\n");
    let check = cross_check_features(&consumer, &short, Some(&snap), None);
    let missing: BTreeSet<_> = check
        .mismatches
        .iter()
        .filter(|m| matches!(m, FeatureMismatch::MissingAtRuntime { .. }))
        .map(|m| m.key().to_owned())
        .collect();
    assert_eq!(
        missing,
        BTreeSet::from(["collab", "multi_agent", "old_thing", "token_budget"].map(String::from))
    );
    assert!(!check.catalog_outdated);

    // No output rows or no catalog: nothing to compare.
    let empty = cross_check_features(&consumer, &FeaturesList::default(), Some(&snap), None);
    assert!(empty.mismatches.is_empty());
    let nocat = cross_check_features(&consumer, &list, None, None);
    assert!(nocat.mismatches.is_empty() && !nocat.catalog_outdated);
    assert_eq!(nocat.effective.len(), 6);
}

#[test]
fn config_values_are_flattened_to_dotted_paths() {
    let config = "\u{feff}model = \"gpt-5\"\n\
web_search = \"live\"\n\
features.fast_mode = true\n\
[tui]\nnotifications = [\"a\", \"b\"]\n\
[mcp_servers.docs]\ncommand = \"npx\"\nenv = { TOKEN = \"t\" }\n\
[projects.\"/home/u/x\"]\ntrust_level = \"trusted\"\n\
[[hooks]]\nname = \"h\"\n\
[when]\nat = 1979-05-27T07:32:00Z\n";
    let values = read_config_values(Some(config.as_bytes())).unwrap();
    let keys: Vec<_> = values.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec![
            "features.fast_mode",
            "hooks",
            "mcp_servers.docs.command",
            "mcp_servers.docs.env.TOKEN",
            "model",
            "tui.notifications",
            "web_search",
        ]
    );
    assert_eq!(values["features.fast_mode"], SettingValue::Bool(true));
    assert!(matches!(values["hooks"], SettingValue::Array(_)));
    assert!(read_config_values(None).unwrap().is_empty());
    assert!(read_config_values(Some(b"")).unwrap().is_empty());
    assert!(matches!(
        read_config_values(Some(b"a = ")),
        Err(Error::Parse { .. })
    ));
}

#[test]
fn key_support_and_value_validation_follow_the_catalog() {
    let cat = catalog();
    let cons = consumers(Some(APP), Some(CLI));
    let support = |p: &str| {
        key_support(p, &cons, &cat)
            .into_iter()
            .map(|c| c.support)
            .collect::<Vec<_>>()
    };
    assert_eq!(support("web_search"), [KeySupport::Supported; 2]);
    assert_eq!(support("features.collab"), [KeySupport::Legacy; 2]);
    assert_eq!(support("features.undo"), [KeySupport::Removed; 2]);
    assert_eq!(support("features.old_thing"), [KeySupport::Removed; 2]);
    assert_eq!(
        support("features.ultrafast_mode"),
        [KeySupport::Supported, KeySupport::Missing]
    );
    assert_eq!(support("mcp_servers.x.command"), [KeySupport::Supported; 2]);
    assert_eq!(support("mcp_servers.x.env.ANY"), [KeySupport::Supported; 2]);
    assert_eq!(support("mcp_servers.x"), [KeySupport::Missing; 2]);

    let ok = |k: &CatalogKey, v: SettingValue| validate_value(k, &v);
    let mut n = key("ratio", SettingKind::Number);
    n.minimum = Some(0.0);
    n.maximum = Some(1.0);
    assert!(ok(&n, SettingValue::Integer(1)).is_ok());
    assert!(ok(&n, SettingValue::float(0.5).unwrap()).is_ok());
    assert_eq!(
        ok(&n, SettingValue::float(1.5).unwrap()),
        Err("a number >= 0 and <= 1".into())
    );
    assert_eq!(ok(&n, "1".into()), Err("a number".into()));
    let mut e = enum_key("approval_policy", &["never", "on-request"]);
    assert!(ok(&e, "never".into()).is_ok());
    assert!(ok(&e, SettingValue::Table(BTreeMap::new())).is_err());
    e.enum_has_table_form = true;
    assert!(ok(&e, SettingValue::Table(BTreeMap::new())).is_ok());
    let free = key("model_reasoning_summary", SettingKind::Enum);
    assert!(ok(&free, "anything".into()).is_ok());
    let mut s = key("name", SettingKind::String);
    s.max_length = Some(3);
    assert_eq!(
        ok(&s, "abcd".into()),
        Err("a string of 0 to 3 characters".into())
    );
    let bt = key("features.token_budget", SettingKind::BoolOrTable);
    assert!(ok(&bt, true.into()).is_ok());
    assert!(ok(&bt, SettingValue::Table(BTreeMap::new())).is_ok());
    assert!(ok(&bt, "on".into()).is_err());
    assert!(
        ok(
            &key("notify", SettingKind::Array),
            SettingValue::Array(vec![])
        )
        .is_ok()
    );
    assert!(ok(&key("history", SettingKind::Table), true.into()).is_err());
    assert!(ok(&key("x", SettingKind::Any), true.into()).is_ok());
    assert!(ok(&key("x", SettingKind::Unknown), true.into()).is_ok());
}
