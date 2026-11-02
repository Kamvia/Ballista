// File: ballista/src/engine/mod.rs
mod closed;
mod debug;
mod open;

pub use debug::debug_run;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::cluster::hub::{Hub, WorkerInfo};
use crate::config::{ClientMode, LoadMode, Plan, ramp_stages};
use crate::csv::CsvRequestLog;
use crate::error::{Error, Result};
use crate::listener::Listener;
use crate::metrics::export::{WindowExport, export};
use crate::metrics::{Key, Kind, Ratio, RunState, ScenarioShare, Snapshot, Stats, TaskShare};
use crate::monitor::{GeneratorHealth, Monitor};
use crate::scenario::feeder::Feeders;
use crate::scenario::{Registry, ScenarioEntry, ScenarioSet};
use crate::shape::Shape;
use crate::threshold::{self, ThresholdResult};
use crate::vu::{RunCtx, Vu, build_client};

const CONTROL_TICK: Duration = Duration::from_millis(100);
const STATS_TICK: Duration = Duration::from_secs(1);
const LIVE_TIMELINE: usize = 900;

enum Command {
    Stop,
    SetTarget { target: f64, ramp: f64 },
    ResumeShape,
    Repartition { index: u32, count: u32 },
}

/// Per-run overrides of the load profile, e.g. from the dashboard start form.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub users: Option<f64>,
    pub spawn_rate: Option<f64>,
    pub run_time: Option<Duration>,
    /// Run only these scenarios (all when `None`).
    pub scenarios: Option<Vec<String>>,
    /// Target host for this run; must be the plan target or in `target.allowed_hosts`.
    pub host: Option<String>,
}

/// One stats window produced by a worker for its controller.
#[derive(Debug)]
pub struct WorkerUpdate {
    pub active: u64,
    pub iterations: u64,
    pub health: GeneratorHealth,
    pub window: WindowExport,
}

/// How an engine takes part in a run.
pub enum Role {
    /// Generates load locally.
    Standalone,
    /// Drives remote workers and aggregates their statistics.
    Controller(Arc<Hub>),
    /// Generates load under remote control and streams stats windows to the controller.
    Worker(mpsc::Sender<WorkerUpdate>),
}

struct RunSpec {
    shared_client: Option<reqwest::Client>,
    shape: Arc<Shape>,
    listeners: Arc<[Arc<dyn Listener>]>,
    remote: bool,
    iteration_limit: Option<u64>,
    partition: Option<(u32, u32)>,
    scenarios: Arc<ScenarioSet>,
    selection: Option<Vec<String>>,
    plan: Arc<Plan>,
    host: Option<String>,
}

struct Meta {
    state: RunState,
    users: u64,
    target: f64,
    manual: bool,
    aborted: bool,
    thresholds: Vec<ThresholdResult>,
    generator: GeneratorHealth,
    iterations_started: u64,
    workers: Vec<WorkerInfo>,
    weights: Vec<(String, u64)>,
    scenario_users: Vec<(String, u64)>,
    started_at: Option<SystemTime>,
    ended_at: Option<SystemTime>,
}

struct RunHandle {
    cmd: mpsc::Sender<Command>,
    task: JoinHandle<()>,
}

struct Inner {
    plan: Arc<Plan>,
    scenarios: Arc<ScenarioSet>,
    feeders: Arc<Feeders>,
    base_shape: Arc<Shape>,
    shape: Mutex<Arc<Shape>>,
    listeners: Vec<Arc<dyn Listener>>,
    stats: Mutex<Stats>,
    meta: Mutex<Meta>,
    run: Mutex<Option<RunHandle>>,
    tx: watch::Sender<Arc<Snapshot>>,
    role: Role,
    shared: Arc<crate::shared::SharedStore>,
}

/// Load engine. Cheap to clone; all clones control the same run.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

enum Driver {
    Closed(closed::ClosedDriver),
    Open(open::OpenDriver),
    Cluster(Arc<Hub>),
}

