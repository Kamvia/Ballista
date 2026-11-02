// File: ballista/src/cluster/worker.rs
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    CLUSTER_PATH, LoadFile, LoadOverrides, MAX_LOAD_FRAME, POOL_FINGERPRINT, PROTOCOL_VERSION, ToController, ToWorker, fingerprint,
    validate_token,
};
use crate::config::{Overrides, Plan};
use crate::engine::{Engine, Role, WorkerUpdate};
use crate::error::{Error, Result, sanitize};
use crate::metrics::{RunState, Snapshot};
use crate::scenario::Registry;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const PING_EVERY: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub controller: String,
    pub token: String,
    pub ca_cert: Option<PathBuf>,
    pub name: String,
    pub allow_plaintext: bool,
}

enum End {
    Shutdown,
    Disconnected,
}

fn controller_url(raw: &str, allow_plaintext: bool) -> Result<Url> {
    let mut url = Url::parse(raw.trim()).map_err(|e| Error::config(format!("invalid controller url: {e}")))?;
    let host = url.host_str().ok_or_else(|| Error::config("controller url needs a host"))?.to_owned();
    match url.scheme() {
        "wss" => {}
        "ws" => {
            let loopback =
                host == "localhost" || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
            if !loopback && !allow_plaintext {
                return Err(Error::config("refusing to send the cluster token over plaintext ws:// to a remote host; use wss://"));
            }
        }
        _ => return Err(Error::config("controller url must use ws:// or wss://")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::config("controller url must not embed credentials"));
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path(CLUSTER_PATH);
    }
    Ok(url)
}

