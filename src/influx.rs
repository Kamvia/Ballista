// File: ballista/src/influx.rs
use std::fmt::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use crate::config::{InfluxConfig, secret_from_env};
use crate::error::{Error, Result, sanitize};
use crate::listener::Listener;
use crate::metrics::{Row, Snapshot};

const QUEUE: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const FLUSH_WAIT: Duration = Duration::from_secs(5);
const MAX_ROWS: usize = 2000;

enum Msg {
    Write(String),
    Flush(String, oneshot::Sender<()>),
}

/// Writes run and per-request metrics to InfluxDB (v2 write API, also served by v3) as line protocol.
pub struct InfluxListener {
    tx: mpsc::Sender<Msg>,
    interval: Duration,
    last: Mutex<Option<Instant>>,
    plan: String,
    measurement: String,
    handle: tokio::runtime::Handle,
}

/// Escapes a tag key, tag value or field key.
fn tag(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in sanitize(s, 200).chars() {
        if matches!(c, ',' | '=' | ' ' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    if o.is_empty() { "-".to_owned() } else { o }
}

fn num(v: f64) -> String {
    if v.is_finite() { format!("{v}") } else { "0".to_owned() }
}

fn row_line(o: &mut String, measurement: &str, plan: &str, kind: &str, r: &Row, ts: u64) {
    let _ = writeln!(
        o,
        "{measurement}_requests,plan={},kind={kind},scenario={},method={},name={} count={}i,failures={}i,rps={},fail_rps={},avg_ms={},p50_ms={},p90_ms={},p95_ms={},p99_ms={},max_ms={} {ts}",
        tag(plan),
        tag(&r.scenario),
        tag(&r.method),
        tag(&r.name),
        r.count,
        r.failures,
        num(r.rps),
        num(r.fail_rps),
        num(r.avg_ms),
        num(r.p50_ms),
        num(r.p90_ms),
        num(r.p95_ms),
        num(r.p99_ms),
        num(r.max_ms),
    );
}

impl InfluxListener {
    pub fn create(cfg: &InfluxConfig, plan_name: &str) -> Result<Self> {
        let token = secret_from_env(&cfg.token_env)?;
        let handle = tokio::runtime::Handle::try_current().map_err(|_| Error::config("influx output requires the async runtime"))?;
        let mut url = cfg.url.join("api/v2/write").map_err(|e| Error::config(format!("influx url: {e}")))?;
        url.query_pairs_mut().append_pair("org", &cfg.org).append_pair("bucket", &cfg.bucket).append_pair("precision", "s");
        let client = reqwest::Client::builder().timeout(WRITE_TIMEOUT).build().map_err(|e| Error::config(format!("influx client: {e}")))?;
        let auth = format!("Token {token}");
        let (tx, mut rx) = mpsc::channel::<Msg>(QUEUE);
        handle.spawn(async move {
            let mut warned = false;
            while let Some(msg) = rx.recv().await {
                let (body, done) = match msg {
                    Msg::Write(b) => (b, None),
                    Msg::Flush(b, d) => (b, Some(d)),
                };
                let res = client
                    .post(url.clone())
                    .header("authorization", &auth)
                    .header("content-type", "text/plain; charset=utf-8")
                    .body(body)
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => warned = false,
                    Ok(r) if !warned => {
                        tracing::warn!(status = r.status().as_u16(), "influx write rejected");
                        warned = true;
                    }
                    Err(e) if !warned => {
                        tracing::warn!(error = %sanitize(&e.to_string(), 200), "influx write failed");
                        warned = true;
                    }
                    _ => {}
                }
                if let Some(d) = done {
                    let _ = d.send(());
                }
            }
        });
        Ok(Self {
            tx,
            interval: cfg.interval,
            last: Mutex::new(None),
            plan: plan_name.to_owned(),
            measurement: cfg.measurement.clone(),
            handle,
        })
    }

    fn encode(&self, s: &Snapshot) -> String {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let m = &self.measurement;
        let mut o = String::with_capacity(4096);
        let t = &s.total;
        let _ = writeln!(
            o,
            "{m}_run,plan={} users={}i,target={},rps={},fail_rps={},avg_ms={},p50_ms={},p95_ms={},p99_ms={},requests={}i,failures={}i,iterations={}i,dropped={}i,saturated={} {ts}",
            tag(&self.plan),
            s.users,
            num(s.target),
            num(t.rps),
            num(t.fail_rps),
            num(t.avg_ms),
            num(t.p50_ms),
            num(t.p95_ms),
            num(t.p99_ms),
            t.count,
            t.failures,
            s.iterations_started,
            s.dropped_iterations,
            s.generator.saturated,
        );
        for r in s.rows.iter().take(MAX_ROWS) {
            row_line(&mut o, m, &self.plan, "request", r, ts);
        }
        for r in s.transactions.iter().take(MAX_ROWS) {
            row_line(&mut o, m, &self.plan, "transaction", r, ts);
        }
        o
    }
}

impl Listener for InfluxListener {
    fn on_tick(&self, s: &Snapshot) {
        let mut last = self.last.lock();
        if last.is_some_and(|l| l.elapsed() < self.interval) {
            return;
        }
        *last = Some(Instant::now());
        drop(last);
        if self.tx.try_send(Msg::Write(self.encode(s))).is_err() {
            tracing::debug!("influx queue full, dropping a write");
        }
    }

    fn on_test_stop(&self, s: &Snapshot) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.tx.try_send(Msg::Flush(self.encode(s), done_tx)).is_err() {
            return;
        }
        if self.handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            let handle = self.handle.clone();
            tokio::task::block_in_place(move || {
                let _ = handle.block_on(tokio::time::timeout(FLUSH_WAIT, done_rx));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_line_protocol_tags() {
        assert_eq!(tag("a b,c=d\\"), "a\\ b\\,c\\=d\\\\");
        assert_eq!(tag(""), "-");
        assert_eq!(num(f64::NAN), "0");
    }
}