impl Driver {
    fn adjust(&mut self, target: f64) {
        match self {
            Self::Closed(d) => d.adjust(target),
            Self::Open(d) => d.adjust(target),
            Self::Cluster(h) => h.set_target(target),
        }
    }

    async fn shutdown(&mut self, graceful: Duration) {
        match self {
            Self::Closed(d) => d.shutdown(graceful).await,
            Self::Open(d) => d.shutdown(graceful).await,
            Self::Cluster(h) => h.end_run(graceful).await,
        }
    }
}

pub(crate) fn build_shape(plan: &Plan) -> Result<Shape> {
    match &plan.load.shape_script {
        Some(p) => Shape::from_script(p, plan.target_cap()),
        None => Ok(Shape::new(&plan.load.stages)),
    }
}

impl Engine {
    pub fn new(plan: Plan, registry: &Registry) -> Result<Self> {
        Self::new_with_role(plan, registry, Role::Standalone)
    }

    pub fn new_with_role(plan: Plan, registry: &Registry, role: Role) -> Result<Self> {
        let plan = Arc::new(plan);
        let feeders = Arc::new(Feeders::load(&plan.feeders)?);
        let shared = Arc::new(crate::shared::SharedStore::default());
        let scenarios = Arc::new(ScenarioSet::build(&plan, registry, &feeders, &shared)?);
        build_client(&plan)?;
        let shape = Arc::new(build_shape(&plan)?);
        let meta = Meta {
            state: RunState::Idle,
            users: 0,
            target: 0.0,
            manual: false,
            aborted: false,
            thresholds: Vec::new(),
            generator: GeneratorHealth::default(),
            iterations_started: 0,
            workers: Vec::new(),
            weights: Vec::new(),
            scenario_users: Vec::new(),
            started_at: None,
            ended_at: None,
        };
        let stats = Stats::with_percentiles(Arc::from(plan.report.percentiles.clone()));
        let initial = Arc::new(build_snapshot(&plan, &stats, &meta, LIVE_TIMELINE));
        let (tx, _rx) = watch::channel(initial);
        let engine = Self {
            inner: Arc::new(Inner {
                plan,
                scenarios,
                feeders,
                base_shape: Arc::clone(&shape),
                shape: Mutex::new(shape),
                listeners: registry.listeners().to_vec(),
                stats: Mutex::new(stats),
                meta: Mutex::new(meta),
                run: Mutex::new(None),
                tx,
                role,
                shared,
            }),
        };
        if let Role::Controller(hub) = &engine.inner.role
            && tokio::runtime::Handle::try_current().is_ok()
        {
            tokio::spawn(publish_idle_workers(Arc::downgrade(&engine.inner), Arc::clone(hub)));
        }
        Ok(engine)
    }

    /// Shared key/value store visible to every virtual user.
    pub fn shared(&self) -> &Arc<crate::shared::SharedStore> {
        &self.inner.shared
    }

    pub fn hub(&self) -> Option<&Arc<Hub>> {
        match &self.inner.role {
            Role::Controller(h) => Some(h),
            _ => None,
        }
    }

    pub fn plan(&self) -> &Arc<Plan> {
        &self.inner.plan
    }

    /// Profile of the current (or next) run.
    pub fn shape(&self) -> Arc<Shape> {
        Arc::clone(&self.inner.shape.lock())
    }

    pub fn scenario_names(&self) -> Vec<String> {
        self.inner.scenarios.entries.iter().map(|e| e.name.to_string()).collect()
    }

    pub fn state(&self) -> RunState {
        self.inner.meta.lock().state
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.inner.tx.subscribe()
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.inner.tx.borrow().clone()
    }

    /// Snapshot including the complete timeline, used for reports.
    pub fn full_snapshot(&self) -> Snapshot {
        let stats = self.inner.stats.lock();
        let meta = self.inner.meta.lock();
        build_snapshot(&self.inner.plan, &stats, &meta, usize::MAX)
    }

