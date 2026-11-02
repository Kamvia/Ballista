// File: ballista/src/engine/closed.rs
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::{ActiveGuard, run_hook, run_iteration};
use crate::metrics::Key;
use crate::scenario::ScenarioSet;
use crate::vu::{RunCtx, Vu};

const HARD_STOP_GRACE: Duration = Duration::from_secs(5);

/// Closed workload model: a population of users looping through iterations with wait times.
/// Fixed-count scenarios are filled first; the remainder tracks scenario weights exactly.
pub(crate) struct ClosedDriver {
    ctx: Arc<RunCtx>,
    set: Arc<ScenarioSet>,
    users: Vec<(CancellationToken, usize)>,
    counts: Vec<u64>,
    fixed: Vec<Option<u64>>,
    tasks: JoinSet<()>,
    graceful: CancellationToken,
    spawned: u64,
    capped_logged: bool,
}

impl ClosedDriver {
    pub fn new(ctx: Arc<RunCtx>, set: Arc<ScenarioSet>, partition: Option<(u32, u32)>) -> Self {
        let n = set.entries.len();
        let fixed = match partition {
            Some((index, count)) => set.fixed_share(index, count),
            None => set.entries.iter().map(|e| e.fixed).collect(),
        };
        Self {
            ctx,
            set,
            users: Vec::new(),
            counts: vec![0; n],
            fixed,
            tasks: JoinSet::new(),
            graceful: CancellationToken::new(),
            spawned: 0,
            capped_logged: false,
        }
    }

    /// Recomputes this worker's fixed-count shares after cluster membership changed.
    pub fn repartition(&mut self, index: u32, count: u32) {
        self.fixed = self.set.fixed_share(index, count);
    }

    pub fn adjust(&mut self, target: f64) {
        while let Some(res) = self.tasks.try_join_next() {
            if let Err(e) = res
                && e.is_panic()
            {
                tracing::error!("virtual user task panicked");
            }
        }
        let want = target.round().max(0.0) as usize;
        while self.users.len() < want {
            let Some(idx) = self.set.choose_with_fixed(&self.counts, &self.fixed) else {
                if !self.capped_logged {
                    self.capped_logged = true;
                    tracing::warn!(users = self.users.len(), "all scenarios use fixed_count; target exceeds their sum");
                }
                break;
            };
            self.spawn_user(idx);
        }
        while self.users.len() > want {
            if let Some((t, idx)) = self.users.pop() {
                t.cancel();
                self.counts[idx] -= 1;
            }
        }
    }

    fn spawn_user(&mut self, idx: usize) {
        let id = self.spawned;
        self.spawned += 1;
        let stop = self.graceful.child_token();
        self.users.push((stop.clone(), idx));
        self.counts[idx] += 1;
        self.tasks.spawn(user_loop(Arc::clone(&self.ctx), Arc::clone(&self.set), idx, id, stop));
    }

    pub async fn shutdown(&mut self, graceful: Duration) {
        self.graceful.cancel();
        self.users.clear();
        let drained = tokio::time::timeout(graceful, async { while self.tasks.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tracing::warn!(remaining = self.tasks.len(), "graceful stop elapsed, cancelling in-flight work");
            self.ctx.cancel.cancel();
            let _ = tokio::time::timeout(HARD_STOP_GRACE, async { while self.tasks.join_next().await.is_some() {} }).await;
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
        }
        self.ctx.cancel.cancel();
    }
}

async fn user_loop(ctx: Arc<RunCtx>, set: Arc<ScenarioSet>, idx: usize, id: u64, stop: CancellationToken) {
    let entry = &set.entries[idx];
    let mut vu = match Vu::new(id, Arc::clone(&ctx), Arc::clone(&entry.name)) {
        Ok(v) => v,
        Err(e) => {
            ctx.recorder.record_error(id as usize, &Key::iteration(&entry.name), &e.to_string());
            return;
        }
    };
    let _guard = ActiveGuard::new(Arc::clone(&ctx), idx);
    if !run_hook(entry, &mut vu, true).await {
        return;
    }
    vu.started = true;
    while !stop.is_cancelled() {
        let began = Instant::now();
        if !run_iteration(entry, &mut vu).await {
            break;
        }
        match entry.delay_after(began.elapsed()) {
            Some(d) => {
                tokio::select! {
                    () = stop.cancelled() => break,
                    () = ctx.cancel.cancelled() => return,
                    () = tokio::time::sleep(d) => {}
                }
            }
            None => tokio::task::yield_now().await,
        }
    }
    if !ctx.cancel.is_cancelled() {
        run_hook(entry, &mut vu, false).await;
    }
}
