//! v0.3 state of the fake service: models, hosts, versions, request log, feeds.

use std::collections::BTreeMap;

use superba_core::*;

/// What the v0.3 commands read and what they asked the service to do.
#[derive(Clone, Debug)]
pub struct FakeV3 {
    pub models: ModelsView,
    pub model_edits: Vec<ModelsEdit>,
    pub hosts: Vec<HostView>,
    pub follows: Vec<(TargetId, bool)>,
    pub sync: HostSyncPlan,
    pub host_sessions: HostSessions,
    pub versions: VersionsView,
    pub installs: Vec<InstallRequest>,
    pub rollbacks: Vec<InstallTarget>,
    pub page: RequestPage,
    pub queries: Vec<RequestQuery>,
    pub stats: RequestStats,
    pub stats_queries: Vec<StatsQuery>,
    pub stats_settings: Vec<RouterStatsSettings>,
    pub clears: Vec<ClearRequest>,
    pub titles: BTreeMap<String, SessionTitle>,
    pub feeds: FeedsStatus,
    pub feed_refreshes: u32,
    /// Windows: the report `download_app_package` returns (else the Linux one).
    pub windows_download: Option<AppDownloadReport>,
    /// `download_app_package` requests.
    pub app_downloads: Vec<AppDownloadRequest>,
    /// Packages `open_app_package` opened.
    pub opened_packages: Vec<std::path::PathBuf>,
}

fn spec(slug: &str, tiers: &[&str]) -> ModelSpec {
    ModelSpec {
        display_name: Some(slug.to_uppercase()),
        context_window: Some(272_000),
        max_context_window: Some(872_000),
        service_tiers: tiers
            .iter()
            .map(|t| ServiceTierSpec {
                id: (*t).into(),
                name: (*t).into(),
                description: String::new(),
            })
            .collect(),
        ..ModelSpec::new(slug)
    }
}

pub fn host_view() -> HostView {
    HostView {
        host: RemoteHost {
            target_id: TargetId::remote("dev"),
            label: "dev".into(),
            host_id: Some("h-dev".into()),
            kind: superba_core::HostKind::Ssh,
            ssh: SshEndpoint {
                alias: Some("dev".into()),
                ..SshEndpoint::default()
            },
            sources: vec![HostSource::GlobalState],
            connected: Some(true),
            wsl: None,
        },
        state: None,
        probe_error: None,
        settings: HostSettings::default(),
        pending_sync: Some(1),
        hot_restart: true,
        pending: None,
    }
}

pub fn row(id: i64) -> RequestRow {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "started_at": 1_760_000_000_000i64,
        "prompt_cache_key": "019a0000-0000-7000-8000-000000000001",
        "member_id": "work",
        "member_name": "Work",
        "line_id": "overseas",
        "line_name": "Overseas",
        "upstream_host": "api.example.test",
        "failovers": 0,
        "transport": "http",
        "client": "desktop",
        "model": "gpt-6-astra",
        "status": {"kind": "ok"},
        "input_tokens": 12345,
        "cached_tokens": 10000,
        "cache_write_tokens": 0,
        "output_tokens": 321,
        "first_token_ms": 820,
        "duration_ms": 4100,
        "tps_milli": 87_000,
        "cost": 4200,
        "injected": true,
        "stripped": 1
    }))
    .expect("request row fixture")
}

