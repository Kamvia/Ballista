// File: ballista/src/engine/debug.rs
use std::sync::Arc;
use std::time::Instant;

use super::{Meta, build_snapshot, run_hook, run_iteration};
use crate::config::{ClientMode, Plan};
use crate::error::{Error, Result};
use crate::listener::Listener;
use crate::metrics::{RunState, Snapshot, Stats};
use crate::monitor::GeneratorHealth;
use crate::scenario::feeder::Feeders;
use crate::scenario::{Registry, ScenarioSet};
use crate::vu::{RunCtx, Vu, build_client};

const MAX_DEBUG_ITERATIONS: u64 = 1000;

/// Runs a single virtual user for `iterations` iterations with request/response tracing (secrets redacted).
pub async fn debug_run(plan: Plan, registry: &Registry, scenario: Option<&str>, iterations: u64) -> Result<Snapshot> {
    if iterations == 0 || iterations > MAX_DEBUG_ITERATIONS {
        return Err(Error::config(format!("debug iterations must be 1..{MAX_DEBUG_ITERATIONS}")));
    }
    let plan = Arc::new(plan);
    let feeders = Arc::new(Feeders::load(&plan.feeders)?);
    let store = Arc::new(crate::shared::SharedStore::default());
    let set = ScenarioSet::build(&plan, registry, &feeders, &store)?;
    let idx = match scenario {
        Some(name) => set
            .entries
            .iter()
            .position(|e| &*e.name == name)
            .ok_or_else(|| Error::config(format!("scenario '{name}' not found (or excluded by tags)")))?,
        None => 0,
    };
    let shared = match plan.target.client {
        ClientMode::Shared => Some(build_client(&plan)?),
        ClientMode::PerUser => None,
    };
    let listeners: Arc<[Arc<dyn Listener>]> = Arc::from(Vec::new());
    let mut run_ctx = RunCtx::new(Arc::clone(&plan), feeders, shared, listeners, true);
    run_ctx.protocols = Arc::new(crate::protocols::Protocols::connect(&plan).await?);
    run_ctx.shared = store;
    run_ctx.set_scenarios(set.entries.iter().map(|e| Arc::clone(&e.name)).collect(), set.entries.iter().map(|e| e.weight).collect());
    let ctx = Arc::new(run_ctx);
    let entry = &set.entries[idx];
    println!("debug: scenario '{}', {iterations} iteration(s)", entry.name);

    let start = Instant::now();
    let mut vu = Vu::new(0, Arc::clone(&ctx), Arc::clone(&entry.name))?;
    run_hook(entry, &mut vu, true).await;
    for n in 1..=iterations {
        println!("debug: iteration {n}");
        if !run_iteration(entry, &mut vu).await {
            break;
        }
    }
    run_hook(entry, &mut vu, false).await;
    let elapsed = start.elapsed().as_secs_f64().max(0.001);

    let mut stats = Stats::with_percentiles(Arc::from(plan.report.percentiles.clone()));
    stats.ingest(ctx.recorder.drain(), elapsed, elapsed, 1, 1.0);
    stats.add_task_counts(ctx.drain_task_counts());
    let meta = Meta {
        state: RunState::Stopped,
        users: 0,
        target: 0.0,
        manual: false,
        aborted: false,
        thresholds: Vec::new(),
        generator: GeneratorHealth::default(),
        iterations_started: iterations,
        workers: Vec::new(),
        weights: set.entries.iter().map(|e| (e.name.to_string(), e.weight)).collect(),
        scenario_users: Vec::new(),
        started_at: None,
        ended_at: None,
    };
    Ok(build_snapshot(&plan, &stats, &meta, 0))
}
