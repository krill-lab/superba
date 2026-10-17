//! Planning (plan 5.2): gather facts through the component traits, resolve the
//! destination, and assemble a [`SwitchPlan`] whose local restart decision comes
//! from the pure table in [`crate::rules`]. Planning never writes.
//!
//! Probes that are not available yet (`Error::NotImplemented` from a component
//! still being written) degrade to a warning; only the configuration engine is
//! mandatory, because nothing can be written without it.

use std::collections::BTreeMap;

use superba_core::{
    AppSession, ApplyPlan, AuthApiHealth, AuthMode, BaselineExtras, BlockReason, BuiltinProfile,
    CdpMode, CodexFileKind, CodexFiles, CodexHome, CodexPath, ConfigState, EnvObservation, EnvOp,
    EnvPlan, EnvSnapshot, Error, FileChange, Line, LineCaps, LocalizedText, MigrationPlan,
    MigrationRequest, MigrationSelector, ModelCheck, Notice, Origin, PlanAlternative, PlanChoice,
    PlanDestination, ProcessInfo, Profile, ProfileId, ProfileSet, ProviderRestore, RemoteHost,
    RemotePlan, ResolvedEndpoints, ResolvedMode, ResolvedProfile, RestartKind, RestoreTarget,
    Result, Secret, SessionReport, Site, SiteId, SiteKind, StashScope, SwitchIntent, SwitchPlan,
    SwitchRequest, TargetId, ThreadActivity,
    catalog::{self, KRILL_SITE_ID, OFFICIAL_SITE_ID, PROFILE_OFFICIAL, PROFILE_ORIGINAL},
    codex::{ENV_AUTHAPI_BASE_URL, LEGACY_KRILL_PROVIDER, PROVIDER_OPENAI, VersionCapability},
};

use superba_config::settings::{HomeInput, PreviewTarget, plan_settings, preview_changes};
use superba_core::{
    AuthChoice, AuthContext, ConsumerKind, PlanSettings, SettingsConsumer, SettingsPlan,
};

use crate::Switcher;
use crate::journal::SwitchJournal;
use crate::messages::text;
use crate::rules::{
    self, AppFacts, ConsumerRole, LocalFacts, VersionedConsumer, decide_local, decide_remote,
};

/// Age after which a stashed ChatGPT login probably needs a new sign-in (plan 9.4).
pub const LOGIN_STASH_WARN_DAYS: i64 = 14;

/// Label of the desktop app in version messages.
pub const APP_CONSUMER_LABEL: &str = "ChatGPT app";

/// What `execute` needs besides the serializable plan.
#[derive(Clone, Debug)]
pub(crate) struct PlanContext {
    pub home: CodexHome,
    pub site: Option<Site>,
    pub auth: Option<AuthMode>,
    pub key: Option<Secret>,
    pub endpoints: Option<ResolvedEndpoints>,
    pub session: Option<AppSession>,
    pub hosts: Vec<RemoteHost>,
    pub launch_authapi: Option<Origin>,
    pub baseline_missing: bool,
    pub baseline_released: bool,
    pub baseline_extras: BaselineExtras,
    pub profile_update: Option<ProfileUpdate>,
    pub install_id: Option<String>,
    pub safe_mode: bool,
    pub restart_app: bool,
    pub rollback_migration: Option<String>,
    pub rollback_remote: Option<TargetId>,
    pub uninstall: Option<bool>,
    /// The settings part of the plan (settings plans per home, diff preview).
    pub settings: superba_core::PlanSettings,
    /// Router mode: new member order and configuration, applied before writing.
    pub router_update: Option<(Vec<ProfileId>, crate::router::BuiltMembers)>,
    /// Router intent: router mode to store after a successful switch.
    pub router_enabled_after: Option<bool>,
    /// Settings sync: the record stored for the host after it succeeded.
    pub host_sync: Option<(TargetId, superba_core::HostSync)>,
    /// App Codex pin: what to launch the app with and store afterwards.
    pub pin: Option<crate::versions::PinPlan>,
    /// Model catalog file written before `config.toml` names it.
    pub catalog: Option<crate::models::CatalogWrite>,
    /// The local home's catalog record after a successful switch (`Some(None)`
    /// = no catalog any more; `None` = untouched).
    pub catalog_record: Option<Option<crate::models::CatalogRecord>>,
    /// A pin change's rewrite of the local `config.toml` for the new Codex (a
    /// rebuilt catalog file, or the key left out), decided at execution.
    pub pin_config: Option<ApplyPlan>,
}

impl PlanContext {
    /// A context that carries nothing but the home (remote-only and app plans).
    pub(crate) fn bare(home: CodexHome, safe_mode: bool) -> Self {
        Self {
            home,
            site: None,
            auth: None,
            key: None,
            endpoints: None,
            session: None,
            hosts: Vec::new(),
            launch_authapi: None,
            baseline_missing: false,
            baseline_released: false,
            baseline_extras: BaselineExtras::default(),
            profile_update: None,
            install_id: None,
            safe_mode,
            restart_app: false,
            rollback_migration: None,
            rollback_remote: None,
            uninstall: None,
            settings: PlanSettings::default(),
            router_update: None,
            router_enabled_after: None,
            host_sync: None,
            pin: None,
            catalog: None,
            catalog_record: None,
            pin_config: None,
        }
    }
}

/// Overrides to store into the profile after a successful switch.
#[derive(Clone, Debug)]
pub(crate) struct ProfileUpdate {
    pub profile_id: ProfileId,
    pub line_id: Option<superba_core::LineId>,
    pub auth_mode: Option<AuthMode>,
}

/// Accumulates findings while planning.
#[derive(Debug, Default)]
struct Draft {
    blocks: Vec<BlockReason>,
    choices: Vec<PlanChoice>,
    warnings: Vec<Notice>,
    alternatives: Vec<PlanAlternative>,
    cleanup: Vec<LocalizedText>,
}

impl Draft {
    fn block(&mut self, reason: BlockReason) {
        if !self.blocks.contains(&reason) {
            self.blocks.push(reason);
        }
    }

    fn warn(&mut self, t: LocalizedText) {
        self.warnings.push(Notice::warning(t));
    }

    fn info(&mut self, t: LocalizedText) {
        self.warnings.push(Notice::info(t));
    }

    /// A probe failed: missing implementations are informational, other errors are
    /// warnings.
    fn skipped(&mut self, what: &str, e: &Error) {
        let t = text("switch.warn.check_skipped")
            .arg("what", what)
            .arg("detail", e.to_string());
        if e.is_not_implemented() {
            tracing::debug!(what, error = %e, "probe not available");
            self.info(t);
        } else {
            self.warn(t);
        }
    }
}

/// The resolved destination of a request.
#[derive(Debug, Default)]
struct Dest {
    resolved: Option<ResolvedProfile>,
    site: Option<Site>,
    line: Option<Line>,
    auth: Option<AuthMode>,
    key: Option<Secret>,
    endpoints: Option<ResolvedEndpoints>,
    caps: Option<LineCaps>,
    label: LocalizedText,
    profile_id: Option<ProfileId>,
    writes_local: bool,
    env_op: Option<EnvOp>,
    migration: Option<MigrationRequest>,
    restart_app: bool,
    rollback_migration: Option<String>,
    rollback_remote: Option<TargetId>,
    uninstall: Option<bool>,
    profile_update: Option<ProfileUpdate>,
    /// Restore of the original configuration: provider question pending.
    restore_provider_question: Option<String>,
    install_id: Option<String>,
    /// The profile's Codex settings (token destinations), filtered per home by the
    /// settings plan.
    codex_settings: Option<superba_core::CodexSettings>,
    /// Router mode: the direct resolution remote hosts get.
    remote_resolved: Option<ResolvedProfile>,
    /// Router mode: new member order and configuration to apply before writing.
    router_update: Option<(Vec<ProfileId>, crate::router::BuiltMembers)>,
    /// Router intent: router mode to store after a successful switch.
    router_enabled_after: Option<bool>,
    /// The local home goes through the router (which maps model names per
    /// member).
    routed: bool,
    /// Model catalog file to write before `config.toml`.
    catalog: Option<crate::models::CatalogWrite>,
    /// Catalog record after success (see [`PlanContext::catalog_record`]).
    catalog_record: Option<Option<crate::models::CatalogRecord>>,
}

/// One line judged for a switch (auth decision, Codex path, blocks).
struct LineEval {
    auth: AuthMode,
    path: CodexPath,
    blocks: Vec<BlockReason>,
    warns: Vec<LocalizedText>,
}

/// Judge `line` for `choice` from its probe (`None` = not probed).
/// `pat_version_ok` (see [`rules::pat_version_ok`]) only steers `Auto`: a manual
/// PAT choice is left to the version gate, which names the consumer and lets the
/// user accept a too-old CLI.
fn evaluate_line(
    site: &Site,
    line: &Line,
    caps: Option<&LineCaps>,
    choice: AuthChoice,
    pat_version_ok: Option<bool>,
) -> LineEval {
    let mut cx = AuthContext::new(site).line(line);
    cx.caps = caps;
    if choice.is_auto() {
        cx.pat_version_ok = pat_version_ok;
    }
    let decision = choice.decide(&cx);
    let mut blocks = Vec::new();
    let mut warns = Vec::new();
    let mut path = site
        .capabilities
        .codex_paths
        .first()
        .copied()
        .unwrap_or(CodexPath::BackendApiCodex);
    if let Some(c) = caps {
        if !c.reachable || c.codex_path.is_none() {
            blocks.push(BlockReason::LineUnavailable {
                line_id: line.id.clone(),
            });
        } else {
            if let Some(access) = &c.key_access
                && !access.is_ok()
            {
                blocks.push(BlockReason::KeyRejected {
                    line_id: line.id.clone(),
                    access: access.clone(),
                });
            }
            if let Some(p) = c.codex_path {
                path = p;
            }
        }
    }
    let auth = match decision.mode {
        Some(mode) => {
            if decision.fell_back() {
                let reason = decision.reason.text();
                warns.push(
                    text("switch.warn.pat_fallback")
                        .arg_text("line", line.name.clone())
                        .arg_text("reason", reason),
                );
            }
            mode
        }
        None => {
            if blocks.is_empty() {
                blocks.push(BlockReason::AuthModeUnavailable {
                    line_id: line.id.clone(),
                    auth_mode: choice.representative_mode(),
                });
            }
            choice.representative_mode()
        }
    };
    if auth.writes_chatgpt_base_url() {
        path = CodexPath::BackendApiCodex;
    }
    LineEval {
        auth,
        path,
        blocks,
        warns,
    }
}

/// Label of the local Codex home in the diff preview.
pub const LOCAL_TARGET_LABEL: &str = "This computer";

/// The desktop app's bundled Codex and the first CLI on the PATH.
pub(crate) fn local_consumers(
    install_id: Option<&str>,
    installs: &[superba_core::AppInstall],
    binaries: &[superba_core::CodexBinary],
) -> Vec<SettingsConsumer> {
    let mut consumers = Vec::new();
    if let Some(install) = install_id
        .and_then(|id| installs.iter().find(|i| i.id == id))
        .or_else(|| installs.first())
    {
        consumers.push(SettingsConsumer::new(
            ConsumerKind::DesktopApp,
            APP_CONSUMER_LABEL,
            install.bundled_codex_version.clone(),
        ));
    }
    if let Some(cli) = binaries
        .iter()
        .find(|b| b.path_rank == Some(0))
        .or_else(|| binaries.first())
    {
        consumers.push(SettingsConsumer::new(
            ConsumerKind::CliProcesses,
            cli.path.display().to_string(),
            cli.version.clone(),
        ));
    }
    consumers
}

