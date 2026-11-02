// File: ballista/src/engine/open.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{ActiveGuard, CONTROL_TICK, run_hook, run_iteration};
use crate::metrics::Key;
use crate::scenario::ScenarioSet;
use crate::vu::{RunCtx, Vu};

const MAX_LAUNCH_PER_TICK: u64 = 200_000;
const HARD_STOP_GRACE: Duration = Duration::from_secs(5);

struct Shared {
    ctx: Arc<RunCtx>,
    set: Arc<ScenarioSet>,
    pools: Vec<Mutex<Vec<Vu>>>,
    allocated: AtomicU64,
    next_id: AtomicU64,
    max: u64,
}

/// Open workload model: iterations start on a fixed arrival schedule regardless of response times,
/// which avoids coordinated omission. Arrivals that cannot get a free user are counted as dropped.
pub(crate) struct OpenDriver {
    shared: Arc<Shared>,
    /// Start time of the most recently scheduled arrival; the next one follows at the current rate.
    last: Option<Instant>,
    tasks: JoinSet<()>,
    stopped: bool,
}

impl OpenDriver {
    pub fn new(ctx: Arc<RunCtx>, set: Arc<ScenarioSet>) -> Self {
        let pools: Vec<Mutex<Vec<Vu>>> = (0..set.entries.len()).map(|_| Mutex::new(Vec::new())).collect();
        let shared = Shared { max: ctx.plan.load.max_users, ctx, set, pools, allocated: AtomicU64::new(0), next_id: AtomicU64::new(0) };
        let mut counts = vec![0_u64; shared.set.entries.len()];
        for _ in 0..shared.ctx.plan.load.preallocate {
            let idx = shared.set.choose_for_spawn(&counts).unwrap_or(0);
            counts[idx] += 1;
            let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
            match Vu::new(id, Arc::clone(&shared.ctx), Arc::clone(&shared.set.entries[idx].name)) {
                Ok(vu) => {
                    shared.pools[idx].lock().push(vu);
                    shared.allocated.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to preallocate virtual user");
                    break;
                }
            }
        }
        Self { shared: Arc::new(shared), last: None, tasks: JoinSet::new(), stopped: false }
    }

    /// Schedules every arrival due before the next control tick at its exact start time.
    pub fn adjust(&mut self, rate_per_sec: f64) {
        while self.tasks.try_join_next().is_some() {}
        if self.stopped {
            return;
        }
        let now = Instant::now();
        let horizon = now + CONTROL_TICK;
        if rate_per_sec <= 0.0 {
            self.last = None;
            return;
        }
        // The gap is derived from the current rate, so a rising rate (for example a ramp from zero)
        // takes effect immediately instead of waiting out a gap computed at the earlier, lower rate.
        let gap = Duration::from_secs_f64(1.0 / rate_per_sec);
        let mut at = self.last.map_or(now, |l| (l + gap).max(now - CONTROL_TICK));
        let mut launched = 0_u64;
        while at < horizon && launched < MAX_LAUNCH_PER_TICK {
            let start = at.max(now);
            self.tasks.spawn(arrival(Arc::clone(&self.shared), start));
            self.last = Some(at);
            at += gap;
            launched += 1;
        }
    }

    pub async fn shutdown(&mut self, graceful: Duration) {
        self.stopped = true;
        let drained = tokio::time::timeout(graceful, async { while self.tasks.join_next().await.is_some() {} }).await;
        let ctx = Arc::clone(&self.shared.ctx);
        if drained.is_err() {
            tracing::warn!(remaining = self.tasks.len(), "graceful stop elapsed, cancelling in-flight iterations");
            ctx.cancel.cancel();
            let _ = tokio::time::timeout(HARD_STOP_GRACE, async { while self.tasks.join_next().await.is_some() {} }).await;
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
        } else {
            let mut stops = JoinSet::new();
            for (idx, pool) in self.shared.pools.iter().enumerate() {
                let vus: Vec<Vu> = pool.lock().drain(..).collect();
                for mut vu in vus.into_iter().filter(|v| v.started) {
                    let shared = Arc::clone(&self.shared);
                    stops.spawn(async move {
                        run_hook(&shared.set.entries[idx], &mut vu, false).await;
                    });
                }
            }
            if tokio::time::timeout(HARD_STOP_GRACE, async { while stops.join_next().await.is_some() {} }).await.is_err() {
                ctx.cancel.cancel();
                stops.abort_all();
                while stops.join_next().await.is_some() {}
            }
        }
        ctx.cancel.cancel();
    }
}

async fn arrival(shared: Arc<Shared>, start: Instant) {
    let ctx = &shared.ctx;
    tokio::select! {
        () = ctx.cancel.cancelled() => return,
        () = tokio::time::sleep_until(start) => {}
    }
    let idx = shared.set.pick_random();
    let entry = &shared.set.entries[idx];
    let pooled = shared.pools[idx].lock().pop();
    let mut vu = match pooled {
        Some(v) => v,
        None => {
            if shared.allocated.fetch_add(1, Ordering::Relaxed) >= shared.max {
                shared.allocated.fetch_sub(1, Ordering::Relaxed);
                ctx.recorder.record_dropped(idx, 1);
                return;
            }
            let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
            match Vu::new(id, Arc::clone(ctx), Arc::clone(&entry.name)) {
                Ok(v) => v,
                Err(e) => {
                    shared.allocated.fetch_sub(1, Ordering::Relaxed);
                    ctx.recorder.record_error(id as usize, &Key::iteration(&entry.name), &e.to_string());
                    return;
                }
            }
        }
    };
    let _guard = ActiveGuard::new(Arc::clone(ctx), idx);
    if !vu.started {
        if !run_hook(entry, &mut vu, true).await {
            return;
        }
        vu.started = true;
    }
    run_iteration(entry, &mut vu).await;
    if !ctx.cancel.is_cancelled() {
        shared.pools[idx].lock().push(vu);
    }
}
