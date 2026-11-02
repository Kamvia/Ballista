// File: ballista/src/scenario/script.rs
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use rhai::{AST, Array, CallFnOptions, Dynamic, Engine, EvalAltResult, FnPtr, INT, Map, Scope};
use serde::Deserialize;

use super::feeder::Feeders;
use super::{BoxFuture, Scenario};
use crate::config::Plan;
use crate::error::{Error, Result, sanitize};
use crate::shared::SharedStore;
use crate::vu::{Resp, Vu};
use futures_util::StreamExt;

const MAX_SCRIPT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_STEPS: i64 = 1000;
const MAX_SLEEP_MS: u64 = 600_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const SCRIPT_BODY_MAX: usize = 4 * 1024 * 1024;
const MAX_WEIGHT: INT = 10_000;
const ENV_PREFIX: &str = "BALLISTA_VAR_";
const MAX_BATCH: usize = 100;
const MAX_SSE_RECONNECTS: u32 = 100;
const MAX_SSE_RETRY_MS: u64 = 30_000;
const MAX_SIO_SESSIONS: usize = 16;

type FnResult<T> = std::result::Result<T, Box<EvalAltResult>>;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ExpectSpec {
    One(i64),
    Many(Vec<i64>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptRequest {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    query: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    json: Option<serde_json::Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    form: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    expect: Option<ExpectSpec>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    continue_on_failure: bool,
    #[serde(default)]
    ws: Option<String>,
    #[serde(default)]
    send: Option<String>,
    #[serde(default)]
    sleep_ms: Option<u64>,
    #[serde(default)]
    multipart: Option<BTreeMap<String, MultipartPart>>,
    #[serde(default)]
    lines: Option<usize>,
    #[serde(default)]
    retries: Option<u32>,
    #[serde(default)]
    grpc: Option<String>,
    #[serde(default)]
    message: Option<serde_json::Value>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    #[serde(default)]
    mqtt: Option<String>,
    #[serde(default)]
    payload: Option<String>,
    #[serde(default)]
    qos: Option<u8>,
    #[serde(default)]
    retain: bool,
    #[serde(default)]
    reply: Option<String>,
    #[serde(default)]
    kafka: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    partition: Option<i32>,
    #[serde(default)]
    sql: Option<String>,
    #[serde(default)]
    params: Vec<serde_json::Value>,
    #[serde(default)]
    tcp: Option<String>,
    #[serde(default)]
    udp: Option<String>,
    #[serde(default)]
    read: Option<String>,
    #[serde(default)]
    read_bytes: Option<usize>,
    #[serde(default)]
    read_until: Option<String>,
    #[serde(default)]
    context: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    sse: Option<usize>,
    #[serde(default)]
    reconnects: Option<u32>,
    #[serde(default)]
    last_event_id: Option<String>,
    #[serde(default)]
    max_messages: Option<usize>,
    #[serde(default)]
    socketio: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    emit: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
    #[serde(default)]
    ack: bool,
    #[serde(default)]
    wait: Option<String>,
    #[serde(default)]
    auth: Option<serde_json::Value>,
    #[serde(default)]
    close: bool,
}

impl ScriptRequest {
    fn empty() -> Self {
        rhai::serde::from_dynamic(&Dynamic::from_map(Map::new())).expect("empty request map deserializes")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum MultipartPart {
    Text(serde_json::Value),
    File {
        file: String,
        #[serde(default)]
        filename: Option<String>,
        #[serde(default)]
        content_type: Option<String>,
    },
}

const MAX_UPLOAD: u64 = 64 * 1024 * 1024;

/// Resolves an upload path and confines it to the plan directory (no traversal outside it).
async fn upload_part(base: &Path, rel: &str, filename: Option<String>, ct: Option<String>) -> Result<reqwest::multipart::Part> {
    let root = tokio::fs::canonicalize(base).await?;
    let full =
        tokio::fs::canonicalize(root.join(rel)).await.map_err(|_| script_err(format!("upload file '{}' not found", sanitize(rel, 120))))?;
    if !full.starts_with(&root) {
        return Err(script_err("upload files must live inside the plan directory"));
    }
    let meta = tokio::fs::metadata(&full).await?;
    if !meta.is_file() || meta.len() > MAX_UPLOAD {
        return Err(script_err("upload must be a regular file up to 64 MiB"));
    }
    let data = tokio::fs::read(&full).await?;
    let name = filename.or_else(|| full.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "file".to_owned());
    let mime = ct.unwrap_or_else(|| mime_guess::from_path(&full).first_or_octet_stream().to_string());
    reqwest::multipart::Part::bytes(data)
        .file_name(sanitize(&name, 128))
        .mime_str(&mime)
        .map_err(|_| script_err("invalid upload content type"))
}

fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub struct ScriptScenario {
    name: Arc<str>,
    engine: Arc<Engine>,
    ast: Arc<AST>,
    tasks: Vec<String>,
    cumulative: Vec<u64>,
    total: u64,
    sequential: bool,
    has_before_request: bool,
    has_on_start: bool,
    has_on_stop: bool,
}

fn script_err(e: impl std::fmt::Display) -> Error {
    Error::Script(sanitize(&e.to_string(), 600))
}

impl ScriptScenario {
    pub fn load(
        name: &str,
        path: &Path,
        feeders: Arc<Feeders>,
        shared: Arc<SharedStore>,
        plan: &Plan,
        scenario_tags: &[String],
    ) -> Result<Self> {
        let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("script {}: {e}", path.display())))?;
        if !meta.is_file() || meta.len() > MAX_SCRIPT_BYTES {
            return Err(Error::config(format!("script {} must be a regular file up to 2 MiB", path.display())));
        }
        let src = std::fs::read_to_string(path)?;
        let filter = |task_tags: &[String]| plan.tags_allowed(scenario_tags.iter().chain(task_tags.iter()).map(String::as_str));
        Self::from_source(name, &src, feeders, shared, &filter).map_err(|e| Error::config(format!("script {}: {e}", path.display())))
    }

    /// Compiles a script. `allowed` decides, from a task's tags, whether it takes part in the run.
    pub fn from_source(
        name: &str,
        src: &str,
        feeders: Arc<Feeders>,
        shared: Arc<SharedStore>,
        allowed: &dyn Fn(&[String]) -> bool,
    ) -> Result<Self> {
        let engine = Arc::new(build_engine(name, feeders, shared));
        let ast = engine.compile(src).map_err(script_err)?;
        let fn_arity = |fname: &str| ast.iter_functions().find(|f| f.name == fname).map(|f| f.params.len());

        let mut declared: Vec<(String, u64, Vec<String>)> = Vec::new();
        let mut sequential = false;
        if fn_arity("tasks") == Some(0) {
            let out: Dynamic =
                engine.call_fn::<Dynamic>(&mut Scope::new(), &ast, "tasks", ()).map_err(|e| script_err(format!("tasks(): {e}")))?;
            if out.is_array() {
                sequential = true;
                for item in out.cast::<Array>() {
                    declared.push(parse_task_entry(item, None)?);
                }
            } else if out.is_map() {
                for (k, v) in out.cast::<Map>() {
                    declared.push(parse_task_entry(v, Some(k.to_string()))?);
                }
            } else {
                return Err(script_err("tasks() must return a map (weighted) or an array (sequential)"));
            }
        } else if fn_arity("run") == Some(2) {
            declared.push(("run".to_owned(), 1, Vec::new()));
        } else {
            return Err(script_err("script must define tasks() or run(step, res)"));
        }
        if declared.is_empty() {
            return Err(script_err("tasks() returned no tasks"));
        }
        for (t, _, _) in &declared {
            if fn_arity(t) != Some(2) {
                return Err(script_err(format!("task function '{t}(step, res)' is not defined")));
            }
        }
        let has_on_start = match fn_arity("on_start") {
            Some(2) => true,
            None => false,
            Some(_) => return Err(script_err("on_start must take (step, res)")),
        };
        let has_before_request = match fn_arity("before_request") {
            Some(1) => true,
            None => false,
            Some(_) => return Err(script_err("before_request must take (req)")),
        };
        let has_on_stop = match fn_arity("on_stop") {
            Some(2) => true,
            None => false,
            Some(_) => return Err(script_err("on_stop must take (step, res)")),
        };
        let mut tasks = Vec::new();
        let mut cumulative = Vec::new();
        let mut acc = 0;
        for (t, w, tags) in declared {
            if !allowed(&tags) {
                continue;
            }
            acc += w;
            cumulative.push(acc);
            tasks.push(t);
        }
        Ok(Self {
            name: Arc::from(name),
            engine,
            ast: Arc::new(ast),
            tasks,
            cumulative,
            total: acc,
            sequential,
            has_before_request,
            has_on_start,
            has_on_stop,
        })
    }

    pub fn task_names(&self) -> &[String] {
        &self.tasks
    }

    fn pick_task(&self, vu: &mut Vu) -> &str {
        if self.sequential {
            let t = &self.tasks[vu.task_cursor % self.tasks.len()];
            vu.task_cursor = vu.task_cursor.wrapping_add(1);
            return t;
        }
        let r = rand::random_range(0..self.total);
        &self.tasks[self.cumulative.partition_point(|&c| c <= r)]
    }

    /// Runs a `check` closure from a request map: `true`/`()` passes, `false` or a string fails.
    fn run_check(&self, f: &FnPtr, res: &Dynamic) -> std::result::Result<(), String> {
        match f.call::<Dynamic>(&self.engine, &self.ast, (res.clone(),)) {
            Ok(v) if v.is_unit() => Ok(()),
            Ok(v) => match v.as_bool() {
                Ok(true) => Ok(()),
                Ok(false) => Err("check returned false".into()),
                Err(_) => match v.into_immutable_string() {
                    Ok(s) => Err(s.to_string()),
                    Err(_) => Err("check must return bool, string or ()".into()),
                },
            },
            Err(e) => Err(sanitize(&e.to_string(), 200)),
        }
    }

    fn call(&self, vu: &mut Vu, func: &str, step: INT, last: Dynamic) -> Result<Dynamic> {
        let mut scope = Scope::new();
        let opts = CallFnOptions::new().eval_ast(false).rewind_scope(true).bind_this_ptr(&mut vu.script_state);
        self.engine
            .call_fn_with_options::<Dynamic>(opts, &mut scope, &self.ast, func, (step, last))
            .map_err(|e| script_err(format!("{}::{func}: {e}", self.name)))
    }

    /// Applies the script's `before_request(req)` hook (with `this` bound) to a request map.
    fn hook(&self, vu: &mut Vu, map: Map) -> Result<Map> {
        if !self.has_before_request {
            return Ok(map);
        }
        let mut scope = Scope::new();
        let opts = CallFnOptions::new().eval_ast(false).rewind_scope(true).bind_this_ptr(&mut vu.script_state);
        let out: Dynamic = self
            .engine
            .call_fn_with_options(opts, &mut scope, &self.ast, "before_request", (Dynamic::from_map(map),))
            .map_err(|e| script_err(format!("{}::before_request: {e}", self.name)))?;
        out.try_cast::<Map>().ok_or_else(|| script_err("before_request must return the request map"))
    }

    fn parse_request(&self, mut map: Map, ctx: &str) -> Result<(ScriptRequest, Option<FnPtr>)> {
        let check = match map.remove("check") {
            Some(c) => Some(c.try_cast::<FnPtr>().ok_or_else(|| script_err(format!("{ctx}: check must be a function")))?),
            None => None,
        };
        let req: ScriptRequest =
            rhai::serde::from_dynamic(&Dynamic::from_map(map)).map_err(|e| script_err(format!("{ctx}: invalid request map: {e}")))?;
        if req.sleep_ms.is_some() {
            return Err(script_err(format!("{ctx}: sleep must be a step of its own")));
        }
        Ok((req, check))
    }

    async fn run_task(&self, vu: &mut Vu, func: &str) -> Result<()> {
        let mut last = Dynamic::UNIT;
        for step in 0..MAX_STEPS {
            let out = self.call(vu, func, step, std::mem::take(&mut last))?;
            if out.is_unit() {
                return Ok(());
            }
            let mut map =
                out.try_cast::<Map>().ok_or_else(|| script_err(format!("{func} step {step}: must return a request map or ()")))?;
            let ctx = format!("{func} step {step}");
            if map.len() == 1 {
                if let Some(n) = map.get("txn_begin") {
                    vu.txn_begin(&n.to_string())?;
                    continue;
                }
                if let Some(n) = map.get("txn_end") {
                    vu.txn_end(&n.to_string())?;
                    continue;
                }
            }
            if let Some(items) = map.remove("batch") {
                last = self.run_batch(vu, items, map, &ctx).await?;
                continue;
            }
            if map.len() == 1
                && let Some(ms) = map.get("sleep_ms")
            {
                let ms = u64::try_from(ms.as_int().unwrap_or(0)).unwrap_or(0);
                vu.sleep(Duration::from_millis(ms.min(MAX_SLEEP_MS))).await?;
                continue;
            }
            let map = self.hook(vu, map)?;
            let (req, check) = self.parse_request(map, &ctx)?;
            last = self.perform(vu, req, check.as_ref()).await?;
        }
        Err(script_err(format!("{func} exceeded {MAX_STEPS} steps")))
    }

    /// Runs HTTP requests concurrently (like a browser loading page resources) and times the whole batch.
    async fn run_batch(&self, vu: &mut Vu, items: Dynamic, opts: Map, ctx: &str) -> Result<Dynamic> {
        let items = items.try_cast::<Array>().ok_or_else(|| script_err(format!("{ctx}: batch must be an array")))?;
        if items.is_empty() || items.len() > MAX_BATCH {
            return Err(script_err(format!("{ctx}: batch must hold 1..{MAX_BATCH} requests")));
        }
        let concurrency = opts.get("concurrency").and_then(|c| c.as_int().ok()).map_or(6, |c| usize::try_from(c.clamp(1, 64)).unwrap_or(6));
        let name = opts.get("name").map_or_else(|| "batch".to_owned(), |n| sanitize(&n.to_string(), 64));
        if !crate::config::is_valid_name(&name) {
            return Err(script_err(format!("{ctx}: batch name must be 1..64 characters of [A-Za-z0-9_.-]")));
        }
        let mut parsed = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            let map = item.try_cast::<Map>().ok_or_else(|| script_err(format!("{ctx}: batch item {i} must be a map")))?;
            let map = self.hook(vu, map)?;
            parsed.push(self.parse_request(map, &format!("{ctx} batch[{i}]"))?);
        }
        let vu_ref: &Vu = vu;
        let mut reqs = Vec::with_capacity(parsed.len());
        for (req, check) in &mut parsed {
            if req.ws.is_some()
                || req.socketio.is_some()
                || req.grpc.is_some()
                || req.mqtt.is_some()
                || req.kafka.is_some()
                || req.sql.is_some()
                || req.tcp.is_some()
                || req.udp.is_some()
            {
                return Err(script_err(format!("{ctx}: batch supports HTTP requests only")));
            }
            let cof = req.continue_on_failure;
            let taken = std::mem::replace(req, ScriptRequest::empty());
            reqs.push((self.build_http(vu_ref, taken, check.as_ref()).await?, cof));
        }
        let start = std::time::Instant::now();
        let futs: Vec<super::BoxFuture<'_, (Result<Resp>, bool)>> = reqs.into_iter().map(|(req, cof)| send_tagged(req, cof)).collect();
        let results: Vec<(Result<Resp>, bool)> = futures_util::stream::iter(futs).buffered(concurrency).collect().await;
        let elapsed = start.elapsed();
        let mut all_ok = true;
        let mut fatal = None;
        let mut out = Array::with_capacity(results.len());
        for (res, cof) in results {
            match res {
                Ok(resp) => {
                    all_ok &= resp.ok;
                    if !resp.ok && !cof && fatal.is_none() {
                        fatal = Some(Error::Check(format!("unexpected status {}", resp.status)));
                    }
                    out.push(resp_to_dynamic(&resp));
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => {
                    all_ok = false;
                    let msg = e.to_string();
                    if !cof && fatal.is_none() {
                        fatal = Some(e);
                    }
                    out.push(failure_dynamic(&msg));
                }
            }
        }
        let key = crate::metrics::Key::transaction(vu.scenario_arc(), &name);
        let err = (!all_ok).then_some("a request in the batch failed");
        vu.record(&key, elapsed, all_ok, None, (0, 0), err);
        if let Some(e) = fatal {
            return Err(e);
        }
        let mut m = Map::new();
        m.insert("ok".into(), Dynamic::from_bool(all_ok));
        m.insert("latency_ms".into(), Dynamic::from_float(elapsed.as_secs_f64() * 1000.0));
        m.insert("results".into(), Dynamic::from_array(out));
        Ok(Dynamic::from_map(m))
    }
}

impl Scenario for ScriptScenario {
    fn prepare(&self, _plan: &Plan, _scenario_tags: &[String]) -> Result<bool> {
        Ok(!self.tasks.is_empty())
    }

    fn on_start<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { if self.has_on_start { self.run_task(vu, "on_start").await } else { Ok(()) } })
    }

    fn iteration<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let task = self.pick_task(vu).to_owned();
            let scenario = std::sync::Arc::clone(vu.scenario_arc());
            vu.ctx.count_task(&scenario, &task);
            self.run_task(vu, &task).await
        })
    }

    fn on_stop<'a>(&'a self, vu: &'a mut Vu) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { if self.has_on_stop { self.run_task(vu, "on_stop").await } else { Ok(()) } })
    }
}

