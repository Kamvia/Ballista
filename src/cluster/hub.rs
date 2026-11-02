// File: ballista/src/cluster/hub.rs
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum_server::tls_rustls::RustlsConfig;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::{
    CLUSTER_PATH, LoadFile, LoadOverrides, MAX_FRAME, POOL_FINGERPRINT, PROTOCOL_VERSION, ToController, ToWorker, split_limit,
    split_target, validate_token,
};
use crate::config::LoadMode;
use crate::error::{Error, Result, sanitize};
use crate::metrics::export;
use crate::monitor::GeneratorHealth;
use crate::vu::RunCtx;
use crate::web::security::{RateLimiter, ct_eq};

const MAX_WORKERS: usize = 512;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const TARGET_REFRESH: Duration = Duration::from_secs(2);
const OUTBOX: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Ready,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerInfo {
    pub id: u64,
    pub name: String,
    pub address: String,
    pub cores: u32,
    pub state: WorkerState,
    pub users: u64,
    pub iterations: u64,
    pub target: f64,
    pub cpu_pct: Option<f64>,
    pub scheduler_lag_ms: f64,
    pub saturated: bool,
    pub last_report_secs: f64,
}

struct Slot {
    name: String,
    addr: IpAddr,
    cores: u32,
    tx: mpsc::Sender<ToWorker>,
    state: WorkerState,
    active: u64,
    iterations: u64,
    health: GeneratorHealth,
    last_report: Instant,
    sent_target: Option<(f64, Instant)>,
    scenario_users: Vec<(String, u64)>,
    /// Fingerprint of the plan this worker holds (fixed workers: from hello; pool workers: from `Loaded`).
    loaded: Option<String>,
}

struct Binding {
    run: u64,
    ctx: Arc<RunCtx>,
    participants: Vec<u64>,
    mode: LoadMode,
    scenarios: Option<Vec<String>>,
    host: Option<String>,
}

/// Controller side of a distributed run: tracks workers, distributes targets and merges their reports.
pub struct Hub {
    token: String,
    /// Plan the current run executes; workers must hold exactly this plan to take part.
    fingerprint: Mutex<String>,
    /// Pool hubs accept plan-less workers and ship bundles to them per run.
    pool: bool,
    load_seq: AtomicU64,
    load_pending_id: Mutex<u64>,
    pending_loads: Mutex<std::collections::HashSet<u64>>,
    workers: Mutex<HashMap<u64, Slot>>,
    binding: Mutex<Option<Binding>>,
    next_id: AtomicU64,
    run_seq: AtomicU64,
    retired_iterations: AtomicU64,
    task_deltas: Mutex<Vec<(String, String, u64)>>,
    shared: Mutex<HashMap<String, serde_json::Value>>,
    changed: Notify,
    auth_limiter: RateLimiter,
}

/// Aggregated worker figures for the controller's statistics tick.
pub(crate) struct RemoteTotals {
    pub active: u64,
    pub iterations: u64,
    pub health: Option<GeneratorHealth>,
}

impl Hub {
    pub fn new(token: String, fingerprint: String) -> Result<Arc<Self>> {
        Self::build(token, fingerprint, false)
    }

    /// A hub for plan-less pool workers; bundles are distributed with [`Hub::load`] before each run.
    pub fn new_pool(token: String) -> Result<Arc<Self>> {
        Self::build(token, String::new(), true)
    }

    fn build(token: String, fingerprint: String, pool: bool) -> Result<Arc<Self>> {
        validate_token(&token)?;
        Ok(Arc::new(Self {
            token,
            fingerprint: Mutex::new(fingerprint),
            pool,
            load_seq: AtomicU64::new(1),
            load_pending_id: Mutex::new(0),
            pending_loads: Mutex::new(std::collections::HashSet::new()),
            workers: Mutex::new(HashMap::new()),
            binding: Mutex::new(None),
            next_id: AtomicU64::new(1),
            run_seq: AtomicU64::new(1),
            retired_iterations: AtomicU64::new(0),
            task_deltas: Mutex::new(Vec::new()),
            shared: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            auth_limiter: RateLimiter::new(10, 0.2),
        }))
    }

    /// Idle workers that hold the current plan.
    pub fn ready_count(&self) -> usize {
        let fp = self.fingerprint.lock().clone();
        self.workers.lock().values().filter(|w| w.state != WorkerState::Running && w.loaded.as_deref() == Some(fp.as_str())).count()
    }

