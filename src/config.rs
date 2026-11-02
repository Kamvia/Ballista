// File: ballista/src/config.rs
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use sha2::Digest as _;
use url::Url;

use crate::error::{Error, Result};

pub const MAX_USERS: f64 = 1_000_000.0;
pub const MAX_RATE: f64 = 10_000_000.0;
const MAX_PLAN_BYTES: u64 = 1 << 20;
const MAX_STAGES: usize = 512;
const MAX_SCENARIOS: usize = 128;
const MAX_THRESHOLDS: usize = 256;
const MAX_FEEDERS: usize = 64;
const MAX_HEADERS: usize = 64;
const MAX_TAGS: usize = 64;
const MAX_RUN: Duration = Duration::from_secs(365 * 24 * 3600);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    #[serde(default)]
    pub name: Option<String>,
    pub target: TargetConfig,
    pub load: LoadConfig,
    #[serde(rename = "scenario")]
    pub scenarios: Vec<ScenarioConfig>,
    #[serde(default, rename = "threshold")]
    pub thresholds: Vec<ThresholdConfig>,
    #[serde(default, rename = "feeder")]
    pub feeders: Vec<FeederConfig>,
    #[serde(default)]
    pub report: ReportConfig,
    #[serde(default)]
    pub protocols: crate::protocols::ProtocolsConfig,
    #[serde(skip)]
    pub base_dir: PathBuf,
    /// SHA-256 of the plan source, used to verify that cluster workers run the same plan.
    #[serde(skip)]
    pub source_digest: [u8; 32],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    #[serde(deserialize_with = "de_url")]
    pub base_url: Url,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default = "default_timeout", deserialize_with = "de_duration")]
    pub timeout: Duration,
    #[serde(default = "default_connect_timeout", deserialize_with = "de_duration")]
    pub connect_timeout: Duration,
    #[serde(default)]
    pub insecure_tls: bool,
    #[serde(default = "default_true")]
    pub cookies: bool,
    #[serde(default)]
    pub http2_prior_knowledge: bool,
    #[serde(default = "default_pool_idle")]
    pub pool_idle_per_host: usize,
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    #[serde(default)]
    pub client: ClientMode,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Outbound proxy, e.g. http://proxy:3128 or socks5://proxy:1080.
    #[serde(default)]
    pub proxy: Option<String>,
    /// Name of an environment variable holding proxy credentials as `user:password`.
    #[serde(default)]
    pub proxy_auth_env: Option<String>,
    /// Extra PEM CA bundle trusted in addition to the system roots.
    #[serde(default)]
    pub ca_cert: Option<PathBuf>,
    /// PEM client certificate chain for mutual TLS.
    #[serde(default)]
    pub client_cert: Option<PathBuf>,
    /// PEM private key for mutual TLS.
    #[serde(default)]
    pub client_key: Option<PathBuf>,
    /// Extra attempts after transport errors or `retry_on` statuses. Every attempt is recorded.
    #[serde(default)]
    pub retries: u32,
    #[serde(default = "default_retry_backoff", deserialize_with = "de_duration")]
    pub retry_backoff: Duration,
    #[serde(default = "default_retry_on")]
    pub retry_on: Vec<u16>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMode {
    #[default]
    PerUser,
    Shared,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadMode {
    #[default]
    Users,
    ArrivalRate,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadConfig {
    #[serde(default)]
    pub mode: LoadMode,
    #[serde(default)]
    pub stages: Vec<Stage>,
    /// Rhai script defining `fn shape(t)` returning the target at `t` seconds, or `()` to finish.
    #[serde(default)]
    pub shape_script: Option<PathBuf>,
    #[serde(default = "default_max_users")]
    pub max_users: u64,
    #[serde(default)]
    pub preallocate: u64,
    #[serde(default = "default_graceful", deserialize_with = "de_duration")]
    pub graceful_stop: Duration,
    /// Stop after this many iterations in total.
    #[serde(default)]
    pub iterations: Option<u64>,
    /// Reset statistics once the profile first reaches its peak target.
    #[serde(default)]
    pub reset_stats_after_ramp: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    #[serde(deserialize_with = "de_duration")]
    pub duration: Duration,
    pub target: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioConfig {
    pub name: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub script: Option<PathBuf>,
    #[serde(default)]
    pub native: Option<String>,
    #[serde(default)]
    pub think_time: Option<ThinkTime>,
    #[serde(default)]
    pub wait: Option<Wait>,
    /// Exact number of users for this scenario (closed model), spawned before weighted users.
    #[serde(default)]
    pub fixed_count: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Declarative steps run in order on every iteration (no script needed).
    #[serde(default, rename = "step")]
    pub steps: Vec<crate::scenario::steps::Step>,
    /// Declarative steps run once when a user starts.
    #[serde(default)]
    pub on_start: Vec<crate::scenario::steps::Step>,
}

impl ScenarioConfig {
    pub fn effective_wait(&self) -> Option<Wait> {
        self.wait.or_else(|| self.think_time.map(|t| Wait::Between(t.min, t.max)))
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkTime {
    #[serde(deserialize_with = "de_duration")]
    pub min: Duration,
    #[serde(deserialize_with = "de_duration")]
    pub max: Duration,
}

/// Delay between iterations of one user.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "WaitRaw")]
pub enum Wait {
    /// Uniform random delay in [min, max].
    Between(Duration, Duration),
    /// Fixed delay after each iteration.
    Constant(Duration),
    /// Each iteration plus wait takes at least this long.
    Pacing(Duration),
    /// Exponentially distributed delay with this mean (Poisson arrivals per user).
    Poisson(Duration),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitRaw {
    #[serde(default)]
    between: Option<Vec<String>>,
    #[serde(default)]
    constant: Option<String>,
    #[serde(default)]
    pacing: Option<String>,
    #[serde(default)]
    throughput: Option<f64>,
    #[serde(default)]
    poisson: Option<String>,
}

fn parse_dur(s: &str) -> std::result::Result<Duration, String> {
    humantime::parse_duration(s.trim()).map_err(|e| e.to_string())
}

impl TryFrom<WaitRaw> for Wait {
    type Error = String;

    fn try_from(r: WaitRaw) -> std::result::Result<Self, Self::Error> {
        let set = [r.between.is_some(), r.constant.is_some(), r.pacing.is_some(), r.throughput.is_some(), r.poisson.is_some()]
            .iter()
            .filter(|b| **b)
            .count();
        if set != 1 {
            return Err("wait requires exactly one of between, constant, pacing, throughput, poisson".into());
        }
        if let Some(b) = r.between {
            let [lo, hi] = b.as_slice() else {
                return Err("wait.between requires [min, max]".into());
            };
            let (lo, hi) = (parse_dur(lo)?, parse_dur(hi)?);
            if lo > hi {
                return Err("wait.between min must be <= max".into());
            }
            return Ok(Self::Between(lo, hi));
        }
        if let Some(c) = r.constant {
            return Ok(Self::Constant(parse_dur(&c)?));
        }
        if let Some(p) = r.pacing {
            return Ok(Self::Pacing(parse_dur(&p)?));
        }
        if let Some(t) = r.throughput {
            if !t.is_finite() || t <= 0.0 || t > 100_000.0 {
                return Err("wait.throughput must be within (0, 100000] iterations per second".into());
            }
            return Ok(Self::Pacing(Duration::from_secs_f64(1.0 / t)));
        }
        let mean = parse_dur(r.poisson.as_deref().unwrap_or_default())?;
        if mean.is_zero() {
            return Err("wait.poisson mean must be positive".into());
        }
        Ok(Self::Poisson(mean))
    }
}

impl Wait {
    /// Delay to apply after an iteration that took `elapsed`.
    pub fn delay(&self, elapsed: Duration) -> Duration {
        match *self {
            Self::Between(lo, hi) if lo == hi => lo,
            Self::Between(lo, hi) => {
                let (a, b) = (lo.as_micros() as u64, hi.as_micros() as u64);
                Duration::from_micros(rand::random_range(a..=b))
            }
            Self::Constant(d) => d,
            Self::Pacing(d) => d.saturating_sub(elapsed),
            Self::Poisson(mean) => {
                let u: f64 = rand::random_range(f64::MIN_POSITIVE..1.0);
                Duration::from_secs_f64((-u.ln() * mean.as_secs_f64()).min(3600.0))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMetric {
    P50,
    P90,
    P95,
    P99,
    Avg,
    Max,
    ErrorRate,
    Rps,
    DroppedIterations,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdConfig {
    pub metric: ThresholdMetric,
    #[serde(default = "default_request")]
    pub request: String,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub abort_on_fail: bool,
    #[serde(default = "default_abort_delay", deserialize_with = "de_duration")]
    pub abort_delay: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeederMode {
    #[default]
    Sequential,
    Random,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeederConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub mode: FeederMode,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportConfig {
    #[serde(default)]
    pub json: Option<PathBuf>,
    #[serde(default)]
    pub html: Option<PathBuf>,
    /// Prefix for `<prefix>_stats.csv`, `<prefix>_history.csv` and `<prefix>_failures.csv`.
    #[serde(default)]
    pub csv: Option<PathBuf>,
    /// Per-request CSV log (one row per request).
    #[serde(default)]
    pub request_log: Option<PathBuf>,
    /// OpenTelemetry OTLP/HTTP metrics export (requires the 'otel' feature).
    #[serde(default)]
    pub otlp: Option<OtlpConfig>,
    /// Extra percentiles to report, e.g. [50, 75, 90, 99, 99.9].
    #[serde(default)]
    pub percentiles: Vec<f64>,
    /// JMeter-compatible JTL (CSV) result file, one row per request.
    #[serde(default)]
    pub jtl: Option<PathBuf>,
    /// InfluxDB metrics output.
    #[serde(default)]
    pub influx: Option<InfluxConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfluxConfig {
    /// Server base URL, e.g. https://influx.example.com:8086
    #[serde(deserialize_with = "de_url")]
    pub url: Url,
    pub org: String,
    pub bucket: String,
    /// Environment variable holding the API token.
    pub token_env: String,
    #[serde(default = "default_otlp_interval", deserialize_with = "de_duration")]
    pub interval: Duration,
    /// Measurement prefix: `<prefix>_run` and `<prefix>_requests`.
    #[serde(default = "default_service_name")]
    pub measurement: String,
    /// Permit plain http to a non-loopback server (token is sent in clear).
    #[serde(default)]
    pub allow_plaintext: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpConfig {
    /// Collector metrics endpoint, e.g. http://collector:4318/v1/metrics
    pub endpoint: String,
    #[serde(default = "default_otlp_interval", deserialize_with = "de_duration")]
    pub interval: Duration,
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Environment variable with extra headers as `key=value,key2=value2` (for collector auth).
    #[serde(default)]
    pub headers_env: Option<String>,
}

fn default_otlp_interval() -> Duration {
    Duration::from_secs(5)
}
fn default_service_name() -> String {
    "ballista".to_owned()
}

/// Command line overrides applied on top of a plan file.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub host: Option<String>,
    pub users: Option<f64>,
    pub spawn_rate: Option<f64>,
    pub run_time: Option<Duration>,
    pub iterations: Option<u64>,
    pub tags: Option<Vec<String>>,
    pub exclude_tags: Option<Vec<String>>,
    pub reset_stats: bool,
    pub json: Option<PathBuf>,
    pub html: Option<PathBuf>,
    pub csv: Option<PathBuf>,
    pub request_log: Option<PathBuf>,
    pub jtl: Option<PathBuf>,
    pub percentiles: Option<Vec<f64>>,
}

fn default_retry_backoff() -> Duration {
    Duration::from_millis(200)
}
fn default_retry_on() -> Vec<u16> {
    vec![502, 503, 504]
}

/// Validates an environment variable *name* (secrets are referenced, never stored in plans).
pub fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

/// Reads a secret from the named environment variable.
pub fn secret_from_env(name: &str) -> Result<String> {
    if !is_valid_env_name(name) {
        return Err(Error::config(format!("invalid environment variable name '{name}'")));
    }
    std::env::var(name).map_err(|_| Error::config(format!("environment variable {name} is not set")))
}

fn default_timeout() -> Duration {
    Duration::from_secs(30)
}
fn default_connect_timeout() -> Duration {
    Duration::from_secs(10)
}
fn default_graceful() -> Duration {
    Duration::from_secs(10)
}
fn default_abort_delay() -> Duration {
    Duration::from_secs(10)
}
fn default_true() -> bool {
    true
}
fn default_pool_idle() -> usize {
    32
}
fn default_max_body() -> usize {
    10 * 1024 * 1024
}
fn default_max_users() -> u64 {
    1000
}
fn default_weight() -> u32 {
    1
}
fn default_request() -> String {
    "*".to_owned()
}

fn de_duration<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
    let raw = String::deserialize(d)?;
    humantime::parse_duration(raw.trim()).map_err(serde::de::Error::custom)
}

fn de_url<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Url, D::Error> {
    let raw = String::deserialize(d)?;
    Url::parse(raw.trim()).map_err(serde::de::Error::custom)
}

pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn validate_tags(tags: &[String], what: &str) -> Result<()> {
    if tags.len() > MAX_TAGS {
        return Err(Error::config(format!("too many {what}")));
    }
    for t in tags {
        if !is_valid_name(t) {
            return Err(Error::config(format!("invalid tag '{t}' in {what}")));
        }
    }
    Ok(())
}

/// Builds stages for a linear ramp to `users` at `rate` per second, held until `run_time`.
pub fn ramp_stages(users: f64, rate: f64, run_time: Option<Duration>) -> Vec<Stage> {
    let ramp = Duration::from_secs_f64((users / rate).max(0.001));
    let total = run_time.unwrap_or(MAX_RUN);
    if total <= ramp {
        let reached = users * total.as_secs_f64() / ramp.as_secs_f64();
        return vec![Stage { duration: total.max(Duration::from_millis(1)), target: reached }];
    }
    vec![Stage { duration: ramp, target: users }, Stage { duration: total - ramp, target: users }]
}

/// Truncates a stage list to a total duration, interpolating the cut stage.
pub fn truncate_stages(stages: &[Stage], total: Duration) -> Vec<Stage> {
    let mut out = Vec::new();
    let mut acc = Duration::ZERO;
    let mut from = 0.0;
    for s in stages {
        if acc + s.duration >= total {
            let part = total - acc;
            if !part.is_zero() {
                let frac = part.as_secs_f64() / s.duration.as_secs_f64();
                out.push(Stage { duration: part, target: from + (s.target - from) * frac });
            }
            return out;
        }
        acc += s.duration;
        from = s.target;
        out.push(s.clone());
    }
    out
}

impl Plan {
    pub fn load(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path).map_err(|e| Error::config(format!("cannot read plan {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(Error::config("plan path is not a regular file"));
        }
        if meta.len() > MAX_PLAN_BYTES {
            return Err(Error::config("plan file exceeds 1 MiB"));
        }
        let raw = std::fs::read_to_string(path)?;
        let base = path.canonicalize()?.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        Self::from_toml(&raw, base)
    }

    pub fn from_toml(raw: &str, base_dir: PathBuf) -> Result<Self> {
        let mut plan: Plan = toml::from_str(raw).map_err(|e| Error::config(format!("invalid plan: {e}")))?;
        plan.base_dir = base_dir;
        plan.source_digest = sha2::Sha256::digest(raw.as_bytes()).into();
        plan.target.connect_timeout = plan.target.connect_timeout.min(plan.target.timeout);
        plan.resolve_paths();
        plan.validate()?;
        Ok(plan)
    }

    fn resolve_paths(&mut self) {
        let base = self.base_dir.clone();
        let fix = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        for s in &mut self.scenarios {
            if let Some(p) = s.script.as_mut() {
                fix(p);
            }
        }
        for f in &mut self.feeders {
            fix(&mut f.path);
        }
        if let Some(p) = self.load.shape_script.as_mut() {
            fix(p);
        }
        let r = &mut self.report;
        for p in [r.json.as_mut(), r.html.as_mut(), r.csv.as_mut(), r.request_log.as_mut(), r.jtl.as_mut()].into_iter().flatten() {
            fix(p);
        }
        let t = &mut self.target;
        for p in [t.ca_cert.as_mut(), t.client_cert.as_mut(), t.client_key.as_mut()].into_iter().flatten() {
            fix(p);
        }
        self.protocols.resolve_paths(&base);
    }

    /// Applies command line overrides and revalidates. Paths in overrides are used as given.
    pub fn apply_overrides(&mut self, o: &Overrides) -> Result<()> {
        if let Some(h) = &o.host {
            self.target.base_url = Url::parse(h.trim()).map_err(|e| Error::config(format!("invalid --host: {e}")))?;
        }
        match (o.users, o.run_time) {
            (Some(users), rt) => {
                let rate = o.spawn_rate.unwrap_or(1.0);
                if !rate.is_finite() || rate <= 0.0 {
                    return Err(Error::config("spawn rate must be positive"));
                }
                if !users.is_finite() || users < 0.0 {
                    return Err(Error::config("users must be a non-negative number"));
                }
                self.load.stages = ramp_stages(users, rate, rt);
                self.load.shape_script = None;
            }
            (None, Some(rt)) => {
                if self.load.shape_script.is_some() {
                    return Err(Error::config("--run-time without --users is not supported with shape_script"));
                }
                self.load.stages = truncate_stages(&self.load.stages, rt);
            }
            (None, None) => {
                if o.spawn_rate.is_some() {
                    return Err(Error::config("--spawn-rate requires --users"));
                }
            }
        }
        if o.iterations.is_some() {
            self.load.iterations = o.iterations;
        }
        if let Some(t) = &o.tags {
            self.load.tags.clone_from(t);
        }
        if let Some(t) = &o.exclude_tags {
            self.load.exclude_tags.clone_from(t);
        }
        if o.reset_stats {
            self.load.reset_stats_after_ramp = true;
        }
        let r = &mut self.report;
        for (dst, src) in [
            (&mut r.json, &o.json),
            (&mut r.html, &o.html),
            (&mut r.csv, &o.csv),
            (&mut r.request_log, &o.request_log),
            (&mut r.jtl, &o.jtl),
        ] {
            if src.is_some() {
                dst.clone_from(src);
            }
        }
        if let Some(p) = &o.percentiles {
            r.percentiles.clone_from(p);
        }
        self.validate()
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(n) = &self.name
            && (n.is_empty() || n.len() > 128 || n.chars().any(char::is_control))
        {
            return Err(Error::config("plan name must be 1..128 printable characters"));
        }
        self.validate_target()?;
        self.validate_load()?;
        self.validate_scenarios()?;
        self.validate_thresholds()?;
        self.protocols.validate(self)?;
        if self.report.percentiles.len() > 12 || self.report.percentiles.iter().any(|p| !p.is_finite() || *p <= 0.0 || *p > 100.0) {
            return Err(Error::config("report.percentiles must hold up to 12 values within (0, 100]"));
        }
        if let Some(o) = &self.report.otlp {
            if !cfg!(feature = "otel") {
                return Err(Error::config("plan uses report.otlp but this binary was built without the 'otel' feature"));
            }
            let u = Url::parse(&o.endpoint).map_err(|e| Error::config(format!("otlp endpoint: {e}")))?;
            if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() {
                return Err(Error::config("otlp endpoint must be http(s) without embedded credentials"));
            }
            if o.interval < Duration::from_secs(1) || o.interval > Duration::from_secs(300) {
                return Err(Error::config("otlp interval must be within [1s, 5m]"));
            }
            if !is_valid_name(&o.service_name) {
                return Err(Error::config("otlp service_name must be a simple name"));
            }
            if let Some(n) = &o.headers_env
                && !is_valid_env_name(n)
            {
                return Err(Error::config("otlp headers_env must be an environment variable name"));
            }
        }
        if let Some(i) = &self.report.influx {
            let u = &i.url;
            if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() || u.query().is_some() {
                return Err(Error::config("influx url must be http(s) without credentials or query"));
            }
            let loopback = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            if u.scheme() == "http" && !loopback && !i.allow_plaintext {
                return Err(Error::config("influx over plain http to a remote host requires allow_plaintext = true"));
            }
            let printable = |v: &str| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control);
            if !printable(&i.org) || !printable(&i.bucket) {
                return Err(Error::config("influx org and bucket must be 1..128 printable characters"));
            }
            if !is_valid_env_name(&i.token_env) {
                return Err(Error::config("influx token_env must be an environment variable name"));
            }
            if !is_valid_name(&i.measurement) {
                return Err(Error::config("influx measurement must be a simple name"));
            }
            if i.interval < Duration::from_secs(1) || i.interval > Duration::from_secs(300) {
                return Err(Error::config("influx interval must be within [1s, 5m]"));
            }
        }
        let mut seen = HashSet::new();
        if self.feeders.len() > MAX_FEEDERS {
            return Err(Error::config("too many feeders"));
        }
        for f in &self.feeders {
            if !is_valid_name(&f.name) || !seen.insert(f.name.as_str()) {
                return Err(Error::config(format!("invalid or duplicate feeder name '{}'", f.name)));
            }
        }
        Ok(())
    }

    fn validate_target(&self) -> Result<()> {
        let t = &self.target;
        if !matches!(t.base_url.scheme(), "http" | "https") {
            return Err(Error::config("base_url must use http or https"));
        }
        if t.base_url.host_str().is_none() {
            return Err(Error::config("base_url must contain a host"));
        }
        if !t.base_url.username().is_empty() || t.base_url.password().is_some() {
            return Err(Error::config("base_url must not embed credentials; use headers"));
        }
        if t.timeout.is_zero() || t.timeout > Duration::from_secs(3600) {
            return Err(Error::config("timeout must be within (0, 1h]"));
        }
        if t.connect_timeout.is_zero() {
            return Err(Error::config("connect_timeout must be positive"));
        }
        if t.max_body_bytes == 0 || t.max_body_bytes > 512 * 1024 * 1024 {
            return Err(Error::config("max_body_bytes must be within (0, 512 MiB]"));
        }
        if t.headers.len() > MAX_HEADERS {
            return Err(Error::config("too many default headers"));
        }
        for (k, v) in &t.headers {
            http::HeaderName::from_bytes(k.as_bytes()).map_err(|_| Error::config(format!("invalid header name '{k}'")))?;
            http::HeaderValue::from_str(v).map_err(|_| Error::config(format!("invalid header value for '{k}'")))?;
        }
        for h in &t.allowed_hosts {
            if h.is_empty() || h.len() > 253 || h.contains(['/', '@', ' ']) {
                return Err(Error::config(format!("invalid allowed host '{h}'")));
            }
        }
        if let Some(p) = &t.proxy {
            let u = Url::parse(p).map_err(|e| Error::config(format!("invalid proxy url: {e}")))?;
            if !matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err(Error::config("proxy must use http, https, socks5 or socks5h"));
            }
            if !u.username().is_empty() || u.password().is_some() {
                return Err(Error::config("proxy url must not embed credentials; use proxy_auth_env"));
            }
        }
        if let Some(n) = &t.proxy_auth_env
            && !is_valid_env_name(n)
        {
            return Err(Error::config("proxy_auth_env must be an environment variable name"));
        }
        if t.client_cert.is_some() != t.client_key.is_some() {
            return Err(Error::config("client_cert and client_key must be set together"));
        }
        if t.retries > 10 {
            return Err(Error::config("retries must not exceed 10"));
        }
        if t.retry_backoff > Duration::from_secs(60) {
            return Err(Error::config("retry_backoff must not exceed 60s"));
        }
        if t.retry_on.iter().any(|c| !(100..=599).contains(c)) {
            return Err(Error::config("retry_on must contain HTTP status codes"));
        }
        Ok(())
    }

    pub fn target_cap(&self) -> f64 {
        match self.load.mode {
            LoadMode::Users => MAX_USERS,
            LoadMode::ArrivalRate => MAX_RATE,
        }
    }

    /// Validates a stage list against this plan's load mode.
    pub fn validate_stages(&self, stages: &[Stage]) -> Result<()> {
        if stages.is_empty() || stages.len() > MAX_STAGES {
            return Err(Error::config("load.stages must contain 1..512 stages"));
        }
        let cap = self.target_cap();
        let mut total = Duration::ZERO;
        for s in stages {
            if s.duration.is_zero() {
                return Err(Error::config("stage duration must be positive"));
            }
            if !s.target.is_finite() || s.target < 0.0 || s.target > cap {
                return Err(Error::config(format!("stage target must be within [0, {cap}]")));
            }
            total += s.duration;
        }
        if total > MAX_RUN + Duration::from_secs(1) {
            return Err(Error::config("total profile duration must not exceed 365 days"));
        }
        Ok(())
    }

    fn validate_load(&self) -> Result<()> {
        let l = &self.load;
        match (&l.shape_script, l.stages.is_empty()) {
            (Some(_), true) => {}
            (Some(_), false) => return Err(Error::config("define either load.stages or load.shape_script, not both")),
            (None, _) => self.validate_stages(&l.stages)?,
        }
        if l.mode == LoadMode::ArrivalRate {
            if l.max_users == 0 || l.max_users as f64 > MAX_USERS {
                return Err(Error::config("load.max_users must be within [1, 1000000]"));
            }
            if l.preallocate > l.max_users {
                return Err(Error::config("load.preallocate must not exceed max_users"));
            }
        }
        if l.graceful_stop > Duration::from_secs(600) {
            return Err(Error::config("graceful_stop must not exceed 10m"));
        }
        if l.iterations == Some(0) {
            return Err(Error::config("load.iterations must be positive"));
        }
        validate_tags(&l.tags, "load.tags")?;
        validate_tags(&l.exclude_tags, "load.exclude_tags")?;
        Ok(())
    }

    fn validate_scenarios(&self) -> Result<()> {
        if self.scenarios.is_empty() || self.scenarios.len() > MAX_SCENARIOS {
            return Err(Error::config("plan requires 1..128 scenarios"));
        }
        let mut seen = HashSet::new();
        for s in &self.scenarios {
            if !is_valid_name(&s.name) || !seen.insert(s.name.as_str()) {
                return Err(Error::config(format!("invalid or duplicate scenario name '{}'", s.name)));
            }
            if s.weight == 0 || s.weight > 10_000 {
                return Err(Error::config(format!("scenario '{}' weight must be 1..10000", s.name)));
            }
            let kinds = u8::from(s.script.is_some()) + u8::from(s.native.is_some()) + u8::from(!s.steps.is_empty());
            if kinds != 1 {
                return Err(Error::config(format!("scenario '{}' must define exactly one of script, native or step", s.name)));
            }
            if !s.on_start.is_empty() && s.steps.is_empty() {
                return Err(Error::config(format!("scenario '{}': on_start steps require step entries", s.name)));
            }
            if s.think_time.is_some() && s.wait.is_some() {
                return Err(Error::config(format!("scenario '{}' must not set both think_time and wait", s.name)));
            }
            if let Some(t) = s.think_time
                && t.min > t.max
            {
                return Err(Error::config(format!("scenario '{}' think_time min > max", s.name)));
            }
            if let Some(n) = s.fixed_count {
                if self.load.mode != LoadMode::Users {
                    return Err(Error::config(format!("scenario '{}': fixed_count requires mode = users", s.name)));
                }
                if n as f64 > MAX_USERS {
                    return Err(Error::config(format!("scenario '{}': fixed_count too large", s.name)));
                }
            }
            validate_tags(&s.tags, "scenario tags")?;
        }
        Ok(())
    }

    fn validate_thresholds(&self) -> Result<()> {
        if self.thresholds.len() > MAX_THRESHOLDS {
            return Err(Error::config("too many thresholds"));
        }
        for t in &self.thresholds {
            if t.max.is_none() && t.min.is_none() {
                return Err(Error::config("threshold requires min or max"));
            }
            if t.max.is_some_and(|v| !v.is_finite()) || t.min.is_some_and(|v| !v.is_finite()) {
                return Err(Error::config("threshold bounds must be finite"));
            }
            if t.request.is_empty() || t.request.len() > 256 {
                return Err(Error::config("threshold request selector must be 1..256 chars"));
            }
        }
        Ok(())
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("unnamed plan")
    }

    /// Tag filter: included when no include tags are set or any tag matches, and no exclude tag matches.
    pub fn tags_allowed<'a>(&self, tags: impl IntoIterator<Item = &'a str> + Clone) -> bool {
        let l = &self.load;
        let excluded = tags.clone().into_iter().any(|t| l.exclude_tags.iter().any(|e| e == t));
        let included = l.tags.is_empty() || tags.into_iter().any(|t| l.tags.iter().any(|i| i == t));
        included && !excluded
    }

    /// Copy of the plan aimed at another host; the host must be the configured target or in `allowed_hosts`.
    pub fn with_host(&self, host: &str) -> Result<Plan> {
        let url = Url::parse(host.trim()).map_err(|e| Error::config(format!("invalid host: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
            return Err(Error::config("host must be http(s) without credentials"));
        }
        if !self.host_allowed(&url) {
            return Err(Error::config("host is not the plan target or listed in target.allowed_hosts"));
        }
        let mut p = self.clone();
        p.target.base_url = url;
        Ok(p)
    }

    pub fn host_allowed(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
            return false;
        }
        if self.target.base_url.host_str() == Some(host) {
            return true;
        }
        self.target.allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"
        [target]
        base_url = "http://127.0.0.1:8080/"
        [load]
        stages = [{ duration = "10s", target = 5 }]
        [[scenario]]
        name = "browse"
        native = "browse"
    "#;

    #[test]
    fn parses_minimal_plan() {
        let plan = Plan::from_toml(PLAN, PathBuf::from(".")).expect("plan");
        assert_eq!(plan.scenarios.len(), 1);
        assert_eq!(plan.load.mode, LoadMode::Users);
    }

    #[test]
    fn rejects_credentials_in_url() {
        let raw = PLAN.replace("http://127.0.0.1", "http://a:b@127.0.0.1");
        assert!(Plan::from_toml(&raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let raw = format!("{PLAN}\nbogus = 1\n");
        assert!(Plan::from_toml(&raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn parses_wait_kinds() {
        for (w, ok) in [
            (r#"wait = { between = ["1s", "2s"] }"#, true),
            (r#"wait = { constant = "1s" }"#, true),
            (r#"wait = { pacing = "2s" }"#, true),
            (r#"wait = { throughput = 0.5 }"#, true),
            (r#"wait = { poisson = "1s" }"#, true),
            (r#"wait = { constant = "1s", pacing = "1s" }"#, false),
        ] {
            let raw = format!("{PLAN}{w}\n");
            assert_eq!(Plan::from_toml(&raw, PathBuf::from(".")).is_ok(), ok, "{w}");
        }
        assert_eq!(Wait::Pacing(Duration::from_secs(2)).delay(Duration::from_millis(500)), Duration::from_millis(1500));
    }

    #[test]
    fn overrides_build_ramp() {
        let mut plan = Plan::from_toml(PLAN, PathBuf::from(".")).expect("plan");
        let o = Overrides { users: Some(100.0), spawn_rate: Some(10.0), run_time: Some(Duration::from_secs(60)), ..Overrides::default() };
        plan.apply_overrides(&o).expect("overrides");
        assert_eq!(plan.load.stages.len(), 2);
        assert_eq!(plan.load.stages[0].duration, Duration::from_secs(10));
        assert_eq!(plan.load.stages[1].duration, Duration::from_secs(50));
    }

    #[test]
    fn truncates_profile() {
        let s =
            vec![Stage { duration: Duration::from_secs(10), target: 100.0 }, Stage { duration: Duration::from_secs(10), target: 100.0 }];
        let t = truncate_stages(&s, Duration::from_secs(5));
        assert_eq!(t.len(), 1);
        assert!((t[0].target - 50.0).abs() < 1e-9);
    }

    #[test]
    fn tag_filter() {
        let mut plan = Plan::from_toml(PLAN, PathBuf::from(".")).expect("plan");
        plan.load.tags = vec!["smoke".into()];
        plan.load.exclude_tags = vec!["slow".into()];
        assert!(plan.tags_allowed(["smoke"]));
        assert!(!plan.tags_allowed(["smoke", "slow"]));
        assert!(!plan.tags_allowed(["other"]));
    }
}
