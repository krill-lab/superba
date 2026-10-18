//! WSL distributions in the switch service (user bug 45).
//!
//! A distribution is a managed host (`superba-remote` reaches it with
//! `wsl.exe`), with three differences from an SSH host:
//!
//! - **It follows this computer by default** ([`superba_core::HostSettings::follows`]),
//!   for profile switches, router on / off, restores and Superba's uninstall,
//!   when it has a Codex (a CLI found by the app's login bash, Codex files, or
//!   the app's WSL app-server running). Turning follow off is remembered.
//! - **A follow writes connection and settings** (the Windows side's rules):
//!   the profile's Codex settings are filtered with the catalog of the
//!   distribution's own Codex version (the version the app's WSL app-server
//!   and the CLI in the distribution run), like a settings sync, and written as
//!   owned keys; a restore restores both families.
//! - **Router mode reaches it** when the network allows: WSL 1 and WSL 2 with
//!   mirrored networking use `127.0.0.1`; WSL 2 behind NAT uses the router's WSL
//!   listener on the Windows host's WSL adapter address (the distribution's
//!   default gateway), which the user turns on once
//!   ([`Switcher::wsl_router_access_impl`]: router configuration plus a Windows
//!   Firewall rule that needs administrator approval). Either way the plan first
//!   asks the router's `health` from inside the distribution; a distribution
//!   that cannot reach it stays direct, with a note naming the fix. Each
//!   distribution authenticates with its own source token (`wsl:<distro>`,
//!   derived from the router token), so request-log rows name it.

use std::net::Ipv4Addr;

use superba_config::settings::{HomeInput, plan_settings};
use superba_core::wsl::{RouterPath, router_source};
use superba_core::wsl::{WslAccessReport, WslOverview};
use superba_core::{
    CodexHome, CodexSettings, ConsumerKind, Error, HostKind, LocalizedText, Notice, Origin,
    ProfileSet, RemoteHost, RemoteState, ResolvedEndpoints, ResolvedMode, ResolvedProfile,
    RestoreTarget, Result, Secret, SettingsConsumer, SwitchIntent, TargetId, WslDistro,
};
use superba_router::config::ROUTER_BASE_PATH;
use superba_router::watchdog::Liveness;

use crate::Switcher;
use crate::messages::text;

/// Router path the router's `health` answers on.
pub(crate) const HEALTH_PATH: &str = "/_superba/v1/health";

/// Intents a WSL distribution follows by default.
pub(crate) fn participates(intent: &SwitchIntent) -> bool {
    matches!(
        intent,
        SwitchIntent::Profile { .. }
            | SwitchIntent::Router { .. }
            | SwitchIntent::Uninstall { .. }
            | SwitchIntent::Restore {
                target: RestoreTarget::Original { .. } | RestoreTarget::Official
            }
    )
}

/// The host follows this computer (its setting, else the default of its kind).
pub(crate) fn follows(set: &ProfileSet, host: &RemoteHost) -> bool {
    set.settings
        .hosts
        .get(&host.target_id)
        .map_or(host.kind == HostKind::Wsl, |h| h.follows(host.kind))
}

/// The user turned following on (not just the default of a WSL distribution).
pub(crate) fn follow_on(set: &ProfileSet, host: &RemoteHost) -> bool {
    set.settings
        .hosts
        .get(&host.target_id)
        .is_some_and(|h| h.follow_switches)
}

/// Intents that also reach a distribution Superba wrote that no longer follows
/// this computer: its own baseline comes back (user bug 54).
pub(crate) fn restores(intent: &SwitchIntent) -> bool {
    matches!(
        intent,
        SwitchIntent::Uninstall { .. }
            | SwitchIntent::Restore {
                target: RestoreTarget::Original { .. }
            }
    )
}

