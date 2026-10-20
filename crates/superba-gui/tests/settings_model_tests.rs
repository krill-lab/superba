#![allow(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use pretty_assertions::assert_eq;
use superba_gui::pages::settings::model::*;

const STAGES: [Stage; 5] = [
    Stage::Stable,
    Stage::Experimental,
    Stage::UnderDevelopment,
    Stage::Deprecated,
    Stage::Removed,
];

fn entry(path: &str, stage: Stage, kind: Kind) -> Entry {
    Entry {
        id: format!("config:{path}"),
        path: path.into(),
        name: None,
        category: "general".into(),
        stage,
        kind,
        text: Explanation::default(),
        default: None,
        current: None,
        effective: None,
        since: None,
        availability: Vec::new(),
        legacy: None,
        recommended: None,
        platform: None,
        name_zh: None,
        file_value: None,
        managed: false,
        advanced_only: false,
        owner: None,
        common: None,
        host_support: Vec::new(),
        new_in: None,
    }
}

fn model(entries: Vec<Entry>) -> Model {
    Model {
        entries,
        ..Model::default()
    }
}

fn legacy_entry(path: &str, canonical: Option<&str>) -> Entry {
    let mut e = entry(path, Stage::Deprecated, Kind::Bool);
    e.current = Some(Value::Bool(true));
    e.legacy = Some(Legacy {
        canonical: canonical.map(str::to_owned),
        canonical_value: None,
        ignored: true,
    });
    e
}

#[test]
fn default_filter_shows_only_stable_and_experimental_stages() {
    for stage in STAGES {
        let e = entry("feature", stage, Kind::Bool);
        assert_eq!(
            Filter::default().accepts(&e, &Pending::default()),
            matches!(stage, Stage::Stable | Stage::Experimental),
            "{stage:?}"
        );
    }
}

#[test]
fn default_filter_hides_unconfigured_legacy_entries() {
    let mut e = legacy_entry("old", Some("new"));
    e.stage = Stage::Stable;
    e.current = None;
    assert!(!Filter::default().accepts(&e, &Pending::default()));
}

#[test]
fn advanced_filter_shows_every_stage_and_legacy_entries() {
    let filter = Filter {
        advanced: true,
        ..Filter::default()
    };
    for stage in STAGES {
        let mut e = entry("feature", stage, Kind::Bool);
        assert!(filter.accepts(&e, &Pending::default()), "{stage:?}");
        e.legacy = legacy_entry("old", None).legacy;
        assert!(filter.accepts(&e, &Pending::default()), "legacy {stage:?}");
    }
}

#[test]
fn configured_entries_remain_visible_at_every_stage() {
    for stage in STAGES {
        let mut e = entry("feature", stage, Kind::Bool);
        e.current = Some(Value::Bool(false));
        assert!(Filter::default().accepts(&e, &Pending::default()));
        e.legacy = legacy_entry("old", None).legacy;
        assert!(Filter::default().accepts(&e, &Pending::default()));
    }
}

#[test]
fn pending_edits_remain_visible_at_every_stage() {
    for stage in STAGES {
        let mut e = entry("feature", stage, Kind::Bool);
        let mut pending = Pending::default();
        pending.set(&e, Change::Set(Value::Bool(false)));
        assert!(Filter::default().accepts(&e, &pending));
        e.legacy = legacy_entry("old", None).legacy;
        assert!(Filter::default().accepts(&e, &pending));
    }
}

#[test]
fn recommended_entries_remain_visible_at_every_stage() {
    for stage in STAGES {
        let mut e = entry("feature", stage, Kind::Bool);
        e.recommended = Some(Value::Bool(false));
        assert!(Filter::default().accepts(&e, &Pending::default()));
        e.legacy = legacy_entry("old", None).legacy;
        assert!(Filter::default().accepts(&e, &Pending::default()));
    }
}

