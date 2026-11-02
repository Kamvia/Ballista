// File: ballista/src/vu.rs
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use http::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::config::{ClientMode, Plan};
use crate::error::{Error, Result, sanitize};
use crate::listener::{Listener, RequestEvent};
use crate::metrics::{Key, Kind, Recorder, Sample};
use crate::scenario::feeder::{FeedRow, Feeders};
use crate::trace;
use crate::ws::WsConn;

/// Shared state for one run, visible to every virtual user.
pub struct RunCtx {
    pub plan: Arc<Plan>,
    pub recorder: Arc<Recorder>,
    pub feeders: Arc<Feeders>,
    pub cancel: CancellationToken,
    pub shared_client: Option<reqwest::Client>,
    /// TLS configuration shared by every per-user client of the run (built once, on first use).
    tls: std::sync::OnceLock<std::result::Result<rustls::ClientConfig, String>>,
    pub active: AtomicU64,
    pub listeners: Arc<[Arc<dyn Listener>]>,
    pub trace: bool,
    pub iterations: AtomicU64,
    pub iteration_limit: Option<u64>,
    pub limit_reached: AtomicBool,
    pub protocols: Arc<crate::protocols::Protocols>,
    pub shared: Arc<crate::shared::SharedStore>,
    /// Names, configured weights and live user counts per scenario (index-aligned with the run's scenario set).
    pub scenario_names: Vec<Arc<str>>,
    pub scenario_weights: Vec<u64>,
    pub scenario_active: Vec<AtomicU64>,
    task_counts: parking_lot::Mutex<TaskTally>,
}

type TaskTally = HashMap<(Arc<str>, Arc<str>), u64>;

/// Optional request details forwarded to listeners.
#[derive(Default, Clone, Copy)]
pub(crate) struct Extra<'a> {
    pub context: Option<&'a BTreeMap<String, String>>,
    pub ttfb: Option<Duration>,
}

impl RunCtx {
    pub fn new(
        plan: Arc<Plan>,
        feeders: Arc<Feeders>,
        shared_client: Option<reqwest::Client>,
        listeners: Arc<[Arc<dyn Listener>]>,
        trace: bool,
    ) -> Self {
        Self {
            iteration_limit: plan.load.iterations,
            plan,
            recorder: Arc::new(Recorder::new()),
            feeders,
            cancel: CancellationToken::new(),
            shared_client,
            tls: std::sync::OnceLock::new(),
            active: AtomicU64::new(0),
            listeners,
            trace,
            iterations: AtomicU64::new(0),
            limit_reached: AtomicBool::new(false),
            protocols: Arc::new(crate::protocols::Protocols::default()),
            shared: Arc::new(crate::shared::SharedStore::default()),
            scenario_names: Vec::new(),
            scenario_weights: Vec::new(),
            scenario_active: Vec::new(),
            task_counts: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn set_scenarios(&mut self, names: Vec<Arc<str>>, weights: Vec<u64>) {
        self.scenario_active = names.iter().map(|_| AtomicU64::new(0)).collect();
        self.scenario_names = names;
        self.scenario_weights = weights;
    }

    /// Counts one task execution for the ratio view.
    pub fn count_task(&self, scenario: &Arc<str>, task: &str) {
        let mut m = self.task_counts.lock();
        if let Some(n) = m.get_mut(&(Arc::clone(scenario), Arc::from(task))) {
            *n += 1;
        } else if m.len() < 10_000 {
            m.insert((Arc::clone(scenario), Arc::from(task)), 1);
        }
    }

    pub(crate) fn drain_task_counts(&self) -> Vec<(String, String, u64)> {
        std::mem::take(&mut *self.task_counts.lock()).into_iter().map(|((s, t), n)| (s.to_string(), t.to_string(), n)).collect()
    }

    pub(crate) fn scenario_users(&self) -> Vec<(String, u64)> {
        self.scenario_names.iter().zip(self.scenario_active.iter()).map(|(n, a)| (n.to_string(), a.load(Ordering::Relaxed))).collect()
    }

    pub fn client(&self) -> Result<reqwest::Client> {
        match &self.shared_client {
            Some(c) => Ok(c.clone()),
            None => {
                let tls = self
                    .tls
                    .get_or_init(|| tls_config(&self.plan).map_err(|e| e.to_string()))
                    .as_ref()
                    .map_err(|e| Error::config(e.clone()))?;
                build_client_with(&self.plan, tls)
            }
        }
    }

    /// Claims one iteration slot; false once the plan's iteration limit is exhausted.
    pub(crate) fn claim_iteration(&self) -> bool {
        let n = self.iterations.fetch_add(1, Ordering::Relaxed);
        match self.iteration_limit {
            Some(limit) if n >= limit => {
                self.limit_reached.store(true, Ordering::Relaxed);
                false
            }
            _ => true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &self,
        shard: usize,
        vu: u64,
        key: &Key,
        latency: Duration,
        ok: bool,
        status: Option<u16>,
        bytes: (u64, u64),
        error: Option<&str>,
    ) {
        self.record_ext(shard, vu, key, latency, ok, status, bytes, error, Extra::default());
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_ext(
        &self,
        shard: usize,
        vu: u64,
        key: &Key,
        latency: Duration,
        ok: bool,
        status: Option<u16>,
        bytes: (u64, u64),
        error: Option<&str>,
        extra: Extra<'_>,
    ) {
        self.recorder.record(shard, Sample { key, latency, ok, bytes_in: bytes.0, bytes_out: bytes.1, error });
        if key.kind == Kind::Request && !self.listeners.is_empty() {
            let ev = RequestEvent {
                vu,
                scenario: &key.scenario,
                method: &key.method,
                name: &key.name,
                status,
                ok,
                latency,
                bytes_in: bytes.0,
                bytes_out: bytes.1,
                error,
                context: extra.context,
                ttfb: extra.ttfb,
            };
            for l in self.listeners.iter() {
                l.on_request(&ev);
            }
        }
    }
}

fn read_pem(path: &std::path::Path, what: &str) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("{what} {}: {e}", path.display())))?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err(Error::config(format!("{what} must be a regular PEM file up to 1 MiB")));
    }
    Ok(std::fs::read(path)?)
}