/// The distribution has a Codex that would read its `CODEX_HOME`: a `codex` the
/// app's login bash finds, Codex files, or the app's app-server running there.
pub(crate) fn has_codex(state: &RemoteState) -> bool {
    state.codex_path.is_some()
        || state.config_sha256.is_some()
        || state.auth_sha256.is_some()
        || !state.app_server_pids.is_empty()
}

/// How a distribution reaches the router.
#[derive(Clone, Debug)]
pub(crate) enum WslRoute {
    /// Through the router at `origin` with the distribution's own token.
    Routed {
        /// `http://127.0.0.1:<port>` or `http://<WSL adapter address>:<port>`.
        origin: Origin,
        /// The source token (`wsl:<distro>`).
        token: Secret,
    },
    /// Not reachable: the distribution stays direct.
    Direct {
        /// Why, and what fixes it.
        reason: LocalizedText,
    },
}

/// True when `answer` (an HTTP response read inside the distribution) is the
/// running router's `health`.
pub(crate) fn is_router_health(answer: &str, instance_id: &str) -> bool {
    let ok = answer
        .lines()
        .next()
        .is_some_and(|l| l.starts_with("HTTP/") && l.split_whitespace().nth(1) == Some("200"));
    let body = answer.split("\r\n\r\n").nth(1).unwrap_or_default();
    let parsed: Option<serde_json::Value> = serde_json::from_str(body.trim()).ok();
    ok && parsed.is_some_and(|v| {
        v.get("service").and_then(serde_json::Value::as_str) == Some("superba-router")
            && v.get("instance_id").and_then(serde_json::Value::as_str) == Some(instance_id)
    })
}

/// The routed resolution of a distribution: the local routed `base` (router
/// endpoints, API key mode) pointed at `origin` with `token`.
pub(crate) fn routed(base: &ResolvedProfile, origin: &Origin, token: &Secret) -> ResolvedProfile {
    let mut r = base.clone();
    if let ResolvedMode::Token { endpoints, key, .. } = &mut r.mode {
        *endpoints = ResolvedEndpoints {
            origin: origin.clone(),
            openai_base_url: origin.join(ROUTER_BASE_PATH),
            chatgpt_base_url: None,
            authapi_origin: None,
            codex_path: endpoints.codex_path,
        };
        *key = token.clone();
    }
    r
}

impl Switcher {
    /// The distribution the desktop app runs this computer's Codex in ("Run
    /// Codex in WSL", user bug 54 (b)): the app's setting in this Codex home is
    /// on and Windows lists the distribution it uses (the default). That Codex
    /// reads this computer's `config.toml` and `auth.json`. Reads two files and
    /// the WSL inventory; starts no process.
    pub(crate) async fn legacy_wsl(&self, home: &CodexHome) -> Option<WslDistro> {
        let config = tokio::fs::read_to_string(home.config_toml()).await.ok();
        let global = tokio::fs::read(home.global_state_json())
            .await
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        if !superba_core::wsl::runs_codex_in_wsl(config.as_deref(), global.as_ref()) {
            return None;
        }
        let distros: Vec<WslDistro> = self
            .deps
            .remote
            .discover(home, &[], &[])
            .await
            .ok()?
            .into_iter()
            .filter(|h| h.kind == HostKind::Wsl)
            .filter_map(|h| h.wsl)
            .collect();
        superba_core::wsl::legacy_distro(&distros).cloned()
    }

    /// Plan notes and blocks of "Run Codex in WSL" for a local switch: router
    /// mode needs `127.0.0.1` to reach Windows from that distribution.
    pub(crate) async fn legacy_wsl_check(
        &self,
        home: &CodexHome,
        routed: bool,
    ) -> (Vec<Notice>, Option<superba_core::BlockReason>) {
        let Some(distro) = self.legacy_wsl(home).await else {
            return (Vec::new(), None);
        };
        let notes = vec![Notice::info(
            text("switch.wsl.legacy_mode").arg("distro", &distro.name),
        )];
        let block = (routed && distro.router_path() != RouterPath::Loopback).then(|| {
            superba_core::BlockReason::RouterFromWslNat {
                distro: distro.name.clone(),
            }
        });
        (notes, block)
    }

