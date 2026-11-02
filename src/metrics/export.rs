// File: ballista/src/metrics/export.rs
use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hdrhistogram::Histogram;
use hdrhistogram::serialization::{Deserializer, Serializer, V2Serializer};
use serde::{Deserialize, Serialize};

use super::{HIST_HIGH_US, Key, Kind, Shard, WindowEntry, new_hist};
use crate::error::{Error, Result, sanitize};

const MAX_ENTRIES: usize = 10_000;
const MAX_ERRORS: usize = 2_000;
const MAX_HIST_B64: usize = 256 * 1024;
const MAX_LABEL: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEntry {
    pub kind: Kind,
    pub scenario: String,
    pub method: String,
    pub name: String,
    pub hist: Option<String>,
    pub ok: u64,
    pub fail: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportError {
    pub kind: Kind,
    pub scenario: String,
    pub method: String,
    pub name: String,
    pub message: String,
    pub count: u64,
}

/// One stats window, serialized for transfer from a worker to the controller.
/// Live users per scenario.
pub type ScenarioUsers = Vec<(String, u64)>;
/// Task executions as (scenario, task, count).
pub type TaskCounts = Vec<(String, String, u64)>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WindowExport {
    pub entries: Vec<ExportEntry>,
    pub errors: Vec<ExportError>,
    pub dropped: u64,
    /// Active users per scenario at the end of the window.
    #[serde(default)]
    pub scenario_users: ScenarioUsers,
    /// Task executions during the window: (scenario, task, count).
    #[serde(default)]
    pub task_counts: TaskCounts,
}

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match String::deserialize(d)?.as_str() {
            "request" => Ok(Self::Request),
            "iteration" => Ok(Self::Iteration),
            "transaction" => Ok(Self::Transaction),
            other => Err(serde::de::Error::custom(format!("unknown kind '{}'", sanitize(other, 32)))),
        }
    }
}

fn encode(h: &Histogram<u64>) -> Option<String> {
    let mut buf = Vec::new();
    V2Serializer::new().serialize(h, &mut buf).ok()?;
    Some(STANDARD.encode(buf))
}

fn decode(b64: &str) -> Result<Histogram<u64>> {
    if b64.len() > MAX_HIST_B64 {
        return Err(Error::Engine("histogram payload too large".into()));
    }
    let raw = STANDARD.decode(b64).map_err(|_| Error::Engine("invalid histogram encoding".into()))?;
    let h: Histogram<u64> =
        Deserializer::new().deserialize(&mut raw.as_slice()).map_err(|_| Error::Engine("invalid histogram payload".into()))?;
    if h.high() > HIST_HIGH_US || h.max() > HIST_HIGH_US {
        return Err(Error::Engine("histogram range exceeds limits".into()));
    }
    Ok(h)
}

/// Merges drained shards and serializes them.
pub(crate) fn export(shards: &[Shard]) -> WindowExport {
    let mut merged: HashMap<&Key, WindowEntry> = HashMap::new();
    let mut errors: HashMap<(&Key, &str), u64> = HashMap::new();
    let mut dropped = 0;
    for s in shards {
        dropped += s.dropped;
        for ((k, msg), n) in &s.errors {
            *errors.entry((k, msg)).or_insert(0) += n;
        }
        for (k, e) in &s.entries {
            let m = merged.entry(k).or_default();
            if let Some(h) = &e.hist {
                let _ = m.hist.get_or_insert_with(new_hist).add(h);
            }
            m.ok += e.ok;
            m.fail += e.fail;
            m.bytes_in += e.bytes_in;
            m.bytes_out += e.bytes_out;
        }
    }
    WindowExport {
        entries: merged
            .into_iter()
            .take(MAX_ENTRIES)
            .map(|(k, e)| ExportEntry {
                kind: k.kind,
                scenario: k.scenario.to_string(),
                method: k.method.to_string(),
                name: k.name.to_string(),
                hist: e.hist.as_ref().and_then(encode),
                ok: e.ok,
                fail: e.fail,
                bytes_in: e.bytes_in,
                bytes_out: e.bytes_out,
            })
            .collect(),
        errors: errors
            .into_iter()
            .take(MAX_ERRORS)
            .map(|((k, msg), count)| ExportError {
                kind: k.kind,
                scenario: k.scenario.to_string(),
                method: k.method.to_string(),
                name: k.name.to_string(),
                message: msg.to_owned(),
                count,
            })
            .collect(),
        dropped,
        scenario_users: Vec::new(),
        task_counts: Vec::new(),
    }
}

fn label(s: &str) -> Arc<str> {
    Arc::from(sanitize(s, MAX_LABEL).as_str())
}

/// Validates and converts an untrusted export into a shard for the controller's recorder.
pub(crate) fn import(w: WindowExport) -> Result<Shard> {
    if w.entries.len() > MAX_ENTRIES || w.errors.len() > MAX_ERRORS {
        return Err(Error::Engine("report exceeds entry limits".into()));
    }
    let mut shard = Shard { dropped: w.dropped, ..Shard::default() };
    for e in w.entries {
        let key = Key { kind: e.kind, scenario: label(&e.scenario), method: label(&e.method), name: label(&e.name) };
        let hist = e.hist.as_deref().map(decode).transpose()?;
        let entry = shard.entries.entry(key).or_default();
        if let Some(h) = hist {
            entry.hist.get_or_insert_with(new_hist).add(&h).map_err(|_| Error::Engine("histogram merge failed".into()))?;
        }
        entry.ok = entry.ok.saturating_add(e.ok);
        entry.fail = entry.fail.saturating_add(e.fail);
        entry.bytes_in = entry.bytes_in.saturating_add(e.bytes_in);
        entry.bytes_out = entry.bytes_out.saturating_add(e.bytes_out);
    }
    for e in w.errors {
        let key = Key { kind: e.kind, scenario: label(&e.scenario), method: label(&e.method), name: label(&e.name) };
        let msg: Box<str> = sanitize(&e.message, 240).into_boxed_str();
        *shard.errors.entry((key, msg)).or_insert(0) += e.count;
    }
    Ok(shard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Recorder, Sample};
    use std::time::Duration;

    #[test]
    fn export_import_roundtrip_preserves_counts_and_quantiles() {
        let rec = Recorder::new();
        let key = Key::request(&Arc::from("s"), "GET", "/x");
        for i in 1..=1000_u64 {
            rec.record(
                i as usize,
                Sample { key: &key, latency: Duration::from_micros(i * 100), ok: i % 10 != 0, bytes_in: 1, bytes_out: 0, error: None },
            );
        }
        let w = export(&rec.drain());
        let json = serde_json::to_string(&w).expect("json");
        let back: WindowExport = serde_json::from_str(&json).expect("parse");
        let shard = import(back).expect("import");
        let e = shard.entries.get(&key).expect("entry");
        assert_eq!(e.ok + e.fail, 1000);
        assert_eq!(e.fail, 100);
        let h = e.hist.as_ref().expect("hist");
        let p99 = h.value_at_quantile(0.99);
        assert!((98_000..=100_500).contains(&p99), "p99 {p99}");
    }
}
