// File: ballista/src/threshold.rs
use serde::Serialize;

use crate::config::{ThresholdConfig, ThresholdMetric};
use crate::metrics::{Row, Snapshot};

#[derive(Debug, Clone, Serialize)]
pub struct ThresholdResult {
    pub metric: ThresholdMetric,
    pub request: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub actual: Option<f64>,
    pub passed: bool,
    pub abort_on_fail: bool,
}

fn metric_value(m: ThresholdMetric, r: &Row, dropped: u64) -> f64 {
    match m {
        ThresholdMetric::P50 => r.p50_ms,
        ThresholdMetric::P90 => r.p90_ms,
        ThresholdMetric::P95 => r.p95_ms,
        ThresholdMetric::P99 => r.p99_ms,
        ThresholdMetric::Avg => r.avg_ms,
        ThresholdMetric::Max => r.max_ms,
        ThresholdMetric::ErrorRate => r.fail_ratio,
        ThresholdMetric::Rps => r.rps_avg,
        ThresholdMetric::DroppedIterations => dropped as f64,
    }
}

fn selector_matches(sel: &str, r: &Row) -> bool {
    if sel == r.name {
        return true;
    }
    if let Some((method, name)) = sel.split_once(' ')
        && method.eq_ignore_ascii_case(&r.method)
        && name == r.name
    {
        return true;
    }
    if let Some((scenario, name)) = sel.split_once('/')
        && scenario == r.scenario
        && (name == r.name || name == "*")
    {
        return true;
    }
    false
}

/// Evaluates every threshold. When `final_pass` is false, missing data is not treated as failure.
pub fn evaluate(cfgs: &[ThresholdConfig], snap: &Snapshot, final_pass: bool) -> Vec<ThresholdResult> {
    cfgs.iter()
        .map(|c| {
            let rows: Vec<&Row> = if c.request == "*" || c.metric == ThresholdMetric::DroppedIterations {
                vec![&snap.total]
            } else {
                snap.rows.iter().chain(snap.transactions.iter()).filter(|r| selector_matches(&c.request, r)).collect()
            };
            let has_data = rows.iter().any(|r| r.count > 0) || c.metric == ThresholdMetric::DroppedIterations;
            let values: Vec<f64> = rows.iter().map(|r| metric_value(c.metric, r, snap.dropped_iterations)).collect();
            let worst_high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let worst_low = values.iter().copied().fold(f64::INFINITY, f64::min);
            let mut passed = true;
            if has_data && !values.is_empty() {
                if let Some(max) = c.max
                    && worst_high > max
                {
                    passed = false;
                }
                if let Some(min) = c.min
                    && worst_low < min
                {
                    passed = false;
                }
            } else if final_pass {
                passed = false;
            }
            let actual = if values.is_empty() || !has_data {
                None
            } else if c.max.is_some() {
                Some(worst_high)
            } else {
                Some(worst_low)
            };
            ThresholdResult {
                metric: c.metric,
                request: c.request.clone(),
                min: c.min,
                max: c.max,
                actual,
                passed,
                abort_on_fail: c.abort_on_fail,
            }
        })
        .collect()
}

pub fn all_passed(results: &[ThresholdResult]) -> bool {
    results.iter().all(|r| r.passed)
}
