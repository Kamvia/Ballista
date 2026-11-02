// File: ballista/src/monitor.rs
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const CPU_LIMIT_PCT: f64 = 90.0;
const LAG_LIMIT: Duration = Duration::from_millis(50);
const WARN_EVERY: Duration = Duration::from_secs(30);

/// Load generator health. When saturated, measured latencies include generator-side delay.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneratorHealth {
    pub cpu_pct: Option<f64>,
    pub scheduler_lag_ms: f64,
    pub saturated: bool,
    pub saturated_secs: u64,
}

pub(crate) struct Monitor {
    cores: f64,
    last_cpu: Option<(Instant, f64)>,
    max_lag: Duration,
    last_warn: Option<Instant>,
    pub health: GeneratorHealth,
}

/// Process CPU seconds from /proc (Linux). Clock ticks are 100 Hz on all mainstream Linux targets.
fn process_cpu_secs() -> Option<f64> {
    let raw = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after = raw.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) / 100.0)
}

impl Monitor {
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
        Self {
            cores,
            last_cpu: process_cpu_secs().map(|c| (Instant::now(), c)),
            max_lag: Duration::ZERO,
            last_warn: None,
            health: GeneratorHealth::default(),
        }
    }

    /// Records how late a scheduler tick fired.
    pub fn observe_lag(&mut self, lag: Duration) {
        self.max_lag = self.max_lag.max(lag);
    }

    /// Closes a sampling window; call once per stats tick.
    pub fn sample(&mut self) -> &GeneratorHealth {
        let now = Instant::now();
        let cpu_pct = match (self.last_cpu, process_cpu_secs()) {
            (Some((t0, c0)), Some(c1)) => {
                let wall = now.duration_since(t0).as_secs_f64();
                self.last_cpu = Some((now, c1));
                (wall > 0.0).then(|| ((c1 - c0) / wall / self.cores * 100.0).clamp(0.0, 100.0))
            }
            (_, c) => {
                self.last_cpu = c.map(|c| (now, c));
                None
            }
        };
        let lag = std::mem::take(&mut self.max_lag);
        let saturated = cpu_pct.is_some_and(|c| c >= CPU_LIMIT_PCT) || lag >= LAG_LIMIT;
        self.health.cpu_pct = cpu_pct;
        self.health.scheduler_lag_ms = lag.as_secs_f64() * 1000.0;
        self.health.saturated = saturated;
        if saturated {
            self.health.saturated_secs += 1;
            if self.last_warn.is_none_or(|w| now.duration_since(w) >= WARN_EVERY) {
                self.last_warn = Some(now);
                tracing::warn!(
                    cpu_pct = cpu_pct.unwrap_or(-1.0),
                    lag_ms = self.health.scheduler_lag_ms,
                    "load generator is saturated; latency figures are unreliable, reduce load or add capacity"
                );
            }
        }
        &self.health
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_triggers_saturation() {
        let mut m = Monitor::new();
        m.observe_lag(Duration::from_millis(80));
        assert!(m.sample().saturated);
        assert!(!m.sample().saturated || m.health.cpu_pct.is_some_and(|c| c >= CPU_LIMIT_PCT));
    }
}
