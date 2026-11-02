// File: ballista/src/metrics/prometheus.rs
use std::fmt::Write;

use super::{RunState, Snapshot};

fn esc(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Renders a snapshot in Prometheus text exposition format 0.0.4.
pub fn render(s: &Snapshot) -> String {
    let mut o = String::with_capacity(4096);
    let running = u8::from(s.state == RunState::Running);
    let _ = writeln!(o, "# TYPE ballista_running gauge\nballista_running {running}");
    let _ = writeln!(o, "# TYPE ballista_users gauge\nballista_users {}", s.users);
    let _ = writeln!(o, "# TYPE ballista_target gauge\nballista_target {}", s.target);
    let _ = writeln!(o, "# TYPE ballista_elapsed_seconds gauge\nballista_elapsed_seconds {}", s.elapsed_secs);
    let _ = writeln!(o, "# TYPE ballista_dropped_iterations_total counter\nballista_dropped_iterations_total {}", s.dropped_iterations);

    let _ = writeln!(o, "# TYPE ballista_requests_total counter");
    for r in &s.rows {
        let _ = writeln!(
            o,
            "ballista_requests_total{{scenario=\"{}\",method=\"{}\",name=\"{}\"}} {}",
            esc(&r.scenario),
            esc(&r.method),
            esc(&r.name),
            r.count
        );
    }
    let _ = writeln!(o, "# TYPE ballista_failures_total counter");
    for r in &s.rows {
        let _ = writeln!(
            o,
            "ballista_failures_total{{scenario=\"{}\",method=\"{}\",name=\"{}\"}} {}",
            esc(&r.scenario),
            esc(&r.method),
            esc(&r.name),
            r.failures
        );
    }
    let _ = writeln!(o, "# TYPE ballista_rps gauge");
    for r in &s.rows {
        let _ = writeln!(
            o,
            "ballista_rps{{scenario=\"{}\",method=\"{}\",name=\"{}\"}} {:.3}",
            esc(&r.scenario),
            esc(&r.method),
            esc(&r.name),
            r.rps
        );
    }
    let _ = writeln!(o, "# TYPE ballista_latency_ms gauge");
    for r in &s.rows {
        for (q, v) in [("0.5", r.p50_ms), ("0.9", r.p90_ms), ("0.95", r.p95_ms), ("0.99", r.p99_ms), ("0.999", r.p999_ms)] {
            let _ = writeln!(
                o,
                "ballista_latency_ms{{scenario=\"{}\",method=\"{}\",name=\"{}\",quantile=\"{q}\"}} {v:.3}",
                esc(&r.scenario),
                esc(&r.method),
                esc(&r.name)
            );
        }
    }
    let t = &s.total;
    let _ = writeln!(o, "# TYPE ballista_total_rps gauge\nballista_total_rps {:.3}", t.rps);
    let _ = writeln!(o, "# TYPE ballista_total_fail_ratio gauge\nballista_total_fail_ratio {:.6}", t.fail_ratio);
    let _ = writeln!(o, "# TYPE ballista_total_p95_ms gauge\nballista_total_p95_ms {:.3}", t.p95_ms);
    let _ = writeln!(o, "# TYPE ballista_total_p99_ms gauge\nballista_total_p99_ms {:.3}", t.p99_ms);
    o
}
