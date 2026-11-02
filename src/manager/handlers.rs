// File: ballista/src/manager/handlers.rs
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::{Deserialize, Serialize};

use super::bundles::{BundleInfo, BundleStore, BundleUpload, PLAN_FILE, valid_id};
use super::runs::{RunRecord, RunRequest, RunState, now_rfc3339, valid_run_id};
use super::tokens::{Principal, Scope};
use super::{ManagerState, OPENAPI};
use crate::config::Plan;
use crate::engine::Engine;
use crate::error::{Error, sanitize};

const MAX_PAGE: usize = 200;

/// JSON error body: `{"error":{"code":"...","message":"..."}}`.
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", format!("{what} not found"))
    }

    fn invalid(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", sanitize(&e.to_string(), 800))
    }

    fn conflict(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", sanitize(&e.to_string(), 300))
    }

    fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %sanitize(&e.to_string(), 300), "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error")
    }
}

#[derive(Serialize)]
struct ErrorInner<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorInner<'a>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody { error: ErrorInner { code: self.code, message: &self.message } };
        let mut res = (self.status, Json(body)).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            res.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"api\""));
        }
        res
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// Authenticated caller, inserted by the auth middleware.
#[derive(Clone)]
pub struct Caller(pub Principal);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> ApiResult<Self> {
        parts
            .extensions
            .get::<Caller>()
            .cloned()
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid bearer token"))
    }
}

impl Caller {
    fn require(&self, scope: Scope) -> ApiResult<()> {
        if self.0.scope >= scope {
            Ok(())
        } else {
            Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", format!("token lacks the '{}' scope", scope.as_str())))
        }
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(ApiError::internal)
}

#[derive(Serialize)]
pub struct Health {
    status: &'static str,
    version: &'static str,
}

pub async fn health() -> Json<Health> {
    Json(Health { status: "ok", version: env!("CARGO_PKG_VERSION") })
}

pub async fn openapi() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], OPENAPI).into_response()
}

#[derive(Serialize)]
pub struct WhoAmI {
    name: String,
    scope: Scope,
}

pub async fn whoami(caller: Caller) -> Json<WhoAmI> {
    Json(WhoAmI { name: caller.0.name, scope: caller.0.scope })
}

#[derive(Serialize)]
pub struct ServerStatus {
    version: &'static str,
    active_run: Option<String>,
    queued: usize,
    limits: Limits,
    /// Pool workers connected to this server (empty without a worker pool).
    workers: Vec<crate::cluster::hub::WorkerInfo>,
}

#[derive(Serialize)]
struct Limits {
    max_users: f64,
    max_run_time_secs: u64,
    max_queue: usize,
}

pub async fn status(State(st): State<Arc<ManagerState>>, caller: Caller) -> ApiResult<Json<ServerStatus>> {
    caller.require(Scope::Read)?;
    let (items, _) = st.runs.list(Some(RunState::Running), 1, None);
    let (queued, _) = st.runs.list(Some(RunState::Queued), MAX_PAGE, None);
    Ok(Json(ServerStatus {
        version: env!("CARGO_PKG_VERSION"),
        active_run: items.first().map(|r| r.id.clone()),
        queued: queued.len(),
        limits: Limits { max_users: st.policy.max_users, max_run_time_secs: st.policy.max_run_time.as_secs(), max_queue: st.max_queue },
        workers: st.hub.as_ref().map(|h| h.infos()).unwrap_or_default(),
    }))
}

/// Loads, policy-checks and compiles a plan without starting it.
fn load_checked(st: &ManagerState, dir: &std::path::Path, overrides: Option<&super::runs::RunOverrides>) -> crate::error::Result<Plan> {
    let mut plan = Plan::load(&dir.join(PLAN_FILE))?;
    if let Some(o) = overrides {
        plan.apply_overrides(&o.to_overrides()?)?;
    }
    st.policy.check(&plan, dir, overrides.is_some())?;
    plan.report = crate::config::ReportConfig { percentiles: plan.report.percentiles.clone(), ..Default::default() };
    drop(Engine::new(plan.clone(), &st.registry)?);
    Ok(plan)
}