/// Accepts any server certificate. Used only when a plan sets `target.insecure_tls`.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// TLS settings for the target: the platform trust store plus `ca_cert`, an optional client
/// certificate, or no verification when `insecure_tls` is set. Built once and shared by all users,
/// which keeps per-user memory low.
pub fn tls_config(plan: &Plan) -> Result<rustls::ClientConfig> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let t = &plan.target;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = if t.insecure_tls {
        Arc::new(AcceptAnyCert(Arc::clone(&provider)))
    } else {
        let mut extra = Vec::new();
        if let Some(ca) = &t.ca_cert {
            for c in CertificateDer::pem_slice_iter(&read_pem(ca, "ca_cert")?) {
                extra.push(c.map_err(|e| Error::config(format!("invalid ca_cert: {e}")))?);
            }
            if extra.is_empty() {
                return Err(Error::config("ca_cert contains no certificates"));
            }
        }
        Arc::new(
            rustls_platform_verifier::Verifier::new_with_extra_roots(extra, Arc::clone(&provider))
                .map_err(|e| Error::config(format!("tls trust store: {e}")))?,
        )
    };
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::config(format!("tls configuration: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    match (&t.client_cert, &t.client_key) {
        (Some(cert), Some(key)) => {
            let chain = CertificateDer::pem_slice_iter(&read_pem(cert, "client_cert")?)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::config(format!("invalid client_cert: {e}")))?;
            let key = PrivateKeyDer::from_pem_slice(&read_pem(key, "client_key")?)
                .map_err(|e| Error::config(format!("invalid client_key: {e}")))?;
            builder.with_client_auth_cert(chain, key).map_err(|e| Error::config(format!("invalid client certificate: {e}")))
        }
        _ => Ok(builder.with_no_client_auth()),
    }
}

pub fn build_client(plan: &Plan) -> Result<reqwest::Client> {
    build_client_with(plan, &tls_config(plan)?)
}