    /// Idle workers regardless of the plan they hold.
    pub fn idle_count(&self) -> usize {
        self.workers.lock().values().filter(|w| w.state != WorkerState::Running).count()
    }

    pub fn is_pool(&self) -> bool {
        self.pool
    }

    /// Waits until at least `n` workers are connected and idle (pool hubs: regardless of plan).
    pub async fn wait_for_idle(&self, n: usize, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            let have = self.idle_count();
            if have >= n {
                return Ok(());
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(Error::Engine(format!("only {have} of {n} workers available before timeout")));
            }
        }
    }

    /// Ships a bundle to every idle pool worker and waits for their answers.
    /// Returns how many workers now hold a plan matching `expected`.
    pub async fn load(
        &self,
        plan: String,
        files: Vec<LoadFile>,
        overrides: LoadOverrides,
        expected: String,
        timeout: Duration,
    ) -> Result<usize> {
        if !self.pool {
            return Err(Error::Engine("bundles are only distributed to pool workers".into()));
        }
        let load = self.load_seq.fetch_add(1, Ordering::Relaxed);
        *self.fingerprint.lock() = expected.clone();
        *self.load_pending_id.lock() = load;
        let targets: Vec<u64> = {
            let mut workers = self.workers.lock();
            let mut pending = self.pending_loads.lock();
            pending.clear();
            workers
                .iter_mut()
                .filter(|(_, w)| w.state != WorkerState::Running)
                .filter_map(|(id, w)| {
                    w.loaded = None;
                    let msg = ToWorker::Load { load, plan: plan.clone(), files: files.clone(), overrides: overrides.clone() };
                    match w.tx.try_send(msg) {
                        Ok(()) => {
                            pending.insert(*id);
                            Some(*id)
                        }
                        Err(_) => {
                            tracing::warn!(worker = id, "worker outbox full, bundle not delivered");
                            None
                        }
                    }
                })
                .collect()
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            if self.pending_loads.lock().is_empty() {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::warn!(missing = self.pending_loads.lock().len(), "some workers did not load the bundle in time");
                break;
            }
        }
        self.pending_loads.lock().clear();
        let ready = self.ready_count();
        tracing::info!(load, offered = targets.len(), ready, "bundle distributed to workers");
        Ok(ready)
    }

    pub fn worker_count(&self) -> usize {
        self.workers.lock().len()
    }

    /// Waits until at least `n` workers are connected and idle.
    pub async fn wait_for_workers(&self, n: usize, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            let have = self.ready_count();
            if have >= n {
                return Ok(());
            }
            tracing::info!(connected = have, expected = n, "waiting for workers");
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(Error::Engine(format!("only {have} of {n} workers connected before timeout")));
            }
        }
    }

    pub fn infos(&self) -> Vec<WorkerInfo> {
        let now = Instant::now();
        let mut out: Vec<WorkerInfo> = self
            .workers
            .lock()
            .iter()
            .map(|(id, w)| WorkerInfo {
                id: *id,
                name: w.name.clone(),
                address: w.addr.to_string(),
                cores: w.cores,
                state: w.state,
                users: w.active,
                iterations: w.iterations,
                target: w.sent_target.map_or(0.0, |t| t.0),
                cpu_pct: w.health.cpu_pct,
                scheduler_lag_ms: w.health.scheduler_lag_ms,
                saturated: w.health.saturated,
                last_report_secs: now.duration_since(w.last_report).as_secs_f64(),
            })
            .collect();
        out.sort_by_key(|w| w.id);
        out
    }

    /// Starts a run on every idle worker. Returns the run id.
    pub(crate) fn begin_run(&self, ctx: Arc<RunCtx>, mode: LoadMode, scenarios: Option<Vec<String>>, host: Option<String>) -> Result<u64> {
        self.shared.lock().clear();
        let fp = self.fingerprint.lock().clone();
        let mut workers = self.workers.lock();
        let mut ids: Vec<u64> = workers
            .iter()
            .filter(|(_, w)| w.state != WorkerState::Running && w.loaded.as_deref() == Some(fp.as_str()))
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return Err(Error::Engine("no idle workers hold the plan of this run".into()));
        }
        ids.sort_unstable();
        let run = self.run_seq.fetch_add(1, Ordering::Relaxed);
        self.retired_iterations.store(0, Ordering::Relaxed);
        let count = ids.len();
        let limits = split_limit(ctx.iteration_limit, count);
        for (i, id) in ids.iter().enumerate() {
            if let Some(w) = workers.get_mut(id) {
                w.state = WorkerState::Running;
                w.active = 0;
                w.iterations = 0;
                w.sent_target = None;
                let msg = ToWorker::Start {
                    run,
                    index: i as u32,
                    count: count as u32,
                    iteration_limit: limits[i],
                    scenarios: scenarios.clone(),
                    host: host.clone(),
                };
                if w.tx.try_send(msg).is_err() {
                    tracing::warn!(worker = id, "worker outbox full, start not delivered");
                }
            }
        }
        drop(workers);
        tracing::info!(run, workers = count, "distributed run started");
        *self.binding.lock() = Some(Binding { run, ctx, participants: ids, mode, scenarios, host });
        Ok(run)
    }

    /// Distributes the total target over participants that are still connected.
    pub(crate) fn set_target(&self, total: f64) {
        let binding = self.binding.lock();
        let Some(b) = binding.as_ref() else {
            return;
        };
        let mut workers = self.workers.lock();
        let live: Vec<u64> =
            b.participants.iter().copied().filter(|id| workers.get(id).is_some_and(|w| w.state == WorkerState::Running)).collect();
        let shares = split_target(total, live.len(), b.mode);
        let now = Instant::now();
        for (id, share) in live.iter().zip(shares) {
            let Some(w) = workers.get_mut(id) else {
                continue;
            };
            let stale = w.sent_target.is_none_or(|(t, at)| (t - share).abs() > 1e-6 || now.duration_since(at) >= TARGET_REFRESH);
            if stale && w.tx.try_send(ToWorker::Target { run: b.run, target: share }).is_ok() {
                w.sent_target = Some((share, now));
            }
        }
    }

    /// True once every participant has stopped or disconnected.
    pub(crate) fn run_finished(&self) -> bool {
        let binding = self.binding.lock();
        let Some(b) = binding.as_ref() else {
            return true;
        };
        let workers = self.workers.lock();
        b.participants.iter().all(|id| workers.get(id).is_none_or(|w| w.state != WorkerState::Running))
    }

    /// Stops all participants and waits for their final reports.
    pub(crate) async fn end_run(&self, graceful: Duration) {
        let run = {
            let binding = self.binding.lock();
            let Some(b) = binding.as_ref() else {
                return;
            };
            let workers = self.workers.lock();
            for id in &b.participants {
                if let Some(w) = workers.get(id)
                    && w.state == WorkerState::Running
                {
                    let _ = w.tx.try_send(ToWorker::Stop { run: b.run });
                }
            }
            b.run
        };
        let deadline = tokio::time::Instant::now() + graceful + Duration::from_secs(10);
        while !self.run_finished() {
            let notified = self.changed.notified();
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::warn!(run, "some workers did not confirm stop in time");
                break;
            }
        }
        *self.binding.lock() = None;
        for w in self.workers.lock().values_mut() {
            if w.state == WorkerState::Running {
                w.state = WorkerState::Stopped;
            }
        }
        tracing::info!(run, "distributed run ended");
    }

    /// Tells every worker to exit (controller shutdown).
    pub fn quit_all(&self) {
        for w in self.workers.lock().values() {
            let _ = w.tx.try_send(ToWorker::Quit);
        }
    }

    /// Live users per scenario across running workers, plus task counts reported since the last call.
    pub(crate) fn take_mix(&self) -> (export::ScenarioUsers, export::TaskCounts) {
        let mut users: Vec<(String, u64)> = Vec::new();
        for w in self.workers.lock().values().filter(|w| w.state == WorkerState::Running) {
            for (name, n) in &w.scenario_users {
                match users.iter_mut().find(|u| &u.0 == name) {
                    Some(u) => u.1 += n,
                    None => users.push((name.clone(), *n)),
                }
            }
        }
        (users, std::mem::take(&mut *self.task_deltas.lock()))
    }

    pub(crate) fn totals(&self) -> RemoteTotals {
        let workers = self.workers.lock();
        let mut t = RemoteTotals { active: 0, iterations: self.retired_iterations.load(Ordering::Relaxed), health: None };
        for w in workers.values() {
            t.active += w.active;
            t.iterations += w.iterations;
            let h = t.health.get_or_insert_with(GeneratorHealth::default);
            h.cpu_pct = match (h.cpu_pct, w.health.cpu_pct) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            h.scheduler_lag_ms = h.scheduler_lag_ms.max(w.health.scheduler_lag_ms);
            h.saturated |= w.health.saturated;
            h.saturated_secs = h.saturated_secs.max(w.health.saturated_secs);
        }
        t
    }

    fn handle_message(&self, id: u64, msg: ToController) -> Result<()> {
        match msg {
            ToController::Hello { .. } => Err(Error::Engine("duplicate hello".into())),
            ToController::Report { run, active, iterations, health, mut window } => {
                let ctx = {
                    let binding = self.binding.lock();
                    match binding.as_ref() {
                        Some(b) if b.run == run && b.participants.contains(&id) => Some(Arc::clone(&b.ctx)),
                        _ => None,
                    }
                };
                let Some(ctx) = ctx else {
                    return Ok(());
                };
                if window.scenario_users.len() > 128 || window.task_counts.len() > 10_000 {
                    return Err(Error::Engine("report exceeds mix limits".into()));
                }
                let users: Vec<(String, u64)> =
                    std::mem::take(&mut window.scenario_users).into_iter().map(|(n, c)| (sanitize(&n, 64), c)).collect();
                let tasks: Vec<(String, String, u64)> = std::mem::take(&mut window.task_counts)
                    .into_iter()
                    .map(|(sc, t, n)| (sanitize(&sc, 64), sanitize(&t, 64), n))
                    .collect();
                let shard = export::import(window)?;
                ctx.recorder.absorb(id as usize, shard);
                {
                    let mut d = self.task_deltas.lock();
                    if d.len() < 100_000 {
                        d.extend(tasks);
                    }
                }
                if let Some(w) = self.workers.lock().get_mut(&id) {
                    w.scenario_users = users;
                    w.active = active.min(10_000_000);
                    w.iterations = iterations;
                    w.health = health;
                    w.last_report = Instant::now();
                }
                Ok(())
            }
            ToController::Shared { key, value } => self.relay_shared(id, key, value),
            ToController::Loaded { load, fingerprint, error } => {
                if !self.pool || load != *self.load_pending_id.lock() {
                    return Ok(());
                }
                let expected = self.fingerprint.lock().clone();
                if let Some(w) = self.workers.lock().get_mut(&id) {
                    match error {
                        Some(e) => tracing::warn!(worker = id, error = %sanitize(&e, 300), "worker could not load the bundle"),
                        None if ct_eq(fingerprint.as_bytes(), expected.as_bytes()) => w.loaded = Some(fingerprint),
                        None => tracing::warn!(worker = id, "worker built a different plan from the bundle (version mismatch?)"),
                    }
                }
                self.pending_loads.lock().remove(&id);
                self.changed.notify_waiters();
                Ok(())
            }
            ToController::Stopped { run } => {
                let current = self.binding.lock().as_ref().map(|b| b.run);
                if current == Some(run)
                    && let Some(w) = self.workers.lock().get_mut(&id)
                {
                    w.state = WorkerState::Stopped;
                    w.active = 0;
                }
                self.changed.notify_waiters();
                Ok(())
            }
        }
    }

    fn register(&self, name: String, addr: IpAddr, cores: u32, tx: mpsc::Sender<ToWorker>, loaded: Option<String>) -> Result<u64> {
        let mut workers = self.workers.lock();
        if workers.len() >= MAX_WORKERS {
            return Err(Error::Engine("worker limit reached".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        workers.insert(
            id,
            Slot {
                name,
                addr,
                cores,
                tx,
                state: WorkerState::Ready,
                active: 0,
                iterations: 0,
                health: GeneratorHealth::default(),
                last_report: Instant::now(),
                sent_target: None,
                scenario_users: Vec::new(),
                loaded,
            },
        );
        drop(workers);
        self.changed.notify_waiters();
        Ok(id)
    }

    /// Brings a worker that connected during a run into it and repartitions everyone.
    /// Runs with an iteration limit keep their original split and do not admit late workers.
    fn admit_late(&self, id: u64) {
        let mut binding = self.binding.lock();
        let Some(b) = binding.as_mut() else {
            return;
        };
        if b.ctx.iteration_limit.is_some() {
            return;
        }
        let fp = self.fingerprint.lock().clone();
        let mut workers = self.workers.lock();
        let Some(w) = workers.get_mut(&id) else {
            return;
        };
        if w.loaded.as_deref() != Some(fp.as_str()) {
            return;
        }
        w.state = WorkerState::Running;
        w.sent_target = None;
        b.participants.push(id);
        tracing::info!(worker = id, run = b.run, "worker joined a running test; rebalancing");
        Self::repartition(b, &mut workers, Some(id));
    }

    /// Sends new partition indices to live participants; `joiner` receives a Start instead.
    fn repartition(b: &Binding, workers: &mut HashMap<u64, Slot>, joiner: Option<u64>) {
        let live: Vec<u64> =
            b.participants.iter().copied().filter(|p| workers.get(p).is_some_and(|w| w.state == WorkerState::Running)).collect();
        let count = live.len() as u32;
        for (i, wid) in live.iter().enumerate() {
            let Some(w) = workers.get_mut(wid) else {
                continue;
            };
            let msg = if Some(*wid) == joiner {
                ToWorker::Start {
                    run: b.run,
                    index: i as u32,
                    count,
                    iteration_limit: None,
                    scenarios: b.scenarios.clone(),
                    host: b.host.clone(),
                }
            } else {
                ToWorker::Repartition { run: b.run, index: i as u32, count }
            };
            if w.tx.try_send(msg).is_err() {
                tracing::warn!(worker = wid, "worker outbox full, membership update not delivered");
            }
        }
    }

    fn relay_shared(&self, from: u64, key: String, value: serde_json::Value) -> Result<()> {
        crate::shared::validate(&key, &value)?;
        {
            let mut map = self.shared.lock();
            if !map.contains_key(&key) && map.len() >= crate::shared::MAX_KEYS {
                return Err(Error::Engine("shared state key limit reached".into()));
            }
            map.insert(key.clone(), value.clone());
        }
        for (id, w) in self.workers.lock().iter() {
            if *id != from {
                let _ = w.tx.try_send(ToWorker::Shared { key: key.clone(), value: value.clone() });
            }
        }
        Ok(())
    }

    fn send_shared_snapshot(&self, id: u64) {
        let entries: Vec<(String, serde_json::Value)> = self.shared.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(w) = self.workers.lock().get(&id) {
            for (key, value) in entries {
                let _ = w.tx.try_send(ToWorker::Shared { key, value });
            }
        }
    }

    fn unregister(&self, id: u64) {
        self.pending_loads.lock().remove(&id);
        let removed = self.workers.lock().remove(&id);
        if removed.as_ref().is_some_and(|w| w.state == WorkerState::Running) {
            let binding = self.binding.lock();
            if let Some(b) = binding.as_ref() {
                Self::repartition(b, &mut self.workers.lock(), None);
            }
        }
        if let Some(w) = removed {
            self.retired_iterations.fetch_add(w.iterations, Ordering::Relaxed);
            if w.state == WorkerState::Running {
                tracing::warn!(worker = id, name = %w.name, "worker disconnected during a run; its share is redistributed");
            } else {
                tracing::info!(worker = id, name = %w.name, "worker disconnected");
            }
        }
        self.changed.notify_waiters();
    }
}

#[derive(Debug, Clone)]
pub struct ClusterServerConfig {
    pub bind: SocketAddr,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub allow_remote_plaintext: bool,
}

impl ClusterServerConfig {
    /// Returns whether TLS is configured, enforcing the plaintext policy.
    pub fn validate(&self) -> Result<bool> {
        let tls = match (&self.tls_cert, &self.tls_key) {
            (Some(_), Some(_)) => true,
            (None, None) => false,
            _ => return Err(Error::config("cluster tls requires both certificate and key")),
        };
        if !tls && !self.bind.ip().is_loopback() && !self.allow_remote_plaintext {
            return Err(Error::config("refusing to accept workers over plaintext on a non-loopback address; configure cluster tls"));
        }
        Ok(tls)
    }
}

/// Serves the worker endpoint until `shutdown` is cancelled.
pub async fn serve(hub: Arc<Hub>, cfg: ClusterServerConfig, shutdown: CancellationToken) -> Result<()> {
    let tls = cfg.validate()?;
    let app = Router::new().route(CLUSTER_PATH, get(upgrade)).with_state(hub).into_make_service_with_connect_info::<SocketAddr>();
    let handle = axum_server::Handle::new();
    let h = handle.clone();
    tokio::spawn(async move {
        shutdown.cancelled().await;
        h.graceful_shutdown(Some(Duration::from_secs(5)));
    });
    let scheme = if tls { "wss" } else { "ws" };
    tracing::info!("cluster endpoint listening on {scheme}://{}{CLUSTER_PATH}", cfg.bind);
    if let (Some(cert), Some(key)) = (&cfg.tls_cert, &cfg.tls_key) {
        let rc =
            RustlsConfig::from_pem_file(cert, key).await.map_err(|e| Error::config(format!("cannot load cluster tls material: {e}")))?;
        axum_server::bind_rustls(cfg.bind, rc).handle(handle).serve(app).await?;
    } else {
        axum_server::bind(cfg.bind).handle(handle).serve(app).await?;
    }
    Ok(())
}

async fn upgrade(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !hub.auth_limiter.allow(addr.ip()) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let provided = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    if !ct_eq(provided.as_bytes(), hub.token.as_bytes()) {
        tracing::warn!(client = %addr.ip(), "rejected worker with invalid cluster token");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |socket| session(hub, socket, addr.ip()))
}

async fn send(sink: &mut futures_util::stream::SplitSink<WebSocket, Message>, msg: &ToWorker) -> bool {
    match serde_json::to_string(msg) {
        Ok(text) => sink.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn session(hub: Arc<Hub>, socket: WebSocket, addr: IpAddr) {
    let (mut sink, mut stream) = socket.split();
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ToController>(&t).ok(),
        _ => None,
    };
    let Some(ToController::Hello { protocol, version, name, fingerprint, cores }) = hello else {
        let _ = send(&mut sink, &ToWorker::Reject { reason: "expected hello".into() }).await;
        return;
    };
    let pool_worker = fingerprint == POOL_FINGERPRINT;
    let reject = if protocol != PROTOCOL_VERSION {
        Some(format!("protocol {protocol} not supported, controller speaks {PROTOCOL_VERSION}"))
    } else if hub.pool && !pool_worker {
        Some("this controller distributes plans itself; start the worker without a plan".into())
    } else if !hub.pool && pool_worker {
        Some("this controller runs a fixed plan; start the worker with the same plan".into())
    } else if !hub.pool && !ct_eq(fingerprint.as_bytes(), hub.fingerprint.lock().as_bytes()) {
        Some("plan fingerprint mismatch: worker plan, scripts, feeders, host or tags differ from the controller".into())
    } else {
        None
    };
    let name = sanitize(&name, 64);
    if let Some(reason) = reject {
        tracing::warn!(client = %addr, worker = %name, version = %sanitize(&version, 32), %reason, "worker rejected");
        let _ = send(&mut sink, &ToWorker::Reject { reason }).await;
        return;
    }
    let (tx, mut rx) = mpsc::channel::<ToWorker>(OUTBOX);
    let loaded = (!pool_worker).then_some(fingerprint);
    let id = match hub.register(name.clone(), addr, cores.clamp(1, 4096), tx, loaded) {
        Ok(id) => id,
        Err(e) => {
            let _ = send(&mut sink, &ToWorker::Reject { reason: e.to_string() }).await;
            return;
        }
    };
    tracing::info!(worker = id, name = %name, client = %addr, "worker connected");
    if !send(&mut sink, &ToWorker::Welcome { id }).await {
        hub.unregister(id);
        return;
    }
    hub.send_shared_snapshot(id);
    hub.admit_late(id);
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(m) => if !send(&mut sink, &m).await { break },
                None => break,
            },
            inbound = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => match inbound {
                Ok(Some(Ok(Message::Text(t)))) => {
                    let parsed = serde_json::from_str::<ToController>(&t)
                        .map_err(|e| Error::Engine(format!("invalid message: {}", sanitize(&e.to_string(), 120))))
                        .and_then(|m| hub.handle_message(id, m));
                    if let Err(e) = parsed {
                        tracing::warn!(worker = id, error = %e, "dropping worker after protocol violation");
                        break;
                    }
                }
                Ok(Some(Ok(Message::Ping(p)))) => {
                    if sink.send(Message::Pong(p)).await.is_err() { break }
                }
                Ok(Some(Ok(Message::Pong(_)))) => {}
                Ok(Some(Ok(Message::Binary(_)))) => {
                    tracing::warn!(worker = id, "unexpected binary frame");
                    break;
                }
                Ok(Some(Ok(Message::Close(_))) | None) | Ok(Some(Err(_))) => break,
                Err(_) => {
                    tracing::warn!(worker = id, "worker idle timeout");
                    break;
                }
            },
        }
    }
    let _ = sink.close().await;
    hub.unregister(id);
}
