//! Claude Code (user bug 79): a view of a Krill provider with its suggested
//! endpoint, a merged model list (one Codex-only id) and the configuration
//! the pages save.

use std::path::PathBuf;

use superba_core::claude::{anthropic_preset, suggested_capabilities};
use superba_core::{
    ClaudeApplyReport, ClaudeChange, ClaudeConfig, ClaudeFileStatus, ClaudeInstall, ClaudeModelRow,
    ClaudeModelsRequest, ClaudeModelsView, ClaudePlan, ClaudePlanKind, ClaudeView, ModelListKind,
    ModelSourceStatus, ProfileId, Result, Timestamp,
};

use super::FakeService;

/// Saved configuration of the fake.
#[derive(Debug, Default)]
pub struct State {
    pub config: ClaudeConfig,
    /// The local router's Claude Code ingress (`None`: not set up).
    pub router_base_url: Option<url::Url>,
    /// Claude Code on the router as the Router page shows it.
    pub router: Option<superba_core::RouterClaudeView>,
}

fn endpoint() -> superba_core::AnthropicEndpoint {
    let origin = url::Url::parse("https://api.krill-code.net").unwrap();
    anthropic_preset(superba_core::claude::PRESET_KRILL)
        .unwrap()
        .endpoint(Some(&origin))
        .unwrap()
}

pub(super) fn view(fake: &FakeService) -> ClaudeView {
    let config = fake.areas.claude.lock().unwrap().config.clone();
    let router_base_url = fake.areas.claude.lock().unwrap().router_base_url.clone();
    ClaudeView {
        config_dir: PathBuf::from("/home/u/.claude"),
        config_dir_from_env: false,
        settings_path: PathBuf::from("/home/u/.claude/settings.json"),
        file: ClaudeFileStatus::Valid,
        install: Some(ClaudeInstall {
            binary: PathBuf::from("/usr/local/bin/claude"),
            version: Some("2.1.295".into()),
        }),
        provider: Some(ProfileId::new("krill")),
        follows_codex: config.provider.is_none(),
        config,
        endpoint: Some(endpoint()),
        endpoint_suggested: true,
        key_available: true,
        applied: None,
        pending: Vec::new(),
        router_base_url,
        notes: Vec::new(),
    }
}

fn row(id: &str, lists: &[ModelListKind], usable: bool) -> ClaudeModelRow {
    ClaudeModelRow {
        id: id.into(),
        display_name: None,
        lists: lists.to_vec(),
        manual: false,
        usable,
        context_window: None,
        reasoning_levels: Vec::new(),
        suggested: suggested_capabilities(id, None),
    }
}

pub(super) fn models(_fake: &FakeService, request: ClaudeModelsRequest) -> ClaudeModelsView {
    use ModelListKind::{Codex, Standard};
    ClaudeModelsView {
        provider: request.provider.unwrap_or_else(|| ProfileId::new("krill")),
        rows: vec![
            row("gpt-6.1-sol", &[Codex, Standard], true),
            row("gpt-5.6-luna", &[Codex, Standard], true),
            row("deepseek-v4", &[Standard], true),
            row("gpt-6-astra", &[Codex], false),
        ],
        sources: vec![
            ModelSourceStatus {
                kind: Codex,
                path: Some("/backend-api/codex/models?client_version=0.162.0".into()),
                fetched_at: Some(Timestamp::from_millis(1_790_000_000_000)),
                count: 3,
                error: None,
            },
            ModelSourceStatus {
                kind: Standard,
                path: Some("/v1/models".into()),
                fetched_at: Some(Timestamp::from_millis(1_790_000_000_000)),
                count: 3,
                error: None,
            },
        ],
        at: Timestamp::from_millis(1_790_000_000_000),
    }
}

pub(super) fn save(fake: &FakeService, config: ClaudeConfig) -> Result<ClaudeView> {
    fake.areas.claude.lock().unwrap().config = config;
    Ok(view(fake))
}

pub(super) fn plan(kind: ClaudePlanKind) -> ClaudePlan {
    ClaudePlan {
        kind,
        settings_path: PathBuf::from("/home/u/.claude/settings.json"),
        provider: Some(ProfileId::new("krill")),
        base_sha256: None,
        changes: vec![ClaudeChange {
            path: "env.ANTHROPIC_BASE_URL".into(),
            before: None,
            after: Some("https://api.krill-code.net".into()),
            drift: false,
            secret: false,
        }],
        notes: Vec::new(),
        desired: None,
    }
}

pub(super) fn apply(plan: ClaudePlan) -> ClaudeApplyReport {
    ClaudeApplyReport {
        kind: plan.kind,
        settings_path: plan.settings_path,
        sha256: Some("0".repeat(64)),
        backup: None,
        changed: plan.changes.into_iter().map(|c| c.path).collect(),
        kept: Vec::new(),
        notes: Vec::new(),
    }
}
