// File: ballista/src/manager/compare.rs
//! Run-to-run comparison: detects latency, error-rate and throughput regressions against a baseline.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const MAX_ROWS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    P50,
    P95,
    P99,
    Avg,
    ErrorRate,
    Rps,
}

impl Metric {
    fn field(self) -> &'static str {
        match self {
            Self::P50 => "p50_ms",
            Self::P95 => "p95_ms",
            Self::P99 => "p99_ms",
            Self::Avg => "avg_ms",
            Self::ErrorRate => "fail_ratio",
            Self::Rps => "rps_avg",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::P50 => "p50",
            Self::P95 => "p95",
            Self::P99 => "p99",
            Self::Avg => "avg",
            Self::ErrorRate => "error_rate",
            Self::Rps => "rps",
        }
    }

    fn is_latency(self) -> bool {
        matches!(self, Self::P50 | Self::P95 | Self::P99 | Self::Avg)
    }
}

fn default_baseline() -> String {
    "auto".to_owned()
}
fn default_pct() -> f64 {
    10.0
}
fn default_metrics() -> Vec<Metric> {
    vec![Metric::P95, Metric::P99, Metric::ErrorRate]
}
fn default_min_samples() -> u64 {
    30
}
fn default_min_change_ms() -> f64 {
    1.0
}
fn default_error_increase() -> f64 {
    0.01
}

/// Regression rule: a run fails when a compared metric is worse than the baseline beyond these limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionGate {
    /// `auto` (latest passed run of the same plan) or a run id.
    #[serde(default = "default_baseline")]
    pub baseline: String,
    /// Allowed latency increase and throughput decrease, in percent.
    #[serde(default = "default_pct")]
    pub max_regression_pct: f64,
    #[serde(default = "default_metrics")]
    pub metrics: Vec<Metric>,
    /// Requests with fewer samples in either run are not compared.
    #[serde(default = "default_min_samples")]
    pub min_samples: u64,
    /// Latency changes smaller than this (milliseconds) never count as regressions.
    #[serde(default = "default_min_change_ms")]
    pub min_change_ms: f64,
    /// Allowed error-rate increase as an absolute fraction (0.01 = one percentage point).
    #[serde(default = "default_error_increase")]
    pub max_error_rate_increase: f64,
}

impl Default for RegressionGate {
    fn default() -> Self {
        Self {
            baseline: default_baseline(),
            max_regression_pct: default_pct(),
            metrics: default_metrics(),
            min_samples: default_min_samples(),
            min_change_ms: default_min_change_ms(),
            max_error_rate_increase: default_error_increase(),
        }
    }
}

