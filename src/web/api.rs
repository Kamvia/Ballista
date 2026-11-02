// File: ballista/src/web/api.rs
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use futures_util::Stream;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::assets;
use super::auth::{Authed, cookie_value};
use super::security::ct_eq;
use crate::config::LoadMode;
use crate::engine::StartOptions;
use crate::metrics::{Snapshot, prometheus};
use crate::report::Report;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: &'static str,
    detail: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message, detail: None }
    }

    fn with_detail(status: StatusCode, message: &'static str, detail: String) -> Self {
        Self { status, message, detail: Some(detail) }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody { error: self.message, detail: self.detail.as_deref() };
        (self.status, Json(body)).into_response()
    }
}

#[derive(Serialize)]
pub struct SessionInfo {
    csrf: String,
    plan: String,
    target: String,
    mode: LoadMode,
    profile_secs: Option<f64>,
    profile_peak: Option<f64>,
    scenarios: Vec<String>,
    tags: Vec<String>,
    exclude_tags: Vec<String>,
    iteration_limit: Option<u64>,
    controller: bool,
    hosts: Vec<String>,
}

pub async fn session(State(state): State<Arc<AppState>>, auth: Authed) -> Json<SessionInfo> {
    let plan = state.engine.plan();
    let mut target = plan.target.base_url.clone();
    target.set_query(None);
    target.set_fragment(None);
    Json(SessionInfo {
        csrf: auth.csrf,
        plan: plan.display_name().to_owned(),
        target: target.to_string(),
        mode: plan.load.mode,
        profile_secs: state.engine.shape().total().map(|d| d.as_secs_f64()),
        profile_peak: state.engine.shape().peak(),
        scenarios: state.engine.scenario_names(),
        tags: plan.load.tags.clone(),
        exclude_tags: plan.load.exclude_tags.clone(),
        iteration_limit: plan.load.iterations,
        controller: state.engine.hub().is_some(),
        hosts: {
            let scheme = plan.target.base_url.scheme().to_owned();
            let port = plan.target.base_url.port().map(|p| format!(":{p}")).unwrap_or_default();
            let mut v = vec![target.to_string()];
            v.extend(plan.target.allowed_hosts.iter().map(|h| format!("{scheme}://{h}{port}/")));
            v
        },
    })
}

pub async fn snapshot(State(state): State<Arc<AppState>>, _auth: Authed) -> Json<Arc<Snapshot>> {
    Json(state.engine.snapshot())
}

