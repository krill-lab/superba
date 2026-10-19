//! Remote hosts keep the user's own model and setting values when Superba
//! releases them (review platform-security #3):
//!
//! - end to end with the real config engine and the exact remote scripts run by
//!   the local `/bin/sh` under a throwaway `HOME` (`LocalShell`, no ssh): a
//!   released `model` returns to the host's latest own value, a restore of a
//!   following host changes only its connection, a sync's released setting gets
//!   the host's pre-sync value back;
//! - over the in-memory fakes: `spb restore` limits following hosts to their
//!   connection, a sync's "remove" rows release keys instead of deleting them.
#![cfg(unix)]
#![allow(missing_docs)]

mod fakes;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_config::{ENV_SUPERBA_SECRET_BACKEND, TomlConfigEngine};
use superba_core::{
    AuthMode, CodexPath, DataDir, ExecuteOptions, HostSync, HostSyncAction, HostSyncSelection,
    KeyResolution, LocalContext, MapEnv, ModelDefaults, NoEvents, Origin, ProfileId,
    ProviderRestore, RemoteApplyScope, RemoteHost, RemoteHosts, ResolvedEndpoints, ResolvedMode,
    ResolvedProfile, SafeDefaults, Secret, SettingEntry, SettingValue, SiteKind, SwitchIntent,
    SwitchRequest, SwitchService, TargetId, catalog,
};
use superba_remote::store::HostStore;
use superba_remote::{LocalShell, SshRemoteHosts};
use superba_switch::hosts::value_hash;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// real engine + remote scripts
// ---------------------------------------------------------------------------

const PROXY: &str = "https://proxy.example.com/v1";

/// The host's own config: a model, its own proxy, a setting and an MCP server.
fn host_config() -> String {
    format!(
        "model = \"gpt-5.5\"\nopenai_base_url = \"{PROXY}\"\napproval_policy = \"on-request\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\n"
    )
}

struct Host {
    _tmp: TempDir,
    config: PathBuf,
    data: DataDir,
    hosts: SshRemoteHosts,
    host: RemoteHost,
}

impl Host {
    fn new() -> Self {
        Self::with_config(&host_config(), false)
    }

