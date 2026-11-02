// File: ballista/src/manager/mod.rs
//! Run manager: a token-authenticated HTTP API (`/api/v1`) that stores test bundles,
//! queues runs, executes them one at a time and keeps their reports.

pub mod bundles;
pub mod compare;
mod console;
pub mod document;
mod handlers;
pub mod policy;
pub mod runs;
pub mod tokens;
pub mod webhook;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum_server::tls_rustls::RustlsConfig;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::scenario::Registry;
use crate::web::security::RateLimiter;
use bundles::BundleStore;
use handlers::{ApiError, Caller};
use policy::Policy;
use runs::{Runs, RunsConfig};
use tokens::TokenStore;
use webhook::Webhook;

pub(crate) const OPENAPI: &str = include_str!("openapi.json");
pub(crate) const MAX_BUNDLE_BODY: usize = 96 * 1024 * 1024;
const MAX_BODY: usize = 64 * 1024;
const MAX_SSE_STREAMS: usize = 32;
const MAX_UPLOADS: usize = 2;

pub struct ManagerConfig {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub allow_remote_plaintext: bool,
    pub tokens: String,
    pub policy: Policy,
    pub max_queue: usize,
    pub keep_runs: usize,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
    /// Endpoint where pool workers connect, with the cluster token they present.
    pub cluster: Option<(crate::cluster::hub::ClusterServerConfig, String)>,
    /// Serve the browser console under /console/.
    pub console: bool,
}

pub(crate) struct ManagerState {
    pub tokens: TokenStore,
    pub bundles: Arc<BundleStore>,
    pub runs: Arc<Runs>,
    pub policy: Arc<Policy>,
    pub registry: Registry,
    pub max_queue: usize,
    pub general_limiter: RateLimiter,
    pub auth_failures: RateLimiter,
    pub sse_slots: Arc<Semaphore>,
    pub upload_slots: Arc<Semaphore>,
    pub tls: bool,
    pub hub: Option<Arc<crate::cluster::hub::Hub>>,
    pub sessions: console::Sessions,
}

async fn security_headers(State(st): State<Arc<ManagerState>>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"));
    }
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if st.tls {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=63072000; includeSubDomains"));
    }
    res
}

async fn rate_limit(State(st): State<Arc<ManagerState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    if !st.general_limiter.allow(addr.ip()) {
        let mut res = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "rate limit exceeded").into_response();
        res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return res;
    }
    next.run(req).await
}

/// Resolves the bearer token. Repeated failures from one address are throttled.
async fn authenticate(
    State(st): State<Arc<ManagerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim);
    if token.is_none()
        && let Some(cookie) = console::session_cookie(req.headers(), st.sessions.cookie_name())
        && let Some(session) = st.sessions.validate(cookie)
    {
        if let Err(e) = console::check_unsafe(req.method(), req.headers(), &session.csrf, st.tls) {
            return e.into_response();
        }
        req.extensions_mut().insert(Caller(session.principal));
        return next.run(req).await;
    }
    if let Some(t) = token {
        match st.tokens.authenticate(t) {
            Some(p) => {
                req.extensions_mut().insert(Caller(p));
            }
            None => {
                if !st.auth_failures.allow(addr.ip()) {
                    let mut res =
                        ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many failed authentications").into_response();
                    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
                    return res;
                }
                tracing::warn!(client = %addr.ip(), "rejected api token");
                return ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "invalid bearer token").into_response();
            }
        }
    }
    next.run(req).await
}

fn console_routes() -> Router<Arc<ManagerState>> {
    Router::new()
        .route("/", get(console::root))
        .route("/console", get(console::root))
        .route("/console/", get(console::index))
        .route("/console/login", get(console::login_page))
        .route("/console/assets/{name}", get(console::asset))
        .route("/console/api/login", post(console::login))
        .route("/console/api/session", get(console::session))
        .route("/console/api/logout", post(console::logout))
}

fn router(state: Arc<ManagerState>, with_console: bool) -> Router {
    let uploads = Router::new()
        .route("/api/v1/bundles", post(handlers::upload_bundle).get(handlers::list_bundles))
        .layer(DefaultBodyLimit::max(MAX_BUNDLE_BODY));
    Router::new()
        .route("/api/v1/health", get(handlers::health))
        .route("/api/v1/openapi.json", get(handlers::openapi))
        .route("/api/v1/whoami", get(handlers::whoami))
        .route("/api/v1/status", get(handlers::status))
        .route("/api/v1/plans/validate", post(handlers::validate_plan))
        .route("/api/v1/bundles/{id}", get(handlers::get_bundle).delete(handlers::delete_bundle))
        .route("/api/v1/runs", post(handlers::create_run).get(handlers::list_runs))
        .route("/api/v1/runs/{id}", get(handlers::get_run).delete(handlers::delete_run))
        .route("/api/v1/runs/{id}/stop", post(handlers::stop_run))
        .route("/api/v1/runs/{id}/stats", get(handlers::run_stats))
        .route("/api/v1/runs/{id}/compare", get(handlers::compare_run))
        .route("/api/v1/runs/{id}/events", get(handlers::run_events))
        .route("/api/v1/runs/{id}/artifacts/{name}", get(handlers::artifact))
        .merge(if with_console { console_routes() } else { Router::new() })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .merge(uploads)
        .fallback(handlers::not_found)
        .layer(from_fn_with_state(Arc::clone(&state), authenticate))
        .layer(from_fn_with_state(Arc::clone(&state), rate_limit))
        .layer(from_fn_with_state(Arc::clone(&state), security_headers))
        .with_state(state)
}

