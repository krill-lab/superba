//! In-memory fakes of every component trait, sharing one ordered call log, for
//! driving `Switcher::plan` / `execute` through every path without IO.

#![allow(dead_code, missing_docs)]

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::{
    ActiveCredentials, ActiveState, AppController, AppInstall, AppSession, AppVariant, ApplyPlan,
    ApplyReport, AuthApiHealth, AuthMode, AuthShape, Baseline, BaselineExtras, BlockReason,
    BridgeStatus, CdpEndpoint, Clock, CodexBinary, CodexFileKind, CodexFiles, CodexHome, CodexPath,
    CodexProcessKind, CodexVersion, ConfigEngine, ConfigState, Decision, DecisionRequest,
    EnvApplied, EnvEdit, EnvEditAction, EnvLocation, EnvObservation, EnvOp, EnvPersister, EnvPlan,
    EnvReport, Error, FileChange, FileWrite, GenerationRecord, HotRestartOutcome,
    HotRestartRequest, InstallSource, KeyAccess, KeyChange, KeyCheck, KeyRef, LaunchOptions, Line,
    LineCaps, LineProber, LocalContext, LoginStashInfo, ManualClock, MigrationPlan,
    MigrationReceipt, MigrationRequest, ModelInfo, PlannedFile, ProcessInfo, Profile, ProfileId,
    ProfileSet, ProfileStore, QuitOptions, QuitOutcome, RecoveryReport, RemoteApplyReport,
    RemoteHost, RemoteHosts, RemoteOs, RemotePlan, RemoteState, RemoteVerification,
    RemoteVerifyRequest, RenderInput, RenderedFiles, RendererState, ResolvedEndpoints,
    ResolvedProfile, Result, RollbackReport, Secret, SecretBackend, SecretBytes, SecretStore,
    SessionMigrator, SessionReport, ShellKind, Site, SiteId, StashScope, SwitchDecider,
    SwitchEvent, SwitchEvents, TargetId, ThreadActivity, ThreadRef, Timestamp, TurnActivity,
    async_trait, catalog,
};
use superba_router::config::RouterConfig;
use superba_router::watchdog::Liveness;
use superba_switch::router::{RouterControl, RouterEndpointInfo, RouterServiceInfo};
use superba_switch::{Switcher, SwitcherDeps, Tuning};
use url::Url;

pub mod v3;

pub type Log = Arc<Mutex<Vec<String>>>;

pub(crate) fn push(log: &Log, s: impl Into<String>) {
    log.lock().unwrap().push(s.into());
}

pub const T0: i64 = 1_760_000_000_000;
pub const KEY: &str = "sk-test-0000aaaa1111";
pub const OVERSEAS: &str = "https://api.krill-code.net";
pub const CN_FAST: &str = "https://api-slb.krill-code.net";

// ---------------------------------------------------------------------------
// Config engine
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeConfig {
    pub log: Log,
    pub state: Mutex<ConfigState>,
    pub baseline: Mutex<Option<Baseline>>,
    pub fail_plan: Mutex<Option<Error>>,
    pub fail_apply: Mutex<Option<Error>>,
    pub fail_rollback: Mutex<Option<Error>>,
    pub noop_plan: Mutex<bool>,
    pub login_stash: Mutex<Option<LoginStashInfo>>,
    pub generations: Mutex<Vec<GenerationRecord>>,
    /// Hash the live config takes after apply (simulates another writer when it
    /// differs from the reported hash).
    pub tamper_after_apply: Mutex<bool>,
    /// Another program rewrites config.toml after apply; `Some(drift)` lists the
    /// owned keys it changed (empty = only keys Superba does not own).
    pub config_rewritten_after_apply: Mutex<Option<Vec<KeyChange>>>,
    pub applied: Mutex<u64>,
    pub last_desired: Mutex<Option<String>>,
    /// Rendered restore destination, independent of the live configuration.
    pub restored_files: Mutex<Option<CodexFiles>>,
    pub restored_auth_mode: Mutex<Option<AuthMode>>,
}

/// Owned-key changes the fake reports: the base URL of token modes and every
/// Codex setting of the resolved profile.
fn fake_key_changes(desired: &ResolvedProfile, state: &ConfigState) -> Vec<KeyChange> {
    let mut out = Vec::new();
    if let superba_core::ResolvedMode::Token { endpoints, .. } = &desired.mode {
        let after = endpoints.openai_base_url.to_string();
        if state.openai_base_url.as_deref() != Some(after.as_str()) {
            out.push(KeyChange {
                path: "openai_base_url".into(),
                before: state.openai_base_url.clone(),
                after: Some(after),
            });
        }
    }
    for (path, entry) in desired.settings.iter() {
        out.push(KeyChange {
            path: path.to_owned(),
            before: None,
            after: entry.to_toml_repr(),
        });
    }
    out
}

pub fn base_state(home: &Path) -> ConfigState {
    ConfigState {
        codex_home: home.to_path_buf(),
        config_sha256: Some("cfg-0".into()),
        auth_sha256: Some("auth-0".into()),
        parse_error: None,
        model_provider: Some("openai".into()),
        provider_table_present: false,
        openai_base_url: Some(format!("{OVERSEAS}/backend-api/codex")),
        chatgpt_base_url: None,
        model: None,
        model_owned: false,
        reasoning_effort: None,
        profile_override: None,
        cli_auth_credentials_store: None,
        auth: AuthShape::ApiKey {
            key_last4: "1111".into(),
        },
        active: None,
        drift: Vec::new(),
        auth_drift: None,
        handed_off: None,
        since_release: Vec::new(),
        since_release_auth: None,
        journal_pending: false,
    }
}

impl FakeConfig {
    fn new(log: Log, home: &Path) -> Self {
        Self {
            log,
            state: Mutex::new(base_state(home)),
            baseline: Mutex::new(Some(Baseline {
                captured_at: Timestamp(T0 - 1_000_000),
                codex_home: home.to_path_buf(),
                config_sha256: Some("cfg-orig".into()),
                auth_sha256: Some("auth-orig".into()),
                keys: Vec::new(),
                provider_key: None,
                provider_table_present: false,
                auth: AuthShape::ChatGpt {
                    email: None,
                    account_id: None,
                },
                login_stashed: true,
                session_distribution: BTreeMap::new(),
                env: Vec::new(),
                legacy_env_lines: Vec::new(),
                first_write_backups: Vec::new(),
                released: false,
            })),
            fail_plan: Mutex::new(None),
            fail_apply: Mutex::new(None),
            fail_rollback: Mutex::new(None),
            noop_plan: Mutex::new(false),
            login_stash: Mutex::new(Some(LoginStashInfo {
                stashed_at: Timestamp(T0 - 86_400_000),
                last_refresh: None,
                id_token_exp: None,
                email: None,
            })),
            generations: Mutex::new(Vec::new()),
            tamper_after_apply: Mutex::new(false),
            config_rewritten_after_apply: Mutex::new(None),
            applied: Mutex::new(0),
            last_desired: Mutex::new(None),
            restored_files: Mutex::new(None),
            restored_auth_mode: Mutex::new(None),
        }
    }

    pub fn restore_token(&self, mode: AuthMode) {
        *self.restored_auth_mode.lock().unwrap() = Some(mode);
        *self.restored_files.lock().unwrap() = Some(CodexFiles {
            config_toml: Some(SecretBytes::new(format!(
                "model_provider = \"openai\"\nopenai_base_url = \"{OVERSEAS}/backend-api/codex\"\nchatgpt_base_url = \"{OVERSEAS}/backend-api/\"\n"
            ).into_bytes())),
            auth_json: Some(superba_config::authjson::render_token(mode, &Secret::new(KEY)).unwrap()),
        });
    }
}

fn mode_name(d: &ResolvedProfile) -> String {
    match &d.mode {
        superba_core::ResolvedMode::Token {
            auth, endpoints, ..
        } => format!("token:{auth:?}:{}", endpoints.openai_base_url),
        superba_core::ResolvedMode::Official => "official".into(),
        superba_core::ResolvedMode::Original { provider } => format!("original:{provider:?}"),
        superba_core::ResolvedMode::Generation { generation } => format!("generation:{generation}"),
        _ => "other".into(),
    }
}

#[async_trait]
impl ConfigEngine for FakeConfig {
    fn render(&self, _input: &RenderInput<'_>) -> Result<RenderedFiles> {
        Err(Error::not_implemented("fake render"))
    }

    fn inspect_files(&self, home: &Path, files: &CodexFiles) -> Result<ConfigState> {
        // This engine method is pure; all IO remains in the fake.
        superba_config::TomlConfigEngine::new(LocalContext::for_tests(home))
            .inspect_files(home, files)
    }

    fn capture_baseline(
        &self,
        _home: &Path,
        _files: &CodexFiles,
        _extras: BaselineExtras,
        _now: Timestamp,
    ) -> Result<Baseline> {
        Err(Error::not_implemented("fake capture_baseline"))
    }

    async fn read_state(&self, _home: &CodexHome) -> Result<ConfigState> {
        Ok(self.state.lock().unwrap().clone())
    }

    async fn baseline(&self, _home: &CodexHome) -> Result<Option<Baseline>> {
        Ok(self.baseline.lock().unwrap().clone())
    }

    async fn snapshot_baseline(
        &self,
        home: &CodexHome,
        extras: BaselineExtras,
    ) -> Result<Baseline> {
        push(&self.log, "config.snapshot_baseline");
        let state = self.state.lock().unwrap().clone();
        let b = Baseline {
            captured_at: Timestamp(T0),
            codex_home: home.root().to_path_buf(),
            config_sha256: Some("cfg-0".into()),
            auth_sha256: Some("auth-0".into()),
            keys: Vec::new(),
            provider_key: state.model_provider.clone(),
            provider_table_present: false,
            auth: state.auth.clone(),
            login_stashed: false,
            session_distribution: extras.session_distribution,
            env: extras.env,
            legacy_env_lines: extras.legacy_env_lines,
            first_write_backups: Vec::new(),
            released: false,
        };
        *self.baseline.lock().unwrap() = Some(b.clone());
        Ok(b)
    }

