#![allow(missing_docs)]

//! v0.3 contract additions: serde round trips and defaults of every new type,
//! and the pure helpers.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use superba_core::codex::{self, config_keys};
use superba_core::*;

/// Serialize, deserialize, compare; returns the JSON.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> serde_json::Value {
    let encoded = serde_json::to_value(value).unwrap();
    let decoded: T = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(&decoded, value, "round trip of {encoded}");
    encoded
}

/// `{}` deserializes to `T::default()`.
fn empty_is_default<T: DeserializeOwned + Default + PartialEq + Debug>() {
    let decoded: T = serde_json::from_value(json!({})).unwrap();
    assert_eq!(decoded, T::default());
}

fn ts(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn v(s: &str) -> CodexVersion {
    s.parse().unwrap()
}

fn usd(dollars: f64) -> Usd {
    Usd::from_dollars(dollars).unwrap()
}

fn host(alias: &str, sources: Vec<HostSource>) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(alias),
        label: alias.into(),
        host_id: Some(format!("host-{alias}")),
        kind: superba_core::HostKind::Ssh,
        ssh: SshEndpoint {
            alias: Some(alias.into()),
            ..SshEndpoint::default()
        },
        sources,
        connected: Some(true),
        wsl: None,
    }
}

fn astra() -> ModelSpec {
    ModelSpec {
        slug: "gpt-6-astra".into(),
        display_name: Some("GPT-6 Astra".into()),
        description: Some("frontier".into()),
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: Some(1),
        context_window: Some(272_000),
        max_context_window: Some(872_000),
        effective_context_window_percent: Some(95),
        auto_compact_token_limit: None,
        reasoning_levels: vec![ReasoningLevel {
            effort: "high".into(),
            description: Some("thinks longer".into()),
        }],
        default_reasoning_level: Some("high".into()),
        service_tiers: vec![
            ServiceTierSpec {
                id: "priority".into(),
                name: "Fast".into(),
                description: "2x speed".into(),
            },
            ServiceTierSpec {
                id: "ultrafast".into(),
                name: "Ultrafast".into(),
                description: "even faster".into(),
            },
        ],
        default_service_tier: Some("priority".into()),
        input_modalities: vec!["text".into(), "image".into()],
        raw: Some(json!({"slug":"gpt-6-astra","zeta_unknown":{"kept":true},"alpha":1})),
    }
}

