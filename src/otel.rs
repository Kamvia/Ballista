// File: ballista/src/otel.rs
use std::collections::HashMap;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, MeterProvider as _};
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use parking_lot::Mutex;

use crate::config::{OtlpConfig, secret_from_env};
use crate::error::{Error, Result};
use crate::listener::{Listener, RequestEvent};
use crate::metrics::Snapshot;

const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADERS: usize = 32;

/// Exports request latency, request counts and run gauges over OTLP/HTTP.
pub struct OtlpListener {
    provider: Mutex<Option<SdkMeterProvider>>,
    duration: Histogram<f64>,
    requests: Counter<u64>,
    users: Gauge<u64>,
    rps: Gauge<f64>,
    p95: Gauge<f64>,
    plan: KeyValue,
    role: KeyValue,
}

fn parse_headers(raw: &str) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for pair in raw.split(',').filter(|p| !p.trim().is_empty()) {
        let (k, v) = pair.split_once('=').ok_or_else(|| Error::config("otlp headers must be key=value pairs"))?;
        let (k, v) = (k.trim(), v.trim());
        if http::HeaderName::from_bytes(k.as_bytes()).is_err() || http::HeaderValue::from_str(v).is_err() {
            return Err(Error::config("otlp headers contain an invalid name or value"));
        }
        out.insert(k.to_owned(), v.to_owned());
        if out.len() > MAX_HEADERS {
            return Err(Error::config("too many otlp headers"));
        }
    }
    Ok(out)
}

impl OtlpListener {
    pub fn create(cfg: &OtlpConfig, plan_name: &str, role: &'static str) -> Result<Self> {
        let headers = match &cfg.headers_env {
            Some(env) => parse_headers(&secret_from_env(env)?)?,
            None => HashMap::new(),
        };
        let endpoint = cfg.endpoint.clone();
        let interval = cfg.interval;
        let service = cfg.service_name.clone();
        // The blocking HTTP exporter must not be built on an async worker thread.
        let provider = std::thread::spawn(move || -> Result<SdkMeterProvider> {
            let exporter = MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_timeout(EXPORT_TIMEOUT)
                .with_headers(headers)
                .build()
                .map_err(|e| Error::config(format!("otlp exporter: {e}")))?;
            let reader = PeriodicReader::builder(exporter).with_interval(interval).build();
            let resource = Resource::builder().with_service_name(service).build();
            Ok(SdkMeterProvider::builder().with_reader(reader).with_resource(resource).build())
        })
        .join()
        .map_err(|_| Error::config("otlp exporter initialisation panicked"))??;
        let meter = provider.meter("ballista");
        Ok(Self {
            duration: meter.f64_histogram("ballista.request.duration").with_unit("ms").with_description("Request latency").build(),
            requests: meter.u64_counter("ballista.requests").with_description("Completed requests").build(),
            users: meter.u64_gauge("ballista.users").with_description("Active virtual users").build(),
            rps: meter.f64_gauge("ballista.rps").with_description("Current requests per second").build(),
            p95: meter.f64_gauge("ballista.latency.p95").with_unit("ms").build(),
            plan: KeyValue::new("plan", plan_name.to_owned()),
            role: KeyValue::new("role", role),
            provider: Mutex::new(Some(provider)),
        })
    }
}

impl Listener for OtlpListener {
    fn on_request(&self, e: &RequestEvent<'_>) {
        let attrs = [
            self.plan.clone(),
            self.role.clone(),
            KeyValue::new("scenario", e.scenario.to_owned()),
            KeyValue::new("method", e.method.to_owned()),
            KeyValue::new("name", e.name.to_owned()),
            KeyValue::new("ok", e.ok),
        ];
        self.duration.record(e.latency.as_secs_f64() * 1000.0, &attrs);
        self.requests.add(1, &attrs);
    }

    fn on_tick(&self, s: &Snapshot) {
        let attrs = [self.plan.clone(), self.role.clone()];
        self.users.record(s.users, &attrs);
        self.rps.record(s.total.rps, &attrs);
        self.p95.record(s.total.p95_ms, &attrs);
    }

    fn on_test_stop(&self, _s: &Snapshot) {
        let Some(provider) = self.provider.lock().take() else {
            return;
        };
        let shutdown = move || {
            if let Err(e) = provider.shutdown() {
                tracing::warn!(error = %e, "otlp flush failed");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::block_in_place(shutdown),
            Err(_) => shutdown(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_headers;

    #[test]
    fn parses_headers() {
        let h = parse_headers("authorization=Bearer x, x-team=load").expect("headers");
        assert_eq!(h.len(), 2);
        assert!(parse_headers("bad header=1").is_err());
    }
}
