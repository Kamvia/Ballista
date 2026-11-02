// File: ballista/src/manager/webhook.rs
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use url::Url;

use crate::error::{Error, Result, sanitize};

const BLOCK: usize = 64;
const ATTEMPTS: u32 = 4;
const TIMEOUT: Duration = Duration::from_secs(10);
const QUEUE: usize = 256;
const MIN_SECRET: usize = 32;

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0_u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// `t=<unix>,v1=<hex hmac of "<unix>.<body>">`, so receivers can reject replays.
pub fn signature(secret: &[u8], ts: u64, body: &[u8]) -> String {
    let mut msg = Vec::with_capacity(body.len() + 21);
    msg.extend_from_slice(ts.to_string().as_bytes());
    msg.push(b'.');
    msg.extend_from_slice(body);
    format!("t={ts},v1={}", hex::encode(hmac_sha256(secret, &msg)))
}

#[derive(Clone)]
pub struct Webhook {
    tx: mpsc::Sender<(String, Vec<u8>)>,
}

impl Webhook {
    pub fn start(url: &str, secret: String) -> Result<Self> {
        let u = Url::parse(url).map_err(|e| Error::config(format!("webhook url: {e}")))?;
        let loopback = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(u.scheme() == "https" || (u.scheme() == "http" && loopback)) || !u.username().is_empty() || u.password().is_some() {
            return Err(Error::config("webhook url must be https (http only for loopback) without credentials"));
        }
        if secret.len() < MIN_SECRET {
            return Err(Error::config("webhook secret must be at least 32 characters"));
        }
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::config(format!("webhook client: {e}")))?;
        let (tx, mut rx) = mpsc::channel::<(String, Vec<u8>)>(QUEUE);
        tokio::spawn(async move {
            while let Some((event, body)) = rx.recv().await {
                for attempt in 0..ATTEMPTS {
                    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let res = client
                        .post(u.clone())
                        .header("content-type", "application/json")
                        .header("x-ballista-event", &event)
                        .header("x-ballista-signature", signature(secret.as_bytes(), ts, &body))
                        .body(body.clone())
                        .send()
                        .await;
                    match res {
                        Ok(r) if r.status().is_success() => break,
                        Ok(r) => tracing::warn!(status = r.status().as_u16(), attempt, "webhook delivery rejected"),
                        Err(e) => tracing::warn!(error = %sanitize(&e.to_string(), 160), attempt, "webhook delivery failed"),
                    }
                    tokio::time::sleep(Duration::from_secs(2_u64.pow(attempt + 1))).await;
                }
            }
        });
        Ok(Self { tx })
    }

    pub fn send(&self, event: &str, payload: &serde_json::Value) {
        let body = serde_json::to_vec(payload).unwrap_or_default();
        if self.tx.try_send((event.to_owned(), body)).is_err() {
            tracing::warn!("webhook queue full, event dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex::encode(mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }
}