#[test]
fn changed_rail_lists_managed_keys_and_pending_edits_at_any_stage() {
    let filter = Filter {
        rail: Rail::Changed,
        ..Filter::default()
    };
    let mut e = entry("feature", Stage::Removed, Kind::Bool);
    e.default = Some(Value::Bool(false));
    let mut pending = Pending::default();
    assert!(!filter.accepts(&e, &pending));
    // A value only in config.toml (not managed by the provider) is not "changed".
    e.current = Some(Value::Bool(true));
    assert!(!filter.accepts(&e, &pending));
    e.managed = true;
    assert!(filter.accepts(&e, &pending));
    e.managed = false;
    e.current = None;
    pending.set(&e, Change::Set(Value::Bool(true)));
    assert!(filter.accepts(&e, &pending));
}

#[test]
fn category_rail_requires_an_exact_category_match() {
    let e = entry("feature", Stage::Stable, Kind::Bool);
    for (rail, accepted) in [
        (Rail::All, true),
        (Rail::Category("general".into()), true),
        (Rail::Category("General".into()), false),
    ] {
        let filter = Filter {
            rail,
            ..Filter::default()
        };
        assert_eq!(filter.accepts(&e, &Pending::default()), accepted);
    }
}

#[test]
fn common_rail_lists_curated_entries_in_their_order_without_unknown_or_reserved_keys() {
    let mut b = entry("b_setting", Stage::Stable, Kind::Bool);
    b.common = Some(0);
    let mut a = entry("a_setting", Stage::Experimental, Kind::Bool);
    a.common = Some(1);
    let mut unknown = entry("new_setting", Stage::Stable, Kind::Bool);
    unknown.common = Some(2);
    unknown.availability = vec![
        (TargetKey::App, Availability::Yes),
        (TargetKey::Cli, Availability::No),
    ];
    let mut reserved = entry("service_tier", Stage::Stable, Kind::Text { path: false });
    reserved.common = Some(3);
    reserved.owner = Some(ReservedOwner::ModelDefaults);
    let plain = entry("plain", Stage::Stable, Kind::Bool);
    let model = model(vec![a, b, unknown, reserved, plain]);
    let filter = Filter {
        rail: Rail::Common,
        ..Filter::default()
    };
    let pending = Pending::default();
    let listed: Vec<&str> = model
        .listed(&filter, &pending)
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(listed, vec!["b_setting", "a_setting"]);
    assert_eq!(model.counts(&filter, &pending).common, 2);
}

#[test]
fn search_covers_every_entry_and_a_selected_category_narrows_it() {
    let mut a = entry("web_search", Stage::Stable, Kind::Bool);
    a.category = "tools".into();
    let mut b = entry(
        "tools.web_search.allowed_domains",
        Stage::Stable,
        Kind::Bool,
    );
    b.category = "tools".into();
    let mut c = entry("history.persistence", Stage::Stable, Kind::Bool);
    c.text.en = Some("Persist the web search history".into());
    let model = model(vec![c, b, a]);
    let pending = Pending::default();
    // The Common rail does not hide search results.
    let filter = Filter {
        query: "web search".into(),
        rail: Rail::Common,
        ..Filter::default()
    };
    let listed: Vec<&str> = model
        .listed(&filter, &pending)
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    // Exact path first, then the path prefix, then explanation-only matches.
    assert_eq!(
        listed,
        vec![
            "web_search",
            "tools.web_search.allowed_domains",
            "history.persistence"
        ]
    );
    let counts = model.counts(&filter, &pending);
    assert_eq!(counts.results, 3);
    assert_eq!(counts.categories.get("tools"), Some(&2));
    let narrowed = Filter {
        rail: Rail::Category("tools".into()),
        ..filter
    };
    assert_eq!(model.listed(&narrowed, &pending).len(), 2);
}

#[test]
fn search_ranks_names_before_text_matches() {
    let mut named = entry("features.memories", Stage::Stable, Kind::Bool);
    named.name_zh = Some("记忆".into());
    let mut texted = entry(
        "history.max_bytes",
        Stage::Stable,
        Kind::Integer {
            min: None,
            max: None,
        },
    );
    texted.text.zh = Some("会话记忆文件的大小上限".into());
    assert_eq!(named.rank("记忆"), 2);
    assert_eq!(texted.rank("记忆"), 4);
    assert_eq!(named.rank("features.memories"), 0);
    assert_eq!(named.rank("features.mem"), 1);
    assert_eq!(texted.rank("max"), 3);
}