impl RegressionGate {
    pub fn validate(&self) -> Result<()> {
        let auto = self.baseline == "auto";
        if !auto && !super::runs::valid_run_id(&self.baseline) {
            return Err(Error::config("baseline must be 'auto' or a run id"));
        }
        let finite = |v: f64| v.is_finite() && v >= 0.0;
        if !finite(self.max_regression_pct) || self.max_regression_pct > 10_000.0 {
            return Err(Error::config("max_regression_pct must be within 0..10000"));
        }
        if !finite(self.min_change_ms) || !finite(self.max_error_rate_increase) || self.max_error_rate_increase > 1.0 {
            return Err(Error::config("min_change_ms and max_error_rate_increase must be non-negative (error rate at most 1)"));
        }
        if self.metrics.is_empty() || self.metrics.len() > 6 {
            return Err(Error::config("metrics must list 1..6 metrics"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricChange {
    pub kind: String,
    pub method: String,
    pub name: String,
    pub metric: Metric,
    pub baseline: f64,
    pub current: f64,
    /// Relative change in percent (positive is an increase).
    pub change_pct: Option<f64>,
    pub regressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    pub baseline: Option<String>,
    pub passed: bool,
    pub regressions: usize,
    pub changes: Vec<MetricChange>,
    /// Requests present in only one of the runs.
    pub unmatched: Vec<String>,
    pub note: Option<String>,
}

impl Comparison {
    pub fn without_baseline(note: &str) -> Self {
        Self { baseline: None, passed: true, regressions: 0, changes: Vec::new(), unmatched: Vec::new(), note: Some(note.to_owned()) }
    }

    /// One-line description of the worst regressions.
    pub fn headline(&self) -> String {
        let worst: Vec<String> = self
            .changes
            .iter()
            .filter(|c| c.regressed)
            .take(3)
            .map(|c| {
                let pct = c.change_pct.map_or_else(String::new, |p| format!(" ({p:+.1}%)"));
                let who = format!("{} {}", c.method, c.name);
                format!("{} {} {:.3} -> {:.3}{pct}", who.trim(), c.metric.label(), c.baseline, c.current)
            })
            .collect();
        format!("{} regression(s) versus {}: {}", self.regressions, self.baseline.as_deref().unwrap_or("baseline"), worst.join("; "))
    }
}

struct Row<'a> {
    kind: &'a str,
    method: &'a str,
    name: &'a str,
    count: u64,
    v: &'a serde_json::Value,
}

fn rows(snapshot: &serde_json::Value) -> Vec<Row<'_>> {
    let mut out = Vec::new();
    for (kind, key) in [("request", "rows"), ("transaction", "transactions")] {
        for v in snapshot.get(key).and_then(serde_json::Value::as_array).into_iter().flatten().take(MAX_ROWS) {
            out.push(Row {
                kind,
                method: v.get("method").and_then(serde_json::Value::as_str).unwrap_or(""),
                name: v.get("name").and_then(serde_json::Value::as_str).unwrap_or(""),
                count: v.get("count").and_then(serde_json::Value::as_u64).unwrap_or(0),
                v,
            });
        }
    }
    if let Some(total) = snapshot.get("total") {
        out.push(Row {
            kind: "total",
            method: "",
            name: "Aggregated",
            count: total.get("count").and_then(serde_json::Value::as_u64).unwrap_or(0),
            v: total,
        });
    }
    out
}

fn value(row: &Row<'_>, m: Metric) -> Option<f64> {
    row.v.get(m.field()).and_then(serde_json::Value::as_f64).filter(|f| f.is_finite())
}

/// Compares two report snapshots under `gate`.
pub fn compare(current: &serde_json::Value, baseline: &serde_json::Value, baseline_id: &str, gate: &RegressionGate) -> Comparison {
    let cur = rows(current);
    let base = rows(baseline);
    let key = |r: &Row<'_>| format!("{}|{}|{}", r.kind, r.method, r.name);
    let mut changes = Vec::new();
    let mut unmatched = Vec::new();
    for c in &cur {
        let Some(b) = base.iter().find(|b| key(b) == key(c)) else {
            unmatched.push(format!("{} {}", c.method, c.name).trim().to_owned());
            continue;
        };
        if c.count < gate.min_samples || b.count < gate.min_samples {
            continue;
        }
        for &m in &gate.metrics {
            let (Some(bv), Some(cv)) = (value(b, m), value(c, m)) else { continue };
            let change_pct = (bv.abs() > f64::EPSILON).then(|| (cv - bv) / bv * 100.0);
            let regressed = match m {
                Metric::ErrorRate => cv - bv > gate.max_error_rate_increase,
                Metric::Rps => change_pct.is_some_and(|p| -p > gate.max_regression_pct),
                _ if m.is_latency() => cv - bv >= gate.min_change_ms && change_pct.is_none_or(|p| p > gate.max_regression_pct),
                _ => false,
            };
            changes.push(MetricChange {
                kind: c.kind.to_owned(),
                method: c.method.to_owned(),
                name: c.name.to_owned(),
                metric: m,
                baseline: bv,
                current: cv,
                change_pct,
                regressed,
            });
        }
    }
    for b in &base {
        if !cur.iter().any(|c| key(c) == key(b)) {
            unmatched.push(format!("{} {}", b.method, b.name).trim().to_owned());
        }
    }
    changes.sort_by(|a, b| b.regressed.cmp(&a.regressed).then(b.change_pct.unwrap_or(0.0).total_cmp(&a.change_pct.unwrap_or(0.0))));
    let regressions = changes.iter().filter(|c| c.regressed).count();
    unmatched.truncate(100);
    Comparison { baseline: Some(baseline_id.to_owned()), passed: regressions == 0, regressions, changes, unmatched, note: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(p95: f64, fail: f64, rps: f64, count: u64) -> serde_json::Value {
        json!({
            "total": { "count": count, "p95_ms": p95, "p99_ms": p95 * 1.5, "fail_ratio": fail, "rps_avg": rps },
            "rows": [{ "method": "GET", "name": "item", "count": count, "p95_ms": p95, "p99_ms": p95 * 1.5, "fail_ratio": fail, "rps_avg": rps }],
            "transactions": []
        })
    }

    #[test]
    fn flags_latency_error_and_throughput_regressions() {
        let gate = RegressionGate { metrics: vec![Metric::P95, Metric::ErrorRate, Metric::Rps], ..RegressionGate::default() };
        let base = snap(100.0, 0.0, 50.0, 1000);
        assert!(compare(&snap(105.0, 0.005, 48.0, 1000), &base, "rb", &gate).passed, "within limits");
        let c = compare(&snap(130.0, 0.0, 50.0, 1000), &base, "rb", &gate);
        assert!(!c.passed && c.regressions == 2, "p95 regressed on row and total: {c:?}");
        assert!(!compare(&snap(100.0, 0.05, 50.0, 1000), &base, "rb", &gate).passed, "error rate");
        assert!(!compare(&snap(100.0, 0.0, 30.0, 1000), &base, "rb", &gate).passed, "throughput");
    }

    #[test]
    fn ignores_small_samples_and_tiny_absolute_changes() {
        let gate = RegressionGate::default();
        assert!(compare(&snap(200.0, 0.0, 1.0, 10), &snap(100.0, 0.0, 1.0, 10), "rb", &gate).passed);
        assert!(compare(&snap(1.5, 0.0, 1.0, 1000), &snap(1.0, 0.0, 1.0, 1000), "rb", &gate).passed, "0.5 ms change");
        assert!(RegressionGate { baseline: "../x".into(), ..RegressionGate::default() }.validate().is_err());
    }
}