    async fn plan(&self, home: &CodexHome, desired: &ResolvedProfile) -> Result<ApplyPlan> {
        push(&self.log, format!("config.plan {}", mode_name(desired)));
        *self.last_desired.lock().unwrap() = Some(mode_name(desired));
        if let Some(e) = self.fail_plan.lock().unwrap().clone() {
            return Err(e);
        }
        // Like the real engine: drifted owned keys need an answer each.
        if self
            .state
            .lock()
            .unwrap()
            .drift
            .iter()
            .any(|k| !desired.key_resolutions.contains_key(&k.path))
        {
            return Err(Error::Conflict {
                message: "owned keys were edited".into(),
            });
        }
        let noop = *self.noop_plan.lock().unwrap();
        let restored = matches!(
            desired.mode,
            superba_core::ResolvedMode::Generation { .. }
                | superba_core::ResolvedMode::Original { .. }
        )
        .then(|| self.restored_files.lock().unwrap().clone())
        .flatten();
        let change = |sha: &str, contents: Option<&SecretBytes>| {
            if noop {
                FileChange::Unchanged
            } else {
                FileChange::Write {
                    sha256: sha.into(),
                    contents: contents
                        .cloned()
                        .unwrap_or_else(|| SecretBytes::new(b"x".to_vec())),
                }
            }
        };
        let gen_no = *self.applied.lock().unwrap() + 1;
        Ok(ApplyPlan {
            id: format!("apply-{gen_no}"),
            codex_home: home.root().to_path_buf(),
            generation: gen_no,
            profile_id: desired.profile_id.clone(),
            site_id: desired.site_id.clone(),
            line_id: desired.line_id.clone(),
            auth_mode: desired.auth_mode().or_else(|| {
                restored
                    .as_ref()
                    .and(*self.restored_auth_mode.lock().unwrap())
            }),
            files: vec![
                PlannedFile {
                    kind: CodexFileKind::AuthJson,
                    path: home.auth_json(),
                    before_sha256: Some("auth-0".into()),
                    change: change(
                        &format!("auth-{gen_no}"),
                        restored.as_ref().and_then(|f| f.auth_json.as_ref()),
                    ),
                },
                PlannedFile {
                    kind: CodexFileKind::ConfigToml,
                    path: home.config_toml(),
                    before_sha256: Some("cfg-0".into()),
                    change: change(
                        &format!("cfg-{gen_no}"),
                        restored.as_ref().and_then(|f| f.config_toml.as_ref()),
                    ),
                },
            ],
            key_changes: if noop {
                Vec::new()
            } else {
                fake_key_changes(desired, &self.state.lock().unwrap())
            },
            auth_change: None,
            notes: Vec::new(),
            desired: Some(desired.clone()),
            releases_ownership: false,
        })
    }

    async fn apply(&self, plan: &ApplyPlan) -> Result<ApplyReport> {
        push(&self.log, "config.apply");
        if let Some(e) = self.fail_apply.lock().unwrap().clone() {
            return Err(e);
        }
        let mut st = self.state.lock().unwrap();
        let before_cfg = st.config_sha256.clone();
        let before_auth = st.auth_sha256.clone();
        let g = plan.generation;
        st.config_sha256 = Some(format!("cfg-{g}"));
        st.auth_sha256 = Some(format!("auth-{g}"));
        if *self.tamper_after_apply.lock().unwrap() {
            st.auth_sha256 = Some("tampered".into());
        }
        if let Some(drift) = self.config_rewritten_after_apply.lock().unwrap().clone() {
            st.config_sha256 = Some("rewritten-by-the-app".into());
            st.drift = drift;
        }
        st.active = Some(ActiveState {
            profile_id: plan.profile_id.clone(),
            site_id: plan.site_id.clone(),
            line_id: plan.line_id.clone(),
            auth_mode: plan.auth_mode,
            generation: g,
            applied_at: Timestamp(T0),
        });
        if let Some(m) = plan.auth_mode {
            let last4 = match plan.desired.as_ref().map(|d| &d.mode) {
                Some(superba_core::ResolvedMode::Token { key, .. }) => key.last4(),
                _ => "1111".into(),
            };
            st.auth = match m {
                AuthMode::Pat => AuthShape::Pat { key_last4: last4 },
                AuthMode::ApiKey => AuthShape::ApiKey { key_last4: last4 },
                _ => AuthShape::ChatGpt {
                    email: None,
                    account_id: None,
                },
            };
        }
        if let Some(superba_core::ResolvedMode::Token { endpoints, .. }) =
            plan.desired.as_ref().map(|d| &d.mode)
        {
            st.openai_base_url = Some(endpoints.openai_base_url.to_string());
        }
        *self.applied.lock().unwrap() = g;
        if let Some(baseline) = self.baseline.lock().unwrap().as_mut() {
            baseline.released = matches!(
                plan.desired.as_ref().map(|d| &d.mode),
                Some(
                    superba_core::ResolvedMode::Original { .. }
                        | superba_core::ResolvedMode::Official
                )
            );
        }
        Ok(ApplyReport {
            journal_id: plan.id.clone(),
            codex_home: plan.codex_home.clone(),
            generation: g,
            applied_at: Timestamp(T0),
            files: vec![
                FileWrite {
                    kind: CodexFileKind::AuthJson,
                    path: plan.codex_home.join("auth.json"),
                    before_sha256: before_auth,
                    after_sha256: Some(format!("auth-{g}")),
                    backup: None,
                },
                FileWrite {
                    kind: CodexFileKind::ConfigToml,
                    path: plan.codex_home.join("config.toml"),
                    before_sha256: before_cfg,
                    after_sha256: Some(format!("cfg-{g}")),
                    backup: None,
                },
            ],
        })
    }

    async fn rollback(&self, report: &ApplyReport) -> Result<()> {
        push(&self.log, "config.rollback");
        if let Some(e) = self.fail_rollback.lock().unwrap().clone() {
            return Err(e);
        }
        let mut st = self.state.lock().unwrap();
        for f in &report.files {
            match f.kind {
                CodexFileKind::ConfigToml => st.config_sha256 = f.before_sha256.clone(),
                CodexFileKind::AuthJson => st.auth_sha256 = f.before_sha256.clone(),
            }
        }
        Ok(())
    }

    async fn recover(&self, _home: &CodexHome) -> Result<RecoveryReport> {
        push(&self.log, "config.recover");
        Ok(RecoveryReport::default())
    }

    async fn generations(&self, _home: &CodexHome) -> Result<Vec<GenerationRecord>> {
        Ok(self.generations.lock().unwrap().clone())
    }

    async fn active_credentials(&self, _home: &CodexHome) -> Result<Option<ActiveCredentials>> {
        let files = self.restored_files.lock().unwrap().clone();
        Ok(files.and_then(|files| {
            let (auth_mode, key) = files
                .auth_json
                .as_ref()
                .and_then(|b| superba_config::authjson::token_credentials(b.expose()))?;
            Some(ActiveCredentials {
                auth_mode,
                key,
                openai_base_url: Some(
                    Url::parse(&format!("{OVERSEAS}/backend-api/codex")).unwrap(),
                ),
                chatgpt_base_url: Some(Url::parse(&format!("{OVERSEAS}/backend-api/")).unwrap()),
            })
        }))
    }

    async fn login_stash(&self, _scope: &StashScope) -> Result<Option<LoginStashInfo>> {
        Ok(self.login_stash.lock().unwrap().clone())
    }

    async fn stash_login(
        &self,
        _scope: &StashScope,
        _auth: &SecretBytes,
    ) -> Result<LoginStashInfo> {
        Err(Error::not_implemented("fake stash_login"))
    }

    async fn load_login_stash(&self, _scope: &StashScope) -> Result<Option<SecretBytes>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Env persister
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeEnv {
    pub log: Log,
    pub report: Mutex<EnvReport>,
    pub fail_apply: Mutex<Option<Error>>,
    pub fail_revert: Mutex<Option<Error>>,
    pub plans: Mutex<Vec<EnvOp>>,
    /// Variable and operation of every applied plan.
    pub applied: Mutex<Vec<(String, EnvOp)>>,
    pub hang_on_revert: Mutex<bool>,
    pub revert_started: tokio::sync::Notify,
    /// `plan_launch` answers like Windows with something missing (one
    /// `HKCU\Environment` edit); `None` = not implemented (other platforms).
    pub launch_edit: Mutex<Option<EnvLocation>>,
    /// Values `plan_launch` was asked for.
    pub launch_plans: Mutex<Vec<String>>,
}

#[async_trait]
impl EnvPersister for FakeEnv {
    async fn plan_launch(&self, var: &str, value: &str) -> Result<EnvPlan> {
        self.launch_plans.lock().unwrap().push(value.to_owned());
        let Some(location) = self.launch_edit.lock().unwrap().clone() else {
            return Err(Error::not_implemented("plan_launch"));
        };
        Ok(EnvPlan {
            var: var.into(),
            op: EnvOp::Set {
                value: value.into(),
                take_over_foreign: false,
            },
            edits: vec![EnvEdit {
                location,
                action: EnvEditAction::SetValue { value: var.into() },
                preview: var.into(),
                before: None,
                after: Some(var.into()),
            }],
            scope_notes: Vec::new(),
        })
    }

    async fn inspect(&self, _var: &str) -> Result<EnvReport> {
        Ok(self.report.lock().unwrap().clone())
    }

    async fn plan(&self, var: &str, op: EnvOp) -> Result<EnvPlan> {
        self.plans.lock().unwrap().push(op.clone());
        let edits = match &op {
            EnvOp::Set { value, .. } => vec![EnvEdit {
                location: EnvLocation::ShellRc {
                    path: PathBuf::from("/fake/.zshenv"),
                    shell: ShellKind::Zsh,
                },
                action: EnvEditAction::WriteBlock,
                preview: format!("export {var}=\"{value}\""),
                before: None,
                after: Some("rc-1".into()),
            }],
            EnvOp::Clear | EnvOp::RestoreBaseline { .. } => vec![EnvEdit {
                location: EnvLocation::ShellRc {
                    path: PathBuf::from("/fake/.zshenv"),
                    shell: ShellKind::Zsh,
                },
                action: EnvEditAction::RemoveBlock,
                preview: String::new(),
                before: Some("rc-1".into()),
                after: None,
            }],
        };
        Ok(EnvPlan {
            var: var.into(),
            op,
            edits,
            scope_notes: Vec::new(),
        })
    }

    async fn apply(&self, plan: &EnvPlan) -> Result<EnvApplied> {
        push(&self.log, "env.apply");
        if let Some(e) = self.fail_apply.lock().unwrap().clone() {
            return Err(e);
        }
        self.applied
            .lock()
            .unwrap()
            .push((plan.var.clone(), plan.op.clone()));
        Ok(EnvApplied {
            id: "env-1".into(),
            var: plan.var.clone(),
            edits: plan.edits.clone(),
            backups: Vec::new(),
            applied_at: Timestamp(T0),
        })
    }

