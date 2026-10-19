//! The loopback server: binding, connection handling, dispatch and shutdown.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use http::header::{AUTHORIZATION, CONNECTION};
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use superba_core::{Error, Result, Secret};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tracing::Instrument;

use crate::admin::{self, ADMIN_PREFIX, VERSION};
use crate::config::{ROUTER_BASE_PATH, ROUTER_V1_PATH, RouterConfig, RouterSecrets};
use crate::forward::{CODEX_HOST_HEADER, HARNESS_HEADER, local_busy, router_error};
use crate::limits::{Budget, Busy, ConnSlot, LOCAL_BUSY_CODE, Limits, Place, file_limit_in};
use crate::paths::RouterPaths;
use crate::probe::health_loop;
use crate::runtime::RuntimeInfo;
use crate::state::{Generation, RespBody, Shared, Totals, unix_ms};
use crate::stats::hub::{End, StatsHub, now_ms};
use crate::stats::meta;
use crate::status::RouterStatus;
use crate::token::{self, Presenter, bearer, constant_time_eq, verify_source_token};
use crate::{proxy, ws};

/// How long a shutdown waits for open connections to finish.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Pause after an accept that failed for want of descriptors.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// Longest time spent discarding the body of a request answered locally.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the WSL listeners are reconciled with the configuration (an
/// adapter address appears when WSL starts its VM).
const WSL_LISTEN_TICK: Duration = Duration::from_secs(5);

/// Where a connection came in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    /// `127.0.0.1:<port>`: this computer (also WSL 1 and mirrored WSL 2).
    Loopback,
    /// A WSL adapter address ([`RouterConfig::wsl_listen`]): WSL 2 behind NAT.
    Wsl,
}

/// A bound, not yet serving router.
pub struct RouterServer {
    shared: Arc<Shared>,
    listener: TcpListener,
}

/// Stops a serving router from another task.
#[derive(Clone)]
pub struct ShutdownHandle {
    shared: Arc<Shared>,
}

impl ShutdownHandle {
    /// Ask the router to stop (in-flight connections get a short grace period).
    pub fn shutdown(&self) {
        self.shared.request_shutdown();
    }
}

impl std::fmt::Debug for ShutdownHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShutdownHandle")
    }
}

impl std::fmt::Debug for RouterServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterServer")
            .field("listen", &self.shared.listen)
            .field("instance_id", &self.shared.instance_id)
            .finish_non_exhaustive()
    }
}

impl RouterServer {
    /// Load `config.json` and `secrets.json` from `paths` and bind
    /// `127.0.0.1:<listen_port>`. Reload through the admin API re-reads them.
    pub async fn bind_from_files(paths: RouterPaths) -> Result<Self> {
        let config = RouterConfig::load(&paths)?;
        let secrets = RouterSecrets::load(&paths)?;
        Self::bind_inner(config, secrets, Some(paths)).await
    }

    /// Bind with an in-memory configuration (tests, embedding). `listen_port` 0
    /// picks an ephemeral port.
    pub async fn bind(config: RouterConfig, secrets: RouterSecrets) -> Result<Self> {
        Self::bind_inner(config, secrets, None).await
    }

    async fn bind_inner(
        config: RouterConfig,
        secrets: RouterSecrets,
        paths: Option<RouterPaths>,
    ) -> Result<Self> {
        let port = config.listen_port;
        let budget = Budget::for_this_process(&config.tuning);
        let generation = Generation::build(1, config, &secrets, &budget)?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(addr).await.map_err(|e| Error::Network {
            url: format!("http://{addr}"),
            message: format!("cannot listen: {e}"),
        })?;
        let listen = listener.local_addr().map_err(|e| Error::Network {
            url: format!("http://{addr}"),
            message: e.to_string(),
        })?;
        let (shutdown, _) = watch::channel(false);
        let stats = Arc::new(StatsHub::new(paths.clone()));
        let shared = Arc::new(Shared {
            generation: RwLock::new(Arc::new(generation)),
            paths,
            instance_id: token::generate("").chars().take(16).collect(),
            admin_token: Secret::new(token::generate(token::ADMIN_TOKEN_PREFIX)),
            started: Instant::now(),
            started_at_ms: unix_ms(),
            listen,
            totals: Totals::default(),
            events: Mutex::new(VecDeque::new()),
            shutdown,
            stats,
            transports: crate::transport::TransportMemory::default(),
            limits: Limits::new(budget),
            stats_retry: Mutex::new(None),
            wsl_bound: Mutex::new(Vec::new()),
            claude: crate::claude::ClaudeState::default(),
            openai: crate::openai::OpenAiState::default(),
        });
        shared.apply_stats(shared.current().config.stats.as_ref());
        Ok(Self { shared, listener })
    }