fn row() -> RequestRow {
    RequestRow {
        id: 42,
        started_at: ts(1_760_000_000_000),
        prompt_cache_key: Some("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into()),
        thread_id: Some("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into()),
        turn_id: Some("turn-1".into()),
        member_id: ProfileId::new("krill-work"),
        member_name: "Krill".into(),
        line_id: Some("overseas".into()),
        line_name: Some("Overseas".into()),
        upstream_host: Some("api.example.com".into()),
        failovers: 1,
        transport: RequestTransport::WebSocket,
        originator: Some(codex::ORIGINATOR_DESKTOP.into()),
        client: RequestClient::Desktop,
        request_kind: Some(RequestKind::Turn),
        thread_source: Some("user".into()),
        model: Some("gpt-6-astra".into()),
        upstream_model: None,
        response_model: Some("gpt-6-astra-2026-10-01".into()),
        service_tier_requested: Some("priority".into()),
        service_tier_answered: Some("priority".into()),
        status: RequestStatus::Ok,
        http_status: Some(200),
        error: None,
        input_tokens: Some(4002),
        cached_tokens: Some(3328),
        cache_write_tokens: Some(10),
        output_tokens: Some(51),
        reasoning_tokens: Some(0),
        ttfb_ms: Some(300),
        first_token_ms: Some(820),
        duration_ms: Some(2920),
        tps_milli: Some(87_000),
        cost: Some(usd(0.0123)),
        price_rev: Some("2026-10-16".into()),
        injected: true,
        stripped: 2,
        upstream_request_id: Some("req_123".into()),
        source: superba_core::RequestSource::Router,
        codex_host: None,
        harness: superba_core::RequestHarness::Codex,
        harness_version: None,
        response_id: None,
        protocol: superba_core::RequestProtocol::Responses,
        requested_model: None,
        session_id: None,
        agent_kind: None,
    }
}

// ---------------------------------------------------------------------------
// codex.rs constants
// ---------------------------------------------------------------------------

#[test]
fn v3_codex_facts_have_the_verified_values() {
    assert_eq!(codex::ENV_CODEX_CLI_PATH, "CODEX_CLI_PATH");
    assert_eq!(codex::ENV_CODEX_INSTALL_DIR, "CODEX_INSTALL_DIR");
    assert_eq!(config_keys::MODEL_CATALOG_JSON, "model_catalog_json");
    assert_eq!(config_keys::MODEL_CONTEXT_WINDOW, "model_context_window");
    assert_eq!(
        config_keys::MODEL_AUTO_COMPACT_TOKEN_LIMIT,
        "model_auto_compact_token_limit"
    );
    assert_eq!(config_keys::SERVICE_TIER, "service_tier");
    assert_eq!(codex::APP_SERVER_FLOOR_FALLBACK, v("0.141.0"));
    assert_eq!(codex::ORIGINATOR_DESKTOP, "Codex Desktop");
    assert_eq!(codex::ORIGINATOR_TUI, "codex_cli_rs");
    assert_eq!(codex::ORIGINATOR_EXEC, "codex_exec");
    // ⚡ (U+26A1) followed by WORD JOINER (U+2060), as UTF-8 bytes.
    assert_eq!(
        codex::STATS_LINE_MARKER.as_bytes(),
        &[0xE2, 0x9A, 0xA1, 0xE2, 0x81, 0xA0]
    );
}

#[test]
fn model_keys_are_reserved_for_the_models_page() {
    for key in [
        config_keys::MODEL_CATALOG_JSON,
        config_keys::MODEL_CONTEXT_WINDOW,
        config_keys::MODEL_AUTO_COMPACT_TOKEN_LIMIT,
        config_keys::SERVICE_TIER,
    ] {
        assert_eq!(
            reserved_owner(key),
            Some(ReservedOwner::ModelDefaults),
            "{key}"
        );
    }
    // The speed feature flags stay ordinary settings.
    assert_eq!(reserved_owner("features.fast_mode"), None);
    assert_eq!(reserved_owner("features.ultrafast_mode"), None);
}

// ---------------------------------------------------------------------------
// models.rs
// ---------------------------------------------------------------------------

#[test]
fn speed_tier_wire_and_config_values() {
    assert_eq!(
        serde_json::to_value(SpeedTier::Standard).unwrap(),
        "standard"
    );
    assert_eq!(serde_json::to_value(SpeedTier::Fast).unwrap(), "fast");
    assert_eq!(
        serde_json::to_value(SpeedTier::Ultrafast).unwrap(),
        "ultrafast"
    );
    // `default` is the config.toml spelling (from_config_value below), not the
    // JSON one.
    assert!(serde_json::from_value::<SpeedTier>(json!("default")).is_err());
    assert_eq!(SpeedTier::default(), SpeedTier::Standard);

    let config: Vec<_> = SpeedTier::ALL.iter().map(|t| t.config_value()).collect();
    assert_eq!(config, ["default", "fast", "ultrafast"]);
    let request: Vec<_> = SpeedTier::ALL.iter().map(|t| t.request_value()).collect();
    assert_eq!(request, [None, Some("priority"), Some("ultrafast")]);
    let suffix: Vec<_> = SpeedTier::ALL.iter().map(|t| t.model_suffix()).collect();
    assert_eq!(suffix, [None, Some("-fast"), Some("-ultrafast")]);
    for (text, tier) in [
        ("default", Some(SpeedTier::Standard)),
        ("standard", Some(SpeedTier::Standard)),
        ("fast", Some(SpeedTier::Fast)),
        ("priority", Some(SpeedTier::Fast)),
        (" ultrafast ", Some(SpeedTier::Ultrafast)),
        ("flex", None),
        ("", None),
    ] {
        assert_eq!(SpeedTier::from_config_value(text), tier, "{text:?}");
    }
    assert_eq!(
        SpeedTier::from_catalog_id("priority"),
        Some(SpeedTier::Fast)
    );
    assert_eq!(SpeedTier::from_catalog_id("flex"), None);
}

#[test]
fn speed_effect_and_support_round_trip_with_defaults() {
    for effect in [
        SpeedEffect::Applies,
        SpeedEffect::ViaModelName {
            model: "gpt-6-astra-ultrafast".into(),
        },
        SpeedEffect::AppForcesStandard,
        SpeedEffect::NotOffered,
        SpeedEffect::AppMenu,
    ] {
        round_trip(&effect);
    }
    let support = SpeedSupport {
        offered: vec![SpeedTier::Standard, SpeedTier::Fast],
        cli: SpeedEffect::Applies,
        app: SpeedEffect::Applies,
        tiers: vec![TierEffect {
            tier: SpeedTier::Fast,
            cli: SpeedEffect::Applies,
            app: SpeedEffect::AppForcesStandard,
        }],
    };
    let encoded = round_trip(&support);
    assert_eq!(encoded["app"], json!({"kind":"applies"}));
    assert_eq!(
        encoded["tiers"][0]["app"],
        json!({"kind":"app_forces_standard"})
    );
    // A tier's own effect, else the saved tier's (ux-visual #0).
    assert_eq!(
        support.effect(SpeedTier::Fast),
        (&SpeedEffect::Applies, &SpeedEffect::AppForcesStandard)
    );
    assert_eq!(
        support.effect(SpeedTier::Ultrafast),
        (&SpeedEffect::Applies, &SpeedEffect::Applies)
    );
    empty_is_default::<SpeedSupport>();
    assert_eq!(SpeedSupport::default().offered, vec![SpeedTier::Standard]);
    assert!(SpeedSupport::default().tiers.is_empty());
}

#[test]
fn model_spec_round_trips_and_keeps_the_raw_entry_in_order() {
    let spec = astra();
    let encoded = round_trip(&spec);
    let raw = serde_json::to_string(&encoded["raw"]).unwrap();
    assert_eq!(
        raw,
        r#"{"slug":"gpt-6-astra","zeta_unknown":{"kept":true},"alpha":1}"#
    );
    assert_eq!(spec.speed_tiers(), SpeedTier::ALL.to_vec());
    assert_eq!(spec.label(), "GPT-6 Astra");

    // Minimal: only a slug, listed and usable with an API key.
    let minimal: ModelSpec = serde_json::from_value(json!({"slug":"m"})).unwrap();
    assert_eq!(minimal, ModelSpec::new("m"));
    assert!(minimal.supported_in_api);
    assert_eq!(minimal.visibility, ModelVisibility::List);
    assert_eq!(minimal.speed_tiers(), vec![SpeedTier::Standard]);
    assert_eq!(
        serde_json::to_value(&minimal).unwrap(),
        json!({"slug":"m","visibility":"list","supported_in_api":true})
    );
    assert_eq!(ModelSpec::default().slug, "");
    assert!(ModelSpec::default().supported_in_api);
}

#[test]
fn model_visibility_reads_unknown_values_as_other() {
    for (text, vis) in [
        ("list", ModelVisibility::List),
        ("hide", ModelVisibility::Hide),
        ("none", ModelVisibility::None),
        ("experimental", ModelVisibility::Other),
    ] {
        let parsed: ModelVisibility = serde_json::from_value(json!(text)).unwrap();
        assert_eq!(parsed, vis, "{text}");
    }
}

#[test]
fn model_lists_round_trip_every_source() {
    for source in [
        ModelListSource::Upstream {
            path: "/backend-api/codex/models?client_version=0.162.0".into(),
            fetched_at: ts(1),
            etag: Some("\"abc\"".into()),
        },
        ModelListSource::Feed {
            codex_version: v("0.163.0"),
            mirror: "jsdelivr".into(),
            serial: 7,
            fetched_at: Some(ts(2)),
        },
        ModelListSource::Snapshot {
            codex_version: v("0.163.0"),
        },
        ModelListSource::Bundled {
            codex_version: v("0.162.0-alpha.17.2"),
        },
        ModelListSource::OfficialReadOnly,
    ] {
        let list = ModelList {
            source,
            models: vec![astra(), ModelSpec::new("gpt-5.5")],
            fallback_reason: Some(Error::Unsupported {
                what: "site does not list models".into(),
            }),
        };
        round_trip(&list);
        assert_eq!(list.model("gpt-5.5"), Some(&ModelSpec::new("gpt-5.5")));
        assert_eq!(list.offered_tiers(), SpeedTier::ALL.to_vec());
    }
}

#[test]
fn model_overrides_picker_and_catalog_status_round_trip() {
    let full = ModelOverride {
        context_window: Some(400_000),
        auto_compact: Some(300_000),
        display_name: Some("Astra 400K".into()),
        hidden: true,
        upstream_name: Some("astra-upstream".into()),
    };
    round_trip(&full);
    assert!(!full.is_empty());
    assert!(ModelOverride::default().is_empty());
    assert_eq!(
        serde_json::to_value(ModelOverride::default()).unwrap(),
        json!({})
    );
    empty_is_default::<ModelOverride>();

    for picker in [ModelPicker::CodexDefault, ModelPicker::Provider] {
        round_trip(&picker);
    }
    assert_eq!(
        serde_json::to_value(ModelPicker::Provider).unwrap(),
        "provider"
    );
    assert!(ModelPicker::default().is_default());

    for status in [
        CatalogFileStatus::NotNeeded,
        CatalogFileStatus::Written {
            path: PathBuf::from("/home/u/.codex/superba/models-0a1b2c3d.json"),
            sha8: "0a1b2c3d".into(),
            consumer: v("0.162.0-alpha.17.2"),
        },
        CatalogFileStatus::Failed {
            reason: LocalizedText::new("x.y", "missing field shell_type"),
        },
    ] {
        round_trip(&status);
    }
    assert_eq!(CatalogFileStatus::default(), CatalogFileStatus::NotNeeded);
}

#[test]
fn models_view_edit_and_report_round_trip() {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "gpt-6-astra".to_owned(),
        ModelOverride {
            context_window: Some(400_000),
            ..ModelOverride::default()
        },
    );
    let view = ModelsView {
        profile_id: ProfileId::new("krill-work"),
        site_id: SiteId::new("krill"),
        read_only: false,
        list: ModelList {
            source: ModelListSource::Snapshot {
                codex_version: v("0.163.0"),
            },
            models: vec![astra()],
            fallback_reason: None,
        },
        defaults: ModelDefaults {
            model: Some("gpt-6-astra".into()),
            reasoning_effort: Some("high".into()),
            speed: Some(SpeedTier::Ultrafast),
            context_window: Some(500_000),
            auto_compact: Some(400_000),
        },
        overrides: overrides.clone(),
        picker: ModelPicker::Provider,
        picker_available: true,
        speed: SpeedSupport::default(),
        catalog_file: CatalogFileStatus::NotNeeded,
        notes: vec![Notice::warning(LocalizedText::new("a.b", "frozen"))],
        per_model: true,
        applied: Some(AppliedModels {
            context_window: None,
            auto_compact: Some(300_000),
            catalog: Some("/home/u/.codex/superba/models-0a1b2c3d.json".into()),
            models: [(
                "gpt-6-astra".to_owned(),
                AppliedModel {
                    context_window: Some(400_000),
                    max_context_window: Some(872_000),
                    auto_compact: None,
                },
            )]
            .into(),
        }),
    };
    round_trip(&view);
    // `applied` is left out for a profile that is not applied.
    let other = ModelsView {
        applied: None,
        ..view.clone()
    };
    assert!(round_trip(&other).get("applied").is_none());

    let edit = ModelsEdit {
        profile_id: Some(ProfileId::new("krill-work")),
        defaults: Some(ModelDefaults::default()),
        set: overrides,
        reset: vec!["gpt-5.5".into()],
        reset_all: true,
        picker: Some(ModelPicker::CodexDefault),
    };
    round_trip(&edit);
    empty_is_default::<ModelsEdit>();
    empty_is_default::<ModelsViewRequest>();

    let profile = catalog::builtin_profiles().remove(0);
    round_trip(&ModelsEditReport {
        profile,
        notes: Vec::new(),
    });
}

#[test]
fn token_counts_parse_decimal_suffixes() {
    for (text, want) in [
        ("400000", Some(400_000)),
        ("400k", Some(400_000)),
        ("400K", Some(400_000)),
        ("272k", Some(272_000)),
        ("1m", Some(1_000_000)),
        ("1.5m", Some(1_500_000)),
        ("0.5k", Some(500)),
        ("1_000_000", Some(1_000_000)),
        ("1,000", Some(1_000)),
        (" 16k ", Some(16_000)),
        ("", None),
        ("k", None),
        ("1.5", None),
        ("-5", None),
        ("1.0001k", None),
        ("10x", None),
        ("99999999999999999999", None),
    ] {
        assert_eq!(parse_token_count(text), want, "{text:?}");
    }
}

#[test]
fn compaction_limits_parse_and_clamp() {
    assert_eq!(CompactLimit::parse("80%"), Some(CompactLimit::Percent(80)));
    assert_eq!(
        CompactLimit::parse("300k"),
        Some(CompactLimit::Tokens(300_000))
    );
    assert_eq!(CompactLimit::parse("0%"), None);
    assert_eq!(CompactLimit::parse("101%"), None);
    assert_eq!(CompactLimit::Percent(80).tokens(400_000), 320_000);
    assert_eq!(CompactLimit::Tokens(5).tokens(400_000), 5);
    round_trip(&CompactLimit::Percent(80));
    round_trip(&CompactLimit::Tokens(300_000));
    assert_eq!(effective_auto_compact(272_000, None), 244_800);
    assert_eq!(effective_auto_compact(272_000, Some(300_000)), 244_800);
    assert_eq!(effective_auto_compact(272_000, Some(100_000)), 100_000);
    assert_eq!(effective_auto_compact(123_456, None), 111_110);
    assert_eq!(MIN_CONTEXT_WINDOW, 16_000);
    assert_eq!(AUTO_COMPACT_MAX_PERCENT, 90);
    assert_eq!(DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT, 95);
}

// ---------------------------------------------------------------------------
// requests.rs
// ---------------------------------------------------------------------------

#[test]
fn request_rows_round_trip_and_derive_fresh_input() {
    let row = row();
    let encoded = round_trip(&row);
    assert_eq!(encoded["status"], json!({"kind":"ok"}));
    assert_eq!(encoded["transport"], "web_socket");
    assert_eq!(row.fresh_input_tokens(), Some(4002 - 3328 - 10));
    assert_eq!(row.short_key().as_deref(), Some("0199a1b2"));

    // A cancelled request without usage keeps unknowns unknown.
    let bare: RequestRow = serde_json::from_value(json!({
        "id": 1, "started_at": 5, "member_id": "m", "status": {"kind":"cancelled"}
    }))
    .unwrap();
    assert_eq!(bare.input_tokens, None);
    assert_eq!(bare.fresh_input_tokens(), None);
    assert_eq!(bare.cost, None);
    assert_eq!(bare.client, RequestClient::Other);
    assert_eq!(bare.short_key(), None);
}

#[test]
fn request_statuses_round_trip_and_classify_errors() {
    let statuses = [
        RequestStatus::Ok,
        RequestStatus::Http {
            status: 502,
            code: Some("router_unavailable".into()),
        },
        RequestStatus::Failed {
            code: Some("server_error".into()),
        },
        RequestStatus::Incomplete {
            reason: Some("max_output_tokens".into()),
        },
        RequestStatus::Broken,
        RequestStatus::Cancelled,
    ];
    for status in &statuses {
        round_trip(status);
        assert_eq!(status.is_error(), status != &RequestStatus::Ok);
    }
    let kind: RequestKind = serde_json::from_value(json!("guardian_review")).unwrap();
    assert_eq!(kind, RequestKind::Other);
    for k in [
        RequestKind::Turn,
        RequestKind::Prewarm,
        RequestKind::Compaction,
        RequestKind::Memory,
    ] {
        round_trip(&k);
    }
}

#[test]
fn request_clients_come_from_the_originator() {
    assert_eq!(
        RequestClient::from_originator(Some("Codex Desktop")),
        RequestClient::Desktop
    );
    assert_eq!(
        RequestClient::from_originator(Some("codex_cli_rs")),
        RequestClient::Tui
    );
    assert_eq!(
        RequestClient::from_originator(Some("codex_exec")),
        RequestClient::Exec
    );
    assert_eq!(
        RequestClient::from_originator(Some("vscode")),
        RequestClient::Other
    );
    assert_eq!(RequestClient::from_originator(None), RequestClient::Other);
}

#[test]
fn harnesses_come_from_the_user_agent_and_round_trip_their_ids() {
    use superba_core::RequestHarness as H;
    assert_eq!(
        H::from_user_agent("claude-cli/2.1.302 (external, sdk-cli)"),
        Some((H::ClaudeCode, Some("2.1.302".into())))
    );
    assert_eq!(
        H::from_user_agent("codex_cli_rs/0.162.0 (Ubuntu 24.4.0; x86_64) xterm"),
        Some((H::Codex, Some("0.162.0".into())))
    );
    assert_eq!(H::from_user_agent("Codex Desktop"), Some((H::Codex, None)));
    assert_eq!(H::from_user_agent("curl/8.5.0"), None);
    assert_eq!(H::from_user_agent(""), None);
    for h in H::ALL {
        assert_eq!(H::from_id(h.id()), Some(h));
    }
    let row = row();
    assert_eq!(row.harness, H::Codex);
    let mut claude = row.clone();
    claude.harness = H::ClaudeCode;
    let back: RequestRow = serde_json::from_value(serde_json::to_value(&claude).unwrap()).unwrap();
    assert_eq!(back.harness, H::ClaudeCode);
    assert_eq!(claude.client_text().key, "request_harness.claude_code");
}

#[test]
fn request_queries_default_when_absent_and_round_trip() {
    empty_is_default::<RequestQuery>();
    let q = RequestQuery::default();
    assert_eq!(q.sort, RequestSort::default());
    assert!(q.sort.descending);
    assert_eq!(q.sort.key, RequestSortKey::Time);
    assert_eq!(RequestQuery::DEFAULT_LIMIT, 500);
    round_trip(&RequestQuery {
        source: None,
        from: Some(ts(1)),
        to: Some(ts(2)),
        member: Some(ProfileId::new("m")),
        line: Some("cdn".into()),
        model: Some("gpt-6-luna".into()),
        status: RequestStatusFilter::Errors,
        client: Some(RequestClient::Tui),
        group_by: RequestGrouping::Provider,
        group: Some(provider_line_key("krill-work", "Overseas")),
        search: Some("req_".into()),
        after_id: Some(9),
        limit: 50,
        sort: RequestSort {
            key: RequestSortKey::Cost,
            descending: false,
        },
    });
    let partial: RequestQuery = serde_json::from_value(json!({"sort": {"key": "tps"}})).unwrap();
    assert!(partial.sort.descending);
    empty_is_default::<StatsQuery>();
    empty_is_default::<ClearRequest>();
    empty_is_default::<ClearReport>();
    empty_is_default::<RequestTotals>();
    empty_is_default::<RequestTiming>();
    empty_is_default::<RouterStatsStatus>();
}

#[test]
fn rows_group_by_provider_and_line() {
    let mut r = row();
    assert_eq!(r.provider_key(), Some("krill-work"));
    assert_eq!(r.line_label(), Some("Overseas"));
    assert!(r.in_provider_group("krill-work"));
    assert!(r.in_provider_group(&provider_line_key("krill-work", "Overseas")));
    assert!(!r.in_provider_group(&provider_line_key("krill-work", "China Express")));
    assert!(!r.in_provider_group("api.example.com"));
    // Failed before a line answered: the last line tried still names it.
    r.line_id = None;
    assert!(r.in_provider_group(&provider_line_key("krill-work", "Overseas")));
    r.line_name = None;
    assert_eq!(r.line_label(), None);
    assert!(r.in_provider_group("krill-work"));
    // An empty line selects the provider's rows without a line (ux-visual #17).
    assert!(r.in_provider_group(&provider_line_key("krill-work", "")));
    r.line_name = Some("Overseas".into());
    assert!(!r.in_provider_group(&provider_line_key("krill-work", "")));
    r.line_name = None;
    // A request that never reached a member has no provider.
    r.member_id = ProfileId::new("");
    assert_eq!(r.provider_key(), None);
    assert!(!r.in_provider_group(""));
    let key = provider_line_key("krill-work", "China Express");
    assert_eq!(
        split_provider_key(&key),
        ("krill-work", Some("China Express"))
    );
    assert_eq!(split_provider_key("krill-work"), ("krill-work", None));
    assert_eq!(round_trip(&RequestGrouping::Provider), json!("provider"));
    assert_eq!(round_trip(&StatsGroupBy::Provider), json!("provider"));
    // Groups and buckets without lines leave the fields out.
    let plain = round_trip(&RequestGroup {
        key: "k".into(),
        label: "k".into(),
        count: 1,
        last: ts(1),
        upstream_host: None,
        lines: Vec::new(),
    });
    assert_eq!(
        plain,
        json!({"key": "k", "label": "k", "count": 1, "last": 1})
    );
}

#[test]
fn request_pages_details_stats_and_inflight_round_trip() {
    let totals = RequestTotals {
        requests: 3,
        ok: 2,
        errors: 1,
        input_tokens: 9000,
        cached_tokens: 6000,
        cache_write_tokens: 10,
        output_tokens: 300,
        cost: usd(0.05),
        unpriced: 1,
        first_token_rows: 2,
        first_token_ms_sum: 900,
        tps_output_tokens: 300,
        tps_weighted_milli: 300 * 42_000,
    };
    assert_eq!(totals.avg_first_token_ms(), Some(450));
    assert_eq!(totals.tps_milli(), Some(42_000));
    assert_eq!(RequestTotals::default().avg_first_token_ms(), None);
    assert_eq!(RequestTotals::default().tps_milli(), None);
    round_trip(&RequestPage {
        rows: vec![row()],
        totals: totals.clone(),
        groups: vec![RequestGroup {
            key: "krill-work".into(),
            label: "Krill".into(),
            count: 3,
            last: ts(10),
            upstream_host: Some("api.example.com".into()),
            lines: vec![RequestGroup {
                key: provider_line_key("krill-work", "Overseas"),
                label: "Overseas".into(),
                count: 2,
                last: ts(9),
                upstream_host: Some("api.example.com".into()),
                lines: Vec::new(),
            }],
        }],
        truncated: true,
        recording: true,
    });
    round_trip(&RequestDetail {
        row: row(),
        timing: RequestTiming {
            sent_ms: Some(2),
            headers_ms: Some(300),
            first_event_ms: Some(320),
            first_token_ms: Some(820),
            last_delta_ms: Some(2900),
            end_ms: Some(2920),
        },
        attempts: vec![RequestAttempt {
            member_id: ProfileId::new("krill-work"),
            line_name: Some("China Express".into()),
            upstream_host: Some("api-a.example.com".into()),
            status: Some(502),
            outcome: "502".into(),
            elapsed_ms: Some(40),
            compression: Some(superba_core::RequestCompression::RouterZstd),
        }],
        log: vec!["failover after 502".into()],
    });
    round_trip(&RequestStats {
        buckets: vec![StatsBucket {
            key: "krill-work".into(),
            label: "Krill".into(),
            totals: totals.clone(),
            lines: vec![StatsBucket {
                key: provider_line_key("krill-work", "Overseas"),
                label: "Overseas".into(),
                totals: totals.clone(),
                lines: Vec::new(),
            }],
        }],
        daily: vec![StatsBucket {
            key: "2026-10-17".into(),
            label: "10-17".into(),
            totals,
            lines: Vec::new(),
        }],
        totals: RequestTotals::default(),
    });
    round_trip(&StatsQuery {
        source: None,
        from: Some(ts(1)),
        to: None,
        by: StatsGroupBy::Session,
        member: None,
        utc_offset_minutes: 480,
    });
    round_trip(&InflightRequest {
        id: 7,
        started_at: ts(3),
        member_id: ProfileId::new("m"),
        member_name: "M".into(),
        line_name: None,
        model: Some("gpt-6-luna".into()),
        prompt_cache_key: None,
        client: RequestClient::Desktop,
        transport: RequestTransport::Http,
        phase: InflightPhase::Holding,
        output_deltas: 12,
        first_token_ms: Some(480),
        elapsed_ms: 900,
    });
    round_trip(&ClearRequest {
        before: Some(ts(5)),
    });
    round_trip(&ClearReport { rows: 4 });
    round_trip(&RouterStatsStatus {
        recording: true,
        rows_written: 10,
        dropped: 0,
        db_bytes: Some(4096),
    });
}

#[test]
fn multipliers_are_exact_and_serialize_as_numbers() {
    let m = Multiplier::from_f64(0.15).unwrap();
    assert_eq!(m.ppm(), 150_000);
    assert_eq!(serde_json::to_value(m).unwrap(), json!(0.15));
    assert_eq!(
        serde_json::from_value::<Multiplier>(json!(2)).unwrap(),
        Multiplier::from_ppm(2_000_000)
    );
    assert_eq!(
        serde_json::from_value::<Multiplier>(json!("0.5")).unwrap(),
        Multiplier::from_ppm(500_000)
    );
    assert!(serde_json::from_value::<Multiplier>(json!(-1)).is_err());
    assert!(Multiplier::from_f64(f64::NAN).is_none());
    assert_eq!(Multiplier::default(), Multiplier::ONE);
    assert_eq!(Multiplier::from_ppm(2_500_000).apply(usd(1.0)), usd(2.5));
    assert_eq!(m.apply(usd(10.0)), usd(1.5));
    assert_eq!(Multiplier::from_ppm(2_500_000).to_string(), "2.5");
    assert_eq!(Multiplier::ONE.to_string(), "1");
    round_trip(&m);
}

#[test]
fn price_tables_round_trip_with_optional_fields_skipped() {
    let mut tiers = BTreeMap::new();
    tiers.insert("priority".to_owned(), Multiplier::from_ppm(2_000_000));
    let entry = PriceEntry {
        input: usd(1.25),
        cached_input: Some(usd(0.125)),
        cache_write: None,
        output: usd(10.0),
        long_context: vec![LongContextPrice {
            above_input_tokens: 272_000,
            input: usd(2.5),
            cached_input: None,
            cache_write: Some(usd(3.125)),
            output: usd(15.0),
        }],
        tiers,
        tier_prices: BTreeMap::from([(
            "flex".to_owned(),
            TierPrice {
                input: usd(0.625),
                cached_input: Some(usd(0.0625)),
                output: usd(5.0),
                source: Some("https://openai.com/api-fast-mode/".into()),
                ..TierPrice::default()
            },
        )]),
        source: Some("https://openai.com/api/pricing/".into()),
        rev: Some("2026-10-16".into()),
    };
    // The optional parts are left out when empty or unknown (a feed entry with
    // multipliers only, no cache-write price).
    let minimal = json!({
        "input": 1.25, "output": 10.0,
        "long_context": [{"above_input_tokens": 272000, "input": 2.5, "output": 15.0}],
        "tiers": {"priority": 2.0}
    });
    let parsed: PriceEntry = serde_json::from_value(minimal.clone()).unwrap();
    assert!(parsed.tier_prices.is_empty());
    assert_eq!(parsed.long_context[0].cache_write, None);
    assert_eq!(serde_json::to_value(&parsed).unwrap(), minimal);
    let mut table = PriceTable {
        rev: "2026-10-16".into(),
        entries: BTreeMap::new(),
    };
    table.entries.insert("gpt-6-astra".into(), entry.clone());
    round_trip(&table);
    assert_eq!(table.get("gpt-6-astra"), Some(&entry));
    assert_eq!(table.get("unknown"), None);
    let minimal = PriceEntry {
        input: usd(1.0),
        output: usd(2.0),
        ..PriceEntry::default()
    };
    assert_eq!(
        serde_json::to_value(&minimal).unwrap(),
        json!({"input": 1.0, "output": 2.0})
    );
}

// ---------------------------------------------------------------------------
// versions.rs
// ---------------------------------------------------------------------------

fn release() -> CodexRelease {
    let mut sha256 = BTreeMap::new();
    sha256.insert("x86_64-unknown-linux-musl".to_owned(), "ab".repeat(32));
    CodexRelease {
        version: v("0.163.0-alpha.10.2"),
        tag: "rust-v0.163.0-alpha.10.2".into(),
        date: Some(ts(100)),
        prerelease: true,
        bundled_by: vec!["26.1013.31256".into()],
        sha256,
    }
}

fn deb() -> AppPackage {
    AppPackage {
        os: AppOs::Linux,
        kind: AppPackageKind::Deb,
        arch: "amd64".into(),
        url: "https://example.com/pool/chatgpt_26.1013.31256_amd64.deb".into(),
        sha256: Some("cd".repeat(32)),
        size: Some(170_000_000),
        signature: None,
    }
}

#[test]
fn versions_types_round_trip() {
    round_trip(&release());
    let app = AppRelease {
        version: "26.1013.31256".into(),
        date: None,
        bundled_codex: Some(v("0.163.0-alpha.10.2")),
        packages: vec![deb()],
        store_product_id: Some("9PLM9XGG6VKS".into()),
    };
    round_trip(&app);
    for kind in [
        CodexInstallKind::Standalone,
        CodexInstallKind::Npm,
        CodexInstallKind::Homebrew,
        CodexInstallKind::AppBundled,
        CodexInstallKind::SuperbaStore,
        CodexInstallKind::Other,
    ] {
        round_trip(&kind);
    }
    for tier in [
        CompatTier::Same,
        CompatTier::Compatible,
        CompatTier::Older,
        CompatTier::BelowFloor,
        CompatTier::Newer,
    ] {
        round_trip(&tier);
        assert_eq!(tier.is_blocked(), tier == CompatTier::BelowFloor);
    }
    for pin in [
        AppCodexPin::Bundled,
        AppCodexPin::Version {
            version: v("0.162.0"),
        },
        AppCodexPin::Path {
            path: PathBuf::from("/opt/codex/bin/codex"),
        },
    ] {
        round_trip(&pin);
    }
    assert_eq!(
        serde_json::to_value(AppCodexPin::Version {
            version: v("0.162.0")
        })
        .unwrap(),
        json!({"kind":"version","version":"0.162.0"})
    );
    let stored = StoredCodex {
        version: v("0.162.0"),
        target: "x86_64-unknown-linux-musl".into(),
        executable: PathBuf::from("/data/codex/0.162.0-x86_64-unknown-linux-musl/bin/codex"),
        size_bytes: Some(432_000_000),
        installed_at: Some(ts(9)),
        in_use: true,
        pinned: true,
    };
    round_trip(&stored);
    for action in [
        AppUpdateAction::LinuxPackage {
            package: deb(),
            command: "sudo apt install ./chatgpt_26.1013.31256_amd64.deb".into(),
        },
        AppUpdateAction::MacSparkle { package: None },
        AppUpdateAction::WindowsStore {
            store_url: "ms-windows-store://pdp/?ProductId=9PLM9XGG6VKS".into(),
            command: "winget upgrade --id 9PLM9XGG6VKS --source msstore".into(),
        },
        AppUpdateAction::None,
    ] {
        round_trip(&action);
    }
    let view = VersionsView {
        checked_at: Some(ts(1)),
        app: AppVersionsSection {
            install: None,
            latest: Some(app),
            update_available: true,
            action: Some(AppUpdateAction::None),
        },
        app_codex: AppCodexSection {
            supported: true,
            pin: AppCodexPin::Bundled,
            effective_version: Some(v("0.162.0-alpha.17.2")),
            effective_exe: Some(PathBuf::from("/opt/app/resources/codex")),
            floor: Some(v("0.141.0")),
            candidates: vec![CodexCandidate {
                release: release(),
                tier: CompatTier::Compatible,
                stored: false,
            }],
            stored: vec![stored],
            mismatch: Some(LocalizedText::new("a.b", "pinned binary not in use")),
        },
        cli: vec![CliVersionRow {
            binary: CodexBinary {
                path: PathBuf::from("/usr/local/bin/codex"),
                version: Some(v("0.150.1")),
                source: InstallSource::Npm,
                path_rank: Some(0),
            },
            kind: CodexInstallKind::Npm,
            latest: Some(v("0.163.0")),
            action: CliUpdateAction::Command {
                command: "npm i -g @openai/codex@0.163.0".into(),
            },
        }],
        remote: vec![RemoteVersionRow {
            target_id: TargetId::remote("devbox"),
            label: "devbox".into(),
            version: None,
            install_kind: Some(CodexInstallKind::Standalone),
            tier: Some(CompatTier::Older),
            error: Some(Error::Ssh {
                host: "devbox".into(),
                message: "timeout".into(),
            }),
        }],
        notes: Vec::new(),
    };
    round_trip(&view);
    empty_is_default::<VersionsView>();
    for target in [
        InstallTarget::Local,
        InstallTarget::Store,
        InstallTarget::Remote {
            target_id: TargetId::remote("devbox"),
        },
    ] {
        round_trip(&InstallRequest {
            version: v("0.162.0"),
            target: target.clone(),
        });
        round_trip(&InstallReport {
            version: v("0.162.0"),
            target,
            executable: Some("/home/u/.local/bin/codex".into()),
            previous: Some(v("0.150.1")),
            notes: Vec::new(),
        });
    }
    round_trip(&CliUpdateAction::Install {
        version: v("0.163.0"),
    });
    round_trip(&AppDownloadReport {
        package: deb(),
        path: PathBuf::from("/data/downloads/chatgpt.deb"),
        sha256: "cd".repeat(32),
        command: Some("sudo apt install ./chatgpt.deb".into()),
        windows: None,
    });
}

#[test]
fn codex_package_targets_cover_the_published_triples() {
    for (os, arch, target) in [
        ("linux", "x86_64", Some("x86_64-unknown-linux-musl")),
        ("Linux", "amd64", Some("x86_64-unknown-linux-musl")),
        ("linux", "aarch64", Some("aarch64-unknown-linux-musl")),
        ("macos", "aarch64", Some("aarch64-apple-darwin")),
        ("Darwin", "arm64", Some("aarch64-apple-darwin")),
        ("darwin", "x86_64", Some("x86_64-apple-darwin")),
        ("windows", "x86_64", Some("x86_64-pc-windows-msvc")),
        ("windows", "aarch64", Some("aarch64-pc-windows-msvc")),
        ("freebsd", "x86_64", None),
        ("linux", "riscv64", None),
    ] {
        assert_eq!(codex_package_target(os, arch), target, "{os} {arch}");
    }
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(
            local_codex_package_target(),
            Some("x86_64-unknown-linux-musl")
        );
    }
}