    pub fn times(&self) -> (Option<SystemTime>, Option<SystemTime>) {
        let m = self.inner.meta.lock();
        (m.started_at, m.ended_at)
    }

    pub fn start(&self) -> Result<()> {
        self.start_with(StartOptions::default())
    }

    /// Starts a run, optionally replacing the profile with a ramp to `users` at `spawn_rate` for `run_time`.
    pub fn start_with(&self, opts: StartOptions) -> Result<()> {
        let mut run = self.inner.run.lock();
        if run.as_ref().is_some_and(|h| !h.task.is_finished()) {
            return Err(Error::Engine("a run is already in progress".into()));
        }
        let plan = &self.inner.plan;
        match &self.inner.role {
            Role::Worker(_) => return Err(Error::Engine("workers are started by their controller".into())),
            Role::Controller(h) if h.ready_count() == 0 => {
                return Err(Error::Engine("no idle workers connected".into()));
            }
            _ => {}
        }
        let shape = match (opts.users, opts.run_time) {
            (None, None) => Arc::clone(&self.inner.base_shape),
            (Some(users), rt) => {
                let rate = opts.spawn_rate.unwrap_or(users.max(1.0));
                if !rate.is_finite() || rate <= 0.0 {
                    return Err(Error::Engine("spawn rate must be positive".into()));
                }
                let stages = ramp_stages(users, rate, rt);
                plan.validate_stages(&stages)?;
                Arc::new(Shape::new(&stages))
            }
            (None, Some(rt)) => {
                if plan.load.shape_script.is_some() {
                    return Err(Error::Engine("run time override needs users when a shape script is used".into()));
                }
                let stages = crate::config::truncate_stages(&plan.load.stages, rt);
                plan.validate_stages(&stages)?;
                Arc::new(Shape::new(&stages))
            }
        };
        *self.inner.shape.lock() = Arc::clone(&shape);
        let spec = self.run_spec(shape, false, plan.load.iterations, None, opts.scenarios, opts.host)?;
        self.launch(&mut run, spec)
    }

    /// Starts a remotely controlled run (worker role): target 0 until the controller sets it.
    pub fn start_remote(
        &self,
        index: u32,
        count: u32,
        iteration_limit: Option<u64>,
        scenarios: Option<Vec<String>>,
        host: Option<String>,
    ) -> Result<()> {
        let mut run = self.inner.run.lock();
        if run.as_ref().is_some_and(|h| !h.task.is_finished()) {
            return Err(Error::Engine("a run is already in progress".into()));
        }
        if !matches!(self.inner.role, Role::Worker(_)) {
            return Err(Error::Engine("remote start is only valid for workers".into()));
        }
        self.inner.feeders.set_partition(index, count);
        let shape = Arc::clone(&self.inner.base_shape);
        let spec = self.run_spec(shape, true, iteration_limit, Some((index, count)), scenarios, host)?;
        self.launch(&mut run, spec)
    }

