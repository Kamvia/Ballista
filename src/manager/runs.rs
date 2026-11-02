// File: ballista/src/manager/runs.rs
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::bundles::{BundleStore, PLAN_FILE, valid_labels};
use super::compare::{Comparison, RegressionGate, compare};
use super::policy::Policy;
use super::webhook::Webhook;
use crate::cluster::hub::Hub;
use crate::cluster::{LoadFile, LoadOverrides};
use crate::config::{Overrides, Plan, ReportConfig};
use crate::engine::{Engine, Role};
use crate::error::{Error, Result, sanitize};
use crate::report::Report;
use crate::scenario::Registry;

const RUN_FILE: &str = "run.json";
const STOP_GRACE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Stopping,
    Passed,
    Failed,
    Stopped,
    Cancelled,
    Error,
}

impl RunState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Stopped | Self::Cancelled | Self::Error)
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
    }
}

/// Run-time adjustments accepted with a run request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub users: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_rate: Option<f64>,
    /// Duration such as `90s` or `5m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Pool workers to use: 0 runs on this server only; unset uses every available worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<u32>,
}

impl RunOverrides {
    pub(crate) fn to_overrides(&self) -> Result<Overrides> {
        let run_time = match &self.run_time {
            Some(t) => Some(humantime::parse_duration(t.trim()).map_err(|e| Error::config(format!("run_time: {e}")))?),
            None => None,
        };
        if self.workers.is_some_and(|w| w > 512) {
            return Err(Error::config("workers must be at most 512"));
        }
        for list in [&self.tags, &self.exclude_tags].into_iter().flatten() {
            if list.len() > 64 || !list.iter().all(|t| crate::config::is_valid_name(t)) {
                return Err(Error::config("tags must be up to 64 simple names"));
            }
        }
        Ok(Overrides {
            host: self.host.clone(),
            users: self.users,
            spawn_rate: self.spawn_rate,
            run_time,
            iterations: self.iterations,
            tags: self.tags.clone(),
            exclude_tags: self.exclude_tags.clone(),
            ..Overrides::default()
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub bundle: String,
    #[serde(default)]
    pub overrides: RunOverrides,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Fail the run when it is slower or less reliable than a baseline run.
    #[serde(default)]
    pub regression: Option<RegressionGate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdOutcome {
    pub metric: String,
    pub request: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub actual: Option<f64>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub requests: u64,
    pub failures: u64,
    pub fail_ratio: f64,
    pub rps: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub iterations: u64,
    pub duration_secs: f64,
    pub aborted: bool,
    pub generator_saturated_secs: u64,
    pub thresholds: Vec<ThresholdOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub seq: u64,
    pub bundle: String,
    pub plan_name: String,
    pub state: RunState,
    pub created_at: String,
    pub created_by: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub overrides: RunOverrides,
    pub passed: Option<bool>,
    pub error: Option<String>,
    pub summary: Option<Summary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<String>,
    /// Pool workers that generated load (absent for runs on the server alone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression: Option<RegressionGate>,
    /// Outcome of the regression gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

pub fn now_rfc3339() -> String {
    humantime::format_rfc3339_millis(SystemTime::now()).to_string()
}

fn new_run_id() -> Result<String> {
    let mut b = [0_u8; 12];
    getrandom::fill(&mut b).map_err(|e| Error::Engine(format!("system rng unavailable: {e}")))?;
    Ok(format!("r{}", hex::encode(b)))
}

pub fn valid_run_id(id: &str) -> bool {
    id.len() == 25 && id.starts_with('r') && id[1..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Default)]
struct Inner {
    records: BTreeMap<u64, RunRecord>,
    by_id: HashMap<String, u64>,
    idempotency: HashMap<(String, String), String>,
    queue: VecDeque<String>,
    next_seq: u64,
}

struct Active {
    id: String,
    engine: Engine,
    stop_requested: bool,
}

/// A run taken off the queue whose engine is still being prepared (bundle checks, worker loading).
struct Starting {
    id: String,
    cancel: CancellationToken,
    stopped_by: Option<String>,
}

pub struct RunsConfig {
    pub max_queue: usize,
    pub keep_runs: usize,
}

/// Run queue, history and the single active engine of this node.
pub struct Runs {
    dir: PathBuf,
    cfg: RunsConfig,
    inner: Mutex<Inner>,
    active: Mutex<Option<Active>>,
    starting: Mutex<Option<Starting>>,
    wake: Notify,
    version: watch::Sender<u64>,
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl Runs {
    /// Loads history. Runs interrupted by a previous shutdown are marked as errors.
    pub fn open(dir: PathBuf, cfg: RunsConfig) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let mut inner = Inner::default();
        for e in std::fs::read_dir(&dir)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !valid_run_id(&name) {
                continue;
            }
            let Ok(raw) = std::fs::read(e.path().join(RUN_FILE)) else { continue };
            let Ok(mut rec) = serde_json::from_slice::<RunRecord>(&raw) else {
                tracing::warn!(run = %name, "skipping unreadable run record");
                continue;
            };
            if rec.state == RunState::Queued {
                inner.queue.push_back(rec.id.clone());
            } else if !rec.state.terminal() {
                rec.state = RunState::Error;
                rec.error = Some("interrupted by server restart".to_owned());
                rec.ended_at.get_or_insert_with(now_rfc3339);
                let _ = write_atomic(&e.path().join(RUN_FILE), &serde_json::to_vec_pretty(&rec).unwrap_or_default());
            }
            if let Some(k) = &rec.idempotency_key {
                inner.idempotency.insert((rec.created_by.clone(), k.clone()), rec.id.clone());
            }
            inner.next_seq = inner.next_seq.max(rec.seq + 1);
            inner.by_id.insert(rec.id.clone(), rec.seq);
            inner.records.insert(rec.seq, rec);
        }
        let mut queued: Vec<(u64, String)> = inner.queue.drain(..).filter_map(|id| inner.by_id.get(&id).map(|s| (*s, id))).collect();
        queued.sort();
        inner.queue = queued.into_iter().map(|(_, id)| id).collect();
        let (version, _) = watch::channel(0);
        Ok(Self { dir, cfg, inner: Mutex::new(inner), active: Mutex::new(None), starting: Mutex::new(None), wake: Notify::new(), version })
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub fn run_dir(&self, id: &str) -> Option<PathBuf> {
        valid_run_id(id).then(|| self.dir.join(id))
    }

    fn persist(&self, rec: &RunRecord) {
        let d = self.dir.join(&rec.id);
        let res = std::fs::create_dir_all(&d)
            .map_err(Error::from)
            .and_then(|()| write_atomic(&d.join(RUN_FILE), &serde_json::to_vec_pretty(rec).unwrap_or_default()));
        if let Err(e) = res {
            tracing::error!(run = %rec.id, error = %e, "cannot persist run record");
        }
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut RunRecord)) -> Option<RunRecord> {
        let rec = {
            let mut inner = self.inner.lock();
            let seq = *inner.by_id.get(id)?;
            let rec = inner.records.get_mut(&seq)?;
            f(rec);
            rec.clone()
        };
        self.persist(&rec);
        self.bump();
        Some(rec)
    }

    pub fn get(&self, id: &str) -> Option<RunRecord> {
        let inner = self.inner.lock();
        inner.by_id.get(id).and_then(|s| inner.records.get(s)).cloned()
    }

    /// Newest first; `before` is a sequence cursor from a previous page.
    pub fn list(&self, state: Option<RunState>, limit: usize, before: Option<u64>) -> (Vec<RunRecord>, Option<u64>) {
        let inner = self.inner.lock();
        let upper = before.unwrap_or(u64::MAX);
        let items: Vec<RunRecord> = inner
            .records
            .range(..upper)
            .rev()
            .map(|(_, r)| r)
            .filter(|r| state.is_none_or(|s| r.state == s))
            .take(limit + 1)
            .cloned()
            .collect();
        if items.len() > limit {
            let page: Vec<RunRecord> = items.into_iter().take(limit).collect();
            let next = page.last().map(|r| r.seq);
            (page, next)
        } else {
            (items, None)
        }
    }

    pub fn queue_position(&self, id: &str) -> Option<usize> {
        self.inner.lock().queue.iter().position(|q| q == id)
    }

    /// Queues a run. A repeated idempotency key from the same principal returns the original run.
    pub fn submit(
        &self,
        req: RunRequest,
        plan_name: String,
        principal: &str,
        idempotency_key: Option<String>,
    ) -> Result<(RunRecord, bool)> {
        valid_labels(&req.labels)?;
        req.overrides.to_overrides()?;
        if let Some(g) = &req.regression {
            g.validate()?;
        }
        let mut inner = self.inner.lock();
        if let Some(k) = &idempotency_key
            && let Some(existing) = inner.idempotency.get(&(principal.to_owned(), k.clone()))
            && let Some(rec) = inner.by_id.get(existing).and_then(|s| inner.records.get(s))
        {
            return Ok((rec.clone(), false));
        }
        if inner.queue.len() >= self.cfg.max_queue {
            return Err(Error::Engine("run queue is full".into()));
        }
        let id = new_run_id()?;
        let seq = inner.next_seq;
        inner.next_seq += 1;
        let rec = RunRecord {
            id: id.clone(),
            seq,
            bundle: req.bundle,
            plan_name,
            state: RunState::Queued,
            created_at: now_rfc3339(),
            created_by: principal.to_owned(),
            started_at: None,
            ended_at: None,
            labels: req.labels,
            overrides: req.overrides,
            passed: None,
            error: None,
            summary: None,
            stopped_by: None,
            workers: None,
            regression: req.regression,
            comparison: None,
            idempotency_key: idempotency_key.clone(),
        };
        if let Some(k) = idempotency_key {
            inner.idempotency.insert((principal.to_owned(), k), id.clone());
        }
        inner.by_id.insert(id.clone(), seq);
        inner.records.insert(seq, rec.clone());
        inner.queue.push_back(id);
        drop(inner);
        self.persist(&rec);
        self.bump();
        self.wake.notify_one();
        Ok((rec, true))
    }

    /// Cancels a queued run or stops the running one.
    pub fn stop(&self, id: &str, by: &str) -> Result<RunRecord> {
        let queued = {
            let mut inner = self.inner.lock();
            let pos = inner.queue.iter().position(|q| q == id);
            pos.map(|p| inner.queue.remove(p))
        };
        if queued.is_some() {
            return self
                .update(id, |r| {
                    r.state = RunState::Cancelled;
                    r.ended_at = Some(now_rfc3339());
                    r.stopped_by = Some(by.to_owned());
                })
                .ok_or_else(|| Error::Engine("run not found".into()));
        }
        {
            let mut starting = self.starting.lock();
            if let Some(st) = starting.as_mut().filter(|st| st.id == id) {
                st.stopped_by.get_or_insert_with(|| by.to_owned());
                st.cancel.cancel();
                drop(starting);
                return self
                    .update(id, |r| {
                        r.state = RunState::Stopping;
                        r.stopped_by = Some(by.to_owned());
                    })
                    .ok_or_else(|| Error::Engine("run not found".into()));
            }
            let mut active = self.active.lock();
            match active.as_mut() {
                Some(a) if a.id == id => {
                    a.stop_requested = true;
                    a.engine.stop()?;
                }
                _ => {
                    return match self.get(id) {
                        Some(r) => Err(Error::Engine(format!("run is {:?} and cannot be stopped", r.state).to_lowercase())),
                        None => Err(Error::Engine("run not found".into())),
                    };
                }
            }
        }
        self.update(id, |r| {
            r.state = RunState::Stopping;
            r.stopped_by = Some(by.to_owned());
        })
        .ok_or_else(|| Error::Engine("run not found".into()))
    }

    /// Deletes a finished run and its artifacts.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.lock();
        let Some(seq) = inner.by_id.get(id).copied() else { return Ok(false) };
        if !inner.records.get(&seq).is_some_and(|r| r.state.terminal()) {
            return Err(Error::Engine("only finished runs can be deleted".into()));
        }
        if let Some(rec) = inner.records.remove(&seq)
            && let Some(k) = rec.idempotency_key
        {
            inner.idempotency.remove(&(rec.created_by, k));
        }
        inner.by_id.remove(id);
        drop(inner);
        if let Some(d) = self.run_dir(id) {
            let _ = std::fs::remove_dir_all(d);
        }
        self.bump();
        Ok(true)
    }

    pub fn bundle_in_use(&self, bundle: &str) -> bool {
        self.inner.lock().records.values().any(|r| r.bundle == bundle && !r.state.terminal())
    }

    /// Live engine of a running run.
    pub fn engine(&self, id: &str) -> Option<Engine> {
        self.active.lock().as_ref().filter(|a| a.id == id).map(|a| a.engine.clone())
    }

    fn prune(&self) {
        let removed: Vec<String> = {
            let mut inner = self.inner.lock();
            let finished: Vec<u64> = inner.records.iter().filter(|(_, r)| r.state.terminal()).map(|(s, _)| *s).collect();
            let excess = finished.len().saturating_sub(self.cfg.keep_runs);
            let mut out = Vec::new();
            for seq in finished.into_iter().take(excess) {
                if let Some(r) = inner.records.remove(&seq) {
                    inner.by_id.remove(&r.id);
                    if let Some(k) = r.idempotency_key {
                        inner.idempotency.remove(&(r.created_by, k));
                    }
                    out.push(r.id);
                }
            }
            out
        };
        for id in removed {
            if let Some(d) = self.run_dir(&id) {
                let _ = std::fs::remove_dir_all(d);
            }
        }
    }

    /// Executes queued runs one at a time until `shutdown`.
    pub async fn runner(
        self: Arc<Self>,
        bundles: Arc<BundleStore>,
        policy: Arc<Policy>,
        registry: Registry,
        webhook: Option<Webhook>,
        hub: Option<Arc<Hub>>,
        shutdown: CancellationToken,
    ) {
        let deps = RunnerDeps { bundles, policy, registry, webhook, hub };
        loop {
            let next = self.inner.lock().queue.pop_front();
            let Some(id) = next else {
                tokio::select! {
                    () = self.wake.notified() => continue,
                    () = shutdown.cancelled() => return,
                }
            };
            self.execute(&id, &deps, &shutdown).await;
            self.prune();
            if shutdown.is_cancelled() {
                return;
            }
        }
    }

    fn prepare(&self, rec: &RunRecord, bundles: &BundleStore, policy: &Policy) -> Result<(Plan, PathBuf)> {
        let dir = bundles.dir(&rec.bundle).ok_or_else(|| Error::config("bundle no longer exists"))?;
        let mut plan = Plan::load(&dir.join(PLAN_FILE))?;
        plan.apply_overrides(&rec.overrides.to_overrides()?)?;
        policy.check(&plan, &dir, true)?;
        plan.report = ReportConfig {
            percentiles: plan.report.percentiles.clone(),
            otlp: plan.report.otlp.clone(),
            influx: plan.report.influx.clone(),
            ..ReportConfig::default()
        };
        Ok((plan, dir))
    }

    /// Builds the engine: on this server alone, or as controller of pool workers that loaded the bundle.
    async fn build_engine(
        &self,
        rec: &RunRecord,
        plan: Plan,
        dir: &Path,
        registry: &Registry,
        hub: Option<&Arc<Hub>>,
    ) -> Result<(Engine, Option<u32>)> {
        let want = rec.overrides.workers;
        let hub = match (hub, want) {
            (_, Some(0)) | (None, None) => return Ok((Engine::new(plan, registry)?, None)),
            (None, Some(_)) => return Err(Error::config("this server has no worker pool (start it with --cluster-bind)")),
            (Some(h), _) => h,
        };
        if let Some(k) = want {
            hub.wait_for_idle(k as usize, WORKER_WAIT).await?;
        } else if hub.idle_count() == 0 {
            return Ok((Engine::new(plan, registry)?, None));
        }
        let fp = crate::cluster::fingerprint(&plan)?;
        let (plan_text, files) = read_bundle(dir).await?;
        let o = &rec.overrides;
        let overrides = LoadOverrides {
            users: o.users,
            spawn_rate: o.spawn_rate,
            run_time: o.run_time.clone(),
            iterations: o.iterations,
            tags: o.tags.clone(),
            exclude_tags: o.exclude_tags.clone(),
            host: o.host.clone(),
        };
        let ready = hub.load(plan_text, files, overrides, fp, LOAD_TIMEOUT).await?;
        match want {
            Some(k) if ready < k as usize => Err(Error::Engine(format!("only {ready} of {k} workers loaded the plan"))),
            None if ready == 0 => {
                tracing::warn!(run = %rec.id, "no worker loaded the plan; running on this server");
                Ok((Engine::new(plan, registry)?, None))
            }
            _ => Ok((Engine::new_with_role(plan, registry, Role::Controller(Arc::clone(hub)))?, Some(ready as u32))),
        }
    }

    /// Starts the built engine and makes it the active run, or reports who stopped the run while it was starting.
    /// Starting and active are switched under the starting lock, so a concurrent stop sees exactly one of them.
    #[allow(clippy::type_complexity)]
    fn switch_to_active(
        &self,
        id: &str,
        built: Result<(Engine, Option<u32>)>,
    ) -> std::result::Result<Result<(Engine, Option<u32>)>, String> {
        let mut starting = self.starting.lock();
        if let Some(by) = starting.take().and_then(|st| st.stopped_by) {
            return Err(by);
        }
        let started = built.and_then(|(e, w)| e.start().map(|()| (e, w)));
        if let Ok((engine, _)) = &started {
            *self.active.lock() = Some(Active { id: id.to_owned(), engine: engine.clone(), stop_requested: false });
        }
        Ok(started)
    }

    async fn execute(&self, id: &str, deps: &RunnerDeps, shutdown: &CancellationToken) {
        let RunnerDeps { bundles, policy, registry, webhook, hub } = deps;
        let (webhook, hub) = (webhook.as_ref(), hub.as_ref());
        let Some(rec) = self.get(id) else { return };
        let cancel = CancellationToken::new();
        *self.starting.lock() = Some(Starting { id: id.to_owned(), cancel: cancel.clone(), stopped_by: None });
        let built = tokio::select! {
            b = async {
                match self.prepare(&rec, bundles, policy) {
                    Ok((plan, dir)) => self.build_engine(&rec, plan, &dir, registry, hub).await,
                    Err(e) => Err(e),
                }
            } => b,
            () = cancel.cancelled() => Err(Error::Engine("cancelled before start".into())),
        };
        let started = match self.switch_to_active(id, built) {
            Ok(started) => started,
            Err(by) => {
                tracing::info!(run = %id, "run cancelled before start");
                if let Some(r) = self.update(id, |r| {
                    r.state = RunState::Cancelled;
                    r.ended_at = Some(now_rfc3339());
                    r.stopped_by = Some(by);
                }) {
                    notify(webhook, "run.finished", &r);
                }
                return;
            }
        };
        let (engine, workers) = match started {
            Ok(e) => e,
            Err(e) => {
                let msg = sanitize(&e.to_string(), 500);
                tracing::warn!(run = %id, error = %msg, "run could not start");
                if let Some(r) = self.update(id, |r| {
                    r.state = RunState::Error;
                    r.error = Some(msg);
                    r.ended_at = Some(now_rfc3339());
                }) {
                    notify(webhook, "run.finished", &r);
                }
                return;
            }
        };
        if let Some(r) = self.update(id, |r| {
            r.state = RunState::Running;
            r.started_at = Some(now_rfc3339());
            r.workers = workers;
        }) {
            tracing::info!(run = %id, plan = %r.plan_name, workers = workers.unwrap_or(0), "run started");
            notify(webhook, "run.started", &r);
        }

        let limit = policy.max_run_time;
        let waiter = engine.wait();
        tokio::pin!(waiter);
        let mut deadline_hit = false;
        let mut shutdown_hit = false;
        let deadline = tokio::time::sleep(limit);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut waiter => break,
                () = &mut deadline => {
                    if deadline_hit {
                        break;
                    }
                    deadline_hit = true;
                    tracing::warn!(run = %id, "run reached the server time limit, stopping");
                    let _ = engine.stop();
                    deadline.as_mut().reset(tokio::time::Instant::now() + STOP_GRACE);
                }
                () = shutdown.cancelled(), if !shutdown_hit => {
                    shutdown_hit = true;
                    let _ = engine.stop();
                }
            }
        }
        let stop_requested = self.active.lock().take().is_some_and(|a| a.stop_requested);

        let report = Report::from_engine(&engine);
        let run_dir = self.dir.join(id);
        let cfg = ReportConfig {
            json: Some(run_dir.join("report.json")),
            html: Some(run_dir.join("report.html")),
            csv: Some(run_dir.join("report")),
            ..ReportConfig::default()
        };
        let write_err = report.write(&cfg).err().map(|e| sanitize(&e.to_string(), 300));
        let s = &report.snapshot;
        let summary = Summary {
            requests: s.total.count,
            failures: s.total.failures,
            fail_ratio: s.total.fail_ratio,
            rps: s.total.rps_avg,
            avg_ms: s.total.avg_ms,
            p50_ms: s.total.p50_ms,
            p95_ms: s.total.p95_ms,
            p99_ms: s.total.p99_ms,
            max_ms: s.total.max_ms,
            iterations: s.iterations_started,
            duration_secs: report.duration_secs,
            aborted: report.aborted,
            generator_saturated_secs: report.generator_saturated_secs,
            thresholds: s
                .thresholds
                .iter()
                .map(|t| ThresholdOutcome {
                    metric: serde_json::to_value(t.metric).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
                    request: t.request.clone(),
                    min: t.min,
                    max: t.max,
                    actual: t.actual,
                    passed: t.passed,
                })
                .collect(),
        };
        let mut passed = report.passed && !deadline_hit;
        let interrupted = stop_requested || shutdown_hit;
        let comparison = match (&rec.regression, passed && !interrupted) {
            (Some(gate), true) => {
                let current = report_value(&report);
                Some(self.compare_with(&rec, &current, gate))
            }
            _ => None,
        };
        let regression_error = comparison.as_ref().filter(|c| !c.passed).map(Comparison::headline);
        if regression_error.is_some() {
            passed = false;
        }
        let state = if interrupted {
            RunState::Stopped
        } else if passed {
            RunState::Passed
        } else {
            RunState::Failed
        };
        if let Some(r) = self.update(id, |r| {
            r.state = state;
            r.passed = Some(passed);
            r.ended_at = Some(now_rfc3339());
            r.summary = Some(summary);
            r.comparison = comparison;
            r.error = match (deadline_hit, write_err, regression_error) {
                (true, _, _) => Some("stopped at the server time limit".to_owned()),
                (false, Some(e), _) => Some(format!("report write failed: {e}")),
                (false, None, Some(e)) => Some(sanitize(&e, 500)),
                (false, None, None) => None,
            };
            if shutdown_hit {
                r.stopped_by.get_or_insert_with(|| "server shutdown".to_owned());
            }
        }) {
            tracing::info!(run = %id, state = ?r.state, "run finished");
            notify(webhook, "run.finished", &r);
        }
    }
}

fn report_value(report: &Report) -> serde_json::Value {
    serde_json::to_value(&report.snapshot).unwrap_or(serde_json::Value::Null)
}

impl Runs {
    /// The run a comparison uses: the given id, or the latest passed run of the same plan before `rec`.
    pub fn resolve_baseline(&self, rec: &RunRecord, wanted: &str) -> Option<String> {
        if wanted != "auto" {
            return self.get(wanted).filter(|b| b.id != rec.id && b.summary.is_some()).map(|b| b.id);
        }
        let inner = self.inner.lock();
        inner
            .records
            .range(..rec.seq)
            .rev()
            .map(|(_, r)| r)
            .find(|r| r.plan_name == rec.plan_name && r.state == RunState::Passed && r.summary.is_some())
            .map(|r| r.id.clone())
    }