    /// A stopped distribution a switch comes back to (pending): the user turned
    /// following on, Superba wrote it, or its last check found a Codex. Read
    /// locally; the distribution is not started.
    pub(crate) async fn wsl_worth_updating(&self, set: &ProfileSet, host: &RemoteHost) -> bool {
        follow_on(set, host)
            || self.deps.remote.written(host).await.unwrap_or(false)
            || self
                .cached_hosts()
                .get(&host.target_id)
                .and_then(|c| c.state.as_ref())
                .is_some_and(has_codex)
    }

    /// Whether and how `host` reaches the running router (read-only: asks the
    /// router's `health` from inside the distribution).
    pub(crate) async fn wsl_route(&self, host: &RemoteHost, state: &RemoteState) -> WslRoute {
        let d = &self.deps;
        let title = host.title();
        let direct = |key: &str| WslRoute::Direct {
            reason: text(key).arg_text("host", title.clone()),
        };
        let Some(distro) = host.wsl_distro() else {
            return direct("switch.wsl.route_unknown");
        };
        let Ok(endpoint) = d.router.endpoint() else {
            return direct("switch.wsl.route_unknown");
        };
        let Liveness::Up(status) = d.router.liveness().await else {
            return direct("switch.wsl.route_router_down");
        };
        let Some(token) = superba_router::token::source_token(
            endpoint.token.expose().as_bytes(),
            &router_source(&distro),
        ) else {
            return direct("switch.wsl.route_unknown");
        };
        let path = host.wsl.as_ref().map_or(
            RouterPath::HostAddress,
            superba_core::WslDistro::router_path,
        );
        let port = endpoint.port;
        let (address, origin) = match path {
            RouterPath::Unreachable => return direct("switch.wsl.route_no_network"),
            RouterPath::Loopback => (Ipv4Addr::LOCALHOST, None),
            RouterPath::HostAddress => {
                let Some(gw) = state
                    .gateway
                    .as_deref()
                    .and_then(|g| g.parse::<Ipv4Addr>().ok())
                else {
                    return direct("switch.wsl.route_no_gateway");
                };
                let listening = status
                    .wsl_listen
                    .iter()
                    .any(|a| a == &format!("{gw}:{port}"));
                if !listening {
                    return direct("switch.wsl.route_needs_access");
                }
                match Origin::local_router_for_wsl(gw, port) {
                    Ok(o) => (gw, Some(o)),
                    Err(_) => return direct("switch.wsl.route_no_gateway"),
                }
            }
            _ => return direct("switch.wsl.route_unknown"),
        };
        match d
            .remote
            .http_get(host, &address.to_string(), port, HEALTH_PATH)
            .await
        {
            Ok(Some(answer)) if is_router_health(&answer, &status.instance_id) => {
                let origin = match origin {
                    Some(o) => o,
                    None => match Origin::parse(&format!("http://127.0.0.1:{port}")) {
                        Ok(o) => o,
                        Err(_) => return direct("switch.wsl.route_unknown"),
                    },
                };
                WslRoute::Routed {
                    origin,
                    token: Secret::new(token),
                }
            }
            Ok(_) if path == RouterPath::HostAddress => direct("switch.wsl.route_blocked"),
            Ok(_) => direct("switch.wsl.route_loopback_failed"),
            Err(e) if e.is_not_implemented() => direct("switch.wsl.route_unknown"),
            Err(e) => WslRoute::Direct {
                reason: text("switch.wsl.route_check_failed")
                    .arg_text("host", title)
                    .arg("detail", e.to_string()),
            },
        }
    }