/// Paths the applied profile manages (Superba wrote them last time).
pub(crate) fn owned_setting_paths(state: &ConfigState, set: &ProfileSet) -> Vec<String> {
    state
        .active
        .as_ref()
        .and_then(|a| a.profile_id.as_ref())
        .and_then(|id| set.profile(id))
        .map(|p| p.codex_settings.paths().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// First Codex is 0.131: earlier versions deserialize `config.toml`'s
/// `service_tier` as an enum of `fast` / `flex`.
const SERVICE_TIER_STRING_SINCE: superba_core::CodexVersion =
    superba_core::CodexVersion::new(0, 131, 0);

/// A local Codex (the app's, pin included, or the terminal's) older than
/// [`SERVICE_TIER_STRING_SINCE`], as (label, version).
fn old_service_tier_consumer(
    install_id: Option<&str>,
    installs: &[superba_core::AppInstall],
    binaries: &[superba_core::CodexBinary],
) -> Option<(String, superba_core::CodexVersion)> {
    let (app, cli) = crate::models::local_codexes(install_id, installs, binaries);
    [app, cli].into_iter().flatten().find_map(|c| {
        let v = c.version?;
        (v < SERVICE_TIER_STRING_SINCE).then(|| {
            let label = c
                .binary
                .map(|b| b.display().to_string())
                .unwrap_or_else(|| "codex".to_owned());
            (label, v)
        })
    })
}

/// The model Codex runs with when the destination profile sets none: the value
/// in `config.toml`, unless the applied profile wrote that value itself (its own
/// model, a site's name for a mapped model or a tier-suffixed name). Then the
/// `model` key is Superba's and a switch that sets none restores the user's own
/// value from the baseline, so that value is the one a site's model map must see
/// (e2e s19: after a Krill profile with a default model, a custom site got the
/// user's model unmapped).
pub(crate) fn model_left_to_config(
    state: &ConfigState,
    set: &ProfileSet,
    sites: &[Site],
    baseline: Option<&superba_core::Baseline>,
) -> Option<String> {
    let current = state.model.clone();
    let applied = state
        .active
        .as_ref()
        .and_then(|a| a.profile_id.as_ref())
        .and_then(|id| set.profile(id));
    let wrote = applied.is_some_and(|a| {
        let site = sites.iter().find(|s| s.id == a.site_id);
        a.model.model.is_some()
            || current.as_deref().is_some_and(|m| {
                site.is_some_and(|s| {
                    s.model_map.values().any(|v| v == m)
                        || (s.capabilities.tier_model_suffix
                            && (m.ends_with("-fast") || m.ends_with("-ultrafast")))
                })
            })
    });
    if !wrote {
        return current;
    }
    match baseline.and_then(|b| b.keys.iter().find(|k| k.path == "model")) {
        Some(snap) => snap.value.as_deref().and_then(toml_basic_string),
        None => current,
    }
}

/// The text of a TOML string value as the baseline records it (`"gpt-5"`,
/// `'gpt-5'`).
fn toml_basic_string(repr: &str) -> Option<String> {
    let r = repr.trim();
    if r.len() >= 2 && r.starts_with('\'') && r.ends_with('\'') {
        return Some(r[1..r.len() - 1].to_owned());
    }
    serde_json::from_str::<String>(r).ok()
}

/// Settings that are not written and engine notes, as plan notes.
fn report_settings(sp: &SettingsPlan, host: Option<&str>, draft: &mut Draft) {
    for skip in &sp.skipped {
        let t = text("switch.warn.setting_skipped")
            .arg("path", &skip.path)
            .arg_text("reason", skip.reason.text());
        draft.info(match host {
            Some(h) => text("switch.warn.on_host")
                .arg("host", h)
                .arg_text("text", t),
            None => t,
        });
    }
    for m in &sp.migrations {
        draft.info(text("switch.warn.setting_migration").arg_text("migration", m.text()));
    }
    for n in &sp.notes {
        if !draft.warnings.contains(n) {
            draft.warnings.push(n.clone());
        }
    }
}

/// The diff preview of a plan: every owned key that changes, local first.
fn preview(plan: &SwitchPlan, settings: &[SettingsPlan]) -> Vec<superba_core::SettingChange> {
    let restore = matches!(
        plan.config
            .as_ref()
            .and_then(|c| c.desired.as_ref())
            .map(|d| &d.mode),
        Some(ResolvedMode::Original { .. } | ResolvedMode::Generation { .. })
    );
    let local = TargetId::local();
    let mut out = Vec::new();
    if let Some(cp) = &plan.config {
        let sp = settings.iter().find(|p| p.home.as_ref() == Some(&local));
        out.extend(preview_changes(
            &cp.key_changes,
            sp,
            restore,
            PreviewTarget {
                id: &local,
                label: LOCAL_TARGET_LABEL,
            },
        ));
    }
    for rp in &plan.remotes {
        let sp = settings
            .iter()
            .find(|p| p.home.as_ref() == Some(&rp.target_id));
        let label = crate::messages::target_label(rp.target_id.as_str());
        out.extend(preview_changes(
            &rp.key_changes,
            sp,
            restore,
            PreviewTarget {
                id: &rp.target_id,
                label: &label,
            },
        ));
    }
    out
}

/// Environment variable naming a JSON file with extra sites (`[Site, ...]`) for
/// end-to-end tests: e.g. a Krill-kind site whose lines are loopback forwarders in
/// front of a test relay. Their lines are always marked [`LineSource::Test`]. Never
/// set in production.
pub const ENV_SUPERBA_TEST_SITES: &str = "SUPERBA_TEST_SITES";

/// Sites from [`ENV_SUPERBA_TEST_SITES`]; an unreadable file is logged and ignored.
pub(crate) fn test_sites(env: &dyn superba_core::EnvSource) -> Vec<Site> {
    let Some(path) = env.var_non_empty(ENV_SUPERBA_TEST_SITES) else {
        return Vec::new();
    };
    let parsed = std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<Vec<Site>>(&b).map_err(|e| e.to_string()));
    match parsed {
        Ok(mut sites) => {
            for s in &mut sites {
                for l in &mut s.lines {
                    l.source = superba_core::LineSource::Test;
                }
            }
            sites
        }
        Err(e) => {
            tracing::warn!(path, error = %e, "ignoring unreadable test sites");
            Vec::new()
        }
    }
}

/// Loopback e2e lines from `SUPERBA_TEST_LINES` (see
/// [`superba_net::ENV_SUPERBA_TEST_LINES`]), appended to the built-in Krill site.
/// An invalid value is logged and ignored.
fn add_test_lines(sites: &mut [Site], env: &dyn superba_core::EnvSource) {
    let Some(spec) = env.var_non_empty(superba_net::ENV_SUPERBA_TEST_LINES) else {
        return;
    };
    let lines = match superba_net::parse_test_lines(&spec) {
        Ok(lines) => lines,
        Err(e) => {
            tracing::warn!(error = %e, "ignoring invalid test lines");
            return;
        }
    };
    if let Some(krill) = sites.iter_mut().find(|s| s.id.as_str() == KRILL_SITE_ID) {
        for line in lines {
            if !krill.lines.iter().any(|l| l.id == line.id) {
                krill.lines.push(line);
            }
        }
    }
}

/// Built-in, test and custom sites (later ones never shadow earlier ids).
pub(crate) fn all_sites(set: &ProfileSet, env: &dyn superba_core::EnvSource) -> Vec<Site> {
    let mut sites = catalog::builtin_sites();
    add_test_lines(&mut sites, env);
    for s in test_sites(env).iter().chain(&set.custom_sites) {
        if !sites.iter().any(|b| b.id == s.id) {
            sites.push(s.clone());
        }
    }
    sites
}

/// Responses base URL of a compatible site's line: the line origin plus the base
/// path recorded in its models endpoint (`/v1/models` -> `<origin>/v1`), as
/// [`superba_net::new_compatible_site`] builds it.
pub(crate) fn compatible_base_url(
    site: &Site,
    line: Option<&superba_core::LineId>,
) -> Option<url::Url> {
    let line = match line {
        Some(id) => site.line(id)?,
        None => site.default_line()?,
    };
    let path = site
        .endpoints
        .models_unauth
        .strip_suffix("/models")
        .unwrap_or("")
        .trim_end_matches('/');
    if path.is_empty() {
        Some(line.origin.url().clone())
    } else {
        Some(line.origin.join(path))
    }
}

/// `Ok(None)` for `NotImplemented`, recording the skip; other errors as warnings.
fn soft<T>(draft: &mut Draft, what: &str, r: Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            draft.skipped(what, &e);
            None
        }
    }
}

fn destination_label(site: &Site, line: Option<&Line>, auth: AuthMode) -> LocalizedText {
    match line {
        Some(l) => text("switch.destination.profile")
            .arg_text("site", site.name.clone())
            .arg_text("line", l.name.clone())
            .arg_text("mode", auth.text()),
        None => text("switch.destination.profile_no_line")
            .arg_text("site", site.name.clone())
            .arg_text("mode", auth.text()),
    }
}

/// The app's Codex core (its app-server processes) started after the current
/// generation was applied, so it already reads the configuration on disk.
fn app_core_current(processes: &[ProcessInfo], active: Option<&superba_core::ActiveState>) -> bool {
    let Some(active) = active else {
        return false;
    };
    let mut cores: Vec<ProcessInfo> = processes
        .iter()
        .filter(|p| p.kind == superba_core::CodexProcessKind::AppServer)
        .cloned()
        .collect();
    if cores.is_empty() || cores.iter().any(|p| p.started_at.is_none()) {
        return false;
    }
    rules::mark_stale(&mut cores, Some(active.applied_at));
    cores.iter().all(|p| !p.stale)
}

/// Refuse sudo and Windows administrator tokens (plan 12.4). Real Unix root
/// logins are allowed, but must launch without CDP.
#[cfg(unix)]
pub(crate) fn elevated(env: &dyn superba_core::EnvSource) -> bool {
    env.var_non_empty("SUDO_USER").is_some() && effective_root()
}

#[cfg(windows)]
pub(crate) fn elevated(_env: &dyn superba_core::EnvSource) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = std::ptr::null_mut();
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    // SAFETY: these APIs receive valid output buffers; the queried process token
    // is closed on every path after opening. Fail closed if its state is unknown.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            tracing::warn!(error = %std::io::Error::last_os_error(), "cannot query process token");
            return true;
        }
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        if ok == 0 {
            tracing::warn!(error = %std::io::Error::last_os_error(), "cannot query token elevation");
        }
        CloseHandle(token);
        ok == 0 || elevation.TokenIsElevated != 0
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn elevated(_env: &dyn superba_core::EnvSource) -> bool {
    false
}

