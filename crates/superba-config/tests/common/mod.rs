//! Shared fixtures for superba-config integration tests. Everything lives in a
//! temp dir: `<tmp>/home` (fake home with `.codex`), `<tmp>/data` (data dir).
//! Secrets use the file backend so no OS keyring is touched.

#![allow(dead_code)]

pub mod tools;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use superba_config::{ENV_SUPERBA_SECRET_BACKEND, TomlConfigEngine};
use superba_core::{
    AuthMode, CodexHome, CodexPath, DataDir, LineId, LocalContext, ManualClock, MapEnv,
    ModelDefaults, Origin, ProfileId, ResolvedEndpoints, ResolvedMode, ResolvedProfile, Secret,
    SiteId, SiteKind, Timestamp,
};

pub const OVERSEAS: &str = "https://api.kbsfdxc.net";
pub const CDN: &str = "https://api.kbsfdxc.com";
pub const PAT_KEY: &str = "krill-test-pat-000000000001";
pub const API_KEY: &str = "krill-test-api-000000000002";

/// A legacy Krill tutorial config: provider "krill" with its own table, MCP
/// server with a token, comments, CJK, a project table.
pub const LEGACY_CONFIG: &str = r#"# Codex 配置（Krill 教程）
model = "gpt-5.5"
model_provider = "krill"
model_reasoning_effort = "high"

[model_providers.krill]
name = "Krill"
base_url = "https://api.kbsfdxc.net/codex/v1"
env_key = "KRILL_API_KEY"
wire_api = "responses"

[mcp_servers.docs]
command = "npx"
args = ["-y", "docs-mcp"]
env = { DOCS_TOKEN = "tok-123" } # inline secret

[projects."/home/u/项目"]
trust_level = "trusted"
"#;

/// A plain official user's config (no owned keys).
pub const OFFICIAL_CONFIG: &str = r#"model = "gpt-5.5"

# Desktop app settings
[desktop]
theme = "dark"

[mcp_servers.fs]
command = "mcp-fs"
"#;

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub ctx: LocalContext,
    pub clock: Arc<ManualClock>,
    pub engine: TomlConfigEngine,
    pub home: CodexHome,
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = LocalContext::new(
            tmp.path().join("home"),
            DataDir::new(tmp.path().join("data")),
            Arc::new(MapEnv::new().with(ENV_SUPERBA_SECRET_BACKEND, "file")),
        );
        let clock = Arc::new(ManualClock::new(Timestamp::from_millis(1_790_000_000_000)));
        let engine = TomlConfigEngine::with_clock(ctx.clone(), clock.clone());
        let home = ctx.codex_home();
        std::fs::create_dir_all(home.root()).unwrap();
        Self {
            tmp,
            ctx,
            clock,
            engine,
            home,
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.home.config_toml()
    }

    pub fn auth_path(&self) -> PathBuf {
        self.home.auth_json()
    }

    pub fn write_config(&self, s: &str) {
        std::fs::write(self.config_path(), s).unwrap();
    }

    pub fn write_auth(&self, s: &str) {
        std::fs::write(self.auth_path(), s).unwrap();
    }

    pub fn config(&self) -> String {
        std::fs::read_to_string(self.config_path()).unwrap()
    }

    pub fn config_opt(&self) -> Option<String> {
        std::fs::read_to_string(self.config_path()).ok()
    }

    pub fn auth(&self) -> Option<String> {
        std::fs::read_to_string(self.auth_path()).ok()
    }

    pub fn auth_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.auth().unwrap()).unwrap()
    }

    pub fn data(&self) -> &Path {
        self.ctx.data_dir.root()
    }
}

pub fn endpoints(origin: &str, path: CodexPath, mode: AuthMode) -> ResolvedEndpoints {
    ResolvedEndpoints::for_line(&Origin::parse(origin).unwrap(), path, mode).unwrap()
}

pub fn token(origin: &str, mode: AuthMode, key: &str) -> ResolvedProfile {
    let path = if mode == AuthMode::Pat {
        CodexPath::BackendApiCodex
    } else {
        CodexPath::CodexV1
    };
    ResolvedProfile {
        model_catalog: None,
        profile_id: Some(ProfileId::new(match mode {
            AuthMode::Pat => "krill-pat",
            _ => "krill-apikey",
        })),
        site_id: Some(SiteId::new("krill")),
        line_id: Some(LineId::new(if origin == CDN { "cdn" } else { "overseas" })),
        mode: ResolvedMode::Token {
            site_kind: SiteKind::Krill,
            auth: mode,
            endpoints: endpoints(origin, path, mode),
            key: Secret::new(key),
        },
        model: ModelDefaults::default(),
        key_resolutions: BTreeMap::new(),
        // A Krill PAT profile starts with Krill's recommended settings.
        settings: if mode == AuthMode::Pat {
            superba_core::CodexSettings::krill_recommended()
        } else {
            Default::default()
        },
    }
}

pub fn pat(origin: &str) -> ResolvedProfile {
    token(origin, AuthMode::Pat, PAT_KEY)
}

pub fn apikey(origin: &str) -> ResolvedProfile {
    token(origin, AuthMode::ApiKey, API_KEY)
}

pub fn restore(mode: ResolvedMode) -> ResolvedProfile {
    ResolvedProfile {
        model_catalog: None,
        profile_id: None,
        site_id: None,
        line_id: None,
        mode,
        model: ModelDefaults::default(),
        key_resolutions: BTreeMap::new(),
        settings: Default::default(),
    }
}

/// An unsigned JWT with the claims Codex puts in a ChatGPT id_token.
pub fn fake_jwt(email: &str, account: &str, exp_secs: i64) -> String {
    let enc = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    format!(
        "{}.{}.c2ln",
        enc(serde_json::json!({"alg": "RS256", "typ": "JWT"})),
        enc(serde_json::json!({
            "email": email,
            "exp": exp_secs,
            "https://api.openai.com/auth": {"chatgpt_account_id": account, "chatgpt_user_id": "user-1"}
        }))
    )
}

/// A ChatGPT login auth.json as Codex writes it.
pub fn chatgpt_login(email: &str, refresh_token: &str, last_refresh: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": fake_jwt(email, "acct-1", 1_800_000_000),
            "access_token": "access-token-xyz",
            "refresh_token": refresh_token,
            "account_id": "acct-1"
        },
        "last_refresh": last_refresh
    }))
    .unwrap()
}

#[cfg(unix)]
pub fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}
