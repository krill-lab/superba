//! Pure decision logic of the planner: the plan 7.1 restart table, the plan 9.8
//! version gates, per-consumer restart decisions and staleness of running
//! processes. No IO: the planner gathers a snapshot of facts and calls these
//! functions, so every rule is unit-tested in isolation.

use std::path::Path;

use superba_core::{
    AuthApiHealth, AuthFamily, AuthMode, BlockReason, CodexProcessKind, CodexVersion, ConsumerKind,
    ConsumerRestart, EnvObservation, FullRestartReason, Line, Origin, ProcessInfo, RestartKind,
    Site, SiteKind, TargetId, Timestamp, Verification,
    codex::{LOCAL_HOST_ID, VersionCapability},
    plan::CheckResult,
};

// ---------------------------------------------------------------------------
// Version gates (plan 9.8)
// ---------------------------------------------------------------------------

/// Minimum Codex versions per capability.
///
/// `WhamAccountsCheck` (0.160) is a verified fact (relay `accounts_check.rs`). The
/// other two rows are provisional until spike S9 pins them from the Codex sources:
/// PAT support shipped together with the `wham/accounts/check` call, and top-level
/// `openai_base_url` is required since custom providers stopped reading auth.json
/// (0.149). The table only ever blocks consumers older than these values.
pub const VERSION_TABLE: &[(VersionCapability, CodexVersion)] = &[
    (
        VersionCapability::TopLevelOpenaiBaseUrl,
        CodexVersion::new(0, 149, 0),
    ),
    (
        VersionCapability::PersonalAccessToken,
        CodexVersion::new(0, 160, 0),
    ),
    (
        VersionCapability::WhamAccountsCheck,
        CodexVersion::new(0, 160, 0),
    ),
];

/// Minimum version for a capability (`0.0.0` for capabilities not in the table).
pub fn min_version(cap: VersionCapability) -> CodexVersion {
    VERSION_TABLE
        .iter()
        .find(|(c, _)| *c == cap)
        .map(|(_, v)| v.clone())
        .unwrap_or(CodexVersion::new(0, 0, 0))
}

/// Capabilities a destination needs from every consumer of the target.
///
/// Official login (or an unknown restore destination) needs nothing; any token mode
/// on a relay needs top-level `openai_base_url` (else the key would go to
/// api.openai.com); PAT additionally needs PAT support and `wham/accounts/check`.
pub fn required_capabilities(
    site_kind: Option<SiteKind>,
    auth: Option<AuthMode>,
) -> Vec<VersionCapability> {
    let Some(auth) = auth else {
        return Vec::new();
    };
    if !auth.uses_key() || matches!(site_kind, Some(SiteKind::Official)) {
        return Vec::new();
    }
    let mut caps = vec![VersionCapability::TopLevelOpenaiBaseUrl];
    if auth.needs_authapi_env() || auth.writes_chatgpt_base_url() {
        caps.push(VersionCapability::PersonalAccessToken);
        caps.push(VersionCapability::WhamAccountsCheck);
    }
    caps
}

/// Role of a consumer in the version gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerRole {
    /// The desktop app's bundled Codex (too old = blocked).
    App,
    /// A CLI binary first on PATH (too old = blocked unless the user accepts
    /// switching for the app only).
    Cli,
    /// A remote host's Codex (too old = that host is blocked).
    Remote,
}

/// A consumer and its Codex version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionedConsumer {
    /// Label shown to the user.
    pub label: String,
    /// Role.
    pub role: ConsumerRole,
    /// Version, `None` = unknown.
    pub version: Option<CodexVersion>,
}

/// Result of [`gate_versions`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionGate {
    /// App / remote consumers below the table: the switch (or host) is blocked.
    pub blocks: Vec<BlockReason>,
    /// CLI consumers below the table (blocked unless accepted; plan 7.7 "switch
    /// anyway, only for the app").
    pub cli_too_old: Vec<BlockReason>,
    /// Consumers whose version is unknown (needs the user's confirmation).
    pub unknown: Vec<String>,
}