/// Builds an HTTP client that uses the given (shared) TLS configuration.
pub fn build_client_with(plan: &Plan, tls: &rustls::ClientConfig) -> Result<reqwest::Client> {
    let t = &plan.target;
    let mut headers = HeaderMap::new();
    for (k, v) in &t.headers {
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| Error::config("invalid header name"))?;
        let value = HeaderValue::from_str(v).map_err(|_| Error::config("invalid header value"))?;
        headers.insert(name, value);
    }
    if !headers.contains_key(http::header::USER_AGENT) {
        headers.insert(http::header::USER_AGENT, HeaderValue::from_static(concat!("ballista/", env!("CARGO_PKG_VERSION"))));
    }
    let mut b = reqwest::Client::builder()
        .tls_backend_preconfigured(tls.clone())
        .default_headers(headers)
        .timeout(t.timeout)
        .connect_timeout(t.connect_timeout)
        .pool_max_idle_per_host(t.pool_idle_per_host)
        .redirect(reqwest::redirect::Policy::limited(10))
        .tcp_nodelay(true)
        .cookie_store(t.cookies && t.client == ClientMode::PerUser);
    if t.http2_prior_knowledge {
        b = b.http2_prior_knowledge();
    }
    if let Some(p) = &t.proxy {
        let mut proxy = reqwest::Proxy::all(p.as_str()).map_err(|e| Error::config(format!("invalid proxy: {e}")))?;
        if let Some(env) = &t.proxy_auth_env {
            let cred = crate::config::secret_from_env(env)?;
            let (user, pass) = cred.split_once(':').ok_or_else(|| Error::config(format!("{env} must contain user:password")))?;
            proxy = proxy.basic_auth(user, pass);
        }
        b = b.proxy(proxy);
    }
    b.build().map_err(|e| Error::config(format!("cannot build http client: {}", error_chain(&e))))
}

pub(crate) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    let mut depth = 0;
    while let Some(s) = src {
        if depth >= 4 {
            break;
        }
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
        depth += 1;
    }
    msg
}

fn classify(e: reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect error"
    } else if e.is_redirect() {
        "redirect error"
    } else if e.is_body() || e.is_decode() {
        "body error"
    } else {
        "request error"
    };
    let e = e.without_url();
    format!("{kind}: {}", error_chain(&e))
}

/// A simulated user. Owns its HTTP client (and cookie jar in per-user mode) and per-user state.
pub struct Vu {
    id: u64,
    pub(crate) ctx: Arc<RunCtx>,
    client: reqwest::Client,
    scenario: Arc<str>,
    shard: usize,
    pub vars: HashMap<String, String>,
    pub(crate) script_state: rhai::Dynamic,
    pub(crate) started: bool,
    pub(crate) task_cursor: usize,
    #[cfg(feature = "mqtt")]
    pub(crate) mqtt: Option<crate::protocols::mqtt::MqttSession>,
    headers: HeaderMap,
    txns: parking_lot::Mutex<Vec<OpenTxn>>,
    state: Option<Box<dyn Any + Send + Sync>>,
    /// Open Socket.IO sessions used by scripts, keyed by path and namespace.
    pub(crate) sio: HashMap<String, crate::socketio::SioConn>,
}

struct OpenTxn {
    name: String,
    start: Instant,
    failed: bool,
}

const MAX_OPEN_TXNS: usize = 16;