// ---------------------------------------------------------------------------
// feeds.rs
// ---------------------------------------------------------------------------

#[test]
fn feed_snapshots_and_status_round_trip() {
    empty_is_default::<FeedSnapshot>();
    empty_is_default::<FeedsStatus>();
    for source in [
        FeedOrigin::Builtin,
        FeedOrigin::Cache,
        FeedOrigin::Mirror {
            name: "jsdelivr".into(),
        },
    ] {
        round_trip(&FeedSnapshot {
            models: vec![astra()],
            prices: PriceTable::default(),
            codex_versions: vec![release()],
            app_versions: Vec::new(),
            serial: 3,
            source: source.clone(),
            fetched_at: Some(ts(4)),
        });
        round_trip(&FeedsStatus {
            serial: 3,
            builtin_serial: 2,
            source,
            fetched_at: Some(ts(4)),
            checked_at: Some(ts(5)),
            mirror: Some("raw".into()),
            online_checks: true,
            last_error: Some(Error::Timeout {
                what: "mirror race".into(),
            }),
            prices_rev: Some("2026-10-01".into()),
        });
    }
}

// ---------------------------------------------------------------------------
// remote.rs, target.rs, sessions.rs
// ---------------------------------------------------------------------------

#[test]
fn only_hosts_the_app_knows_are_app_known() {
    assert!(host("a", vec![HostSource::GlobalState]).is_app_known());
    assert!(host("b", vec![HostSource::SshConfig, HostSource::Cdp]).is_app_known());
    assert!(host("c", vec![HostSource::ProcessScan]).is_app_known());
    assert!(!host("d", vec![HostSource::SshConfig]).is_app_known());
    assert!(!host("e", vec![HostSource::Manual]).is_app_known());
    assert!(!host("f", vec![]).is_app_known());
}