impl ScriptScenario {
    async fn perform(&self, vu: &mut Vu, mut r: ScriptRequest, check: Option<&FnPtr>) -> Result<Dynamic> {
        let proto = crate::protocols::script::ProtoRequest {
            grpc: r.grpc.take(),
            message: r.message.take(),
            metadata: std::mem::take(&mut r.metadata),
            mqtt: r.mqtt.take(),
            payload: r.payload.take(),
            qos: r.qos,
            retain: r.retain,
            reply: r.reply.take(),
            kafka: r.kafka.take(),
            key: r.key.take(),
            value: r.value.take(),
            partition: r.partition,
            sql: r.sql.take(),
            params: std::mem::take(&mut r.params),
            tcp: r.tcp.take(),
            udp: r.udp.take(),
            send: r.send.clone(),
            read: r.read.take(),
            read_bytes: r.read_bytes,
            read_until: r.read_until.take(),
            name: r.name.clone(),
            timeout: r.timeout_ms.map(|ms| Duration::from_millis(ms.clamp(1, MAX_TIMEOUT_MS))),
            max_messages: r.max_messages,
        };
        if proto.is_set() {
            let continue_on_failure = r.continue_on_failure;
            return match crate::protocols::script::dispatch(vu, proto).await {
                Ok(Some(d)) => Ok(d),
                Ok(None) => Err(script_err("unrecognised protocol request")),
                Err(Error::Check(msg) | Error::Http(msg)) if continue_on_failure => Ok(failure_dynamic(&msg)),
                Err(e) => Err(e),
            };
        }
        if let Some(path) = r.socketio.take() {
            return perform_sio(vu, path, r).await;
        }
        if r.sse.is_some() {
            return self.perform_sse(vu, r, check).await;
        }
        if let Some(target) = r.ws {
            return perform_ws(vu, &target, r.send.as_deref().unwrap_or(""), r.name.as_deref(), r.timeout_ms, r.continue_on_failure).await;
        }
        let continue_on_failure = r.continue_on_failure;
        let req = self.build_http(vu, r, check).await?;
        match req.send().await {
            Ok(resp) => {
                if !resp.ok && !continue_on_failure {
                    return Err(Error::Check(format!("unexpected status {}", resp.status)));
                }
                Ok(resp_to_dynamic(&resp))
            }
            Err(Error::Http(msg)) if continue_on_failure => Ok(failure_dynamic(&msg)),
            Err(e) => Err(e),
        }
    }