impl Vu {
    pub(crate) fn new(id: u64, ctx: Arc<RunCtx>, scenario: Arc<str>) -> Result<Self> {
        let client = ctx.client()?;
        let shard = (id as usize).wrapping_mul(0x9E37_79B9) % ctx.recorder.shard_count();
        let mut map = rhai::Map::new();
        map.insert("vu_id".into(), rhai::Dynamic::from_int(id as rhai::INT));
        map.insert("scenario".into(), rhai::Dynamic::from(scenario.to_string()));
        Ok(Self {
            id,
            ctx,
            client,
            scenario,
            shard,
            vars: HashMap::new(),
            script_state: rhai::Dynamic::from_map(map),
            started: false,
            task_cursor: 0,
            #[cfg(feature = "mqtt")]
            mqtt: None,
            headers: HeaderMap::new(),
            txns: parking_lot::Mutex::new(Vec::new()),
            state: None,
            sio: HashMap::new(),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    pub fn plan(&self) -> &Plan {
        &self.ctx.plan
    }

    pub fn is_cancelled(&self) -> bool {
        self.ctx.cancel.is_cancelled()
    }

    /// Typed per-user state, created on first access.
    pub fn state_mut<T: Default + Send + Sync + 'static>(&mut self) -> &mut T {
        let fresh = !self.state.as_ref().is_some_and(|s| s.is::<T>());
        if fresh {
            self.state = Some(Box::new(T::default()));
        }
        self.state.as_mut().and_then(|s| s.downcast_mut::<T>()).expect("state type verified above")
    }

    pub fn feed(&self, name: &str) -> Option<Arc<FeedRow>> {
        self.ctx.feeders.next(name)
    }

    pub fn request(&self, method: Method, target: impl Into<String>) -> Req<'_> {
        Req::new(self, method, target.into())
    }
    pub fn get(&self, target: impl Into<String>) -> Req<'_> {
        self.request(Method::GET, target)
    }
    pub fn post(&self, target: impl Into<String>) -> Req<'_> {
        self.request(Method::POST, target)
    }
    pub fn put(&self, target: impl Into<String>) -> Req<'_> {
        self.request(Method::PUT, target)
    }
    pub fn patch(&self, target: impl Into<String>) -> Req<'_> {
        self.request(Method::PATCH, target)
    }
    pub fn delete(&self, target: impl Into<String>) -> Req<'_> {
        self.request(Method::DELETE, target)
    }
    pub fn head(&self, target: impl Into<String>) -> Req<'_> {
        self.request(Method::HEAD, target)
    }

    /// Cancellable sleep. Returns `Error::Cancelled` when the run is hard-stopped.
    pub async fn sleep(&self, d: Duration) -> Result<()> {
        tokio::select! {
            () = self.ctx.cancel.cancelled() => Err(Error::Cancelled),
            () = tokio::time::sleep(d) => Ok(()),
        }
    }

    /// Times any fallible future and records it as a request. Use this for custom protocols.
    pub async fn measure<T, E, F>(&self, label: &str, name: &str, fut: F) -> Result<T>
    where
        E: Display,
        F: Future<Output = std::result::Result<T, E>>,
    {
        let key = Key::request(&self.scenario, label, name);
        let start = Instant::now();
        let out = tokio::select! {
            () = self.ctx.cancel.cancelled() => return Err(Error::Cancelled),
            r = fut => r,
        };
        let latency = start.elapsed();
        match out {
            Ok(v) => {
                self.record(&key, latency, true, None, (0, 0), None);
                if self.ctx.trace {
                    println!("[vu {}] {label} {name} ok in {:.1} ms", self.id, latency.as_secs_f64() * 1000.0);
                }
                Ok(v)
            }
            Err(e) => {
                let msg = sanitize(&e.to_string(), 240);
                self.record(&key, latency, false, None, (0, 0), Some(&msg));
                if self.ctx.trace {
                    trace::failure(self.id, latency, &msg);
                }
                Err(Error::Check(msg))
            }
        }
    }

    pub async fn ws_connect(&self, target: &str, name: &str) -> Result<WsConn> {
        WsConn::connect(self, target, name).await
    }

    pub(crate) fn record(&self, key: &Key, latency: Duration, ok: bool, status: Option<u16>, bytes: (u64, u64), err: Option<&str>) {
        self.record_ext(key, latency, ok, status, bytes, err, Extra::default());
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_ext(
        &self,
        key: &Key,
        latency: Duration,
        ok: bool,
        status: Option<u16>,
        bytes: (u64, u64),
        err: Option<&str>,
        extra: Extra<'_>,
    ) {
        if !ok && key.kind == Kind::Request {
            self.fail_open_transactions();
        }
        self.ctx.record_ext(self.shard, self.id, key, latency, ok, status, bytes, err, extra);
    }

    /// Sets a header sent with every subsequent request of this user (e.g. a rotated auth token).
    pub fn set_header(&mut self, name: &str, value: &str) -> Result<()> {
        let n = HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::check("invalid header name"))?;
        let v = HeaderValue::from_str(value).map_err(|_| Error::check("invalid header value"))?;
        self.headers.insert(n, v);
        Ok(())
    }

    pub fn remove_header(&mut self, name: &str) {
        self.headers.remove(name);
    }

    /// Starts a named transaction: a business step spanning several requests, timed as one.
    pub fn txn_begin(&self, name: &str) -> Result<()> {
        if !crate::config::is_valid_name(name) {
            return Err(Error::Script("transaction name must be 1..64 characters of [A-Za-z0-9_.-]".into()));
        }
        let mut txns = self.txns.lock();
        if txns.iter().any(|t| t.name == name) {
            return Err(Error::Script(format!("transaction '{name}' is already open")));
        }
        if txns.len() >= MAX_OPEN_TXNS {
            return Err(Error::Script("too many open transactions".into()));
        }
        txns.push(OpenTxn { name: name.to_owned(), start: Instant::now(), failed: false });
        Ok(())
    }

    /// Ends a transaction; it fails if any request inside it failed.
    pub fn txn_end(&self, name: &str) -> Result<()> {
        let txn = {
            let mut txns = self.txns.lock();
            let pos = txns
                .iter()
                .position(|t| t.name == name)
                .ok_or_else(|| Error::Script(format!("transaction '{}' is not open", sanitize(name, 64))))?;
            txns.remove(pos)
        };
        let key = Key::transaction(&self.scenario, &txn.name);
        let err = txn.failed.then_some("a request inside the transaction failed");
        self.ctx.record(self.shard, self.id, &key, txn.start.elapsed(), !txn.failed, None, (0, 0), err);
        Ok(())
    }

    fn fail_open_transactions(&self) {
        for t in self.txns.lock().iter_mut() {
            t.failed = true;
        }
    }

    /// Closes transactions left open at the end of an iteration as failed.
    pub(crate) fn abandon_transactions(&self, reason: &str) {
        let open: Vec<OpenTxn> = std::mem::take(&mut *self.txns.lock());
        for t in open {
            let key = Key::transaction(&self.scenario, &t.name);
            self.ctx.record(self.shard, self.id, &key, t.start.elapsed(), false, None, (0, 0), Some(reason));
        }
    }

    pub fn shared_get(&self, key: &str) -> Option<serde_json::Value> {
        self.ctx.shared.get(key)
    }

    pub fn shared_set(&self, key: &str, value: serde_json::Value) -> Result<()> {
        self.ctx.shared.set(key, value)
    }

    /// Records a protocol outcome and converts failures into recorded check errors.
    #[cfg_attr(not(feature = "mqtt"), allow(dead_code))]
    pub(crate) fn record_outcome<T>(&self, key: &Key, latency: Duration, res: std::result::Result<T, String>) -> Result<T> {
        match res {
            Ok(v) => {
                self.record(key, latency, true, None, (0, 0), None);
                Ok(v)
            }
            Err(e) => {
                let msg = sanitize(&e, 240);
                self.record(key, latency, false, None, (0, 0), Some(&msg));
                if self.ctx.trace {
                    trace::failure(self.id, latency, &msg);
                }
                Err(Error::Check(msg))
            }
        }
    }

    pub(crate) fn record_error(&self, key: &Key, err: &str) {
        self.ctx.recorder.record_error(self.shard, key, err);
    }

    pub(crate) fn scenario_arc(&self) -> &Arc<str> {
        &self.scenario
    }

    pub(crate) fn shard(&self) -> usize {
        self.shard
    }

    /// Resolves a relative path against base_url, or validates an absolute URL against the host allowlist.
    pub(crate) fn resolve(&self, target: &str) -> Result<Url> {
        let plan = &self.ctx.plan;
        let url = if target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with("ws://")
            || target.starts_with("wss://")
        {
            Url::parse(target).map_err(|e| Error::check(format!("invalid url: {e}")))?
        } else {
            plan.target.base_url.join(target).map_err(|e| Error::check(format!("invalid path: {e}")))?
        };
        if !plan.host_allowed(&url) {
            return Err(Error::check(format!("host '{}' is not in target.allowed_hosts", url.host_str().unwrap_or(""))));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::check("credentials in url are not allowed"));
        }
        Ok(url)
    }
}