pub async fn upload_bundle(State(st): State<Arc<ManagerState>>, caller: Caller, req: Request) -> ApiResult<Response> {
    caller.require(Scope::Run)?;
    let ct = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.starts_with("application/json") {
        return Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", "send application/json"));
    }
    let bytes = axum::body::to_bytes(req.into_body(), super::MAX_BUNDLE_BODY)
        .await
        .map_err(|_| ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "bundle body too large"))?;
    let _permit = st
        .upload_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy", "too many concurrent uploads"))?;
    let up: BundleUpload = serde_json::from_slice(&bytes).map_err(ApiError::invalid)?;
    drop(bytes);
    let who = caller.0.name.clone();
    let st2 = Arc::clone(&st);
    let rt = tokio::runtime::Handle::current();
    let (info, created) = blocking(move || -> ApiResult<(BundleInfo, bool)> {
        let _guard = rt.enter();
        let prepared = st2.bundles.prepare(up).map_err(ApiError::invalid)?;
        if let Some(existing) = st2.bundles.info(&prepared.id) {
            return Ok((existing, false));
        }
        let staging = st2.bundles.stage(&prepared).map_err(ApiError::internal)?;
        let plan = match load_checked(&st2, &staging, None) {
            Ok(p) => p,
            Err(e) => {
                st2.bundles.discard(&staging);
                return Err(ApiError::invalid(e));
            }
        };
        let (files, size) = BundleStore::file_list(&prepared);
        let info = BundleInfo {
            id: prepared.id.clone(),
            plan_name: plan.display_name().to_owned(),
            scenarios: plan.scenarios.iter().map(|s| s.name.clone()).collect(),
            files,
            size,
            labels: prepared.labels.clone(),
            created_at: now_rfc3339(),
            created_by: who,
        };
        let created = st2.bundles.commit(&staging, &info).map_err(ApiError::internal)?;
        Ok((if created { info } else { st2.bundles.info(&prepared.id).unwrap_or(info) }, created))
    })
    .await??;
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    if created {
        tracing::info!(bundle = %info.id, by = %caller.0.name, "bundle stored");
    }
    Ok((status, Json(info)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidateRequest {
    plan: super::document::PlanSource,
    /// Run overrides to apply before checking limits, as they would be passed to a run.
    #[serde(default)]
    overrides: Option<super::runs::RunOverrides>,
}

/// Dry validation of a plan: structure, steps, targets and limits. Returns the files a bundle must carry.
pub async fn validate_plan(
    State(st): State<Arc<ManagerState>>,
    caller: Caller,
    body: Json<ValidateRequest>,
) -> ApiResult<Json<super::document::PlanCheck>> {
    caller.require(Scope::Run)?;
    let Json(req) = body;
    let st2 = Arc::clone(&st);
    let out = blocking(move || super::document::check(&req.plan, req.overrides.as_ref(), &st2.policy)).await?;
    out.map(Json).map_err(ApiError::invalid)
}

#[derive(Serialize)]
pub struct Page<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
}

pub async fn list_bundles(State(st): State<Arc<ManagerState>>, caller: Caller) -> ApiResult<Json<Page<BundleInfo>>> {
    caller.require(Scope::Read)?;
    let st2 = Arc::clone(&st);
    let mut items = blocking(move || st2.bundles.list()).await?;
    items.truncate(MAX_PAGE);
    Ok(Json(Page { items, next_cursor: None }))
}

pub async fn get_bundle(State(st): State<Arc<ManagerState>>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<BundleInfo>> {
    caller.require(Scope::Read)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found("bundle"));
    }
    st.bundles.info(&id).map(Json).ok_or_else(|| ApiError::not_found("bundle"))
}

pub async fn delete_bundle(State(st): State<Arc<ManagerState>>, caller: Caller, Path(id): Path<String>) -> ApiResult<StatusCode> {
    caller.require(Scope::Admin)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found("bundle"));
    }
    if st.runs.bundle_in_use(&id) {
        return Err(ApiError::conflict("bundle is used by a queued or running run"));
    }
    match st.bundles.delete(&id) {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found("bundle")),
        Err(e) => Err(ApiError::internal(e)),
    }
}

