// File: ballista/src/report/html.rs
use std::fmt::Write;

use super::Report;
use crate::metrics::{Bucket, Point, Row};

const MAX_CHART_POINTS: usize = 1200;

type Series<'a> = (&'a str, &'a str, fn(&Point) -> f64);

pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c if c.is_control() => {}
            c => o.push(c),
        }
    }
    o
}

fn downsample(points: &[Point]) -> Vec<&Point> {
    if points.len() <= MAX_CHART_POINTS {
        return points.iter().collect();
    }
    let step = points.len() as f64 / MAX_CHART_POINTS as f64;
    (0..MAX_CHART_POINTS).map(|i| &points[(i as f64 * step) as usize]).collect()
}

fn chart(title: &str, pts: &[&Point], series: &[Series<'_>]) -> String {
    let (w, h, pad) = (960.0_f64, 260.0_f64, 40.0_f64);
    let tmax = pts.last().map_or(1.0, |p| p.t.max(1.0));
    let vmax = pts.iter().flat_map(|p| series.iter().map(move |(_, _, f)| f(p))).fold(0.0_f64, f64::max).max(1.0) * 1.1;
    let prec = if vmax < 5.0 {
        2
    } else if vmax < 50.0 {
        1
    } else {
        0
    };
    let x = |t: f64| pad + (t / tmax) * (w - pad * 2.0);
    let y = |v: f64| h - pad - (v / vmax) * (h - pad * 2.0);
    let mut s = String::new();
    let _ = write!(
        s,
        "<figure><figcaption>{}</figcaption><svg viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{}\">",
        esc(title),
        esc(title)
    );
    for i in 0..=4 {
        let v = vmax * f64::from(i) / 4.0;
        let yy = y(v);
        let _ = write!(
            s,
            "<line x1=\"{pad}\" x2=\"{}\" y1=\"{yy:.1}\" y2=\"{yy:.1}\" class=\"grid\"/><text x=\"4\" y=\"{:.1}\" class=\"lbl\">{v:.prec$}</text>",
            w - pad,
            yy + 4.0
        );
    }
    let _ = write!(s, "<text x=\"{}\" y=\"{}\" class=\"lbl\" text-anchor=\"end\">{tmax:.0}s</text>", w - pad, h - 12.0);
    for (i, (name, color, f)) in series.iter().enumerate() {
        let path: Vec<String> = pts.iter().map(|p| format!("{:.1},{:.1}", x(p.t), y(f(p)))).collect();
        let _ = write!(s, "<polyline fill=\"none\" stroke=\"{color}\" stroke-width=\"1.6\" points=\"{}\"/>", path.join(" "));
        let lx = pad + 10.0 + i as f64 * 120.0;
        let _ = write!(
            s,
            "<rect x=\"{lx}\" y=\"8\" width=\"10\" height=\"10\" fill=\"{color}\"/><text x=\"{}\" y=\"17\" class=\"lbl\">{}</text>",
            lx + 14.0,
            esc(name)
        );
    }
    s.push_str("</svg></figure>");
    s
}

fn error_pct(p: &Point) -> f64 {
    if p.rps > 0.0 { p.fail_rps / p.rps * 100.0 } else { 0.0 }
}

/// Vertical bars with category labels below each bar.
fn bars(title: &str, labels: &[String], values: &[f64], color: &str, unit: &str) -> String {
    let (w, h, pad, top) = (960.0_f64, 260.0_f64, 40.0_f64, 24.0_f64);
    let vmax = values.iter().copied().fold(0.0_f64, f64::max).max(1.0) * 1.1;
    let n = values.len().max(1) as f64;
    let slot = (w - pad * 2.0) / n;
    let bw = (slot * 0.7).max(2.0);
    let mut s = String::new();
    let _ = write!(
        s,
        "<figure><figcaption>{}</figcaption><svg viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{}\">",
        esc(title),
        esc(title)
    );
    let base = h - pad;
    let _ = write!(s, "<line x1=\"{pad}\" x2=\"{}\" y1=\"{base}\" y2=\"{base}\" class=\"grid\"/>", w - pad);
    for (i, (label, v)) in labels.iter().zip(values).enumerate() {
        let x = pad + i as f64 * slot + (slot - bw) / 2.0;
        let bh = (v / vmax) * (h - pad - top);
        let _ = write!(
            s,
            "<rect x=\"{x:.1}\" y=\"{:.1}\" width=\"{bw:.1}\" height=\"{bh:.1}\" fill=\"{color}\"><title>{}: {v:.1}{unit}</title></rect>",
            base - bh,
            esc(label)
        );
        if *v > 0.0 {
            let _ = write!(
                s,
                "<text x=\"{:.1}\" y=\"{:.1}\" class=\"lbl\" text-anchor=\"middle\">{}</text>",
                x + bw / 2.0,
                base - bh - 4.0,
                compact(*v)
            );
        }
        let _ = write!(
            s,
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"lbl\" text-anchor=\"middle\">{}</text>",
            x + bw / 2.0,
            base + 14.0,
            esc(label)
        );
    }
    s.push_str("</svg></figure>");
    s
}

fn compact(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.0}k", v / 1000.0)
    } else if v >= 100.0 || v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn bucket_label(b: &Bucket, prev: Option<f64>) -> String {
    let ms = |v: f64| if v >= 1000.0 { format!("{}s", v / 1000.0) } else { format!("{v}ms") };
    match (b.upper_ms, prev) {
        (Some(u), _) => format!("<={}", ms(u)),
        (None, Some(p)) => format!(">{}", ms(p)),
        (None, None) => "all".to_owned(),
    }
}