    /// Snapshot stored in a finished run's report.
    pub fn stored_snapshot(&self, id: &str) -> Option<serde_json::Value> {
        let raw = std::fs::read(self.run_dir(id)?.join("report.json")).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
        v.get("snapshot").cloned()
    }

    /// Compares a run's snapshot with its baseline under `gate`.
    pub fn compare_with(&self, rec: &RunRecord, current: &serde_json::Value, gate: &RegressionGate) -> Comparison {
        match self.resolve_baseline(rec, &gate.baseline) {
            Some(base_id) => match self.stored_snapshot(&base_id) {
                Some(base) => compare(current, &base, &base_id, gate),
                None => Comparison::without_baseline("baseline report is missing"),
            },
            None if gate.baseline == "auto" => {
                Comparison::without_baseline("no earlier passed run of this plan; this run becomes the baseline")
            }
            None => Comparison::without_baseline("baseline run not found or not finished"),
        }
    }
}

const WORKER_WAIT: Duration = Duration::from_secs(60);
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Plan text and every other file of a stored bundle, for shipping to pool workers.
async fn read_bundle(dir: &Path) -> Result<(String, Vec<LoadFile>)> {
    use base64::Engine as _;
    let plan = tokio::fs::read_to_string(dir.join(PLAN_FILE)).await?;
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&d).await?;
        while let Some(e) = rd.next_entry().await? {
            let ft = e.file_type().await?;
            let path = e.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let rel = path.strip_prefix(dir).map_err(|_| Error::Engine("bundle path".into()))?.to_string_lossy().replace('\\', "/");
                if rel == PLAN_FILE || rel == "bundle.json" {
                    continue;
                }
                let data = tokio::fs::read(&path).await?;
                files.push(LoadFile { path: rel, data: base64::engine::general_purpose::STANDARD.encode(data) });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((plan, files))
}

fn notify(webhook: Option<&Webhook>, event: &str, rec: &RunRecord) {
    if let Some(w) = webhook {
        let payload = serde_json::json!({ "event": event, "sent_at": now_rfc3339(), "run": rec });
        w.send(event, &payload);
    }
}

/// Shared services a queued run needs while executing.
struct RunnerDeps {
    bundles: Arc<BundleStore>,
    policy: Arc<Policy>,
    registry: Registry,
    webhook: Option<Webhook>,
    hub: Option<Arc<Hub>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_and_states() {
        let id = new_run_id().expect("id");
        assert!(valid_run_id(&id));
        assert!(!valid_run_id("r../../../etc/passwd000000"));
        assert_eq!(RunState::parse("passed"), Some(RunState::Passed));
        assert!(RunState::Cancelled.terminal() && !RunState::Queued.terminal());
    }