enum Expect {
    Default,
    Codes(Vec<u16>),
}

enum ReqBody {
    None,
    Bytes(Bytes, Option<&'static str>),
    Multipart(Option<reqwest::multipart::Form>),
}

#[derive(Clone, Copy)]
enum ReadMode {
    Full,
    Lines(usize),
    Sse(usize),
}

type Validator<'a> = Box<dyn FnOnce(&Resp) -> std::result::Result<(), String> + Send + 'a>;

/// Request builder bound to a virtual user. Every `send` is timed and recorded.
pub struct Req<'a> {
    vu: &'a Vu,
    method: Method,
    target: String,
    name: Option<String>,
    headers: HeaderMap,
    query: Vec<(String, String)>,
    body: ReqBody,
    expect: Expect,
    allow_failure: bool,
    timeout: Option<Duration>,
    validator: Option<Validator<'a>>,
    retries: Option<u32>,
    read: ReadMode,
    context: Option<BTreeMap<String, String>>,
    build_error: Option<String>,
}

const MAX_LINES: usize = 100_000;
const MAX_SSE_EVENTS: usize = 100_000;

impl<'a> Req<'a> {
    fn new(vu: &'a Vu, method: Method, target: String) -> Self {
        Self {
            vu,
            method,
            target,
            name: None,
            headers: HeaderMap::new(),
            query: Vec::new(),
            body: ReqBody::None,
            expect: Expect::Default,
            allow_failure: false,
            timeout: None,
            validator: None,
            retries: None,
            read: ReadMode::Full,
            context: None,
            build_error: None,
        }
    }