    /// The bound address (`127.0.0.1:<port>`).
    pub fn local_addr(&self) -> SocketAddr {
        self.shared.listen
    }

    /// Codex's `openai_base_url` for this router.
    pub fn base_url(&self) -> String {
        format!("http://{}{ROUTER_BASE_PATH}", self.shared.listen)
    }

    /// The per-instance admin token.
    pub fn admin_token(&self) -> &Secret {
        &self.shared.admin_token
    }

    /// Random id of this instance.
    pub fn instance_id(&self) -> &str {
        &self.shared.instance_id
    }

    /// A handle that stops the server.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            shared: self.shared.clone(),
        }
    }

    /// Current status (same as the admin API's).
    pub fn status(&self) -> RouterStatus {
        admin::status(&self.shared)
    }

    /// The runtime record of this instance.
    pub fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo {
            pid: std::process::id(),
            port: self.shared.listen.port(),
            instance_id: self.shared.instance_id.clone(),
            admin_token: self.shared.admin_token.clone(),
            started_at_ms: self.shared.started_at_ms,
            version: VERSION.into(),
        }
    }

    /// Serve until shut down. When started from files, publishes `runtime.json`
    /// first and removes it at the end.
    pub async fn serve(self) -> Result<()> {
        let RouterServer { shared, listener } = self;
        if let Some(paths) = &shared.paths {
            RuntimeInfo {
                pid: std::process::id(),
                port: shared.listen.port(),
                instance_id: shared.instance_id.clone(),
                admin_token: shared.admin_token.clone(),
                started_at_ms: shared.started_at_ms,
                version: VERSION.into(),
            }
            .save(paths)?;
        }
        let generation = shared.current();
        let budget = shared.limits.budget;
        tracing::info!(
            listen = %shared.listen,
            members = generation.members.len(),
            lines = generation.members.iter().map(|m| m.lines.len()).sum::<usize>(),
            version = VERSION,
            open_files_limit = ?budget.open_files_limit,
            max_concurrent = budget.max_concurrent,
            max_connections = budget.max_connections,
            "superba-router listening"
        );
        drop(generation);
        tokio::spawn(health_loop(shared.clone()));
        let (wsl_tx, mut wsl_rx) = tokio::sync::mpsc::channel::<(TcpStream, SocketAddr)>(64);
        tokio::spawn(wsl_listeners(shared.clone(), wsl_tx));
        let mut stop = shared.shutdown.subscribe();
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, peer, entry) = tokio::select! {
                wsl = wsl_rx.recv() => match wsl {
                    Some((stream, peer)) => (stream, peer, Entry::Wsl),
                    None => continue,
                },
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => (stream, peer, Entry::Loopback),
                    Err(e) => {
                        if file_limit_in(&e).is_some() {
                            // No descriptor for the connection: it stays in
                            // the backlog until some close.
                            shared.local_file_limit(Place::Accept, &e);
                            tokio::time::sleep(ACCEPT_BACKOFF).await;
                        } else {
                            tracing::warn!(error = %e, "accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        continue;
                    }
                },
                _ = stop.changed() => break,
            };
            let allowed = match entry {
                Entry::Loopback => peer.ip().is_loopback(),
                Entry::Wsl => wsl_peer(peer.ip()),
            };
            if !allowed {
                continue;
            }
            // At the connection cap this one waits for a place (open
            // connections stop keep-alive meanwhile) and later ones wait in
            // the listen backlog (crate::limits).
            let slot = tokio::select! {
                slot = shared.limits.connection() => slot,
                _ = stop.changed() => break,
            };
            let _ = stream.set_nodelay(true);
            connections.spawn(
                serve_connection(shared.clone(), stream, slot, entry)
                    .instrument(tracing::debug_span!("conn", %peer)),
            );
            while connections.try_join_next().is_some() {}
        }
        tracing::info!("superba-router stopping");
        drop(listener);
        let _ = tokio::time::timeout(SHUTDOWN_GRACE + Duration::from_secs(1), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        let hub = shared.stats.clone();
        let _ = tokio::task::spawn_blocking(move || hub.shutdown()).await;
        if let Some(paths) = &shared.paths {
            RuntimeInfo::remove_if_owned(paths, &shared.instance_id);
        }
        Ok(())
    }
}