/// Latency histogram trimmed to the populated range.
fn distribution(buckets: &[Bucket]) -> Option<String> {
    let first = buckets.iter().position(|b| b.count > 0)?;
    let last = buckets.iter().rposition(|b| b.count > 0)?;
    let total: u64 = buckets.iter().map(|b| b.count).sum();
    let mut labels = Vec::new();
    let mut values = Vec::new();
    for i in first..=last {
        let prev = i.checked_sub(1).and_then(|p| buckets[p].upper_ms);
        labels.push(bucket_label(&buckets[i], prev));
        values.push(buckets[i].count as f64 / total as f64 * 100.0);
    }
    Some(bars("Response time distribution (% of requests)", &labels, &values, "#2563eb", "%"))
}

/// Slowest requests by p95.
fn slowest(rows: &[Row]) -> Option<String> {
    let mut v: Vec<&Row> = rows.iter().filter(|r| r.count > 0).collect();
    if v.len() < 2 {
        return None;
    }
    v.sort_by(|a, b| b.p95_ms.total_cmp(&a.p95_ms));
    v.truncate(12);
    let labels: Vec<String> = v.iter().map(|r| crate::error::sanitize(&format!("{} {}", r.method, r.name), 13)).collect();
    let values: Vec<f64> = v.iter().map(|r| r.p95_ms).collect();
    Some(bars("Slowest requests (p95 ms)", &labels, &values, "#d97706", " ms"))
}

fn row_html(r: &Row) -> String {
    let extra: String = r.percentiles.iter().map(|v| format!("<td>{v:.1}</td>")).collect();
    format!(
        "<tr><td>{}</td><td>{}</td><td class=\"l\">{}</td><td>{}</td><td>{}</td><td>{:.2}%</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.0}</td>{extra}</tr>",
        esc(&r.scenario),
        esc(&r.method),
        esc(&r.name),
        r.count,
        r.failures,
        r.fail_ratio * 100.0,
        r.rps_avg,
        r.avg_ms,
        r.min_ms,
        r.p50_ms,
        r.p90_ms,
        r.p95_ms,
        r.p99_ms,
        r.max_ms,
        r.avg_bytes
    )
}

const HEAD_BASE: &str = "<tr><th>Scenario</th><th>Method</th><th class=\"l\">Name</th><th>Count</th><th>Fail</th><th>Fail %</th><th>Per s</th><th>Avg ms</th><th>Min</th><th>p50</th><th>p90</th><th>p95</th><th>p99</th><th>Max</th><th>Avg bytes</th>";

fn head(percentiles: &[f64]) -> String {
    let extra: String = percentiles.iter().map(|p| format!("<th>p{p}</th>")).collect();
    format!("{HEAD_BASE}{extra}</tr>")
}