    fn run_spec(
        &self,
        shape: Arc<Shape>,
        remote: bool,
        iteration_limit: Option<u64>,
        partition: Option<(u32, u32)>,
        selection: Option<Vec<String>>,
        host: Option<String>,
    ) -> Result<RunSpec> {
        let run_plan = match &host {
            Some(h) => Arc::new(self.inner.plan.with_host(h)?),
            None => Arc::clone(&self.inner.plan),
        };
        let plan = &run_plan;
        let scenarios = match &selection {
            Some(names) => Arc::new(self.inner.scenarios.subset(names)?),
            None => Arc::clone(&self.inner.scenarios),
        };
        let local = !matches!(self.inner.role, Role::Controller(_));
        let shared_client = match plan.target.client {
            ClientMode::Shared if local => Some(build_client(plan)?),
            _ => None,
        };
        let mut listeners = self.inner.listeners.clone();
        if local && let Some(p) = &plan.report.request_log {
            listeners.push(Arc::new(CsvRequestLog::create(p)?));
        }
        if local && let Some(p) = &plan.report.jtl {
            listeners.push(Arc::new(CsvRequestLog::create_with(p, crate::csv::LogFormat::Jtl)?));
        }
        if let Some(i) = &plan.report.influx
            && !matches!(self.inner.role, Role::Worker(_))
        {
            listeners.push(Arc::new(crate::influx::InfluxListener::create(i, plan.display_name())?));
        }
        #[cfg(feature = "otel")]
        if let Some(o) = &plan.report.otlp {
            let role = match self.inner.role {
                Role::Standalone => "standalone",
                Role::Controller(_) => "controller",
                Role::Worker(_) => "worker",
            };
            listeners.push(Arc::new(crate::otel::OtlpListener::create(o, plan.display_name(), role)?));
        }
        Ok(RunSpec {
            shared_client,
            shape,
            listeners: listeners.into(),
            remote,
            iteration_limit,
            partition,
            scenarios,
            selection,
            plan: Arc::clone(&run_plan),
            host,
        })
    }

    fn launch(&self, run: &mut Option<RunHandle>, spec: RunSpec) -> Result<()> {
        self.inner.shared.clear();
        {
            *self.inner.stats.lock() = Stats::with_percentiles(Arc::from(self.inner.plan.report.percentiles.clone()));
            let mut m = self.inner.meta.lock();
            m.state = RunState::Running;
            m.users = 0;
            m.target = 0.0;
            m.manual = false;
            m.aborted = false;
            m.thresholds.clear();
            m.generator = GeneratorHealth::default();
            m.iterations_started = 0;
            m.started_at = Some(SystemTime::now());
            m.ended_at = None;
        }
        publish(&self.inner);
        let (cmd, rx) = mpsc::channel(32);
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(run_loop(inner, rx, spec));
        *run = Some(RunHandle { cmd, task });
        Ok(())
    }

    fn send(&self, c: Command) -> Result<()> {
        let run = self.inner.run.lock();
        match run.as_ref() {
            Some(h) if !h.task.is_finished() => h.cmd.try_send(c).map_err(|_| Error::Engine("engine is busy, retry".into())),
            _ => Err(Error::Engine("no run in progress".into())),
        }
    }

    pub fn stop(&self) -> Result<()> {
        self.send(Command::Stop)
    }

    /// Overrides the load profile with a manual target, approached at `ramp` units per second (0 = immediate).
    pub fn set_target(&self, target: f64, ramp: f64) -> Result<()> {
        let cap = self.inner.plan.target_cap();
        if !target.is_finite() || !(0.0..=cap).contains(&target) || !ramp.is_finite() || ramp < 0.0 {
            return Err(Error::Engine(format!("target must be within [0, {cap}] and ramp >= 0")));
        }
        self.send(Command::SetTarget { target, ramp })
    }

    /// Applies a new cluster partition to the running test (worker role).
    pub fn repartition(&self, index: u32, count: u32) -> Result<()> {
        if count == 0 || index >= count {
            return Err(Error::Engine("invalid partition".into()));
        }
        self.send(Command::Repartition { index, count })
    }

    pub fn resume_shape(&self) -> Result<()> {
        self.send(Command::ResumeShape)
    }

    pub fn reset_stats(&self) {
        self.inner.stats.lock().reset_counters();
        publish(&self.inner);
    }

    /// Waits until the current run (if any) has fully stopped.
    pub async fn wait(&self) -> Arc<Snapshot> {
        let mut rx = self.subscribe();
        loop {
            let done = matches!(rx.borrow_and_update().state, RunState::Stopped | RunState::Idle);
            if done {
                return rx.borrow().clone();
            }
            if rx.changed().await.is_err() {
                return self.snapshot();
            }
        }
    }