#[test]
fn search_matches_path_name_and_both_explanations_case_insensitively() {
    let mut e = entry("features.fast_mode", Stage::Stable, Kind::Bool);
    e.name = Some("Quick Replies".into());
    e.text.en = Some("Lower LATENCY".into());
    e.text.zh = Some("更快的中文回复".into());
    e.name_zh = Some("快速模式".into());
    for query in ["FAST_MODE", "qUiCk", "latency", "中文", "快速"] {
        let filter = Filter {
            query: query.into(),
            ..Filter::default()
        };
        assert!(filter.accepts(&e, &Pending::default()), "{query}");
    }
}

#[test]
fn search_requires_every_word_even_across_different_fields() {
    let mut e = entry("features.fast_mode", Stage::Stable, Kind::Bool);
    e.name = Some("Quick Replies".into());
    e.text.en = Some("Lower latency".into());
    e.text.zh = Some("中文回复".into());
    for (query, accepted) in [
        ("  FAST_MODE\tquick\nlatency 中文  ", true),
        ("quick missing", false),
        ("missing 中文", false),
        (" \t\n", true),
    ] {
        let filter = Filter {
            query: query.into(),
            ..Filter::default()
        };
        assert_eq!(filter.accepts(&e, &Pending::default()), accepted, "{query}");
    }
}

#[test]
fn counts_cover_common_changed_and_categories_whatever_rail_is_selected() {
    let mut a = entry("a", Stage::Stable, Kind::Bool);
    a.common = Some(0);
    let mut b = entry("b", Stage::Stable, Kind::Bool);
    b.category = "model".into();
    b.managed = true;
    b.current = Some(Value::Bool(true));
    let hidden = entry("hidden", Stage::UnderDevelopment, Kind::Bool);
    let model = model(vec![a, b, hidden]);
    let filter = Filter {
        rail: Rail::Category("general".into()),
        ..Filter::default()
    };
    let counts = model.counts(&filter, &Pending::default());
    assert_eq!(counts.common, 1);
    assert_eq!(counts.changed, 1);
    assert_eq!(
        counts.categories,
        BTreeMap::from([("general".into(), 1), ("model".into(), 1)])
    );
    let advanced = Filter {
        advanced: true,
        ..filter
    };
    assert_eq!(
        model.counts(&advanced, &Pending::default()).categories["general"],
        2
    );
}

#[test]
fn titles_are_chinese_first_with_the_path_below_and_a_badge_only_without_chinese() {
    let mut e = entry("approval_policy", Stage::Stable, Kind::Bool);
    e.name_zh = Some("审批策略".into());
    e.text.en = Some("Default approval policy for executing commands. More text.".into());
    let t = e.title_for(true);
    assert_eq!(
        (t.text, t.is_path, t.zh_missing),
        ("审批策略", false, false)
    );
    assert_eq!(e.subtitle_for(true).as_deref(), Some("approval_policy"));
    // English: no official name, so the path and the first English sentence.
    let t = e.title_for(false);
    assert_eq!(
        (t.text, t.is_path, t.zh_missing),
        ("approval_policy", true, false)
    );
    assert_eq!(
        e.subtitle_for(false).as_deref(),
        Some("Default approval policy for executing commands.")
    );
    // Without a Chinese name the path stands in, flagged for the badge.
    e.name_zh = None;
    let t = e.title_for(true);
    assert_eq!(
        (t.text, t.is_path, t.zh_missing),
        ("approval_policy", true, true)
    );
    // An official English name is the English title.
    e.name = Some("Approval policy".into());
    assert_eq!(e.title_for(false).text, "Approval policy");
    assert_eq!(e.subtitle_for(false).as_deref(), Some("approval_policy"));
}

#[test]
fn first_sentences_stop_at_the_first_full_stop_or_line_break() {
    assert_eq!(first_sentence("One. Two."), "One.");
    assert_eq!(first_sentence("第一句。第二句。"), "第一句。");
    assert_eq!(first_sentence("Line one\nline two"), "Line one");
    assert_eq!(first_sentence("Version 1.2 is out"), "Version 1.2 is out");
    let long = "a".repeat(200);
    assert_eq!(first_sentence(&long).chars().count(), 140);
}