/// Apply the version table to `consumers` for the capabilities `caps`.
pub fn gate_versions(consumers: &[VersionedConsumer], caps: &[VersionCapability]) -> VersionGate {
    let mut gate = VersionGate::default();
    if caps.is_empty() {
        return gate;
    }
    for c in consumers {
        let Some(found) = &c.version else {
            if !gate.unknown.contains(&c.label) {
                gate.unknown.push(c.label.clone());
            }
            continue;
        };
        // Report the strictest failing capability once per consumer.
        let failing = caps
            .iter()
            .map(|cap| (*cap, min_version(*cap)))
            .filter(|(_, min)| !found.meets(min))
            .max_by(|a, b| a.1.cmp(&b.1));
        if let Some((capability, required)) = failing {
            let reason = BlockReason::VersionTooOld {
                consumer: c.label.clone(),
                found: found.clone(),
                required,
                capability,
            };
            match c.role {
                ConsumerRole::App | ConsumerRole::Remote => gate.blocks.push(reason),
                ConsumerRole::Cli => gate.cli_too_old.push(reason),
            }
        }
    }
    gate
}

/// The local consumers of the version gate: the desktop app's bundled Codex (the
/// install `install_id`, else the first one found) and the CLI first on the PATH.
pub fn local_versioned_consumers(
    install_id: Option<&str>,
    installs: &[superba_core::AppInstall],
    binaries: &[superba_core::CodexBinary],
    app_label: &str,
) -> Vec<VersionedConsumer> {
    let mut consumers = Vec::new();
    if let Some(install) = install_id
        .and_then(|id| installs.iter().find(|i| i.id == id))
        .or_else(|| installs.first())
    {
        consumers.push(VersionedConsumer {
            label: app_label.to_owned(),
            role: ConsumerRole::App,
            version: install.bundled_codex_version.clone(),
        });
    }
    if let Some(cli) = binaries.iter().find(|b| b.path_rank == Some(0)) {
        consumers.push(VersionedConsumer {
            label: superba_core::paths::display_path(&cli.path),
            role: ConsumerRole::Cli,
            version: cli.version.clone(),
        });
    }
    consumers
}

/// Whether every Codex in `consumers` can use a personal access token, for
/// [`superba_core::AuthContext::pat_version_ok`]: `Some(false)` as soon as one
/// known version lacks a PAT-only capability (Automatic authentication then uses an
/// API key), `Some(true)` when every version is known and new enough, `None` when
/// nothing is known (no consumer, or an unknown version: the version gate asks).
pub fn pat_version_ok(consumers: &[VersionedConsumer]) -> Option<bool> {
    let pat_only = [
        VersionCapability::PersonalAccessToken,
        VersionCapability::WhamAccountsCheck,
    ];
    let mut all_known = !consumers.is_empty();
    for c in consumers {
        match &c.version {
            Some(v) if pat_only.iter().any(|cap| !v.meets(&min_version(*cap))) => {
                return Some(false);
            }
            Some(_) => {}
            None => all_known = false,
        }
    }
    all_known.then_some(true)
}

// ---------------------------------------------------------------------------
// Restart decision (plan 7.1)
// ---------------------------------------------------------------------------

/// What the planner knows about the running desktop app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppFacts {
    /// No app main process.
    NotRunning,
    /// Running.
    Running {
        /// Launched with a verified loopback CDP port.
        cdp: bool,
        /// `electronBridge.sendMessageFromView` is callable.
        bridge_ready: bool,
        /// `CODEX_AUTHAPI_BASE_URL` in the app main process (inherited by every
        /// hot-restarted app-server).
        authapi_env: EnvObservation,
        /// whoami with the destination key on that origin; `None` = not checked
        /// (treated as unhealthy: a hot switch must not risk the login).
        authapi_health: Option<AuthApiHealth>,
    },
}

impl AppFacts {
    /// True if the app runs.
    pub fn is_running(&self) -> bool {
        matches!(self, AppFacts::Running { .. })
    }
}

/// Snapshot of facts for the local target (input of [`decide_local`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFacts {
    /// Fatal problems found while planning (versions, line, key, journal, files).
    pub blocks: Vec<BlockReason>,
    /// Sessions must be migrated (takeover of a legacy provider or reverse migration).
    pub migration: bool,
    /// Auth family of the current auth.json (`None` = missing / unknown).
    pub current_family: Option<AuthFamily>,
    /// Auth family of the destination (`None` = unknown up front, e.g. generation restore).
    pub target_family: Option<AuthFamily>,
    /// The destination needs `CODEX_AUTHAPI_BASE_URL` in the app-server (PAT).
    pub target_needs_authapi: bool,
    /// Desktop app.
    pub app: AppFacts,
    /// This platform build can hot-restart the app-server at all.
    pub hot_supported: bool,
}