    pub fn aborted(&self) -> bool {
        self.inner.meta.lock().aborted
    }
}

fn build_snapshot(plan: &Plan, stats: &Stats, meta: &Meta, timeline: usize) -> Snapshot {
    Snapshot {
        plan: plan.display_name().to_owned(),
        state: meta.state,
        mode: plan.load.mode,
        elapsed_secs: stats.elapsed(),
        users: meta.users,
        target: meta.target,
        manual_target: meta.manual,
        total: stats.total_row(),
        rows: stats.rows(Kind::Request),
        iterations: stats.rows(Kind::Iteration),
        errors: stats.error_rows(100),
        dropped_iterations: stats.dropped(),
        timeline: stats.timeline_tail(timeline),
        thresholds: meta.thresholds.clone(),
        aborted: meta.aborted,
        generator: meta.generator.clone(),
        iterations_started: meta.iterations_started,
        workers: meta.workers.clone(),
        transactions: stats.rows(Kind::Transaction),
        percentiles: stats.percentile_labels(),
        ratio: build_ratio(meta, stats),
        distribution: stats.distribution(),
    }
}

fn share(part: f64, whole: f64) -> f64 {
    if whole > 0.0 { part / whole } else { 0.0 }
}

fn build_ratio(meta: &Meta, stats: &Stats) -> Ratio {
    let iterations = stats.rows(Kind::Iteration);
    let weight_total: u64 = meta.weights.iter().map(|w| w.1).sum();
    let user_total: u64 = meta.scenario_users.iter().map(|u| u.1).sum();
    let iter_total: u64 = iterations.iter().map(|r| r.count).sum();
    let scenarios = meta
        .weights
        .iter()
        .map(|(name, w)| {
            let users = meta.scenario_users.iter().find(|u| &u.0 == name).map_or(0, |u| u.1);
            let iters = iterations.iter().find(|r| &r.scenario == name).map_or(0, |r| r.count);
            ScenarioShare {
                name: name.clone(),
                weight_share: share(*w as f64, weight_total as f64),
                users,
                user_share: share(users as f64, user_total as f64),
                iterations: iters,
                iteration_share: share(iters as f64, iter_total as f64),
            }
        })
        .collect();
    let counts = stats.task_counts();
    let tasks = counts
        .iter()
        .map(|(sc, task, n)| {
            let scen_total: u64 = counts.iter().filter(|c| &c.0 == sc).map(|c| c.2).sum();
            TaskShare { scenario: sc.clone(), task: task.clone(), count: *n, share: share(*n as f64, scen_total as f64) }
        })
        .collect();
    Ratio { scenarios, tasks }
}

/// Keeps the worker list fresh on the dashboard while no run is active.
async fn publish_idle_workers(weak: std::sync::Weak<Inner>, hub: Arc<Hub>) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let idle = {
            let mut m = inner.meta.lock();
            let idle = !matches!(m.state, RunState::Running | RunState::Stopping);
            if idle {
                m.workers = hub.infos();
            }
            idle
        };
        if idle {
            publish(&inner);
        }
    }
}

fn publish(inner: &Inner) -> Arc<Snapshot> {
    let snap = {
        let stats = inner.stats.lock();
        let meta = inner.meta.lock();
        Arc::new(build_snapshot(&inner.plan, &stats, &meta, LIVE_TIMELINE))
    };
    inner.tx.send_replace(Arc::clone(&snap));
    snap
}

struct Collector<'a> {
    inner: &'a Inner,
    ctx: &'a RunCtx,
    monitor: Monitor,
    listeners: &'a [Arc<dyn Listener>],
}

