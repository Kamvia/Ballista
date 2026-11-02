// File: ballista/src/console.rs
use std::fmt::Write;
use std::time::Duration;

use crate::engine::Engine;
use crate::metrics::{Row, RunState, Snapshot};
use crate::threshold::ThresholdResult;

const NAME_WIDTH: usize = 40;

fn clip(s: &str, w: usize) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    if s.chars().count() <= w {
        s
    } else {
        let mut out: String = s.chars().take(w.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn line(o: &mut String, r: &Row) {
    let label = if r.method.is_empty() { r.name.clone() } else { format!("{} {}", r.method, r.name) };
    let _ = writeln!(
        o,
        "{:<w$} {:>10} {:>8} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>9.1}",
        clip(&label, NAME_WIDTH),
        r.count,
        r.failures,
        r.rps,
        r.avg_ms,
        r.p50_ms,
        r.p95_ms,
        r.p99_ms,
        r.max_ms,
        w = NAME_WIDTH
    );
}

pub fn render_table(s: &Snapshot) -> String {
    let mut o = String::with_capacity(4096);
    let g = &s.generator;
    let cpu = g.cpu_pct.map_or_else(|| "n/a".to_owned(), |c| format!("{c:.0}%"));
    let _ = writeln!(
        o,
        "[{:?}] elapsed={:.0}s users={} target={:.1} iterations={} dropped={} generator_cpu={} lag={:.1}ms{}",
        s.state,
        s.elapsed_secs,
        s.users,
        s.target,
        s.iterations_started,
        s.dropped_iterations,
        cpu,
        g.scheduler_lag_ms,
        if g.saturated { " SATURATED" } else { "" }
    );
    let _ = writeln!(
        o,
        "{:<w$} {:>10} {:>8} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "Request",
        "Count",
        "Fails",
        "Req/s",
        "Avg ms",
        "p50",
        "p95",
        "p99",
        "Max",
        w = NAME_WIDTH
    );
    o.push_str(&"-".repeat(NAME_WIDTH + 83));
    o.push('\n');
    for r in &s.rows {
        line(&mut o, r);
    }
    o.push_str(&"-".repeat(NAME_WIDTH + 83));
    o.push('\n');
    line(&mut o, &s.total);
    if !s.transactions.is_empty() {
        o.push_str("Transactions\n");
        for r in &s.transactions {
            line(&mut o, r);
        }
    }
    o
}

pub fn render_thresholds(results: &[ThresholdResult]) -> String {
    let mut o = String::new();
    for t in results {
        let actual = t.actual.map_or_else(|| "no data".to_owned(), |v| format!("{v:.3}"));
        let _ = writeln!(
            o,
            "threshold {:?} on '{}' min={:?} max={:?} actual={} -> {}",
            t.metric,
            clip(&t.request, 60),
            t.min,
            t.max,
            actual,
            if t.passed { "PASS" } else { "FAIL" }
        );
    }
    o
}

pub fn render_errors(s: &Snapshot, limit: usize) -> String {
    let mut o = String::new();
    for e in s.errors.iter().take(limit) {
        let _ = writeln!(o, "{:>8}  {} {}  {}", e.count, e.method, clip(&e.name, 40), clip(&e.message, 120));
    }
    o
}

/// Prints a stats table to stdout every `every` until the run stops.
pub async fn report_loop(engine: Engine, every: Duration) {
    let mut rx = engine.subscribe();
    let mut tick = tokio::time::interval(every);
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let snap = rx.borrow_and_update().clone();
                if snap.state == RunState::Running || snap.state == RunState::Stopping {
                    println!("{}", render_table(&snap));
                }
            }
            changed = rx.changed() => {
                if changed.is_err() || rx.borrow().state == RunState::Stopped {
                    return;
                }
            }
        }
    }
}
