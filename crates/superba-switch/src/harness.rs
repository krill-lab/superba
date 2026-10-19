//! The other coding-agent harnesses through the service (dsh, opencode, pi,
//! ZCode, WorkBuddy): their state, Superba's settings for each, plan / apply
//! / restore of the entries Superba owns in their files, and backups.
//!
//! A harness uses one Superba provider (default: the one Codex uses) and
//! one protocol that the provider offers and the harness speaks (no
//! translation until 0.1.2). Direct mode writes the provider's endpoint and
//! key; routed mode writes the local router's ingress for that protocol and
//! a per-harness router token from the key store.
//!
//! Seams for later work:
//! - router: [`harness_router_route`] is `None` until the router crate hands
//!   out per-harness routes and serves `/openai/v1`; until then routed mode
//!   takes an explicit router URL and token.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use superba_config::harness::{HarnessConfigStore, HarnessWriter};
use superba_core::harness::{
    EndpointSource, HarnessApplyReport, HarnessBackup, HarnessConfig, HarnessId, HarnessInstall,
    HarnessModel, HarnessPlan, HarnessPlanKind, HarnessProtocol, HarnessRoute, HarnessSettings,
    HarnessSummary, HarnessTarget, HarnessView, OpencodeMajor, ProtocolEndpoint, ProviderProtocols,
    WorkBuddyEdition, pick_protocol, provider_endpoints, routed_endpoint,
};
use superba_core::{
    Error, LocalizedText, ModelListRequest, ModelSource, ModelVisibility, Notice, Profile,
    ProfileId, ProfileSet, ResolvedMode, Result, Secret,
};
use url::Url;

use crate::Switcher;
use crate::journal::StateLock;

/// A harness's base URL and token through the local router (the router side
/// is a follow-up: `None` until it serves the harness).
#[derive(Clone, Debug)]
pub struct HarnessRouterRoute {
    /// The router's base URL (`http://127.0.0.1:<port>`).
    pub base_url: Url,
    /// The harness's router token.
    pub token: Secret,
}

/// The local router's route for `harness`, when the router serves it
/// (boundary for the router crate; `None` for now).
pub async fn harness_router_route(
    router: &dyn crate::router::RouterControl,
    harness: HarnessId,
) -> Option<HarnessRouterRoute> {
    let _ = (router, harness);
    None
}

fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(format!("switch.harness.{key}"), fallback)
}

/// The account name of a harness's router token in the key store.
pub fn router_token_account(harness: HarnessId) -> ProfileId {
    ProfileId::new(format!("harness-router-{}", harness.as_str()))
}

// ---------------------------------------------------------------------------
// install detection

/// A found program: its modification time and what it said.
type Found = (Option<std::time::SystemTime>, HarnessInstall);

/// Found programs and their versions, by path and modification time.
static INSTALLS: Mutex<Option<HashMap<PathBuf, Found>>> = Mutex::new(None);

fn on_path(
    env: &dyn superba_core::EnvSource,
    names: &[&str],
    extra: &[PathBuf],
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = env
        .var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(extra.iter().cloned());
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

fn exe_names(base: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{base}.exe"),
            format!("{base}.cmd"),
            base.to_owned(),
        ]
    } else {
        vec![base.to_owned()]
    }
}

/// `<program> --version` (stdin closed: opencode and pi wait for it), first
/// word that looks like a version.
async fn version_of(binary: &Path) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::from(superba_core::process::command(binary))
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())?
        .trim()
        .to_owned();
    Some(line)
}

fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.exists()).cloned()
}

impl Switcher {
    fn harness_store(&self) -> HarnessConfigStore {
        HarnessConfigStore::new(&self.deps.ctx)
    }

    fn harness_writer(&self, harness: HarnessId) -> HarnessWriter {
        HarnessWriter::new(self.deps.ctx.clone(), harness)
    }