fn connector(ca: Option<&PathBuf>) -> Result<Option<Connector>> {
    let Some(path) = ca else {
        return Ok(None);
    };
    let mut roots = rustls::RootCertStore::empty();
    let certs =
        CertificateDer::pem_file_iter(path).map_err(|e| Error::config(format!("cannot read ca certificate {}: {e}", path.display())))?;
    for c in certs {
        let c = c.map_err(|e| Error::config(format!("invalid ca certificate: {e}")))?;
        roots.add(c).map_err(|e| Error::config(format!("invalid ca certificate: {e}")))?;
    }
    if roots.is_empty() {
        return Err(Error::config("ca certificate file contains no certificates"));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::config(format!("tls configuration: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Some(Connector::Rustls(Arc::new(cfg))))
}

async fn send(ws: &mut Ws, msg: &ToController) -> Result<()> {
    let text = serde_json::to_string(msg).map_err(|e| Error::Engine(format!("encode: {e}")))?;
    ws.send(Message::text(text)).await.map_err(|e| Error::Engine(format!("send: {}", sanitize(&e.to_string(), 120))))
}

async fn stop_engine(engine: &Engine) {
    if matches!(engine.state(), RunState::Running | RunState::Stopping) {
        let _ = engine.stop();
        let grace = engine.plan().load.graceful_stop + Duration::from_secs(15);
        let _ = tokio::time::timeout(grace, engine.wait()).await;
    }
}

/// An engine bound to this worker, with the channels that carry its reports and shared state.
pub struct Loaded {
    engine: Engine,
    updates: mpsc::Receiver<WorkerUpdate>,
    shared_rx: mpsc::Receiver<(String, serde_json::Value)>,
    state_rx: watch::Receiver<Arc<Snapshot>>,
    cap: f64,
    /// Bundle directory owned by this load (pool mode), removed when replaced.
    dir: Option<PathBuf>,
}

impl Loaded {
    /// Builds a worker engine for `plan`.
    pub fn new(plan: Plan, registry: &Registry, dir: Option<PathBuf>) -> Result<Self> {
        let (tx, updates) = mpsc::channel(64);
        let engine = Engine::new_with_role(plan, registry, Role::Worker(tx))?;
        let (shared_tx, shared_rx) = mpsc::channel(1024);
        engine.shared().set_outbound(shared_tx);
        let cap = engine.plan().target_cap();
        let state_rx = engine.subscribe();
        Ok(Self { engine, updates, shared_rx, state_rx, cap, dir })
    }

    async fn release(self) {
        stop_engine(&self.engine).await;
        if let Some(d) = self.dir {
            let _ = tokio::fs::remove_dir_all(d).await;
        }
    }
}

/// How the worker obtains its plan.
pub enum Mode {
    /// The plan is loaded locally at start and announced by fingerprint.
    Fixed { loaded: Loaded, fingerprint: String },
    /// No local plan: the controller sends a bundle before each run.
    Pool { registry: Registry, work_dir: PathBuf },
}

async fn flush_updates(ws: &mut Ws, slot: &mut Option<Loaded>, run: u64) -> Result<()> {
    if let Some(l) = slot.as_mut() {
        while let Ok(u) = l.updates.try_recv() {
            send(ws, &ToController::Report { run, active: u.active, iterations: u.iterations, health: u.health, window: u.window }).await?;
        }
    }
    Ok(())
}

async fn stop_slot(slot: &Option<Loaded>) {
    if let Some(l) = slot {
        stop_engine(&l.engine).await;
    }
}

/// Writes a bundle received from the controller into a fresh directory and builds its engine.
fn materialize(
    registry: &Registry,
    work_dir: &Path,
    load: u64,
    plan_text: &str,
    files: &[LoadFile],
    overrides: &LoadOverrides,
) -> Result<(Loaded, String)> {
    use base64::Engine as _;
    if plan_text.len() > 1024 * 1024 || files.len() > 256 {
        return Err(Error::Engine("bundle exceeds limits".into()));
    }
    let dir = work_dir.join(format!("load-{}-{load}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let result = (|| -> Result<(Loaded, String)> {
        let mut total = 0_usize;
        for f in files {
            if !crate::manager::bundles::valid_path(&f.path) && !f.path.starts_with("inline/")
                || f.path.contains("..")
                || f.path == "plan.toml"
            {
                return Err(Error::Engine(format!("invalid bundle path '{}'", sanitize(&f.path, 80))));
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(f.data.as_bytes())
                .map_err(|_| Error::Engine("invalid bundle encoding".into()))?;
            total += data.len();
            if total > 64 * 1024 * 1024 {
                return Err(Error::Engine("bundle exceeds 64 MiB".into()));
            }
            let full = dir.join(&f.path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(full, data)?;
        }
        let plan_path = dir.join("plan.toml");
        std::fs::write(&plan_path, plan_text)?;
        let mut plan = Plan::load(&plan_path)?;
        let run_time = match &overrides.run_time {
            Some(t) => Some(humantime::parse_duration(t).map_err(|e| Error::config(format!("run_time: {e}")))?),
            None => None,
        };
        plan.apply_overrides(&Overrides {
            host: overrides.host.clone(),
            users: overrides.users,
            spawn_rate: overrides.spawn_rate,
            run_time,
            iterations: overrides.iterations,
            tags: overrides.tags.clone(),
            exclude_tags: overrides.exclude_tags.clone(),
            ..Overrides::default()
        })?;
        plan.report = crate::config::ReportConfig { percentiles: plan.report.percentiles.clone(), ..Default::default() };
        let fp = fingerprint(&plan)?;
        Ok((Loaded::new(plan, registry, Some(dir.clone()))?, fp))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

/// Connects to the controller and executes its commands, reconnecting with backoff.
/// Any active run is stopped immediately when the controller link is lost.
pub async fn run(mode: Mode, cfg: WorkerConfig, shutdown: CancellationToken) -> Result<()> {
    validate_token(&cfg.token)?;
    let url = controller_url(&cfg.controller, cfg.allow_plaintext)?;
    let tls = connector(cfg.ca_cert.as_ref())?;
    let (mut slot, fingerprint, pool) = match mode {
        Mode::Fixed { loaded, fingerprint } => (Some(loaded), fingerprint, None),
        Mode::Pool { registry, work_dir } => {
            std::fs::create_dir_all(&work_dir)?;
            (None, POOL_FINGERPRINT.to_owned(), Some((registry, work_dir)))
        }
    };
    let mut backoff = Duration::from_secs(1);
    let outcome = loop {
        if shutdown.is_cancelled() {
            break Ok(());
        }
        match session(&mut slot, pool.as_ref(), &cfg, &url, tls.clone(), &fingerprint, &shutdown).await {
            Ok(End::Shutdown) => break Ok(()),
            Ok(End::Disconnected) => {
                tracing::warn!("controller connection lost");
                backoff = Duration::from_secs(1);
            }
            Err(Error::Config(m)) => break Err(Error::Config(m)),
            Err(e) => tracing::warn!(error = %e, "controller session failed"),
        }
        stop_slot(&slot).await;
        if let Some(l) = slot.as_mut() {
            while l.updates.try_recv().is_ok() {}
        }
        tracing::info!(retry_in = ?backoff, "reconnecting to controller");
        tokio::select! {
            () = shutdown.cancelled() => break Ok(()),
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    };
    if pool.is_some()
        && let Some(l) = slot.take()
    {
        l.release().await;
    }
    outcome
}

enum SlotEvent {
    Update(Option<WorkerUpdate>),
    Shared(Option<(String, serde_json::Value)>),
    /// The engine published a new state; true when it is stopped.
    State(Result<bool>),
}

/// Next event from the loaded engine; never resolves while no plan is loaded.
async fn next_event(slot: &mut Option<Loaded>) -> SlotEvent {
    let Some(l) = slot.as_mut() else {
        return std::future::pending().await;
    };
    tokio::select! {
        u = l.updates.recv() => SlotEvent::Update(u),
        s = l.shared_rx.recv() => SlotEvent::Shared(s),
        c = l.state_rx.changed() => SlotEvent::State(match c {
            Ok(()) => Ok(l.state_rx.borrow_and_update().state == RunState::Stopped),
            Err(_) => Err(Error::Engine("engine stopped publishing".into())),
        }),
    }
}

#[allow(clippy::too_many_lines)]
async fn session(
    slot: &mut Option<Loaded>,
    pool: Option<&(Registry, PathBuf)>,
    cfg: &WorkerConfig,
    url: &Url,
    tls: Option<Connector>,
    fingerprint: &str,
    shutdown: &CancellationToken,
) -> Result<End> {
    let mut request = url.as_str().into_client_request().map_err(|e| Error::config(format!("controller url: {e}")))?;
    let auth = HeaderValue::from_str(&format!("Bearer {}", cfg.token)).map_err(|_| Error::config("invalid token"))?;
    request.headers_mut().insert("authorization", auth);
    let ws_cfg = WebSocketConfig::default().max_message_size(Some(MAX_LOAD_FRAME)).max_frame_size(Some(MAX_LOAD_FRAME));
    let connect = tokio_tungstenite::connect_async_tls_with_config(request, Some(ws_cfg), true, tls);
    let (mut ws, _) = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await.map_err(|_| Error::Engine("connect timeout".into()))? {
        Ok(ok) => ok,
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) if resp.status().as_u16() == 401 => {
            return Err(Error::config("controller rejected the cluster token"));
        }
        Err(e) => return Err(Error::Engine(format!("connect: {}", sanitize(&e.to_string(), 200)))),
    };

    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    send(
        &mut ws,
        &ToController::Hello {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            name: cfg.name.clone(),
            fingerprint: fingerprint.to_owned(),
            cores,
        },
    )
    .await?;
    let first = tokio::time::timeout(CONNECT_TIMEOUT, ws.next()).await.map_err(|_| Error::Engine("no welcome from controller".into()))?;
    let id = match first {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<ToWorker>(&t) {
            Ok(ToWorker::Welcome { id }) => id,
            Ok(ToWorker::Reject { reason }) => {
                return Err(Error::config(format!("controller rejected worker: {}", sanitize(&reason, 300))));
            }
            _ => return Err(Error::Engine("unexpected first message".into())),
        },
        _ => return Err(Error::Engine("connection closed during handshake".into())),
    };
    tracing::info!(worker_id = id, controller = %url, pool = pool.is_some(), "registered with controller");

    if let Some(l) = slot.as_mut() {
        l.state_rx.borrow_and_update();
    }
    let mut current_run: Option<u64> = None;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                stop_slot(slot).await;
                if let Some(run) = current_run.take() {
                    flush_updates(&mut ws, slot, run).await?;
                    let _ = send(&mut ws, &ToController::Stopped { run }).await;
                }
                let _ = ws.close(None).await;
                return Ok(End::Shutdown);
            }
            _ = ping.tick() => {
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Ok(End::Disconnected);
                }
            }
            event = next_event(slot) => match event {
                SlotEvent::Shared(Some((key, value))) => {
                    send(&mut ws, &ToController::Shared { key, value }).await?;
                }
                SlotEvent::Update(Some(u)) => {
                    if let Some(run) = current_run {
                        send(&mut ws, &ToController::Report {
                            run, active: u.active, iterations: u.iterations, health: u.health, window: u.window,
                        }).await?;
                    }
                }
                SlotEvent::State(stopped) => {
                    if stopped? && let Some(run) = current_run.take() {
                        flush_updates(&mut ws, slot, run).await?;
                        send(&mut ws, &ToController::Stopped { run }).await?;
                        tracing::info!(run, "run finished on this worker");
                    }
                }
                SlotEvent::Shared(None) | SlotEvent::Update(None) => {
                    return Err(Error::Engine("engine channel closed".into()));
                }
            },
            inbound = tokio::time::timeout(READ_TIMEOUT, ws.next()) => {
                let msg = match inbound {
                    Err(_) => return Ok(End::Disconnected),
                    Ok(None | Some(Err(_))) => return Ok(End::Disconnected),
                    Ok(Some(Ok(Message::Close(_)))) => return Ok(End::Disconnected),
                    Ok(Some(Ok(Message::Text(t)))) => t,
                    Ok(Some(Ok(_))) => continue,
                };
                let cmd: ToWorker = serde_json::from_str(&msg)
                    .map_err(|e| Error::Engine(format!("invalid command: {}", sanitize(&e.to_string(), 120))))?;
                match cmd {
                    ToWorker::Load { load, plan, files, overrides } => {
                        let Some((registry, work_dir)) = pool else {
                            return Err(Error::Engine("controller sent a bundle to a fixed-plan worker".into()));
                        };
                        if current_run.is_some() {
                            send(&mut ws, &ToController::Loaded { load, fingerprint: String::new(), error: Some("busy with a run".into()) }).await?;
                            continue;
                        }
                        if let Some(old) = slot.take() {
                            old.release().await;
                        }
                        let (registry, work_dir, plan_text) = (registry.clone(), work_dir.clone(), plan);
                        let built = tokio::task::spawn_blocking(move || materialize(&registry, &work_dir, load, &plan_text, &files, &overrides))
                            .await
                            .map_err(|e| Error::Engine(format!("load task: {e}")))?;
                        let reply = match built {
                            Ok((loaded, fp)) => {
                                tracing::info!(load, plan = %loaded.engine.plan().display_name(), "bundle loaded");
                                *slot = Some(loaded);
                                ToController::Loaded { load, fingerprint: fp, error: None }
                            }
                            Err(e) => {
                                tracing::warn!(load, error = %e, "bundle rejected");
                                ToController::Loaded { load, fingerprint: String::new(), error: Some(sanitize(&e.to_string(), 300)) }
                            }
                        };
                        send(&mut ws, &reply).await?;
                    }
                    ToWorker::Start { run, index, count, iteration_limit, scenarios, host } => {
                        if count == 0 || index >= count {
                            return Err(Error::Engine("invalid start partition".into()));
                        }
                        let Some(l) = slot.as_mut() else {
                            send(&mut ws, &ToController::Stopped { run }).await?;
                            continue;
                        };
                        stop_engine(&l.engine).await;
                        while l.updates.try_recv().is_ok() {}
                        l.state_rx.borrow_and_update();
                        match l.engine.start_remote(index, count, iteration_limit, scenarios, host) {
                            Ok(()) => {
                                current_run = Some(run);
                                tracing::info!(run, index, count, "run started by controller");
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "cannot start run");
                                send(&mut ws, &ToController::Stopped { run }).await?;
                            }
                        }
                    }
                    ToWorker::Target { run, target } => {
                        if let Some(l) = slot.as_ref()
                            && current_run == Some(run)
                            && target.is_finite()
                            && (0.0..=l.cap).contains(&target)
                        {
                            let _ = l.engine.set_target(target, 0.0);
                        }
                    }
                    ToWorker::Stop { run } => {
                        if let Some(l) = slot.as_ref()
                            && current_run == Some(run)
                        {
                            let _ = l.engine.stop();
                        }
                    }
                    ToWorker::Reject { reason } => {
                        return Err(Error::config(format!("controller rejected worker: {}", sanitize(&reason, 300))));
                    }
                    ToWorker::Welcome { .. } => {}
                    ToWorker::Repartition { run, index, count } => {
                        if let Some(l) = slot.as_ref()
                            && current_run == Some(run)
                        {
                            l.engine.repartition(index, count)?;
                        }
                    }
                    ToWorker::Shared { key, value } => {
                        if let Some(l) = slot.as_ref()
                            && let Err(e) = l.engine.shared().apply_remote(key, value)
                        {
                            tracing::warn!(error = %e, "ignored invalid shared state update");
                        }
                    }
                    ToWorker::Quit => {
                        tracing::info!("controller requested shutdown");
                        stop_slot(slot).await;
                        if let Some(run) = current_run.take() {
                            flush_updates(&mut ws, slot, run).await?;
                            let _ = send(&mut ws, &ToController::Stopped { run }).await;
                        }
                        let _ = ws.close(None).await;
                        return Ok(End::Shutdown);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::controller_url;

    #[test]
    fn plaintext_policy() {
        assert!(controller_url("ws://127.0.0.1:5557", false).is_ok());
        assert!(controller_url("ws://10.0.0.5:5557", false).is_err());
        assert!(controller_url("ws://10.0.0.5:5557", true).is_ok());
        assert_eq!(controller_url("wss://ctl.example.com", false).expect("url").path(), "/cluster");
        assert!(controller_url("http://ctl", false).is_err());
    }
}