    /// Statistics name. Use it to group URLs with dynamic segments.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        match (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
            (Ok(n), Ok(v)) => {
                self.headers.insert(n, v);
            }
            _ => self.build_error = Some(format!("invalid header '{}'", sanitize(name, 64))),
        }
        self
    }

    pub fn bearer(self, token: &str) -> Self {
        let value = format!("Bearer {token}");
        self.header("authorization", &value)
    }

    pub fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(v) => self.body = ReqBody::Bytes(Bytes::from(v), Some("application/json")),
            Err(e) => self.build_error = Some(format!("json encode: {e}")),
        }
        self
    }

    pub fn form<K: AsRef<str>, V: AsRef<str>>(mut self, pairs: &[(K, V)]) -> Self {
        let encoded =
            url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().map(|(k, v)| (k.as_ref(), v.as_ref()))).finish();
        self.body = ReqBody::Bytes(Bytes::from(encoded), Some("application/x-www-form-urlencoded"));
        self
    }

    /// Multipart body. Multipart requests are never retried because the body cannot be replayed.
    pub fn multipart(mut self, form: reqwest::multipart::Form) -> Self {
        self.body = ReqBody::Multipart(Some(form));
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>, content_type: Option<&'static str>) -> Self {
        self.body = ReqBody::Bytes(body.into(), content_type);
        self
    }

    pub fn expect(mut self, status: u16) -> Self {
        self.expect = Expect::Codes(vec![status]);
        self
    }

    pub fn expect_any(mut self, statuses: &[u16]) -> Self {
        self.expect = Expect::Codes(statuses.to_vec());
        self
    }

    /// Inspects the response before it is recorded; returning `Err(reason)` marks the request failed.
    pub fn validate(mut self, f: impl FnOnce(&Resp) -> std::result::Result<(), String> + Send + 'a) -> Self {
        self.validator = Some(Box::new(f));
        self
    }

    /// Return a response even when a check fails (the failure is still recorded).
    pub fn allow_failure(mut self) -> Self {
        self.allow_failure = true;
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// Attaches key/value context passed to listeners (request log, custom hooks).
    pub fn context(mut self, key: &str, value: &str) -> Self {
        let m = self.context.get_or_insert_with(BTreeMap::new);
        if m.len() < 32 {
            m.insert(sanitize(key, 64), sanitize(value, 256));
        }
        self
    }

    /// Overrides `target.retries` for this request.
    pub fn retries(mut self, n: u32) -> Self {
        self.retries = Some(n.min(10));
        self
    }

    /// Reads a streaming body line by line (SSE, NDJSON, chunked feeds) and stops after `max` lines.
    pub fn lines(mut self, max: usize) -> Self {
        self.read = ReadMode::Lines(max.clamp(1, MAX_LINES));
        self
    }

    /// Reads a `text/event-stream` body as server-sent events and stops after `max_events`.
    /// Time to the first event is recorded as `SSE <name>` alongside transactions.
    pub fn sse(mut self, max_events: usize) -> Self {
        self.read = ReadMode::Sse(max_events.clamp(1, MAX_SSE_EVENTS));
        if !self.headers.contains_key(http::header::ACCEPT) {
            self.headers.insert(http::header::ACCEPT, HeaderValue::from_static("text/event-stream"));
        }
        self
    }

    /// Resumes an event stream after the given event id (`Last-Event-ID`).
    pub fn last_event_id(self, id: &str) -> Self {
        self.header("last-event-id", id)
    }

    fn stat_name(&self) -> String {
        if let Some(n) = &self.name {
            return sanitize(n, 128);
        }
        let without_query = self.target.split(['?', '#']).next().unwrap_or("");
        sanitize(without_query, 128)
    }

    pub async fn send(mut self) -> Result<Resp> {
        let vu = self.vu;
        let key = Key::request(vu.scenario_arc(), self.method.as_str(), &self.stat_name());
        if let Some(e) = self.build_error.take() {
            vu.record_error(&key, &e);
            return Err(Error::Check(e));
        }
        let url = match vu.resolve(&self.target) {
            Ok(u) => u,
            Err(e) => {
                vu.record_error(&key, &e.to_string());
                return Err(e);
            }
        };
        let target_cfg = &vu.ctx.plan.target;
        let multipart = matches!(self.body, ReqBody::Multipart(_));
        let retries = if multipart { 0 } else { self.retries.unwrap_or(target_cfg.retries) };
        let tracing_on = vu.ctx.trace;
        if tracing_on {
            let body: &[u8] = match &self.body {
                ReqBody::Bytes(b, _) => b.as_ref(),
                _ => &[],
            };
            let mut traced = url.clone();
            if !self.query.is_empty() {
                traced.query_pairs_mut().extend_pairs(self.query.iter());
            }
            trace::request(&trace::TraceRequest {
                vu: vu.id,
                scenario: vu.scenario(),
                method: self.method.as_str(),
                url: &traced,
                headers: &self.headers,
                body,
            });
        }

        let mut attempt = 0_u32;
        loop {
            let mut headers = vu.headers.clone();
            headers.extend(self.headers.clone());
            let mut rb = vu.client.request(self.method.clone(), url.clone()).headers(headers);
            if !self.query.is_empty() {
                rb = rb.query(&self.query);
            }
            if let Some(t) = self.timeout {
                rb = rb.timeout(t);
            }
            let mut bytes_out = 0_u64;
            match &mut self.body {
                ReqBody::None => {}
                ReqBody::Bytes(body, ct) => {
                    bytes_out = body.len() as u64;
                    if let Some(ct) = ct {
                        rb = rb.header(http::header::CONTENT_TYPE, *ct);
                    }
                    rb = rb.body(body.clone());
                }
                ReqBody::Multipart(form) => {
                    if let Some(f) = form.take() {
                        rb = rb.multipart(f);
                    }
                }
            }
            let outcome = execute(vu, self.read, rb).await?;
            let last = attempt >= retries;
            match outcome {
                Err(msg) => {
                    vu.record(&key, outcome_latency(&msg), false, None, (0, bytes_out), Some(&msg.text));
                    if tracing_on {
                        trace::failure(vu.id, msg.latency, &msg.text);
                    }
                    if last {
                        return Err(Error::Http(msg.text));
                    }
                }
                Ok(mut resp) => {
                    let retryable = target_cfg.retry_on.contains(&resp.status);
                    if retryable && !last {
                        let m = format!("unexpected status {} (retrying)", resp.status);
                        vu.record(&key, resp.latency, false, Some(resp.status), (resp.bytes_in, bytes_out), Some(&m));
                    } else {
                        let status_ok = match &self.expect {
                            Expect::Default => resp.status < 400,
                            Expect::Codes(c) => c.contains(&resp.status),
                        };
                        let mut err_msg = (!status_ok).then(|| format!("unexpected status {}", resp.status));
                        if status_ok
                            && let Some(v) = self.validator.take()
                            && let Err(reason) = v(&resp)
                        {
                            err_msg = Some(format!("validation failed: {}", sanitize(&reason, 200)));
                        }
                        resp.ok = err_msg.is_none();
                        resp.attempts = attempt + 1;
                        vu.record_ext(
                            &key,
                            resp.latency,
                            resp.ok,
                            Some(resp.status),
                            (resp.bytes_in, bytes_out),
                            err_msg.as_deref(),
                            Extra { context: self.context.as_ref(), ttfb: Some(resp.ttfb) },
                        );
                        if let ReadMode::Sse(_) = self.read
                            && status_ok
                        {
                            let sse_key = Key::derived(vu.scenario_arc(), "SSE", &self.stat_name());
                            match resp.events.first() {
                                Some(first) => vu.record(&sse_key, first.at, true, Some(resp.status), (0, 0), None),
                                None => vu.record(&sse_key, resp.latency, false, Some(resp.status), (0, 0), Some("no events received")),
                            }
                        }
                        if tracing_on {
                            trace::response(vu.id, resp.status, resp.latency, &resp.headers, &resp.body, resp.ok, err_msg.as_deref());
                        }
                        return match err_msg {
                            Some(m) if !self.allow_failure => Err(Error::Check(m)),
                            _ => Ok(resp),
                        };
                    }
                }
            }
            let backoff = target_cfg.retry_backoff.saturating_mul(1 << attempt.min(6));
            vu.sleep(backoff).await?;
            attempt += 1;
        }
    }
}