const CSS: &str = "body{font:14px/1.45 system-ui,-apple-system,Segoe UI,sans-serif;margin:0;background:#f6f7f9;color:#1c2330}main{max-width:1180px;margin:0 auto;padding:24px 16px}h1{font-size:22px;margin:0 0 4px}h2{font-size:16px;margin:28px 0 8px}.meta{color:#5b6475}.badge{display:inline-block;padding:2px 10px;border-radius:999px;font-weight:600;font-size:12px}.pass{background:#dcf5e6;color:#11652f}.fail{background:#fde2e1;color:#9b1c1c}.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px;margin-top:16px}.card{background:#fff;border:1px solid #e3e6eb;border-radius:8px;padding:12px}.card b{display:block;font-size:20px}.card span{color:#5b6475;font-size:12px}.wrap{overflow-x:auto;background:#fff;border:1px solid #e3e6eb;border-radius:8px}table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}th,td{padding:6px 10px;border-bottom:1px solid #eef0f3;text-align:right;white-space:nowrap}th{background:#fafbfc;font-weight:600}.l{text-align:left}tr.total td{font-weight:600}figure{margin:12px 0;background:#fff;border:1px solid #e3e6eb;border-radius:8px;padding:8px}figcaption{font-weight:600;padding:4px 8px}svg{width:100%;height:auto}.grid{stroke:#e6e9ee}.lbl{font-size:11px;fill:#5b6475}.warn{background:#fff4e5;border:1px solid #f0b46c;color:#7a4100;padding:8px 12px;border-radius:8px}@media (prefers-color-scheme:dark){body{background:#12151b;color:#e3e7ee}.card,.wrap,figure{background:#1a1f27;border-color:#2a313c}th{background:#1f252e}th,td{border-color:#262d37}.grid{stroke:#2a313c}.meta,.card span{color:#9aa3b2}.lbl{fill:#9aa3b2}}";

