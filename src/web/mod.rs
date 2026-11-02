// File: ballista/src/web/mod.rs
mod api;
mod assets;
pub mod auth;
pub(crate) mod security;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum_server::tls_rustls::RustlsConfig;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::error::{Error, Result};
use auth::Auth;
use security::RateLimiter;

const MAX_BODY: usize = 16 * 1024;
const MAX_SSE_STREAMS: usize = 16;
const MIN_METRICS_TOKEN: usize = 24;

#[derive(Debug, Clone)]
pub struct WebConfig {
    pub bind: SocketAddr,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub allow_remote_plaintext: bool,
    pub admin_hash: Option<String>,
    pub metrics_token: Option<String>,
    pub public_origin: Option<String>,
}

pub(crate) struct Settings {
    pub tls: bool,
    pub cookie_name: &'static str,
    pub metrics_token: Option<String>,
    pub public_origin: Option<String>,
}

pub(crate) struct AppState {
    pub engine: Engine,
    pub auth: Arc<Auth>,
    pub settings: Settings,
    pub general_limiter: RateLimiter,
    pub login_limiter: RateLimiter,
    pub sse_slots: Arc<Semaphore>,
}

fn validate_origin(o: &str) -> Result<String> {
    let url = url::Url::parse(o).map_err(|_| Error::config("public origin must be an absolute url"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || url.path() != "/" {
        return Err(Error::config("public origin must be scheme://host[:port]"));
    }
    Ok(url.origin().ascii_serialization())
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(api::index))
        .route("/login", get(api::login_page))
        .route("/healthz", get(api::health))
        .route("/assets/{name}", get(assets::asset))
        .route("/metrics", get(api::metrics))
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/session", get(api::session))
        .route("/api/state", get(api::snapshot))
        .route("/api/stream", get(api::stream))
        .route("/api/run/start", post(api::start))
        .route("/api/run/stop", post(api::stop))
        .route("/api/run/target", post(api::set_target))
        .route("/api/run/resume", post(api::resume_shape))
        .route("/api/stats/reset", post(api::reset_stats))
        .route("/api/report.json", get(api::report_json))
        .route("/api/report.html", get(api::report_html))
        .route("/api/stats.csv", get(api::stats_csv))
        .route("/api/logs", get(api::logs))
        .route("/api/history.csv", get(api::history_csv))
        .route("/api/failures.csv", get(api::failures_csv))
        .fallback(api::not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(from_fn_with_state(Arc::clone(&state), security::same_origin))
        .layer(from_fn_with_state(Arc::clone(&state), security::rate_limit))
        .layer(from_fn_with_state(Arc::clone(&state), security::headers))
        .with_state(state)
}

/// Serves the dashboard until `shutdown` is cancelled.
pub async fn serve(engine: Engine, cfg: WebConfig, shutdown: CancellationToken) -> Result<()> {
    let tls = match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        _ => return Err(Error::config("tls requires both certificate and key")),
    };
    if !tls && !cfg.bind.ip().is_loopback() && !cfg.allow_remote_plaintext {
        return Err(Error::config("refusing to serve the dashboard over plaintext on a non-loopback address; configure tls"));
    }
    if let Some(t) = &cfg.metrics_token
        && t.len() < MIN_METRICS_TOKEN
    {
        return Err(Error::config("metrics token must be at least 24 characters"));
    }
    let public_origin = cfg.public_origin.as_deref().map(validate_origin).transpose()?;

    let hash = match cfg.admin_hash.clone() {
        Some(h) => h,
        None => {
            let pw = auth::generate_password()?;
            eprintln!("No admin password hash configured. One-time dashboard password: {pw}");
            auth::hash_password(&pw)?
        }
    };
    let state = Arc::new(AppState {
        engine,
        auth: Arc::new(Auth::new(hash)?),
        settings: Settings {
            tls,
            cookie_name: if tls { "__Host-bsession" } else { "bsession" },
            metrics_token: cfg.metrics_token.clone(),
            public_origin,
        },
        general_limiter: RateLimiter::new(200, 50.0),
        login_limiter: RateLimiter::new(5, 1.0 / 12.0),
        sse_slots: Arc::new(Semaphore::new(MAX_SSE_STREAMS)),
    });
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();

    let handle = axum_server::Handle::new();
    let h = handle.clone();
    tokio::spawn(async move {
        shutdown.cancelled().await;
        h.graceful_shutdown(Some(Duration::from_secs(5)));
    });

    let scheme = if tls { "https" } else { "http" };
    tracing::info!(address = %cfg.bind, "dashboard listening on {scheme}://{}", cfg.bind);
    if !tls && !cfg.bind.ip().is_loopback() {
        tracing::warn!("dashboard served over plaintext on a non-loopback address");
    }

    if let (Some(cert), Some(key)) = (&cfg.tls_cert, &cfg.tls_key) {
        let rc = RustlsConfig::from_pem_file(cert, key).await.map_err(|e| Error::config(format!("cannot load tls material: {e}")))?;
        axum_server::bind_rustls(cfg.bind, rc).handle(handle).serve(app).await?;
    } else {
        axum_server::bind(cfg.bind).handle(handle).serve(app).await?;
    }
    Ok(())
}