/// True if this platform build has a hot-switch implementation. Linux is verified;
/// macOS (spike S2) and Windows (S3) use the same renderer IPC once the app was
/// launched with CDP, and fall back to a full restart through `NoCdp`/`NoBridge`
/// when the launch could not expose it. Other systems have no app build.
pub const fn platform_hot_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))
}

/// The plan 7.1 decision table for the local desktop app. First match wins:
///
/// 1. blocked -> `Blocked`
/// 2. session migration -> `FullAppRestart(SessionMigration)` (every Codex process,
///    CLI included, must stop, so this holds even when the app is not running)
/// 3. auth family changes while the app runs -> `FullAppRestart(CrossAuthFamily)`
///    (with the app stopped no process can race on auth.json, so rule 4 applies)
/// 4. app not running -> `None`
/// 5. no hot switch on this platform / no CDP / no bridge ->
///    `FullAppRestart(PlatformNoHot | NoCdp | NoBridge)`
/// 6. PAT destination, app env lacks `CODEX_AUTHAPI_BASE_URL` (or it cannot be
///    read) -> `FullAppRestart(AuthApiMissing)`
/// 7. PAT destination, whoami on that origin unhealthy or unchecked ->
///    `FullAppRestart(AuthApiUnhealthy)`
/// 8. otherwise -> `AppServerHot { host_id: "local", kill_remote: false }`
pub fn decide_local(f: &LocalFacts) -> RestartKind {
    if !f.blocks.is_empty() {
        return RestartKind::Blocked {
            reasons: f.blocks.clone(),
        };
    }
    let full = |reason| RestartKind::FullAppRestart { reason };
    if f.migration {
        return full(FullRestartReason::SessionMigration);
    }
    let AppFacts::Running {
        cdp,
        bridge_ready,
        authapi_env,
        authapi_health,
    } = &f.app
    else {
        return RestartKind::None;
    };
    if let (Some(from), Some(to)) = (f.current_family, f.target_family)
        && from != to
    {
        return full(FullRestartReason::CrossAuthFamily);
    }
    if !f.hot_supported {
        return full(FullRestartReason::PlatformNoHot);
    }
    if !cdp {
        return full(FullRestartReason::NoCdp);
    }
    if !bridge_ready {
        return full(FullRestartReason::NoBridge);
    }
    if f.target_needs_authapi {
        if authapi_env.value().is_none_or(|v| v.trim().is_empty()) {
            return full(FullRestartReason::AuthApiMissing);
        }
        if !matches!(authapi_health, Some(AuthApiHealth::Healthy)) {
            return full(FullRestartReason::AuthApiUnhealthy);
        }
    }
    RestartKind::AppServerHot {
        host_id: LOCAL_HOST_ID.to_owned(),
        kill_remote: false,
    }
}

/// Restart decision for a remote host: blocked when its plan is blocked; a remote
/// hot restart (`killCodexProcess`) when the app knows the host's `hostId`;
/// otherwise the change applies when the host is reconnected.
pub fn decide_remote(host_id: Option<&str>, blocked: &[BlockReason]) -> RestartKind {
    if !blocked.is_empty() {
        return RestartKind::Blocked {
            reasons: blocked.to_vec(),
        };
    }
    match host_id {
        Some(id) if !id.trim().is_empty() => RestartKind::AppServerHot {
            host_id: id.to_owned(),
            kill_remote: true,
        },
        _ => RestartKind::NextLaunch,
    }
}

/// [`decide_remote`] for a host probed just now: with no `codex app-server`
/// running there (the app is not connected to it at the moment) there is
/// nothing to restart or wait for; the host's Codex reads the change when the
/// app next starts it.
pub fn decide_remote_probed(
    host_id: Option<&str>,
    blocked: &[BlockReason],
    app_server_pids: &[u32],
) -> RestartKind {
    match decide_remote(host_id, blocked) {
        RestartKind::AppServerHot { .. } if app_server_pids.is_empty() => RestartKind::NextLaunch,
        other => other,
    }
}