#[test]
fn reserved_keys_are_read_only_and_needs_version_names_the_first_version() {
    let mut e = entry(
        "model_context_window",
        Stage::Stable,
        Kind::Integer {
            min: None,
            max: None,
        },
    );
    e.owner = Some(ReservedOwner::ModelDefaults);
    assert!(e.reserved());
    assert!(!e.editable());
    let mut f = entry("features.memories", Stage::Stable, Kind::Bool);
    f.since = Some("0.163.0".into());
    assert_eq!(f.needs_version(), None);
    f.availability = vec![
        (TargetKey::App, Availability::Yes),
        (TargetKey::Cli, Availability::No),
    ];
    assert_eq!(f.needs_version(), Some("0.163.0"));
}

#[test]
fn bool_parser_accepts_only_trimmed_boolean_literals() {
    assert_eq!(Kind::Bool.parse(" true\n"), Ok(Value::Bool(true)));
    assert_eq!(Kind::Bool.parse("false"), Ok(Value::Bool(false)));
    for input in ["TRUE", "False", "1", "yes", ""] {
        assert_eq!(Kind::Bool.parse(input), Err(ParseError::NotBool));
    }
}

#[test]
fn enum_parser_accepts_only_listed_values() {
    let kind = Kind::Enum {
        options: ["live", "cached"]
            .map(|value| EnumOption {
                value: value.into(),
                en: None,
                zh: None,
            })
            .to_vec(),
        table_form: true,
    };
    for input in [" live ", "cached"] {
        assert_eq!(kind.parse(input), Ok(Value::Str(input.trim().into())));
    }
    for input in ["LIVE", "offline", "{ mode = 'live' }", ""] {
        assert_eq!(kind.parse(input), Err(ParseError::NotAnOption));
    }
}

#[test]
fn integer_parser_checks_inclusive_optional_bounds() {
    let bounded = Kind::Integer {
        min: Some(-2),
        max: Some(3),
    };
    for n in [-2, 0, 3] {
        assert_eq!(bounded.parse(&format!(" {n} ")), Ok(Value::Int(n)));
    }
    assert_eq!(bounded.parse("-3"), Err(ParseError::BelowMinimum));
    assert_eq!(bounded.parse("4"), Err(ParseError::AboveMaximum));
    for (min, max, n) in [
        (None, None, i64::MIN),
        (None, None, i64::MAX),
        (Some(0), None, i64::MAX),
        (None, Some(0), i64::MIN),
    ] {
        assert_eq!(
            Kind::Integer { min, max }.parse(&n.to_string()),
            Ok(Value::Int(n))
        );
    }
}

#[test]
fn integer_parser_rejects_fractional_invalid_and_overflowing_input() {
    let kind = Kind::Integer {
        min: None,
        max: None,
    };
    for input in [
        "",
        "one",
        "1.5",
        "1e2",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        assert_eq!(kind.parse(input), Err(ParseError::NotInteger), "{input}");
    }
}

#[test]
fn number_parser_checks_inclusive_optional_bounds() {
    let kind = Kind::Number {
        min: Some(-1.5),
        max: Some(2.5),
    };
    for (input, value) in [(" -1.5 ", -1.5), ("0", 0.0), ("2.5e0", 2.5)] {
        assert_eq!(kind.parse(input), Ok(Value::Float(value)));
    }
    assert_eq!(kind.parse("-1.6"), Err(ParseError::BelowMinimum));
    assert_eq!(kind.parse("2.6"), Err(ParseError::AboveMaximum));
    for (min, max, input, value) in [
        (None, None, "1e100", 1e100),
        (Some(0.0), None, "1e100", 1e100),
        (None, Some(0.0), "-1e100", -1e100),
    ] {
        assert_eq!(
            Kind::Number { min, max }.parse(input),
            Ok(Value::Float(value))
        );
    }
}