    /// Reads server-sent events, reconnecting with `Last-Event-ID` until `sse` events arrived or
    /// `reconnects` is exhausted. Each connection is recorded; time to first event is recorded as `SSE`.
    async fn perform_sse(&self, vu: &mut Vu, mut r: ScriptRequest, check: Option<&FnPtr>) -> Result<Dynamic> {
        let max = r.sse.take().unwrap_or(1).clamp(1, 100_000);
        let reconnects = r.reconnects.take().unwrap_or(0).min(MAX_SSE_RECONNECTS);
        let cof = r.continue_on_failure;
        let mut last_id = r.last_event_id.take();
        let mut events: Vec<crate::sse::SseEvent> = Vec::new();
        let mut attempt = 0;
        let last = loop {
            let mut req = self.build_http(vu, r.clone(), check).await?.sse(max - events.len());
            if let Some(id) = &last_id {
                req = req.last_event_id(id);
            }
            let resp = match req.send().await {
                Ok(resp) => resp,
                Err(Error::Http(msg) | Error::Check(msg)) if cof => return Ok(failure_dynamic(&msg)),
                Err(e) => return Err(e),
            };
            if !resp.ok {
                if !cof {
                    return Err(Error::Check(format!("unexpected status {}", resp.status)));
                }
                break resp;
            }
            events.extend(resp.events.iter().cloned());
            if resp.last_event_id.is_some() {
                last_id.clone_from(&resp.last_event_id);
            }
            if events.len() >= max || attempt >= reconnects {
                break resp;
            }
            attempt += 1;
            let delay = resp.retry_ms.unwrap_or(1000).min(MAX_SSE_RETRY_MS);
            vu.sleep(Duration::from_millis(delay)).await?;
        };
        let mut out = resp_to_dynamic(&last);
        if let Some(m) = out.write_lock::<Map>().as_deref_mut() {
            let arr: Array = events.iter().map(sse_event_dynamic).collect();
            m.insert("events".into(), Dynamic::from_array(arr));
            m.insert("reconnects".into(), Dynamic::from_int(INT::from(attempt)));
            m.insert("last_event_id".into(), last_id.map_or(Dynamic::UNIT, Dynamic::from));
        }
        Ok(out)
    }