/// Group of a local process for consumer decisions; `None` for the app itself and
/// its ssh links (covered by the app / remote decisions).
pub fn consumer_group(kind: &CodexProcessKind) -> Option<ConsumerKind> {
    match kind {
        CodexProcessKind::AppMain
        | CodexProcessKind::AppServer
        | CodexProcessKind::RemoteLink { .. } => None,
        CodexProcessKind::CliTui | CodexProcessKind::CliExec | CodexProcessKind::Other => {
            Some(ConsumerKind::CliProcesses)
        }
        CodexProcessKind::CliDaemon => Some(ConsumerKind::CliDaemon),
        CodexProcessKind::IdeAppServer => Some(ConsumerKind::IdeAppServers),
        _ => Some(ConsumerKind::CliProcesses),
    }
}

/// True if a process uses `home` (unknown homes count as ours: better one warning
/// too many than a missed consumer).
pub fn uses_home(p: &ProcessInfo, home: &Path) -> bool {
    p.codex_home.as_deref().is_none_or(|h| h == home)
}

/// Restart decisions for every consumer group: the app gets `primary`; CLI TUI /
/// exec and IDE app-servers pick the change up at their next launch (never
/// killed); the daemon needs an explicit restart; remotes get their own decision.
/// Groups without processes are left out (the app is always listed when
/// `include_app`).
pub fn consumer_restarts(
    primary: &RestartKind,
    include_app: bool,
    processes: &[ProcessInfo],
    home: &Path,
    remotes: &[(TargetId, RestartKind)],
) -> Vec<ConsumerRestart> {
    let mut out = Vec::new();
    if include_app {
        out.push(ConsumerRestart {
            consumer: ConsumerKind::DesktopApp,
            restart: primary.clone(),
            pids: processes
                .iter()
                .filter(|p| matches!(p.kind, CodexProcessKind::AppMain) && uses_home(p, home))
                .map(|p| p.pid)
                .collect(),
        });
    }
    for (group, restart) in [
        (ConsumerKind::CliProcesses, RestartKind::NextLaunch),
        (ConsumerKind::CliDaemon, RestartKind::DaemonRestart),
        (ConsumerKind::IdeAppServers, RestartKind::NextLaunch),
    ] {
        let pids: Vec<u32> = processes
            .iter()
            .filter(|p| uses_home(p, home) && consumer_group(&p.kind).as_ref() == Some(&group))
            .map(|p| p.pid)
            .collect();
        if !pids.is_empty() {
            out.push(ConsumerRestart {
                consumer: group,
                restart,
                pids,
            });
        }
    }
    for (target_id, restart) in remotes {
        out.push(ConsumerRestart {
            consumer: ConsumerKind::Remote {
                target_id: target_id.clone(),
            },
            restart: restart.clone(),
            pids: Vec::new(),
        });
    }
    out
}

/// Processes that keep a session migration from running (every Codex consumer of
/// the home except the app, which Superba quits itself, and the daemon, which it
/// stops). Plan 10.2: Superba lists them and never kills them.
pub fn migration_blockers<'a>(processes: &'a [ProcessInfo], home: &Path) -> Vec<&'a ProcessInfo> {
    processes
        .iter()
        .filter(|p| uses_home(p, home))
        .filter(|p| {
            matches!(
                consumer_group(&p.kind),
                Some(ConsumerKind::CliProcesses | ConsumerKind::IdeAppServers)
            )
        })
        .collect()
}

/// Tolerance when comparing a process start time with one of our own stamps.
/// Start times come from the OS in whole seconds and, on Linux, are derived from
/// the boot time plus clock ticks, which drifts from the wall clock (measured:
/// ~0.7 s on a host up for 16 days). Comparisons therefore only conclude
/// "started before" / "started after" outside this window.
pub const PROCESS_START_SLACK_MS: i64 = 5_000;