    async fn revert(&self, _applied: &EnvApplied) -> Result<()> {
        push(&self.log, "env.revert");
        self.revert_started.notify_one();
        if *self.hang_on_revert.lock().unwrap() {
            std::future::pending::<()>().await;
        }
        if let Some(e) = self.fail_revert.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Secrets and profiles
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct FakeSecrets {
    pub map: Mutex<BTreeMap<String, String>>,
    pub deleted: Mutex<Vec<KeyRef>>,
}

fn ref_name(r: &KeyRef) -> String {
    match r {
        KeyRef::Keyring { account, .. } => account.clone(),
        KeyRef::File { path } => path.display().to_string(),
        KeyRef::Env { var } => format!("env:{var}"),
        _ => "other".into(),
    }
}

#[async_trait]
impl SecretStore for FakeSecrets {
    async fn get(&self, key_ref: &KeyRef) -> Result<Secret> {
        self.map
            .lock()
            .unwrap()
            .get(&ref_name(key_ref))
            .map(|s| Secret::new(s.clone()))
            .ok_or_else(|| Error::Secret {
                message: "no such key".into(),
            })
    }

    async fn put(&self, profile_id: &ProfileId, secret: &Secret) -> Result<KeyRef> {
        let account = format!("profile:{profile_id}");
        self.map
            .lock()
            .unwrap()
            .insert(account.clone(), secret.expose().to_owned());
        Ok(KeyRef::Keyring {
            service: "superba".into(),
            account,
        })
    }

    async fn delete(&self, key_ref: &KeyRef) -> Result<()> {
        self.deleted.lock().unwrap().push(key_ref.clone());
        self.map.lock().unwrap().remove(&ref_name(key_ref));
        Ok(())
    }

    fn backend(&self) -> SecretBackend {
        SecretBackend::OsKeyring
    }
}

#[derive(Debug)]
pub struct FakeProfiles {
    pub set: Mutex<ProfileSet>,
    pub saves: Mutex<u32>,
}

#[async_trait]
impl ProfileStore for FakeProfiles {
    async fn load(&self) -> Result<ProfileSet> {
        Ok(self.set.lock().unwrap().clone())
    }

    async fn save(&self, set: &ProfileSet) -> Result<()> {
        *self.saves.lock().unwrap() += 1;
        *self.set.lock().unwrap() = set.clone();
        Ok(())
    }
}

pub fn krill_profile(id: &str, line: Option<&str>, auth: AuthMode) -> Profile {
    Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        id: ProfileId::new(id),
        name: id.into(),
        builtin: None,
        site_id: SiteId::new(catalog::KRILL_SITE_ID),
        line_id: line.map(superba_core::LineId::new),
        auth_mode: auth,
        key_ref: Some(KeyRef::Keyring {
            service: "superba".into(),
            account: format!("profile:{id}"),
        }),
        base_url_override: None,
        model: Default::default(),
        auth: superba_core::AuthChoice::from_mode(auth),
        backup_lines: Vec::new(),
        codex_settings: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// Line prober
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeProber {
    pub log: Log,
    /// Caps per line id (default: everything works).
    pub caps: Mutex<BTreeMap<String, LineCaps>>,
    pub probe_error: Mutex<Option<Error>>,
    pub authapi: Mutex<BTreeMap<String, AuthApiHealth>>,
    pub models: Mutex<std::result::Result<Vec<String>, Error>>,
    pub authapi_checks: Mutex<Vec<String>>,
    /// `model_list` answers (`None` = not implemented).
    pub model_list: Mutex<Option<std::result::Result<superba_core::ModelList, Error>>>,
    /// Base URLs `model_list` was called with.
    pub model_list_calls: Mutex<Vec<String>>,
    /// The Codex version of each `model_list` call.
    pub model_list_versions: Mutex<Vec<Option<String>>>,
}

pub fn good_caps(site: &str, line: &str) -> LineCaps {
    LineCaps {
        site_id: SiteId::new(site),
        line_id: superba_core::LineId::new(line),
        checked_at: Timestamp(T0),
        reachable: true,
        latency_ms: Some(80),
        pat_ok: true,
        apikey_ok: true,
        codex_path: Some(CodexPath::BackendApiCodex),
        blocked_by_edge: false,
        key_access: Some(KeyAccess::Ok),
        checks: Vec::new(),
        detail: None,
    }
}

#[async_trait]
impl LineProber for FakeProber {
    async fn probe(&self, site: &Site, line: &Line) -> Result<LineCaps> {
        Ok(self
            .caps
            .lock()
            .unwrap()
            .get(line.id.as_str())
            .cloned()
            .unwrap_or_else(|| good_caps(site.id.as_str(), line.id.as_str())))
    }

    async fn probe_all(&self, site: &Site) -> Result<Vec<LineCaps>> {
        let mut out = Vec::new();
        for l in &site.lines {
            out.push(self.probe(site, l).await?);
        }
        Ok(out)
    }

    async fn probe_with_key(&self, site: &Site, line: &Line, _key: &Secret) -> Result<LineCaps> {
        push(&self.log, format!("prober.probe_with_key {}", line.id));
        if let Some(e) = self.probe_error.lock().unwrap().clone() {
            return Err(e);
        }
        self.probe(site, line).await
    }

    async fn check_key(
        &self,
        site: &Site,
        line: &Line,
        _mode: AuthMode,
        key: &Secret,
    ) -> Result<KeyCheck> {
        Ok(KeyCheck {
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            checked_at: Timestamp(T0),
            key_last4: key.last4(),
            whoami: Some(AuthApiHealth::Healthy),
            identity: None,
            access: KeyAccess::Ok,
            codex_path: Some(CodexPath::BackendApiCodex),
        })
    }

    async fn check_authapi(
        &self,
        _site: &Site,
        origin: &superba_core::Origin,
        _key: &Secret,
    ) -> Result<AuthApiHealth> {
        self.authapi_checks.lock().unwrap().push(origin.as_str());
        Ok(self
            .authapi
            .lock()
            .unwrap()
            .get(&origin.as_str())
            .cloned()
            .unwrap_or(AuthApiHealth::Healthy))
    }

    async fn list_models(&self, url: &Url, _key: &Secret) -> Result<Vec<ModelInfo>> {
        push(&self.log, format!("prober.list_models {url}"));
        self.models.lock().unwrap().clone().map(|v| {
            v.into_iter()
                .map(|id| ModelInfo {
                    service_tiers: Vec::new(),
                    context_window: None,
                    max_context_window: None,
                    raw: None,
                    id,
                    display_name: None,
                })
                .collect()
        })
    }

    async fn probe_responses(
        &self,
        _endpoints: &ResolvedEndpoints,
        _key: &Secret,
        _model: &str,
    ) -> Result<superba_core::ResponsesProbe> {
        Err(Error::not_implemented("fake probe_responses"))
    }

    async fn account_summary(
        &self,
        _site: &Site,
        _line: &Line,
        _key: &Secret,
    ) -> Result<Option<superba_core::AccountSummary>> {
        Ok(None)
    }

    async fn model_list(
        &self,
        request: superba_core::ModelListRequest<'_>,
    ) -> superba_core::Result<superba_core::ModelList> {
        self.model_list_calls
            .lock()
            .unwrap()
            .push(request.base_url.to_string());
        self.model_list_versions
            .lock()
            .unwrap()
            .push(request.client_version.map(ToString::to_string));
        match self.model_list.lock().unwrap().clone() {
            Some(answer) => answer,
            None => Err(superba_core::Error::not_implemented(
                "LineProber::model_list",
            )),
        }
    }

    async fn usage_report(
        &self,
        _: &superba_core::Site,
        _: &superba_core::Line,
        _: &superba_core::Secret,
        _: &superba_core::UsageQuery,
    ) -> superba_core::Result<superba_core::UsageReport> {
        Err(superba_core::Error::not_implemented(
            "LineProber::usage_report",
        ))
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub const ROUTER_TOKEN: &str = "sbr-local-router-token-0001";

#[derive(Debug)]
pub struct FakeRouter {
    pub log: Log,
    /// Port of the stored configuration (`None` = not configured).
    pub port: Mutex<Option<u16>>,
    /// The router answers.
    pub up: Mutex<bool>,
    /// The service is installed.
    pub installed: Mutex<bool>,
    /// Configurations written by install / update (member ids in order).
    pub configs: Mutex<Vec<Vec<String>>>,
    pub fail_install: Mutex<Option<Error>>,
    /// Request log rows served by `requests`.
    pub page: Mutex<superba_core::RequestPage>,
    /// Stats settings handed to the router.
    pub stats_configured: Mutex<Vec<(superba_core::RouterStatsSettings, superba_core::PriceTable)>>,
    /// UI language of each `configure_stats` (the default line's language).
    pub stats_locales: Mutex<Vec<superba_core::Locale>>,
    pub clears: Mutex<Vec<superba_core::ClearRequest>>,
    pub inflight: Mutex<Vec<superba_core::InflightRequest>>,
    /// Recorder state the running router reports.
    pub stats_status: Mutex<Option<superba_core::RouterStatsStatus>>,
    /// WSL listeners of the stored configuration (bound while the router runs).
    pub wsl_listen: Mutex<Vec<std::net::Ipv4Addr>>,
    /// Firewall rules asked for (`address:port`); `Some(error)` fails them.
    pub firewall: Mutex<Vec<String>>,
    pub firewall_error: Mutex<Option<Error>>,
    /// Another program holds this port (user bug 65): a router that is down
    /// on it moves to `move_to` when asked, and on install.
    pub taken: Mutex<Option<u16>>,
    /// The port a move picks.
    pub move_to: Mutex<u16>,
    /// A move fails with this error.
    pub fail_move: Mutex<Option<Error>>,
}

impl FakeRouter {
    fn new(log: Log) -> Self {
        Self {
            log,
            port: Mutex::new(None),
            up: Mutex::new(false),
            installed: Mutex::new(false),
            configs: Mutex::new(Vec::new()),
            fail_install: Mutex::new(None),
            page: Mutex::new(superba_core::RequestPage::default()),
            stats_configured: Mutex::new(Vec::new()),
            stats_locales: Mutex::new(Vec::new()),
            clears: Mutex::new(Vec::new()),
            inflight: Mutex::new(Vec::new()),
            stats_status: Mutex::new(None),
            wsl_listen: Mutex::new(Vec::new()),
            firewall: Mutex::new(Vec::new()),
            firewall_error: Mutex::new(None),
            taken: Mutex::new(None),
            move_to: Mutex::new(19_440),
            fail_move: Mutex::new(None),
        }
    }

    /// The router is down and another program holds its stored port: the new
    /// port, logged and stored (`RouterManager::port_to_leave` + the move).
    fn leave_taken_port(&self) -> Option<u16> {
        let mut port = self.port.lock().unwrap();
        let old = (*port)?;
        if *self.up.lock().unwrap() || *self.taken.lock().unwrap() != Some(old) {
            return None;
        }
        let new = *self.move_to.lock().unwrap();
        push(&self.log, format!("router.move {old}->{new}"));
        *port = Some(new);
        Some(new)
    }

    pub fn base_url(port: u16) -> String {
        format!("http://127.0.0.1:{port}/backend-api/codex")
    }

    fn info(&self) -> Result<RouterEndpointInfo> {
        let port = self.port.lock().unwrap().ok_or_else(|| Error::NotFound {
            what: "router configuration".into(),
        })?;
        Ok(RouterEndpointInfo {
            base_url: Url::parse(&Self::base_url(port)).unwrap(),
            token: Secret::new(ROUTER_TOKEN),
            port,
        })
    }

    fn record(&self, what: &str, config: &RouterConfig, keys: &BTreeMap<String, Secret>) {
        let ids: Vec<String> = config.members.iter().map(|m| m.id.clone()).collect();
        assert!(
            ids.iter().all(|id| keys.contains_key(id)),
            "every member has a key"
        );
        push(&self.log, format!("router.{what} {}", ids.join(",")));
        self.configs.lock().unwrap().push(ids);
    }
}

#[async_trait]
impl RouterControl for FakeRouter {
    fn endpoint(&self) -> Result<RouterEndpointInfo> {
        self.info()
    }

    async fn install(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        if let Some(e) = self.fail_install.lock().unwrap().clone() {
            return Err(e);
        }
        self.record("install", &config, &keys);
        self.leave_taken_port();
        let mut port = self.port.lock().unwrap();
        if port.is_none() {
            *port = Some(if config.listen_port == 0 {
                18_431
            } else {
                config.listen_port
            });
        }
        drop(port);
        *self.up.lock().unwrap() = true;
        *self.installed.lock().unwrap() = true;
        self.info()
    }

    async fn update(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.record("update", &config, &keys);
        self.info()
    }

    async fn rewrite(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.record("rewrite", &config, &keys);
        self.info()
    }

    async fn uninstall(&self) -> Result<()> {
        push(&self.log, "router.uninstall");
        *self.up.lock().unwrap() = false;
        *self.installed.lock().unwrap() = false;
        Ok(())
    }

    async fn move_off_taken_port(&self) -> Result<Option<RouterEndpointInfo>> {
        if let Some(e) = self.fail_move.lock().unwrap().clone() {
            return Err(e);
        }
        if self.leave_taken_port().is_none() {
            return Ok(None);
        }
        *self.up.lock().unwrap() = true;
        self.info().map(Some)
    }

    async fn overview(&self) -> (RouterServiceInfo, Liveness) {
        let port = *self.port.lock().unwrap();
        let installed = *self.installed.lock().unwrap();
        let liveness = self.liveness().await;
        (
            RouterServiceInfo {
                configured: port.is_some(),
                installed,
                port,
                detail: None,
            },
            liveness,
        )
    }

    async fn liveness(&self) -> Liveness {
        let port = self.port.lock().unwrap().unwrap_or(18_431);
        if *self.up.lock().unwrap() {
            Liveness::Up(Box::new(
                serde_json::from_value(serde_json::json!({
                    "service": "superba-router", "version": "test", "instance_id": "i",
                    "pid": 1, "listen": format!("127.0.0.1:{port}"), "base_url": Self::base_url(port),
                    "started_at_ms": 0, "uptime_ms": 0, "config_generation": 1,
                    "members": [], "totals": {"requests": 0, "active_requests": 0,
                    "websockets": 0, "active_websockets": 0, "failovers": 0,
                    "unavailable": 0, "rejected": 0, "ws_refused": 0},
                    "sticky_sessions": 0, "recent": [],
                    "wsl_listen": self.wsl_listen.lock().unwrap().iter()
                        .map(|ip| format!("{ip}:{port}")).collect::<Vec<_>>()
                }))
                .expect("router status fixture"),
            ))
        } else {
            Liveness::Down {
                detail: "connection refused".into(),
            }
        }
    }

    async fn requests(
        &self,
        _query: &superba_core::RequestQuery,
    ) -> Result<superba_core::RequestPage> {
        Ok(self.page.lock().unwrap().clone())
    }

    async fn requests_for_threads(
        &self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
        threads: Vec<String>,
    ) -> Result<Vec<superba_core::RequestRow>> {
        let page = self.page.lock().unwrap();
        Ok(page
            .rows
            .iter()
            .filter(|r| {
                from.is_none_or(|f| r.started_at >= f)
                    && to.is_none_or(|t| r.started_at < t)
                    && threads.iter().any(|t| {
                        r.thread_id.as_deref() == Some(t.as_str())
                            || r.prompt_cache_key.as_deref() == Some(t.as_str())
                    })
            })
            .cloned()
            .collect())
    }

    async fn clear_requests(
        &self,
        request: &superba_core::ClearRequest,
    ) -> Result<superba_core::ClearReport> {
        self.clears.lock().unwrap().push(request.clone());
        Ok(superba_core::ClearReport::default())
    }

    async fn inflight(&self) -> Result<Vec<superba_core::InflightRequest>> {
        Ok(self.inflight.lock().unwrap().clone())
    }

    async fn set_wsl_listen(&self, addresses: Vec<std::net::Ipv4Addr>) -> Result<bool> {
        push(
            &self.log,
            format!(
                "router.wsl_listen {}",
                addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        );
        *self.wsl_listen.lock().unwrap() = addresses;
        Ok(*self.up.lock().unwrap())
    }

    fn wsl_listen(&self) -> Vec<std::net::Ipv4Addr> {
        self.wsl_listen.lock().unwrap().clone()
    }

    async fn allow_wsl_firewall(
        &self,
        address: std::net::Ipv4Addr,
        port: u16,
    ) -> Result<superba_router::firewall::FirewallChange> {
        if let Some(e) = self.firewall_error.lock().unwrap().clone() {
            return Err(e);
        }
        push(&self.log, format!("router.firewall {address}:{port}"));
        self.firewall
            .lock()
            .unwrap()
            .push(format!("{address}:{port}"));
        Ok(superba_router::firewall::FirewallChange::Added)
    }

    async fn stats_status(&self) -> Option<superba_core::RouterStatsStatus> {
        self.stats_status.lock().unwrap().clone()
    }

    async fn configure_stats(
        &self,
        settings: &superba_core::RouterStatsSettings,
        prices: &superba_core::PriceTable,
        locale: superba_core::Locale,
    ) -> Result<()> {
        push(&self.log, "router.configure_stats");
        self.stats_locales.lock().unwrap().push(locale);
        self.stats_configured
            .lock()
            .unwrap()
            .push((settings.clone(), prices.clone()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeSessions {
    pub log: Log,
    pub report: Mutex<SessionReport>,
    pub fail_migrate: Mutex<Option<Error>>,
    pub fail_rollback: Mutex<Option<Error>>,
    pub history: Mutex<Vec<MigrationReceipt>>,
    pub requests: Mutex<Vec<MigrationRequest>>,
}

pub fn session_report(home: &Path, counts: &[(&str, u32)]) -> SessionReport {
    SessionReport {
        codex_home: home.to_path_buf(),
        scanned_at: Timestamp(T0),
        sqlite_paths: Vec::new(),
        by_provider: counts
            .iter()
            .map(|(p, n)| {
                (
                    (*p).to_owned(),
                    superba_core::ProviderSessions {
                        threads: *n,
                        rollouts: *n,
                        archived: 0,
                        compressed: 0,
                        distinct: *n,
                    },
                )
            })
            .collect(),
        inconsistent: 0,
        warnings: Vec::new(),
    }
}

#[async_trait]
impl SessionMigrator for FakeSessions {
    async fn scan(&self, _home: &CodexHome) -> Result<SessionReport> {
        Ok(self.report.lock().unwrap().clone())
    }

    async fn plan(&self, home: &CodexHome, request: &MigrationRequest) -> Result<MigrationPlan> {
        self.requests.lock().unwrap().push(request.clone());
        let n: u32 = request
            .selectors
            .iter()
            .map(|s| match s {
                superba_core::MigrationSelector::Provider { provider } => self
                    .report
                    .lock()
                    .unwrap()
                    .by_provider
                    .get(provider)
                    .map_or(0, |p| p.distinct),
                _ => 1,
            })
            .sum();
        Ok(MigrationPlan {
            id: "mig-1".into(),
            codex_home: home.root().to_path_buf(),
            request: request.clone(),
            thread_count: n,
            rollout_count: n,
            by_source_provider: BTreeMap::from([("krill".to_owned(), n)]),
            backup_dir: PathBuf::from("/fake/backup"),
            backup_bytes: 0,
            items: Vec::new(),
        })
    }

    async fn migrate(&self, home: &CodexHome, plan: &MigrationPlan) -> Result<MigrationReceipt> {
        push(&self.log, "sessions.migrate");
        if let Some(e) = self.fail_migrate.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(MigrationReceipt {
            id: plan.id.clone(),
            codex_home: home.root().to_path_buf(),
            migrated_at: Timestamp(T0),
            to_provider: plan.request.to_provider.clone(),
            by_source_provider: plan.by_source_provider.clone(),
            threads: plan.thread_count,
            rollouts: plan.rollout_count,
            backup_dir: plan.backup_dir.clone(),
            ledger_path: PathBuf::from("/fake/ledger"),
            rolled_back: false,
        })
    }

    async fn rollback(&self, _home: &CodexHome, id: &str) -> Result<RollbackReport> {
        push(&self.log, format!("sessions.rollback {id}"));
        if let Some(e) = self.fail_rollback.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(RollbackReport::default())
    }

    async fn history(&self, _home: &CodexHome) -> Result<Vec<MigrationReceipt>> {
        Ok(self.history.lock().unwrap().clone())
    }

    async fn recover(&self, _home: &CodexHome) -> Result<RollbackReport> {
        Ok(RollbackReport::default())
    }

    /// Reports three steps; a stop request while moving undoes (reported as a
    /// restore) and fails with `Cancelled`, like `CodexSessionMigrator`.
    async fn migrate_stopped_with_progress(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        stopped: &superba_core::ConsumersStopped,
        progress: Arc<dyn superba_core::SessionProgressSink>,
    ) -> Result<MigrationReceipt> {
        use superba_core::{SessionDirection, SessionProgress, SessionStage};
        let total = plan.thread_count;
        let report = |direction, stage, done, can_stop| {
            progress.report(SessionProgress {
                direction,
                stage,
                done,
                total,
                step_done: done,
                step_total: total,
                current: None,
                can_stop,
            })
        };
        report(SessionDirection::Migrate, SessionStage::Reading, 0, true);
        report(
            SessionDirection::Migrate,
            SessionStage::Moving,
            total / 2,
            true,
        );
        if progress.stop_requested() {
            push(&self.log, "sessions.stopped");
            report(SessionDirection::Restore, SessionStage::Moving, 0, false);
            report(SessionDirection::Restore, SessionStage::Done, total, false);
            return Err(Error::Cancelled);
        }
        let receipt = self.migrate_stopped(home, plan, stopped).await?;
        report(SessionDirection::Migrate, SessionStage::Done, total, false);
        Ok(receipt)
    }

    async fn rollback_stopped_with_progress(
        &self,
        home: &CodexHome,
        migration_id: &str,
        stopped: &superba_core::ConsumersStopped,
        progress: Arc<dyn superba_core::SessionProgressSink>,
    ) -> Result<RollbackReport> {
        use superba_core::{SessionDirection, SessionProgress, SessionStage};
        let r = self.rollback_stopped(home, migration_id, stopped).await?;
        progress.report(SessionProgress {
            direction: SessionDirection::Restore,
            stage: SessionStage::Done,
            done: 1,
            total: 1,
            step_done: 1,
            step_total: 1,
            current: None,
            can_stop: false,
        });
        Ok(r)
    }
}

// ---------------------------------------------------------------------------
// App controller
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeApp {
    pub log: Log,
    pub clock: Arc<ManualClock>,
    pub installs: Mutex<Vec<AppInstall>>,
    pub binaries: Mutex<Vec<CodexBinary>>,
    pub session: Mutex<Option<AppSession>>,
    pub bridge_ready: Mutex<bool>,
    pub processes: Mutex<Vec<ProcessInfo>>,
    /// Busy lists returned by successive `turn_activity` calls (then idle).
    pub turns: Mutex<VecDeque<Vec<ThreadActivity>>>,
    pub fail_hot_restart: Mutex<u32>,
    /// A hot restart spawns a new app-server (else verification sees none).
    pub restart_spawns: Mutex<bool>,
    pub quit_outcomes: Mutex<VecDeque<QuitOutcome>>,
    /// Successive `quit_report` answers (then: the main process still listed).
    pub quit_reports: Mutex<VecDeque<superba_core::QuitReport>>,
    /// Successive `end_app_processes` answers (then: nothing left).
    pub end_reports: Mutex<VecDeque<superba_core::QuitReport>>,
    pub fail_launch: Mutex<Option<Error>>,
    pub launches: Mutex<Vec<LaunchOptions>>,
    pub renderer: Mutex<RendererState>,
    /// Successive renderer reads; successful reads also update `renderer`.
    pub renderer_states: Mutex<VecDeque<Result<RendererState>>>,
    /// Renderer state after the next local hot restart (kicked to home ...).
    pub renderer_after_restart: Mutex<Option<RendererState>>,
    pub next_pid: Mutex<u32>,
    pub hosts: Mutex<Vec<RemoteHost>>,
    /// `codex debug models --bundled` output.
    pub bundled_dump: Mutex<String>,
    /// `codex debug models -c model_catalog_json=..` fails with this.
    pub preflight_error: Mutex<Option<Error>>,
    /// Binaries the preflight ran with.
    pub preflights: Mutex<Vec<PathBuf>>,
    /// Catalog files every Codex refuses (a stale file; rebuilt ones load).
    pub refused_catalogs: Mutex<Vec<PathBuf>>,
    /// Codex binaries that refuse every catalog file.
    pub refusing_binaries: Mutex<Vec<PathBuf>>,
    /// Process image of the app-server (`None` = not observable).
    pub app_server_exe: Mutex<Option<PathBuf>>,
    /// What a graceful `stop_process` answers per pid (default `Exited`); a
    /// forced stop always ends the process. Gone processes leave `processes`.
    pub stop_answers: Mutex<BTreeMap<u32, superba_core::StopOutcome>>,
    /// App-servers a launch or local hot restart starts, as (delay, lifetime)
    /// after it on tokio's clock, instead of one at once: the desktop app's
    /// short-lived first app-server before the real one (user bug 62).
    pub spawn_script: Mutex<Vec<(Duration, Option<Duration>)>>,
    /// Scheduled app-servers: (pid, from, until).
    scheduled: Mutex<Vec<(u32, tokio::time::Instant, Option<tokio::time::Instant>)>>,
}

pub fn app_server(pid: u32, ppid: u32, started: i64) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: Some(ppid),
        kind: CodexProcessKind::AppServer,
        exe: None,
        cmdline: vec!["codex".into(), "app-server".into()],
        started_at: Some(Timestamp(started)),
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

pub fn cli_process(pid: u32, kind: CodexProcessKind, started: i64) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: Some(1),
        kind,
        exe: None,
        cmdline: vec!["codex".into()],
        started_at: Some(Timestamp(started)),
        authapi_env: EnvObservation::Present(OVERSEAS.into()),
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

pub fn session(authapi: Option<&str>, cdp: bool) -> AppSession {
    AppSession {
        app_server_exe: None,
        install_id: Some("chatgpt".into()),
        main_pid: 100,
        started_at: Some(Timestamp(T0 - 3_600_000)),
        executable: Some(PathBuf::from("/opt/chatgpt/ChatGPT")),
        cdp: cdp.then_some(CdpEndpoint {
            port: 9333,
            owner_verified: true,
        }),
        launched_by_us: cdp,
        authapi_env: match authapi {
            Some(v) => EnvObservation::Present(v.into()),
            None => EnvObservation::Absent,
        },
        home_override: None,
        codex_home: None,
    }
}

pub fn busy(target: &str, title: &str) -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::new(target),
        thread_id: Some(format!("thread-{title}")),
        title: Some(title.into()),
        rollout_path: None,
        last_event_at: None,
        signal: superba_core::ActivitySignal::RolloutTail,
        originator: None,
        cwd: None,
    }
}

impl FakeApp {
    /// The app quit: no session, no app main / app-server processes.
    fn app_gone(&self) {
        *self.session.lock().unwrap() = None;
        self.processes.lock().unwrap().retain(|p| {
            !matches!(
                p.kind,
                CodexProcessKind::AppServer | CodexProcessKind::AppMain
            )
        });
    }

    fn spawn_app_server(&self) {
        let mut pid = self.next_pid.lock().unwrap();
        let now = self.clock.now().as_millis();
        let mut procs = self.processes.lock().unwrap();
        procs.retain(|p| !matches!(p.kind, CodexProcessKind::AppServer));
        let script = self.spawn_script.lock().unwrap().clone();
        let mut scheduled = self.scheduled.lock().unwrap();
        scheduled.clear();
        if script.is_empty() {
            *pid += 1;
            procs.push(app_server(*pid, 100, now));
            return;
        }
        let at = tokio::time::Instant::now();
        for (delay, life) in script {
            *pid += 1;
            scheduled.push((*pid, at + delay, life.map(|l| at + delay + l)));
        }
    }

    /// The scheduled app-servers running now (none once the app is gone).
    fn scheduled_now(&self) -> Vec<ProcessInfo> {
        if self.session.lock().unwrap().is_none() {
            return Vec::new();
        }
        let now = tokio::time::Instant::now();
        let started = self.clock.now().as_millis();
        self.scheduled
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, from, until)| *from <= now && until.is_none_or(|u| now < u))
            .map(|(pid, _, _)| app_server(*pid, 100, started))
            .collect()
    }
}

#[async_trait]
impl AppController for FakeApp {
    async fn detect_installs(&self) -> Result<Vec<AppInstall>> {
        Ok(self.installs.lock().unwrap().clone())
    }

    async fn codex_binaries(&self) -> Result<Vec<CodexBinary>> {
        Ok(self.binaries.lock().unwrap().clone())
    }

    async fn running_session(&self) -> Result<Option<AppSession>> {
        Ok(self.session.lock().unwrap().clone())
    }

    async fn activate(&self, session: &AppSession) -> Result<()> {
        push(&self.log, format!("app.activate {}", session.main_pid));
        Ok(())
    }

    async fn launch(&self, options: &LaunchOptions) -> Result<AppSession> {
        push(
            &self.log,
            format!(
                "app.launch authapi={}",
                options
                    .env
                    .get(superba_core::codex::ENV_AUTHAPI_BASE_URL)
                    .cloned()
                    .unwrap_or_default()
            ),
        );
        self.launches.lock().unwrap().push(options.clone());
        if let Some(e) = self.fail_launch.lock().unwrap().clone() {
            return Err(e);
        }
        let mut s = session(
            options
                .env
                .get(superba_core::codex::ENV_AUTHAPI_BASE_URL)
                .map(String::as_str),
            options.cdp != superba_core::CdpMode::Disabled,
        );
        s.main_pid = 200;
        *self.session.lock().unwrap() = Some(s.clone());
        self.spawn_app_server();
        Ok(s)
    }

    async fn probe_bridge(&self, _session: &AppSession) -> Result<BridgeStatus> {
        Ok(if *self.bridge_ready.lock().unwrap() {
            BridgeStatus::Ready {
                target_url: "app://-/index.html".into(),
            }
        } else {
            BridgeStatus::BridgeMissing
        })
    }

    async fn hot_restart(
        &self,
        _session: &AppSession,
        request: &HotRestartRequest,
    ) -> Result<HotRestartOutcome> {
        push(&self.log, format!("app.hot_restart {}", request.host_id));
        {
            let mut f = self.fail_hot_restart.lock().unwrap();
            if *f > 0 {
                *f -= 1;
                return Err(Error::Cdp {
                    message: "bridge gone".into(),
                });
            }
        }
        if request.host_id == "local" && *self.restart_spawns.lock().unwrap() {
            self.spawn_app_server();
        }
        if request.host_id == "local"
            && let Some(r) = self.renderer_after_restart.lock().unwrap().take()
        {
            *self.renderer.lock().unwrap() = r;
        }
        Ok(HotRestartOutcome {
            sent_at: self.clock.now(),
        })
    }

    async fn quit(&self, _session: &AppSession, options: &QuitOptions) -> Result<QuitOutcome> {
        push(&self.log, format!("app.quit force={}", options.force));
        let outcome = self
            .quit_outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(QuitOutcome::Exited);
        if outcome != QuitOutcome::TimedOut {
            self.app_gone();
        }
        Ok(outcome)
    }

    async fn quit_report(&self, session: &AppSession) -> Result<superba_core::QuitReport> {
        push(&self.log, "app.quit_report");
        Ok(self
            .quit_reports
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| superba_core::QuitReport {
                steps: vec![superba_core::QuitStep::ClosedWindows],
                left: vec![superba_core::LingeringProcess {
                    pid: session.main_pid,
                    role: "main".into(),
                    ..Default::default()
                }],
            }))
    }

    async fn end_app_processes(
        &self,
        _session: &AppSession,
        processes: &[superba_core::LingeringProcess],
    ) -> Result<superba_core::QuitReport> {
        let pids: Vec<String> = processes.iter().map(|p| p.pid.to_string()).collect();
        push(
            &self.log,
            format!("app.end_app_processes {}", pids.join(",")),
        );
        let after = self
            .end_reports
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        if after.left.is_empty() {
            self.app_gone();
        }
        Ok(after)
    }

    async fn processes(&self, _home: &CodexHome) -> Result<Vec<ProcessInfo>> {
        let mut out = self.processes.lock().unwrap().clone();
        out.extend(self.scheduled_now());
        Ok(out)
    }

    async fn turn_activity(
        &self,
        _home: &CodexHome,
        _session: Option<&AppSession>,
    ) -> Result<TurnActivity> {
        let busy = self.turns.lock().unwrap().pop_front().unwrap_or_default();
        Ok(TurnActivity {
            target_id: TargetId::local(),
            checked_at: self.clock.now(),
            busy,
            complete: true,
        })
    }

    async fn renderer_state(&self, _session: &AppSession) -> Result<RendererState> {
        if let Some(state) = self.renderer_states.lock().unwrap().pop_front() {
            *self.renderer.lock().unwrap() = state?;
        }
        Ok(self.renderer.lock().unwrap().clone())
    }

    async fn return_to_thread(&self, _session: &AppSession, thread: &ThreadRef) -> Result<()> {
        push(
            &self.log,
            format!("app.return_to_thread {}", thread.thread_id),
        );
        Ok(())
    }

    async fn restart_daemon(&self, _home: &CodexHome) -> Result<()> {
        Ok(())
    }

    async fn stop_process(
        &self,
        process: &ProcessInfo,
        force: bool,
        _timeout_ms: u64,
    ) -> Result<superba_core::StopOutcome> {
        use superba_core::StopOutcome;
        push(
            &self.log,
            format!("app.stop_process pid={} force={force}", process.pid),
        );
        let alive = self
            .processes
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.pid == process.pid && p.started_at == process.started_at);
        if !alive {
            return Ok(StopOutcome::NotRunning);
        }
        let outcome = if force {
            StopOutcome::Killed
        } else {
            self.stop_answers
                .lock()
                .unwrap()
                .get(&process.pid)
                .copied()
                .unwrap_or(StopOutcome::Exited)
        };
        if outcome.is_gone() {
            self.processes
                .lock()
                .unwrap()
                .retain(|p| p.pid != process.pid);
        }
        Ok(outcome)
    }

    async fn stop_daemon(&self, _home: &CodexHome) -> Result<()> {
        push(&self.log, "app.stop_daemon");
        self.processes
            .lock()
            .unwrap()
            .retain(|p| !matches!(p.kind, CodexProcessKind::CliDaemon));
        Ok(())
    }

    async fn remote_connections(&self, _session: &AppSession) -> Result<Vec<RemoteHost>> {
        Ok(self.hosts.lock().unwrap().clone())
    }

    async fn launcher_entry(&self) -> Result<bool> {
        Ok(false)
    }

    async fn set_launcher_entry(&self, enabled: bool) -> Result<()> {
        push(&self.log, format!("app.set_launcher_entry {enabled}"));
        Ok(())
    }

    async fn app_server_exe(&self, _session: &AppSession) -> Result<Option<PathBuf>> {
        Ok(self.app_server_exe.lock().unwrap().clone())
    }

    async fn app_server_floor(&self, _install: &AppInstall) -> Result<CodexVersion> {
        Ok(CodexVersion::new(0, 141, 0))
    }

    async fn codex_debug_models(&self, binary: &Path, catalog: Option<&Path>) -> Result<String> {
        match catalog {
            None => Ok(self.bundled_dump.lock().unwrap().clone()),
            Some(path) => {
                push(&self.log, format!("app.preflight {}", binary.display()));
                self.preflights.lock().unwrap().push(binary.to_path_buf());
                assert!(path.exists(), "the preflight copy exists while it runs");
                if self
                    .refused_catalogs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p == path)
                    || self
                        .refusing_binaries
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|b| b == binary)
                {
                    return Err(Error::parse("model_catalog_json", "missing field `x`"));
                }
                match self.preflight_error.lock().unwrap().clone() {
                    Some(e) => Err(e),
                    None => Ok(std::fs::read_to_string(path).unwrap()),
                }
            }
        }
    }

    async fn cli_install_kind(&self, _binary: &Path) -> Result<superba_core::CodexInstallKind> {
        Ok(superba_core::CodexInstallKind::Npm)
    }
}

// ---------------------------------------------------------------------------
// Remote hosts
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FakeRemote {
    pub log: Log,
    pub hosts: Mutex<Vec<RemoteHost>>,
    pub version: Mutex<Option<CodexVersion>>,
    pub shared_home: Mutex<bool>,
    pub fail_apply: Mutex<BTreeMap<String, Error>>,
    pub verify_ok: Mutex<BTreeMap<String, bool>>,
    pub busy: Mutex<BTreeMap<String, Vec<ThreadActivity>>>,
    pub incomplete: Mutex<Vec<String>>,
    /// The host's `config.toml` as the probe reads it.
    pub config_toml: Mutex<Option<String>>,
    /// Owned keys Superba wrote on the host last time.
    pub owned_paths: Mutex<Vec<String>>,
    /// Scoped plans asked for: (host, scope, desired).
    pub scoped: Mutex<Vec<(String, superba_core::RemoteApplyScope, ResolvedProfile)>>,
    /// Sessions a scan reports.
    pub sessions: Mutex<SessionReport>,
    /// No `codex app-server` runs on the host (the app is not connected to it).
    pub idle: Mutex<bool>,
    /// WSL (user bug 45): the distribution's default gateway.
    pub gateway: Mutex<Option<String>>,
    /// WSL: no Codex at all (no command, no files, no app-server).
    pub no_codex: Mutex<bool>,
    /// WSL: what `http_get` answers per `address:port` (missing = no answer).
    pub http: Mutex<BTreeMap<String, String>>,
    /// WSL: `http_get` calls (`host address:port path`).
    pub http_calls: Mutex<Vec<String>>,
    /// WSL: blocked plans per host label (e.g. no baseline).
    pub blocked: Mutex<BTreeMap<String, Vec<BlockReason>>>,
    /// WSL: helper runs (`host label`, arguments).
    pub helper_runs: Mutex<Vec<(String, Vec<String>)>>,
    /// Hosts (labels) Superba wrote before: they have a baseline (user bug 54).
    pub written: Mutex<Vec<String>>,
    /// Probes run, by host label.
    pub probes: Mutex<Vec<String>>,
    /// `wsl_running` calls.
    pub running_calls: Mutex<u32>,
    /// The host's `auth.json` as the probe reads it (`None` = missing).
    pub auth: Mutex<Option<AuthShape>>,
    /// `openai_base_url` as the probe reads it.
    pub openai_base_url: Mutex<Option<String>>,
    /// Plans list the host's `model` change and applies change the host like
    /// the real one (auth, line, `model` and its ownership; user bug 63).
    pub mirror: Mutex<bool>,
}

/// The top-level `model` of a fake host's `config.toml`.
pub fn toml_model(config: Option<&str>) -> Option<String> {
    config?
        .lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .find_map(|l| l.trim().strip_prefix("model = "))
        .map(|v| v.trim_matches('"').to_owned())
}

impl FakeRemote {
    /// The `model` value `desired` leaves on the host (`None` = absent).
    fn model_after(&self, desired: &ResolvedProfile) -> Option<String> {
        let config = self.config_toml.lock().unwrap().clone();
        let now = toml_model(config.as_deref());
        let owned = self
            .owned_paths
            .lock()
            .unwrap()
            .iter()
            .any(|p| p == "model");
        match (&desired.mode, &desired.model.model) {
            (superba_core::ResolvedMode::Token { .. }, Some(m)) => Some(m.clone()),
            // Released: the host had no model of its own.
            _ if owned => None,
            _ => now,
        }
    }

    fn mirror_apply(&self, desired: &ResolvedProfile) {
        let after = self.model_after(desired);
        let mut config = self.config_toml.lock().unwrap();
        let rest: Vec<String> = config
            .as_deref()
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim_start().starts_with("model = "))
            .map(str::to_owned)
            .collect();
        let mut lines = Vec::new();
        if let Some(m) = &after {
            lines.push(format!("model = \"{m}\""));
        }
        lines.extend(rest);
        *config = Some(lines.join("\n") + "\n");
        let mut owned = self.owned_paths.lock().unwrap();
        owned.retain(|p| p != "model");
        if let superba_core::ResolvedMode::Token {
            auth,
            endpoints,
            key,
            ..
        } = &desired.mode
        {
            if desired.model.model.is_some() {
                owned.push("model".into());
            }
            *self.auth.lock().unwrap() = Some(match auth {
                AuthMode::Pat => AuthShape::Pat {
                    key_last4: key.last4(),
                },
                _ => AuthShape::ApiKey {
                    key_last4: key.last4(),
                },
            });
            *self.openai_base_url.lock().unwrap() = Some(endpoints.openai_base_url.to_string());
        }
    }
}

pub fn remote_host(key: &str, host_id: Option<&str>) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(key),
        label: key.into(),
        host_id: host_id.map(str::to_owned),
        kind: superba_core::HostKind::Ssh,
        ssh: superba_core::SshEndpoint {
            alias: Some(key.into()),
            ..Default::default()
        },
        sources: vec![superba_core::HostSource::GlobalState],
        connected: Some(true),
        wsl: None,
    }
}