impl Collector<'_> {
    fn collect(&mut self, elapsed: Duration, dt: Duration, final_pass: bool) {
        let mut users = self.ctx.active.load(Ordering::Relaxed);
        let limit = self.ctx.iteration_limit.unwrap_or(u64::MAX);
        let mut started = self.ctx.iterations.load(Ordering::Relaxed).min(limit);
        let mut health = self.monitor.sample().clone();
        let local_health = health.clone();
        let mut workers = Vec::new();
        let mut scenario_users = self.ctx.scenario_users();
        let mut task_deltas = self.ctx.drain_task_counts();
        let local_tasks = task_deltas.clone();
        let local_users = scenario_users.clone();
        if let Role::Controller(hub) = &self.inner.role {
            let (remote_users, remote_tasks) = hub.take_mix();
            for (name, n) in remote_users {
                match scenario_users.iter_mut().find(|u| u.0 == name) {
                    Some(u) => u.1 += n,
                    None => scenario_users.push((name, n)),
                }
            }
            task_deltas.extend(remote_tasks);
            let t = hub.totals();
            users += t.active;
            started += t.iterations;
            if let Some(r) = t.health {
                health.cpu_pct = r.cpu_pct.or(health.cpu_pct);
                health.scheduler_lag_ms = r.scheduler_lag_ms;
                health.saturated = r.saturated;
                health.saturated_secs = r.saturated_secs;
            }
            workers = hub.infos();
        }
        let target = {
            let mut m = self.inner.meta.lock();
            m.users = users;
            m.generator = health;
            m.iterations_started = started;
            m.workers = workers;
            m.scenario_users = scenario_users;
            m.target
        };
        let drained = self.ctx.recorder.drain();
        if let Role::Worker(tx) = &self.inner.role {
            let mut window = export(&drained);
            window.scenario_users = local_users;
            window.task_counts = local_tasks;
            let update =
                WorkerUpdate { active: self.ctx.active.load(Ordering::Relaxed), iterations: started, health: local_health, window };
            if tx.try_send(update).is_err() {
                tracing::warn!("controller link is congested; a stats window was dropped");
            }
        }
        {
            let mut stats = self.inner.stats.lock();
            stats.ingest(drained, dt.as_secs_f64(), elapsed.as_secs_f64(), users, target);
            stats.add_task_counts(task_deltas);
        }
        if !self.inner.plan.thresholds.is_empty() {
            let snap = {
                let stats = self.inner.stats.lock();
                let meta = self.inner.meta.lock();
                build_snapshot(&self.inner.plan, &stats, &meta, 0)
            };
            let results = threshold::evaluate(&self.inner.plan.thresholds, &snap, final_pass);
            self.inner.meta.lock().thresholds = results;
        }
        let snap = publish(self.inner);
        for l in self.listeners {
            l.on_tick(&snap);
        }
    }
}

fn should_abort(inner: &Inner, elapsed: Duration) -> bool {
    let meta = inner.meta.lock();
    inner
        .plan
        .thresholds
        .iter()
        .zip(meta.thresholds.iter())
        .any(|(cfg, res)| cfg.abort_on_fail && !res.passed && elapsed >= cfg.abort_delay)
}

fn approach(current: f64, target: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return target;
    }
    if current < target { (current + step).min(target) } else { (current - step).max(target) }
}