#[test]
fn remote_state_v3_fields_default_when_absent() {
    let state: RemoteState = serde_json::from_value(json!({
        "target_id": "ssh:devbox", "probed_at": 1, "os": "linux", "shell": "bash",
        "codex_home": "/home/u/.codex", "auth": {"kind": "missing"}
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(state.arch, None);
    assert_eq!(state.kernel, None);
    assert_eq!(state.disk_free_kib, None);
    assert_eq!(state.codex_install, None);
    assert_eq!(state.codex_resolved_path, None);
}

#[test]
fn host_sync_types_round_trip() {
    for scope in [
        RemoteApplyScope::Connection,
        RemoteApplyScope::Settings,
        RemoteApplyScope::Both,
    ] {
        round_trip(&scope);
    }
    assert_eq!(RemoteApplyScope::default(), RemoteApplyScope::Both);
    let rows = vec![
        HostSyncRow {
            path: "approval_policy".into(),
            local: Some("\"on-request\"".into()),
            remote: None,
            action: HostSyncAction::Write,
            selected: true,
        },
        HostSyncRow {
            path: "features.memories".into(),
            local: Some("true".into()),
            remote: None,
            action: HostSyncAction::Skip {
                reason: SkipReason::NotSupported {
                    consumers: vec!["devbox 0.150.1".into()],
                },
            },
            selected: false,
        },
        HostSyncRow {
            path: "web_search".into(),
            local: None,
            remote: Some("\"live\"".into()),
            action: HostSyncAction::Remove,
            selected: true,
        },
        HostSyncRow {
            path: "model_verbosity".into(),
            local: Some("\"low\"".into()),
            remote: Some("\"high\"".into()),
            action: HostSyncAction::Conflict,
            selected: false,
        },
    ];
    let encoded = round_trip(&HostSyncPlan {
        target_id: TargetId::remote("devbox"),
        profile_id: Some(ProfileId::new("krill-work")),
        host_version: Some(v("0.150.1")),
        rows,
        blocked: Vec::new(),
        notes: Vec::new(),
    });
    assert_eq!(encoded["rows"][1]["action"]["action"], "skip");
    assert_eq!(
        encoded["rows"][1]["action"]["reason"]["kind"],
        "not_supported"
    );
    round_trip(&HostSyncSelection {
        paths: Some(vec!["approval_policy".into()]),
    });
    empty_is_default::<HostSyncSelection>();
    round_trip(&HostSyncReport {
        target_id: TargetId::remote("devbox"),
        at: ts(1),
        written: vec!["approval_policy".into()],
        removed: vec!["web_search".into()],
        restarted: true,
        notes: Vec::new(),
    });
    round_trip(&RemoteInstallReport {
        target_id: TargetId::remote("devbox"),
        version: v("0.162.0"),
        executable: "/home/u/.local/bin/codex".into(),
        previous: Some(v("0.150.1")),
        at: ts(2),
    });
    round_trip(&HostView {
        host: host("devbox", vec![HostSource::GlobalState]),
        state: None,
        probe_error: Some(Error::Ssh {
            host: "devbox".into(),
            message: "Permission denied".into(),
        }),
        settings: HostSettings {
            follow_switches: true,
            last_sync: None,
            follow_chosen: false,
        },
        pending_sync: Some(3),
        hot_restart: true,
        pending: None,
    });
}

#[test]
fn session_titles_and_host_sessions_round_trip() {
    round_trip(&SessionTitle {
        thread_id: Some("0199".into()),
        title: "Fix the build".into(),
        source: TitleSource::Title,
    });
    let minimal: SessionTitle = serde_json::from_value(json!({"title": "0199a1b2"})).unwrap();
    assert_eq!(minimal.source, TitleSource::ShortKey);
    round_trip(&HostSessions {
        target_id: TargetId::remote("devbox"),
        report: SessionReport {
            codex_home: PathBuf::from("/home/u/.codex"),
            scanned_at: ts(1),
            sqlite_paths: Vec::new(),
            by_provider: BTreeMap::new(),
            inconsistent: 0,
            warnings: Vec::new(),
        },
        history: Vec::new(),
        read_only: true,
        migrate_command: Some("spb sessions migrate --from krill".into()),
    });
}

// ---------------------------------------------------------------------------
// app.rs, caps.rs, site.rs, catalog.rs, views.rs, plan.rs
// ---------------------------------------------------------------------------

#[test]
fn app_session_server_exe_and_model_info_metadata_default_when_absent() {
    let session: AppSession =
        serde_json::from_value(json!({"main_pid": 1, "launched_by_us": true})).unwrap();
    assert_eq!(session.app_server_exe, None);
    let info: ModelInfo = serde_json::from_value(json!({"id": "gpt-5.5"})).unwrap();
    assert_eq!(
        info,
        ModelInfo {
            id: "gpt-5.5".into(),
            ..ModelInfo::default()
        }
    );
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        json!({"id": "gpt-5.5", "display_name": null})
    );
    let full = ModelInfo {
        id: "gpt-6-astra".into(),
        display_name: Some("GPT-6 Astra".into()),
        context_window: Some(272_000),
        max_context_window: Some(872_000),
        service_tiers: astra().service_tiers,
        raw: astra().raw,
    };
    round_trip(&full);
}

#[test]
fn presets_list_krill_first_as_the_default_and_never_claim_tier_suffixes() {
    let presets = catalog::presets();
    let ids: Vec<_> = presets.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            catalog::PRESET_KRILL,
            catalog::PRESET_OFFICIAL,
            catalog::PRESET_CUSTOM
        ]
    );
    assert_eq!(presets.iter().filter(|p| p.default).count(), 1);
    assert!(presets[0].default);
    assert_eq!(presets[0].site, catalog::krill_site());
    assert_eq!(presets[0].key_placeholder.as_deref(), Some("nb_\u{2026}"));
    assert_eq!(presets[1].site, catalog::official_site());
    let custom = &presets[2];
    assert_eq!(custom.site.kind, SiteKind::Compatible);
    assert!(custom.site.lines.is_empty());
    assert_eq!(custom.caps(), &catalog::compatible_capabilities());
    assert_eq!(custom.key_placeholder, None);
    for p in &presets {
        assert!(!p.caps().tier_model_suffix, "{}", p.id);
        round_trip(p);
    }
    // A capability flag that is off may be left out (hand-written test sites,
    // `SUPERBA_TEST_SITES`).
    let mut site = serde_json::to_value(catalog::krill_site()).unwrap();
    site["capabilities"]
        .as_object_mut()
        .unwrap()
        .remove("tier_model_suffix");
    let back: Site = serde_json::from_value(site).unwrap();
    assert!(!back.capabilities.tier_model_suffix);
}

