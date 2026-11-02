// File: ballista/src/web/security.rs
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;

use super::AppState;
use super::api::ApiError;

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";
const MAX_TRACKED_IPS: usize = 50_000;

/// Token bucket per client address.
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self { capacity: f64::from(capacity), refill_per_sec, buckets: Mutex::new(HashMap::new()) }
    }

    pub fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.buckets.lock();
        if map.len() >= MAX_TRACKED_IPS {
            let cap = self.capacity;
            let rate = self.refill_per_sec;
            map.retain(|_, (tokens, last)| *tokens + now.duration_since(*last).as_secs_f64() * rate < cap);
            if map.len() >= MAX_TRACKED_IPS {
                return false;
            }
        }
        let (tokens, last) = map.entry(ip).or_insert((self.capacity, now));
        *tokens = (*tokens + now.duration_since(*last).as_secs_f64() * self.refill_per_sec).min(self.capacity);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

pub async fn headers(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/") || req.uri().path() == "/metrics";
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"));
    if state.settings.tls {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=63072000; includeSubDomains"));
    }
    if is_api && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

pub async fn rate_limit(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !state.general_limiter.allow(addr.ip()) {
        return ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }
    next.run(req).await
}

fn origin_of(headers: &HeaderMap, tls: bool) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host.is_empty() || host.len() > 255 || host.contains(['/', '@', ' ']) {
        return None;
    }
    Some(format!("{}://{}", if tls { "https" } else { "http" }, host.to_ascii_lowercase()))
}

/// Rejects cross-site state-changing requests (defence in depth alongside SameSite cookies and CSRF tokens).
pub async fn same_origin(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        let h = req.headers();
        if let Some(fetch_site) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok())
            && !matches!(fetch_site, "same-origin" | "none")
        {
            return ApiError::new(StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
        }
        if let Some(origin) = h.get(header::ORIGIN) {
            let expected = state.settings.public_origin.clone().or_else(|| origin_of(h, state.settings.tls));
            let matches = origin.to_str().ok().zip(expected.as_deref()).is_some_and(|(o, e)| o.eq_ignore_ascii_case(e));
            if !matches {
                return ApiError::new(StatusCode::FORBIDDEN, "origin mismatch").into_response();
            }
        }
    }
    next.run(req).await
}

/// Constant-time byte comparison.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

pub fn retry_after(d: Duration) -> HeaderValue {
    HeaderValue::from(d.as_secs().max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_blocks_after_capacity() {
        let l = RateLimiter::new(2, 0.0);
        let ip: IpAddr = "127.0.0.1".parse().expect("ip");
        assert!(l.allow(ip));
        assert!(l.allow(ip));
        assert!(!l.allow(ip));
    }
}