    /// Builds (but does not send) an HTTP request from a script map.
    async fn build_http<'a>(&'a self, vu: &'a Vu, r: ScriptRequest, check: Option<&'a FnPtr>) -> Result<crate::vu::Req<'a>> {
        let target = match (r.url, r.path) {
            (Some(u), None) => u,
            (None, Some(p)) => p,
            _ => return Err(script_err("request requires exactly one of path or url")),
        };
        let method_raw = r.method.unwrap_or_else(|| "GET".to_owned()).to_ascii_uppercase();
        if method_raw.is_empty() || method_raw.len() > 16 || !method_raw.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(script_err("invalid http method"));
        }
        let method = Method::from_bytes(method_raw.as_bytes()).map_err(|_| script_err("invalid http method"))?;
        let set_body =
            u8::from(r.json.is_some()) + u8::from(r.body.is_some()) + u8::from(r.form.is_some()) + u8::from(r.multipart.is_some());
        if set_body > 1 {
            return Err(script_err("request may set only one of json, body, form or multipart"));
        }

        let mut req = vu.request(method, target).allow_failure();
        if let Some(n) = r.name {
            req = req.name(n);
        }
        for (k, v) in &r.headers {
            req = req.header(k, &scalar(v));
        }
        for (k, v) in &r.query {
            req = req.query(k.clone(), scalar(v));
        }
        if let Some(j) = &r.json {
            req = req.json(j);
        } else if let Some(b) = r.body {
            req = req.body(b, None);
        } else if let Some(f) = &r.form {
            let pairs: Vec<(String, String)> = f.iter().map(|(k, v)| (k.clone(), scalar(v))).collect();
            req = req.form(&pairs);
        } else if let Some(parts) = r.multipart {
            let mut form = reqwest::multipart::Form::new();
            for (field, part) in parts {
                form = match part {
                    MultipartPart::Text(v) => form.text(field, scalar(&v)),
                    MultipartPart::File { file, filename, content_type } => {
                        form.part(field, upload_part(&vu.plan().base_dir, &file, filename, content_type).await?)
                    }
                };
            }
            req = req.multipart(form);
        }
        if let Some(n) = r.lines {
            req = req.lines(n);
        }
        if let Some(n) = r.retries {
            req = req.retries(n);
        }
        match r.expect {
            Some(ExpectSpec::One(c)) => req = req.expect(status_code(c)?),
            Some(ExpectSpec::Many(cs)) => {
                let codes = cs.into_iter().map(status_code).collect::<Result<Vec<u16>>>()?;
                req = req.expect_any(&codes);
            }
            None => {}
        }
        if let Some(ms) = r.timeout_ms {
            req = req.timeout(Duration::from_millis(ms.clamp(1, MAX_TIMEOUT_MS)));
        }
        if let Some(f) = check {
            req = req.validate(move |resp| self.run_check(f, &resp_to_dynamic(resp)));
        }
        for (k, v) in &r.context {
            req = req.context(k, &scalar(v));
        }
        Ok(req)
    }
}

