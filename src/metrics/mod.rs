// File: ballista/src/metrics/mod.rs
pub mod export;
pub mod prometheus;
pub mod stats;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use serde::Serialize;

use crate::error::sanitize;

pub use stats::{Bucket, ErrorRow, Point, Ratio, Row, RunState, ScenarioShare, Snapshot, Stats, TaskShare};

pub(crate) const HIST_HIGH_US: u64 = 3_600_000_000;
pub(crate) const HIST_SIGFIG: u8 = 2;
const MAX_ERROR_KEYS_PER_SHARD: usize = 1024;
const MAX_ERROR_CHARS: usize = 240;

pub(crate) fn new_hist() -> Histogram<u64> {
    Histogram::new_with_bounds(1, HIST_HIGH_US, HIST_SIGFIG).expect("static histogram bounds are valid")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Request,
    Iteration,
    Transaction,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub kind: Kind,
    pub scenario: Arc<str>,
    pub method: Arc<str>,
    pub name: Arc<str>,
}

impl Key {
    pub fn request(scenario: &Arc<str>, method: &str, name: &str) -> Self {
        Self { kind: Kind::Request, scenario: Arc::clone(scenario), method: Arc::from(method), name: Arc::from(name) }
    }

    /// Derived timing (e.g. time to first event) kept out of the request totals.
    pub fn derived(scenario: &Arc<str>, label: &str, name: &str) -> Self {
        Self { kind: Kind::Transaction, scenario: Arc::clone(scenario), method: Arc::from(label), name: Arc::from(name) }
    }

    pub fn transaction(scenario: &Arc<str>, name: &str) -> Self {
        Self { kind: Kind::Transaction, scenario: Arc::clone(scenario), method: Arc::from("TXN"), name: Arc::from(name) }
    }

    pub fn iteration(scenario: &Arc<str>) -> Self {
        Self { kind: Kind::Iteration, scenario: Arc::clone(scenario), method: Arc::from("ITER"), name: Arc::clone(scenario) }
    }
}

#[derive(Default)]
pub(crate) struct WindowEntry {
    pub hist: Option<Histogram<u64>>,
    pub ok: u64,
    pub fail: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Default)]
pub(crate) struct Shard {
    pub entries: HashMap<Key, WindowEntry>,
    pub errors: HashMap<(Key, Box<str>), u64>,
    pub dropped: u64,
}

#[repr(align(128))]
struct Padded(Mutex<Shard>);

/// Lock-sharded window recorder. Hot path cost is one uncontended mutex and one histogram increment.
pub struct Recorder {
    shards: Box<[Padded]>,
}

pub struct Sample<'a> {
    pub key: &'a Key,
    pub latency: Duration,
    pub ok: bool,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub error: Option<&'a str>,
}

impl Recorder {
    pub fn new() -> Self {
        let n = std::thread::available_parallelism().map(|n| n.get() * 2).unwrap_or(8).clamp(4, 128);
        let shards = (0..n).map(|_| Padded(Mutex::new(Shard::default()))).collect();
        Self { shards }
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn record(&self, hint: usize, s: Sample<'_>) {
        let us = u64::try_from(s.latency.as_micros()).unwrap_or(u64::MAX).max(1);
        let mut shard = self.shards[hint % self.shards.len()].0.lock();
        let entry = match shard.entries.get_mut(s.key) {
            Some(e) => e,
            None => shard.entries.entry(s.key.clone()).or_default(),
        };
        entry.hist.get_or_insert_with(new_hist).saturating_record(us);
        if s.ok {
            entry.ok += 1;
        } else {
            entry.fail += 1;
        }
        entry.bytes_in += s.bytes_in;
        entry.bytes_out += s.bytes_out;
        if let Some(err) = s.error {
            Self::push_error(&mut shard, s.key, err);
        }
    }

    pub fn record_error(&self, hint: usize, key: &Key, err: &str) {
        let mut shard = self.shards[hint % self.shards.len()].0.lock();
        Self::push_error(&mut shard, key, err);
    }

    pub fn record_dropped(&self, hint: usize, n: u64) {
        self.shards[hint % self.shards.len()].0.lock().dropped += n;
    }

    fn push_error(shard: &mut Shard, key: &Key, err: &str) {
        let msg: Box<str> = sanitize(err, MAX_ERROR_CHARS).into_boxed_str();
        let full = shard.errors.len() >= MAX_ERROR_KEYS_PER_SHARD;
        let slot_key = if full { (key.clone(), Box::from("other errors (distinct error limit reached)")) } else { (key.clone(), msg) };
        *shard.errors.entry(slot_key).or_insert(0) += 1;
    }

    /// Merges an imported shard (e.g. from a remote worker) into the current window.
    pub(crate) fn absorb(&self, hint: usize, other: Shard) {
        let mut shard = self.shards[hint % self.shards.len()].0.lock();
        shard.dropped += other.dropped;
        for (k, e) in other.entries {
            let dst = shard.entries.entry(k).or_default();
            if let Some(h) = e.hist {
                match dst.hist.as_mut() {
                    Some(d) => {
                        let _ = d.add(&h);
                    }
                    None => dst.hist = Some(h),
                }
            }
            dst.ok += e.ok;
            dst.fail += e.fail;
            dst.bytes_in += e.bytes_in;
            dst.bytes_out += e.bytes_out;
        }
        for (k, n) in other.errors {
            if shard.errors.len() < MAX_ERROR_KEYS_PER_SHARD || shard.errors.contains_key(&k) {
                *shard.errors.entry(k).or_insert(0) += n;
            }
        }
    }

    pub(crate) fn drain(&self) -> Vec<Shard> {
        self.shards.iter().map(|p| std::mem::take(&mut *p.0.lock())).collect()
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}