/// Mark processes started before `applied_at` (the current generation) as stale.
/// Only consumers that read the configuration at start count; processes without a
/// start time, or started within [`PROCESS_START_SLACK_MS`] of the switch (such as
/// the app-server the switch itself restarted), are left alone (no false banner,
/// plan 7.7 "only when certain").
pub fn mark_stale(processes: &mut [ProcessInfo], applied_at: Option<Timestamp>) {
    for p in processes.iter_mut() {
        p.stale = match (applied_at, p.started_at) {
            (Some(applied), Some(started)) => {
                !matches!(
                    p.kind,
                    CodexProcessKind::AppMain | CodexProcessKind::RemoteLink { .. }
                ) && started.as_millis().saturating_add(PROCESS_START_SLACK_MS)
                    < applied.as_millis()
            }
            _ => false,
        };
    }
}

// ---------------------------------------------------------------------------
// Lines and auth API origin
// ---------------------------------------------------------------------------

/// The line of `site` whose origin equals `origin` (an env value such as
/// `https://api.krill-code.net` or with a trailing slash).
pub fn line_for_origin<'a>(site: &'a Site, origin: &str) -> Option<&'a Line> {
    let o = Origin::parse(origin).ok()?;
    site.lines.iter().find(|l| l.origin == o)
}

/// The origin Superba injects as `CODEX_AUTHAPI_BASE_URL` when it launches the app
/// (plan 7.1 note): the destination line when it is a Krill line, else the line of
/// the currently applied Krill profile, else the Krill default line.
pub fn launch_authapi_origin(
    krill: &Site,
    destination: Option<&Line>,
    destination_is_krill: bool,
    active_line: Option<&Line>,
) -> Option<Origin> {
    if destination_is_krill && let Some(l) = destination {
        return Some(l.origin.clone());
    }
    if let Some(l) = active_line
        && krill.lines.iter().any(|k| k.origin == l.origin)
    {
        return Some(l.origin.clone());
    }
    krill.default_line().map(|l| l.origin.clone())
}

/// The Krill-kind site to take the launch-time auth origin from, and the applied
/// profile's line on it: the site of the applied profile when it is Krill-kind
/// (the built-in Krill site or an e2e test site from `SUPERBA_TEST_SITES`), else
/// the built-in Krill site from `sites` (or the catalog) with no active line. Feed
/// the result to [`launch_authapi_origin`].
pub fn active_krill_site(
    sites: &[Site],
    active_site: Option<&superba_core::SiteId>,
    active_line: Option<&superba_core::LineId>,
) -> (Site, Option<Line>) {
    if let Some(site) = active_site
        .and_then(|id| sites.iter().find(|s| &s.id == id))
        .filter(|s| s.kind == SiteKind::Krill)
        && let Some(line) = active_line.and_then(|l| site.line(l))
    {
        return (site.clone(), Some(line.clone()));
    }
    let krill = sites
        .iter()
        .find(|s| s.id == superba_core::catalog::KRILL_SITE_ID)
        .cloned()
        .unwrap_or_else(superba_core::catalog::krill_site);
    (krill, None)
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// One-line English summary of failed checks ("auth: whoami timed out; line: ...").
pub fn verification_summary(v: &Verification) -> String {
    let mut parts = Vec::new();
    for (name, check) in [
        ("process", &v.process),
        ("files", &v.files),
        ("auth", &v.auth),
        ("line", &v.line),
        ("renderer", &v.renderer),
    ] {
        match check {
            CheckResult::Failed { detail } => parts.push(format!("{name}: {detail}")),
            CheckResult::Pending => parts.push(format!("{name}: not run")),
            _ => {}
        }
    }
    if parts.is_empty() {
        "all checks passed".to_owned()
    } else {
        parts.join("; ")
    }
}

/// True for failures worth one automatic retry of the hot restart IPC (the process
/// check: the new app-server did not come up). Auth/line failures are about the
/// configuration and do not improve with another restart.
pub fn retry_ipc_helps(v: &Verification) -> bool {
    matches!(v.process, CheckResult::Failed { .. })
}

/// A full restart of the desktop app ends it by force: on Windows (`windows_app`)
/// an app Superba did not open has no debugging port, so it cannot be asked to
/// quit (closing its windows leaves it running in the notification area). The
/// plan says so up front and the executor does not wait for a graceful quit
/// (user bug 76).
pub fn quit_needs_force(
    windows_app: bool,
    primary: &RestartKind,
    session: Option<&superba_core::AppSession>,
) -> bool {
    windows_app
        && matches!(primary, RestartKind::FullAppRestart { .. })
        && session.is_some_and(|s| s.cdp.is_none())
}