fn sse_event_dynamic(e: &crate::sse::SseEvent) -> Dynamic {
    let mut m = Map::new();
    m.insert("event".into(), Dynamic::from(e.event.clone()));
    m.insert("data".into(), Dynamic::from(e.data.clone()));
    m.insert("id".into(), e.id.clone().map_or(Dynamic::UNIT, Dynamic::from));
    m.insert("at_ms".into(), Dynamic::from_float(e.at.as_secs_f64() * 1000.0));
    let json =
        serde_json::from_str::<serde_json::Value>(&e.data).ok().and_then(|v| rhai::serde::to_dynamic(&v).ok()).unwrap_or(Dynamic::UNIT);
    m.insert("json".into(), json);
    Dynamic::from_map(m)
}

/// Socket.IO step on a per-user session that stays open across steps and iterations.
async fn perform_sio(vu: &mut Vu, path: String, r: ScriptRequest) -> Result<Dynamic> {
    let ns = r.namespace.clone().unwrap_or_else(|| "/".to_owned());
    let auth_id = r.auth.as_ref().map_or_else(String::new, |a| {
        use sha2::Digest as _;
        hex::encode(&sha2::Sha256::digest(a.to_string().as_bytes())[..8])
    });
    let session = format!("{path}|{ns}|{auth_id}");
    let name = r.name.clone().unwrap_or_else(|| path.clone());
    let timeout = r.timeout_ms.map(|ms| Duration::from_millis(ms.clamp(1, MAX_TIMEOUT_MS)));
    let cof = r.continue_on_failure;
    let soft = |e: Error| match e {
        Error::Ws(msg) | Error::Check(msg) if cof => Ok(failure_dynamic(&msg)),
        e => Err(e),
    };
    if r.ack && (r.emit.is_none() || r.wait.is_some()) {
        return Err(script_err("socketio: ack requires emit and cannot be combined with wait"));
    }
    let mut conn = match vu.sio.remove(&session) {
        Some(c) => c,
        None if r.close => return Ok(Dynamic::from_map(with(Map::new(), "ok", Dynamic::TRUE))),
        None => {
            if vu.sio.len() >= MAX_SIO_SESSIONS {
                return Err(script_err(format!("socketio: at most {MAX_SIO_SESSIONS} open sessions per user")));
            }
            match crate::socketio::SioConn::connect(vu, &path, &ns, r.auth.as_ref(), &name).await {
                Ok(c) => c,
                Err(e) => return soft(e),
            }
        }
    };
    if r.close {
        conn.close().await;
        return Ok(Dynamic::from_map(with(Map::new(), "ok", Dynamic::TRUE)));
    }
    let data = r.data.as_ref();
    let res = match (r.emit.as_deref(), r.wait.as_deref()) {
        (Some(e), None) if r.ack => conn.emit_ack(vu, e, data, &name, timeout).await,
        (Some(e), None) => conn.emit(vu, e, data, &name).await.map(|()| Vec::new()),
        (emit, Some(w)) => conn.wait_event(vu, w, emit.map(|e| (e, data)), &name, timeout).await,
        (None, None) => Ok(Vec::new()),
    };
    match res {
        Ok(mut args) => {
            vu.sio.insert(session, conn);
            let json = match args.len() {
                0 => serde_json::Value::Null,
                1 => args.remove(0),
                _ => serde_json::Value::Array(args),
            };
            let mut m = with(Map::new(), "ok", Dynamic::TRUE);
            m.insert("json".into(), rhai::serde::to_dynamic(&json).unwrap_or(Dynamic::UNIT));
            Ok(Dynamic::from_map(m))
        }
        Err(e) => soft(e),
    }
}

