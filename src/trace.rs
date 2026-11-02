// File: ballista/src/trace.rs
use std::time::Duration;

use http::HeaderMap;
use url::Url;

use crate::error::sanitize;

const PREVIEW_CHARS: usize = 2048;
const SENSITIVE: [&str; 12] =
    ["password", "passwd", "secret", "token", "auth", "authorization", "cookie", "session", "apikey", "api_key", "signature", "credential"];
const REDACTED: &str = "[redacted]";

fn is_sensitive(name: &str) -> bool {
    let n = name.to_ascii_lowercase().replace('-', "_");
    SENSITIVE.iter().any(|s| n.contains(s)) || n == "x_api_key" || n == "sig"
}

pub fn redact_url(url: &Url) -> String {
    let mut u = url.clone();
    if u.query().is_some() {
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| {
                let v = if is_sensitive(&k) { REDACTED.to_owned() } else { v.into_owned() };
                (k.into_owned(), v)
            })
            .collect();
        u.query_pairs_mut().clear().extend_pairs(pairs);
    }
    u.set_fragment(None);
    u.to_string()
}

pub fn redact_headers(h: &HeaderMap) -> String {
    if h.is_empty() {
        return "(none)".to_owned();
    }
    let mut out = Vec::new();
    for (k, v) in h {
        let val = if is_sensitive(k.as_str()) { REDACTED.to_owned() } else { sanitize(v.to_str().unwrap_or("[binary]"), 200) };
        out.push(format!("{}: {val}", k.as_str()));
    }
    out.join("\n      ")
}

fn redact_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if is_sensitive(k) {
                    *val = serde_json::Value::String(REDACTED.to_owned());
                } else {
                    redact_json(val);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_json),
        _ => {}
    }
}

pub fn preview(body: &[u8]) -> String {
    if body.is_empty() {
        return "(empty)".to_owned();
    }
    if let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(body) {
        redact_json(&mut v);
        return sanitize(&v.to_string(), PREVIEW_CHARS);
    }
    let text = String::from_utf8_lossy(body);
    if text.contains('=') && !text.contains(' ') {
        let redacted: Vec<String> = url::form_urlencoded::parse(text.as_bytes())
            .map(|(k, v)| if is_sensitive(&k) { format!("{k}={REDACTED}") } else { format!("{k}={v}") })
            .collect();
        return sanitize(&redacted.join("&"), PREVIEW_CHARS);
    }
    sanitize(&text, PREVIEW_CHARS)
}

pub struct TraceRequest<'a> {
    pub vu: u64,
    pub scenario: &'a str,
    pub method: &'a str,
    pub url: &'a Url,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
}

pub fn request(t: &TraceRequest<'_>) {
    println!(
        "[vu {}] {} >> {} {}\n    req headers:\n      {}\n    req body: {}",
        t.vu,
        sanitize(t.scenario, 64),
        t.method,
        redact_url(t.url),
        redact_headers(t.headers),
        preview(t.body)
    );
}

pub fn response(vu: u64, status: u16, latency: Duration, headers: &HeaderMap, body: &[u8], ok: bool, err: Option<&str>) {
    println!(
        "[vu {vu}] << {status} in {:.1} ms ok={ok}{}\n    resp headers:\n      {}\n    resp body: {}",
        latency.as_secs_f64() * 1000.0,
        err.map(|e| format!(" error={}", sanitize(e, 200))).unwrap_or_default(),
        redact_headers(headers),
        preview(body)
    );
}

pub fn failure(vu: u64, latency: Duration, err: &str) {
    println!("[vu {vu}] << transport failure after {:.1} ms: {}", latency.as_secs_f64() * 1000.0, sanitize(err, 300));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sensitive_values() {
        let u = Url::parse("http://h/x?q=1&token=abc").expect("url");
        assert!(redact_url(&u).contains("token=%5Bredacted%5D"));
        let p = preview(br#"{"user":"a","password":"p","nested":{"api_key":"k"}}"#);
        assert!(!p.contains("\"p\"") && !p.contains("\"k\""));
        assert!(preview(b"user=a&password=b").contains("password=[redacted]"));
    }
}
