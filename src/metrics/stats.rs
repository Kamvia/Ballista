// File: ballista/src/metrics/stats.rs
use std::collections::{HashMap, VecDeque};

use hdrhistogram::Histogram;
use serde::Serialize;

use super::{Key, Kind, Shard, new_hist};
use crate::threshold::ThresholdResult;

const RECENT_WINDOWS: usize = 5;
const MAX_TIMELINE: usize = 21_600;
const MAX_ERROR_ROWS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Running,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub t: f64,
    pub users: u64,
    pub target: f64,
    pub rps: f64,
    pub fail_rps: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Row {
    pub kind: Option<Kind>,
    pub scenario: String,
    pub method: String,
    pub name: String,
    pub count: u64,
    pub failures: u64,
    pub fail_ratio: f64,
    pub rps: f64,
    pub fail_rps: f64,
    pub rps_avg: f64,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub avg_bytes: f64,
    /// Values for the configured `report.percentiles`, in the same order.
    pub percentiles: Vec<f64>,
}

/// Request count in one latency range; `upper_ms` is `None` for the open-ended last bucket.
#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub upper_ms: Option<f64>,
    pub count: u64,
}

const BUCKET_BOUNDS_MS: [f64; 15] =
    [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 30_000.0, 60_000.0];

#[derive(Debug, Clone, Serialize)]
pub struct ErrorRow {
    pub scenario: String,
    pub method: String,
    pub name: String,
    pub message: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub plan: String,
    pub state: RunState,
    pub mode: crate::config::LoadMode,
    pub elapsed_secs: f64,
    pub users: u64,
    pub target: f64,
    pub manual_target: bool,
    pub total: Row,
    pub rows: Vec<Row>,
    pub iterations: Vec<Row>,
    pub errors: Vec<ErrorRow>,
    pub dropped_iterations: u64,
    pub timeline: Vec<Point>,
    pub thresholds: Vec<ThresholdResult>,
    pub aborted: bool,
    pub generator: crate::monitor::GeneratorHealth,
    pub iterations_started: u64,
    pub workers: Vec<crate::cluster::hub::WorkerInfo>,
    pub transactions: Vec<Row>,
    /// Configured percentile labels (e.g. 50, 99.9) matching `Row::percentiles`.
    pub percentiles: Vec<f64>,
    pub ratio: Ratio,
    /// Latency distribution of all requests since the start (or last reset).
    pub distribution: Vec<Bucket>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScenarioShare {
    pub name: String,
    pub weight_share: f64,
    pub users: u64,
    pub user_share: f64,
    pub iterations: u64,
    pub iteration_share: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TaskShare {
    pub scenario: String,
    pub task: String,
    pub count: u64,
    /// Share within its scenario.
    pub share: f64,
}

/// Configured versus observed mix of scenarios and tasks.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Ratio {
    pub scenarios: Vec<ScenarioShare>,
    pub tasks: Vec<TaskShare>,
}

struct Total {
    hist: Histogram<u64>,
    ok: u64,
    fail: u64,
    bytes_in: u64,
    recent: VecDeque<(f64, u64, u64)>,
}

impl Total {
    fn new() -> Self {
        Self { hist: new_hist(), ok: 0, fail: 0, bytes_in: 0, recent: VecDeque::new() }
    }

    fn push_recent(&mut self, dt: f64, count: u64, fail: u64) {
        if self.recent.len() == RECENT_WINDOWS {
            self.recent.pop_front();
        }
        self.recent.push_back((dt, count, fail));
    }