fn send_tagged(req: crate::vu::Req<'_>, cof: bool) -> super::BoxFuture<'_, (Result<Resp>, bool)> {
    Box::pin(async move { (req.send().await, cof) })
}

async fn perform_ws(
    vu: &mut Vu,
    target: &str,
    send: &str,
    name: Option<&str>,
    timeout_ms: Option<u64>,
    continue_on_failure: bool,
) -> Result<Dynamic> {
    let name = name.unwrap_or(target);
    let timeout = timeout_ms.map(|ms| Duration::from_millis(ms.clamp(1, MAX_TIMEOUT_MS)));
    let res = async {
        let mut conn = vu.ws_connect(target, name).await?;
        let reply = conn.request(name, send, timeout).await;
        conn.close().await;
        reply
    }
    .await;
    match res {
        Ok(text) => {
            let mut m = Map::new();
            m.insert("ok".into(), Dynamic::TRUE);
            m.insert("status".into(), Dynamic::from_int(101));
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                && let Ok(d) = rhai::serde::to_dynamic(&v)
            {
                m.insert("json".into(), d);
            }
            m.insert("body".into(), Dynamic::from(text));
            Ok(Dynamic::from_map(m))
        }
        Err(Error::Ws(msg)) if continue_on_failure => Ok(failure_dynamic(&msg)),
        Err(e) => Err(e),
    }
}

fn tags_of(v: Option<&Dynamic>) -> Result<Vec<String>> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let arr = v.clone().try_cast::<Array>().ok_or_else(|| script_err("task tags must be an array of strings"))?;
    arr.into_iter()
        .map(|t| {
            let s = t.into_immutable_string().map_err(|_| script_err("task tags must be strings"))?;
            if crate::config::is_valid_name(&s) { Ok(s.to_string()) } else { Err(script_err(format!("invalid tag '{s}'"))) }
        })
        .collect()
}

/// Parses one entry of `tasks()`: `name: weight`, `name: #{ weight, tags }`, `"name"` or `#{ name, tags }`.
fn parse_task_entry(item: Dynamic, key: Option<String>) -> Result<(String, u64, Vec<String>)> {
    let (name, weight, tags) = if item.is_map() {
        let m = item.cast::<Map>();
        let name = match (&key, m.get("name")) {
            (Some(k), _) => k.clone(),
            (None, Some(n)) => n.clone().into_immutable_string().map_err(|_| script_err("task name must be a string"))?.to_string(),
            (None, None) => return Err(script_err("sequential task map requires a name")),
        };
        let weight = match m.get("weight") {
            Some(w) => w.as_int().map_err(|_| script_err(format!("weight for task '{name}' must be an integer")))?,
            None => 1,
        };
        (name, weight, tags_of(m.get("tags"))?)
    } else if let Some(k) = key {
        let w = item.as_int().map_err(|_| script_err(format!("weight for task '{k}' must be an integer")))?;
        (k, w, Vec::new())
    } else {
        let n = item.into_immutable_string().map_err(|_| script_err("sequential task entries must be names or maps"))?;
        (n.to_string(), 1, Vec::new())
    };
    if !(1..=MAX_WEIGHT).contains(&weight) {
        return Err(script_err(format!("weight for task '{name}' must be 1..{MAX_WEIGHT}")));
    }
    Ok((name, weight as u64, tags))
}

fn status_code(c: i64) -> Result<u16> {
    u16::try_from(c).ok().filter(|c| (100..=599).contains(c)).ok_or_else(|| script_err(format!("invalid expected status {c}")))
}

fn failure_dynamic(msg: &str) -> Dynamic {
    let mut m = Map::new();
    m.insert("ok".into(), Dynamic::FALSE);
    m.insert("status".into(), Dynamic::from_int(0));
    m.insert("error".into(), Dynamic::from(msg.to_owned()));
    m.insert("body".into(), Dynamic::from(String::new()));
    Dynamic::from_map(m)
}

fn resp_to_dynamic(resp: &Resp) -> Dynamic {
    let mut m = Map::new();
    m.insert("status".into(), Dynamic::from_int(INT::from(resp.status)));
    m.insert("ok".into(), Dynamic::from_bool(resp.ok));
    m.insert("latency_ms".into(), Dynamic::from_float(resp.latency.as_secs_f64() * 1000.0));
    m.insert("truncated".into(), Dynamic::from_bool(resp.truncated));
    m.insert("ttfb_ms".into(), Dynamic::from_float(resp.ttfb.as_secs_f64() * 1000.0));
    m.insert("attempts".into(), Dynamic::from_int(INT::from(resp.attempts)));
    if !resp.lines.is_empty() {
        let arr: Array = resp.lines.iter().map(|l| Dynamic::from(l.clone())).collect();
        m.insert("lines".into(), Dynamic::from_array(arr));
    }
    let slice = &resp.body[..resp.body.len().min(SCRIPT_BODY_MAX)];
    let is_json = resp.header("content-type").is_some_and(|ct| ct.contains("json"));
    let json = if is_json && resp.body.len() <= SCRIPT_BODY_MAX {
        serde_json::from_slice::<serde_json::Value>(slice).ok().and_then(|v| rhai::serde::to_dynamic(&v).ok()).unwrap_or(Dynamic::UNIT)
    } else {
        Dynamic::UNIT
    };
    m.insert("json".into(), json);
    m.insert("body".into(), Dynamic::from(String::from_utf8_lossy(slice).into_owned()));
    let mut headers = Map::new();
    for (k, v) in &resp.headers {
        if let Ok(s) = v.to_str() {
            headers.insert(k.as_str().into(), Dynamic::from(s.to_owned()));
        }
    }
    m.insert("headers".into(), Dynamic::from_map(headers));
    Dynamic::from_map(m)
}

fn req_map(method: &str, path: &str, name: Option<&str>) -> Map {
    let mut m = Map::new();
    m.insert("method".into(), Dynamic::from(method.to_owned()));
    m.insert("path".into(), Dynamic::from(path.to_owned()));
    if let Some(n) = name {
        m.insert("name".into(), Dynamic::from(n.to_owned()));
    }
    m
}

fn with(mut m: Map, key: &str, v: Dynamic) -> Map {
    m.insert(key.into(), v);
    m
}