fn remote_state(host: &RemoteHost, version: Option<CodexVersion>, shared: bool) -> RemoteState {
    RemoteState {
        disk_free_kib: None,
        kernel: None,
        arch: None,
        codex_install: None,
        codex_resolved_path: None,
        target_id: host.target_id.clone(),
        probed_at: Timestamp(T0),
        os: RemoteOs::Linux,
        shell: ShellKind::Bash,
        codex_home: "/home/u/.codex".into(),
        codex_path: Some("/usr/bin/codex".into()),
        codex_version: version,
        config_sha256: None,
        auth_sha256: None,
        auth: AuthShape::Missing,
        model_provider: None,
        openai_base_url: None,
        machine_id: Some("m1".into()),
        marker_machine_id: Some(if shared { "m2" } else { "m1" }.into()),
        authapi_env: EnvObservation::Absent,
        app_server_pids: vec![900],
        owned_paths: Vec::new(),
        config_toml: None,
        app_bash: None,
        gateway: None,
    }
}

#[async_trait]
impl RemoteHosts for FakeRemote {
    async fn read_codex_file(
        &self,
        host: &RemoteHost,
        kind: superba_core::CodexFileKind,
    ) -> Result<superba_core::RemoteCodexFile> {
        self.log
            .lock()
            .unwrap()
            .push(format!("remote.read_codex_file:{}", host.label));
        let contents = match kind {
            superba_core::CodexFileKind::ConfigToml => self.config_toml.lock().unwrap().clone(),
            superba_core::CodexFileKind::AuthJson => None,
        };
        Ok(superba_core::RemoteCodexFile {
            path: format!("/home/u/.codex/{}", kind.file_name()),
            contents: contents.map(|t| superba_core::SecretBytes::new(t.into_bytes())),
            unreadable: None,
            os: RemoteOs::Linux,
            codex_version: self.version.lock().unwrap().clone(),
            owned: self
                .owned_paths
                .lock()
                .unwrap()
                .iter()
                .map(|p| superba_core::KeySnapshot {
                    path: p.clone(),
                    value: None,
                })
                .collect(),
        })
    }