/// Sends once and reads the body. Outer error = cancelled; inner error = transport failure.
async fn execute(vu: &Vu, read: ReadMode, rb: reqwest::RequestBuilder) -> Result<std::result::Result<Resp, TransportError>> {
    {
        let max_body = vu.ctx.plan.target.max_body_bytes;
        let start = Instant::now();
        let fut = async move {
            let mut resp = rb.send().await?;
            let ttfb = start.elapsed();
            let status = resp.status().as_u16();
            let headers = resp.headers().clone();
            let mut buf = BytesMut::new();
            let mut total = 0_u64;
            let mut truncated = false;
            let mut lines = Vec::new();
            let mut pending = Vec::new();
            let mut events = Vec::new();
            let mut parser = crate::sse::SseParser::default();
            let mut budget = max_body;
            while let Some(chunk) = resp.chunk().await? {
                total += chunk.len() as u64;
                if let ReadMode::Sse(max) = read {
                    parser.feed(&chunk, start.elapsed(), &mut events, max, &mut budget);
                    truncated |= parser.overflow;
                    if events.len() >= max {
                        break;
                    }
                } else if let ReadMode::Lines(max) = read {
                    pending.extend_from_slice(&chunk);
                    while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=pos).collect();
                        let text = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_owned();
                        lines.push(text);
                        if lines.len() >= max {
                            break;
                        }
                    }
                    if pending.len() > max_body {
                        truncated = true;
                        pending.clear();
                    }
                    if lines.len() >= max {
                        break;
                    }
                } else if buf.len() + chunk.len() <= max_body {
                    buf.extend_from_slice(&chunk);
                } else {
                    truncated = true;
                }
            }
            if let ReadMode::Lines(max) = read
                && !pending.is_empty()
                && lines.len() < max
            {
                lines.push(String::from_utf8_lossy(&pending).trim_end_matches('\r').to_owned());
            }
            Ok::<_, reqwest::Error>(Resp {
                status,
                headers,
                body: buf.freeze(),
                latency: start.elapsed(),
                ttfb,
                ok: true,
                truncated,
                lines,
                bytes_in: total,
                attempts: 1,
                events,
                last_event_id: parser.last_id,
                retry_ms: parser.retry_ms,
            })
        };
        let result = tokio::select! {
            () = vu.ctx.cancel.cancelled() => return Err(Error::Cancelled),
            r = fut => r,
        };
        Ok(result.map_err(|e| TransportError { latency: start.elapsed(), text: classify(e) }))
    }
}