async fn run_loop(inner: Arc<Inner>, mut rx: mpsc::Receiver<Command>, spec: RunSpec) {
    let RunSpec { shared_client, shape, listeners, remote, iteration_limit, partition, scenarios, selection, plan, host } = spec;
    let mut run_ctx = RunCtx::new(Arc::clone(&plan), Arc::clone(&inner.feeders), shared_client, Arc::clone(&listeners), false);
    run_ctx.iteration_limit = iteration_limit;
    run_ctx.shared = Arc::clone(&inner.shared);
    run_ctx.set_scenarios(
        scenarios.entries.iter().map(|e| Arc::clone(&e.name)).collect(),
        scenarios.entries.iter().map(|e| e.weight).collect(),
    );
    inner.meta.lock().weights = scenarios.entries.iter().map(|e| (e.name.to_string(), e.weight)).collect();
    let mut fatal = false;
    if !matches!(inner.role, Role::Controller(_)) {
        match crate::protocols::Protocols::connect(&plan).await {
            Ok(p) => run_ctx.protocols = Arc::new(p),
            Err(e) => {
                tracing::error!(error = %e, "protocol setup failed, aborting run");
                inner.meta.lock().aborted = true;
                fatal = true;
            }
        }
    }
    let ctx = Arc::new(run_ctx);
    for l in listeners.iter() {
        l.on_test_start(&plan);
    }
    let hub = match &inner.role {
        Role::Controller(h) => Some(Arc::clone(h)),
        _ => None,
    };
    let mut driver = match (&hub, plan.load.mode) {
        (Some(h), mode) => {
            if let Err(e) = h.begin_run(Arc::clone(&ctx), mode, selection.clone(), host.clone()) {
                tracing::error!(error = %e, "cannot start distributed run");
            }
            Driver::Cluster(Arc::clone(h))
        }
        (None, LoadMode::Users) => Driver::Closed(closed::ClosedDriver::new(Arc::clone(&ctx), Arc::clone(&scenarios), partition)),
        (None, LoadMode::ArrivalRate) => Driver::Open(open::OpenDriver::new(Arc::clone(&ctx), Arc::clone(&scenarios))),
    };
    let mut col = Collector { inner: &inner, ctx: &ctx, monitor: Monitor::new(), listeners: &listeners };
    tracing::info!(plan = plan.display_name(), mode = ?plan.load.mode, "run started");

    let start = Instant::now();
    let mut last_stats = start;
    let mut current = 0.0_f64;
    let mut manual: Option<(f64, f64)> = remote.then_some((0.0, 0.0));
    let mut last_control = start;
    let peak = shape.peak();
    let mut stats_reset = !plan.load.reset_stats_after_ramp || remote;
    if plan.load.reset_stats_after_ramp && peak.is_none() {
        tracing::warn!("reset_stats_after_ramp has no effect with a scripted shape");
        stats_reset = true;
    }
    let mut ticker = tokio::time::interval(CONTROL_TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        if fatal {
            break;
        }
        tokio::select! {
            scheduled = ticker.tick() => {
                col.monitor.observe_lag(tokio::time::Instant::now().saturating_duration_since(scheduled));
            }
            cmd = rx.recv() => match cmd {
                Some(Command::Stop) | None => break,
                Some(Command::SetTarget { target, ramp }) => manual = Some((target, ramp)),
                Some(Command::ResumeShape) => if !remote { manual = None },
                Some(Command::Repartition { index, count }) => {
                    inner.feeders.repartition(index, count, false);
                    if let Driver::Closed(d) = &mut driver {
                        d.repartition(index, count);
                    }
                    tracing::info!(index, count, "cluster membership changed, partition updated");
                }
            },
        }
        let now = Instant::now();
        let elapsed = now - start;
        let dt = now - last_control;
        last_control = now;

        current = match manual {
            Some((t, ramp)) => approach(current, t, ramp * dt.as_secs_f64()),
            None => match shape.target_at(elapsed) {
                Some(v) => v,
                None => break,
            },
        };
        {
            let mut m = inner.meta.lock();
            m.target = current;
            m.manual = manual.is_some();
        }
        driver.adjust(current);

        if !stats_reset
            && manual.is_none()
            && let Some(p) = peak
            && current >= p - 1e-9
        {
            stats_reset = true;
            inner.stats.lock().reset_counters();
            tracing::info!("profile reached its peak, statistics reset");
        }
        if let Some(h) = &hub
            && h.run_finished()
        {
            tracing::info!("all workers finished");
            break;
        }
        if ctx.limit_reached.load(Ordering::Relaxed) {
            tracing::info!(limit = plan.load.iterations.unwrap_or(0), "iteration limit reached");
            break;
        }
        if now - last_stats >= STATS_TICK {
            col.collect(elapsed, now - last_stats, false);
            last_stats = now;
            if should_abort(&inner, elapsed) {
                tracing::warn!("threshold with abort_on_fail violated, stopping run");
                inner.meta.lock().aborted = true;
                break;
            }
        }
    }

    inner.meta.lock().state = RunState::Stopping;
    publish(&inner);
    tracing::info!("stopping virtual users");

    {
        let shutdown = driver.shutdown(plan.load.graceful_stop);
        tokio::pin!(shutdown);
        let mut stats_tick = tokio::time::interval(STATS_TICK);
        stats_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        stats_tick.tick().await;
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                _ = stats_tick.tick() => {
                    let now = Instant::now();
                    col.collect(now - start, now - last_stats, false);
                    last_stats = now;
                }
            }
        }
    }

    let now = Instant::now();
    inner.meta.lock().target = 0.0;
    col.collect(now - start, (now - last_stats).max(Duration::from_millis(1)), true);
    {
        let mut m = inner.meta.lock();
        m.state = RunState::Stopped;
        m.users = 0;
        m.ended_at = Some(SystemTime::now());
    }
    let snap = publish(&inner);
    for l in listeners.iter() {
        l.on_test_stop(&snap);
    }
    tracing::info!(elapsed_secs = (now - start).as_secs_f64(), "run finished");
}