    async fn write_codex_file(
        &self,
        host: &RemoteHost,
        write: &superba_core::RemoteFileWrite,
    ) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("remote.write_codex_file:{}", host.label));
        let mut current = self.config_toml.lock().unwrap();
        let expect = write
            .expect
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.expose()).into_owned());
        if *current != expect {
            return Err(Error::Conflict {
                message: "changed on the host".into(),
            });
        }
        *current = Some(String::from_utf8_lossy(write.contents.expose()).into_owned());
        Ok(())
    }

    async fn discover(
        &self,
        _home: &CodexHome,
        _from_app: &[RemoteHost],
        _live: &[ProcessInfo],
    ) -> Result<Vec<RemoteHost>> {
        Ok(self.hosts.lock().unwrap().clone())
    }

    async fn probe(&self, host: &RemoteHost) -> Result<RemoteState> {
        self.probes.lock().unwrap().push(host.label.clone());
        let mut state = remote_state(
            host,
            self.version.lock().unwrap().clone(),
            *self.shared_home.lock().unwrap(),
        );
        state.config_toml = self
            .config_toml
            .lock()
            .unwrap()
            .clone()
            .map(|t| SecretBytes::new(t.into_bytes()));
        state.owned_paths = self.owned_paths.lock().unwrap().clone();
        if let Some(a) = self.auth.lock().unwrap().clone() {
            state.auth = a;
        }
        state.openai_base_url = self.openai_base_url.lock().unwrap().clone();
        if *self.idle.lock().unwrap() {
            state.app_server_pids.clear();
        }
        if host.kind == superba_core::HostKind::Wsl {
            state.gateway = self.gateway.lock().unwrap().clone();
            state.app_bash = Some(true);
            if *self.no_codex.lock().unwrap() {
                state.codex_path = None;
                state.app_server_pids.clear();
            }
        }
        Ok(state)
    }

    async fn plan_apply(&self, host: &RemoteHost, desired: &ResolvedProfile) -> Result<RemotePlan> {
        let mut key_changes = Vec::new();
        if *self.mirror.lock().unwrap() {
            let before = toml_model(self.config_toml.lock().unwrap().as_deref());
            let after = self.model_after(desired);
            if before != after {
                let quoted = |m: Option<String>| m.map(|m| format!("\"{m}\""));
                key_changes.push(KeyChange {
                    path: "model".into(),
                    before: quoted(before),
                    after: quoted(after),
                });
            }
        }
        Ok(RemotePlan {
            target_id: host.target_id.clone(),
            state: self.probe(host).await?,
            files: Vec::new(),
            key_changes,
            auth_change: None,
            env: None,
            blocked: self
                .blocked
                .lock()
                .unwrap()
                .get(&host.label)
                .cloned()
                .unwrap_or_default(),
            warnings: Vec::new(),
            scope: superba_core::RemoteApplyScope::Both,
            desired: Some(desired.clone()),
        })
    }

    async fn apply(&self, host: &RemoteHost, plan: &RemotePlan) -> Result<RemoteApplyReport> {
        push(&self.log, format!("remote.apply {}", host.label));
        if let Some(e) = self.fail_apply.lock().unwrap().get(&host.label) {
            return Err(e.clone());
        }
        if *self.mirror.lock().unwrap()
            && let Some(desired) = &plan.desired
        {
            self.mirror_apply(desired);
        }
        Ok(RemoteApplyReport {
            target_id: host.target_id.clone(),
            applied_at: Timestamp(T0),
            files: Vec::new(),
            env: None,
        })
    }

    async fn rollback(&self, host: &RemoteHost, _report: &RemoteApplyReport) -> Result<()> {
        push(&self.log, format!("remote.rollback {}", host.label));
        Ok(())
    }

    async fn turn_activity(&self, host: &RemoteHost) -> Result<TurnActivity> {
        Ok(TurnActivity {
            target_id: host.target_id.clone(),
            checked_at: Timestamp(T0),
            busy: self
                .busy
                .lock()
                .unwrap()
                .get(&host.label)
                .cloned()
                .unwrap_or_default(),
            complete: !self.incomplete.lock().unwrap().contains(&host.label),
        })
    }

    async fn verify(
        &self,
        host: &RemoteHost,
        _request: &RemoteVerifyRequest,
    ) -> Result<RemoteVerification> {
        let ok = self
            .verify_ok
            .lock()
            .unwrap()
            .get(&host.label)
            .copied()
            .unwrap_or(true);
        Ok(RemoteVerification {
            new_app_server: ok,
            authapi_env: EnvObservation::Unknown,
            files_match: true,
            detail: (!ok).then(|| "no new app-server".into()),
        })
    }

    async fn plan_apply_scoped(
        &self,
        host: &RemoteHost,
        desired: &ResolvedProfile,
        scope: superba_core::RemoteApplyScope,
    ) -> Result<RemotePlan> {
        push(&self.log, format!("remote.plan {} {scope:?}", host.label));
        self.scoped
            .lock()
            .unwrap()
            .push((host.label.clone(), scope, desired.clone()));
        let mut plan = self.plan_apply(host, desired).await?;
        plan.scope = scope;
        Ok(plan)
    }

    async fn install_codex(
        &self,
        host: &RemoteHost,
        package: &Path,
        version: &CodexVersion,
    ) -> Result<superba_core::RemoteInstallReport> {
        push(
            &self.log,
            format!(
                "remote.install_codex {} {version} {}",
                host.label,
                package.display()
            ),
        );
        let previous = self.version.lock().unwrap().replace(version.clone());
        Ok(superba_core::RemoteInstallReport {
            target_id: host.target_id.clone(),
            version: version.clone(),
            executable: "/home/u/.local/bin/codex".into(),
            previous,
            at: Timestamp(T0),
        })
    }

    async fn rollback_codex(&self, host: &RemoteHost) -> Result<superba_core::RemoteInstallReport> {
        push(&self.log, format!("remote.rollback_codex {}", host.label));
        let previous = self.version.lock().unwrap().take();
        Ok(superba_core::RemoteInstallReport {
            target_id: host.target_id.clone(),
            version: CodexVersion::new(0, 150, 1),
            executable: "/home/u/.local/bin/codex".into(),
            previous,
            at: Timestamp(T0),
        })
    }

    async fn scan_sessions(&self, host: &RemoteHost) -> Result<SessionReport> {
        push(&self.log, format!("remote.scan_sessions {}", host.label));
        Ok(self.sessions.lock().unwrap().clone())
    }

    async fn http_get(
        &self,
        host: &RemoteHost,
        address: &str,
        port: u16,
        path: &str,
    ) -> Result<Option<String>> {
        self.http_calls
            .lock()
            .unwrap()
            .push(format!("{} {address}:{port} {path}", host.label));
        Ok(self
            .http
            .lock()
            .unwrap()
            .get(&format!("{address}:{port}"))
            .cloned())
    }

    async fn run_helper(
        &self,
        host: &RemoteHost,
        _helper: &Path,
        args: &[String],
        sink: superba_core::LineSink,
    ) -> Result<superba_core::HelperRun> {
        self.helper_runs
            .lock()
            .unwrap()
            .push((host.label.clone(), args.to_vec()));
        for line in ["Migrated 1 of 2 sessions", "Migrated 2 of 2 sessions"] {
            sink(line);
        }
        Ok(superba_core::HelperRun {
            exit_code: Some(0),
            uploaded: true,
            tail: vec!["Migrated 2 of 2 sessions".into()],
        })
    }

    async fn wsl_status(&self) -> Result<(superba_core::WslNetworking, Option<String>)> {
        Ok((superba_core::WslNetworking::Nat, None))
    }

    async fn wsl_running(&self) -> Result<Option<Vec<String>>> {
        *self.running_calls.lock().unwrap() += 1;
        // The fixtures' own states answer (discover returns them as they are).
        Ok(None)
    }

    async fn written(&self, host: &RemoteHost) -> Result<bool> {
        Ok(self.written.lock().unwrap().contains(&host.label))
    }
}

