//! Claude Code through the service (user bug 79): its page state, the
//! provider's models, apply / restore of the keys Superba owns in
//! `settings.json`, diagnostics and the endpoint check.
//!
//! Claude Code uses its own provider (default: the one Codex uses). Only a
//! provider with an Anthropic Messages endpoint can be used (its own, or a
//! verified preset): protocol translation comes later. Model lists merge the
//! provider's Codex list with the standard list of its Anthropic endpoint;
//! ids only in the Codex list cannot be used unless the endpoint serves the
//! Codex list too (Krill).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use superba_config::claude::{ClaudeConfigStore, ClaudeWriter, SettingsFile, json as cjson};
use superba_core::claude::{
    self, ENV_API_KEY, ENV_AUTH_TOKEN, ENV_BASE_URL, ENV_MODEL, anthropic_preset,
    effective_endpoint, env_name, managed_paths, suggested_capabilities, suggested_preset,
};
use superba_core::{
    AnthropicEndpoint, ClaudeApplyReport, ClaudeConfig, ClaudeDiagnostics, ClaudeEnvConflict,
    ClaudeFileStatus, ClaudeInstall, ClaudeModelRow, ClaudeModelsRequest, ClaudeModelsView,
    ClaudePlan, ClaudePlanKind, ClaudeProbe, ClaudeRoute, ClaudeView, DoctorFinding, Error,
    LocalizedText, MessagesProbeRequest, ModelList, ModelListKind, ModelListRequest, ModelSource,
    ModelSpec, Notice, Profile, ProfileId, ProfileSet, ResolvedMode, Result, Secret, Severity,
    Site,
};
use url::Url;

use crate::Switcher;
use crate::journal::StateLock;
use crate::messages::text;
use crate::planner::all_sites;

/// What the Claude Code views remember between calls.
#[derive(Debug, Default)]
pub(crate) struct ClaudeCache {
    /// The last model list read per provider (capability suggestions).
    lists: Mutex<HashMap<ProfileId, ModelList>>,
    /// `claude` found and its version, by path and modification time.
    install: Mutex<Option<(PathBuf, Option<std::time::SystemTime>, ClaudeInstall)>>,
}

/// Claude Code's base URL and key through the local router (design §6).
#[derive(Clone, Debug)]
pub struct ClaudeRouterRoute {
    /// `http://127.0.0.1:<port>/claude` (Claude Code adds `/v1/messages`).
    pub base_url: Url,
    /// Claude Code's own router token (derived from the router token, so the
    /// router records its requests as Claude Code's), sent as
    /// `ANTHROPIC_AUTH_TOKEN`.
    pub token: Secret,
}

/// The local router's Anthropic ingress for Claude Code: its base URL and
/// Claude Code's harness token. `None` while the router is not set up (no
/// configuration and token yet: install it on the Router page first).
pub async fn claude_router_route(
    router: &dyn crate::router::RouterControl,
) -> Option<ClaudeRouterRoute> {
    let e = router.endpoint().ok()?;
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, e.port));
    let base_url = Url::parse(&superba_router::claude::base_url(addr)).ok()?;
    let token = superba_router::token::harness_token(
        e.token.expose().as_bytes(),
        superba_core::RequestHarness::ClaudeCode,
        None,
    )?;
    Some(ClaudeRouterRoute {
        base_url,
        token: Secret::new(token),
    })
}

/// The values for `endpoint` / `key` / `models`, plus what the route needs:
/// through the router, Claude Code sends its prompt id, request class and
/// agent type (`CLAUDE_CODE_GATEWAY_HINT_HEADERS`), which the router records.
pub(crate) fn desired_for(
    config: &ClaudeConfig,
    endpoint: &AnthropicEndpoint,
    key: &Secret,
    models: &claude::ClaudeModels,
    spec: &dyn Fn(&str) -> Option<ModelSpec>,
) -> claude::ClaudeDesired {
    let mut d = claude::desired(endpoint, key, models, spec);
    if config.route == ClaudeRoute::Router {
        d.set(
            claude::env_path(claude::ENV_GATEWAY_HINT_HEADERS),
            Some(serde_json::Value::String("1".into())),
        );
    }
    d
}