    /// The profile's Codex settings for a distribution, filtered with the
    /// catalog of its Codex version (like a settings sync), and the notes.
    pub(crate) fn wsl_settings(
        &self,
        set: &ProfileSet,
        resolved: &ResolvedProfile,
        host: &RemoteHost,
        state: &RemoteState,
    ) -> (CodexSettings, Vec<Notice>) {
        let wanted = resolved
            .profile_id
            .as_ref()
            .and_then(|id| set.profile(id))
            .map(|p| p.codex_settings.clone())
            .unwrap_or_default();
        if wanted.is_empty() {
            return (CodexSettings::default(), Vec::new());
        }
        let consumers = [SettingsConsumer::new(
            ConsumerKind::Remote {
                target_id: host.target_id.clone(),
            },
            host.title().render_fallback(),
            state.codex_version.clone(),
        )];
        let catalog = (self.deps.remote_catalog)(&state.os);
        let config = state.config_toml.as_ref().map(|b| b.expose());
        let sp = plan_settings(
            &wanted,
            &HomeInput::new(&host.target_id, &consumers, catalog.as_ref())
                .config_toml(config)
                .owned_paths(&state.owned_paths),
        );
        let mut notes = Vec::new();
        if !sp.skipped.is_empty() {
            notes.push(Notice::info(
                text("switch.wsl.settings_skipped")
                    .arg_text("host", host.title())
                    .arg("count", sp.skipped.len()),
            ));
        }
        (sp.apply, notes)
    }
}

/// How long router access waits for the router to bind a new WSL listener.
const LISTEN_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

impl Switcher {
    pub(crate) async fn wsl_overview_impl(&self) -> Result<WslOverview> {
        let d = &self.deps;
        // Asked for (spb wsl list): which distributions run now, without
        // starting any.
        if let Err(e) = d.remote.wsl_running().await
            && !e.is_not_implemented()
        {
            tracing::debug!(error = %e, "cannot tell which WSL distributions run");
        }
        let (networking, error) = match d.remote.wsl_status().await {
            Ok(s) => s,
            Err(e) if e.is_not_implemented() => (Default::default(), None),
            Err(e) => (Default::default(), Some(e.to_string())),
        };
        let distros = self
            .hosts_impl()
            .await?
            .into_iter()
            .filter(|h| h.host.kind == HostKind::Wsl)
            .collect();
        let router_wsl_listen = match d.router.liveness().await {
            Liveness::Up(status) => status.wsl_listen.clone(),
            Liveness::Down { .. } => Vec::new(),
        };
        let app_runs_codex_in = self
            .legacy_wsl(&d.ctx.codex_home())
            .await
            .map(|distro| distro.name);
        Ok(WslOverview {
            networking,
            error,
            distros,
            router_wsl_listen,
            app_runs_codex_in,
        })
    }

