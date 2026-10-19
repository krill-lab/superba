//! Claude Code's specialized rendering on the shared harness writer.
pub mod json;
use crate::fsx;
use crate::harness::{HarnessState, HarnessWriter};
use std::path::Path;
use std::path::PathBuf;
use superba_core::claude::ClaudeDesired;
use superba_core::harness::{
    EndpointSource, HarnessId, HarnessOptions, HarnessProtocol, HarnessRoute, HarnessTarget,
    ProtocolEndpoint,
};
use superba_core::{
    ClaudeApplied, ClaudeApplyReport, ClaudeConfig, ClaudeFileStatus, ClaudeHome, ClaudePlan,
    ClaudePlanKind, LocalContext, ProfileId, Result, Timestamp,
};

/// Claude Code's user settings as read now.
#[derive(Clone, Debug)]
pub struct SettingsFile {
    /// Path.
    pub path: PathBuf,
    /// Original bytes.
    pub bytes: Option<Vec<u8>>,
    /// Hash used for compare-and-swap.
    pub sha256: Option<String>,
    /// Parsed object.
    pub value: Option<serde_json::Value>,
    /// File status.
    pub status: ClaudeFileStatus,
}
/// Read a Claude settings file.
pub fn read_settings(path: &Path) -> Result<SettingsFile> {
    let bytes = fsx::read_optional(path)?;
    let (value, status) = json::parse(bytes.as_deref());
    Ok(SettingsFile {
        path: path.to_owned(),
        sha256: bytes.as_deref().map(fsx::sha256_hex),
        bytes,
        value,
        status,
    })
}
/// Claude-specific inputs to the shared ownership, backup and write engine.
#[derive(Clone, Debug)]
pub struct ClaudeWriter {
    ctx: LocalContext,
    writer: HarnessWriter,
}
impl ClaudeWriter {
    /// A writer in this context.
    pub fn new(ctx: LocalContext) -> Self {
        Self {
            writer: HarnessWriter::new(ctx.clone(), HarnessId::ClaudeCode),
            ctx,
        }
    }
    /// Claude's configuration home.
    pub fn home(&self) -> ClaudeHome {
        ClaudeHome::resolve(self.ctx.env.as_ref(), &self.ctx.home_dir)
    }
    /// The shared encrypted ownership state.
    pub fn state(&self) -> Result<Option<HarnessState>> {
        self.writer.state()
    }
    /// Current settings.
    pub fn read(&self) -> Result<SettingsFile> {
        read_settings(&self.home().settings_json())
    }
    /// Summary for the Claude page (paths are relative to its single file).
    pub fn applied(&self, _file: &SettingsFile) -> Result<Option<ClaudeApplied>> {
        Ok(self.writer.applied()?.map(|a| ClaudeApplied {
            generation: a.generation,
            at: a.at,
            provider: a.provider,
            owned: a.owned.into_iter().map(|p| relative(&p)).collect(),
            drift: a.drift.into_iter().map(|p| relative(&p)).collect(),
        }))
    }
    /// Plan without writing any configuration or state.
    pub fn plan(
        &self,
        kind: ClaudePlanKind,
        desired: Option<ClaudeDesired>,
        provider: Option<ProfileId>,
    ) -> Result<ClaudePlan> {
        let target = desired.map(|d| HarnessTarget {
            claude: Some(d),
            harness: HarnessId::ClaudeCode,
            provider,
            provider_name: String::new(),
            endpoint: ProtocolEndpoint {
                protocol: HarnessProtocol::Anthropic,
                base_url: url::Url::parse("http://127.0.0.1").expect("literal URL"),
                source: EndpointSource::Provider,
            },
            key: superba_core::Secret::new("claude-rendered"),
            models: Vec::new(),
            default_model: None,
            route: HarnessRoute::Direct,
            options: HarnessOptions::default(),
        });
        self.writer.plan(kind, target)
    }
    /// Execute the same compare-and-swap plan and backup flow as other tools.
    pub fn apply(
        &self,
        plan: &ClaudePlan,
        take_over: bool,
        now: Timestamp,
    ) -> Result<ClaudeApplyReport> {
        self.writer.apply(plan, take_over, now)
    }
}
fn relative(path: &str) -> String {
    path.strip_prefix("settings.json ")
        .unwrap_or(path)
        .to_owned()
}

/// The keys Superba owns that `new` changes compared with `old` (both
/// `settings.json` texts): values for people (keys masked). Empty when either
/// text does not parse.
pub fn owned_changes(
    old: &str,
    new: &str,
    owned: &[superba_core::OwnedKey],
) -> Vec<superba_core::KeyChange> {
    let (Some(a), _) = json::parse(Some(old.as_bytes())) else {
        return Vec::new();
    };
    let (Some(b), _) = json::parse(Some(new.as_bytes())) else {
        return Vec::new();
    };
    owned
        .iter()
        .filter_map(|o| {
            let before = json::value_at(&a, &o.path);
            let after = json::value_at(&b, &o.path);
            (before != after).then(|| superba_core::KeyChange {
                path: o.path.clone(),
                before: before.map(|v| superba_core::claude::display_value(&o.path, &v)),
                after: after.map(|v| superba_core::claude::display_value(&o.path, &v)),
            })
        })
        .collect()
}

/// Superba's Claude Code configuration (`<data>/claude-code.json`).
#[derive(Clone, Debug)]
pub struct ClaudeConfigStore {
    path: PathBuf,
}

impl ClaudeConfigStore {
    /// The store of `ctx`'s data directory.
    pub fn new(ctx: &LocalContext) -> Self {
        Self {
            path: ctx.data_dir.claude_config_json(),
        }
    }

    /// The configuration (default when there is none).
    pub fn load(&self) -> Result<ClaudeConfig> {
        Ok(fsx::read_json::<ClaudeConfig>(&self.path, "claude-code.json")?.unwrap_or_default())
    }

    /// Save it.
    pub fn save(&self, config: &ClaudeConfig) -> Result<()> {
        fsx::write_json(&self.path, config)
    }
}