#[test]
fn setting_rows_carry_common_and_host_support() {
    let row = SettingRow {
        path: "approval_policy".into(),
        key: None,
        feature: None,
        zh: None,
        current: None,
        managed: None,
        support: Vec::new(),
        advanced: false,
        common: true,
        host_support: vec![ConsumerSupport {
            consumer: ConsumerKind::Remote {
                target_id: TargetId::remote("devbox"),
            },
            label: "devbox".into(),
            version: Some(v("0.150.1")),
            support: KeySupport::Missing,
            catalog: None,
        }],
    };
    round_trip(&row);
    let minimal: SettingRow = serde_json::from_value(json!({"path": "x"})).unwrap();
    assert!(!minimal.common);
    assert!(minimal.host_support.is_empty());
}

#[test]
fn new_switch_intents_round_trip() {
    for intent in [
        SwitchIntent::AppCodexPin {
            pin: AppCodexPin::Version {
                version: v("0.162.0"),
            },
        },
        SwitchIntent::AppCodexPin {
            pin: AppCodexPin::Bundled,
        },
        SwitchIntent::HostSync {
            target_id: TargetId::remote("devbox"),
            selection: HostSyncSelection {
                paths: Some(vec!["approval_policy".into()]),
            },
        },
    ] {
        round_trip(&intent);
    }
    let parsed: SwitchIntent =
        serde_json::from_value(json!({"kind": "host_sync", "target_id": "ssh:devbox"})).unwrap();
    assert_eq!(
        parsed,
        SwitchIntent::HostSync {
            target_id: TargetId::remote("devbox"),
            selection: HostSyncSelection::default(),
        }
    );
}

