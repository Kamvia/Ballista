// File: ballista/src/csv.rs
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::error::Result;
use crate::listener::{Listener, RequestEvent};
use crate::metrics::{Row, Snapshot};

const LOG_QUEUE: usize = 65_536;

/// Quotes a field per RFC 4180 and neutralises spreadsheet formula injection (OWASP CSV injection).
pub fn field(s: &str) -> String {
    let mut v: String = s.chars().map(|c| if c.is_control() && c != '\t' { ' ' } else { c }).collect();
    if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        v.insert(0, '\'');
    }
    if v.contains([',', '"', '\n', '\r']) || v.starts_with(' ') || v.ends_with(' ') { format!("\"{}\"", v.replace('"', "\"\"")) } else { v }
}

fn row_line(r: &Row) -> String {
    let extra: String = r.percentiles.iter().map(|v| format!(",{v:.3}")).collect();
    format!(
        "{},{},{},{},{},{:.6},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.1}{extra}\n",
        field(&r.scenario),
        field(&r.method),
        field(&r.name),
        r.count,
        r.failures,
        r.fail_ratio,
        r.rps_avg,
        r.rps,
        r.avg_ms,
        r.min_ms,
        r.max_ms,
        r.p50_ms,
        r.p90_ms,
        r.p95_ms,
        r.p99_ms,
        r.p999_ms,
        r.fail_rps,
        r.avg_bytes
    )
}

const STATS_HEADER: &str = "scenario,method,name,count,failures,fail_ratio,rps_avg,rps_current,avg_ms,min_ms,max_ms,p50_ms,p90_ms,p95_ms,p99_ms,p999_ms,fail_rps,avg_bytes\n";

pub fn stats_csv(s: &Snapshot) -> String {
    let mut o = String::from(STATS_HEADER.trim_end());
    for p in &s.percentiles {
        o.push_str(&format!(",p{p}_ms"));
    }
    o.push('\n');
    for r in s.rows.iter().chain(std::iter::once(&s.total)).chain(&s.transactions).chain(&s.iterations) {
        o.push_str(&row_line(r));
    }
    o
}

pub fn history_csv(s: &Snapshot) -> String {
    let mut o = String::from("elapsed_secs,users,target,rps,fail_rps,avg_ms,p50_ms,p95_ms,p99_ms\n");
    for p in &s.timeline {
        o.push_str(&format!(
            "{:.3},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}\n",
            p.t, p.users, p.target, p.rps, p.fail_rps, p.avg_ms, p.p50_ms, p.p95_ms, p.p99_ms
        ));
    }
    o
}

pub fn failures_csv(s: &Snapshot) -> String {
    let mut o = String::from("count,scenario,method,name,message\n");
    for e in &s.errors {
        o.push_str(&format!("{},{},{},{},{}\n", e.count, field(&e.scenario), field(&e.method), field(&e.name), field(&e.message)));
    }
    o
}

pub fn csv_paths(prefix: &Path) -> [(PathBuf, &'static str); 3] {
    let base = prefix.to_string_lossy();
    [
        (PathBuf::from(format!("{base}_stats.csv")), "stats"),
        (PathBuf::from(format!("{base}_history.csv")), "history"),
        (PathBuf::from(format!("{base}_failures.csv")), "failures"),
    ]
}

struct LogRecord {
    ts_ms: u128,
    vu: u64,
    scenario: String,
    method: String,
    name: String,
    status: Option<u16>,
    ok: bool,
    latency_us: u128,
    bytes_in: u64,
    bytes_out: u64,
    error: Option<String>,
    ttfb_us: Option<u128>,
    context: Option<String>,
}

/// Output layout of a per-request log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Native columns, including request context as JSON.
    Native,
    /// JMeter JTL (CSV) so existing JMeter tooling can read the results.
    Jtl,
}