#[test]
fn number_parser_rejects_nonfinite_and_invalid_input() {
    let kind = Kind::Number {
        min: None,
        max: None,
    };
    for input in [
        "NaN", "nan", "inf", "+inf", "-inf", "Infinity", "1e999", "", "many",
    ] {
        assert_eq!(kind.parse(input), Err(ParseError::NotNumber), "{input}");
    }
}

#[test]
fn text_parser_trims_text_and_rejects_blank_input() {
    for path in [false, true] {
        let kind = Kind::Text { path };
        assert_eq!(
            kind.parse("  a b/中文\n"),
            Ok(Value::Str("a b/中文".into()))
        );
        for input in ["", " \t\n"] {
            assert_eq!(kind.parse(input), Err(ParseError::Empty));
        }
    }
}

#[test]
fn readonly_parser_always_rejects_input() {
    for input in ["", "true", "42", "[1, 2]", "text"] {
        assert_eq!(Kind::ReadOnly.parse(input), Err(ParseError::ReadOnly));
    }
}

#[test]
fn parse_errors_have_distinct_settings_translation_keys() {
    let cases = [
        (ParseError::NotBool, "codex_settings.invalid.bool"),
        (ParseError::NotAnOption, "codex_settings.invalid.option"),
        (ParseError::NotInteger, "codex_settings.invalid.integer"),
        (ParseError::NotNumber, "codex_settings.invalid.number"),
        (ParseError::BelowMinimum, "codex_settings.invalid.minimum"),
        (ParseError::AboveMaximum, "codex_settings.invalid.maximum"),
        (ParseError::Empty, "codex_settings.invalid.empty"),
        (ParseError::ReadOnly, "codex_settings.invalid.read_only"),
    ];
    for (error, key) in cases {
        assert_eq!(error.key(), key);
    }
    assert_eq!(
        cases
            .map(|(error, _)| error.key())
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        cases.len()
    );
}

#[test]
fn pending_set_drops_an_edit_back_to_the_configured_value() {
    let mut e = entry("feature", Stage::Stable, Kind::Bool);
    e.current = Some(Value::Bool(false));
    let mut pending = Pending::default();
    pending.set(&e, Change::Set(Value::Bool(true)));
    assert_eq!(pending.get(&e.path), Some(&Change::Set(Value::Bool(true))));
    pending.set(&e, Change::Set(Value::Bool(false)));
    assert_eq!(pending.get(&e.path), None);
}

#[test]
fn pending_unset_drops_an_edit_when_the_key_is_already_unset() {
    let e = entry("feature", Stage::Stable, Kind::Bool);
    let mut pending = Pending::default();
    pending.set(&e, Change::Set(Value::Bool(true)));
    pending.set(&e, Change::Unset);
    assert!(pending.is_empty());
}

#[test]
fn pending_reset_removes_the_configured_key_instead_of_setting_the_default() {
    let mut e = entry("feature", Stage::Stable, Kind::Bool);
    e.current = Some(Value::Bool(true));
    e.default = Some(Value::Bool(false));
    let mut pending = Pending::default();
    pending.reset(&e);
    assert_eq!(pending.get(&e.path), Some(&Change::Unset));
}

#[test]
fn pending_discard_removes_only_the_named_path() {
    let a = entry("a", Stage::Stable, Kind::Bool);
    let b = entry("b", Stage::Stable, Kind::Bool);
    let mut pending = Pending::default();
    pending.set(&a, Change::Set(Value::Bool(true)));
    pending.set(&b, Change::Set(Value::Bool(false)));
    pending.discard("a");
    pending.discard("missing");
    assert_eq!(pending.get("a"), None);
    assert_eq!(pending.get("b"), Some(&Change::Set(Value::Bool(false))));
    assert_eq!(pending.len(), 1);
}

#[test]
fn pending_length_tracks_distinct_paths_and_clear_empties_it() {
    let a = entry("a", Stage::Stable, Kind::Bool);
    let b = entry("b", Stage::Stable, Kind::Bool);
    let mut pending = Pending::default();
    assert_eq!(pending.len(), 0);
    assert!(pending.is_empty());
    pending.set(&a, Change::Set(Value::Bool(true)));
    pending.set(&a, Change::Set(Value::Bool(false)));
    assert_eq!(pending.len(), 1);
    assert!(!pending.is_empty());
    pending.set(&b, Change::Set(Value::Bool(true)));
    assert_eq!(pending.len(), 2);
    pending.clear();
    assert_eq!(pending.len(), 0);
    assert!(pending.is_empty());
    assert_eq!(pending.iter().count(), 0);
    assert_eq!(pending.get("a"), None);
    assert_eq!(pending.get("b"), None);
}