// ---------------------------------------------------------------------------
// profile.rs, router.rs
// ---------------------------------------------------------------------------

#[test]
fn profiles_and_settings_leave_unused_optional_fields_out() {
    // A profile and settings that use no model override, speed tier, host,
    // network or app-Codex choice: those fields default and are not written.
    let profile = json!({
        "id": "work", "name": "Work", "builtin": null, "site_id": "krill",
        "line_id": "overseas", "auth_mode": "pat", "auth": "auto", "key_ref": null,
        "base_url_override": null,
        "model": {"model": "gpt-6-luna", "reasoning_effort": null}
    });
    let parsed: Profile = serde_json::from_value(profile.clone()).unwrap();
    assert!(parsed.model_overrides.is_empty());
    assert_eq!(parsed.model_picker, ModelPicker::CodexDefault);
    assert_eq!(parsed.model.speed, None);
    assert_eq!(serde_json::to_value(&parsed).unwrap(), profile);

    let settings = serde_json::to_value(Settings::default()).unwrap();
    for absent in ["hosts", "network", "app_codex"] {
        assert!(settings.get(absent).is_none(), "{absent} must be skipped");
    }
    assert!(settings["router"].get("stats").is_none());
    let back: Settings = serde_json::from_value(settings).unwrap();
    assert_eq!(back, Settings::default());
}