/// Serves the API and executes queued runs until `shutdown`.
pub async fn serve(cfg: ManagerConfig, registry: Registry, shutdown: CancellationToken) -> Result<()> {
    let tls = match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        _ => return Err(Error::config("tls requires both certificate and key")),
    };
    if !tls && !cfg.bind.ip().is_loopback() && !cfg.allow_remote_plaintext {
        return Err(Error::config("refusing to serve the api over plaintext on a non-loopback address; configure tls"));
    }
    if cfg.policy.targets.is_empty() {
        return Err(Error::config("at least one --allow-target rule is required (use '*' to allow any host)"));
    }
    let webhook = match (&cfg.webhook_url, cfg.webhook_secret.clone()) {
        (Some(u), Some(s)) => Some(Webhook::start(u, s)?),
        (Some(_), None) => return Err(Error::config("webhook url requires BALLISTA_WEBHOOK_SECRET")),
        _ => None,
    };
    let tokens = TokenStore::parse(&cfg.tokens)?;
    std::fs::create_dir_all(&cfg.data_dir)?;
    let bundles = Arc::new(BundleStore::open(cfg.data_dir.join("bundles"))?);
    let runs = Arc::new(Runs::open(cfg.data_dir.join("runs"), RunsConfig { max_queue: cfg.max_queue, keep_runs: cfg.keep_runs })?);
    let policy = Arc::new(cfg.policy);
    let hub = match &cfg.cluster {
        Some((ccfg, token)) => {
            ccfg.validate()?;
            let hub = crate::cluster::hub::Hub::new_pool(token.clone())?;
            let (h, c, sd) = (Arc::clone(&hub), ccfg.clone(), shutdown.clone());
            tokio::spawn(async move {
                if let Err(e) = crate::cluster::hub::serve(h, c, sd).await {
                    tracing::error!(error = %e, "worker endpoint stopped");
                }
            });
            Some(hub)
        }
        None => None,
    };
    let state = Arc::new(ManagerState {
        tokens,
        bundles: Arc::clone(&bundles),
        runs: Arc::clone(&runs),
        policy: Arc::clone(&policy),
        registry: registry.clone(),
        max_queue: cfg.max_queue,
        general_limiter: RateLimiter::new(300, 100.0),
        auth_failures: RateLimiter::new(10, 0.2),
        sse_slots: Arc::new(Semaphore::new(MAX_SSE_STREAMS)),
        upload_slots: Arc::new(Semaphore::new(MAX_UPLOADS)),
        tls,
        hub: hub.clone(),
        sessions: console::Sessions::new(tls),
    });
    let runner = tokio::spawn(Arc::clone(&runs).runner(bundles, policy, registry, webhook, hub.clone(), shutdown.clone()));

    let app = router(state, cfg.console).into_make_service_with_connect_info::<SocketAddr>();
    let handle = axum_server::Handle::new();
    let h = handle.clone();
    let sd = shutdown.clone();
    tokio::spawn(async move {
        sd.cancelled().await;
        h.graceful_shutdown(Some(Duration::from_secs(10)));
    });
    let scheme = if tls { "https" } else { "http" };
    tracing::info!(address = %cfg.bind, "api listening on {scheme}://{}/api/v1", cfg.bind);
    if cfg.console {
        tracing::info!("console at {scheme}://{}/console/ (sign in with an api token)", cfg.bind);
    }
    let served = if let (Some(cert), Some(key)) = (&cfg.tls_cert, &cfg.tls_key) {
        let rc = RustlsConfig::from_pem_file(cert, key).await.map_err(|e| Error::config(format!("cannot load tls material: {e}")))?;
        axum_server::bind_rustls(cfg.bind, rc).handle(handle).serve(app).await
    } else {
        axum_server::bind(cfg.bind).handle(handle).serve(app).await
    };
    shutdown.cancel();
    let _ = runner.await;
    if let Some(h) = &hub {
        h.quit_all();
    }
    served.map_err(Error::from)
}