    fn current_rates(&self) -> (f64, f64) {
        let secs: f64 = self.recent.iter().map(|r| r.0).sum();
        if secs <= 0.0 {
            return (0.0, 0.0);
        }
        let count: u64 = self.recent.iter().map(|r| r.1).sum();
        let fail: u64 = self.recent.iter().map(|r| r.2).sum();
        (count as f64 / secs, fail as f64 / secs)
    }
}

pub struct Stats {
    entries: HashMap<Key, Total>,
    total: Total,
    errors: HashMap<(Key, Box<str>), u64>,
    dropped: u64,
    timeline: VecDeque<Point>,
    elapsed: f64,
    origin: f64,
    percentiles: std::sync::Arc<[f64]>,
    tasks: HashMap<(String, String), u64>,
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

impl Stats {
    pub fn new() -> Self {
        Self::with_percentiles(std::sync::Arc::from(Vec::new()))
    }

    pub fn with_percentiles(percentiles: std::sync::Arc<[f64]>) -> Self {
        Self {
            entries: HashMap::new(),
            total: Total::new(),
            errors: HashMap::new(),
            dropped: 0,
            timeline: VecDeque::new(),
            elapsed: 0.0,
            origin: 0.0,
            percentiles,
            tasks: HashMap::new(),
        }
    }

    pub fn percentile_labels(&self) -> Vec<f64> {
        self.percentiles.to_vec()
    }

    pub(crate) fn add_task_counts(&mut self, deltas: impl IntoIterator<Item = (String, String, u64)>) {
        for (scenario, task, n) in deltas {
            if self.tasks.len() < 10_000 || self.tasks.contains_key(&(scenario.clone(), task.clone())) {
                *self.tasks.entry((scenario, task)).or_insert(0) += n;
            }
        }
    }

    pub fn task_counts(&self) -> Vec<(String, String, u64)> {
        let mut v: Vec<_> = self.tasks.iter().map(|((s, t), n)| (s.clone(), t.clone(), *n)).collect();
        v.sort();
        v
    }

    pub(crate) fn ingest(&mut self, shards: Vec<Shard>, dt: f64, elapsed: f64, users: u64, target: f64) {
        self.elapsed = elapsed;
        let mut window: HashMap<Key, (u64, u64)> = HashMap::new();
        let mut win_hist = new_hist();
        let (mut win_ok, mut win_fail) = (0_u64, 0_u64);

        for shard in shards {
            self.dropped += shard.dropped;
            for ((k, msg), n) in shard.errors {
                *self.errors.entry((k, msg)).or_insert(0) += n;
            }
            for (key, e) in shard.entries {
                let is_request = key.kind == Kind::Request;
                let total = self.entries.entry(key.clone()).or_insert_with(Total::new);
                if let Some(h) = &e.hist {
                    let _ = total.hist.add(h);
                    if is_request {
                        let _ = win_hist.add(h);
                        let _ = self.total.hist.add(h);
                    }
                }
                total.ok += e.ok;
                total.fail += e.fail;
                total.bytes_in += e.bytes_in;
                let w = window.entry(key).or_insert((0, 0));
                w.0 += e.ok + e.fail;
                w.1 += e.fail;
                if is_request {
                    self.total.ok += e.ok;
                    self.total.fail += e.fail;
                    self.total.bytes_in += e.bytes_in;
                    win_ok += e.ok;
                    win_fail += e.fail;
                }
            }
        }

        for (key, total) in &mut self.entries {
            let (c, f) = window.get(key).copied().unwrap_or((0, 0));
            total.push_recent(dt, c, f);
        }
        self.total.push_recent(dt, win_ok + win_fail, win_fail);

        let has = !win_hist.is_empty();
        let point = Point {
            t: elapsed,
            users,
            target,
            rps: if dt > 0.0 { (win_ok + win_fail) as f64 / dt } else { 0.0 },
            fail_rps: if dt > 0.0 { win_fail as f64 / dt } else { 0.0 },
            avg_ms: if has { win_hist.mean() / 1000.0 } else { 0.0 },
            p50_ms: if has { ms(win_hist.value_at_quantile(0.50)) } else { 0.0 },
            p95_ms: if has { ms(win_hist.value_at_quantile(0.95)) } else { 0.0 },
            p99_ms: if has { ms(win_hist.value_at_quantile(0.99)) } else { 0.0 },
        };
        if self.timeline.len() == MAX_TIMELINE {
            self.timeline.pop_front();
        }
        self.timeline.push_back(point);
    }