#[cfg(unix)]
pub(crate) fn effective_root() -> bool {
    // SAFETY: geteuid has no preconditions and does not depend on environment variables.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
pub(crate) fn effective_root() -> bool {
    false
}

/// Never expose a privileged app over an unauthenticated loopback CDP port.
pub(crate) fn launch_cdp_mode(safe_mode: bool, is_root: bool) -> CdpMode {
    if safe_mode || is_root {
        CdpMode::Disabled
    } else {
        CdpMode::Auto
    }
}

impl Switcher {
    pub(crate) async fn site(&self, id: &SiteId) -> Result<Site> {
        let set = self.deps.profiles.load().await?;
        all_sites(&set, self.deps.ctx.env.as_ref())
            .into_iter()
            .find(|s| &s.id == id)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {id}"),
            })
    }

    pub(crate) async fn plan_impl(&self, mut request: SwitchRequest) -> Result<SwitchPlan> {
        match &request.intent {
            SwitchIntent::HostSync { .. } => return self.plan_host_sync(request).await,
            SwitchIntent::AppCodexPin { .. } => return self.plan_pin(request).await,
            _ => {}
        }
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let set = d.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        let state = d.config.read_state(&home).await?;
        let baseline = d.config.baseline(&home).await?;
        let first_takeover = baseline.as_ref().is_none_or(|b| b.released);
        let mut draft = Draft::default();
        let local = request.targets.local;
        // v0.3: a profile switch changes a remote host only when the host follows
        // this computer (its connection only); explicit targets stay as asked.
        let followed: Vec<TargetId> = if local
            && request.targets.remotes.is_empty()
            && matches!(request.intent, SwitchIntent::Profile { .. })
        {
            set.settings
                .hosts
                .iter()
                .filter(|(_, h)| h.follow_switches)
                .map(|(t, _)| t.clone())
                .collect()
        } else {
            Vec::new()
        };
        request.targets.remotes.extend(followed.iter().cloned());

        if local {
            if state.journal_pending {
                draft.block(BlockReason::JournalPending);
            }
            if let Some(message) = &state.parse_error {
                draft.block(BlockReason::Unparseable {
                    path: home.config_toml(),
                    message: message.clone(),
                });
            }
        }
        if elevated(d.ctx.env.as_ref()) {
            draft.block(BlockReason::Elevated);
        }
        if local && effective_root() {
            draft.warn(text("switch.warn.root_no_hot_switch"));
        }
        if let Ok(unfinished) = SwitchJournal::new(&d.ctx.data_dir).unfinished()
            && !unfinished.is_empty()
        {
            draft.block(BlockReason::JournalPending);
        }

        let report = soft(&mut draft, "sessions", d.sessions.scan(&home).await);

        // --- app and CLI (before the destination: Automatic authentication
        //     needs the Codex versions that read this home) --------------------
        let installs =
            soft(&mut draft, "app installs", d.app.detect_installs().await).unwrap_or_default();
        // The app runs its pinned Codex when one is set: that version is the app
        // consumer for every version rule below.
        let installs = self.effective_installs(installs, &set).await;
        let binaries =
            soft(&mut draft, "codex binaries", d.app.codex_binaries().await).unwrap_or_default();
        let session = soft(&mut draft, "app session", d.app.running_session().await).flatten();
        // An app running on another Codex home does not read what we write: it is not
        // a consumer of this switch (and must never be restarted by it).
        let session = match session {
            Some(s) if s.codex_home.as_ref().is_some_and(|h| h != home.root()) => {
                draft.warn(
                    text("switch.doctor.codex_home_mismatch")
                        .arg(
                            "app_home",
                            s.codex_home
                                .as_ref()
                                .map(|h| h.display().to_string())
                                .unwrap_or_default(),
                        )
                        .arg("home", home.root().display()),
                );
                None
            }
            other => other,
        };
        let install_id = session
            .as_ref()
            .and_then(|s| s.install_id.clone())
            .or_else(|| set.settings.preferred_app_id.clone())
            .or_else(|| installs.first().map(|i| i.id.clone()));
        let local_versions = rules::local_versioned_consumers(
            install_id.as_deref(),
            &installs,
            &binaries,
            APP_CONSUMER_LABEL,
        );
        let pat_version_ok = if local {
            rules::pat_version_ok(&local_versions)
        } else {
            None
        };

        // --- destination ------------------------------------------------------
        let mut dest = self
            .resolve_destination(
                &request,
                &set,
                &sites,
                &state,
                baseline.as_ref(),
                pat_version_ok,
                &mut draft,
            )
            .await?;
        // An explicit install (RestartApp) wins over the running / preferred one.
        dest.install_id = dest.install_id.take().or(install_id);

        // --- processes and the restart bridge --------------------------------------
        let processes: Vec<ProcessInfo> =
            soft(&mut draft, "processes", d.app.processes(&home).await).unwrap_or_default();
        let cdp_ok = session
            .as_ref()
            .and_then(|s| s.cdp.as_ref())
            .is_some_and(|c| c.owner_verified);
        let bridge_ready = match (&session, cdp_ok) {
            (Some(s), true) => soft(&mut draft, "restart bridge", d.app.probe_bridge(s).await)
                .is_some_and(|b| b.is_ready()),
            _ => false,
        };

        // --- version gates (plan 9.8) ------------------------------------------
        let site_kind = dest.site.as_ref().map(|s| s.kind);
        let caps = rules::required_capabilities(site_kind, dest.auth);
        if local && !caps.is_empty() {
            let consumers = rules::local_versioned_consumers(
                dest.install_id.as_deref(),
                &installs,
                &binaries,
                APP_CONSUMER_LABEL,
            );
            let gate = rules::gate_versions(&consumers, &caps);
            for b in gate.blocks {
                draft.block(b);
            }
            for b in gate.cli_too_old {
                if request.accept_unknown_versions {
                    if let BlockReason::VersionTooOld {
                        consumer,
                        found,
                        required,
                        ..
                    } = &b
                    {
                        draft.warn(
                            text("switch.warn.cli_too_old")
                                .arg("consumer", consumer)
                                .arg("found", found)
                                .arg("required", required),
                        );
                    }
                } else {
                    draft.block(b);
                }
            }
            self.unknown_versions(&gate.unknown, &request, &mut draft);
        }

        // --- Codex settings (decisions 3/4) ----------------------------------------
        let mut settings_plans: Vec<SettingsPlan> = Vec::new();
        if local
            && let Some(sp) = self
                .plan_local_settings(
                    &set, &state, &home, &installs, &binaries, &mut dest, &mut draft,
                )
                .await
        {
            settings_plans.push(sp);
        }

        // --- sessions (plan 10) --------------------------------------------------
        if local {
            self.legacy_sessions(&request, &state, report.as_ref(), &mut dest, &mut draft);
            if let Some(provider) = dest.restore_provider_question.clone() {
                let migrated_in: u32 = soft(
                    &mut draft,
                    "migration history",
                    d.sessions.history(&home).await,
                )
                .unwrap_or_default()
                .iter()
                .filter(|r| !r.rolled_back && r.to_provider == PROVIDER_OPENAI)
                .map(|r| r.by_source_provider.get(&provider).copied().unwrap_or(0))
                .sum();
                // Sessions under `openai` now, minus those there at takeover and
                // those Superba migrated in (they are counted separately).
                let created_during = report.as_ref().map_or(0, |r| {
                    let now = r.distribution().get(PROVIDER_OPENAI).copied().unwrap_or(0);
                    let before = baseline
                        .as_ref()
                        .and_then(|b| b.session_distribution.get(PROVIDER_OPENAI).copied())
                        .unwrap_or(0);
                    now.saturating_sub(before).saturating_sub(migrated_in)
                });
                draft.choices.push(PlanChoice::RestoreProvider {
                    baseline_provider: provider,
                    created_during,
                    migrated_in,
                });
            }
        }
        let migration = match (&dest.migration, local) {
            (Some(req), true) => self.plan_migration(&home, req, &mut draft).await,
            _ => None,
        };

        // --- model catalog (plan 4 #17) ---------------------------------------
        let model_check = if local && draft.blocks.is_empty() {
            let left = model_left_to_config(&state, &set, &sites, baseline.as_ref());
            self.check_model(&request, left, &mut dest, &mut draft)
                .await
        } else {
            None
        };

        // --- models: speed tier and model catalog file (v0.3) --------------------
        if local && draft.blocks.is_empty() {
            self.plan_models(&set, &state, &installs, &binaries, &mut dest, &mut draft)
                .await;
        }

        // --- local writes ---------------------------------------------------------
        let mut config_plan = None;
        let mut env_plan: Option<EnvPlan> = None;
        let mut extras = BaselineExtras {
            session_distribution: report
                .as_ref()
                .map(SessionReport::distribution)
                .unwrap_or_default(),
            ..BaselineExtras::default()
        };
        if local && draft.blocks.is_empty() && dest.writes_local {
            if let Some(resolved) = &dest.resolved {
                match d.config.plan(&home, resolved).await {
                    Ok(p) => {
                        self.resolve_rendered_auth(&home, &state, &sites, &p, &mut dest)
                            .await?;
                        if let Some(EnvOp::Set {
                            take_over_foreign, ..
                        }) = &mut dest.env_op
                        {
                            *take_over_foreign = request.take_over_foreign_env.unwrap_or(false);
                        }
                        config_plan = Some(p);
                    }
                    Err(Error::Conflict { .. }) => {
                        let conflicts = state
                            .drift
                            .iter()
                            .filter(|k| !request.key_resolutions.contains_key(&k.path))
                            .cloned()
                            .collect::<Vec<_>>();
                        draft.choices.push(PlanChoice::OwnedKeyConflict {
                            conflicts: if conflicts.is_empty() {
                                state.drift.clone()
                            } else {
                                conflicts
                            },
                        });
                    }
                    Err(Error::Parse { what, message }) => draft.block(BlockReason::Unparseable {
                        path: home.root().join(what),
                        message,
                    }),
                    Err(e) => return Err(e),
                }
            }
            if let Some(op) = dest.env_op.clone() {
                env_plan = self
                    .plan_env(op, &request, first_takeover, &mut extras, &mut draft)
                    .await?;
            } else if first_takeover
                && let Some(r) = soft(
                    &mut draft,
                    "environment",
                    d.env.inspect(ENV_AUTHAPI_BASE_URL).await,
                )
            {
                extras.env = snapshots(&r);
                extras.legacy_env_lines = r.legacy_lines;
            }
        }

        // --- app facts and restart decision (plan 7.1) ------------------------
        let needs_authapi = dest.auth.is_some_and(AuthMode::needs_authapi_env);
        let authapi_env = session
            .as_ref()
            .map(|s| s.authapi_env.clone())
            .unwrap_or(EnvObservation::Unknown);
        let authapi_health = match (&session, needs_authapi, authapi_env.value()) {
            (Some(_), true, Some(origin)) => Some(self.authapi_health(&dest, origin).await),
            _ => None,
        };
        let app_facts = match &session {
            Some(s) => AppFacts::Running {
                cdp: cdp_ok,
                bridge_ready,
                authapi_env: s.authapi_env.clone(),
                authapi_health: authapi_health.clone(),
            },
            None => AppFacts::NotRunning,
        };
        let primary = if !local {
            if draft.blocks.is_empty() {
                RestartKind::None
            } else {
                RestartKind::Blocked {
                    reasons: draft.blocks.clone(),
                }
            }
        } else if dest.restart_app {
            if !draft.blocks.is_empty() {
                RestartKind::Blocked {
                    reasons: draft.blocks.clone(),
                }
            } else if session.is_some() {
                RestartKind::FullAppRestart {
                    reason: if cdp_ok {
                        superba_core::FullRestartReason::Requested
                    } else {
                        superba_core::FullRestartReason::NoCdp
                    },
                }
            } else {
                RestartKind::None
            }
        } else {
            decide_local(&LocalFacts {
                blocks: draft.blocks.clone(),
                migration: migration.is_some() || dest.rollback_migration.is_some(),
                current_family: state.auth.family(),
                target_family: dest.auth.map(AuthMode::family),
                target_needs_authapi: needs_authapi,
                app: app_facts,
                hot_supported: rules::platform_hot_supported(),
            })
        };

        // Router mode: a profile switch that only reorders the router's members
        // needs no restart (the router reloads; Codex keeps pointing at it).
        let primary = if local
            && dest.router_update.is_some()
            && config_plan.as_ref().is_none_or(|c| c.is_noop())
            && env_plan.as_ref().is_none_or(|e| e.is_noop())
            && migration.is_none()
            && !primary.is_blocked()
        {
            RestartKind::None
        } else {
            primary
        };

        // Nothing to do locally: no file or environment change, nothing to
        // migrate, and the app's Codex core already runs on this configuration
        // (it started after the current generation was written). A hot restart
        // would only interrupt running turns.
        let primary = if local
            && primary.is_hot()
            && config_plan.as_ref().is_some_and(|c| c.is_noop())
            && env_plan.as_ref().is_none_or(|e| e.is_noop())
            && migration.is_none()
            && app_core_current(&processes, state.active.as_ref())
        {
            RestartKind::None
        } else {
            primary
        };

        // --- turn activity (plan 7.4) -------------------------------------------
        let mut active_threads: Vec<ThreadActivity> = Vec::new();
        if local
            && let Some(a) = soft(
                &mut draft,
                "running conversations",
                d.app.turn_activity(&home, session.as_ref()).await,
            )
        {
            active_threads.extend(a.busy);
        }

        // --- remote hosts (plan 8) ---------------------------------------------
        let (hosts, remote_plans, remote_restarts) = self
            .plan_remotes(
                &request,
                &followed,
                &dest,
                &home,
                session.as_ref(),
                &processes,
                caps.as_slice(),
                &mut active_threads,
                &mut draft,
            )
            .await;

        // --- consumers, warnings, alternatives ----------------------------------
        let restarts =
            rules::consumer_restarts(&primary, local, &processes, home.root(), &remote_restarts);
        if local && !primary.is_blocked() {
            self.consumer_warnings(&restarts, &processes, &dest, &mut draft);
            self.restart_notes(&primary, &dest, &sites, &authapi_env, &mut draft);
            if let (Some(from), Some(to)) = (
                state.active.as_ref().and_then(|a| a.site_id.as_ref()),
                dest.site.as_ref(),
            ) && from != &to.id
                && to.kind != SiteKind::Official
            {
                draft.info(text("switch.warn.cross_site"));
            }
            if dest.uninstall.is_some() {
                draft.cleanup.push(text("switch.destination.uninstall"));
            }
        }
        let remote_hosts: Vec<String> = remote_plans
            .iter()
            .filter(|p| p.blocked.is_empty())
            .map(|p| crate::messages::target_label(p.target_id.as_str()))
            .collect();
        if !remote_hosts.is_empty() {
            draft.warn(text("switch.warn.remotes").arg("hosts", remote_hosts.join(", ")));
        }
        self.alternatives(&request, &primary, &dest, &mut draft);

        let launch_authapi = {
            let active = state.active.as_ref();
            let (krill, active_line) = rules::active_krill_site(
                &sites,
                active.and_then(|a| a.site_id.as_ref()),
                active.and_then(|a| a.line_id.as_ref()),
            );
            rules::launch_authapi_origin(
                &krill,
                dest.line.as_ref(),
                dest.site
                    .as_ref()
                    .is_some_and(|s| s.kind == SiteKind::Krill),
                active_line.as_ref(),
            )
        };

        let id = uuid::Uuid::new_v4().simple().to_string();
        let to = PlanDestination {
            profile_id: dest.profile_id.clone(),
            site_id: dest.site.as_ref().map(|s| s.id.clone()),
            line_id: dest.line.as_ref().map(|l| l.id.clone()),
            auth_mode: dest.auth,
            label: dest.label.clone(),
        };
        let plan = SwitchPlan {
            id: id.clone(),
            created_at: d.clock.now(),
            request,
            from: state.active.clone(),
            to,
            primary,
            restarts,
            config: config_plan,
            env: env_plan,
            migration,
            remotes: remote_plans,
            active_threads,
            model_check,
            choices: draft.choices,
            alternatives: draft.alternatives,
            warnings: draft.warnings,
            cleanup: draft.cleanup,
        };
        let settings = PlanSettings {
            changes: preview(&plan, &settings_plans),
            targets: settings_plans,
        };
        self.remember_plan(
            id,
            PlanContext {
                home,
                site: dest.site,
                auth: dest.auth,
                key: dest.key,
                endpoints: dest.endpoints,
                session,
                hosts,
                launch_authapi,
                baseline_missing: first_takeover,
                baseline_released: baseline.as_ref().is_some_and(|b| b.released),
                baseline_extras: extras,
                profile_update: dest.profile_update,
                install_id: dest.install_id,
                safe_mode: set.settings.safe_mode,
                restart_app: dest.restart_app,
                rollback_migration: dest.rollback_migration,
                rollback_remote: dest.rollback_remote,
                uninstall: dest.uninstall,
                settings,
                router_update: dest.router_update,
                router_enabled_after: dest.router_enabled_after,
                host_sync: None,
                pin: None,
                catalog: dest.catalog,
                catalog_record: dest.catalog_record,
                pin_config: None,
            },
        );
        Ok(plan)
    }

    /// Speed tier and model catalog of a key-based destination: `service_tier`
    /// is always written (`default` for Standard); a catalog file only when the
    /// profile needs one (built, preflighted, written at execution). A catalog
    /// that cannot be built or is refused by a local Codex blocks the switch.
    #[allow(clippy::too_many_arguments)]
    async fn plan_models(
        &self,
        set: &ProfileSet,
        state: &ConfigState,
        installs: &[superba_core::AppInstall],
        binaries: &[superba_core::CodexBinary],
        dest: &mut Dest,
        draft: &mut Draft,
    ) {
        let had_catalog = crate::models::current_record(
            &self.deps.ctx.data_dir,
            self.deps.ctx.codex_home().root(),
        )
        .is_some();
        let token = dest
            .resolved
            .as_ref()
            .is_some_and(|r| matches!(r.mode, ResolvedMode::Token { .. }));
        let profile = dest.profile_id.as_ref().and_then(|id| set.profile(id));
        let Some(profile) = profile.filter(|p| token && !p.is_builtin()) else {
            // Restores and the official login release the catalog key.
            if had_catalog && dest.resolved.is_some() {
                dest.catalog_record = Some(None);
            }
            return;
        };
        let Some(resolved) = dest.resolved.as_ref() else {
            return;
        };
        let Some(site) = dest.site.clone() else {
            return;
        };
        let auth = dest.auth.unwrap_or(AuthMode::ApiKey);
        // Without a default model the live one is suffixed; on a tier-suffix site
        // that may be the `<slug>-fast` name Superba wrote last time, whose base
        // gets the suffix again (never `<slug>-fast-fast`).
        let configured = resolved.model.model.clone().or_else(|| {
            state.model.clone().map(|m| {
                if site.capabilities.tier_model_suffix {
                    superba_models::split_tier_suffix(&m)
                        .map(|(base, _)| base.to_owned())
                        .unwrap_or(m)
                } else {
                    m
                }
            })
        });
        let cp = self
            .plan_catalog(crate::models::CatalogPlanInput {
                profile,
                site: &site,
                auth,
                routed: dest.routed,
                endpoints: dest.endpoints.as_ref(),
                key: dest.key.as_ref(),
                install_id: dest.install_id.as_deref(),
                installs,
                binaries,
                configured_model: configured,
            })
            .await;
        for n in cp.notes {
            if !draft.warnings.contains(&n) {
                draft.warnings.push(n);
            }
        }
        if let Some(reason) = cp.failed {
            draft.block(BlockReason::ModelCatalog { reason });
            return;
        }
        // Remote hosts get the destination without local-only model changes.
        if dest.remote_resolved.is_none() {
            dest.remote_resolved = dest.resolved.clone();
        }
        let speed = crate::models::profile_speed(profile);
        let tier = if cp.speed_via_model {
            superba_core::SpeedTier::Standard
        } else {
            speed
        };
        // Codex 0.110 - 0.130 parse `service_tier` as `fast` / `flex` only and
        // refuse the whole config.toml for any other value: with such a Codex on
        // this computer only `fast` is written.
        let too_old = old_service_tier_consumer(dest.install_id.as_deref(), installs, binaries);
        let Some(resolved) = dest.resolved.as_mut() else {
            return;
        };
        resolved.model.speed = match too_old {
            Some((consumer, version)) if tier != superba_core::SpeedTier::Fast => {
                draft.warn(
                    text("switch.models.service_tier_old_codex")
                        .arg("consumer", consumer)
                        .arg("version", version)
                        .arg("tier", tier.config_value()),
                );
                None
            }
            _ => Some(tier),
        };
        if let Some(m) = cp.model {
            draft.info(text("switch.models.suffix_model").arg("model", &m));
            resolved.model.model = Some(m);
        }
        match cp.write {
            Some(w) => {
                resolved.model_catalog = Some(w.path.clone());
                if cp.folded_globals {
                    resolved.model.context_window = None;
                    resolved.model.auto_compact = None;
                }
                draft.info(text("switch.models.catalog_written").arg("file", w.path.display()));
                if site.kind == SiteKind::Krill {
                    draft.warn(text("switch.models.fingerprint"));
                }
                dest.catalog = Some(w);
                dest.catalog_record = Some(cp.record);
            }
            None => {
                resolved.model_catalog = None;
                if had_catalog {
                    dest.catalog_record = Some(None);
                }
            }
        }
    }

    /// Filter the destination's Codex settings for the local home's Codex
    /// versions and store the result in the resolved profile.
    #[allow(clippy::too_many_arguments)]
    async fn plan_local_settings(
        &self,
        set: &ProfileSet,
        state: &ConfigState,
        home: &CodexHome,
        installs: &[superba_core::AppInstall],
        binaries: &[superba_core::CodexBinary],
        dest: &mut Dest,
        draft: &mut Draft,
    ) -> Option<SettingsPlan> {
        let wanted = dest.codex_settings.clone()?;
        let resolved = dest.resolved.as_mut()?;
        if !matches!(resolved.mode, ResolvedMode::Token { .. }) {
            return None;
        }
        let consumers = local_consumers(dest.install_id.as_deref(), installs, binaries);
        let bytes = tokio::fs::read(home.config_toml()).await.ok();
        let owned = owned_setting_paths(state, set);
        let local = TargetId::local();
        let input = HomeInput::new(&local, &consumers, self.deps.catalog.as_ref())
            .config_toml(bytes.as_deref())
            .owned_paths(&owned);
        let sp = plan_settings(&wanted, &input);
        resolved.settings = sp.apply.clone();
        report_settings(&sp, None, draft);
        Some(sp)
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_destination(
        &self,
        request: &SwitchRequest,
        set: &ProfileSet,
        sites: &[Site],
        state: &ConfigState,
        baseline: Option<&superba_core::Baseline>,
        pat_version_ok: Option<bool>,
        draft: &mut Draft,
    ) -> Result<Dest> {
        match &request.intent {
            SwitchIntent::Profile {
                profile_id,
                line_id,
                auth_mode,
            } => {
                let profile = set.profile(profile_id).ok_or_else(|| Error::NotFound {
                    what: format!("profile {profile_id}"),
                })?;
                match profile.builtin {
                    Some(BuiltinProfile::Original) => {
                        Ok(self.restore_original(request, None, baseline, false, draft))
                    }
                    Some(BuiltinProfile::Official) => Ok(self
                        .restore_official(request, Some(profile.id.clone()), draft)
                        .await),
                    None => {
                        let site = sites
                            .iter()
                            .find(|s| s.id == profile.site_id)
                            .cloned()
                            .ok_or_else(|| Error::NotFound {
                                what: format!("site {}", profile.site_id),
                            })?;
                        if site.kind == SiteKind::Official {
                            return Ok(self
                                .restore_official(request, Some(profile.id.clone()), draft)
                                .await);
                        }
                        self.token_destination(
                            request,
                            profile,
                            site,
                            line_id.clone(),
                            *auth_mode,
                            None,
                            pat_version_ok,
                            draft,
                        )
                        .await
                    }
                }
            }
            SwitchIntent::Router { enable } => {
                let router = &set.settings.router;
                if *enable && router.members.is_empty() {
                    return Err(Error::invalid(
                        "the router has no members yet: install it with member profiles first",
                    ));
                }
                let usable = |id: &ProfileId| {
                    set.profile(id).is_some_and(|p| {
                        !p.is_builtin()
                            && sites
                                .iter()
                                .any(|s| s.id == p.site_id && s.kind != SiteKind::Official)
                    })
                };
                let profile_id = state
                    .active
                    .as_ref()
                    .and_then(|a| a.profile_id.clone())
                    .filter(|id| usable(id))
                    .or_else(|| router.members.iter().find(|id| usable(id)).cloned())
                    .ok_or_else(|| Error::NotFound {
                        what: "a key-based profile to switch".into(),
                    })?;
                let profile = set.profile(&profile_id).ok_or_else(|| Error::NotFound {
                    what: format!("profile {profile_id}"),
                })?;
                let site = sites
                    .iter()
                    .find(|s| s.id == profile.site_id)
                    .cloned()
                    .ok_or_else(|| Error::NotFound {
                        what: format!("site {}", profile.site_id),
                    })?;
                let mut dest = self
                    .token_destination(
                        request,
                        profile,
                        site,
                        None,
                        None,
                        Some(*enable),
                        pat_version_ok,
                        draft,
                    )
                    .await?;
                dest.router_enabled_after = Some(*enable);
                // Router mode never changes the profile's stored line or auth.
                if let Some(u) = dest.profile_update.as_mut() {
                    u.line_id = None;
                    u.auth_mode = None;
                }
                Ok(dest)
            }
            SwitchIntent::Restore { target } => match target {
                RestoreTarget::Original { provider } => {
                    Ok(self.restore_original(request, Some(*provider), baseline, false, draft))
                }
                RestoreTarget::Official => Ok(self.restore_official(request, None, draft).await),
                RestoreTarget::Generation { generation } => {
                    let resolved = ResolvedProfile {
                        model_catalog: None,
                        profile_id: None,
                        site_id: None,
                        line_id: None,
                        mode: ResolvedMode::Generation {
                            generation: *generation,
                        },
                        model: Default::default(),
                        key_resolutions: request.key_resolutions.clone(),
                        settings: Default::default(),
                    };
                    Ok(Dest {
                        resolved: Some(resolved),
                        label: text("switch.destination.generation").arg("generation", generation),
                        writes_local: true,
                        ..Dest::default()
                    })
                }
            },
            SwitchIntent::MigrateSessions { request: mreq } => {
                let mut mreq = mreq.clone();
                for id in &request.migration_exclude {
                    if !mreq.exclude.contains(id) {
                        mreq.exclude.push(id.clone());
                    }
                }
                Ok(Dest {
                    label: text("switch.destination.migrate"),
                    migration: Some(mreq),
                    ..Dest::default()
                })
            }
            SwitchIntent::RollbackMigration { migration_id } => Ok(Dest {
                label: text("switch.destination.rollback_migration").arg("id", migration_id),
                rollback_migration: Some(migration_id.clone()),
                ..Dest::default()
            }),
            SwitchIntent::Uninstall {
                delete_backups,
                provider,
            } => {
                let mut dest = if baseline.is_some() {
                    self.restore_original(request, *provider, baseline, true, draft)
                } else {
                    Dest {
                        env_op: Some(EnvOp::Clear),
                        writes_local: true,
                        ..Dest::default()
                    }
                };
                dest.label = text("switch.destination.uninstall");
                dest.uninstall = Some(*delete_backups);
                Ok(dest)
            }
            SwitchIntent::RestartApp { install_id } => Ok(Dest {
                label: text("switch.destination.restart_app"),
                restart_app: true,
                install_id: install_id.clone(),
                ..Dest::default()
            }),
            SwitchIntent::RollbackRemote { target_id } => {
                let journal = SwitchJournal::new(&self.deps.ctx.data_dir);
                if journal.last_remote(target_id)?.is_none() {
                    draft.block(BlockReason::UnsupportedTarget {
                        detail: format!("no recorded switch for {target_id}"),
                    });
                }
                Ok(Dest {
                    label: text("switch.destination.rollback_remote")
                        .arg("host", crate::messages::target_label(target_id.as_str())),
                    rollback_remote: Some(target_id.clone()),
                    ..Dest::default()
                })
            }
            other => Err(Error::Unsupported {
                what: format!("switch intent {other:?}"),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn token_destination(
        &self,
        request: &SwitchRequest,
        profile: &Profile,
        site: Site,
        line_override: Option<superba_core::LineId>,
        auth_override: Option<AuthMode>,
        router_override: Option<bool>,
        pat_version_ok: Option<bool>,
        draft: &mut Draft,
    ) -> Result<Dest> {
        let d = &self.deps;
        // v2 auth: the profile's choice (Auto by default); a per-switch override is
        // a manual choice (Advanced) that never falls back silently.
        let choice = match auth_override {
            Some(AuthMode::Pat) => AuthChoice::Pat,
            Some(AuthMode::ApiKey) => AuthChoice::ApiKey,
            _ => profile.auth_choice(),
        };
        if let Some(mode) = auth_override
            && (!mode.uses_key() || !site.supports(mode))
        {
            draft.block(BlockReason::AuthModeUnavailable {
                line_id: line_override
                    .clone()
                    .or_else(|| profile.line_id.clone())
                    .unwrap_or_else(|| superba_core::LineId::new("-")),
                auth_mode: mode,
            });
        }
        let key = match &profile.key_ref {
            Some(r) => match d.secrets.get(r).await {
                Ok(k) if !k.is_empty() => Some(k),
                Ok(_) => {
                    draft.block(BlockReason::MissingKey);
                    None
                }
                Err(e) => {
                    draft.skipped("key", &e);
                    draft.block(BlockReason::MissingKey);
                    None
                }
            },
            None => {
                draft.block(BlockReason::MissingKey);
                None
            }
        };
        let router_settings = match d.profiles.load().await {
            Ok(set) => set.settings.router,
            Err(_) => Default::default(),
        };
        let router_on = router_override.unwrap_or(router_settings.enabled) && request.targets.local;

        let mut dest = Dest {
            site: Some(site.clone()),
            profile_id: Some(profile.id.clone()),
            writes_local: true,
            profile_update: Some(ProfileUpdate {
                profile_id: profile.id.clone(),
                line_id: line_override.clone(),
                auth_mode: auth_override,
            }),
            codex_settings: Some(profile.codex_settings.clone()),
            ..Dest::default()
        };

        // Direct mode for the local home, and for remote hosts in router mode.
        let need_direct = !router_on || !request.targets.remotes.is_empty();
        let mut auth = choice.representative_mode();
        let mut endpoints = None;
        if need_direct {
            // A compatible site created from a base URL keeps its base path in the
            // models endpoint (`<base>/models`); profiles without an explicit base
            // URL use it, so `openai_base_url` is not just the bare origin.
            let derived_base =
                if site.kind == SiteKind::Compatible && profile.base_url_override.is_none() {
                    compatible_base_url(&site, line_override.as_ref().or(profile.line_id.as_ref()))
                } else {
                    None
                };
            if let Some(base) = profile.base_url_override.as_ref().or(derived_base.as_ref()) {
                let decision = choice.decide(&AuthContext::new(&site).custom_base_url(true));
                match decision.mode {
                    Some(m) => auth = m,
                    None => draft.block(BlockReason::AuthModeUnavailable {
                        line_id: superba_core::LineId::new("-"),
                        auth_mode: choice.representative_mode(),
                    }),
                }
                endpoints = Some(ResolvedEndpoints::for_base_url(base)?);
            } else {
                let candidates = match &line_override {
                    Some(l) => vec![l.clone()],
                    None => {
                        let mut v = profile.line_priority();
                        v.retain(|l| site.line(l).is_some());
                        if v.is_empty() {
                            v.extend(site.default_line().map(|l| l.id.clone()));
                        }
                        v
                    }
                };
                if candidates.is_empty() {
                    let missing = line_override.clone().or_else(|| profile.line_id.clone());
                    return Err(Error::NotFound {
                        what: match missing {
                            Some(l) => format!("line {l} of site {}", site.id),
                            None => format!("line for site {}", site.id),
                        },
                    });
                }
                if let Some(l) = &line_override
                    && site.line(l).is_none()
                {
                    return Err(Error::NotFound {
                        what: format!("line {l} of site {}", site.id),
                    });
                }
                let mut skipped_lines: Vec<(Line, LocalizedText)> = Vec::new();
                let mut chosen: Option<(Line, Option<LineCaps>, LineEval)> = None;
                let last = candidates.len() - 1;
                for (i, line_id) in candidates.iter().enumerate() {
                    let Some(line) = site.line(line_id).cloned() else {
                        continue;
                    };
                    let caps = self
                        .probe_for_switch(&site, &line, key.as_ref(), draft)
                        .await;
                    let eval = evaluate_line(&site, &line, caps.as_ref(), choice, pat_version_ok);
                    if eval.blocks.is_empty() || i == last {
                        chosen = Some((line, caps, eval));
                        break;
                    }
                    let why = eval.blocks[0].text();
                    skipped_lines.push((line, why));
                }
                let (line, caps, eval) = chosen.ok_or_else(|| Error::NotFound {
                    what: format!("line for site {}", site.id),
                })?;
                if let Some((first, why)) = skipped_lines.first()
                    && eval.blocks.is_empty()
                {
                    draft.warn(
                        text("switch.warn.line_fallback")
                            .arg_text("first", first.name.clone())
                            .arg_text("reason", why.clone())
                            .arg_text("line", line.name.clone()),
                    );
                }
                for b in eval.blocks {
                    draft.block(b);
                }
                for w in eval.warns {
                    draft.warn(w);
                }
                auth = eval.auth;
                endpoints = match ResolvedEndpoints::for_line(&line.origin, eval.path, auth) {
                    Ok(e) => Some(e),
                    Err(_) => {
                        draft.block(BlockReason::AuthModeUnavailable {
                            line_id: line.id.clone(),
                            auth_mode: auth,
                        });
                        None
                    }
                };
                dest.caps = caps;
                dest.line = Some(line);
            }
        }

        let resolved = |endpoints: &ResolvedEndpoints, auth: AuthMode, key: &Secret| {
            let mut model = profile.model.clone();
            if let Some(m) = &request.model_override {
                model.model = Some(m.clone());
            }
            ResolvedProfile {
                model_catalog: None,
                profile_id: Some(profile.id.clone()),
                site_id: Some(site.id.clone()),
                line_id: None,
                mode: ResolvedMode::Token {
                    site_kind: site.kind,
                    auth,
                    endpoints: endpoints.clone(),
                    key: key.clone(),
                },
                model,
                key_resolutions: request.key_resolutions.clone(),
                settings: Default::default(),
            }
        };

        if router_on {
            // Remote hosts stay direct: keep the direct resolution for them.
            if let (Some(e), Some(k)) = (&endpoints, &key) {
                let mut r = resolved(e, auth, k);
                r.line_id = dest.line.as_ref().map(|l| l.id.clone());
                dest.remote_resolved = Some(r);
            }
            self.route_through_router(profile, &site, choice, &router_settings, &mut dest, draft)
                .await;
            if let (Some(e), Some(token)) = (dest.endpoints.clone(), dest.key.clone()) {
                dest.resolved = Some(resolved(&e, AuthMode::ApiKey, &token));
            }
            dest.label = text("switch.destination.router").arg_text("site", site.name.clone());
            return Ok(dest);
        }

        dest.auth = Some(auth);
        dest.label = destination_label(&site, dest.line.as_ref(), auth);
        if let (Some(endpoints), Some(key)) = (&endpoints, &key) {
            let mut r = resolved(endpoints, auth, key);
            r.line_id = dest.line.as_ref().map(|l| l.id.clone());
            dest.resolved = Some(r);
            if auth.needs_authapi_env()
                && let Some(origin) = &endpoints.authapi_origin
            {
                dest.env_op = Some(EnvOp::Set {
                    value: origin.as_str(),
                    take_over_foreign: request.take_over_foreign_env.unwrap_or(false),
                });
            }
        }
        dest.endpoints = endpoints;
        dest.key = key;
        Ok(dest)
    }

    /// Probe a line with the profile's key (when the site can be probed).
    async fn probe_for_switch(
        &self,
        site: &Site,
        line: &Line,
        key: Option<&Secret>,
        draft: &mut Draft,
    ) -> Option<LineCaps> {
        let k = key?;
        if !(site.capabilities.latency_probe || site.capabilities.key_check) {
            return None;
        }
        match self.deps.prober.probe_with_key(site, line, k).await {
            Ok(c) => Some(c),
            Err(e) => {
                draft.skipped("line", &e);
                if e.is_not_implemented() {
                    None
                } else {
                    // Reported as an unreachable line.
                    Some(LineCaps {
                        site_id: site.id.clone(),
                        line_id: line.id.clone(),
                        checked_at: self.deps.clock.now(),
                        reachable: false,
                        latency_ms: None,
                        pat_ok: false,
                        apikey_ok: false,
                        codex_path: None,
                        blocked_by_edge: false,
                        key_access: None,
                        checks: Vec::new(),
                        detail: Some(LocalizedText::new("", e.to_string())),
                    })
                }
            }
        }
    }

    /// Point the local destination at the router: its endpoint and token (API key
    /// mode), and the router's member order with `profile` first.
    async fn route_through_router(
        &self,
        profile: &Profile,
        site: &Site,
        choice: AuthChoice,
        router: &superba_core::RouterSettings,
        dest: &mut Dest,
        draft: &mut Draft,
    ) {
        let d = &self.deps;
        let decision = choice.decide(&AuthContext::new(site).router(true));
        if decision.mode.is_none() {
            draft.block(BlockReason::AuthModeUnavailable {
                line_id: superba_core::LineId::new("router"),
                auth_mode: choice.representative_mode(),
            });
        }
        dest.auth = Some(AuthMode::ApiKey);
        dest.line = None;
        dest.caps = None;
        dest.env_op = None;
        dest.routed = true;
        let endpoint = match d.router.endpoint() {
            Ok(e) => e,
            Err(e) => {
                draft.block(BlockReason::RouterUnavailable {
                    detail: e.to_string(),
                });
                return;
            }
        };
        if let superba_router::watchdog::Liveness::Down { detail } = d.router.liveness().await {
            draft.block(BlockReason::RouterUnavailable { detail });
            return;
        }
        let mut members = vec![profile.id.clone()];
        members.extend(router.members.iter().filter(|m| *m != &profile.id).cloned());
        if members != router.members {
            match d.profiles.load().await {
                Ok(set) => match self.build_router_members(&set, &members).await {
                    Ok(built) => dest.router_update = Some((members, built)),
                    Err(e) => draft.block(BlockReason::RouterUnavailable {
                        detail: e.to_string(),
                    }),
                },
                Err(e) => draft.block(BlockReason::RouterUnavailable {
                    detail: e.to_string(),
                }),
            }
        }
        draft.warn(
            text("switch.warn.router_mode")
                .arg_text("site", site.name.clone())
                .arg("port", endpoint.port),
        );
        match ResolvedEndpoints::for_base_url(&endpoint.base_url) {
            Ok(e) => {
                dest.endpoints = Some(e);
                dest.key = Some(endpoint.token);
            }
            Err(e) => draft.block(BlockReason::RouterUnavailable {
                detail: e.to_string(),
            }),
        }
    }

    async fn restore_official(
        &self,
        request: &SwitchRequest,
        profile_id: Option<ProfileId>,
        draft: &mut Draft,
    ) -> Dest {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let scope = StashScope::Local(home.root().to_path_buf());
        match d.config.login_stash(&scope).await {
            Ok(None) => draft.warn(text("switch.warn.no_login_stash")),
            Ok(Some(info)) => {
                let since = info.last_refresh.unwrap_or(info.stashed_at);
                let days = d
                    .clock
                    .now()
                    .duration_since(since)
                    .map_or(0, |dur| (dur.as_secs() / 86_400) as i64);
                if days > LOGIN_STASH_WARN_DAYS {
                    draft.warn(text("switch.warn.login_stash_age").arg("days", days));
                }
            }
            Err(e) => draft.skipped("login", &e),
        }
        let site = catalog::official_site();
        Dest {
            resolved: Some(ResolvedProfile {
                model_catalog: None,
                profile_id: profile_id
                    .clone()
                    .or_else(|| Some(ProfileId::new(PROFILE_OFFICIAL))),
                site_id: Some(SiteId::new(OFFICIAL_SITE_ID)),
                line_id: None,
                mode: ResolvedMode::Official,
                model: Default::default(),
                key_resolutions: request.key_resolutions.clone(),
                settings: Default::default(),
            }),
            site: Some(site),
            auth: Some(AuthMode::OfficialChatGpt),
            label: text("switch.destination.official"),
            profile_id: profile_id.or_else(|| Some(ProfileId::new(PROFILE_OFFICIAL))),
            writes_local: true,
            ..Dest::default()
        }
    }

    fn restore_original(
        &self,
        request: &SwitchRequest,
        provider: Option<ProviderRestore>,
        baseline: Option<&superba_core::Baseline>,
        uninstall: bool,
        draft: &mut Draft,
    ) -> Dest {
        let Some(baseline) = baseline else {
            draft.block(BlockReason::NoBaseline);
            return Dest {
                label: text("switch.destination.original"),
                profile_id: Some(ProfileId::new(PROFILE_ORIGINAL)),
                ..Dest::default()
            };
        };
        let baseline_provider = baseline
            .provider_key
            .clone()
            .filter(|p| p != PROVIDER_OPENAI);
        let mut question = None;
        let provider = match (provider, &baseline_provider) {
            (Some(p), _) => p,
            (None, None) => ProviderRestore::KeepOpenai,
            (None, Some(bp)) => {
                question = Some(bp.clone());
                ProviderRestore::KeepOpenai
            }
        };
        let migration = match (provider, &baseline_provider) {
            (
                ProviderRestore::Baseline {
                    reverse_migrate: true,
                },
                Some(bp),
            ) => Some(MigrationRequest {
                to_provider: bp.clone(),
                // Resolved into ledger and generation ranges by `plan_migration`.
                selectors: Vec::new(),
                include_archived: true,
                exclude: request.migration_exclude.clone(),
            }),
            _ => None,
        };
        let _ = uninstall;
        Dest {
            resolved: Some(ResolvedProfile {
                model_catalog: None,
                profile_id: Some(ProfileId::new(PROFILE_ORIGINAL)),
                site_id: None,
                line_id: None,
                mode: ResolvedMode::Original { provider },
                model: Default::default(),
                key_resolutions: request.key_resolutions.clone(),
                settings: Default::default(),
            }),
            auth: baseline.auth.mode(),
            label: text("switch.destination.original"),
            profile_id: Some(ProfileId::new(PROFILE_ORIGINAL)),
            writes_local: true,
            env_op: Some(EnvOp::RestoreBaseline {
                snapshots: baseline.env.clone(),
                legacy_lines: baseline.legacy_env_lines.clone(),
            }),
            migration,
            restore_provider_question: question,
            ..Dest::default()
        }
    }

    fn unknown_versions(&self, unknown: &[String], request: &SwitchRequest, draft: &mut Draft) {
        for consumer in unknown {
            if request.accept_unknown_versions {
                draft.info(text("switch.warn.unknown_version_accepted").arg("consumer", consumer));
            } else {
                draft.choices.push(PlanChoice::UnknownVersion {
                    consumer: consumer.clone(),
                });
            }
        }
    }

    /// Plan 10.3: switching away from a legacy provider hides its sessions unless
    /// they are migrated; the user must choose.
    fn legacy_sessions(
        &self,
        request: &SwitchRequest,
        state: &ConfigState,
        report: Option<&SessionReport>,
        dest: &mut Dest,
        draft: &mut Draft,
    ) {
        let leaves_provider = matches!(
            dest.resolved.as_ref().map(|r| &r.mode),
            Some(ResolvedMode::Token { .. } | ResolvedMode::Official)
        ) || matches!(
            dest.resolved.as_ref().map(|r| &r.mode),
            Some(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai
            })
        );
        let Some(current) = state
            .model_provider
            .clone()
            .filter(|p| p != PROVIDER_OPENAI)
        else {
            return;
        };
        if !leaves_provider || dest.migration.is_some() {
            return;
        }
        let count = report
            .and_then(|r| r.by_provider.get(&current))
            .map_or(0, |p| p.distinct.max(p.threads));
        if count == 0 {
            return;
        }
        match request.legacy_sessions {
            None => draft.choices.push(PlanChoice::LegacySessions {
                providers: BTreeMap::from([(current, count)]),
            }),
            Some(superba_core::LegacySessionChoice::Migrate) => {
                dest.migration = Some(MigrationRequest {
                    to_provider: PROVIDER_OPENAI.to_owned(),
                    selectors: vec![MigrationSelector::Provider { provider: current }],
                    include_archived: true,
                    exclude: request.migration_exclude.clone(),
                });
            }
            Some(superba_core::LegacySessionChoice::Skip) => draft.warn(
                text("switch.warn.legacy_skipped")
                    .arg("count", count)
                    .arg("provider", current),
            ),
        }
    }

    async fn plan_migration(
        &self,
        home: &CodexHome,
        req: &MigrationRequest,
        draft: &mut Draft,
    ) -> Option<MigrationPlan> {
        let d = &self.deps;
        let mut req = req.clone();
        if req.selectors.is_empty() {
            // Reverse migration (plan 10.4 option 2): ledger of the takeover
            // migrations, plus sessions created during Krill generations.
            let history = soft(draft, "migration history", d.sessions.history(home).await)
                .unwrap_or_default();
            for r in history.iter().filter(|r| {
                !r.rolled_back
                    && r.to_provider == PROVIDER_OPENAI
                    && r.by_source_provider.contains_key(&req.to_provider)
            }) {
                req.selectors.push(MigrationSelector::Ledger {
                    migration_id: r.id.clone(),
                });
            }
            let generations =
                soft(draft, "generations", d.config.generations(home).await).unwrap_or_default();
            for (i, g) in generations.iter().enumerate() {
                if g.site_kind == Some(SiteKind::Krill) {
                    req.selectors.push(MigrationSelector::CreatedBetween {
                        provider: PROVIDER_OPENAI.to_owned(),
                        from: g.applied_at,
                        to: generations.get(i + 1).map(|n| n.applied_at),
                    });
                }
            }
            if req.selectors.is_empty() {
                return None;
            }
        }
        match d.sessions.plan(home, &req).await {
            Ok(p) if p.thread_count == 0 && p.rollout_count == 0 => None,
            Ok(p) => {
                let from: Vec<&str> = p.by_source_provider.keys().map(String::as_str).collect();
                draft.warn(
                    text("switch.warn.migration")
                        // One item per session; `thread_count` counts rows in every
                        // thread table (threads + catalogs), not sessions.
                        .arg(
                            "count",
                            if p.items.is_empty() {
                                p.thread_count.max(p.rollout_count)
                            } else {
                                u32::try_from(p.items.len()).unwrap_or(u32::MAX)
                            },
                        )
                        .arg(
                            "from",
                            if from.is_empty() {
                                LEGACY_KRILL_PROVIDER.to_owned()
                            } else {
                                from.join(", ")
                            },
                        )
                        .arg("to", &p.request.to_provider),
                );
                Some(p)
            }
            Err(e) => {
                draft.skipped("sessions", &e);
                draft.block(BlockReason::UnsupportedTarget {
                    detail: format!("session migration: {e}"),
                });
                None
            }
        }
    }

    /// `left_to_config`: the model Codex runs with when the destination
    /// profile sets none ([`model_left_to_config`]).
    async fn check_model(
        &self,
        request: &SwitchRequest,
        left_to_config: Option<String>,
        dest: &mut Dest,
        draft: &mut Draft,
    ) -> Option<ModelCheck> {
        if dest.routed {
            // The router maps Codex's model name per member and serves the
            // members' own catalogs: `config.toml` keeps the Codex name, so a
            // failover never sends one member's name to another.
            return None;
        }
        let site = dest.site.as_ref()?;
        let (endpoints, key) = (dest.endpoints.as_ref()?, dest.key.as_ref()?);
        let resolved = dest.resolved.as_mut()?;
        let from_config = resolved.model.model.is_none();
        let configured = resolved.model.model.clone().or(left_to_config)?;
        // Direct mode cannot rewrite requests: a model the site serves under
        // another name is written under that name.
        let current = site.site_model(&configured).to_owned();
        if from_config && current == configured && site.model_map.values().any(|m| m == &current) {
            // config.toml already holds the site's name (written by an earlier
            // switch to this site): keep writing it. Left unset, the owned
            // `model` key would go back to the user's own value, the name the
            // site does not serve.
            resolved.model.model = Some(current.clone());
        } else if current != configured {
            resolved.model.model = Some(current.clone());
            draft.info(
                text("switch.warn.model_mapped")
                    .arg_text("site", site.name.clone())
                    .arg("model", &configured)
                    .arg("mapped", &current),
            );
        }
        if !site.capabilities.model_catalog {
            return None;
        }
        let models = match self
            .deps
            .prober
            .list_models(&endpoints.openai_base_url, key)
            .await
        {
            Ok(m) if !m.is_empty() => m,
            Ok(_) => return None,
            Err(e) => {
                draft.skipped("models", &e);
                return None;
            }
        };
        if models.iter().any(|m| m.id == current) {
            return Some(ModelCheck {
                current: Some(current),
                available: true,
                suggestions: Vec::new(),
            });
        }
        let suggestions = suggest_models(&current, models.iter().map(|m| m.id.as_str()));
        match (request.model_override.as_ref(), suggestions.first()) {
            (None, Some(replacement)) => {
                resolved.model.model = Some(replacement.clone());
                draft.info(
                    text("switch.warn.model_missing")
                        .arg_text("site", site.name.clone())
                        .arg("model", &current)
                        .arg("replacement", replacement),
                );
            }
            _ => draft.choices.push(PlanChoice::ReplacementModel {
                current: current.clone(),
                suggestions: suggestions.clone(),
            }),
        }
        Some(ModelCheck {
            current: Some(current),
            available: false,
            suggestions,
        })
    }

    /// Restores are resolved by the config engine. Use their rendered auth and
    /// endpoints before deciding how to restart, including the saved key for PAT
    /// health checks. Never infer a key's site from the running app's environment.
    async fn resolve_rendered_auth(
        &self,
        home: &CodexHome,
        state: &ConfigState,
        sites: &[Site],
        plan: &ApplyPlan,
        dest: &mut Dest,
    ) -> Result<()> {
        if !matches!(
            dest.resolved.as_ref().map(|r| &r.mode),
            Some(ResolvedMode::Generation { .. } | ResolvedMode::Original { .. })
        ) {
            return Ok(());
        }
        let change = |kind| {
            plan.files
                .iter()
                .find(|f| f.kind == kind)
                .map(|f| &f.change)
        };
        let auth_change = change(CodexFileKind::AuthJson);
        let rendered_auth = match auth_change {
            Some(FileChange::Write { contents, .. }) => {
                superba_config::authjson::shape(Some(contents.expose())).mode()
            }
            Some(FileChange::Remove) => None,
            _ => state.auth.mode(),
        };
        dest.auth = plan
            .auth_mode
            .or_else(|| plan.auth_change.as_ref().and_then(|c| c.after.mode()))
            .or(rendered_auth)
            .or(dest.auth);
        let Some(auth) = dest.auth.filter(|a| a.uses_key()) else {
            return Ok(());
        };
        dest.key = match auth_change {
            Some(FileChange::Write { contents, .. }) => {
                superba_config::authjson::token_credentials(contents.expose()).map(|(_, key)| key)
            }
            Some(FileChange::Remove) => None,
            _ => self
                .deps
                .config
                .active_credentials(home)
                .await?
                .map(|c| c.key),
        };
        let rendered_state = match change(CodexFileKind::ConfigToml) {
            Some(FileChange::Write { contents, .. }) => self.deps.config.inspect_files(
                home.root(),
                &CodexFiles {
                    config_toml: Some(contents.clone()),
                    auth_json: None,
                },
            )?,
            Some(FileChange::Remove) => self
                .deps
                .config
                .inspect_files(home.root(), &CodexFiles::default())?,
            _ => state.clone(),
        };
        let Some(base) = rendered_state
            .openai_base_url
            .as_deref()
            .and_then(|s| url::Url::parse(s).ok())
        else {
            return Ok(());
        };
        let mut endpoints = ResolvedEndpoints::for_base_url(&base)?;
        endpoints.chatgpt_base_url = rendered_state
            .chatgpt_base_url
            .as_deref()
            .and_then(|s| url::Url::parse(s).ok());
        if auth.needs_authapi_env() {
            endpoints.authapi_origin = endpoints
                .chatgpt_base_url
                .as_ref()
                .and_then(|u| Origin::parse(&u.origin().ascii_serialization()).ok())
                .or_else(|| Some(endpoints.origin.clone()));
        }
        // Only a configured site that owns the restored API origin may receive
        // this key. The prober also checks the launch-time auth origin against it.
        if let Some((site, line)) = sites.iter().find_map(|site| {
            site.supports(auth)
                .then(|| site.lines.iter().find(|l| l.origin == endpoints.origin))
                .flatten()
                .map(|line| (site, line))
        }) {
            dest.site = Some(site.clone());
            dest.line = Some(line.clone());
        }
        if matches!(
            dest.resolved.as_ref().map(|r| &r.mode),
            Some(ResolvedMode::Generation { .. })
        ) && let Some(origin) = &endpoints.authapi_origin
        {
            dest.env_op = Some(EnvOp::Set {
                value: origin.as_str(),
                take_over_foreign: false,
            });
        }
        dest.endpoints = Some(endpoints);
        Ok(())
    }

    async fn authapi_health(&self, dest: &Dest, origin: &str) -> AuthApiHealth {
        let (Some(site), Some(key)) = (dest.site.as_ref(), dest.key.as_ref()) else {
            return AuthApiHealth::Unhealthy {
                reason: "no key to check the auth address with".into(),
            };
        };
        let origin = match Origin::parse(origin) {
            Ok(o) => o,
            Err(e) => {
                return AuthApiHealth::Unhealthy {
                    reason: e.to_string(),
                };
            }
        };
        match self.deps.prober.check_authapi(site, &origin, key).await {
            Ok(h) => h,
            Err(e) => AuthApiHealth::Unhealthy {
                reason: e.to_string(),
            },
        }
    }

    async fn plan_env(
        &self,
        op: EnvOp,
        request: &SwitchRequest,
        capture_baseline: bool,
        extras: &mut BaselineExtras,
        draft: &mut Draft,
    ) -> Result<Option<EnvPlan>> {
        let d = &self.deps;
        let report = soft(
            draft,
            "environment",
            d.env.inspect(ENV_AUTHAPI_BASE_URL).await,
        );
        if let Some(r) = &report {
            if capture_baseline {
                extras.env = snapshots(r);
                extras.legacy_env_lines = r.legacy_lines.clone();
            }
            if matches!(op, EnvOp::Set { .. })
                && !r.foreign_lines.is_empty()
                && request.take_over_foreign_env.is_none()
            {
                draft.choices.push(PlanChoice::ForeignEnv {
                    lines: r.foreign_lines.clone(),
                });
            }
        }
        match d.env.plan(ENV_AUTHAPI_BASE_URL, op).await {
            Ok(p) if p.is_noop() => Ok(None),
            Ok(p) => Ok(Some(p)),
            Err(e) if e.is_not_implemented() => {
                draft.skipped("environment", &e);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn plan_remotes(
        &self,
        request: &SwitchRequest,
        followed: &[TargetId],
        dest: &Dest,
        home: &CodexHome,
        session: Option<&AppSession>,
        processes: &[ProcessInfo],
        caps: &[VersionCapability],
        active_threads: &mut Vec<ThreadActivity>,
        draft: &mut Draft,
    ) -> (
        Vec<RemoteHost>,
        Vec<RemotePlan>,
        Vec<(TargetId, RestartKind)>,
    ) {
        let d = &self.deps;
        let mut wanted = request.targets.remotes.clone();
        if let Some(t) = &dest.rollback_remote
            && !wanted.contains(t)
        {
            wanted.push(t.clone());
        }
        if wanted.is_empty() {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let live: Vec<ProcessInfo> = processes
            .iter()
            .filter(|p| matches!(p.kind, superba_core::CodexProcessKind::RemoteLink { .. }))
            .cloned()
            .collect();
        let from_app = match session.filter(|s| s.cdp.is_some()) {
            Some(s) => {
                soft(draft, "remote hosts", d.app.remote_connections(s).await).unwrap_or_default()
            }
            None => Vec::new(),
        };
        // Only hosts the desktop app knows; ssh config entries alone are never
        // switched (v0.3).
        let hosts: Vec<RemoteHost> = soft(
            draft,
            "remote hosts",
            d.remote.discover(home, &from_app, &live).await,
        )
        .unwrap_or_default()
        .into_iter()
        .filter(RemoteHost::is_app_known)
        .collect();

        let mut plans = Vec::new();
        let mut restarts = Vec::new();
        let mut shared = Vec::new();
        let mut used_hosts = Vec::new();
        for tid in wanted {
            let unavailable = |detail: String| RestartKind::Blocked {
                reasons: vec![BlockReason::UnsupportedTarget { detail }],
            };
            let Some(host) = hosts.iter().find(|h| h.target_id == tid).cloned() else {
                if followed.contains(&tid) {
                    // A followed host the app no longer knows is skipped quietly.
                    draft.info(
                        text("switch.hosts.follow_missing")
                            .arg("host", crate::messages::target_label(tid.as_str())),
                    );
                    continue;
                }
                let detail = "host not found".to_owned();
                draft.warn(
                    text("switch.warn.remote_unavailable")
                        .arg("host", crate::messages::target_label(tid.as_str()))
                        .arg("detail", &detail),
                );
                restarts.push((tid, unavailable(detail)));
                continue;
            };
            used_hosts.push(host.clone());
            if dest.rollback_remote.as_ref() == Some(&tid) {
                restarts.push((tid, decide_remote(host.host_id.as_deref(), &[])));
                continue;
            }
            let state = match d.remote.probe(&host).await {
                Ok(s) => s,
                Err(e) => {
                    draft.warn(
                        text("switch.warn.remote_unavailable")
                            .arg("host", &host.label)
                            .arg("detail", e.to_string()),
                    );
                    restarts.push((tid, unavailable(e.to_string())));
                    continue;
                }
            };
            let gate = rules::gate_versions(
                &[VersionedConsumer {
                    label: host.label.clone(),
                    role: ConsumerRole::Remote,
                    version: state.codex_version.clone(),
                }],
                caps,
            );
            self.unknown_versions(&gate.unknown, request, draft);
            if state.shared_home_suspected() {
                shared.push(tid.clone());
            }
            // Remote hosts are never routed through the local router.
            let Some(base) = dest.remote_resolved.as_ref().or(dest.resolved.as_ref()) else {
                restarts.push((tid, unavailable("nothing to apply".into())));
                continue;
            };
            let (resolved, scope) = remote_resolution(base, followed.contains(&tid));
            match d.remote.plan_apply_scoped(&host, &resolved, scope).await {
                Ok(mut p) => {
                    for b in gate.blocks {
                        if !p.blocked.contains(&b) {
                            p.blocked.push(b);
                        }
                    }
                    if host.host_id.is_none() && p.blocked.is_empty() {
                        draft.warn(text("switch.warn.remote_no_host_id").arg("host", &host.label));
                    }
                    if let Some(a) = soft(
                        draft,
                        "remote conversations",
                        d.remote.turn_activity(&host).await,
                    ) {
                        active_threads.extend(a.busy);
                    }
                    restarts.push((
                        tid,
                        rules::decide_remote_probed(
                            host.host_id.as_deref(),
                            &p.blocked,
                            &state.app_server_pids,
                        ),
                    ));
                    draft.warnings.extend(p.warnings.iter().cloned());
                    plans.push(p);
                }
                Err(e) => {
                    draft.warn(
                        text("switch.warn.remote_unavailable")
                            .arg("host", &host.label)
                            .arg("detail", e.to_string()),
                    );
                    restarts.push((tid, unavailable(e.to_string())));
                }
            }
        }
        if !shared.is_empty() && !request.accept_shared_home {
            draft
                .choices
                .push(PlanChoice::SharedHome { targets: shared });
        }
        (used_hosts, plans, restarts)
    }

    fn consumer_warnings(
        &self,
        restarts: &[superba_core::ConsumerRestart],
        processes: &[ProcessInfo],
        dest: &Dest,
        draft: &mut Draft,
    ) {
        for r in restarts {
            match r.consumer {
                superba_core::ConsumerKind::CliProcesses
                | superba_core::ConsumerKind::IdeAppServers
                    if !r.pids.is_empty() =>
                {
                    draft.info(text("switch.warn.cli_processes").arg("count", r.pids.len()));
                }
                superba_core::ConsumerKind::CliDaemon => {
                    draft.info(text("switch.warn.daemon_running"));
                }
                _ => {}
            }
        }
        if dest.auth.is_some_and(AuthMode::needs_authapi_env) {
            let missing = processes
                .iter()
                .filter(|p| {
                    matches!(
                        rules::consumer_group(&p.kind),
                        Some(
                            superba_core::ConsumerKind::CliProcesses
                                | superba_core::ConsumerKind::IdeAppServers
                                | superba_core::ConsumerKind::CliDaemon
                        )
                    )
                })
                .filter(|p| matches!(p.authapi_env, EnvObservation::Absent))
                .count();
            if missing > 0 {
                draft.warn(text("switch.warn.missing_env_processes").arg("count", missing));
            }
        }
    }

    fn restart_notes(
        &self,
        primary: &RestartKind,
        dest: &Dest,
        sites: &[Site],
        authapi_env: &EnvObservation,
        draft: &mut Draft,
    ) {
        match primary {
            RestartKind::AppServerHot { .. }
                if dest.auth.is_some_and(AuthMode::needs_authapi_env) =>
            {
                if let Some(origin) = authapi_env.value() {
                    let line = sites
                        .iter()
                        .find_map(|s| rules::line_for_origin(s, origin))
                        .map(|l| l.name.clone())
                        .unwrap_or_else(|| LocalizedText::new("raw", origin));
                    // Only worth a line when it differs from the destination line.
                    if dest.line.as_ref().is_none_or(|l| l.name != line) {
                        draft.info(text("switch.warn.authapi_line").arg_text("line", line));
                    }
                }
            }
            RestartKind::FullAppRestart {
                reason: superba_core::FullRestartReason::NoCdp,
            } => draft.info(text("switch.warn.app_not_ours")),
            _ => {}
        }
    }

    fn alternatives(
        &self,
        request: &SwitchRequest,
        primary: &RestartKind,
        dest: &Dest,
        draft: &mut Draft,
    ) {
        let SwitchIntent::Profile {
            profile_id,
            line_id,
            ..
        } = &request.intent
        else {
            return;
        };
        if dest.auth != Some(AuthMode::Pat) {
            return;
        }
        let apikey_ok = dest
            .caps
            .as_ref()
            .is_none_or(|c| c.allows(AuthMode::ApiKey));
        let wants_alt = match primary {
            RestartKind::FullAppRestart { reason } => matches!(
                reason,
                superba_core::FullRestartReason::AuthApiUnhealthy
                    | superba_core::FullRestartReason::AuthApiMissing
            ),
            RestartKind::Blocked { reasons } => reasons.iter().any(|r| {
                matches!(
                    r,
                    BlockReason::AuthModeUnavailable {
                        auth_mode: AuthMode::Pat,
                        ..
                    }
                )
            }),
            _ => false,
        };
        if wants_alt && apikey_ok {
            let mut alt = request.clone();
            alt.intent = SwitchIntent::Profile {
                profile_id: profile_id.clone(),
                line_id: line_id.clone(),
                auth_mode: Some(AuthMode::ApiKey),
            };
            draft.alternatives.push(PlanAlternative {
                label: text("switch.action.hot_with_apikey"),
                request: alt,
            });
        }
    }
}

/// What a remote host gets from a local destination (v0.3): a token or official
/// switch changes only its connection (routing and auth keys, always direct; no
/// settings, model catalog, speed tier or context keys). A restore changes only
/// the connection of a host that merely follows this computer (`followed`), so
/// the settings synced there stay; a host the user selected gets both key
/// families restored.
pub(crate) fn remote_resolution(
    base: &ResolvedProfile,
    followed: bool,
) -> (ResolvedProfile, superba_core::RemoteApplyScope) {
    let mut resolved = base.clone();
    match resolved.mode {
        ResolvedMode::Token { .. } | ResolvedMode::Official => {
            resolved.settings = Default::default();
            resolved.model_catalog = None;
            resolved.model.speed = None;
            resolved.model.context_window = None;
            resolved.model.auto_compact = None;
            (resolved, superba_core::RemoteApplyScope::Connection)
        }
        _ if followed => (resolved, superba_core::RemoteApplyScope::Connection),
        _ => (resolved, superba_core::RemoteApplyScope::Both),
    }
}

/// Persisted values that existed before Superba (baseline env snapshots).
fn snapshots(r: &superba_core::EnvReport) -> Vec<EnvSnapshot> {
    r.persisted
        .iter()
        .filter(|p| !p.managed)
        .map(|p| EnvSnapshot {
            location: p.location.clone(),
            value: p.value.clone(),
        })
        .collect()
}

/// Up to three replacement models: same family prefix first (text before the
/// last `-`), then the catalog order.
pub fn suggest_models<'a>(current: &str, catalog: impl Iterator<Item = &'a str>) -> Vec<String> {
    let all: Vec<&str> = catalog.collect();
    let family = current.rsplit_once('-').map_or(current, |(f, _)| f);
    let mut out: Vec<String> = all
        .iter()
        .filter(|m| m.starts_with(family))
        .map(|m| (*m).to_owned())
        .collect();
    for m in &all {
        if out.len() >= 3 {
            break;
        }
        if !out.iter().any(|o| o == m) {
            out.push((*m).to_owned());
        }
    }
    out.truncate(3);
    out
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn root_and_safe_mode_always_disable_the_debugging_port() {
        for (safe_mode, is_root, expected) in [
            (false, false, CdpMode::Auto),
            (true, false, CdpMode::Disabled),
            (false, true, CdpMode::Disabled),
            (true, true, CdpMode::Disabled),
        ] {
            assert_eq!(launch_cdp_mode(safe_mode, is_root), expected);
        }
    }
}