/// The provider Claude Code uses: the configured one (when it still exists
/// and uses a key), else the provider Codex uses now.
pub(crate) fn pick_provider<'a>(
    set: &'a ProfileSet,
    config: &ClaudeConfig,
    codex_active: Option<&ProfileId>,
) -> (Option<&'a Profile>, bool) {
    let usable = |p: &&Profile| p.builtin.is_none() && p.key_ref.is_some();
    if let Some(id) = &config.provider
        && let Some(p) = set.profile(id).filter(usable)
    {
        return (Some(p), false);
    }
    (
        codex_active.and_then(|id| set.profile(id)).filter(usable),
        true,
    )
}

/// The preset's endpoint serves every id of the provider's Codex list.
fn serves_codex_list(endpoint: &AnthropicEndpoint) -> bool {
    endpoint
        .preset
        .as_deref()
        .and_then(anthropic_preset)
        .is_some_and(|p| p.serves_codex_list)
}

/// Rows of a merged list for Claude Code, then the ids added by hand.
pub(crate) fn rows_for(
    list: &[ModelSpec],
    extra: &[String],
    all_usable: bool,
) -> Vec<ClaudeModelRow> {
    let mut rows: Vec<ClaudeModelRow> = list
        .iter()
        .map(|m| {
            let only_codex = m.lists == [ModelListKind::Codex];
            ClaudeModelRow {
                id: m.slug.clone(),
                display_name: m.display_name.clone(),
                lists: m.lists.clone(),
                manual: false,
                usable: all_usable || !only_codex,
                context_window: m.context_window,
                reasoning_levels: m
                    .reasoning_levels
                    .iter()
                    .map(|l| l.effort.clone())
                    .collect(),
                suggested: suggested_capabilities(&m.slug, Some(m)),
            }
        })
        .collect();
    for id in extra {
        let id = id.trim();
        if id.is_empty() || rows.iter().any(|r| r.id == id) {
            continue;
        }
        rows.push(ClaudeModelRow {
            id: id.to_owned(),
            display_name: None,
            lists: Vec::new(),
            manual: true,
            usable: true,
            context_window: None,
            reasoning_levels: Vec::new(),
            suggested: suggested_capabilities(id, None),
        });
    }
    rows
}

/// Shell files that may export variables Claude Code reads.
const SHELL_FILES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".config/fish/config.fish",
];

/// Variables that make Claude Code use another provider altogether.
const PROVIDER_SWITCHES: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Names a shell file exports (`export NAME=`, `set -x NAME`, `setenv NAME`).
pub(crate) fn exported_names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        let rest = l
            .strip_prefix("export ")
            .or_else(|| l.strip_prefix("set -gx "))
            .or_else(|| l.strip_prefix("set -x "))
            .or_else(|| l.strip_prefix("setenv "));
        let Some(rest) = rest else { continue };
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.insert(name);
        }
    }
    out
}

fn finding(
    id: &str,
    severity: Severity,
    title: LocalizedText,
    detail: Option<LocalizedText>,
) -> DoctorFinding {
    DoctorFinding {
        id: id.to_owned(),
        severity,
        title,
        detail,
        fixable: false,
    }
}

/// The `claude` program on `PATH` or in the usual install places.
fn find_claude(env: &dyn superba_core::EnvSource, home: &Path) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude"]
    } else {
        &["claude"]
    };
    let mut dirs: Vec<PathBuf> = env
        .var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.push(home.join(".local").join("bin"));
    dirs.push(home.join(".claude").join("local"));
    if let Some(appdata) = env.var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