#[test]
fn v3_settings_defaults_match_the_decisions() {
    let s = Settings::default();
    assert!(s.hosts.is_empty());
    assert!(s.network.online_checks);
    assert_eq!(s.network.download_source, DownloadSource::Auto);
    assert_eq!(s.network.feed_source, FeedSourceChoice::Auto);
    assert!(!s.network.third_party_proxies);
    assert!(s.network.extra_mirrors.is_empty());
    assert_eq!(s.app_codex.pin, AppCodexPin::Bundled);
    assert!(!s.app_codex.persist_env);
    let stats = &s.router.stats;
    assert!(stats.record);
    assert_eq!(stats.usage_days, 90);
    assert_eq!(stats.max_rows, 200_000);
    assert!(!stats.line.enabled);
    assert_eq!(stats.line.clients, vec![StatsLineClient::Desktop]);
    assert_eq!(
        stats.line.metrics,
        vec![
            StatsMetric::Tps,
            StatsMetric::FirstToken,
            StatsMetric::TurnTokens
        ]
    );
    assert_eq!(stats.line.template, None);
    assert_eq!(stats.line.style, StatsLineStyle::Quote);
    assert!(stats.prices.ratio_by_profile.is_empty());
    assert_eq!(StatsLineClient::Desktop.originator(), "Codex Desktop");
    assert_eq!(StatsLineClient::Tui.originator(), "codex_cli_rs");
    empty_is_default::<RouterStatsSettings>();
    empty_is_default::<StatsLineSettings>();
    empty_is_default::<NetworkSettings>();
    empty_is_default::<AppCodexSettings>();
    empty_is_default::<HostSettings>();
    empty_is_default::<HostSync>();
    empty_is_default::<PriceSettings>();
}

