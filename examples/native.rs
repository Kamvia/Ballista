// File: ballista/examples/native.rs
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use ballista::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Item {
    id: u32,
}

#[derive(Deserialize)]
struct ItemList {
    items: Vec<Item>,
}

fn list_and_open(vu: &mut Vu) -> BoxFuture<'_, Result<()>> {
    Box::pin(async move {
        let list: ItemList = vu.get("/items").name("list items").send().await?.json()?;
        let Some(first) = list.items.first() else {
            return Err(Error::Assertion("empty catalog".into()));
        };
        vu.get(format!("/items/{}", first.id))
            .name("item detail")
            .expect(200)
            .validate(|r| if r.body.len() > 2 { Ok(()) } else { Err("empty body".into()) })
            .send()
            .await?;
        Ok(())
    })
}

fn search(vu: &mut Vu) -> BoxFuture<'_, Result<()>> {
    Box::pin(async move {
        vu.get("/items").query("q", "item-1").name("search").send().await?;
        Ok(())
    })
}

fn slow_page(vu: &mut Vu) -> BoxFuture<'_, Result<()>> {
    Box::pin(async move {
        vu.get("/slow").name("slow page").send().await?;
        Ok(())
    })
}

/// Custom-protocol measurement: any future can be timed and recorded with `measure`.
fn probe(vu: &mut Vu) -> BoxFuture<'_, Result<()>> {
    Box::pin(async move {
        vu.measure("TCP", "connect 8080", async { tokio::net::TcpStream::connect("127.0.0.1:8080").await.map(drop) }).await?;
        let mut conn = vu.ws_connect("/ws", "echo socket").await?;
        let reply = conn.request("echo", "hello", Some(Duration::from_secs(2))).await?;
        conn.close().await;
        if !reply.starts_with("echo:") {
            return Err(Error::Assertion("bad echo".into()));
        }
        Ok(())
    })
}

/// Example listener: counts slow requests without blocking the hot path.
#[derive(Default)]
struct SlowRequests {
    over_100ms: AtomicU64,
}

impl Listener for SlowRequests {
    fn on_request(&self, e: &RequestEvent<'_>) {
        if e.latency > Duration::from_millis(100) {
            self.over_100ms.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn on_test_stop(&self, _snapshot: &ballista::metrics::Snapshot) {
        println!("requests slower than 100 ms: {}", self.over_100ms.load(Ordering::Relaxed));
    }
}

fn main() -> ExitCode {
    let catalog = Tasks::new().tagged("list_and_open", 3, &["catalog"], list_and_open).tagged("search", 1, &["catalog"], search).tagged(
        "slow_page",
        1,
        &["slow"],
        slow_page,
    );
    let probes = Tasks::new().sequential().task("probe", 1, probe);
    main_with(Registry::new().register("catalog", catalog).register("probe", probes).listener(SlowRequests::default()))
}