pub(crate) fn render(r: &Report) -> String {
    let s = &r.snapshot;
    let pts = downsample(&s.timeline);
    let mut o = String::with_capacity(64 * 1024);
    let _ = write!(
        o,
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src data:\"><title>Load Test Report</title><style>{CSS}</style></head><body><main>"
    );
    let badge = if r.passed { "<span class=\"badge pass\">PASSED</span>" } else { "<span class=\"badge fail\">FAILED</span>" };
    let _ = write!(
        o,
        "<h1>{} {badge}</h1><div class=\"meta\">Target {} &middot; mode {:?} &middot; started {} &middot; ended {} &middot; {:.0}s{}</div>",
        esc(&r.plan),
        esc(&r.target),
        r.mode,
        esc(r.started_at.as_deref().unwrap_or("-")),
        esc(r.ended_at.as_deref().unwrap_or("-")),
        r.duration_secs,
        if r.aborted { " &middot; aborted by threshold" } else { "" }
    );
    if r.generator_saturated_secs > 0 {
        let _ = write!(
            o,
            "<p class=\"warn\">Load generator was saturated for {}s; latency figures in those periods include generator delay.</p>",
            r.generator_saturated_secs
        );
    }
    let t = &s.total;
    let _ = write!(
        o,
        "<div class=\"cards\"><div class=\"card\"><b>{}</b><span>requests</span></div><div class=\"card\"><b>{:.2}%</b><span>failure rate</span></div><div class=\"card\"><b>{:.1}</b><span>avg req/s</span></div><div class=\"card\"><b>{:.1} ms</b><span>p50</span></div><div class=\"card\"><b>{:.1} ms</b><span>p95</span></div><div class=\"card\"><b>{:.1} ms</b><span>p99</span></div><div class=\"card\"><b>{}</b><span>dropped iterations</span></div></div>",
        t.count,
        t.fail_ratio * 100.0,
        t.rps_avg,
        t.p50_ms,
        t.p95_ms,
        t.p99_ms,
        s.dropped_iterations
    );

    if !s.thresholds.is_empty() {
        o.push_str("<h2>Thresholds</h2><div class=\"wrap\"><table><tr><th class=\"l\">Metric</th><th class=\"l\">Request</th><th>Min</th><th>Max</th><th>Actual</th><th>Result</th></tr>");
        for th in &s.thresholds {
            let fmt = |v: Option<f64>| v.map_or_else(|| "-".to_owned(), |v| format!("{v:.3}"));
            let _ = write!(
                o,
                "<tr><td class=\"l\">{:?}</td><td class=\"l\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                th.metric,
                esc(&th.request),
                fmt(th.min),
                fmt(th.max),
                fmt(th.actual),
                if th.passed { "<span class=\"badge pass\">pass</span>" } else { "<span class=\"badge fail\">fail</span>" }
            );
        }
        o.push_str("</table></div>");
    }

    if !pts.is_empty() {
        o.push_str("<h2>Timeline</h2>");
        o.push_str(&chart("Throughput", &pts, &[("req/s", "#2563eb", |p| p.rps), ("fail/s", "#dc2626", |p| p.fail_rps)]));
        o.push_str(&chart(
            "Latency (ms)",
            &pts,
            &[("p50", "#16a34a", |p| p.p50_ms), ("p95", "#d97706", |p| p.p95_ms), ("p99", "#9333ea", |p| p.p99_ms)],
        ));
        o.push_str(&chart("Error rate (%)", &pts, &[("errors", "#dc2626", error_pct)]));
        o.push_str(&chart("Load", &pts, &[("active users", "#0891b2", |p| p.users as f64), ("target", "#64748b", |p| p.target)]));
    }
    if let Some(d) = distribution(&s.distribution) {
        o.push_str("<h2>Latency</h2>");
        o.push_str(&d);
        if let Some(sl) = slowest(&s.rows) {
            o.push_str(&sl);
        }
    }

    let head = head(&s.percentiles);
    o.push_str("<h2>Requests</h2><div class=\"wrap\"><table>");
    o.push_str(&head);
    for row in &s.rows {
        o.push_str(&row_html(row));
    }
    o.push_str(&row_html(t).replacen("<tr>", "<tr class=\"total\">", 1));
    o.push_str("</table></div>");

    if !s.transactions.is_empty() {
        o.push_str("<h2>Transactions</h2><div class=\"wrap\"><table>");
        o.push_str(&head);
        for row in &s.transactions {
            o.push_str(&row_html(row));
        }
        o.push_str("</table></div>");
    }

    if !s.iterations.is_empty() {
        o.push_str("<h2>Iterations</h2><div class=\"wrap\"><table>");
        o.push_str(&head);
        for row in &s.iterations {
            o.push_str(&row_html(row));
        }
        o.push_str("</table></div>");
    }

    if !s.ratio.scenarios.is_empty() {
        o.push_str("<h2>Scenario and task mix</h2><div class=\"wrap\"><table><tr><th class=\"l\">Scenario</th><th>Configured</th><th>Users at end</th><th>Iterations</th><th>Iteration share</th></tr>");
        for r in &s.ratio.scenarios {
            let _ = write!(
                o,
                "<tr><td class=\"l\">{}</td><td>{:.1}%</td><td>{}</td><td>{}</td><td>{:.1}%</td></tr>",
                esc(&r.name),
                r.weight_share * 100.0,
                r.users,
                r.iterations,
                r.iteration_share * 100.0
            );
        }
        o.push_str("</table></div>");
        if !s.ratio.tasks.is_empty() {
            o.push_str("<div class=\"wrap\" style=\"margin-top:10px\"><table><tr><th class=\"l\">Scenario</th><th class=\"l\">Task</th><th>Executions</th><th>Share in scenario</th></tr>");
            for t in &s.ratio.tasks {
                let _ = write!(
                    o,
                    "<tr><td class=\"l\">{}</td><td class=\"l\">{}</td><td>{}</td><td>{:.1}%</td></tr>",
                    esc(&t.scenario),
                    esc(&t.task),
                    t.count,
                    t.share * 100.0
                );
            }
            o.push_str("</table></div>");
        }
    }

    if !s.errors.is_empty() {
        o.push_str("<h2>Errors</h2><div class=\"wrap\"><table><tr><th>Count</th><th class=\"l\">Scenario</th><th class=\"l\">Request</th><th class=\"l\">Message</th></tr>");
        for e in &s.errors {
            let _ = write!(
                o,
                "<tr><td>{}</td><td class=\"l\">{}</td><td class=\"l\">{} {}</td><td class=\"l\">{}</td></tr>",
                e.count,
                esc(&e.scenario),
                esc(&e.method),
                esc(&e.name),
                esc(&e.message)
            );
        }
        o.push_str("</table></div>");
    }
    let _ = write!(o, "<p class=\"meta\">Generated by ballista {}</p></main></body></html>", esc(r.version));
    o
}
