// File: ballista/examples/demo_target.rs
use std::net::SocketAddr;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

async fn items(Query(q): Query<SearchQuery>) -> Json<serde_json::Value> {
    let filter = q.q.unwrap_or_default();
    let list: Vec<_> = (1..=20)
        .map(|i| json!({ "id": i, "name": format!("item-{i}"), "price": i * 3 }))
        .filter(|v| filter.is_empty() || v["name"].as_str().is_some_and(|n| n.contains(&filter)))
        .collect();
    Json(json!({ "items": list }))
}

async fn item(Path(id): Path<u32>) -> Response {
    tokio::time::sleep(Duration::from_millis(u64::from(id % 7) * 3)).await;
    if id == 0 || id > 20 {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    }
    Json(json!({ "id": id, "name": format!("item-{id}"), "stock": 100 - id })).into_response()
}

async fn login(Json(b): Json<Login>) -> Response {
    if b.username.is_empty() || b.password.len() < 4 {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid" }))).into_response();
    }
    Json(json!({ "token": format!("tok-{}", b.username) })).into_response()
}

async fn cart(headers: HeaderMap, Json(body): Json<serde_json::Value>) -> Response {
    let authed = headers.get("authorization").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("Bearer tok-"));
    if !authed {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if rand::random_range(0..100) < 2 {
        return (StatusCode::SERVICE_UNAVAILABLE, "busy").into_response();
    }
    (StatusCode::CREATED, Json(json!({ "ok": true, "line": body }))).into_response()
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(rand::random_range(50..250))).await;
    "done"
}

static FLAKY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Fails two out of three calls with 503 to exercise retries.
async fn flaky() -> Response {
    let n = FLAKY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n % 3 == 2 { (StatusCode::OK, "recovered").into_response() } else { StatusCode::SERVICE_UNAVAILABLE.into_response() }
}

async fn upload(mut multipart: axum::extract::Multipart) -> Response {
    let mut parts = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_owned();
        let file = field.file_name().map(str::to_owned);
        let len = field.bytes().await.map(|b| b.len()).unwrap_or(0);
        parts.push(json!({ "field": name, "file": file, "bytes": len }));
    }
    Json(json!({ "parts": parts })).into_response()
}

async fn events() -> Response {
    let body: String = (1..=20).map(|i| format!("event: tick\ndata: {{\"n\":{i}}}\n\n")).collect();
    ([("content-type", "text/event-stream")], body).into_response()
}

/// Event stream that resumes after `Last-Event-ID` and closes after three events per connection.
async fn sse(headers: HeaderMap) -> Response {
    let after: u32 = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let stream = futures_util::stream::unfold(0_u32, move |i| async move {
        if i >= 3 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(if i == 0 { 30 } else { 10 })).await;
        let id = after + i + 1;
        let prefix = if i == 0 { "retry: 100\n" } else { "" };
        let frame = format!("{prefix}id: {id}\nevent: price\ndata: {{\"seq\":{id}}}\n\n");
        Some((Ok::<_, std::convert::Infallible>(frame), i + 1))
    });
    ([("content-type", "text/event-stream"), ("cache-control", "no-cache")], axum::body::Body::from_stream(stream)).into_response()
}

/// Minimal Socket.IO v5 (Engine.IO v4, websocket transport) endpoint: acks `echo` and answers `ping` with `pong`.
async fn socketio(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(|mut socket: WebSocket| async move {
        let open = r#"0{"sid":"demo","upgrades":[],"pingInterval":25000,"pingTimeout":20000,"maxPayload":1000000}"#;
        if socket.send(Message::Text(open.into())).await.is_err() {
            return;
        }
        while let Some(Ok(Message::Text(t))) = socket.recv().await {
            let t = t.to_string();
            let reply = if let Some(rest) = t.strip_prefix("40") {
                let ns = rest.split_once(',').map(|(n, _)| format!("{n},")).filter(|n| n.starts_with('/')).unwrap_or_default();
                Some(format!("40{ns}{{\"sid\":\"s1\"}}"))
            } else if let Some(rest) = t.strip_prefix("42") {
                let (ns, rest) = match rest.strip_prefix('/') {
                    Some(r) => r.split_once(',').map(|(n, r)| (format!("/{n},"), r)).unwrap_or_default(),
                    None => (String::new(), rest),
                };
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                let (id, payload) = rest.split_at(digits);
                let args: Vec<serde_json::Value> = serde_json::from_str(payload).unwrap_or_default();
                let event = args.first().and_then(|e| e.as_str()).unwrap_or("");
                match (event, id.is_empty()) {
                    ("echo", false) => Some(format!("43{ns}{id}{}", serde_json::Value::Array(args[1..].to_vec()))),
                    ("ping", _) => Some(format!("42{ns}{}", json!(["pong", { "at": "server" }]))),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(r) = reply
                && socket.send(Message::Text(r.into())).await.is_err()
            {
                break;
            }
        }
    })
}

async fn tcp_echo(addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let (mut r, mut w) = sock.split();
            let _ = tokio::io::copy(&mut r, &mut w).await;
        });
    }
}

async fn udp_echo(addr: SocketAddr) -> std::io::Result<()> {
    let sock = tokio::net::UdpSocket::bind(addr).await?;
    let mut buf = vec![0_u8; 65_535];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;
        let _ = sock.send_to(&buf[..n], peer).await;
    }
}

async fn ws(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(|mut socket: WebSocket| async move {
        while let Some(Ok(msg)) = socket.recv().await {
            if let Message::Text(t) = msg
                && socket.send(Message::Text(format!("echo:{t}").into())).await.is_err()
            {
                break;
            }
        }
    })
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr: SocketAddr =
        std::env::var("DEMO_ADDR").ok().and_then(|a| a.parse().ok()).unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8080)));
    let app = Router::new()
        .route("/items", get(items))
        .route("/items/{id}", get(item))
        .route("/login", post(login))
        .route("/cart", post(cart))
        .route("/slow", get(slow))
        .route("/ws", get(ws))
        .route("/flaky", get(flaky))
        .route("/upload", post(upload))
        .route("/events", get(events))
        .route("/sse", get(sse))
        .route("/socket.io/", get(socketio));
    tokio::spawn(tcp_echo(SocketAddr::from(([127, 0, 0, 1], 7000))));
    tokio::spawn(udp_echo(SocketAddr::from(([127, 0, 0, 1], 7001))));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("demo target listening on http://{addr}");
    axum::serve(listener, app).await
}