fn native_line(r: &LogRecord) -> String {
    format!(
        "{},{},{},{},{},{},{},{:.3},{},{},{},{}\n",
        r.ts_ms,
        r.vu,
        field(&r.scenario),
        field(&r.method),
        field(&r.name),
        r.status.map(|s| s.to_string()).unwrap_or_default(),
        u8::from(r.ok),
        r.latency_us as f64 / 1000.0,
        r.bytes_in,
        r.bytes_out,
        field(r.error.as_deref().unwrap_or("")),
        field(r.context.as_deref().unwrap_or(""))
    )
}

fn jtl_line(r: &LogRecord) -> String {
    let code = r.status.map_or_else(|| "Non HTTP response code".to_owned(), |s| s.to_string());
    let message = r.error.as_deref().unwrap_or(if r.ok { "OK" } else { "" });
    let label = format!("{} {}", r.method, r.name);
    format!(
        "{},{},{},{},{},{},text,{},{},{},{},1,1,,{},0,0\n",
        r.ts_ms,
        r.latency_us / 1000,
        field(&label),
        field(&code),
        field(message),
        field(&format!("{} {}", r.scenario, r.vu)),
        r.ok,
        field(if r.ok { "" } else { message }),
        r.bytes_in,
        r.bytes_out,
        r.ttfb_us.map_or(r.latency_us, |t| t) / 1000
    )
}

/// Per-request CSV log written by a background thread. Records are dropped (and counted) if the writer falls behind.
pub struct CsvRequestLog {
    tx: Mutex<Option<SyncSender<LogRecord>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    dropped: AtomicU64,
}

impl CsvRequestLog {
    pub fn create(path: &Path) -> Result<Self> {
        Self::create_with(path, LogFormat::Native)
    }

    pub fn create_with(path: &Path, format: LogFormat) -> Result<Self> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let file = File::create(path)?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        let header: &[u8] = match format {
            LogFormat::Native => b"timestamp_ms,vu,scenario,method,name,status,ok,latency_ms,bytes_in,bytes_out,error,context\n",
            LogFormat::Jtl => b"timeStamp,elapsed,label,responseCode,responseMessage,threadName,dataType,success,failureMessage,bytes,sentBytes,grpThreads,allThreads,URL,Latency,IdleTime,Connect\n",
        };
        w.write_all(header)?;
        let (tx, rx) = sync_channel::<LogRecord>(LOG_QUEUE);
        let worker = std::thread::Builder::new().name("request-log".into()).spawn(move || {
            for r in rx {
                let line = match format {
                    LogFormat::Native => native_line(&r),
                    LogFormat::Jtl => jtl_line(&r),
                };
                if w.write_all(line.as_bytes()).is_err() {
                    tracing::error!("request log write failed, stopping log");
                    return;
                }
            }
            let _ = w.flush();
        })?;
        Ok(Self { tx: Mutex::new(Some(tx)), worker: Mutex::new(Some(worker)), dropped: AtomicU64::new(0) })
    }
}

impl Listener for CsvRequestLog {
    fn on_request(&self, e: &RequestEvent<'_>) {
        let guard = self.tx.lock();
        let Some(tx) = guard.as_ref() else {
            return;
        };
        let rec = LogRecord {
            ts_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
            vu: e.vu,
            scenario: e.scenario.to_owned(),
            method: e.method.to_owned(),
            name: e.name.to_owned(),
            status: e.status,
            ok: e.ok,
            latency_us: e.latency.as_micros(),
            bytes_in: e.bytes_in,
            bytes_out: e.bytes_out,
            error: e.error.map(str::to_owned),
            ttfb_us: e.ttfb.map(|t| t.as_micros()),
            context: e.context.and_then(|c| serde_json::to_string(c).ok()),
        };
        if let Err(TrySendError::Full(_)) = tx.try_send(rec) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn on_test_stop(&self, _snapshot: &Snapshot) {
        drop(self.tx.lock().take());
        if let Some(w) = self.worker.lock().take() {
            let _ = w.join();
        }
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            tracing::warn!(dropped, "request log could not keep up; records were dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::field;

    #[test]
    fn escapes_and_guards() {
        assert_eq!(field("plain"), "plain");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(field("-1+2"), "'-1+2");
    }
}