/// The router's `health` answer for the fake router's instance.
pub fn router_health() -> String {
    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"service\":\"superba-router\",\"version\":\"test\",\"instance_id\":\"i\"}".into()
}

/// A WSL distribution host (`version` 1 or 2, `networking`).
pub fn wsl_host(distro: &str, version: u8, networking: superba_core::WslNetworking) -> RemoteHost {
    RemoteHost {
        target_id: superba_core::wsl::target_id(distro),
        label: format!("WSL: {distro}"),
        host_id: Some(superba_core::wsl::host_id(distro)),
        kind: superba_core::HostKind::Wsl,
        ssh: superba_core::SshEndpoint::default(),
        sources: vec![superba_core::HostSource::Wsl],
        connected: None,
        wsl: Some(superba_core::WslDistro {
            version: Some(version),
            networking,
            ..superba_core::WslDistro::named(distro)
        }),
    }
}

// ---------------------------------------------------------------------------
// Decider and events
// ---------------------------------------------------------------------------

/// Answers from a script per request kind; records every question.
#[derive(Debug, Default)]
pub struct ScriptedDecider {
    pub answers: Mutex<BTreeMap<&'static str, VecDeque<Decision>>>,
    pub asked: Mutex<Vec<DecisionRequest>>,
    /// Never answer `WaitForTurns` (the executor must drop it when turns end).
    pub hang_on_wait: Mutex<bool>,
}