pub async fn stream(
    State(state): State<Arc<AppState>>,
    auth: Authed,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let permit = Arc::clone(&state.sse_slots)
        .try_acquire_owned()
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "too many live streams"))?;
    let rx = state.engine.subscribe();
    let token = auth.token;
    let st = Arc::clone(&state);
    let stream = futures_util::stream::unfold((rx, permit, true), move |(mut rx, permit, first)| {
        let st = Arc::clone(&st);
        let token = token.clone();
        async move {
            if !first && rx.changed().await.is_err() {
                return None;
            }
            if !st.auth.is_valid(&token) {
                return None;
            }
            let snap = rx.borrow_and_update().clone();
            let data = serde_json::to_string(&*snap).unwrap_or_else(|_| "{}".to_owned());
            Some((Ok(Event::default().event("snapshot").data(data)), (rx, permit, false)))
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[derive(Serialize)]
struct Ack {
    ok: bool,
}

fn engine_result(r: crate::error::Result<()>) -> Response {
    match r {
        Ok(()) => (StatusCode::ACCEPTED, Json(Ack { ok: true })).into_response(),
        Err(e) => ApiError::with_detail(StatusCode::CONFLICT, "operation rejected", e.to_string()).into_response(),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StartBody {
    #[serde(default)]
    users: Option<f64>,
    #[serde(default)]
    spawn_rate: Option<f64>,
    #[serde(default)]
    run_time_secs: Option<f64>,
    #[serde(default)]
    scenarios: Option<Vec<String>>,
    #[serde(default)]
    host: Option<String>,
}

const MAX_RUN_SECS: f64 = 365.0 * 24.0 * 3600.0;

pub async fn start(State(state): State<Arc<AppState>>, _auth: Authed, body: Option<Json<StartBody>>) -> Response {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let finite_pos = |v: Option<f64>| v.is_none_or(|x| x.is_finite() && x >= 0.0);
    if !finite_pos(b.users) || !finite_pos(b.spawn_rate) || !finite_pos(b.run_time_secs) {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "values must be non-negative numbers").into_response();
    }
    if b.spawn_rate == Some(0.0) || b.run_time_secs.is_some_and(|t| t <= 0.0 || t > MAX_RUN_SECS) {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "spawn rate and run time must be positive").into_response();
    }
    let opts = StartOptions {
        users: b.users,
        spawn_rate: b.spawn_rate,
        run_time: b.run_time_secs.map(Duration::from_secs_f64),
        scenarios: b.scenarios.filter(|s| !s.is_empty()),
        host: b.host.filter(|h| !h.trim().is_empty()),
    };
    if opts.host.as_ref().is_some_and(|h| h.len() > 512) {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid host").into_response();
    }
    if opts.scenarios.as_ref().is_some_and(|s| s.len() > 128 || s.iter().any(|n| !crate::config::is_valid_name(n))) {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid scenario selection").into_response();
    }
    tracing::info!(users = ?opts.users, spawn_rate = ?opts.spawn_rate, run_time = ?opts.run_time, "run start requested from dashboard");
    engine_result(state.engine.start_with(opts))
}

pub async fn stop(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    tracing::info!("run stop requested from dashboard");
    engine_result(state.engine.stop())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetBody {
    target: f64,
    #[serde(default)]
    ramp: f64,
}

pub async fn set_target(State(state): State<Arc<AppState>>, _auth: Authed, Json(b): Json<TargetBody>) -> Response {
    if !b.target.is_finite() || !b.ramp.is_finite() {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "values must be finite numbers").into_response();
    }
    match state.engine.set_target(b.target, b.ramp) {
        Ok(()) => (StatusCode::ACCEPTED, Json(Ack { ok: true })).into_response(),
        Err(e) => ApiError::with_detail(StatusCode::UNPROCESSABLE_ENTITY, "invalid target", e.to_string()).into_response(),
    }
}

pub async fn resume_shape(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    engine_result(state.engine.resume_shape())
}

pub async fn reset_stats(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    state.engine.reset_stats();
    (StatusCode::ACCEPTED, Json(Ack { ok: true })).into_response()
}

fn attachment(content_type: &'static str, filename: &'static str, body: String) -> Response {
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")).unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    res
}

pub async fn report_json(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    let report = Report::from_engine(&state.engine);
    attachment("application/json", "load-report.json", report.to_json())
}

pub async fn report_html(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    let report = Report::from_engine(&state.engine);
    attachment("text/html; charset=utf-8", "load-report.html", report.to_html())
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    after: u64,
}

pub async fn logs(_auth: Authed, axum::extract::Query(q): axum::extract::Query<LogsQuery>) -> Json<Vec<crate::logbuf::LogEntry>> {
    Json(crate::logbuf::since(q.after, 500))
}

pub async fn stats_csv(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    attachment("text/csv; charset=utf-8", "load-stats.csv", crate::csv::stats_csv(&state.engine.full_snapshot()))
}

pub async fn history_csv(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    attachment("text/csv; charset=utf-8", "load-history.csv", crate::csv::history_csv(&state.engine.full_snapshot()))
}

pub async fn failures_csv(State(state): State<Arc<AppState>>, _auth: Authed) -> Response {
    attachment("text/csv; charset=utf-8", "load-failures.csv", crate::csv::failures_csv(&state.engine.full_snapshot()))
}

pub async fn metrics(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(expected) = state.settings.metrics_token.as_deref() else {
        return ApiError::new(StatusCode::NOT_FOUND, "not found").into_response();
    };
    let provided = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    if !ct_eq(provided.as_bytes(), expected.as_bytes()) {
        let mut r = ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return r;
    }
    let body = prometheus::render(&state.engine.snapshot());
    let mut res = body.into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"));
    res
}

pub async fn index(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let authed = cookie_value(&headers, state.settings.cookie_name).is_some_and(|t| state.auth.is_valid(t));
    if !authed {
        return Redirect::to("/login").into_response();
    }
    assets::html(assets::INDEX_HTML)
}

pub async fn login_page() -> Response {
    assets::html(assets::LOGIN_HTML)
}

pub async fn health() -> &'static str {
    "ok"
}

pub async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not found")
}
