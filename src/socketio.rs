// File: ballista/src/socketio.rs
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result, sanitize};
use crate::metrics::Key;
use crate::vu::Vu;
use crate::ws::WsConn;

const MAX_BUFFERED: usize = 1000;
const MAX_EVENT_NAME: usize = 128;

/// One decoded Socket.IO packet (protocol v5 over Engine.IO v4).
#[derive(Debug, PartialEq)]
enum Packet {
    Ping,
    Close,
    Open,
    Connect(String),
    ConnectError(String, Value),
    Disconnect(String),
    Event { ns: String, id: Option<u64>, args: Vec<Value> },
    Ack { ns: String, id: u64, args: Vec<Value> },
    Other,
}

fn parse(raw: &str) -> Packet {
    let mut chars = raw.chars();
    match chars.next() {
        Some('0') => return Packet::Open,
        Some('1') => return Packet::Close,
        Some('2') => return Packet::Ping,
        Some('4') => {}
        _ => return Packet::Other,
    }
    let Some(kind) = chars.next() else { return Packet::Other };
    let mut rest = chars.as_str();
    let mut ns = "/".to_owned();
    if rest.starts_with('/') {
        let end = rest.find(',').unwrap_or(rest.len());
        ns = rest[..end].to_owned();
        rest = rest.get(end + 1..).unwrap_or("");
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let id = if digits > 0 && digits <= 18 { rest[..digits].parse::<u64>().ok() } else { None };
    let body = &rest[digits..];
    let payload = || serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
    let args = || match payload() {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    match kind {
        '0' => Packet::Connect(ns),
        '1' => Packet::Disconnect(ns),
        '2' => Packet::Event { ns, id, args: args() },
        '3' => match id {
            Some(id) => Packet::Ack { ns, id, args: args() },
            None => Packet::Other,
        },
        '4' => Packet::ConnectError(ns, payload()),
        _ => Packet::Other,
    }
}

/// A Socket.IO client connection bound to one namespace. Handshake, acks and event round trips are recorded.
pub struct SioConn {
    ws: WsConn,
    ns: String,
    next_id: u64,
    buffered: VecDeque<(String, Vec<Value>)>,
    timeout: Duration,
}

fn ns_prefix(ns: &str) -> String {
    if ns == "/" { String::new() } else { format!("{ns},") }
}

fn valid_event(e: &str) -> Result<()> {
    if e.is_empty() || e.len() > MAX_EVENT_NAME || e.chars().any(char::is_control) {
        return Err(Error::check("socket.io event names must be 1..128 printable characters"));
    }
    Ok(())
}

impl SioConn {
    /// Opens the WebSocket transport at `path` (usually `/socket.io/`) and joins `namespace`.
    pub async fn connect(vu: &Vu, path: &str, namespace: &str, auth: Option<&Value>, name: &str) -> Result<Self> {
        if !namespace.starts_with('/') || namespace.len() > 128 || namespace.contains([',', '?', '#']) {
            return Err(Error::check("socket.io namespace must start with '/' and contain no ',', '?' or '#'"));
        }
        let sep = if path.contains('?') { '&' } else { '?' };
        let target = format!("{path}{sep}EIO=4&transport=websocket");
        let key = Key::request(vu.scenario_arc(), "SIO-CONNECT", &sanitize(name, 128));
        let start = Instant::now();
        let timeout = vu.plan().target.timeout;
        let res = async {
            let mut ws = WsConn::connect(vu, &target, name).await?;
            let open = ws.recv_text(Some(timeout)).await?;
            if parse(&open) != Packet::Open {
                return Err(Error::Ws("socket.io: expected engine.io open packet".into()));
            }
            let auth = match auth {
                Some(a) if a.is_object() => a.to_string(),
                Some(_) => return Err(Error::check("socket.io auth must be an object")),
                None => String::new(),
            };
            ws.send_text(&format!("40{}{auth}", ns_prefix(namespace))).await?;
            let mut conn = Self { ws, ns: namespace.to_owned(), next_id: 0, buffered: VecDeque::new(), timeout };
            let deadline = Instant::now() + timeout;
            loop {
                match conn.next(deadline).await? {
                    Packet::Connect(ns) if ns == conn.ns => return Ok(conn),
                    Packet::ConnectError(ns, v) if ns == conn.ns => {
                        let msg = v.get("message").and_then(Value::as_str).unwrap_or("rejected");
                        return Err(Error::Ws(format!("socket.io connect error: {}", sanitize(msg, 160))));
                    }
                    _ => {}
                }
            }
        }
        .await;
        match res {
            Ok(c) => {
                vu.record(&key, start.elapsed(), true, Some(101), (0, 0), None);
                Ok(c)
            }
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Err(e) => {
                vu.record(&key, start.elapsed(), false, None, (0, 0), Some(&e.to_string()));
                Err(e)
            }
        }
    }

    /// Reads the next non-ping packet, answering Engine.IO pings and buffering foreign events.
    async fn next(&mut self, deadline: Instant) -> Result<Packet> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Ws("socket.io receive timeout".into()));
            }
            let raw = self.ws.recv_text(Some(left)).await?;
            match parse(&raw) {
                Packet::Ping => self.ws.send_text("3").await?,
                Packet::Close => return Err(Error::Ws("socket.io: server closed the session".into())),
                Packet::Disconnect(ns) if ns == self.ns => {
                    return Err(Error::Ws("socket.io: namespace disconnected by server".into()));
                }
                p => return Ok(p),
            }
        }
    }

    fn frame(&self, id: Option<u64>, event: &str, data: Option<&Value>) -> String {
        let mut args = vec![Value::String(event.to_owned())];
        match data {
            Some(Value::Array(a)) => args.extend(a.iter().cloned()),
            Some(v) => args.push(v.clone()),
            None => {}
        }
        let id = id.map(|i| i.to_string()).unwrap_or_default();
        format!("42{}{id}{}", ns_prefix(&self.ns), Value::Array(args))
    }

    /// Fire-and-forget emit. An array `data` is sent as multiple arguments.
    pub async fn emit(&mut self, vu: &Vu, event: &str, data: Option<&Value>, name: &str) -> Result<()> {
        valid_event(event)?;
        let frame = self.frame(None, event, data);
        let key = Key::request(vu.scenario_arc(), "SIO-EMIT", &sanitize(name, 128));
        let start = Instant::now();
        let res = self.ws.send_text(&frame).await;
        let ok = res.is_ok();
        let err = res.as_ref().err().map(ToString::to_string);
        vu.record(&key, start.elapsed(), ok, None, (0, frame.len() as u64), err.as_deref());
        res
    }

    /// Emits with an acknowledgement id and waits for the server's ack; the round trip is recorded.
    pub async fn emit_ack(
        &mut self,
        vu: &Vu,
        event: &str,
        data: Option<&Value>,
        name: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<Value>> {
        valid_event(event)?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let frame = self.frame(Some(id), event, data);
        let bytes_out = frame.len() as u64;
        self.timed(vu, name, bytes_out, timeout, async move |c: &mut Self, deadline| {
            c.ws.send_text(&frame).await?;
            loop {
                match c.next(deadline).await? {
                    Packet::Ack { ns, id: got, args } if ns == c.ns && got == id => return Ok(args),
                    Packet::Event { ns, args, .. } if ns == c.ns => c.buffer(args),
                    _ => {}
                }
            }
        })
        .await
    }

    /// Waits for `event` (optionally after emitting `emit`), recording the time until it arrives.
    pub async fn wait_event(
        &mut self,
        vu: &Vu,
        event: &str,
        emit: Option<(&str, Option<&Value>)>,
        name: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<Value>> {
        valid_event(event)?;
        if emit.is_none()
            && let Some(pos) = self.buffered.iter().position(|(e, _)| e == event)
        {
            let key = Key::request(vu.scenario_arc(), "SIO", &sanitize(name, 128));
            vu.record(&key, Duration::ZERO, true, None, (0, 0), None);
            return Ok(self.buffered.remove(pos).map(|(_, a)| a).unwrap_or_default());
        }
        let frame = match emit {
            Some((e, d)) => {
                valid_event(e)?;
                Some(self.frame(None, e, d))
            }
            None => None,
        };
        let bytes_out = frame.as_ref().map_or(0, |f| f.len() as u64);
        let wanted = event.to_owned();
        self.timed(vu, name, bytes_out, timeout, async move |c: &mut Self, deadline| {
            if let Some(f) = &frame {
                c.ws.send_text(f).await?;
            }
            loop {
                if let Packet::Event { ns, mut args, .. } = c.next(deadline).await?
                    && ns == c.ns
                {
                    if args.first().and_then(Value::as_str) == Some(wanted.as_str()) {
                        args.remove(0);
                        return Ok(args);
                    }
                    c.buffer(args);
                }
            }
        })
        .await
    }

    fn buffer(&mut self, mut args: Vec<Value>) {
        let Some(Value::String(event)) = args.first().cloned() else { return };
        args.remove(0);
        if self.buffered.len() == MAX_BUFFERED {
            self.buffered.pop_front();
        }
        self.buffered.push_back((event, args));
    }

    async fn timed<F>(&mut self, vu: &Vu, name: &str, bytes_out: u64, timeout: Option<Duration>, f: F) -> Result<Vec<Value>>
    where
        F: AsyncFnOnce(&mut Self, Instant) -> Result<Vec<Value>>,
    {
        let key = Key::request(vu.scenario_arc(), "SIO", &sanitize(name, 128));
        let start = Instant::now();
        let deadline = start + timeout.unwrap_or(self.timeout);
        let res = f(self, deadline).await;
        let latency = start.elapsed();
        match &res {
            Ok(args) => {
                let bytes_in = args.iter().map(|a| a.to_string().len() as u64).sum();
                vu.record(&key, latency, true, None, (bytes_in, bytes_out), None);
            }
            Err(Error::Cancelled) => {}
            Err(e) => vu.record(&key, latency, false, None, (0, bytes_out), Some(&e.to_string())),
        }
        res
    }

    pub async fn close(mut self) {
        let _ = self.ws.send_text(&format!("41{}", ns_prefix(&self.ns))).await;
        self.ws.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_packets() {
        assert_eq!(parse("2"), Packet::Ping);
        assert_eq!(parse("0{\"sid\":\"x\"}"), Packet::Open);
        assert_eq!(parse("40{\"sid\":\"a\"}"), Packet::Connect("/".into()));
        assert_eq!(parse("40/chat,{\"sid\":\"a\"}"), Packet::Connect("/chat".into()));
        assert_eq!(
            parse("42/chat,7[\"msg\",{\"a\":1}]"),
            Packet::Event { ns: "/chat".into(), id: Some(7), args: vec![Value::from("msg"), serde_json::json!({"a":1})] }
        );
        assert_eq!(parse("4312[\"ok\"]"), Packet::Ack { ns: "/".into(), id: 12, args: vec![Value::from("ok")] });
        assert_eq!(parse("9"), Packet::Other);
    }
}