#[test]
fn pending_iteration_is_sorted_by_path() {
    let mut pending = Pending::default();
    for path in ["z.last", "a.first", "m.middle"] {
        pending.set(
            &entry(path, Stage::Stable, Kind::Bool),
            Change::Set(Value::Bool(true)),
        );
    }
    assert_eq!(
        pending
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        ["a.first", "m.middle", "z.last"]
    );
}

#[test]
fn pending_set_path_keeps_unknown_sets_and_unsets() {
    let model = Model::default();
    let mut pending = Pending::default();
    pending.set_path(&model, "unknown.set", Change::Set(Value::Int(7)));
    pending.set_path(&model, "unknown.unset", Change::Unset);
    assert_eq!(
        pending.get("unknown.set"),
        Some(&Change::Set(Value::Int(7)))
    );
    assert_eq!(pending.get("unknown.unset"), Some(&Change::Unset));
    assert_eq!(pending.len(), 2);
}

#[test]
fn pending_set_path_normalizes_known_entries() {
    let mut e = entry("known", Stage::Stable, Kind::Bool);
    e.current = Some(Value::Bool(true));
    let model = model(vec![e]);
    let mut pending = Pending::default();
    pending.set_path(&model, "known", Change::Unset);
    assert_eq!(pending.len(), 1);
    pending.set_path(&model, "known", Change::Set(Value::Bool(true)));
    assert!(pending.is_empty());
}

