// File: ballista/src/logbuf.rs
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::error::sanitize;

const CAPACITY: usize = 1000;
const MAX_MESSAGE: usize = 1000;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    pub ts_ms: u128,
    pub level: &'static str,
    pub message: String,
}

#[derive(Default)]
struct Ring {
    seq: u64,
    entries: VecDeque<LogEntry>,
}

static RING: OnceLock<Mutex<Ring>> = OnceLock::new();

fn ring() -> &'static Mutex<Ring> {
    RING.get_or_init(|| Mutex::new(Ring::default()))
}

/// Entries with a sequence number greater than `after`, oldest first, at most `limit`.
pub fn since(after: u64, limit: usize) -> Vec<LogEntry> {
    let r = ring().lock();
    let skip = r.entries.iter().take_while(|e| e.seq <= after).count();
    r.entries.iter().skip(skip).take(limit).cloned().collect()
}

#[derive(Default)]
struct Collect {
    message: String,
    fields: String,
}

impl Visit for Collect {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

/// Keeps the most recent log events in memory for the dashboard.
pub struct RingLayer;

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            Level::INFO => "info",
            Level::DEBUG => "debug",
            Level::TRACE => "trace",
        };
        let mut c = Collect::default();
        event.record(&mut c);
        c.message.push_str(&c.fields);
        let mut r = ring().lock();
        r.seq += 1;
        let entry = LogEntry {
            seq: r.seq,
            ts_ms: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis()),
            level,
            message: sanitize(&c.message, MAX_MESSAGE),
        };
        if r.entries.len() == CAPACITY {
            r.entries.pop_front();
        }
        r.entries.push_back(entry);
    }
}