fn build_engine(scenario: &str, feeders: Arc<Feeders>, shared: Arc<SharedStore>) -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(2_000_000)
        .set_max_call_levels(48)
        .set_max_expr_depths(96, 48)
        .set_max_string_size(SCRIPT_BODY_MAX * 2)
        .set_max_array_size(200_000)
        .set_max_map_size(50_000)
        .set_max_modules(0)
        .set_strict_variables(true);
    e.disable_symbol("eval");

    let tag = scenario.to_owned();
    e.on_print(move |s| tracing::info!(target: "script", scenario = %tag, "{}", sanitize(s, 2000)));
    let tag = scenario.to_owned();
    e.on_debug(move |s, _src, pos| {
        tracing::debug!(target: "script", scenario = %tag, line = ?pos.line(), "{}", sanitize(s, 2000));
    });
    let tag = scenario.to_owned();
    e.register_fn("log", move |s: &str| {
        tracing::info!(target: "script", scenario = %tag, "{}", sanitize(s, 2000));
    });

    for m in ["GET", "DELETE", "HEAD", "OPTIONS"] {
        let lower = m.to_ascii_lowercase();
        e.register_fn(lower.as_str(), move |p: &str| req_map(m, p, None));
        e.register_fn(lower.as_str(), move |p: &str, n: &str| req_map(m, p, Some(n)));
    }
    for m in ["POST", "PUT", "PATCH"] {
        let lower = m.to_ascii_lowercase();
        e.register_fn(lower.as_str(), move |p: &str, body: &str| with(req_map(m, p, None), "body", Dynamic::from(body.to_owned())));
        e.register_fn(lower.as_str(), move |p: &str, body: &str, n: &str| {
            with(req_map(m, p, Some(n)), "body", Dynamic::from(body.to_owned()))
        });
        let json_fn = format!("{lower}_json");
        e.register_fn(json_fn.as_str(), move |p: &str, v: Dynamic| with(req_map(m, p, None), "json", v));
        e.register_fn(json_fn.as_str(), move |p: &str, v: Dynamic, n: &str| with(req_map(m, p, Some(n)), "json", v));
        let form_fn = format!("{lower}_form");
        e.register_fn(form_fn.as_str(), move |p: &str, v: Map| with(req_map(m, p, None), "form", Dynamic::from_map(v)));
        e.register_fn(form_fn.as_str(), move |p: &str, v: Map, n: &str| with(req_map(m, p, Some(n)), "form", Dynamic::from_map(v)));
    }
    e.register_fn("ws", |p: &str, msg: &str| {
        let mut m = Map::new();
        m.insert("ws".into(), Dynamic::from(p.to_owned()));
        m.insert("send".into(), Dynamic::from(msg.to_owned()));
        m
    });
    e.register_fn("ws", |p: &str, msg: &str, n: &str| {
        let mut m = Map::new();
        m.insert("ws".into(), Dynamic::from(p.to_owned()));
        m.insert("send".into(), Dynamic::from(msg.to_owned()));
        m.insert("name".into(), Dynamic::from(n.to_owned()));
        m
    });
    e.register_fn("sse", |p: &str, max: INT| with(req_map("GET", p, None), "sse", Dynamic::from_int(max.clamp(1, 100_000))));
    e.register_fn("sse", |p: &str, max: INT, n: &str| with(req_map("GET", p, Some(n)), "sse", Dynamic::from_int(max.clamp(1, 100_000))));
    e.register_fn("sio_emit", |event: &str, data: Dynamic| {
        let mut m = with(Map::new(), "socketio", Dynamic::from("/socket.io/".to_owned()));
        m.insert("emit".into(), Dynamic::from(event.to_owned()));
        with(m, "data", data)
    });
    e.register_fn("sio_ack", |event: &str, data: Dynamic| {
        let mut m = with(Map::new(), "socketio", Dynamic::from("/socket.io/".to_owned()));
        m.insert("emit".into(), Dynamic::from(event.to_owned()));
        m.insert("ack".into(), Dynamic::TRUE);
        with(m, "data", data)
    });
    e.register_fn("sio_wait", |event: &str| {
        let m = with(Map::new(), "socketio", Dynamic::from("/socket.io/".to_owned()));
        with(m, "wait", Dynamic::from(event.to_owned()))
    });
    e.register_fn("step_var", |vars: Map, path: &str| -> FnResult<Dynamic> {
        let mut cur = Dynamic::from_map(vars);
        for seg in path.split('.') {
            let next = if cur.is_map() {
                cur.cast::<Map>().get(seg).cloned()
            } else if cur.is_array() {
                seg.parse::<usize>().ok().and_then(|i| cur.cast::<Array>().get(i).cloned())
            } else {
                None
            };
            match next {
                Some(v) if !v.is_unit() => cur = v,
                _ => return Err(format!("variable '{}' is not set", sanitize(path, 64)).into()),
            }
        }
        Ok(cur)
    });
    e.register_fn("concat", |parts: Array| -> String {
        let mut o = String::new();
        for p in parts {
            if p.is_string() {
                o.push_str(&p.into_immutable_string().unwrap_or_default());
            } else {
                o.push_str(&p.to_string());
            }
        }
        o
    });
    e.register_fn("step_env", |name: &str| -> FnResult<String> {
        let valid = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !valid {
            return Err("invalid environment variable name".into());
        }
        std::env::var(format!("{ENV_PREFIX}{name}")).map_err(|_| format!("environment variable {ENV_PREFIX}{name} is not set").into())
    });
    e.register_fn("capture_json", |res: Dynamic, path: &str| capture_json(&res, path));
    e.register_fn("capture_header", |res: Dynamic, name: &str| -> Dynamic {
        res.try_cast::<Map>()
            .and_then(|m| m.get("headers").cloned())
            .and_then(|h| h.try_cast::<Map>())
            .and_then(|h| h.get(name.to_ascii_lowercase().as_str()).cloned())
            .unwrap_or(Dynamic::UNIT)
    });
    e.register_fn("capture_between", |res: Dynamic, left: &str, right: &str| -> Dynamic {
        let body = res.try_cast::<Map>().and_then(|m| m.get("body").cloned()).and_then(|b| b.into_immutable_string().ok());
        match body {
            Some(b) if !left.is_empty() => b
                .find(left)
                .map(|i| &b[i + left.len()..])
                .and_then(|rest| if right.is_empty() { Some(rest) } else { rest.find(right).map(|j| &rest[..j]) })
                .map_or(Dynamic::UNIT, |v| Dynamic::from(v.to_owned())),
            _ => Dynamic::UNIT,
        }
    });
    e.register_fn("sleep", |ms: INT| {
        let mut m = Map::new();
        m.insert("sleep_ms".into(), Dynamic::from_int(ms.clamp(0, MAX_SLEEP_MS as INT)));
        m
    });

    let store = Arc::clone(&shared);
    e.register_fn("shared_get", move |key: &str| -> Dynamic {
        store.get(key).and_then(|v| rhai::serde::to_dynamic(&v).ok()).unwrap_or(Dynamic::UNIT)
    });
    e.register_fn("shared_set", move |key: &str, value: Dynamic| -> FnResult<()> {
        let v: serde_json::Value = rhai::serde::from_dynamic(&value)?;
        shared.set(key, v).map_err(|err| err.to_string().into())
    });
    e.register_fn("txn_begin", |name: &str| -> Map {
        let mut m = Map::new();
        m.insert("txn_begin".into(), Dynamic::from(name.to_owned()));
        m
    });
    e.register_fn("txn_end", |name: &str| -> Map {
        let mut m = Map::new();
        m.insert("txn_end".into(), Dynamic::from(name.to_owned()));
        m
    });
    e.register_fn("batch", |items: Array| -> Map {
        let mut m = Map::new();
        m.insert("batch".into(), Dynamic::from_array(items));
        m
    });
    e.register_fn("batch", |items: Array, name: &str| -> Map {
        let mut m = Map::new();
        m.insert("batch".into(), Dynamic::from_array(items));
        m.insert("name".into(), Dynamic::from(name.to_owned()));
        m
    });
    e.register_fn("feed", move |name: &str| -> Dynamic {
        match feeders.next(name) {
            Some(row) => {
                let map: Map = row.iter().map(|(k, v)| (k.as_str().into(), Dynamic::from(v.clone()))).collect();
                Dynamic::from_map(map)
            }
            None => Dynamic::UNIT,
        }
    });

    e.register_fn("uuid", uuid_v4);
    e.register_fn("now_ms", || -> INT {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as INT).unwrap_or(0)
    });
    e.register_fn("rand_int", |lo: INT, hi: INT| -> FnResult<INT> {
        if lo > hi {
            return Err("rand_int: lo must be <= hi".into());
        }
        Ok(rand::random_range(lo..=hi))
    });
    e.register_fn("rand_float", || -> rhai::FLOAT { rand::random_range(0.0..1.0) });
    e.register_fn("rand_pick", |arr: Array| -> Dynamic {
        if arr.is_empty() { Dynamic::UNIT } else { arr[rand::random_range(0..arr.len())].clone() }
    });
    e.register_fn("rand_string", |n: INT| -> FnResult<String> {
        if !(0..=4096).contains(&n) {
            return Err("rand_string: length must be 0..4096".into());
        }
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        Ok((0..n).map(|_| CHARS[rand::random_range(0..CHARS.len())] as char).collect())
    });
    e.register_fn("env", |name: &str| -> Dynamic {
        let valid = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !valid {
            return Dynamic::UNIT;
        }
        std::env::var(format!("{ENV_PREFIX}{name}")).map(Dynamic::from).unwrap_or(Dynamic::UNIT)
    });
    e.register_fn("url_encode", |s: &str| -> String { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() });
    e.register_fn("json_encode", |v: Dynamic| -> FnResult<String> {
        serde_json::to_string(&v).map_err(|err| format!("json_encode: {err}").into())
    });
    e.register_fn("json_decode", |s: &str| -> FnResult<Dynamic> {
        if s.len() > SCRIPT_BODY_MAX {
            return Err("json_decode: input too large".into());
        }
        let v: serde_json::Value = serde_json::from_str(s).map_err(|err| format!("json_decode: {err}"))?;
        rhai::serde::to_dynamic(&v)
    });
    e
}