/// Runs one iteration, recording its duration. Returns false when the run was cancelled
/// or the iteration limit is exhausted.
pub(crate) async fn run_iteration(entry: &ScenarioEntry, vu: &mut Vu) -> bool {
    if !vu.ctx.claim_iteration() {
        return false;
    }
    let key = Key::iteration(&entry.name);
    let start = Instant::now();
    let res = entry.imp.iteration(vu).await;
    let latency = start.elapsed();
    if !matches!(res, Err(Error::Cancelled)) {
        let reason = if res.is_ok() { "transaction not ended in its iteration" } else { "iteration failed inside transaction" };
        vu.abandon_transactions(reason);
    }
    match res {
        Ok(()) => vu.record(&key, latency, true, None, (0, 0), None),
        Err(Error::Cancelled) => return false,
        Err(e) => {
            let msg = (!e.is_recorded()).then(|| e.to_string());
            if let Some(m) = &msg {
                tracing::debug!(scenario = %entry.name, error = %m, "iteration failed");
                if vu.ctx.trace {
                    println!("[vu {}] iteration error: {m}", vu.id());
                }
            }
            vu.record(&key, latency, false, None, (0, 0), msg.as_deref());
        }
    }
    true
}

/// Runs a lifecycle hook (on_start / on_stop), recording non-request errors. Returns false when cancelled.
pub(crate) async fn run_hook(entry: &ScenarioEntry, vu: &mut Vu, start_hook: bool) -> bool {
    let res = if start_hook { entry.imp.on_start(vu).await } else { entry.imp.on_stop(vu).await };
    match res {
        Ok(()) => true,
        Err(Error::Cancelled) => false,
        Err(e) => {
            if !e.is_recorded() {
                let key = Key::iteration(&entry.name);
                let hook = if start_hook { "on_start" } else { "on_stop" };
                vu.record_error(&key, &format!("{hook}: {e}"));
                if vu.ctx.trace {
                    println!("[vu {}] {hook} error: {e}", vu.id());
                }
            }
            true
        }
    }
}

pub(crate) struct ActiveGuard(pub Arc<RunCtx>, usize);

impl ActiveGuard {
    pub fn new(ctx: Arc<RunCtx>, scenario: usize) -> Self {
        ctx.active.fetch_add(1, Ordering::Relaxed);
        if let Some(a) = ctx.scenario_active.get(scenario) {
            a.fetch_add(1, Ordering::Relaxed);
        }
        Self(ctx, scenario)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
        if let Some(a) = self.0.scenario_active.get(self.1) {
            a.fetch_sub(1, Ordering::Relaxed);
        }
    }
}