    fn with_config(original: &str, wsl: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("host-home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".profile"), "").unwrap();
        let config = home.join(".codex/config.toml");
        std::fs::write(&config, original).unwrap();
        let data = DataDir::new(tmp.path().join("data"));
        let ctx = LocalContext::new(
            tmp.path().join("local-home"),
            data.clone(),
            Arc::new(MapEnv::new().with(ENV_SUPERBA_SECRET_BACKEND, "file")),
        );
        let engine = Arc::new(TomlConfigEngine::new(ctx));
        let shell = Arc::new(LocalShell::new(&home).with_login_shell("/bin/sh"));
        let hosts = SshRemoteHosts::new(engine, data.clone())
            .with_shell(shell.clone())
            .with_wsl_shell(shell);
        let mut host = remote_host("devbox", Some("remote-ssh-discovered:devbox"));
        if wsl {
            host.kind = superba_core::HostKind::Wsl;
            host.wsl = Some(superba_core::WslDistro::named("test-distro"));
        }
        Self {
            _tmp: tmp,
            config,
            data,
            hosts,
            host,
        }
    }

    fn config(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap()
    }

    fn edit(&self, from: &str, to: &str) {
        let cfg = self.config();
        assert!(cfg.contains(from), "{cfg}");
        std::fs::write(&self.config, cfg.replace(from, to)).unwrap();
    }

    async fn apply(&self, desired: &ResolvedProfile, scope: RemoteApplyScope) {
        let plan = self
            .hosts
            .plan_apply_scoped(&self.host, desired, scope)
            .await
            .unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        self.hosts.apply(&self.host, &plan).await.unwrap();
    }

    async fn owned(&self) -> Vec<String> {
        HostStore::new(&self.data, &self.host.target_id)
            .load_state()
            .await
            .unwrap()
            .last_written
            .into_iter()
            .map(|k| k.path)
            .collect()
    }
}

async fn original_restore_round_trips(wsl: bool) {
    for original_url in ["", "openai_base_url = \"https://original.example/v1\"\n"] {
        let original = format!(
            "{original_url}model_provider = \"legacy\"\nmodel = \"gpt-5.5\"\n\n[model_providers.legacy]\nbase_url = \"https://legacy.example/v1\"\nenv_key = \"ORIGINAL_KEY\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nenv = {{ DOCS_TOKEN = \"fixture\" }}\n"
        );
        let h = Host::with_config(&original, wsl);
        let home = h.config.parent().unwrap().parent().unwrap();
        let rc = "# user's environment\nexport CODEX_AUTHAPI_BASE_URL=https://api-slb.krill-code.net\nexport ORIGINAL_KEY=fixture\n";
        std::fs::write(home.join(".profile"), rc).unwrap();
        let origin = Origin::parse(OVERSEAS).unwrap();
        let mut pat = resolved(ResolvedMode::Token {
            site_kind: SiteKind::Krill,
            auth: AuthMode::Pat,
            endpoints: ResolvedEndpoints::for_line(
                &origin,
                CodexPath::BackendApiCodex,
                AuthMode::Pat,
            )
            .unwrap(),
            key: Secret::new(KEY),
        });
        pat.settings = superba_core::CodexSettings::krill_recommended();
        for profile in [&pat, &apikey(None), &pat] {
            h.apply(profile, RemoteApplyScope::Both).await;
            assert!(h.config().contains("openai_base_url"));
        }
        let restore = resolved(ResolvedMode::Original {
            provider: ProviderRestore::Baseline {
                reverse_migrate: true,
            },
        });
        h.apply(&restore, RemoteApplyScope::Both).await;
        assert_eq!(h.config(), original);
        assert!(!home.join(".codex/auth.json").exists());
        assert_eq!(std::fs::read_to_string(home.join(".profile")).unwrap(), rc);
        assert!(h.owned().await.is_empty());
        h.apply(&restore, RemoteApplyScope::Both).await;
        assert_eq!(h.config(), original);
    }
}

#[tokio::test]
async fn ssh_restore_removes_added_keys_and_preserves_original_values_after_switches() {
    original_restore_round_trips(false).await;
}

#[tokio::test]
async fn wsl_restore_removes_added_keys_and_preserves_original_values_after_switches() {
    original_restore_round_trips(true).await;
}

fn line(path: &str, cfg: &str) -> Option<String> {
    cfg.lines()
        .find(|l| l.split('=').next().map(str::trim) == Some(path))
        .map(str::to_owned)
}

fn resolved(mode: ResolvedMode) -> ResolvedProfile {
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

/// An API-key profile on Krill's overseas line, optionally with a default model.
fn apikey(model: Option<&str>) -> ResolvedProfile {
    let origin = Origin::parse(OVERSEAS).unwrap();
    let mut p = resolved(ResolvedMode::Token {
        site_kind: SiteKind::Krill,
        auth: AuthMode::ApiKey,
        endpoints: ResolvedEndpoints::for_line(&origin, CodexPath::CodexV1, AuthMode::ApiKey)
            .unwrap(),
        key: Secret::new(KEY),
    });
    p.model.model = model.map(str::to_owned);
    p
}

fn official() -> ResolvedProfile {
    resolved(ResolvedMode::Official)
}

fn original() -> ResolvedProfile {
    resolved(ResolvedMode::Original {
        provider: ProviderRestore::KeepOpenai,
    })
}

/// A settings sync writing `settings` and releasing `release`.
fn sync(settings: &[(&str, &str)], release: &[&str]) -> ResolvedProfile {
    let mut p = apikey(None);
    for (path, value) in settings {
        p.settings.set(path, *value).unwrap();
    }
    for path in release {
        p.key_resolutions
            .insert((*path).to_owned(), KeyResolution::Overwrite);
    }
    p
}

#[tokio::test]
async fn a_released_model_returns_to_the_hosts_latest_own_value() {
    let h = Host::new();
    let astra = apikey(Some("gpt-6-astra"));

    // A followed switch to a profile with a default model overwrites the host's.
    h.apply(&astra, RemoteApplyScope::Connection).await;
    assert_eq!(
        line("model", &h.config()).as_deref(),
        Some("model = \"gpt-6-astra\"")
    );

    // The official login has no default model: the host's own comes back
    // (it used to be deleted).
    h.apply(&official(), RemoteApplyScope::Connection).await;
    let cfg = h.config();
    assert_eq!(line("model", &cfg).as_deref(), Some("model = \"gpt-5.5\""));
    assert_eq!(line("model_provider", &cfg), None);
    assert_eq!(line("openai_base_url", &cfg), None);
    assert_eq!(h.owned().await, Vec::<String>::new());

    // The app picks another model on the host while Superba owns nothing; the
    // next takeover and release bring back that latest choice (no drift
    // conflict over the released key either).
    h.edit("gpt-5.5", "gpt-6-luna");
    h.apply(&astra, RemoteApplyScope::Connection).await;
    h.apply(&apikey(None), RemoteApplyScope::Connection).await;
    let cfg = h.config();
    assert_eq!(
        line("model", &cfg).as_deref(),
        Some("model = \"gpt-6-luna\"")
    );
    assert!(!h.owned().await.contains(&"model".to_owned()));
    // Untouched all along.
    assert!(cfg.contains("approval_policy = \"on-request\""), "{cfg}");
    assert!(cfg.contains("[mcp_servers.docs]"), "{cfg}");
}

#[tokio::test]
async fn a_followed_restore_keeps_synced_settings_and_restores_the_connection() {
    let h = Host::new();
    h.apply(
        &sync(&[("approval_policy", "never")], &[]),
        RemoteApplyScope::Settings,
    )
    .await;
    h.apply(&apikey(Some("gpt-6-astra")), RemoteApplyScope::Connection)
        .await;
    h.apply(&official(), RemoteApplyScope::Connection).await;
    assert_eq!(line("openai_base_url", &h.config()), None);

    // `spb restore` on a following host: its connection goes back to the
    // original (also the keys the official switch released), the synced
    // setting stays.
    h.apply(&original(), RemoteApplyScope::Connection).await;
    let cfg = h.config();
    assert_eq!(
        line("openai_base_url", &cfg),
        Some(format!("openai_base_url = \"{PROXY}\""))
    );
    assert_eq!(line("model", &cfg).as_deref(), Some("model = \"gpt-5.5\""));
    assert!(cfg.contains("approval_policy = \"never\""), "{cfg}");
    assert_eq!(h.owned().await, vec!["approval_policy".to_owned()]);

    // A restore of a host the user selected restores both key families: the
    // file is the host's original again.
    h.apply(&original(), RemoteApplyScope::Both).await;
    assert_eq!(h.config(), host_config());
    assert_eq!(h.owned().await, Vec::<String>::new());
}

#[tokio::test]
async fn a_restore_while_a_profile_is_active_restores_the_users_model() {
    let h = Host::new();
    h.apply(&apikey(Some("gpt-6-astra")), RemoteApplyScope::Connection)
        .await;
    h.apply(&original(), RemoteApplyScope::Connection).await;
    let cfg = h.config();
    assert_eq!(line("model", &cfg).as_deref(), Some("model = \"gpt-5.5\""));
    assert_eq!(
        line("openai_base_url", &cfg),
        Some(format!("openai_base_url = \"{PROXY}\""))
    );
    assert_eq!(cfg, host_config());
}

#[tokio::test]
async fn a_released_setting_gets_the_hosts_pre_sync_value_back() {
    let h = Host::new();
    h.apply(
        &sync(
            &[("approval_policy", "never"), ("model_verbosity", "high")],
            &[],
        ),
        RemoteApplyScope::Settings,
    )
    .await;
    let cfg = h.config();
    assert!(cfg.contains("approval_policy = \"never\""), "{cfg}");
    assert!(cfg.contains("model_verbosity = \"high\""), "{cfg}");

    // The sync's "remove" rows: the host's own approval_policy comes back (it
    // used to be deleted); model_verbosity, which the host did not have, goes.
    h.apply(
        &sync(&[], &["approval_policy", "model_verbosity"]),
        RemoteApplyScope::Settings,
    )
    .await;
    assert_eq!(h.config(), host_config());
    assert_eq!(h.owned().await, Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// planner and host sync over fakes
// ---------------------------------------------------------------------------

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn world_with_host() -> World {
    let w = World::new();
    w.profiles
        .set
        .lock()
        .unwrap()
        .profiles
        .extend(catalog::builtin_profiles());
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    w
}

fn scopes(w: &World) -> Vec<(String, RemoteApplyScope)> {
    w.remote
        .scoped
        .lock()
        .unwrap()
        .iter()
        .map(|(h, s, _)| (h.clone(), *s))
        .collect()
}

#[tokio::test]
async fn restoring_the_original_changes_only_the_connection_of_a_following_host() {
    let w = world_with_host();
    let sw = w.switcher();
    sw.set_host_follow(&TargetId::remote("dev"), true)
        .await
        .unwrap();
    // The GUI's restore entry points use the "original" profile.
    let plan = sw.plan(req(catalog::PROFILE_ORIGINAL)).await.unwrap();
    assert_eq!(plan.remotes.len(), 1);
    assert_eq!(
        scopes(&w),
        vec![("dev".to_owned(), RemoteApplyScope::Connection)]
    );
    // Even if the local original used the default provider, a following host
    // may have used a custom one. It must receive an exact restore policy too.
    assert!(matches!(
        w.remote.scoped.lock().unwrap()[0].2.mode,
        ResolvedMode::Original {
            provider: ProviderRestore::Baseline {
                reverse_migrate: true
            }
        }
    ));

    // A host the user selects explicitly gets both key families restored.
    w.remote.scoped.lock().unwrap().clear();
    let mut r = req(catalog::PROFILE_ORIGINAL);
    r.targets.remotes = vec![TargetId::remote("dev")];
    sw.plan(r).await.unwrap();
    assert_eq!(scopes(&w), vec![("dev".to_owned(), RemoteApplyScope::Both)]);
}

#[tokio::test]
async fn a_sync_remove_row_releases_the_key_instead_of_deleting_it() {
    let w = world_with_host();
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        p.codex_settings.set("model_verbosity", "high").unwrap();
        // The last sync wrote approval_policy and hide_agent_reasoning there.
        set.settings
            .hosts
            .entry(TargetId::remote("dev"))
            .or_default()
            .last_sync = Some(HostSync {
            paths: vec!["approval_policy".into(), "hide_agent_reasoning".into()],
            value_hashes: [
                ("approval_policy".to_owned(), value_hash(Some("\"never\""))),
                ("hide_agent_reasoning".to_owned(), value_hash(Some("true"))),
            ]
            .into(),
            ..HostSync::default()
        });
    }
    *w.remote.config_toml.lock().unwrap() =
        Some("approval_policy = \"never\"\nhide_agent_reasoning = true\n".into());
    // Superba still owns approval_policy there; hide_agent_reasoning was
    // released since (a restore) and holds the host's own value.
    *w.remote.owned_paths.lock().unwrap() = vec!["approval_policy".into()];
    let sw = w.switcher();
    let preview = sw.host_sync_plan(&TargetId::remote("dev")).await.unwrap();
    let action = |p: &str| {
        preview
            .rows
            .iter()
            .find(|r| r.path == p)
            .map(|r| r.action.clone())
    };
    assert_eq!(action("approval_policy"), Some(HostSyncAction::Remove));
    assert_eq!(action("hide_agent_reasoning"), None);
    assert_eq!(action("model_verbosity"), Some(HostSyncAction::Write));

    let mut r = req("work");
    r.intent = SwitchIntent::HostSync {
        target_id: TargetId::remote("dev"),
        selection: HostSyncSelection::default(),
    };
    let plan = sw.plan(r).await.unwrap();
    let (_, scope, desired) = w.remote.scoped.lock().unwrap().last().cloned().unwrap();
    assert_eq!(scope, RemoteApplyScope::Settings);
    // Written: the profile's value. Released: resolved, not listed (the settings
    // scope restores the host's own value instead of deleting the key).
    assert_eq!(
        desired.settings.get("model_verbosity"),
        Some(&SettingEntry::Set(SettingValue::String("high".into())))
    );
    assert_eq!(desired.settings.get("approval_policy"), None);
    assert_eq!(
        desired.key_resolutions.get("approval_policy"),
        Some(&KeyResolution::Overwrite)
    );

    sw.execute(
        plan,
        ExecuteOptions {
            allow_full_restart: true,
            turn_quiet_ms: 0,
            ..ExecuteOptions::default()
        },
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    let last = w.profiles.set.lock().unwrap().settings.hosts[&TargetId::remote("dev")]
        .last_sync
        .clone()
        .unwrap();
    assert!(
        !last.paths.contains(&"approval_policy".to_owned()),
        "{last:?}"
    );
    assert!(!last.value_hashes.contains_key("approval_policy"));
    assert!(last.paths.contains(&"model_verbosity".to_owned()));
}