/// Navigates `res.json` by a dotted path such as `items.0.id`.
fn capture_json(res: &Dynamic, path: &str) -> Dynamic {
    let Some(mut cur) = res.clone().try_cast::<Map>().and_then(|m| m.get("json").cloned()) else {
        return Dynamic::UNIT;
    };
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        let next = if cur.is_map() {
            cur.cast::<Map>().get(seg).cloned()
        } else if cur.is_array() {
            seg.parse::<usize>().ok().and_then(|i| cur.cast::<Array>().get(i).cloned())
        } else {
            None
        };
        match next {
            Some(v) => cur = v,
            None => return Dynamic::UNIT,
        }
    }
    cur
}

fn uuid_v4() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_lists_tasks() {
        let src = r#"
            fn tasks() { #{ browse: 3, buy: 1 } }
            fn browse(step, res) { if step == 0 { get("/items", "list") } }
            fn buy(step, res) { if step == 0 { post_json("/cart", #{ id: 1 }) } }
        "#;
        let s = ScriptScenario::from_source("t", src, Arc::new(Feeders::default()), Arc::new(SharedStore::default()), &|_| true)
            .expect("script");
        assert_eq!(s.task_names().len(), 2);
    }

    #[test]
    fn filters_tagged_and_sequential_tasks() {
        let src = r#"
            fn tasks() { ["a", #{ name: "b", tags: ["slow"] }] }
            fn a(step, res) { () }
            fn b(step, res) { () }
        "#;
        let allow = |tags: &[String]| !tags.iter().any(|t| t == "slow");
        let s =
            ScriptScenario::from_source("t", src, Arc::new(Feeders::default()), Arc::new(SharedStore::default()), &allow).expect("script");
        assert_eq!(s.task_names(), ["a".to_owned()]);
        assert!(s.sequential);
    }

    #[test]
    fn rejects_missing_task_function() {
        let src = r#"fn tasks() { #{ nope: 1 } }"#;
        assert!(ScriptScenario::from_source("t", src, Arc::new(Feeders::default()), Arc::new(SharedStore::default()), &|_| true).is_err());
    }

    #[test]
    fn uuid_format() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }
}
