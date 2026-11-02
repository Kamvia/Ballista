// File: ballista/src/listener.rs
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::Plan;
use crate::metrics::Snapshot;

/// One completed request, as seen by listeners.
#[derive(Debug, Clone, Copy)]
pub struct RequestEvent<'a> {
    pub vu: u64,
    pub scenario: &'a str,
    pub method: &'a str,
    pub name: &'a str,
    pub status: Option<u16>,
    pub ok: bool,
    pub latency: Duration,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub error: Option<&'a str>,
    /// Caller-supplied request context (`Req::context`, script `context` map).
    pub context: Option<&'a BTreeMap<String, String>>,
    /// Time to first byte (HTTP only).
    pub ttfb: Option<Duration>,
}

/// Event hooks. `on_request` runs on the hot path and must not block; hand work off to another thread.
pub trait Listener: Send + Sync + 'static {
    fn on_test_start(&self, _plan: &Plan) {}
    fn on_request(&self, _event: &RequestEvent<'_>) {}
    fn on_tick(&self, _snapshot: &Snapshot) {}
    fn on_test_stop(&self, _snapshot: &Snapshot) {}
}