    fn row_of(&self, key: Option<&Key>, t: &Total) -> Row {
        let elapsed = self.elapsed - self.origin;
        let count = t.ok + t.fail;
        let (rps, fail_rps) = t.current_rates();
        let has = !t.hist.is_empty();
        let q = |v: f64| if has { ms(t.hist.value_at_quantile(v)) } else { 0.0 };
        Row {
            kind: key.map(|k| k.kind),
            scenario: key.map(|k| k.scenario.to_string()).unwrap_or_default(),
            method: key.map(|k| k.method.to_string()).unwrap_or_default(),
            name: key.map(|k| k.name.to_string()).unwrap_or_else(|| "Aggregated".to_owned()),
            count,
            failures: t.fail,
            fail_ratio: if count > 0 { t.fail as f64 / count as f64 } else { 0.0 },
            rps,
            fail_rps,
            rps_avg: if elapsed > 0.0 { count as f64 / elapsed } else { 0.0 },
            avg_ms: if has { t.hist.mean() / 1000.0 } else { 0.0 },
            min_ms: if has { ms(t.hist.min()) } else { 0.0 },
            max_ms: if has { ms(t.hist.max()) } else { 0.0 },
            p50_ms: q(0.50),
            p90_ms: q(0.90),
            p95_ms: q(0.95),
            p99_ms: q(0.99),
            p999_ms: q(0.999),
            avg_bytes: if count > 0 { t.bytes_in as f64 / count as f64 } else { 0.0 },
            percentiles: self.percentiles.iter().map(|p| q(p / 100.0)).collect(),
        }
    }

    pub fn total_row(&self) -> Row {
        self.row_of(None, &self.total)
    }

    pub fn rows(&self, kind: Kind) -> Vec<Row> {
        let mut rows: Vec<Row> = self.entries.iter().filter(|(k, _)| k.kind == kind).map(|(k, t)| self.row_of(Some(k), t)).collect();
        rows.sort_by(|a, b| {
            (a.scenario.as_str(), a.name.as_str(), a.method.as_str()).cmp(&(b.scenario.as_str(), b.name.as_str(), b.method.as_str()))
        });
        rows
    }

    pub fn error_rows(&self, limit: usize) -> Vec<ErrorRow> {
        let mut rows: Vec<ErrorRow> = self
            .errors
            .iter()
            .map(|((k, msg), n)| ErrorRow {
                scenario: k.scenario.to_string(),
                method: k.method.to_string(),
                name: k.name.to_string(),
                message: msg.to_string(),
                count: *n,
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.count));
        rows.truncate(limit.min(MAX_ERROR_ROWS));
        rows
    }

    /// Clears counters and histograms while keeping the timeline.
    pub fn reset_counters(&mut self) {
        self.entries.clear();
        self.total = Total::new();
        self.errors.clear();
        self.dropped = 0;
        self.tasks.clear();
        self.origin = self.elapsed;
    }

    /// Latency histogram of all requests in fixed millisecond buckets (empty buckets included).
    pub fn distribution(&self) -> Vec<Bucket> {
        let mut counts = [0_u64; BUCKET_BOUNDS_MS.len() + 1];
        for v in self.total.hist.iter_recorded() {
            let ms = ms(v.value_iterated_to());
            let idx = BUCKET_BOUNDS_MS.iter().position(|b| ms <= *b).unwrap_or(BUCKET_BOUNDS_MS.len());
            counts[idx] += v.count_at_value();
        }
        counts.iter().enumerate().map(|(i, c)| Bucket { upper_ms: BUCKET_BOUNDS_MS.get(i).copied(), count: *c }).collect()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn timeline_tail(&self, n: usize) -> Vec<Point> {
        let skip = self.timeline.len().saturating_sub(n);
        self.timeline.iter().skip(skip).cloned().collect()
    }

    pub fn elapsed(&self) -> f64 {
        self.elapsed
    }
}