pub fn fake_v3() -> FakeV3 {
    let list = ModelList {
        source: ModelListSource::Upstream {
            path: "/backend-api/codex/models?client_version=0.162.0".into(),
            fetched_at: Timestamp::EPOCH,
            etag: None,
        },
        models: vec![
            spec("gpt-6-astra", &["priority", "ultrafast"]),
            spec("gpt-6-luna", &["priority"]),
        ],
        fallback_reason: None,
    };
    let page = RequestPage {
        rows: vec![row(1)],
        totals: RequestTotals {
            requests: 1,
            input_tokens: 12_345,
            output_tokens: 321,
            cached_tokens: 10_000,
            ..RequestTotals::default()
        },
        recording: true,
        ..RequestPage::default()
    };
    FakeV3 {
        models: ModelsView {
            profile_id: ProfileId::new("work"),
            site_id: SiteId::new("krill"),
            read_only: false,
            list,
            defaults: ModelDefaults {
                model: Some("gpt-6-astra".into()),
                ..ModelDefaults::default()
            },
            overrides: BTreeMap::new(),
            picker: ModelPicker::CodexDefault,
            picker_available: false,
            speed: SpeedSupport {
                offered: vec![SpeedTier::Standard, SpeedTier::Fast, SpeedTier::Ultrafast],
                cli: SpeedEffect::Applies,
                app: SpeedEffect::AppForcesStandard,
                tiers: Vec::new(),
            },
            catalog_file: CatalogFileStatus::NotNeeded,
            notes: Vec::new(),
            per_model: true,
            applied: None,
        },
        model_edits: Vec::new(),
        hosts: vec![host_view()],
        follows: Vec::new(),
        sync: HostSyncPlan {
            target_id: TargetId::remote("dev"),
            profile_id: Some(ProfileId::new("work")),
            host_version: Some(CodexVersion::new(0, 150, 1)),
            rows: vec![
                HostSyncRow {
                    path: "model_verbosity".into(),
                    local: Some("\"high\"".into()),
                    remote: None,
                    action: HostSyncAction::Write,
                    selected: true,
                },
                HostSyncRow {
                    path: "features.new_thing".into(),
                    local: Some("true".into()),
                    remote: None,
                    action: HostSyncAction::Skip {
                        reason: SkipReason::NotSupported {
                            consumers: vec!["dev 0.150.1".into()],
                        },
                    },
                    selected: false,
                },
            ],
            blocked: Vec::new(),
            notes: Vec::new(),
        },
        host_sessions: HostSessions {
            target_id: TargetId::remote("dev"),
            report: SessionReport {
                codex_home: "/home/u/.codex".into(),
                scanned_at: Timestamp::EPOCH,
                sqlite_paths: Vec::new(),
                by_provider: [(
                    "krill".into(),
                    ProviderSessions {
                        distinct: 3,
                        threads: 0,
                        rollouts: 3,
                        archived: 1,
                        compressed: 0,
                    },
                )]
                .into(),
                inconsistent: 0,
                warnings: Vec::new(),
            },
            history: Vec::new(),
            read_only: true,
            migrate_command: Some("spb sessions migrate --from krill".into()),
        },
        versions: VersionsView {
            checked_at: Some(Timestamp::EPOCH),
            app_codex: AppCodexSection {
                supported: true,
                pin: AppCodexPin::Bundled,
                effective_version: Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap()),
                floor: Some(CodexVersion::new(0, 141, 0)),
                candidates: vec![CodexCandidate {
                    release: CodexRelease {
                        version: CodexVersion::new(0, 163, 0),
                        tag: "rust-v0.163.0".into(),
                        date: None,
                        prerelease: false,
                        bundled_by: Vec::new(),
                        sha256: BTreeMap::new(),
                    },
                    tier: CompatTier::Compatible,
                    stored: false,
                }],
                ..AppCodexSection::default()
            },
            cli: vec![CliVersionRow {
                binary: CodexBinary {
                    path: "/usr/local/bin/codex".into(),
                    version: Some(CodexVersion::new(0, 160, 0)),
                    source: InstallSource::Npm,
                    path_rank: Some(0),
                },
                kind: CodexInstallKind::Npm,
                latest: Some(CodexVersion::new(0, 163, 0)),
                action: CliUpdateAction::Command {
                    command: "npm install -g @openai/codex@0.163.0".into(),
                },
            }],
            ..VersionsView::default()
        },
        installs: Vec::new(),
        rollbacks: Vec::new(),
        page,
        queries: Vec::new(),
        stats: RequestStats::default(),
        stats_queries: Vec::new(),
        stats_settings: Vec::new(),
        clears: Vec::new(),
        titles: [(
            "019a0000-0000-7000-8000-000000000001".to_owned(),
            SessionTitle {
                thread_id: Some("019a0000-0000-7000-8000-000000000001".into()),
                title: "Refactor the parser".into(),
                source: TitleSource::Title,
            },
        )]
        .into(),
        feeds: FeedsStatus {
            serial: 7,
            builtin_serial: 7,
            online_checks: true,
            ..FeedsStatus::default()
        },
        feed_refreshes: 0,
        windows_download: None,
        app_downloads: Vec::new(),
        opened_packages: Vec::new(),
    }
}