pub fn request_kind(r: &DecisionRequest) -> &'static str {
    match r {
        DecisionRequest::WaitForTurns { .. } => "wait_for_turns",
        DecisionRequest::ForceQuit { .. } => "force_quit",
        DecisionRequest::AppStillRunning { .. } => "app_still_running",
        DecisionRequest::IpcFailed { .. } => "ipc_failed",
        DecisionRequest::VerificationFailed { .. } => "verification_failed",
        DecisionRequest::UnknownRemoteActivity { .. } => "unknown_remote_activity",
        DecisionRequest::CloseConsumers { .. } => "close_consumers",
        DecisionRequest::ForceStop { .. } => "force_stop",
        _ => "other",
    }
}

impl ScriptedDecider {
    pub fn with(answers: &[(&'static str, Decision)]) -> Arc<Self> {
        let d = Self::default();
        {
            let mut map = d.answers.lock().unwrap();
            for (k, v) in answers {
                map.entry(k).or_default().push_back(*v);
            }
        }
        Arc::new(d)
    }

    pub fn asked_kinds(&self) -> Vec<&'static str> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(request_kind)
            .collect()
    }
}

#[async_trait]
impl SwitchDecider for ScriptedDecider {
    async fn decide(&self, request: DecisionRequest) -> Decision {
        let kind = request_kind(&request);
        self.asked.lock().unwrap().push(request.clone());
        if kind == "wait_for_turns" && *self.hang_on_wait.lock().unwrap() {
            std::future::pending::<()>().await;
        }
        let next = self
            .answers
            .lock()
            .unwrap()
            .get_mut(kind)
            .and_then(VecDeque::pop_front);
        next.unwrap_or_else(|| request.safe_default())
    }
}

#[derive(Debug, Default)]
pub struct RecordingEvents {
    pub events: Mutex<Vec<SwitchEvent>>,
    /// Answer `stop_requested` with true (the user stopped the migration).
    pub stop: std::sync::atomic::AtomicBool,
}

impl RecordingEvents {
    pub fn phases(&self) -> Vec<superba_core::SwitchPhase> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                SwitchEvent::Phase { phase, .. } => Some(*phase),
                _ => None,
            })
            .collect()
    }
}

