//! The loopback server: binding, connection handling, dispatch and shutdown.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use http::header::AUTHORIZATION;
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use superba_core::{Error, Result, Secret};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::Instrument;

use crate::admin::{self, ADMIN_PREFIX, VERSION};
use crate::config::{ROUTER_BASE_PATH, ROUTER_V1_PATH, RouterConfig, RouterSecrets};
use crate::forward::router_error;
use crate::paths::RouterPaths;
use crate::probe::health_loop;
use crate::runtime::RuntimeInfo;
use crate::state::{Generation, RespBody, Shared, Totals, unix_ms};
use crate::stats::hub::StatsHub;
use crate::status::RouterStatus;
use crate::token::{self, bearer, constant_time_eq};
use crate::{proxy, ws};

/// How long a shutdown waits for open connections to finish.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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
        let generation = Generation::build(1, config, &secrets)?;
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
        stats.apply(generation.config.stats.as_ref());
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
        });
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
        tracing::info!(
            listen = %shared.listen,
            members = generation.members.len(),
            lines = generation.members.iter().map(|m| m.lines.len()).sum::<usize>(),
            version = VERSION,
            "superba-router listening"
        );
        drop(generation);
        tokio::spawn(health_loop(shared.clone()));
        let mut stop = shared.shutdown.subscribe();
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::warn!(error = %e, "accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    };
                    if !peer.ip().is_loopback() {
                        continue;
                    }
                    let _ = stream.set_nodelay(true);
                    let s = shared.clone();
                    let mut conn_stop = shared.shutdown.subscribe();
                    connections.spawn(
                        async move {
                            let service = service_fn(move |req| {
                                let s = s.clone();
                                async move { Ok::<_, Infallible>(dispatch(s, req).await) }
                            });
                            let conn = http1::Builder::new()
                                .keep_alive(true)
                                .serve_connection(TokioIo::new(stream), service)
                                .with_upgrades();
                            tokio::pin!(conn);
                            tokio::select! {
                                r = conn.as_mut() => {
                                    if let Err(e) = r {
                                        tracing::debug!(error = %e, "connection ended");
                                    }
                                }
                                _ = conn_stop.changed() => {
                                    conn.as_mut().graceful_shutdown();
                                    let _ = tokio::time::timeout(SHUTDOWN_GRACE, conn.as_mut()).await;
                                }
                            }
                        }
                        .instrument(tracing::debug_span!("conn", %peer)),
                    );
                    while connections.try_join_next().is_some() {}
                }
                _ = stop.changed() => break,
            }
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
    if ws::is_upgrade(&req) {
        ws::proxy_ws(shared, req, rest).await
    } else {
        proxy::proxy_http(shared, req, rest).await
    }
}

/// The remainder below a router prefix (`""` or `/...`), or `None`.
fn strip_prefix(path: &str) -> Option<String> {
    [ROUTER_BASE_PATH, ROUTER_V1_PATH].iter().find_map(|p| {
        let rest = path.strip_prefix(p)?;
        (rest.is_empty() || rest.starts_with('/')).then(|| rest.to_owned())
    })
}