/// Per-connection state shared by its task and its requests.
#[derive(Debug)]
struct ConnState {
    /// When the connection was accepted (the base of `idle_since_ms`).
    opened: Instant,
    /// Requests received on it.
    requests: AtomicU64,
    /// Responses not finished yet (their body still streams).
    in_flight: AtomicU64,
    /// Milliseconds after `opened` when the last response finished.
    idle_since_ms: AtomicU64,
    /// Its next response closes it: it was asked to give its place back
    /// before its first request had been read.
    close_after: AtomicBool,
}

impl ConnState {
    fn new() -> Self {
        Self {
            opened: Instant::now(),
            requests: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            idle_since_ms: AtomicU64::new(0),
            close_after: AtomicBool::new(false),
        }
    }

    /// How long the connection has had no response in flight (zero while
    /// one is).
    fn idle_for(&self) -> Duration {
        if self.in_flight.load(Ordering::Acquire) > 0 {
            return Duration::ZERO;
        }
        let since = Duration::from_millis(self.idle_since_ms.load(Ordering::Acquire));
        self.opened.elapsed().saturating_sub(since)
    }

    fn response_done(&self) {
        let ms = u64::try_from(self.opened.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.idle_since_ms.store(ms, Ordering::Release);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A response body that tells its connection when it is finished.
struct DoneBody {
    inner: RespBody,
    state: Option<Arc<ConnState>>,
}

impl DoneBody {
    fn finish(&mut self) {
        if let Some(state) = self.state.take() {
            state.response_done();
        }
    }
}

impl Drop for DoneBody {
    fn drop(&mut self) {
        self.finish();
    }
}

impl http_body::Body for DoneBody {
    type Data = bytes::Bytes;
    type Error = crate::state::BoxError;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>>
    {
        let polled = std::pin::Pin::new(&mut self.inner).poll_frame(cx);
        if matches!(polled, std::task::Poll::Ready(None | Some(Err(_)))) {
            self.finish();
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// How often a connection in short-idle mode checks whether to close.
fn pressure_tick(idle: Duration) -> Duration {
    (idle / 2).clamp(Duration::from_millis(50), Duration::from_secs(1))
}

/// A peer the WSL listeners accept: a private or shared IPv4 address (the WSL
/// VM behind NAT), never a public one.
fn wsl_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => crate::config::wsl_listen_allowed(v4),
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .is_some_and(crate::config::wsl_listen_allowed),
    }
}

/// Keep one listener per configured WSL address ([`RouterConfig::wsl_listen`])
/// bound on the router's port, forwarding accepted connections to the main
/// loop. An address that cannot be bound yet (WSL's VM not started) is tried
/// again every [`WSL_LISTEN_TICK`]; a listener whose address went away (its
/// accept fails) is dropped and bound again later.
async fn wsl_listeners(
    shared: Arc<Shared>,
    tx: tokio::sync::mpsc::Sender<(TcpStream, SocketAddr)>,
) {
    let port = shared.listen.port();
    let mut stop = shared.shutdown.subscribe();
    let mut running: BTreeMap<Ipv4Addr, tokio::task::JoinHandle<()>> = BTreeMap::new();
    loop {
        let wanted: Vec<Ipv4Addr> = shared.current().config.wsl_listen.clone();
        running.retain(|ip, task| {
            let keep = wanted.contains(ip) && !task.is_finished();
            if !keep {
                task.abort();
            }
            keep
        });
        for ip in wanted {
            if running.contains_key(&ip) {
                continue;
            }
            let addr = SocketAddr::from((ip, port));
            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    tracing::info!(%addr, "listening for WSL");
                    let tx = tx.clone();
                    running.insert(
                        ip,
                        tokio::spawn(async move {
                            loop {
                                match listener.accept().await {
                                    Ok(conn) => {
                                        if tx.send(conn).await.is_err() {
                                            return;
                                        }
                                    }
                                    Err(e) if file_limit_in(&e).is_some() => {
                                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                                    }
                                    Err(e) => {
                                        tracing::info!(%addr, error = %e, "WSL listener closed");
                                        return;
                                    }
                                }
                            }
                        }),
                    );
                }
                Err(e) => tracing::debug!(%addr, error = %e, "WSL address not available yet"),
            }
        }
        *shared.wsl_bound.lock().unwrap_or_else(|p| p.into_inner()) = running
            .keys()
            .map(|ip| SocketAddr::from((*ip, port)))
            .collect();
        tokio::select! {
            () = tokio::time::sleep(WSL_LISTEN_TICK) => {}
            _ = stop.changed() => break,
        }
    }
    for task in running.values() {
        task.abort();
    }
}

/// Serve one inbound connection until it closes or the router stops.
async fn serve_connection(shared: Arc<Shared>, stream: TcpStream, slot: ConnSlot, entry: Entry) {
    let mut conn_stop = shared.shutdown.subscribe();
    let mut shed = shared.limits.shed_signal();
    let mut pressure = shared.limits.pressure_signal();
    let tuning = shared.current().config.tuning.clone();
    let idle_timeout = Duration::from_secs(tuning.inbound_idle_timeout_secs);
    let pressure_idle = Duration::from_millis(tuning.pressure_idle_timeout_ms);
    let state = Arc::new(ConnState::new());
    // Declared before the connection so it is released after the connection
    // (and its place) is gone.
    let mut _pressure_close = None;
    let (s, st) = (shared.clone(), state.clone());
    let service = service_fn(move |mut req: Request<Incoming>| {
        let (s, st) = (s.clone(), st.clone());
        // Requests carry the connection's place, so an upgraded WebSocket
        // keeps it after this task ends.
        req.extensions_mut().insert(slot.clone());
        st.requests.fetch_add(1, Ordering::Relaxed);
        st.in_flight.fetch_add(1, Ordering::AcqRel);
        async move {
            let mut resp = dispatch(s, req, entry).await;
            if st.close_after.load(Ordering::Relaxed)
                && resp.status() != StatusCode::SWITCHING_PROTOCOLS
            {
                resp.headers_mut()
                    .insert(CONNECTION, HeaderValue::from_static("close"));
            }
            let resp = resp.map(|inner| {
                DoneBody {
                    inner,
                    state: Some(st),
                }
                .boxed_unsync()
            });
            Ok::<_, Infallible>(resp)
        }
    });
    let conn = http1::Builder::new()
        .keep_alive(true)
        // Idle keep-alive connections give their descriptor back, later than
        // HTTP clients drop idle connections themselves (crate::limits).
        .timer(TokioTimer::new())
        .header_read_timeout(idle_timeout)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    tokio::pin!(conn);
    let mut shedding = false;
    let mut short_idle = false;
    loop {
        tokio::select! {
            r = conn.as_mut() => {
                if let Err(e) = r {
                    tracing::debug!(error = %e, "connection ended");
                }
                return;
            }
            _ = conn_stop.changed() => {
                conn.as_mut().graceful_shutdown();
                let _ = tokio::time::timeout(SHUTDOWN_GRACE, conn.as_mut()).await;
                return;
            }
            // The connection cap is reached: no more keep-alive. A connection
            // that served a request closes now when idle, else after its
            // response; one whose first request was not read yet (accepted
            // from the backlog) closes after answering it. Nothing in flight is
            // cut.
            changed = shed.changed(), if !shedding => {
                shedding = true;
                if changed.is_ok() && close_now(&state) {
                    conn.as_mut().graceful_shutdown();
                }
            }
            // The budget is nearly full: the short idle timeout applies while
            // the pressure lasts.
            changed = pressure.changed(), if !shedding && !short_idle => {
                short_idle = changed.is_ok();
            }
            () = tokio::time::sleep(pressure_tick(pressure_idle)), if short_idle && !shedding => {
                if !shared.limits.under_pressure() {
                    short_idle = false;
                } else if state.idle_for() >= pressure_idle
                    && let Some(claim) = shared.limits.claim_pressure_close()
                {
                    tracing::debug!("closing an idle connection: the inbound budget is nearly full");
                    _pressure_close = Some(claim);
                    shedding = true;
                    if close_now(&state) {
                        conn.as_mut().graceful_shutdown();
                    }
                }
            }
        }
    }
}

/// Whether a connection asked to give its place back can be shut down
/// gracefully now (an idle one then closes at once, a busy one after its
/// response). One whose first request was not read yet is instead closed
/// after answering it: a graceful shutdown would drop that request.
fn close_now(state: &ConnState) -> bool {
    if state.requests.load(Ordering::Relaxed) > 0 {
        true
    } else {
        state.close_after.store(true, Ordering::Relaxed);
        false
    }
}

/// Who a presented token belongs to: this computer's router token (loopback
/// only) names neither host nor harness; a source token names its host
/// and/or harness ([`token::presenter_of`]; a WSL listener accepts only
/// tokens of a `wsl:` host). `None` when it is neither.
fn authenticate(router_token: &[u8], presented: &[u8], entry: Entry) -> Option<Presenter> {
    if entry == Entry::Loopback && constant_time_eq(presented, router_token) {
        return Some(Presenter::default());
    }
    let presenter = token::presenter_of(&verify_source_token(router_token, presented)?);
    let wsl = presenter
        .host
        .as_deref()
        .is_some_and(|h| h.starts_with("wsl:"));
    (entry == Entry::Loopback || wsl).then_some(presenter)
}

/// Route one request.
async fn dispatch(
    shared: Arc<Shared>,
    mut req: Request<Incoming>,
    entry: Entry,
) -> Response<RespBody> {
    let path = req.uri().path();
    if path.starts_with(ADMIN_PREFIX) {
        // Over a WSL listener only `health` answers (reachability checks).
        if entry == Entry::Wsl && path != format!("{ADMIN_PREFIX}health") {
            return router_error(
                StatusCode::FORBIDDEN,
                "admin_auth",
                "the admin API is only available on 127.0.0.1",
            );
        }
        return admin::handle(shared, req).await;
    }
    let claude = crate::claude::ingress_rest(path);
    let openai = crate::openai::ingress_rest(path);
    let Some(rest) = claude
        .clone()
        .or_else(|| openai.clone())
        .or_else(|| strip_prefix(path))
    else {
        return router_error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!(
                "unknown path; Codex's base URL is http://{}{ROUTER_BASE_PATH}",
                shared.listen
            ),
        );
    };
    let generation = shared.current();
    let router_token = generation.router_token.expose().as_bytes();
    let bearer_token = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| bearer(v.as_bytes()));
    // Claude Code sends its token as a bearer (`ANTHROPIC_AUTH_TOKEN`) or as
    // `x-api-key` (`ANTHROPIC_API_KEY`); on the OpenAI ingress WorkBuddy sends
    // both.
    let api_key = (claude.is_some() || openai.is_some())
        .then(|| req.headers().get(crate::claude::API_KEY_HEADER))
        .flatten()
        .map(|v| v.as_bytes().trim_ascii())
        .filter(|v| !v.is_empty());
    let presenter = bearer_token
        .or(api_key)
        .and_then(|t| authenticate(router_token, t, entry));
    drop(generation);
    // The host and harness are ours to say: a client's own headers are
    // dropped (and the headers are never forwarded upstream).
    req.headers_mut().remove(CODEX_HOST_HEADER);
    req.headers_mut().remove(HARNESS_HEADER);
    if let Some(p) = &presenter {
        if let Some(v) = p
            .host
            .as_deref()
            .and_then(|h| HeaderValue::from_str(h).ok())
        {
            req.headers_mut().insert(CODEX_HOST_HEADER, v);
        }
        if let Some(h) = p.harness {
            req.headers_mut()
                .insert(HARNESS_HEADER, HeaderValue::from_static(h.id()));
        }
    }
    if presenter.is_none() {
        if claude.is_some() {
            shared.totals.rejected.fetch_add(1, Ordering::Relaxed);
            return crate::claude::error::local(
                StatusCode::UNAUTHORIZED,
                crate::claude::error::Kind::Authentication,
                "missing or wrong router token (Claude Code's ANTHROPIC_AUTH_TOKEN must be the token Superba wrote)",
            );
        }
        shared.totals.rejected.fetch_add(1, Ordering::Relaxed);
        if openai.is_some() {
            return router_error(
                StatusCode::UNAUTHORIZED,
                "router_auth",
                "missing or wrong router token (the tool's API key must be the token Superba wrote for it)",
            );
        }
        return router_error(
            StatusCode::UNAUTHORIZED,
            "router_auth",
            "missing or wrong router token (Codex's auth.json must hold the token Superba wrote)",
        );
    }
    let wait = Duration::from_millis(shared.current().config.tuning.busy_wait_ms);
    let t0 = Instant::now();
    let started_ms = now_ms();
    let admission = match shared.limits.admit(wait).await {
        Ok(admission) => admission,
        Err(Busy) if claude.is_some() => {
            let message = shared.local_busy();
            let limit = shared.current().config.tuning.max_body_bytes;
            drain(req.into_body(), limit).await;
            return crate::claude::error::local_busy(&message);
        }
        Err(Busy) => {
            let ingress = match openai {
                Some(_) => proxy::Ingress::OpenAi,
                None => proxy::Ingress::Codex,
            };
            return busy(&shared, req, (&rest, ingress), t0, started_ms).await;
        }
    };
    if claude.is_some() {
        crate::claude::handle(shared, req, rest, admission).await
    } else if openai.is_some() {
        crate::openai::handle(shared, req, rest, admission).await
    } else if ws::is_upgrade(&req) {
        ws::proxy_ws(shared, req, rest, admission).await
    } else {
        proxy::proxy_http(shared, req, rest, admission, proxy::Ingress::Codex).await
    }
}