impl SwitchEvents for RecordingEvents {
    fn emit(&self, event: SwitchEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

pub struct World {
    pub tmp: tempfile::TempDir,
    pub ctx: LocalContext,
    pub log: Log,
    pub clock: Arc<ManualClock>,
    pub config: Arc<FakeConfig>,
    pub env: Arc<FakeEnv>,
    pub secrets: Arc<FakeSecrets>,
    pub profiles: Arc<FakeProfiles>,
    pub prober: Arc<FakeProber>,
    pub sessions: Arc<FakeSessions>,
    pub app: Arc<FakeApp>,
    pub remote: Arc<FakeRemote>,
    pub router: Arc<FakeRouter>,
    pub v3: v3::V3,
    pub common: Vec<String>,
}

impl World {
    /// A Krill PAT profile `work` (overseas, key stored), API-key state applied,
    /// the app running with CDP + bridge and the overseas auth address (hot path).
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = LocalContext::for_tests(tmp.path());
        let home = ctx.codex_home();
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let clock = Arc::new(ManualClock::new(Timestamp(T0)));
        let mut set = ProfileSet::default();
        set.profiles
            .push(krill_profile("work", Some("overseas"), AuthMode::Pat));
        let secrets = FakeSecrets::default();
        secrets
            .map
            .lock()
            .unwrap()
            .insert("profile:work".into(), KEY.into());
        let app = FakeApp {
            log: log.clone(),
            clock: clock.clone(),
            installs: Mutex::new(vec![AppInstall {
                id: "chatgpt".into(),
                variant: AppVariant::ChatGpt,
                path: PathBuf::from("/opt/chatgpt"),
                executable: PathBuf::from("/opt/chatgpt/ChatGPT"),
                app_version: Some("26.1013".into()),
                bundled_codex: Some(PathBuf::from("/opt/chatgpt/resources/codex")),
                bundled_codex_version: Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap()),
            }]),
            binaries: Mutex::new(vec![CodexBinary {
                path: PathBuf::from("/usr/local/bin/codex"),
                version: Some(CodexVersion::new(0, 162, 0)),
                source: InstallSource::Npm,
                path_rank: Some(0),
            }]),
            session: Mutex::new(Some(session(Some(OVERSEAS), true))),
            bridge_ready: Mutex::new(true),
            processes: Mutex::new(vec![
                ProcessInfo {
                    pid: 100,
                    ppid: Some(1),
                    kind: CodexProcessKind::AppMain,
                    exe: None,
                    cmdline: vec!["ChatGPT".into()],
                    started_at: Some(Timestamp(T0 - 3_600_000)),
                    authapi_env: EnvObservation::Present(OVERSEAS.into()),
                    codex_home: None,
                    stale: false,
                    ide: None,
                    cwd: None,
                },
                app_server(150, 100, T0 - 3_000_000),
            ]),
            turns: Mutex::new(VecDeque::new()),
            fail_hot_restart: Mutex::new(0),
            restart_spawns: Mutex::new(true),
            quit_outcomes: Mutex::new(VecDeque::new()),
            quit_reports: Mutex::new(VecDeque::new()),
            end_reports: Mutex::new(VecDeque::new()),
            fail_launch: Mutex::new(None),
            launches: Mutex::new(Vec::new()),
            renderer: Mutex::new(RendererState {
                url: "app://-/index.html#/thread/t1".into(),
                on_login_page: false,
                on_home: false,
                current_thread: Some(ThreadRef {
                    thread_id: "t1".into(),
                    title: Some("Refactor".into()),
                }),
            }),
            renderer_after_restart: Mutex::new(None),
            renderer_states: Mutex::new(VecDeque::new()),
            next_pid: Mutex::new(500),
            hosts: Mutex::new(Vec::new()),
            bundled_dump: Mutex::new(v3::bundled_dump(&["gpt-6-astra", "gpt-6-luna"], true)),
            preflight_error: Mutex::new(None),
            preflights: Mutex::new(Vec::new()),
            refused_catalogs: Mutex::new(Vec::new()),
            refusing_binaries: Mutex::new(Vec::new()),
            app_server_exe: Mutex::new(None),
            stop_answers: Mutex::new(BTreeMap::new()),
            spawn_script: Mutex::new(Vec::new()),
            scheduled: Mutex::new(Vec::new()),
        };
        Self {
            config: Arc::new(FakeConfig::new(log.clone(), home.root())),
            env: Arc::new(FakeEnv {
                log: log.clone(),
                report: Mutex::new(EnvReport {
                    var: superba_core::codex::ENV_AUTHAPI_BASE_URL.into(),
                    persisted: Vec::new(),
                    legacy_lines: Vec::new(),
                    foreign_lines: Vec::new(),
                    process_value: EnvObservation::Unknown,
                    superba_value: None,
                }),
                fail_apply: Mutex::new(None),
                fail_revert: Mutex::new(None),
                plans: Mutex::new(Vec::new()),
                applied: Mutex::new(Vec::new()),
                hang_on_revert: Mutex::new(false),
                revert_started: tokio::sync::Notify::new(),
                launch_edit: Mutex::new(None),
                launch_plans: Mutex::new(Vec::new()),
            }),
            secrets: Arc::new(secrets),
            profiles: Arc::new(FakeProfiles {
                set: Mutex::new(set),
                saves: Mutex::new(0),
            }),
            prober: Arc::new(FakeProber {
                log: log.clone(),
                caps: Mutex::new(BTreeMap::new()),
                probe_error: Mutex::new(None),
                authapi: Mutex::new(BTreeMap::new()),
                models: Mutex::new(Ok(Vec::new())),
                authapi_checks: Mutex::new(Vec::new()),
                model_list: Mutex::new(None),
                model_list_calls: Mutex::new(Vec::new()),
                model_list_versions: Mutex::new(Vec::new()),
            }),
            sessions: Arc::new(FakeSessions {
                log: log.clone(),
                report: Mutex::new(session_report(home.root(), &[("openai", 3)])),
                fail_migrate: Mutex::new(None),
                fail_rollback: Mutex::new(None),
                history: Mutex::new(Vec::new()),
                requests: Mutex::new(Vec::new()),
            }),
            app: Arc::new(app),
            remote: Arc::new(FakeRemote {
                log: log.clone(),
                hosts: Mutex::new(Vec::new()),
                version: Mutex::new(Some(CodexVersion::new(0, 162, 0))),
                shared_home: Mutex::new(false),
                fail_apply: Mutex::new(BTreeMap::new()),
                verify_ok: Mutex::new(BTreeMap::new()),
                busy: Mutex::new(BTreeMap::new()),
                incomplete: Mutex::new(Vec::new()),
                config_toml: Mutex::new(None),
                owned_paths: Mutex::new(Vec::new()),
                scoped: Mutex::new(Vec::new()),
                sessions: Mutex::new(session_report(
                    Path::new("/home/u/.codex"),
                    &[("openai", 2), ("krill", 3)],
                )),
                idle: Mutex::new(false),
                gateway: Mutex::new(None),
                no_codex: Mutex::new(false),
                http: Mutex::new(BTreeMap::new()),
                http_calls: Mutex::new(Vec::new()),
                blocked: Mutex::new(BTreeMap::new()),
                helper_runs: Mutex::new(Vec::new()),
                written: Mutex::new(Vec::new()),
                probes: Mutex::new(Vec::new()),
                running_calls: Mutex::new(0),
                auth: Mutex::new(None),
                openai_base_url: Mutex::new(None),
                mirror: Mutex::new(false),
            }),
            router: Arc::new(FakeRouter::new(log.clone())),
            v3: v3::V3 {
                versions: Arc::new(v3::FakeVersions::new(
                    log.clone(),
                    ctx.data_dir.codex_store_dir(),
                )),
                feeds: Arc::new(v3::FakeFeeds::default()),
                builder: Arc::new(v3::FakeBuilder::default()),
            },
            common: Vec::new(),
            tmp,
            ctx,
            log,
            clock,
        }
    }

    pub fn deps(&self) -> SwitcherDeps {
        SwitcherDeps {
            ctx: self.ctx.clone(),
            config: self.config.clone(),
            env: self.env.clone(),
            secrets: self.secrets.clone(),
            profiles: self.profiles.clone(),
            prober: self.prober.clone(),
            sessions: self.sessions.clone(),
            app: self.app.clone(),
            remote: self.remote.clone(),
            clock: self.clock.clone(),
            router: self.router.clone(),
            catalog: Arc::new(superba_core::StaticCatalog::default()),
            remote_catalog: Arc::new(|_os: &superba_core::RemoteOs| {
                Arc::new(superba_core::StaticCatalog::default())
                    as Arc<dyn superba_core::CatalogSource>
            }),
            explain_zh: Arc::new(|_path: &str| None),
            versions: {
                let v = self.v3.versions.clone();
                Arc::new(move |_n: &superba_core::NetworkSettings| {
                    v.clone() as Arc<dyn superba_core::VersionStore>
                })
            },
            feeds: {
                let f = self.v3.feeds.clone();
                Arc::new(move |_n: &superba_core::NetworkSettings| {
                    f.clone() as Arc<dyn superba_core::FeedSource>
                })
            },
            models: self.v3.builder.clone(),
            common_settings: Arc::new(self.common.clone()),
        }
    }

    pub fn switcher(&self) -> Switcher {
        Switcher::with_tuning(
            self.deps(),
            Tuning {
                poll: Duration::from_millis(10),
                verify_timeout: Duration::from_millis(200),
                launch_verify_timeout: Duration::from_millis(200),
                stable: Duration::ZERO,
                reask_after: Duration::from_secs(60),
                plan_cache: 16,
            },
        )
    }

    pub fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Calls that write (config / env / sessions / remote / app restarts).
    pub fn writes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| {
                c.starts_with("config.apply")
                    || c.starts_with("config.snapshot")
                    || c.starts_with("env.apply")
                    || c.starts_with("sessions.migrate")
                    || c.starts_with("remote.apply")
                    || c.starts_with("app.hot_restart")
                    || c.starts_with("app.quit")
                    || c.starts_with("app.launch")
            })
            .collect()
    }

    pub fn set_session(&self, s: Option<AppSession>) {
        if s.is_none() {
            self.app.processes.lock().unwrap().retain(|p| {
                !matches!(
                    p.kind,
                    CodexProcessKind::AppMain | CodexProcessKind::AppServer
                )
            });
        }
        *self.app.session.lock().unwrap() = s;
    }

    pub fn push_turns(&self, lists: Vec<Vec<ThreadActivity>>) {
        self.app.turns.lock().unwrap().extend(lists);
    }

    /// Where the service looks for AI Toolbox's database in this world's home:
    /// the application data directory of the system the tests run on
    /// ([`superba_import::ScanEnv::app_data_dir`]: `~/.local/share`,
    /// `~/Library/Application Support`, `%APPDATA%`), resolved as the import
    /// scan and the usage import resolve it.
    pub fn ai_toolbox_db(&self) -> PathBuf {
        use superba_import::{Os, ScanEnv, aitoolbox};
        let scratch = self.tmp.path().join("scratch");
        let env = ScanEnv {
            home: &self.ctx.home_dir,
            env: self.ctx.env.as_ref(),
            os: Os::current(),
            scratch: &scratch,
        };
        aitoolbox::default_dir(&env).join(aitoolbox::DB_FILE)
    }
}