    /// The program / app of `harness` and its version.
    pub(crate) async fn harness_install(&self, harness: HarnessId) -> Option<HarnessInstall> {
        let ctx = &self.deps.ctx;
        let env = ctx.env.as_ref();
        let home = &ctx.home_dir;
        let local_app = env.var_os("LOCALAPPDATA").map(PathBuf::from);
        let names = |base: &str| exe_names(base);
        let binary = match harness {
            HarnessId::Dsh => {
                let n = names("dsh");
                let mut extra = Vec::new();
                if let Some(a) = env.var_os("APPDATA") {
                    extra.push(PathBuf::from(a).join("npm"));
                }
                on_path(
                    env,
                    &n.iter().map(String::as_str).collect::<Vec<_>>(),
                    &extra,
                )
            }
            HarnessId::OpenCode => {
                let n = names("opencode");
                on_path(
                    env,
                    &n.iter().map(String::as_str).collect::<Vec<_>>(),
                    &[home.join(".opencode").join("bin")],
                )
            }
            HarnessId::Pi => {
                let n = names("pi");
                on_path(
                    env,
                    &n.iter().map(String::as_str).collect::<Vec<_>>(),
                    &[home.join(".pi").join("agent").join("bin")],
                )
            }
            HarnessId::ZCode => {
                let mut c = vec![
                    PathBuf::from("/Applications/ZCode.app"),
                    home.join("Applications").join("ZCode.app"),
                    PathBuf::from("/opt/ZCode/zcode"),
                ];
                if let Some(l) = &local_app {
                    c.push(l.join("Programs").join("ZCode").join("ZCode.exe"));
                }
                first_existing(&c)
            }
            HarnessId::WorkBuddy => {
                let mut c = vec![
                    PathBuf::from("/Applications/WorkBuddy.app"),
                    PathBuf::from("/Applications/WorkBuddy AI.app"),
                    home.join("Applications").join("WorkBuddy.app"),
                    PathBuf::from("/opt/WorkBuddy"),
                ];
                if let Some(l) = &local_app {
                    c.push(l.join("Programs").join("WorkBuddy").join("WorkBuddy.exe"));
                    c.push(
                        l.join("Programs")
                            .join("WorkBuddyAI")
                            .join("WorkBuddyAI.exe"),
                    );
                }
                first_existing(&c)
            }
        };
        let version_from_file = || -> Option<String> {
            // WorkBuddy records the version it last launched as.
            [WorkBuddyEdition::Cn, WorkBuddyEdition::Intl]
                .into_iter()
                .filter_map(|e| {
                    let p = superba_config::harness::workbuddy::config_dir(ctx, e)
                        .join("last-launch.json");
                    let b = std::fs::read(p).ok()?;
                    let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
                    v.get("version")?.as_str().map(str::to_owned)
                })
                .next()
        };
        let Some(binary) = binary else {
            return match harness {
                HarnessId::WorkBuddy => version_from_file().map(|v| HarnessInstall {
                    binary: None,
                    version: Some(v),
                    opencode_major: None,
                }),
                _ => None,
            };
        };
        let runs = matches!(
            harness,
            HarnessId::Dsh | HarnessId::OpenCode | HarnessId::Pi
        );
        let modified = std::fs::metadata(&binary).and_then(|m| m.modified()).ok();
        if let Some((m, i)) = INSTALLS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|c| c.get(&binary))
            && *m == modified
        {
            return Some(i.clone());
        }
        let raw = if runs {
            version_of(&binary).await
        } else {
            None
        };
        let opencode_major = (harness == HarnessId::OpenCode)
            .then(|| raw.as_deref().and_then(OpencodeMajor::of_version))
            .flatten();
        let version = match harness {
            HarnessId::WorkBuddy => version_from_file(),
            _ => raw.map(|r| {
                r.trim()
                    .trim_start_matches("opencode")
                    .trim()
                    .trim_start_matches('v')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            }),
        };
        let install = HarnessInstall {
            binary: Some(binary.clone()),
            version,
            opencode_major,
        };
        INSTALLS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(binary, (modified, install.clone()));
        Some(install)
    }

    /// The configuration directory exists (the harness ran here).
    fn harness_dir_exists(&self, harness: HarnessId) -> bool {
        let ctx = &self.deps.ctx;
        match harness {
            HarnessId::Dsh => superba_config::harness::dsh::dsh_home(ctx).is_dir(),
            HarnessId::OpenCode => superba_config::harness::opencode::config_dir(ctx).is_dir(),
            HarnessId::Pi => superba_config::harness::pi::agent_dir(ctx).is_dir(),
            HarnessId::ZCode => ctx.home_dir.join(".zcode").is_dir(),
            HarnessId::WorkBuddy => WorkBuddyEdition::ALL
                .iter()
                .any(|e| superba_config::harness::workbuddy::config_dir(ctx, *e).is_dir()),
        }
    }

    /// The provider a harness uses: the configured one (when it still exists
    /// and uses a key), else the provider Codex uses now.
    async fn harness_provider(
        &self,
        set: &ProfileSet,
        settings: &HarnessSettings,
    ) -> Result<Option<Profile>> {
        let usable = |p: &&Profile| p.builtin.is_none() && p.key_ref.is_some();
        if let Some(id) = &settings.provider {
            return set
                .profile(id)
                .filter(usable)
                .cloned()
                .map(Some)
                .ok_or_else(|| Error::NotFound {
                    what: format!("provider {id} (with a key)"),
                });
        }
        let d = &self.deps;
        let active = d
            .config
            .read_state(&d.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active)
            .and_then(|a| a.profile_id);
        Ok(active
            .and_then(|id| set.profile(&id))
            .filter(usable)
            .cloned())
    }

    /// Read the shared model sources, respecting the provider's selection.
    async fn harness_models(&self, provider: &ProfileId) -> Result<Vec<superba_core::ModelSpec>> {
        let set = self.deps.profiles.load().await?;
        let profile = set.profile(provider).ok_or_else(|| Error::NotFound {
            what: format!("provider {provider}"),
        })?;
        let (site, resolved) = self.direct_resolution(&set, profile).await?;
        let ResolvedMode::Token { endpoints, key, .. } = &resolved.mode else {
            return Err(Error::invalid("model sources require an API key"));
        };
        let endpoint = superba_core::claude::effective_endpoint(&site, profile);
        let list = self
            .deps
            .prober
            .model_list(ModelListRequest {
                site: &site,
                base_url: &endpoints.openai_base_url,
                key,
                client_version: None,
                refresh: false,
                source: Some(profile.model_source.unwrap_or(ModelSource::Auto)),
                standard_base: endpoint.as_ref().map(|e| &e.base_url),
            })
            .await?;
        Ok(list
            .models
            .into_iter()
            .filter(|m| m.supported_in_api && !matches!(m.visibility, ModelVisibility::None))
            .collect())
    }

    /// The endpoints `profile` offers (direct).
    async fn harness_endpoints(
        &self,
        set: &ProfileSet,
        profile: &Profile,
        config: &HarnessConfig,
    ) -> Result<(Vec<ProtocolEndpoint>, Secret)> {
        let (site, resolved) = self.direct_resolution(set, profile).await?;
        let ResolvedMode::Token { endpoints, key, .. } = &resolved.mode else {
            return Err(Error::invalid(format!(
                "provider {} has no key-based connection",
                profile.name
            )));
        };
        let list = provider_endpoints(
            &site,
            profile,
            endpoints.origin.url(),
            &endpoints.openai_base_url,
            config.declared(&profile.id),
        );
        Ok((list, key.clone()))
    }

    /// Everything one apply writes into `harness` for its saved settings.
    pub(crate) async fn harness_target(&self, harness: HarnessId) -> Result<HarnessTarget> {
        let config = self.harness_store().load()?;
        let settings = config.settings(harness);
        let set = self.deps.profiles.load().await?;
        let profile = self.harness_provider(&set, &settings).await?.ok_or_else(|| {
            Error::invalid(format!(
                "{} has no provider: pick one with a key (spb harness apply {} --provider <id>)",
                harness.display_name(),
                harness.as_str()
            ))
        })?;
        let (endpoints, key) = self.harness_endpoints(&set, &profile, &config).await?;
        let offered: Vec<HarnessProtocol> = endpoints.iter().map(|e| e.protocol).collect();
        let protocol = pick_protocol(harness, settings.protocol, &offered).ok_or_else(|| {
            let wanted = settings
                .protocol
                .map_or_else(|| "any protocol it speaks".to_owned(), |p| p.label().to_owned());
            Error::invalid(format!(
                "{} cannot use provider {} with {wanted}: the provider offers {}, {} speaks {} (no translation until 0.1.2; declare more endpoints with spb harness endpoints)",
                harness.display_name(),
                profile.name,
                offered.iter().map(|p| p.label()).collect::<Vec<_>>().join(", "),
                harness.display_name(),
                harness.protocols().iter().map(|p| p.label()).collect::<Vec<_>>().join(", "),
            ))
        })?;
        let (endpoint, key) = match settings.route {
            HarnessRoute::Direct => (
                endpoints
                    .into_iter()
                    .find(|e| e.protocol == protocol)
                    .expect("picked from the offered protocols"),
                key,
            ),
            HarnessRoute::Routed => {
                let route = harness_router_route(self.deps.router.as_ref(), harness).await;
                let base = match (&route, &settings.router_url) {
                    (Some(r), _) => r.base_url.clone(),
                    (None, Some(u)) => u.clone(),
                    (None, None) => {
                        return Err(Error::invalid(
                            "routed mode needs the router's URL (spb harness apply --router-url) until the router hands out harness routes",
                        ));
                    }
                };
                let token = match (route, &settings.router_token) {
                    (Some(r), _) => r.token,
                    (None, Some(k)) => self.deps.secrets.get(k).await?,
                    (None, None) => {
                        return Err(Error::invalid(
                            "routed mode needs the harness's router token (spb harness token)",
                        ));
                    }
                };
                let endpoint = routed_endpoint(&base, protocol)
                    .ok_or_else(|| Error::invalid("the router URL cannot take an ingress path"))?;
                (endpoint, token)
            }
        };
        let specs = self.harness_models(&profile.id).await.unwrap_or_default();
        let models: Vec<HarnessModel> = if settings.models.is_empty() {
            specs.iter().map(HarnessModel::from_spec).collect()
        } else {
            settings
                .models
                .iter()
                .map(|id| {
                    specs
                        .iter()
                        .find(|s| s.slug == *id)
                        .map(HarnessModel::from_spec)
                        .unwrap_or_else(|| HarnessModel::id_only(id.clone()))
                })
                .collect()
        };
        let mut options = settings.options.clone();
        if harness == HarnessId::OpenCode && options.opencode_major.is_none() {
            let installed = self
                .harness_install(harness)
                .await
                .and_then(|i| i.opencode_major);
            options.opencode_major = Some(installed.unwrap_or(OpencodeMajor::V2));
        }
        Ok(HarnessTarget {
            harness,
            provider: Some(profile.id.clone()),
            provider_name: profile.name.clone(),
            endpoint,
            key,
            models,
            default_model: settings.default_model.clone(),
            route: settings.route,
            options,
        })
    }

    /// [`superba_core::SwitchService::harness_list`].
    pub(crate) async fn harness_list_impl(&self) -> Result<Vec<HarnessSummary>> {
        let mut out = Vec::new();
        for h in HarnessId::ALL {
            let install = self.harness_install(h).await;
            let writer = self.harness_writer(h);
            let applied = tokio::task::spawn_blocking(move || writer.applied())
                .await
                .map_err(|e| Error::other(format!("harness task: {e}")))??;
            out.push(HarnessSummary {
                harness: h,
                installed: install.is_some() || self.harness_dir_exists(h),
                install,
                applied_provider: applied.as_ref().and_then(|a| a.provider.clone()),
                applied: applied.is_some(),
                drift: applied.as_ref().map_or(0, |a| a.drift.len()),
            });
        }
        Ok(out)
    }

    /// [`superba_core::SwitchService::harness_view`].
    pub(crate) async fn harness_view_impl(&self, harness: HarnessId) -> Result<HarnessView> {
        let config = self.harness_store().load()?;
        let settings = config.settings(harness);
        let install = self.harness_install(harness).await;
        let mut notes = Vec::new();
        let mut protocols = Vec::new();
        let set = self.deps.profiles.load().await?;
        if let Ok(Some(p)) = self.harness_provider(&set, &settings).await
            && let Ok((eps, _)) = self.harness_endpoints(&set, &p, &config).await
        {
            protocols = eps
                .iter()
                .map(|e| e.protocol)
                .filter(|p| harness.speaks(*p))
                .collect();
        }
        let target = self.harness_target(harness).await;
        if let Err(e) = &target {
            notes.push(Notice::warning(
                text("not_ready", "Not ready to apply: {reason}").arg("reason", e.detail()),
            ));
        }
        let options = target
            .as_ref()
            .map(|t| t.options.clone())
            .unwrap_or_else(|_| settings.options.clone());
        let writer = self.harness_writer(harness);
        let t2 = target.as_ref().ok().cloned();
        let (files, applied, pending, diag) = tokio::task::spawn_blocking(move || {
            let files = writer.file_views(&options)?;
            let applied = writer.applied()?;
            let pending = match &t2 {
                Some(t) => writer.pending(t).unwrap_or_default(),
                None => Vec::new(),
            };
            let diag = writer.diagnostics(&options).unwrap_or_default();
            Ok::<_, Error>((files, applied, pending, diag))
        })
        .await
        .map_err(|e| Error::other(format!("harness task: {e}")))??;
        notes.extend(diag);
        Ok(HarnessView {
            harness,
            settings,
            install,
            files,
            protocols,
            endpoint: target.ok().map(|t| t.endpoint),
            applied,
            pending,
            notes,
        })
    }

    /// [`superba_core::SwitchService::save_harness_settings`].
    pub(crate) async fn save_harness_settings_impl(
        &self,
        harness: HarnessId,
        settings: HarnessSettings,
    ) -> Result<HarnessView> {
        if let Some(id) = &settings.provider {
            let set = self.deps.profiles.load().await?;
            if set.profile(id).is_none() {
                return Err(Error::NotFound {
                    what: format!("provider {id}"),
                });
            }
        }
        if let Some(p) = settings.protocol
            && !harness.speaks(p)
        {
            return Err(Error::invalid(format!(
                "{} does not speak {}",
                harness.display_name(),
                p.label()
            )));
        }
        if let Some(u) = &settings.router_url {
            superba_core::harness::check_base_url(u.as_str())
                .map_err(|k| Error::invalid(superba_core::harness::base_url_problem(k).fallback))?;
        }
        let store = self.harness_store();
        let mut config = store.load()?;
        config.harnesses.insert(harness, settings);
        store.save(&config)?;
        self.harness_view_impl(harness).await
    }

    /// [`superba_core::SwitchService::save_provider_protocols`].
    pub(crate) async fn save_provider_protocols_impl(
        &self,
        provider: ProfileId,
        protocols: ProviderProtocols,
    ) -> Result<()> {
        let set = self.deps.profiles.load().await?;
        if set.profile(&provider).is_none() {
            return Err(Error::NotFound {
                what: format!("provider {provider}"),
            });
        }
        for u in protocols.chat.iter().chain(protocols.anthropic.iter()) {
            superba_core::harness::check_base_url(u.as_str())
                .map_err(|k| Error::invalid(superba_core::harness::base_url_problem(k).fallback))?;
        }
        let store = self.harness_store();
        let mut config = store.load()?;
        if protocols.is_empty() {
            config.providers.remove(&provider);
        } else {
            config.providers.insert(provider, protocols);
        }
        store.save(&config)
    }

    /// [`superba_core::SwitchService::set_harness_router_token`].
    pub(crate) async fn set_harness_router_token_impl(
        &self,
        harness: HarnessId,
        token: Option<Secret>,
    ) -> Result<HarnessView> {
        let store = self.harness_store();
        let mut config = store.load()?;
        let mut settings = config.settings(harness);
        match token {
            Some(t) => {
                if t.is_empty() {
                    return Err(Error::invalid("the token is empty"));
                }
                let r = self
                    .deps
                    .secrets
                    .put(&router_token_account(harness), &t)
                    .await?;
                settings.router_token = Some(r);
            }
            None => {
                if let Some(r) = settings.router_token.take() {
                    self.deps.secrets.delete(&r).await?;
                }
            }
        }
        config.harnesses.insert(harness, settings);
        store.save(&config)?;
        self.harness_view_impl(harness).await
    }

    /// [`superba_core::SwitchService::harness_plan`].
    pub(crate) async fn harness_plan_impl(
        &self,
        harness: HarnessId,
        kind: HarnessPlanKind,
    ) -> Result<HarnessPlan> {
        let writer = self.harness_writer(harness);
        let target = match kind {
            HarnessPlanKind::Apply => Some(self.harness_target(harness).await?),
            HarnessPlanKind::Restore => None,
        };
        let source_note = target.as_ref().and_then(|t| {
            (t.endpoint.source == EndpointSource::Router).then(|| {
                Notice::info(
                    text(
                        "routed",
                        "{harness} goes through the local router at {url}.",
                    )
                    .arg("harness", harness.display_name())
                    .arg("url", t.endpoint.base_text()),
                )
            })
        });
        let shape_note = target.as_ref().and_then(|t| {
            (harness == HarnessId::OpenCode).then(|| {
                let major = t.options.opencode_major.unwrap_or(OpencodeMajor::V2);
                Notice::info(
                    text("opencode_shape", "Written in the OpenCode {major} shape.")
                        .arg("major", major.as_str()),
                )
            })
        });
        let mut plan = tokio::task::spawn_blocking(move || writer.plan(kind, target))
            .await
            .map_err(|e| Error::other(format!("plan task: {e}")))??;
        plan.notes.extend(source_note);
        plan.notes.extend(shape_note);
        Ok(plan)
    }

    /// [`superba_core::SwitchService::harness_apply`].
    pub(crate) async fn harness_apply_impl(
        &self,
        plan: HarnessPlan,
        take_over: bool,
    ) -> Result<HarnessApplyReport> {
        self.refuse_elevated()?;
        let _lock = StateLock::acquire(&self.deps.ctx.data_dir)?;
        let writer = self.harness_writer(plan.harness);
        let now = self.deps.clock.now();
        tokio::task::spawn_blocking(move || writer.apply(&plan, take_over, now))
            .await
            .map_err(|e| Error::other(format!("apply task: {e}")))?
    }

    /// [`superba_core::SwitchService::harness_backups`].
    pub(crate) async fn harness_backups_impl(
        &self,
        harness: HarnessId,
    ) -> Result<Vec<HarnessBackup>> {
        let writer = self.harness_writer(harness);
        tokio::task::spawn_blocking(move || writer.backups())
            .await
            .map_err(|e| Error::other(format!("harness task: {e}")))?
    }

    /// [`superba_core::SwitchService::harness_restore_backup`].
    pub(crate) async fn harness_restore_backup_impl(
        &self,
        harness: HarnessId,
        id: String,
    ) -> Result<HarnessApplyReport> {
        self.refuse_elevated()?;
        let _lock = StateLock::acquire(&self.deps.ctx.data_dir)?;
        let writer = self.harness_writer(harness);
        let now = self.deps.clock.now();
        tokio::task::spawn_blocking(move || writer.restore_backup(&id, now))
            .await
            .map_err(|e| Error::other(format!("harness task: {e}")))?
    }
}