struct TransportError {
    latency: Duration,
    text: String,
}

fn outcome_latency(e: &TransportError) -> Duration {
    e.latency
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub latency: Duration,
    /// Time until response headers arrived.
    pub ttfb: Duration,
    pub ok: bool,
    pub truncated: bool,
    /// Lines read in streaming mode (`Req::lines`).
    pub lines: Vec<String>,
    pub bytes_in: u64,
    pub attempts: u32,
    /// Server-sent events read in SSE mode (`Req::sse`).
    pub events: Vec<crate::sse::SseEvent>,
    /// Last event id seen, for resuming with `Req::last_event_id`.
    pub last_event_id: Option<String>,
    /// Reconnection delay requested by the server (`retry:` field).
    pub retry_ms: Option<u64>,
}

impl Resp {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::check(format!("invalid json response: {e}")))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Fails the iteration with an assertion error when `cond` is false.
    pub fn check(&self, cond: bool, what: &str) -> Result<()> {
        if cond { Ok(()) } else { Err(Error::Assertion(sanitize(what, 200))) }
    }
}

impl Vu {
    /// JSON REST call: serializes `body` (if any), expects 2xx and deserializes the response.
    pub async fn rest<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        name: &str,
    ) -> Result<T> {
        let mut req = self.request(method, path).name(name).header("accept", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        req.send().await?.json()
    }
}