    pub(crate) async fn wsl_router_access_impl(&self, id: &TargetId) -> Result<WslAccessReport> {
        let d = &self.deps;
        let (host, _) = self.app_host(id).await?;
        if host.kind != HostKind::Wsl {
            return Err(Error::invalid(format!(
                "{} is not a WSL distribution",
                host.title().render_fallback()
            )));
        }
        let state = d.remote.probe(&host).await?;
        self.cache_host(id, |c| {
            c.state = Some(state.clone());
            c.error = None;
        });
        let needs_router =
            || Error::invalid(text("switch.wsl.access_needs_router").render_fallback());
        let endpoint = d.router.endpoint().map_err(|_| needs_router())?;
        let Liveness::Up(_) = d.router.liveness().await else {
            return Err(needs_router());
        };
        let path = host.wsl.as_ref().map_or(
            RouterPath::HostAddress,
            superba_core::WslDistro::router_path,
        );
        let mut report = WslAccessReport {
            target_id: id.clone(),
            path,
            address: None,
            reachable: false,
            firewall_changed: false,
            notes: Vec::new(),
        };
        let title = host.title();
        let finish = |report: &mut WslAccessReport, route: WslRoute| match route {
            WslRoute::Routed { origin, .. } => {
                report.reachable = true;
                report.address = Some(origin.as_str());
                report.notes.push(Notice::info(
                    text("switch.wsl.access_on")
                        .arg_text("host", title.clone())
                        .arg("address", origin.as_str()),
                ));
            }
            WslRoute::Direct { reason } => report.notes.push(Notice::warning(reason)),
        };
        match path {
            RouterPath::HostAddress => {}
            RouterPath::Loopback => {
                let route = self.wsl_route(&host, &state).await;
                if matches!(route, WslRoute::Routed { .. }) {
                    report.notes.push(Notice::info(
                        text("switch.wsl.access_not_needed").arg_text("host", title.clone()),
                    ));
                }
                finish(&mut report, route);
                return Ok(report);
            }
            _ => {
                finish(&mut report, self.wsl_route(&host, &state).await);
                return Ok(report);
            }
        }
        let gateway = state
            .gateway
            .as_deref()
            .and_then(|g| g.parse::<Ipv4Addr>().ok())
            .ok_or_else(|| {
                Error::invalid(
                    text("switch.wsl.route_no_gateway")
                        .arg_text("host", title.clone())
                        .render_fallback(),
                )
            })?;
        // 1. The router listens on the WSL adapter address.
        let mut listen = d.router.wsl_listen();
        if !listen.contains(&gateway) {
            listen.push(gateway);
            d.router.set_wsl_listen(listen).await?;
        }
        let bound = format!("{gateway}:{}", endpoint.port);
        let started = std::time::Instant::now();
        loop {
            if let Liveness::Up(status) = d.router.liveness().await
                && status.wsl_listen.contains(&bound)
            {
                break;
            }
            if started.elapsed() > LISTEN_WAIT {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        // 2. Reachable already (no firewall in the way)?
        let route = self.wsl_route(&host, &state).await;
        if matches!(route, WslRoute::Routed { .. }) {
            finish(&mut report, route);
            return Ok(report);
        }
        // 3. Windows Firewall: Superba's rule (administrator approval).
        match d.router.allow_wsl_firewall(gateway, endpoint.port).await {
            Ok(change) => {
                report.firewall_changed = true;
                let key = match change {
                    superba_router::firewall::FirewallChange::Added => {
                        "switch.wsl.access_firewall_added"
                    }
                    _ => "switch.wsl.access_firewall_updated",
                };
                report.notes.push(Notice::info(
                    text(key)
                        .arg("address", gateway.to_string())
                        .arg("port", endpoint.port),
                ));
            }
            Err(Error::Cancelled) => {
                report
                    .notes
                    .push(Notice::warning(text("switch.wsl.access_cancelled")));
                return Ok(report);
            }
            Err(e) => return Err(e),
        }
        // The rule applies to new connections at once; give it a moment.
        for _ in 0..3 {
            let route = self.wsl_route(&host, &state).await;
            if matches!(route, WslRoute::Routed { .. }) {
                finish(&mut report, route);
                return Ok(report);
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        report.notes.push(Notice::warning(
            text("switch.wsl.access_unreachable")
                .arg_text("host", title)
                .arg("address", bound),
        ));
        Ok(report)
    }
}

/// Environment variable naming Superba's Linux helper (`spb` for Linux) to run
/// inside WSL distributions (tests, development builds).
pub const ENV_WSL_HELPER: &str = "SUPERBA_WSL_HELPER";

/// File name of the helper the Windows package ships next to `superba.exe`.
pub fn helper_file_name(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("spb-linux-x86_64"),
        "aarch64" | "arm64" => Some("spb-linux-aarch64"),
        _ => None,
    }
}

/// The `spb` arguments that migrate sessions as `request` says.
pub fn migrate_args(request: &superba_core::MigrationRequest, locale: &str) -> Result<Vec<String>> {
    let mut providers = request.selectors.iter().filter_map(|s| match s {
        superba_core::MigrationSelector::Provider { provider } => Some(provider.clone()),
        _ => None,
    });
    let from = providers
        .next()
        .ok_or_else(|| Error::invalid("a WSL session migration needs one source provider"))?;
    if providers.next().is_some() || request.selectors.len() != 1 {
        return Err(Error::invalid(
            "a WSL session migration takes one source provider at a time",
        ));
    }
    let mut args = vec![
        "sessions".to_owned(),
        "migrate".to_owned(),
        "--from".to_owned(),
        from,
        "--to".to_owned(),
        request.to_provider.clone(),
        "--yes".to_owned(),
        "--locale".to_owned(),
        locale.to_owned(),
    ];
    if !request.include_archived {
        args.push("--no-archived".to_owned());
    }
    for t in &request.exclude {
        args.push("--exclude-thread".to_owned());
        args.push(t.clone());
    }
    Ok(args)
}

impl Switcher {
    /// The Linux helper for a distribution of `arch` (`SUPERBA_WSL_HELPER`, else
    /// next to the running program); errors in `catalog`'s language.
    fn wsl_helper(
        &self,
        arch: Option<&str>,
        catalog: &superba_core::Catalog,
    ) -> Result<std::path::PathBuf> {
        if let Some(p) = self.deps.ctx.env.var_non_empty(ENV_WSL_HELPER) {
            let p = std::path::PathBuf::from(p);
            if p.is_file() {
                return Ok(p);
            }
        }
        let arch = arch.unwrap_or("x86_64");
        let name = helper_file_name(arch).ok_or_else(|| Error::Unsupported {
            what: format!("migrating sessions in a WSL distribution on {arch}"),
        })?;
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| Error::other(format!("cannot find this program: {e}")))?;
        let p = exe.with_file_name(name);
        if p.is_file() {
            Ok(p)
        } else {
            Err(Error::NotFound {
                what: catalog.text(&text("switch.wsl.no_helper").arg("file", name)),
            })
        }
    }

    pub(crate) async fn host_migrate_sessions_impl(
        &self,
        id: &TargetId,
        request: &superba_core::MigrationRequest,
        locale: superba_core::Locale,
        events: std::sync::Arc<dyn superba_core::SwitchEvents>,
    ) -> Result<superba_core::HostMigrationReport> {
        let d = &self.deps;
        let catalog = crate::messages::catalog(locale);
        let (host, _) = self.app_host(id).await?;
        if host.kind != HostKind::Wsl {
            return Err(Error::Unsupported {
                what: format!(
                    "migrating sessions on {} from here (run spb sessions migrate on the host)",
                    host.title().render_fallback()
                ),
            });
        }
        let state = d.remote.probe(&host).await?;
        if !state.app_server_pids.is_empty() {
            return Err(Error::Blocked {
                reasons: vec![superba_core::BlockReason::UnsupportedTarget {
                    detail: catalog.text(
                        &text("switch.wsl.migrate_app_running").arg_text("host", host.title()),
                    ),
                }],
            });
        }
        let helper = self.wsl_helper(state.arch.as_deref(), &catalog)?;
        let args = migrate_args(request, locale.code())?;
        events.emit(superba_core::SwitchEvent::Log {
            level: superba_core::Severity::Info,
            text: text("switch.wsl.migrating").arg_text("host", host.title()),
        });
        let sink_events = events.clone();
        let sink: superba_core::LineSink = std::sync::Arc::new(move |line: &str| {
            sink_events.emit(superba_core::SwitchEvent::Log {
                level: superba_core::Severity::Info,
                text: LocalizedText::new(String::new(), line),
            });
        });
        let run = d.remote.run_helper(&host, &helper, &args, sink).await?;
        let ok = run.exit_code == Some(0);
        let report = d.remote.scan_sessions(&host).await.ok();
        Ok(superba_core::HostMigrationReport {
            target_id: id.clone(),
            ok,
            run,
            report,
        })
    }
}