#[test]
fn v3_settings_round_trip_with_every_field_set() {
    let mut s = Settings::default();
    let mut value_hashes = BTreeMap::new();
    value_hashes.insert("approval_policy".to_owned(), "ef".repeat(32));
    s.hosts.insert(
        TargetId::remote("devbox"),
        HostSettings {
            follow_switches: true,
            last_sync: Some(HostSync {
                at: ts(1),
                values_hash: "ab".repeat(32),
                paths: vec!["approval_policy".into()],
                value_hashes,
            }),
            follow_chosen: false,
        },
    );
    s.network = NetworkSettings {
        online_checks: false,
        download_source: DownloadSource::NpmMirror,
        feed_source: FeedSourceChoice::JsDelivr,
        third_party_proxies: true,
        extra_mirrors: vec!["https://mirror.example/feeds/".into()],
        windows_app_source: WindowsAppSource::StoreMirrorOnly,
    };
    s.app_codex = AppCodexSettings {
        pin: AppCodexPin::Version {
            version: v("0.162.0"),
        },
        persist_env: true,
    };
    let mut ratio = BTreeMap::new();
    ratio.insert(ProfileId::new("krill-work"), Multiplier::from_ppm(150_000));
    let mut models = BTreeMap::new();
    models.insert(
        "gpt-6-astra".to_owned(),
        PriceEntry {
            input: usd(1.0),
            output: usd(8.0),
            ..PriceEntry::default()
        },
    );
    let mut overrides = BTreeMap::new();
    overrides.insert(ProfileId::new("krill-work"), models);
    s.router.stats = RouterStatsSettings {
        record: false,
        usage_days: 30,
        max_rows: 1000,
        line: StatsLineSettings {
            enabled: true,
            clients: vec![StatsLineClient::Desktop, StatsLineClient::Tui],
            harnesses: vec![superba_core::RequestHarness::Codex],
            metrics: vec![
                StatsMetric::Cost,
                StatsMetric::Requests,
                StatsMetric::Cached,
            ],
            template: Some("\u{26a1}\u{2060} {tps} tok/s".into()),
            style: StatsLineStyle::QuoteItalic,
        },
        prices: PriceSettings {
            ratio_by_profile: ratio,
            overrides,
        },
    };
    let encoded = round_trip(&s);
    assert_eq!(encoded["network"]["download_source"], "npm_mirror");
    assert_eq!(encoded["network"]["feed_source"], "jsdelivr");
    assert_eq!(
        encoded["router"]["stats"]["prices"]["ratio_by_profile"]["krill-work"],
        json!(0.15)
    );
    assert_eq!(encoded["hosts"]["ssh:devbox"]["follow_switches"], true);
    for (src, text) in [
        (DownloadSource::GitHub, "github"),
        (DownloadSource::Npm, "npm"),
        (DownloadSource::Auto, "auto"),
    ] {
        assert_eq!(serde_json::to_value(src).unwrap(), text);
    }
    assert_eq!(
        serde_json::to_value(FeedSourceChoice::GitHubRaw).unwrap(),
        "github_raw"
    );

    let view = RouterView {
        stats: Some(RouterStatsStatus {
            recording: true,
            rows_written: 1,
            dropped: 0,
            db_bytes: None,
        }),
        local: Some(RouterLocalView {
            open_files_limit: Some(256),
            max_concurrent: 64,
            in_flight: 3,
            busy: 2,
            file_limit_errors: 1,
            last_problem_ms: Some(1_760_000_000_000),
            last_file_limit: Some(
                "connecting to krill/cdn: Too many open files (os error 24)".into(),
            ),
        }),
        ..RouterView::default()
    };
    assert!(view.local.as_ref().unwrap().ran_short());
    round_trip(&view);
    // A view from before the local budget has none.
    let old: RouterView = serde_json::from_value(serde_json::json!({
        "settings": {}, "configured": true, "installed": true, "running": true,
        "codex_routed": false
    }))
    .unwrap();
    assert_eq!(old.local, None);
}

#[test]
fn profile_model_fields_round_trip() {
    let mut profile = catalog::builtin_profiles().remove(1);
    profile.builtin = None;
    profile.model = ModelDefaults {
        model: Some("gpt-6-astra".into()),
        reasoning_effort: None,
        speed: Some(SpeedTier::Fast),
        context_window: Some(400_000),
        auto_compact: Some(300_000),
    };
    profile.model_overrides.insert(
        "gpt-6-astra".into(),
        ModelOverride {
            context_window: Some(500_000),
            ..ModelOverride::default()
        },
    );
    profile.model_picker = ModelPicker::Provider;
    let encoded = round_trip(&profile);
    assert_eq!(encoded["model"]["speed"], "fast");
    assert_eq!(encoded["model_picker"], "provider");
    assert_eq!(
        encoded["model_overrides"]["gpt-6-astra"],
        json!({"context_window": 500000})
    );
}

// ---------------------------------------------------------------------------
// paths.rs
// ---------------------------------------------------------------------------

#[test]
fn v3_storage_paths() {
    let home = CodexHome::new("/home/u/.codex");
    assert_eq!(home.superba_dir(), PathBuf::from("/home/u/.codex/superba"));
    assert_eq!(
        home.model_catalog_file("0a1b2c3d"),
        PathBuf::from("/home/u/.codex/superba/models-0a1b2c3d.json")
    );
    let data = DataDir::new("/home/user/.local/share/superba");
    assert_eq!(
        data.feeds_dir(),
        PathBuf::from("/home/user/.local/share/superba/feeds")
    );
    assert_eq!(
        data.codex_store_dir(),
        PathBuf::from("/home/user/.local/share/superba/codex")
    );
    assert_eq!(
        data.downloads_dir(),
        PathBuf::from("/home/user/.local/share/superba/downloads")
    );
}