    #[test]
    fn queue_cancel_and_idempotency() {
        let dir = std::env::temp_dir().join(format!("runs-test-{}", new_run_id().expect("id")));
        let runs = Runs::open(dir.clone(), RunsConfig { max_queue: 2, keep_runs: 10 }).expect("open");
        let req = || RunRequest { bundle: "0".repeat(32), overrides: RunOverrides::default(), labels: BTreeMap::new(), regression: None };
        let (a, created) = runs.submit(req(), "p".into(), "ci", Some("k1".into())).expect("submit");
        assert!(created);
        let (again, created) = runs.submit(req(), "p".into(), "ci", Some("k1".into())).expect("repeat");
        assert!(!created && again.id == a.id);
        runs.submit(req(), "p".into(), "ci", None).expect("second");
        assert!(runs.submit(req(), "p".into(), "ci", None).is_err(), "queue limit");
        assert_eq!(runs.stop(&a.id, "ci").expect("cancel").state, RunState::Cancelled);
        let reopened = Runs::open(dir.clone(), RunsConfig { max_queue: 2, keep_runs: 10 }).expect("reopen");
        assert_eq!(reopened.list(None, 10, None).0.len(), 2);
        assert_eq!(reopened.get(&a.id).map(|r| r.state), Some(RunState::Cancelled));
        assert_eq!(reopened.inner.lock().queue.len(), 1, "queued run survives restart");
        let (same, created) = reopened.submit(req(), "p".into(), "ci", Some("k1".into())).expect("repeat after restart");
        assert!(!created && same.id == a.id);
        let _ = std::fs::remove_dir_all(dir);
    }
}
