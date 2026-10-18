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
    CodexSettings, ConsumerKind, Error, HostKind, LocalizedText, Notice, Origin, ProfileSet,
    RemoteHost, RemoteState, ResolvedEndpoints, ResolvedMode, ResolvedProfile, RestoreTarget,
    Result, Secret, SettingsConsumer, SwitchIntent, TargetId,
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

/// The user chose to follow (not just the default of a WSL distribution).
pub(crate) fn follow_chosen(set: &ProfileSet, host: &RemoteHost) -> bool {
    set.settings
        .hosts
        .get(&host.target_id)
        .is_some_and(|h| h.follow_chosen || h.follow_switches)
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
        Ok(WslOverview {
            networking,
            error,
            distros,
            router_wsl_listen,
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