/// 503 `local_router_busy` for a request that got no admission: recorded like
/// any Responses (or, on the OpenAI ingress, Chat Completions) request, its
/// body drained first (closing on an unread body would reset the connection
/// before the client reads the answer).
async fn busy(
    shared: &Arc<Shared>,
    req: Request<Incoming>,
    (rest, ingress): (&str, proxy::Ingress),
    t0: Instant,
    started_ms: i64,
) -> Response<RespBody> {
    let message = shared.local_busy();
    let generation = shared.current();
    let (parts, body) = req.into_parts();
    let facts = match ingress {
        proxy::Ingress::Codex => proxy::is_responses_path(&parts.method, rest)
            .then(|| meta::from_headers(&parts.headers)),
        proxy::Ingress::OpenAi => {
            let p = rest.trim_end_matches('/');
            let protocol = match p {
                "/responses" => Some(crate::openai::OpenAiProtocol::Responses),
                "/chat/completions" => Some(crate::openai::OpenAiProtocol::Chat),
                _ => None,
            };
            protocol
                .filter(|_| parts.method == http::Method::POST)
                .map(|pr| crate::openai::meta::from_headers(&parts.headers, pr))
        }
    };
    if generation.config.stats.is_some()
        && let Some(facts) = facts
    {
        shared
            .stats
            .track(generation.clone(), facts, t0, started_ms)
            .finish(End {
                status: Some(superba_core::RequestStatus::Http {
                    status: 503,
                    code: Some(LOCAL_BUSY_CODE.into()),
                }),
                error: Some(message.clone()),
                ..End::default()
            });
    }
    drain(body, generation.config.tuning.max_body_bytes).await;
    local_busy(&message)
}

/// Read and discard a request body (bounded in size and time).
pub(crate) async fn drain<B>(body: B, limit: usize)
where
    B: http_body::Body + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut body = std::pin::pin!(http_body_util::Limited::new(body, limit));
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
        while let Some(Ok(_)) = body.frame().await {}
    })
    .await;
}

/// The remainder below a router prefix (`""` or `/...`), or `None`.
fn strip_prefix(path: &str) -> Option<String> {
    [ROUTER_BASE_PATH, ROUTER_V1_PATH].iter().find_map(|p| {
        let rest = path.strip_prefix(p)?;
        (rest.is_empty() || rest.starts_with('/')).then(|| rest.to_owned())
    })
}