#[derive(Serialize)]
pub struct RunView {
    #[serde(flatten)]
    record: RunRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_position: Option<usize>,
}

fn view(st: &ManagerState, record: RunRecord) -> RunView {
    let queue_position = (record.state == RunState::Queued).then(|| st.runs.queue_position(&record.id)).flatten();
    RunView { record, queue_position }
}

fn idempotency_key(h: &HeaderMap) -> ApiResult<Option<String>> {
    match h.get("idempotency-key") {
        None => Ok(None),
        Some(v) => {
            let s = v.to_str().map_err(|_| ApiError::invalid("Idempotency-Key must be printable ASCII"))?;
            if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(ApiError::invalid("Idempotency-Key must be 1..128 printable characters"));
            }
            Ok(Some(s.to_owned()))
        }
    }
}

pub async fn create_run(
    State(st): State<Arc<ManagerState>>,
    caller: Caller,
    headers: HeaderMap,
    body: Json<RunRequest>,
) -> ApiResult<Response> {
    caller.require(Scope::Run)?;
    let Json(req) = body;
    let key = idempotency_key(&headers)?;
    if !valid_id(&req.bundle) {
        return Err(ApiError::invalid("bundle must be a bundle id"));
    }
    let dir = st.bundles.dir(&req.bundle).ok_or_else(|| ApiError::not_found("bundle"))?;
    let st2 = Arc::clone(&st);
    let overrides = req.overrides.clone();
    let rt = tokio::runtime::Handle::current();
    let plan = blocking(move || {
        let _guard = rt.enter();
        load_checked(&st2, &dir, Some(&overrides))
    })
    .await?
    .map_err(ApiError::invalid)?;
    let (rec, created) = st.runs.submit(req, plan.display_name().to_owned(), &caller.0.name, key).map_err(|e| match e {
        Error::Engine(m) if m.contains("queue is full") => ApiError::new(StatusCode::TOO_MANY_REQUESTS, "queue_full", m),
        other => ApiError::invalid(other),
    })?;
    if created {
        tracing::info!(run = %rec.id, bundle = %rec.bundle, by = %caller.0.name, "run queued");
    }
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(view(&st, rec))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    state: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

pub async fn list_runs(State(st): State<Arc<ManagerState>>, caller: Caller, Query(q): Query<ListQuery>) -> ApiResult<Json<Page<RunView>>> {
    caller.require(Scope::Read)?;
    let state = match q.state.as_deref() {
        Some(s) => Some(RunState::parse(s).ok_or_else(|| ApiError::invalid("unknown state"))?),
        None => None,
    };
    let before = match q.cursor.as_deref() {
        Some(c) => Some(c.parse::<u64>().map_err(|_| ApiError::invalid("invalid cursor"))?),
        None => None,
    };
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let (items, next) = st.runs.list(state, limit, before);
    Ok(Json(Page { items: items.into_iter().map(|r| view(&st, r)).collect(), next_cursor: next.map(|n| n.to_string()) }))
}

fn run_id(id: &str) -> ApiResult<&str> {
    if valid_run_id(id) { Ok(id) } else { Err(ApiError::not_found("run")) }
}

pub async fn get_run(State(st): State<Arc<ManagerState>>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<RunView>> {
    caller.require(Scope::Read)?;
    let rec = st.runs.get(run_id(&id)?).ok_or_else(|| ApiError::not_found("run"))?;
    Ok(Json(view(&st, rec)))
}

pub async fn stop_run(State(st): State<Arc<ManagerState>>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<RunView>> {
    caller.require(Scope::Run)?;
    let id = run_id(&id)?;
    if st.runs.get(id).is_none() {
        return Err(ApiError::not_found("run"));
    }
    let rec = st.runs.stop(id, &caller.0.name).map_err(ApiError::conflict)?;
    tracing::info!(run = %id, by = %caller.0.name, "stop requested");
    Ok(Json(view(&st, rec)))
}

pub async fn delete_run(State(st): State<Arc<ManagerState>>, caller: Caller, Path(id): Path<String>) -> ApiResult<StatusCode> {
    caller.require(Scope::Admin)?;
    match st.runs.delete(run_id(&id)?) {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found("run")),
        Err(e) => Err(ApiError::conflict(e)),
    }
}

/// Live snapshot of a running run, or the final snapshot of a finished one.
pub async fn run_stats(State(st): State<Arc<ManagerState>>, caller: Caller, Path(id): Path<String>) -> ApiResult<Response> {
    caller.require(Scope::Read)?;
    let id = run_id(&id)?;
    if let Some(engine) = st.runs.engine(id) {
        return Ok(Json(&*engine.snapshot()).into_response());
    }
    let rec = st.runs.get(id).ok_or_else(|| ApiError::not_found("run"))?;
    if !rec.state.terminal() {
        return Err(ApiError::new(StatusCode::CONFLICT, "not_started", "run has not started yet"));
    }
    let path = st.runs.run_dir(id).map(|d| d.join("report.json")).ok_or_else(|| ApiError::not_found("run"))?;
    let raw = tokio::fs::read(&path).await.map_err(|_| ApiError::not_found("stats"))?;
    let v: serde_json::Value = serde_json::from_slice(&raw).map_err(ApiError::internal)?;
    Ok(Json(v.get("snapshot").cloned().unwrap_or(serde_json::Value::Null)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareQuery {
    baseline: Option<String>,
    max_regression_pct: Option<f64>,
    metrics: Option<String>,
    min_samples: Option<u64>,
    min_change_ms: Option<f64>,
    max_error_rate_increase: Option<f64>,
}

/// Compares a finished run with a baseline (default: the latest earlier passed run of the same plan).
pub async fn compare_run(
    State(st): State<Arc<ManagerState>>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<CompareQuery>,
) -> ApiResult<Json<super::compare::Comparison>> {
    use super::compare::{Metric, RegressionGate};
    caller.require(Scope::Read)?;
    let rec = st.runs.get(run_id(&id)?).ok_or_else(|| ApiError::not_found("run"))?;
    if !rec.state.terminal() || rec.summary.is_none() {
        return Err(ApiError::new(StatusCode::CONFLICT, "not_started", "run has no results yet"));
    }
    let mut gate = rec.regression.clone().unwrap_or_default();
    if let Some(b) = q.baseline {
        gate.baseline = b;
    }
    if let Some(p) = q.max_regression_pct {
        gate.max_regression_pct = p;
    }
    if let Some(m) = q.metrics {
        gate.metrics = m
            .split(',')
            .map(|x| serde_json::from_value::<Metric>(serde_json::Value::String(x.trim().to_owned())))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| ApiError::invalid("metrics: p50, p95, p99, avg, error_rate, rps"))?;
    }
    if let Some(n) = q.min_samples {
        gate.min_samples = n;
    }
    if let Some(v) = q.min_change_ms {
        gate.min_change_ms = v;
    }
    if let Some(v) = q.max_error_rate_increase {
        gate.max_error_rate_increase = v;
    }
    gate.validate().map_err(ApiError::invalid)?;
    let runs = Arc::clone(&st.runs);
    let out = blocking(move || -> ApiResult<super::compare::Comparison> {
        let current = runs.stored_snapshot(&rec.id).ok_or_else(|| ApiError::not_found("report"))?;
        Ok(runs.compare_with(&rec, &current, &RegressionGate { ..gate }))
    })
    .await??;
    Ok(Json(out))
}

const ARTIFACTS: &[(&str, &str, &str)] = &[
    ("report.json", "report.json", "application/json"),
    ("report.html", "report.html", "text/html; charset=utf-8"),
    ("stats.csv", "report_stats.csv", "text/csv; charset=utf-8"),
    ("history.csv", "report_history.csv", "text/csv; charset=utf-8"),
    ("failures.csv", "report_failures.csv", "text/csv; charset=utf-8"),
];

pub async fn artifact(
    State(st): State<Arc<ManagerState>>,
    caller: Caller,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Response> {
    caller.require(Scope::Read)?;
    let id = run_id(&id)?;
    let (_, file, ct) = ARTIFACTS.iter().find(|(n, _, _)| *n == name).ok_or_else(|| ApiError::not_found("artifact"))?;
    let path = st.runs.run_dir(id).map(|d| d.join(file)).ok_or_else(|| ApiError::not_found("run"))?;
    let data = tokio::fs::read(&path).await.map_err(|_| ApiError::not_found("artifact"))?;
    let mut res = ([(header::CONTENT_TYPE, *ct)], data).into_response();
    if name == "report.html" {
        res.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; frame-ancestors 'none'"),
        );
    } else {
        let disp = format!("attachment; filename=\"{id}-{name}\"");
        if let Ok(v) = HeaderValue::from_str(&disp) {
            res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    Ok(res)
}

enum Phase {
    Start,
    Live,
    Done,
}

/// Server-sent events: `run` on every state change, `stats` once per engine tick, `end` when finished.
pub async fn run_events(
    State(st): State<Arc<ManagerState>>,
    caller: Caller,
    Path(id): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    caller.require(Scope::Read)?;
    let id = run_id(&id)?.to_owned();
    if st.runs.get(&id).is_none() {
        return Err(ApiError::not_found("run"));
    }
    let permit = Arc::clone(&st.sse_slots)
        .try_acquire_owned()
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy", "too many live streams"))?;
    let versions = st.runs.subscribe();
    struct S {
        st: Arc<ManagerState>,
        id: String,
        versions: tokio::sync::watch::Receiver<u64>,
        engine: Option<(Engine, tokio::sync::watch::Receiver<Arc<crate::metrics::Snapshot>>)>,
        phase: Phase,
        _permit: tokio::sync::OwnedSemaphorePermit,
    }
    let init = S { st, id, versions, engine: None, phase: Phase::Start, _permit: permit };
    let stream = futures_util::stream::unfold(init, |mut s| async move {
        let event = |name: &str, v: &dyn erased_json::ToJson| Event::default().event(name).data(v.json());
        loop {
            match s.phase {
                Phase::Done => return None,
                Phase::Start => {
                    s.phase = Phase::Live;
                    let rec = s.st.runs.get(&s.id)?;
                    s.versions.borrow_and_update();
                    return Some((Ok(event("run", &view(&s.st, rec))), s));
                }
                Phase::Live => {
                    if s.engine.is_none()
                        && let Some(e) = s.st.runs.engine(&s.id)
                    {
                        let rx = e.subscribe();
                        s.engine = Some((e, rx));
                    }
                    let rec = s.st.runs.get(&s.id)?;
                    if rec.state.terminal() {
                        s.phase = Phase::Done;
                        return Some((Ok(event("end", &view(&s.st, rec))), s));
                    }
                    let tick = async {
                        match s.engine.as_mut() {
                            Some((_, rx)) => rx.changed().await.is_ok(),
                            None => std::future::pending().await,
                        }
                    };
                    tokio::select! {
                        changed = tick => {
                            if !changed {
                                s.engine = None;
                                continue;
                            }
                            let snap = s.engine.as_mut().map(|(_, rx)| rx.borrow_and_update().clone())?;
                            return Some((Ok(event("stats", &*snap)), s));
                        }
                        r = s.versions.changed() => {
                            r.ok()?;
                            s.versions.borrow_and_update();
                            let rec = s.st.runs.get(&s.id)?;
                            if rec.state.terminal() {
                                continue;
                            }
                            return Some((Ok(event("run", &view(&s.st, rec))), s));
                        }
                        () = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    }
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

mod erased_json {
    pub trait ToJson {
        fn json(&self) -> String;
    }

    impl<T: serde::Serialize> ToJson for T {
        fn json(&self) -> String {
            serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
        }
    }
}

pub async fn not_found() -> ApiError {
    ApiError::not_found("route")
}