impl Switcher {
    pub(crate) fn claude_writer(&self) -> ClaudeWriter {
        ClaudeWriter::new(self.deps.ctx.clone())
    }

    pub(crate) fn claude_store(&self) -> ClaudeConfigStore {
        ClaudeConfigStore::new(&self.deps.ctx)
    }

    /// `claude` and its version (cached while the file is unchanged).
    async fn claude_install(&self) -> Option<ClaudeInstall> {
        let d = &self.deps;
        let binary = find_claude(d.ctx.env.as_ref(), &d.ctx.home_dir)?;
        let modified = std::fs::metadata(&binary).and_then(|m| m.modified()).ok();
        if let Some((p, m, i)) = self
            .claude
            .install
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && *p == binary
            && *m == modified
        {
            return Some(i.clone());
        }
        let version = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            tokio::process::Command::from(superba_core::process::command(&binary))
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .map(str::to_owned)
        });
        let install = ClaudeInstall {
            binary: binary.clone(),
            version,
        };
        *self
            .claude
            .install
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some((binary, modified, install.clone()));
        Some(install)
    }

    /// The profile set, the configuration and the provider Claude Code uses
    /// (`want` overrides it) with its site.
    pub(crate) async fn claude_context(
        &self,
        want: Option<&ProfileId>,
    ) -> Result<(ProfileSet, ClaudeConfig, Option<(Profile, Site)>, bool)> {
        let d = &self.deps;
        let set = d.profiles.load().await?;
        let config = self.claude_store().load()?;
        let active = d
            .config
            .read_state(&d.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active)
            .and_then(|a| a.profile_id);
        let (profile, follows) = match want {
            Some(id) => (
                Some(set.profile(id).ok_or_else(|| Error::NotFound {
                    what: format!("provider {id}"),
                })?),
                false,
            ),
            None => pick_provider(&set, &config, active.as_ref()),
        };
        let sites = all_sites(&set, d.ctx.env.as_ref());
        let pair = profile.and_then(|p| {
            sites
                .iter()
                .find(|s| s.id == p.site_id)
                .map(|s| (p.clone(), s.clone()))
        });
        Ok((set, config, pair, follows))
    }

    /// The endpoint and key Claude Code is written with for `profile`: the
    /// router's when the route is the router, else the provider's.
    async fn claude_target(
        &self,
        config: &ClaudeConfig,
        profile: &Profile,
        site: &Site,
    ) -> Result<(AnthropicEndpoint, Secret)> {
        if config.route == ClaudeRoute::Router {
            let route = claude_router_route(self.deps.router.as_ref())
                .await
                .ok_or_else(|| Error::Unsupported {
                    what: "Claude Code through the local router (not available yet)".into(),
                })?;
            return Ok((AnthropicEndpoint::new(route.base_url), route.token));
        }
        let endpoint = effective_endpoint(site, profile).ok_or_else(|| {
            Error::invalid(format!(
                "provider {} has no Anthropic endpoint: direct connections require an Anthropic endpoint; use the local router for protocol conversion",
                profile.name
            ))
        })?;
        let key_ref = profile.key_ref.as_ref().ok_or_else(|| Error::NotFound {
            what: format!("the key of provider {}", profile.name),
        })?;
        let key = self.deps.secrets.get(key_ref).await?;
        Ok((endpoint, key))
    }

    /// [`superba_core::SwitchService::claude_view`].
    pub(crate) async fn claude_view_impl(&self) -> Result<ClaudeView> {
        let writer = self.claude_writer();
        let home = writer.home();
        let file = writer.read()?;
        let (_, config, pair, follows) = self.claude_context(None).await?;
        let applied = writer.applied(&file)?;
        let mut notes = Vec::new();
        let (provider, endpoint, suggested, key_available) = match &pair {
            Some((p, s)) => {
                let suggested = p.anthropic.is_none() && suggested_preset(s, p).is_some();
                (
                    Some(p.id.clone()),
                    effective_endpoint(s, p),
                    suggested,
                    p.key_ref.is_some(),
                )
            }
            None => (None, None, false, false),
        };
        // Owned keys whose value differs from what the configuration wants.
        let mut pending = Vec::new();
        if let (Some((p, s)), Some(_)) = (&pair, &file.value)
            && let Ok((e, key)) = self.claude_target(&config, p, s).await
        {
            let models = config.models_of(&p.id);
            let spec = self.claude_spec_lookup(&p.id);
            let desired = desired_for(&config, &e, &key, &models, &spec);
            pending = writer
                .plan(ClaudePlanKind::Apply, Some(desired), Some(p.id.clone()))?
                .changes
                .into_iter()
                .map(|c| c.slot)
                .collect();
        }
        if !file.status.writable() {
            notes.push(Notice::warning(text("switch.claude.file_invalid")));
        }
        let router_base_url = claude_router_route(self.deps.router.as_ref())
            .await
            .map(|r| r.base_url);
        if config.route == ClaudeRoute::Router && router_base_url.is_none() {
            notes.push(Notice::warning(text("switch.claude.router.not_set_up")));
        }
        Ok(ClaudeView {
            config_dir: home.root().to_path_buf(),
            config_dir_from_env: home.from_env(),
            settings_path: file.path.clone(),
            file: file.status.clone(),
            install: self.claude_install().await,
            config,
            provider,
            follows_codex: follows,
            endpoint,
            endpoint_suggested: suggested,
            key_available,
            applied,
            pending,
            router_base_url,
            notes,
        })
    }

    /// A model's metadata from the last list read for `provider`.
    fn claude_spec_lookup(
        &self,
        provider: &ProfileId,
    ) -> impl Fn(&str) -> Option<ModelSpec> + use<> {
        let list = self
            .claude
            .lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(provider)
            .cloned();
        move |model: &str| list.as_ref().and_then(|l| l.model(model).cloned())
    }

    /// [`superba_core::SwitchService::save_claude_config`].
    pub(crate) async fn save_claude_config_impl(&self, config: ClaudeConfig) -> Result<ClaudeView> {
        if let Some(id) = &config.provider {
            let set = self.deps.profiles.load().await?;
            if set.profile(id).is_none() {
                return Err(Error::NotFound {
                    what: format!("provider {id}"),
                });
            }
        }
        self.claude_store().save(&config)?;
        // The router's Claude Code section follows the provider, its models
        // and the route (a running router reloads; nothing is started).
        if let Err(e) = self.sync_claude_router(false).await {
            tracing::warn!(error = %e, "the local router's Claude Code section was not updated");
        }
        self.claude_view_impl().await
    }

    /// [`superba_core::SwitchService::claude_models`].
    pub(crate) async fn claude_models_impl(
        &self,
        req: ClaudeModelsRequest,
    ) -> Result<ClaudeModelsView> {
        let (set, config, pair, _) = self.claude_context(req.provider.as_ref()).await?;
        let (profile, site) = pair.ok_or_else(|| {
            Error::invalid("Claude Code has no provider: pick one with a key first")
        })?;
        let models = config.models_of(&profile.id);
        let endpoint = effective_endpoint(&site, &profile);
        let (_, resolved) = self.direct_resolution(&set, &profile).await?;
        let ResolvedMode::Token { endpoints, key, .. } = &resolved.mode else {
            return Err(Error::invalid(format!(
                "provider {} has no key-based connection",
                profile.name
            )));
        };
        let list = self
            .deps
            .prober
            .model_list(ModelListRequest {
                site: &site,
                base_url: &endpoints.openai_base_url,
                key,
                client_version: None,
                refresh: req.refresh,
                source: Some(profile.model_source.unwrap_or(ModelSource::Auto)),
                standard_base: endpoint.as_ref().map(|e| &e.base_url),
            })
            .await?;
        self.claude
            .lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(profile.id.clone(), list.clone());
        let all_usable =
            config.route == ClaudeRoute::Router || endpoint.as_ref().is_some_and(serves_codex_list);
        Ok(ClaudeModelsView {
            provider: profile.id.clone(),
            rows: rows_for(&list.models, &models.extra_models, all_usable),
            sources: list.sources,
            at: self.deps.clock.now(),
        })
    }

    /// [`superba_core::SwitchService::claude_plan`].
    pub(crate) async fn claude_plan_impl(&self, kind: ClaudePlanKind) -> Result<ClaudePlan> {
        let writer = self.claude_writer();
        if kind == ClaudePlanKind::Restore {
            return tokio::task::spawn_blocking(move || {
                writer.plan(ClaudePlanKind::Restore, None, None)
            })
            .await
            .map_err(|e| Error::other(format!("plan task: {e}")))?;
        }
        let (_, config, pair, _) = self.claude_context(None).await?;
        let (profile, site) = pair.ok_or_else(|| {
            Error::invalid("Claude Code has no provider: pick one with a key first")
        })?;
        let (endpoint, key) = self.claude_target(&config, &profile, &site).await?;
        let models = config.models_of(&profile.id);
        let spec = self.claude_spec_lookup(&profile.id);
        let desired = desired_for(&config, &endpoint, &key, &models, &spec);
        let provider = Some(profile.id.clone());
        let mut plan = tokio::task::spawn_blocking(move || {
            writer.plan(ClaudePlanKind::Apply, Some(desired), provider)
        })
        .await
        .map_err(|e| Error::other(format!("plan task: {e}")))??;
        let diag = self.claude_diagnostics_impl().await?;
        for f in diag
            .findings
            .iter()
            .filter(|f| f.severity != Severity::Info && f.id != "claude_not_found")
        {
            plan.notes.push(Notice {
                severity: f.severity,
                text: f.title.clone(),
            });
        }
        Ok(plan)
    }

    /// [`superba_core::SwitchService::claude_apply`].
    pub(crate) async fn claude_apply_impl(
        &self,
        plan: ClaudePlan,
        take_over: bool,
    ) -> Result<ClaudeApplyReport> {
        self.refuse_elevated()?;
        let d = &self.deps;
        // Through the router: its Claude Code section first, and the router
        // running, before settings.json points Claude Code at it; direct: the
        // section goes (Claude-only members leave the router).
        if plan.kind == ClaudePlanKind::Apply {
            let router = plan
                .desired
                .as_ref()
                .and_then(|d| d.claude.as_ref())
                .and_then(|d| d.get(&claude::env_path(ENV_BASE_URL)))
                .and_then(serde_json::Value::as_str)
                .is_some_and(crate::claude_router::is_router_url);
            if router {
                self.sync_claude_router(true).await?;
            } else if let Err(e) = self.sync_claude_router(false).await {
                // Going direct never waits for the router.
                tracing::warn!(error = %e, "the local router's Claude Code section was not removed");
            }
        }
        let _lock = StateLock::acquire(&d.ctx.data_dir)?;
        let writer = self.claude_writer();
        let now = d.clock.now();
        tokio::task::spawn_blocking(move || writer.apply(&plan, take_over, now))
            .await
            .map_err(|e| Error::other(format!("apply task: {e}")))?
    }

    /// [`superba_core::SwitchService::claude_diagnostics`].
    pub(crate) async fn claude_diagnostics_impl(&self) -> Result<ClaudeDiagnostics> {
        let d = &self.deps;
        let writer = self.claude_writer();
        let home = writer.home();
        let file = writer.read()?;
        let install = self.claude_install().await;
        let mut findings = Vec::new();
        match &install {
            Some(i) => findings.push(finding(
                "claude_found",
                Severity::Info,
                text("switch.claude.diag.found")
                    .arg("path", superba_core::paths::display_path(&i.binary))
                    .arg("version", i.version.as_deref().unwrap_or("?")),
                None,
            )),
            None => findings.push(finding(
                "claude_not_found",
                Severity::Warning,
                text("switch.claude.diag.not_found"),
                Some(text("switch.claude.diag.not_found_detail")),
            )),
        }
        match &file.status {
            ClaudeFileStatus::Absent => findings.push(finding(
                "claude_settings_absent",
                Severity::Info,
                text("switch.claude.diag.settings_absent")
                    .arg("path", superba_core::paths::display_path(&file.path)),
                None,
            )),
            ClaudeFileStatus::Valid => findings.push(finding(
                "claude_settings_ok",
                Severity::Info,
                text("switch.claude.diag.settings_ok")
                    .arg("path", superba_core::paths::display_path(&file.path)),
                None,
            )),
            ClaudeFileStatus::Invalid { line, column, .. } => findings.push(finding(
                "claude_settings_invalid",
                Severity::Danger,
                text("switch.claude.diag.settings_invalid")
                    .arg("path", superba_core::paths::display_path(&file.path))
                    .arg("line", line)
                    .arg("column", column),
                None,
            )),
            ClaudeFileStatus::NotObject | ClaudeFileStatus::Unreadable { .. } => {
                findings.push(finding(
                    "claude_settings_invalid",
                    Severity::Danger,
                    text("switch.claude.diag.settings_unreadable")
                        .arg("path", superba_core::paths::display_path(&file.path)),
                    None,
                ))
            }
        }

        // Managed settings override user settings.
        let owned = managed_paths();
        let mut managed = Vec::new();
        let dir = claude::managed_settings_dir();
        let mut files = vec![dir.join(claude::MANAGED_SETTINGS_FILE)];
        if let Ok(rd) = std::fs::read_dir(dir.join("managed-settings.d")) {
            let mut extra: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            extra.sort();
            files.extend(extra);
        }
        let mut helper = file
            .value
            .as_ref()
            .is_some_and(|v| v.get("apiKeyHelper").is_some());
        for path in files {
            let Ok(Some(bytes)) = superba_config_read(&path) else {
                continue;
            };
            let (value, _) = cjson::parse(Some(&bytes));
            let Some(v) = value else { continue };
            helper |= v.get("apiKeyHelper").is_some();
            let keys: Vec<String> = owned
                .iter()
                .filter(|p| claude::get_path(&v, p).is_some())
                .cloned()
                .collect();
            if !keys.is_empty() {
                findings.push(finding(
                    "claude_managed_override",
                    Severity::Warning,
                    text("switch.claude.diag.managed")
                        .arg("path", superba_core::paths::display_path(&path))
                        .arg("keys", keys.join(", ")),
                    None,
                ));
            }
            managed.push((path, keys));
        }
        if helper {
            findings.push(finding(
                "claude_api_key_helper",
                Severity::Warning,
                text("switch.claude.diag.api_key_helper"),
                None,
            ));
        }

        // Variables outside settings.json.
        let mut conflicts = Vec::new();
        let names: Vec<String> = owned
            .iter()
            .filter_map(|p| env_name(p).map(str::to_owned))
            .chain(PROVIDER_SWITCHES.iter().map(|s| (*s).to_owned()))
            .collect();
        let in_settings = |n: &str| {
            file.value
                .as_ref()
                .and_then(|v| v.get(claude::KEY_ENV))
                .and_then(|e| e.get(n))
                .is_some()
        };
        for n in &names {
            if d.ctx.env.var_non_empty(n).is_some() && !in_settings(n) {
                conflicts.push(ClaudeEnvConflict {
                    name: n.clone(),
                    source: "process".into(),
                });
            }
        }
        for rel in SHELL_FILES {
            let path = d.ctx.home_dir.join(rel);
            let Ok(t) = std::fs::read_to_string(&path) else {
                continue;
            };
            let exported = exported_names(&t);
            for n in &names {
                if exported.contains(n) && !in_settings(n) {
                    conflicts.push(ClaudeEnvConflict {
                        name: n.clone(),
                        source: superba_core::paths::display_path(&path),
                    });
                }
            }
        }
        if !conflicts.is_empty() {
            let list = conflicts
                .iter()
                .map(|c| format!("{} ({})", c.name, c.source))
                .collect::<Vec<_>>()
                .join(", ");
            findings.push(finding(
                "claude_env_conflict",
                Severity::Warning,
                text("switch.claude.diag.env_conflict"),
                Some(text("switch.claude.diag.env_conflict_detail").arg("vars", list)),
            ));
        }

        // A claude.ai login next to Superba's key.
        let official = home.credentials_json().is_file()
            || superba_config_read(&home.global_state_json())
                .ok()
                .flatten()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_some_and(|v| v.get("oauthAccount").is_some_and(|a| !a.is_null()));
        let ours = file.value.as_ref().is_some_and(|v| {
            claude::get_path(v, &claude::env_path(ENV_AUTH_TOKEN)).is_some()
                || claude::get_path(v, &claude::env_path(ENV_API_KEY)).is_some()
        });
        if official {
            findings.push(finding(
                "claude_official_login",
                Severity::Info,
                if ours {
                    text("switch.claude.diag.official_ours")
                } else {
                    text("switch.claude.diag.official")
                },
                None,
            ));
        }
        if let Some(v) = &file.value
            && claude::get_path(v, &claude::env_path(ENV_BASE_URL)).is_some()
            && claude::get_path(v, &claude::env_path(ENV_MODEL)).is_some()
        {
            findings.push(finding(
                "claude_model_env",
                Severity::Info,
                text("switch.claude.diag.model_env"),
                None,
            ));
        }
        Ok(ClaudeDiagnostics {
            install,
            settings_path: file.path.clone(),
            file: file.status.clone(),
            managed,
            env_conflicts: conflicts,
            official_login: official,
            api_key_helper: helper,
            findings,
        })
    }

    /// [`superba_core::SwitchService::claude_probe`].
    pub(crate) async fn claude_probe_impl(&self, model: Option<String>) -> Result<ClaudeProbe> {
        let (_, config, pair, _) = self.claude_context(None).await?;
        let (profile, site) = pair.ok_or_else(|| {
            Error::invalid("Claude Code has no provider: pick one with a key first")
        })?;
        let (endpoint, key) = self.claude_target(&config, &profile, &site).await?;
        let models = config.models_of(&profile.id);
        let model = model
            .filter(|m| !m.trim().is_empty())
            .or(models.main.clone())
            .or_else(|| models.slots.values().next().map(|s| s.model.clone()))
            .ok_or_else(|| Error::invalid("choose a model to check the endpoint with"))?;
        self.deps
            .prober
            .probe_messages(MessagesProbeRequest {
                endpoint: &endpoint,
                key: &key,
                model: &model,
            })
            .await
    }

    /// `settings.json` as the file editor shows it (local only).
    pub(crate) fn claude_settings_file(&self) -> Result<SettingsFile> {
        self.claude_writer().read()
    }

    /// The keys Superba owns in `settings.json` (values only when they hold
    /// no key) and the provider it applied.
    pub(crate) fn claude_owned(&self) -> (Vec<superba_core::OwnedKey>, Option<ProfileId>) {
        let Ok(Some(state)) = self.claude_writer().state() else {
            return (Vec::new(), None);
        };
        let owned = state
            .files
            .get("settings.json")
            .into_iter()
            .flat_map(|f| &f.slots)
            .map(|(p, slot)| superba_core::OwnedKey {
                path: p.clone(),
                written: if claude::is_secret_path(p) {
                    None
                } else {
                    slot.written.as_ref().map(|v| v.to_string())
                },
            })
            .collect();
        (owned, state.provider)
    }
}

fn superba_config_read(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