#[test]
fn pending_rebase_drops_applied_edits_and_keeps_remaining_changes() {
    let mut pending = Pending::default();
    for (path, change) in [
        ("applied_set", Change::Set(Value::Bool(true))),
        ("applied_unset", Change::Unset),
        ("different", Change::Set(Value::Bool(false))),
        ("new_set", Change::Set(Value::Bool(true))),
        ("still_set", Change::Unset),
        ("unknown_set", Change::Set(Value::Int(1))),
        ("unknown_unset", Change::Unset),
    ] {
        pending.set_path(&Model::default(), path, change);
    }
    let entries = [
        "applied_set",
        "applied_unset",
        "different",
        "new_set",
        "still_set",
    ]
    .map(|path| {
        let mut e = entry(path, Stage::Stable, Kind::Bool);
        if matches!(path, "applied_set" | "different" | "still_set") {
            e.current = Some(Value::Bool(true));
        }
        e
    });
    pending.rebase(&model(entries.to_vec()));
    assert_eq!(
        pending
            .iter()
            .map(|(path, change)| (path.as_str(), change.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("different", Change::Set(Value::Bool(false))),
            ("new_set", Change::Set(Value::Bool(true))),
            ("still_set", Change::Unset),
            ("unknown_set", Change::Set(Value::Int(1))),
            ("unknown_unset", Change::Unset),
        ]
    );
}

#[test]
fn value_with_applies_pending_set_and_unset_over_current() {
    let mut e = entry("feature", Stage::Stable, Kind::Bool);
    let mut pending = Pending::default();
    assert_eq!(e.value_with(&pending), None);
    e.current = Some(Value::Bool(false));
    assert_eq!(e.value_with(&pending), Some(&Value::Bool(false)));
    pending.set(&e, Change::Set(Value::Bool(true)));
    assert_eq!(e.value_with(&pending), Some(&Value::Bool(true)));
    pending.reset(&e);
    assert_eq!(e.value_with(&pending), None);
}

#[test]
fn shown_value_falls_back_to_default_after_unset() {
    let mut e = entry("feature", Stage::Stable, Kind::Bool);
    e.default = Some(Value::Bool(false));
    let mut pending = Pending::default();
    assert_eq!(e.shown_value(&pending), Some(&Value::Bool(false)));
    e.current = Some(Value::Bool(true));
    assert_eq!(e.shown_value(&pending), Some(&Value::Bool(true)));
    pending.set(&e, Change::Set(Value::Bool(false)));
    assert_eq!(e.shown_value(&pending), Some(&Value::Bool(false)));
    pending.reset(&e);
    assert_eq!(e.shown_value(&pending), Some(&Value::Bool(false)));
    e.default = None;
    assert_eq!(e.shown_value(&pending), None);
}

#[test]
fn differs_from_default_uses_the_pending_configuration() {
    for (current, default, differs) in [
        (None, None, false),
        (None, Some(Value::Bool(false)), false),
        (Some(Value::Bool(false)), Some(Value::Bool(false)), false),
        (Some(Value::Bool(true)), Some(Value::Bool(false)), true),
        (Some(Value::Bool(true)), None, true),
    ] {
        let mut e = entry("feature", Stage::Stable, Kind::Bool);
        e.current = current;
        e.default = default;
        let mut pending = Pending::default();
        assert_eq!(e.differs_from_default(&pending), differs);
        pending.set(&e, Change::Set(Value::Bool(true)));
        assert!(e.differs_from_default(&pending));
        pending.reset(&e);
        assert!(!e.differs_from_default(&pending));
    }
}

#[test]
fn runtime_mismatch_compares_effective_with_the_file_or_default() {
    for (file, default, effective, mismatch) in [
        (Some(true), Some(false), Some(false), true),
        (Some(true), Some(false), Some(true), false),
        (None, Some(false), Some(true), true),
        (None, Some(false), Some(false), false),
        (Some(true), Some(false), None, false),
        (None, None, Some(true), false),
    ] {
        let mut e = entry("features.fast_mode", Stage::Stable, Kind::Bool);
        e.id = "feature:fast_mode".into();
        e.file_value = file.map(Value::Bool);
        e.current = file.map(Value::Bool);
        e.default = default.map(Value::Bool);
        e.effective = effective;
        assert_eq!(e.runtime_mismatch(), mismatch, "{e:?}");
    }
    // A saved but unapplied profile value is not what Codex read: no mismatch
    // (a cancelled switch is not an outdated catalog).
    let mut e = entry("features.fast_mode", Stage::Stable, Kind::Bool);
    e.id = "feature:fast_mode".into();
    e.current = Some(Value::Bool(false));
    e.file_value = None;
    e.default = Some(Value::Bool(true));
    e.effective = Some(true);
    assert!(!e.runtime_mismatch(), "{e:?}");
}

#[test]
fn migrations_include_only_configured_legacy_keys_with_replacements() {
    let migratable = legacy_entry("old", Some("new"));
    let no_replacement = legacy_entry("obsolete", None);
    let mut unset = legacy_entry("unset", Some("new"));
    unset.current = None;
    let mut ordinary = entry("ordinary", Stage::Stable, Kind::Bool);
    ordinary.current = Some(Value::Bool(true));
    let model = model(vec![ordinary, no_replacement, migratable, unset]);
    assert_eq!(
        model
            .migrations()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        ["old"]
    );
}

#[test]
fn migration_changes_prefer_the_canonical_value() {
    let mut e = legacy_entry("web_search_request", Some("web_search"));
    e.legacy.as_mut().unwrap().canonical_value = Some(Value::Str("live".into()));
    let canonical = entry("web_search", Stage::Stable, Kind::Text { path: false });
    let model = model(vec![e.clone(), canonical]);
    assert_eq!(
        model.migration_changes(&e),
        vec![
            ("web_search_request".into(), Change::Unset),
            ("web_search".into(), Change::Set(Value::Str("live".into()))),
        ]
    );
}

#[test]
fn migration_changes_fall_back_to_the_legacy_value() {
    let e = legacy_entry("old", Some("new"));
    assert_eq!(
        model(vec![e.clone()]).migration_changes(&e),
        vec![
            ("old".into(), Change::Unset),
            ("new".into(), Change::Set(Value::Bool(true))),
        ]
    );
}

#[test]
fn migration_changes_do_not_overwrite_a_configured_canonical_key() {
    let e = legacy_entry("old", Some("new"));
    let mut canonical = entry("new", Stage::Stable, Kind::Bool);
    canonical.current = Some(Value::Bool(false));
    assert_eq!(
        model(vec![e.clone(), canonical]).migration_changes(&e),
        vec![("old".into(), Change::Unset)]
    );
}

#[test]
fn migration_changes_for_an_ordinary_entry_are_empty() {
    let e = entry("ordinary", Stage::Stable, Kind::Bool);
    assert!(model(vec![e.clone()]).migration_changes(&e).is_empty());
}

#[test]
fn diff_reports_before_and_after_values_in_path_order() {
    let mut a = entry("a.set", Stage::Stable, Kind::Bool);
    a.current = Some(Value::Bool(false));
    let mut z = entry("z.unset", Stage::Stable, Kind::Bool);
    z.current = Some(Value::Bool(true));
    let mut pending = Pending::default();
    pending.reset(&z);
    pending.set(&a, Change::Set(Value::Bool(true)));
    let model = model(vec![z, a]);
    assert_eq!(
        diff(&model, &pending),
        vec![
            DiffLine {
                path: "a.set".into(),
                before: Some(Value::Bool(false)),
                after: Some(Value::Bool(true)),
                unsupported: vec![]
            },
            DiffLine {
                path: "z.unset".into(),
                before: Some(Value::Bool(true)),
                after: None,
                unsupported: vec![]
            },
        ]
    );
}

#[test]
fn diff_lists_only_unsupported_targets_in_target_order() {
    let mut e = entry("feature", Stage::Stable, Kind::Bool);
    e.availability = vec![
        (TargetKey::Cli, Availability::No),
        (TargetKey::App, Availability::Yes),
        (TargetKey::Remote("unknown".into()), Availability::Unknown),
        (TargetKey::Remote("old".into()), Availability::No),
    ];
    let mut pending = Pending::default();
    pending.set(&e, Change::Set(Value::Bool(true)));
    assert_eq!(
        diff(&model(vec![e]), &pending)[0].unsupported,
        vec![TargetKey::Cli, TargetKey::Remote("old".into())]
    );
}

#[test]
fn diff_for_unknown_paths_has_no_before_value_or_unsupported_targets() {
    let model = Model::default();
    let mut pending = Pending::default();
    pending.set_path(&model, "new", Change::Set(Value::Int(9)));
    pending.set_path(&model, "old", Change::Unset);
    assert_eq!(
        diff(&model, &pending),
        vec![
            DiffLine {
                path: "new".into(),
                before: None,
                after: Some(Value::Int(9)),
                unsupported: vec![]
            },
            DiffLine {
                path: "old".into(),
                before: None,
                after: None,
                unsupported: vec![]
            },
        ]
    );
}

#[test]
fn value_display_quotes_strings_and_renders_other_values_verbatim() {
    for (value, expected) in [
        (Value::Str("live".into()), "\"live\""),
        (
            Value::Str("line\n\"quoted\"".into()),
            "\"line\\n\\\"quoted\\\"\"",
        ),
        (Value::Bool(true), "true"),
        (Value::Bool(false), "false"),
        (Value::Int(-42), "-42"),
        (Value::Float(1.5), "1.5"),
        (
            Value::Other("[1, { key = 'value' }]".into()),
            "[1, { key = 'value' }]",
        ),
    ] {
        assert_eq!(value.to_string(), expected);
    }
}

#[test]
fn only_settings_no_codex_knows_are_version_blocked() {
    let mut e = entry("features.fast_mode", Stage::Stable, Kind::Bool);
    assert!(!e.version_blocked(), "no availability data");
    e.availability = vec![
        (TargetKey::App, Availability::No),
        (TargetKey::Cli, Availability::No),
    ];
    assert!(e.version_blocked());
    e.availability = vec![
        (TargetKey::App, Availability::Yes),
        (TargetKey::Cli, Availability::No),
    ];
    assert!(!e.version_blocked(), "written for the app");
    e.availability = vec![(TargetKey::App, Availability::Unknown)];
    assert!(!e.version_blocked());
}
