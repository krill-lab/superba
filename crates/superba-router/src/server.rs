//! The loopback server: binding, connection handling, dispatch and shutdown.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
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
use crate::forward::{local_busy, router_error};
use crate::limits::{
    Budget, Busy, ConnSlot, INBOUND_IDLE_TIMEOUT, LOCAL_BUSY_CODE, Limits, Place, file_limit_in,
};
use crate::paths::RouterPaths;
use crate::probe::health_loop;
use crate::runtime::RuntimeInfo;
use crate::state::{Generation, RespBody, Shared, Totals, unix_ms};
use crate::stats::hub::{End, StatsHub, now_ms};
use crate::stats::meta;
use crate::status::RouterStatus;
use crate::token::{self, bearer, constant_time_eq};
use crate::{proxy, ws};

/// How long a shutdown waits for open connections to finish.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Pause after an accept that failed for want of descriptors.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// Longest time spent discarding the body of a request answered locally.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

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
        let mut stop = shared.shutdown.subscribe();
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, peer) = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(a) => a,
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
            if !peer.ip().is_loopback() {
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
                serve_connection(shared.clone(), stream, slot)
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
#[derive(Debug, Default)]
struct ConnState {
    /// Requests received on it.
    requests: AtomicU64,
    /// Its next response closes it: it was asked to give its place back
    /// before its first request had been read.
    close_after: AtomicBool,
}

/// Serve one inbound connection until it closes or the router stops.
async fn serve_connection(shared: Arc<Shared>, stream: TcpStream, slot: ConnSlot) {
    let mut conn_stop = shared.shutdown.subscribe();
    let mut shed = shared.limits.shed_signal();
    let state = Arc::new(ConnState::default());
    let (s, st) = (shared.clone(), state.clone());
    let service = service_fn(move |mut req: Request<Incoming>| {
        let (s, st) = (s.clone(), st.clone());
        // Requests carry the connection's place, so an upgraded WebSocket
        // keeps it after this task ends.
        req.extensions_mut().insert(slot.clone());
        st.requests.fetch_add(1, Ordering::Relaxed);
        async move {
            let mut resp = dispatch(s, req).await;
            if st.close_after.load(Ordering::Relaxed)
                && resp.status() != StatusCode::SWITCHING_PROTOCOLS
            {
                resp.headers_mut()
                    .insert(CONNECTION, HeaderValue::from_static("close"));
            }
            Ok::<_, Infallible>(resp)
        }
    });
    let conn = http1::Builder::new()
        .keep_alive(true)
        // Idle keep-alive connections give their descriptor back.
        .timer(TokioTimer::new())
        .header_read_timeout(INBOUND_IDLE_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    tokio::pin!(conn);
    let mut shedding = false;
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
                if changed.is_ok() {
                    if state.requests.load(Ordering::Relaxed) > 0 {
                        conn.as_mut().graceful_shutdown();
                    } else {
                        state.close_after.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

/// Route one request.
async fn dispatch(shared: Arc<Shared>, req: Request<Incoming>) -> Response<RespBody> {
    let path = req.uri().path();
    if path.starts_with(ADMIN_PREFIX) {
        return admin::handle(shared, req).await;
    }
    let Some(rest) = strip_prefix(path) else {
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
    let authorized = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| bearer(v.as_bytes()))
        .is_some_and(|t| constant_time_eq(t, generation.router_token.expose().as_bytes()));
    drop(generation);
    if !authorized {
        shared.totals.rejected.fetch_add(1, Ordering::Relaxed);
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
        Err(Busy) => return busy(&shared, req, &rest, t0, started_ms).await,
    };
    if ws::is_upgrade(&req) {
        ws::proxy_ws(shared, req, rest, admission).await
    } else {
        proxy::proxy_http(shared, req, rest, admission).await
    }
}

/// 503 `local_router_busy` for a request that got no admission: recorded like
/// any Responses request, its body drained first (closing on an unread body
/// would reset the connection before Codex reads the answer).
async fn busy(
    shared: &Arc<Shared>,
    req: Request<Incoming>,
    rest: &str,
    t0: Instant,
    started_ms: i64,
) -> Response<RespBody> {
    let message = shared.local_busy();
    let generation = shared.current();
    let (parts, body) = req.into_parts();
    if generation.config.stats.is_some() && proxy::is_responses_path(&parts.method, rest) {
        shared
            .stats
            .track(
                generation.clone(),
                meta::from_headers(&parts.headers),
                t0,
                started_ms,
            )
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
