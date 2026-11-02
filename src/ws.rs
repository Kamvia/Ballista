// File: ballista/src/ws.rs
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result, sanitize};
use crate::metrics::Key;
use crate::vu::{RunCtx, Vu};

const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;

/// A recorded WebSocket connection. Connect time and request/response round trips are measured.
pub struct WsConn {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    ctx: Arc<RunCtx>,
    vu: u64,
    cancel: CancellationToken,
    scenario: Arc<str>,
    shard: usize,
    default_timeout: Duration,
}

impl WsConn {
    pub(crate) async fn connect(vu: &Vu, target: &str, name: &str) -> Result<Self> {
        let key = Key::request(vu.scenario_arc(), "WS-CONNECT", &sanitize(name, 128));
        let mut url = vu.resolve(target)?;
        let scheme = match url.scheme() {
            "http" | "ws" => "ws",
            _ => "wss",
        };
        url.set_scheme(scheme).map_err(|()| Error::Ws("cannot set websocket scheme".into()))?;
        let plan = vu.plan();
        if plan.target.insecure_tls && scheme == "wss" {
            return Err(Error::Ws("insecure_tls is not supported for websocket targets".into()));
        }
        let cfg = WebSocketConfig::default().max_message_size(Some(MAX_WS_MESSAGE)).max_frame_size(Some(MAX_WS_MESSAGE));
        let start = Instant::now();
        let connect = tokio_tungstenite::connect_async_with_config(url.as_str(), Some(cfg), true);
        let out = tokio::select! {
            () = vu.ctx.cancel.cancelled() => return Err(Error::Cancelled),
            r = tokio::time::timeout(plan.target.connect_timeout, connect) => r,
        };
        let latency = start.elapsed();
        match out {
            Ok(Ok((stream, _resp))) => {
                vu.record(&key, latency, true, Some(101), (0, 0), None);
                Ok(Self {
                    stream,
                    ctx: Arc::clone(&vu.ctx),
                    vu: vu.id(),
                    cancel: vu.ctx.cancel.clone(),
                    scenario: Arc::clone(vu.scenario_arc()),
                    shard: vu.shard(),
                    default_timeout: plan.target.timeout,
                })
            }
            Ok(Err(e)) => {
                let msg = sanitize(&format!("websocket connect: {e}"), 240);
                vu.record(&key, latency, false, None, (0, 0), Some(&msg));
                Err(Error::Ws(msg))
            }
            Err(_) => {
                let msg = "websocket connect: timeout".to_owned();
                vu.record(&key, latency, false, None, (0, 0), Some(&msg));
                Err(Error::Ws(msg))
            }
        }
    }

    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        self.stream.send(Message::text(text)).await.map_err(|e| Error::Ws(sanitize(&e.to_string(), 240)))
    }

    /// Receives the next text or binary message, answering pings transparently.
    pub async fn recv_text(&mut self, timeout: Option<Duration>) -> Result<String> {
        let deadline = timeout.unwrap_or(self.default_timeout);
        let fut = async {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Text(t))) => return Ok(t.to_string()),
                    Some(Ok(Message::Binary(b))) => return Ok(String::from_utf8_lossy(&b).into_owned()),
                    Some(Ok(Message::Ping(p))) => {
                        let _ = self.stream.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(Error::Ws("connection closed by peer".into()));
                    }
                    Some(Err(e)) => return Err(Error::Ws(sanitize(&e.to_string(), 240))),
                }
            }
        };
        tokio::select! {
            () = self.cancel.cancelled() => Err(Error::Cancelled),
            r = tokio::time::timeout(deadline, fut) => r.unwrap_or_else(|_| Err(Error::Ws("receive timeout".into()))),
        }
    }

    /// Sends a message and waits for the next one; the round trip is recorded under `name`.
    pub async fn request(&mut self, name: &str, text: &str, timeout: Option<Duration>) -> Result<String> {
        let key = Key::request(&self.scenario, "WS", &sanitize(name, 128));
        let start = Instant::now();
        let bytes_out = text.len() as u64;
        let res = match self.send_text(text).await {
            Ok(()) => self.recv_text(timeout).await,
            Err(e) => Err(e),
        };
        let latency = start.elapsed();
        if self.ctx.trace {
            match &res {
                Ok(reply) => println!(
                    "[vu {}] WS {} sent {} bytes, reply in {:.1} ms: {}",
                    self.vu,
                    sanitize(name, 64),
                    bytes_out,
                    latency.as_secs_f64() * 1000.0,
                    crate::trace::preview(reply.as_bytes())
                ),
                Err(e) => crate::trace::failure(self.vu, latency, &e.to_string()),
            }
        }
        match res {
            Ok(reply) => {
                self.ctx.record(self.shard, self.vu, &key, latency, true, None, (reply.len() as u64, bytes_out), None);
                Ok(reply)
            }
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Err(e) => {
                let msg = e.to_string();
                self.ctx.record(self.shard, self.vu, &key, latency, false, None, (0, bytes_out), Some(&msg));
                Err(e)
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }
}
