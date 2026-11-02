// File: ballista/src/cli.rs
use std::io::{BufRead, IsTerminal};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::cluster::hub::{ClusterServerConfig, Hub};
use crate::cluster::worker::WorkerConfig;
use crate::cluster::{self, fingerprint};
use crate::config::{Overrides, Plan};
use crate::console;
use crate::engine::{Engine, Role, debug_run};
use crate::error::{Error, Result};
use crate::metrics::RunState;
use crate::report::Report;
use crate::scenario::Registry;
use crate::web::{self, WebConfig};

const EXIT_ERROR: u8 = 1;
const EXIT_THRESHOLDS: u8 = 2;
const EXIT_INTERRUPTED: u8 = 130;

#[derive(Parser)]
#[command(name = "ballista", version, about = "Scriptable load testing engine")]
struct Cli {
    /// TOML file with default option values (precedence: command line, environment, this file, plan).
    #[arg(long, global = true, env = "BALLISTA_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// Option names accepted in the config file and the environment variables they map to.
/// Secrets are deliberately excluded: tokens and hashes belong in the environment or a secret store.
const CONFIG_KEYS: &[(&str, &str)] = &[
    ("bind", "BALLISTA_BIND"),
    ("tls_cert", "BALLISTA_TLS_CERT"),
    ("tls_key", "BALLISTA_TLS_KEY"),
    ("public_origin", "BALLISTA_PUBLIC_ORIGIN"),
    ("cluster_bind", "BALLISTA_CLUSTER_BIND"),
    ("cluster_tls_cert", "BALLISTA_CLUSTER_TLS_CERT"),
    ("cluster_tls_key", "BALLISTA_CLUSTER_TLS_KEY"),
    ("controller", "BALLISTA_CONTROLLER"),
    ("ca_cert", "BALLISTA_CA_CERT"),
    ("host", "BALLISTA_HOST"),
    ("users", "BALLISTA_USERS"),
    ("spawn_rate", "BALLISTA_SPAWN_RATE"),
    ("run_time", "BALLISTA_RUN_TIME"),
    ("iterations", "BALLISTA_ITERATIONS"),
    ("tags", "BALLISTA_TAGS"),
    ("exclude_tags", "BALLISTA_EXCLUDE_TAGS"),
    ("csv", "BALLISTA_CSV"),
    ("jtl", "BALLISTA_JTL"),
    ("percentiles", "BALLISTA_PERCENTILES"),
    ("log", "BALLISTA_LOG"),
];
const SECRET_KEYS: &[&str] = &["admin_hash", "metrics_token", "cluster_token", "token", "password"];
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

fn config_path_from_args() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let a = a.to_string_lossy().into_owned();
        if a == "--config" {
            return args.next().map(PathBuf::from);
        }
        if let Some(v) = a.strip_prefix("--config=") {
            return Some(PathBuf::from(v));
        }
    }
    std::env::var_os("BALLISTA_CONFIG").map(PathBuf::from)
}

/// Loads the config file into environment defaults that clap reads. Existing variables win.
fn apply_config_file() -> Result<()> {
    let Some(path) = config_path_from_args() else {
        return Ok(());
    };
    let meta = std::fs::metadata(&path).map_err(|e| Error::config(format!("config {}: {e}", path.display())))?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return Err(Error::config("config file must be a regular file up to 64 KiB"));
    }
    let table: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path)?).map_err(|e| Error::config(format!("invalid config file: {e}")))?;
    let mut pending = Vec::new();
    for (key, value) in &table {
        if SECRET_KEYS.contains(&key.as_str()) {
            return Err(Error::config(format!("'{key}' must not be stored in the config file; use the environment")));
        }
        let env = CONFIG_KEYS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, e)| *e)
            .ok_or_else(|| Error::config(format!("unknown config key '{key}'")))?;
        let text = match value {
            toml::Value::String(v) => v.clone(),
            toml::Value::Integer(v) => v.to_string(),
            toml::Value::Float(v) => v.to_string(),
            toml::Value::Boolean(v) => v.to_string(),
            toml::Value::Array(items) => items
                .iter()
                .map(|i| match i {
                    toml::Value::String(v) => Ok(v.clone()),
                    toml::Value::Integer(v) => Ok(v.to_string()),
                    toml::Value::Float(v) => Ok(v.to_string()),
                    _ => Err(Error::config(format!("config key '{key}' holds an unsupported array item"))),
                })
                .collect::<Result<Vec<_>>>()?
                .join(","),
            _ => return Err(Error::config(format!("config key '{key}' has an unsupported value type"))),
        };
        if text.len() > 4096 || text.contains('\0') {
            return Err(Error::config(format!("config key '{key}' value is too long or invalid")));
        }
        if std::env::var_os(env).is_none() {
            pending.push((env, text));
        }
    }
    for (env, text) in pending {
        // SAFETY: called from main before the async runtime or any other thread exists,
        // so no concurrent reader of the process environment can observe the update.
        unsafe { std::env::set_var(env, text) };
    }
    Ok(())
}

fn parse_percentiles(s: &str) -> std::result::Result<PercentileList, String> {
    let v: std::result::Result<Vec<f64>, _> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::parse).collect();
    match v {
        Ok(v) if !v.is_empty() && v.len() <= 12 && v.iter().all(|p| *p > 0.0 && *p <= 100.0) => Ok(PercentileList(v)),
        _ => Err("percentiles must be up to 12 comma-separated values within (0, 100]".into()),
    }
}

#[derive(Debug, Clone)]
struct PercentileList(Vec<f64>);

#[derive(Subcommand)]
enum Command {
    /// Execute a plan headless, print live stats, write reports and enforce thresholds.
    Run(RunArgs),
    /// Start the dashboard and wait for runs to be started from the browser.
    Serve(ServeArgs),
    /// Validate a plan, its scripts and feeders without generating load.
    Check {
        plan: PathBuf,
        #[command(flatten)]
        overrides: OverrideArgs,
    },
    /// Run one virtual user with a request/response trace (sensitive values redacted).
    Debug(DebugArgs),
    /// Join a controller (`run --master` / `serve --master`) and generate load on its behalf.
    Worker(WorkerArgs),
    /// Read a password from stdin and print its Argon2id hash for BALLISTA_ADMIN_HASH.
    HashPassword,
    /// Convert a browser recording (HAR) into a plan with steps or a script.
    Import(ImportArgs),
    /// Serve the run manager API: store bundles, queue runs and keep reports (tokens in BALLISTA_API_TOKENS).
    Manager(ManagerArgs),
    /// Generate an API token for the run manager and print its configuration entry.
    ApiToken {
        /// Token name shown in logs and run records.
        #[arg(long)]
        name: String,
        /// read, run or admin.
        #[arg(long, default_value = "run")]
        scope: String,
    },
}

#[derive(Args)]
struct ManagerArgs {
    /// API bind address.
    #[arg(long, default_value = "127.0.0.1:8070", env = "BALLISTA_API_BIND")]
    bind: SocketAddr,
    /// Directory for bundles, run records and reports.
    #[arg(long, default_value = "ballista-data", env = "BALLISTA_DATA_DIR")]
    data_dir: PathBuf,
    #[arg(long, env = "BALLISTA_TLS_CERT")]
    tls_cert: Option<PathBuf>,
    #[arg(long, env = "BALLISTA_TLS_KEY")]
    tls_key: Option<PathBuf>,
    /// Permit plaintext HTTP on non-loopback addresses (only behind a TLS-terminating proxy).
    #[arg(long)]
    allow_remote_plaintext: bool,
    /// Hosts submitted plans may target: exact host, '*.example.com' or '*' (comma-separated).
    #[arg(long, env = "BALLISTA_ALLOW_TARGETS", value_delimiter = ',', required = true)]
    allow_target: Vec<String>,
    /// Environment variables submitted plans may reference (comma-separated).
    #[arg(long, env = "BALLISTA_ALLOW_ENV", value_delimiter = ',')]
    allow_env: Vec<String>,
    /// Highest user count (or arrival rate) a plan may reach.
    #[arg(long, default_value_t = 5000.0)]
    max_users: f64,
    /// Longest run; runs are stopped when they reach it.
    #[arg(long, default_value = "1h", value_parser = humantime::parse_duration)]
    max_run_time: Duration,
    #[arg(long, default_value_t = 50)]
    max_queue: usize,
    /// Finished runs kept on disk.
    #[arg(long, default_value_t = 500)]
    keep_runs: usize,
    /// Endpoint notified on run start and finish (signed with BALLISTA_WEBHOOK_SECRET).
    #[arg(long, env = "BALLISTA_WEBHOOK_URL")]
    webhook_url: Option<String>,
    /// Accept pool workers (`ballista worker` without a plan) on this address; runs are spread across them.
    #[arg(long, env = "BALLISTA_CLUSTER_BIND")]
    cluster_bind: Option<SocketAddr>,
    #[arg(long, env = "BALLISTA_CLUSTER_TLS_CERT")]
    cluster_tls_cert: Option<PathBuf>,
    #[arg(long, env = "BALLISTA_CLUSTER_TLS_KEY")]
    cluster_tls_key: Option<PathBuf>,
    /// Accept workers over plaintext on non-loopback addresses (only inside a trusted, encrypted network).
    #[arg(long)]
    cluster_allow_remote_plaintext: bool,
    /// Shared secret pool workers present.
    #[arg(long, env = "BALLISTA_CLUSTER_TOKEN", hide_env_values = true)]
    cluster_token: Option<String>,
    /// Do not serve the browser console under /console/ (the API is unaffected).
    #[arg(long, env = "BALLISTA_DISABLE_CONSOLE")]
    disable_console: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ImportFormat {
    /// Plan TOML with declarative steps.
    Plan,
    /// Script with a sequential task.
    Script,
}

#[derive(Args)]
struct ImportArgs {
    /// HAR file exported from browser developer tools or a proxy.
    har: PathBuf,
    /// Output file (stdout when omitted).
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ImportFormat::Plan)]
    format: ImportFormat,
    /// Keep only requests to this host (defaults to the first page request's host).
    #[arg(long)]
    host: Option<String>,
    /// Keep scripts, styles, images, fonts and media.
    #[arg(long)]
    include_static: bool,
    /// Pauses at least this long become think steps.
    #[arg(long, default_value = "1s", value_parser = humantime::parse_duration)]
    think_min: std::time::Duration,
    /// Longest think step.
    #[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
    think_max: std::time::Duration,
    /// Scenario name.
    #[arg(long, default_value = "recorded")]
    scenario: String,
    /// Overwrite an existing output file.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Clone, Default)]
struct TagList(Vec<String>);

fn parse_tags(s: &str) -> std::result::Result<TagList, String> {
    let tags: Vec<String> = s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned).collect();
    if tags.iter().all(|t| crate::config::is_valid_name(t)) {
        Ok(TagList(tags))
    } else {
        Err("tags must be comma-separated names of [A-Za-z0-9_.-]".into())
    }
}

#[derive(Args, Default, Clone)]
struct OverrideArgs {
    /// Override target.base_url.
    #[arg(long = "host", short = 'H', env = "BALLISTA_HOST")]
    host: Option<String>,
    /// Peak users (or iterations/s in arrival-rate mode); replaces the profile with a ramp.
    #[arg(long, short = 'u', env = "BALLISTA_USERS")]
    users: Option<f64>,
    /// Ramp speed per second used with --users (default 1).
    #[arg(long, short = 'r', env = "BALLISTA_SPAWN_RATE")]
    spawn_rate: Option<f64>,
    /// Total run time, e.g. 90s, 10m, 1h30m.
    #[arg(long, short = 't', env = "BALLISTA_RUN_TIME", value_parser = humantime::parse_duration)]
    run_time: Option<Duration>,
    /// Stop after this many iterations in total.
    #[arg(long, short = 'i', env = "BALLISTA_ITERATIONS")]
    iterations: Option<u64>,
    /// Only run scenarios or tasks carrying one of these tags (comma-separated).
    #[arg(long, env = "BALLISTA_TAGS", value_parser = parse_tags)]
    tags: Option<TagList>,
    /// Skip scenarios or tasks carrying any of these tags (comma-separated).
    #[arg(long, env = "BALLISTA_EXCLUDE_TAGS", value_parser = parse_tags)]
    exclude_tags: Option<TagList>,
    /// Reset statistics once the profile reaches its peak.
    #[arg(long)]
    reset_stats: bool,
    /// Write report JSON to this path.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Write report HTML to this path.
    #[arg(long)]
    html: Option<PathBuf>,
    /// Write <PREFIX>_stats.csv, <PREFIX>_history.csv and <PREFIX>_failures.csv.
    #[arg(long, value_name = "PREFIX", env = "BALLISTA_CSV")]
    csv: Option<PathBuf>,
    /// Write one CSV row per request to this file.
    #[arg(long)]
    request_log: Option<PathBuf>,
    /// Write a JMeter-compatible JTL result file.
    #[arg(long, env = "BALLISTA_JTL")]
    jtl: Option<PathBuf>,
    /// Extra percentiles to report, e.g. 50,75,99.9
    #[arg(long, env = "BALLISTA_PERCENTILES", value_parser = parse_percentiles)]
    percentiles: Option<PercentileList>,
}

impl OverrideArgs {
    fn to_overrides(&self) -> Overrides {
        Overrides {
            host: self.host.clone(),
            users: self.users,
            spawn_rate: self.spawn_rate,
            run_time: self.run_time,
            iterations: self.iterations,
            tags: self.tags.clone().map(|t| t.0),
            exclude_tags: self.exclude_tags.clone().map(|t| t.0),
            reset_stats: self.reset_stats,
            json: self.json.clone(),
            html: self.html.clone(),
            csv: self.csv.clone(),
            request_log: self.request_log.clone(),
            jtl: self.jtl.clone(),
            percentiles: self.percentiles.clone().map(|p| p.0),
        }
    }
}

#[derive(Args)]
struct RunArgs {
    plan: PathBuf,
    #[command(flatten)]
    overrides: OverrideArgs,
    /// Also serve the live dashboard during the run.
    #[arg(long)]
    web: bool,
    /// Console stats interval.
    #[arg(long, default_value = "5s", value_parser = humantime::parse_duration)]
    interval: Duration,
    /// Suppress periodic console tables.
    #[arg(long)]
    quiet: bool,
    #[command(flatten)]
    web_opts: WebArgs,
    #[command(flatten)]
    master: MasterArgs,
}

#[derive(Args)]
struct MasterArgs {
    /// Act as controller: distribute the load to connected workers instead of generating it locally.
    #[arg(long)]
    master: bool,
    /// Address the worker endpoint listens on.
    #[arg(long, default_value = "127.0.0.1:5557", env = "BALLISTA_CLUSTER_BIND")]
    cluster_bind: SocketAddr,
    /// PEM certificate chain for the worker endpoint (wss).
    #[arg(long, env = "BALLISTA_CLUSTER_TLS_CERT")]
    cluster_tls_cert: Option<PathBuf>,
    /// PEM private key for the worker endpoint.
    #[arg(long, env = "BALLISTA_CLUSTER_TLS_KEY")]
    cluster_tls_key: Option<PathBuf>,
    /// Accept workers over plaintext on non-loopback addresses (only inside a trusted, encrypted network).
    #[arg(long)]
    cluster_allow_remote_plaintext: bool,
    /// Workers to wait for before starting (default 1).
    #[arg(long)]
    expect_workers: Option<usize>,
    /// How long to wait for the expected workers.
    #[arg(long, default_value = "120s", value_parser = humantime::parse_duration)]
    expect_workers_timeout: Duration,
    /// Shared secret workers must present (at least 32 characters).
    #[arg(long, env = "BALLISTA_CLUSTER_TOKEN", hide_env_values = true)]
    cluster_token: Option<String>,
}

#[derive(Args)]
struct WorkerArgs {
    /// Plan to run. Without a plan the worker joins a pool and receives each run's plan from the controller.
    plan: Option<PathBuf>,
    /// Directory for bundles received in pool mode.
    #[arg(long, env = "BALLISTA_WORK_DIR")]
    work_dir: Option<PathBuf>,
    /// Controller endpoint, e.g. wss://controller.internal:5557
    #[arg(long, env = "BALLISTA_CONTROLLER")]
    controller: String,
    /// PEM CA certificate used to verify the controller (for private or self-signed certificates).
    #[arg(long, env = "BALLISTA_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// Worker name shown on the controller.
    #[arg(long)]
    name: Option<String>,
    /// Permit ws:// to a non-loopback controller (only inside a trusted, encrypted network).
    #[arg(long)]
    allow_plaintext: bool,
    /// Shared secret configured on the controller.
    #[arg(long, env = "BALLISTA_CLUSTER_TOKEN", hide_env_values = true)]
    cluster_token: Option<String>,
    #[command(flatten)]
    overrides: OverrideArgs,
}

struct Controller {
    hub: Arc<Hub>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Result<()>>,
}

fn build_engine(plan: Plan, registry: &Registry, m: &MasterArgs) -> Result<(Engine, Option<Controller>)> {
    if !m.master {
        if m.expect_workers.is_some() {
            return Err(Error::config("--expect-workers requires --master"));
        }
        return Ok((Engine::new(plan, registry)?, None));
    }
    let token = m.cluster_token.clone().ok_or_else(|| Error::config("--master requires BALLISTA_CLUSTER_TOKEN"))?;
    let fp = fingerprint(&plan)?;
    let hub = Hub::new(token, fp)?;
    let engine = Engine::new_with_role(plan, registry, Role::Controller(Arc::clone(&hub)))?;
    let cfg = ClusterServerConfig {
        bind: m.cluster_bind,
        tls_cert: m.cluster_tls_cert.clone(),
        tls_key: m.cluster_tls_key.clone(),
        allow_remote_plaintext: m.cluster_allow_remote_plaintext,
    };
    cfg.validate()?;
    let listener =
        std::net::TcpListener::bind(cfg.bind).map_err(|e| Error::config(format!("cannot bind cluster endpoint {}: {e}", cfg.bind)))?;
    drop(listener);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(cluster::hub::serve(Arc::clone(&hub), cfg, shutdown.clone()));
    Ok((engine, Some(Controller { hub, shutdown, task })))
}

async fn wait_workers(c: &Controller, m: &MasterArgs) -> Result<()> {
    let n = m.expect_workers.unwrap_or(1).max(1);
    tokio::select! {
        r = c.hub.wait_for_workers(n, m.expect_workers_timeout) => r,
        _ = tokio::signal::ctrl_c() => Err(Error::Engine("interrupted while waiting for workers".into())),
    }
}

async fn stop_controller(c: Option<Controller>) {
    if let Some(c) = c {
        c.hub.quit_all();
        tokio::time::sleep(Duration::from_millis(300)).await;
        c.shutdown.cancel();
        if let Ok(Ok(Err(e))) = tokio::time::timeout(Duration::from_secs(10), c.task).await {
            tracing::error!(error = %e, "cluster endpoint error");
        }
    }
}

#[derive(Args)]
struct ServeArgs {
    plan: PathBuf,
    #[command(flatten)]
    overrides: OverrideArgs,
    /// Start a run immediately.
    #[arg(long)]
    autostart: bool,
    /// Exit this long after the first run finishes (requires --autostart).
    #[arg(long, value_parser = humantime::parse_duration)]
    autoquit: Option<Duration>,
    #[command(flatten)]
    web_opts: WebArgs,
    #[command(flatten)]
    master: MasterArgs,
}

#[derive(Args)]
struct DebugArgs {
    plan: PathBuf,
    /// Scenario to run (defaults to the first one).
    #[arg(long, short = 's')]
    scenario: Option<String>,
    #[command(flatten)]
    overrides: OverrideArgs,
}

#[derive(Args)]
struct WebArgs {
    /// Dashboard bind address.
    #[arg(long, default_value = "127.0.0.1:8089", env = "BALLISTA_BIND")]
    bind: SocketAddr,
    /// PEM certificate chain for HTTPS.
    #[arg(long, env = "BALLISTA_TLS_CERT")]
    tls_cert: Option<PathBuf>,
    /// PEM private key for HTTPS.
    #[arg(long, env = "BALLISTA_TLS_KEY")]
    tls_key: Option<PathBuf>,
    /// Permit plaintext HTTP on non-loopback addresses (only behind a TLS-terminating proxy).
    #[arg(long)]
    allow_remote_plaintext: bool,
    /// Argon2id PHC hash of the dashboard password.
    #[arg(long, env = "BALLISTA_ADMIN_HASH", hide_env_values = true)]
    admin_hash: Option<String>,
    /// Bearer token enabling the /metrics endpoint.
    #[arg(long, env = "BALLISTA_METRICS_TOKEN", hide_env_values = true)]
    metrics_token: Option<String>,
    /// Externally visible origin when served behind a reverse proxy, e.g. https://load.example.com
    #[arg(long, env = "BALLISTA_PUBLIC_ORIGIN")]
    public_origin: Option<String>,
}

impl WebArgs {
    fn into_config(self) -> WebConfig {
        WebConfig {
            bind: self.bind,
            tls_cert: self.tls_cert,
            tls_key: self.tls_key,
            allow_remote_plaintext: self.allow_remote_plaintext,
            admin_hash: self.admin_hash,
            metrics_token: self.metrics_token,
            public_origin: self.public_origin,
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let filter = EnvFilter::try_from_env("BALLISTA_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,rustls_platform_verifier=off,rustls=off,hyper_util=warn,h2=warn,tower=warn"));
    let fmt = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_ansi(std::io::stderr().is_terminal()).with_target(false);
    let _ = tracing_subscriber::registry().with(filter).with(fmt).with(crate::logbuf::RingLayer).try_init();
}

fn load_plan(path: &Path, o: &OverrideArgs) -> Result<Plan> {
    let mut plan = Plan::load(path)?;
    plan.apply_overrides(&o.to_overrides())?;
    Ok(plan)
}

/// Entry point for the stock binary and for custom binaries that register native scenarios.
pub fn main_with(registry: Registry) -> ExitCode {
    if let Err(e) = apply_config_file() {
        eprintln!("error: {e}");
        return ExitCode::from(EXIT_ERROR);
    }
    let cli = Cli::parse();
    let _ = &cli.config;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    init_tracing();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start runtime: {e}");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    let code = rt.block_on(async move {
        match cli.command {
            Command::Run(args) => run(args, &registry).await,
            Command::Serve(args) => serve(args, &registry).await,
            Command::Check { plan, overrides } => check(&plan, &overrides, &registry),
            Command::Debug(args) => debug(args, &registry).await,
            Command::Worker(args) => worker(args, &registry).await,
            Command::HashPassword => hash_password(),
            Command::Import(args) => import(&args),
            Command::Manager(args) => manager(args, &registry).await,
            Command::ApiToken { name, scope } => api_token(&name, &scope),
        }
    });
    match code {
        Ok(c) => ExitCode::from(c),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn check(path: &Path, o: &OverrideArgs, registry: &Registry) -> Result<u8> {
    let plan = load_plan(path, o)?;
    let engine = Engine::new(plan, registry)?;
    let shape = engine.shape();
    let profile = match (shape.total(), shape.peak()) {
        (Some(t), Some(p)) => format!("profile {:.0}s, peak {p:.1}", t.as_secs_f64()),
        _ => "scripted profile".to_owned(),
    };
    println!("plan '{}' is valid: scenarios [{}], {profile}", engine.plan().display_name(), engine.scenario_names().join(", "));
    Ok(0)
}

fn import(a: &ImportArgs) -> Result<u8> {
    if !crate::config::is_valid_name(&a.scenario) {
        return Err(Error::config("scenario name must be 1..64 characters of [A-Za-z0-9_.-]"));
    }
    if a.think_min > a.think_max {
        return Err(Error::config("--think-min must not exceed --think-max"));
    }
    let opts = crate::har::ImportOptions {
        host: a.host.clone(),
        include_static: a.include_static,
        think_min: a.think_min,
        think_max: a.think_max,
        scenario: a.scenario.clone(),
        format: match a.format {
            ImportFormat::Plan => crate::har::OutputFormat::Plan,
            ImportFormat::Script => crate::har::OutputFormat::Script,
        },
    };
    let (out, sum) = crate::har::import_file(&a.har, &opts)?;
    match &a.output {
        Some(path) => {
            let mut f = std::fs::OpenOptions::new();
            f.write(true);
            if a.force {
                f.create(true).truncate(true);
            } else {
                f.create_new(true);
            }
            let mut file = f.open(path).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => Error::config(format!("{} exists; pass --force to overwrite", path.display())),
                _ => Error::Io(e),
            })?;
            std::io::Write::write_all(&mut file, out.as_bytes())?;
            eprintln!(
                "imported {} requests ({} static, {} foreign skipped, {} values redacted, {} correlated) into {}",
                sum.requests,
                sum.skipped_static,
                sum.skipped_foreign,
                sum.redacted,
                sum.correlated,
                path.display()
            );
        }
        None => print!("{out}"),
    }
    Ok(0)
}

fn api_token(name: &str, scope: &str) -> Result<u8> {
    let (token, entry) = crate::manager::tokens::generate(name, crate::manager::tokens::Scope::parse(scope)?)?;
    eprintln!("Token (shown once, give it to the client):");
    println!("{token}");
    eprintln!("Add this entry to BALLISTA_API_TOKENS (comma-separated):");
    println!("{entry}");
    Ok(0)
}

async fn manager(a: ManagerArgs, registry: &Registry) -> Result<u8> {
    let tokens = std::env::var("BALLISTA_API_TOKENS").map_err(|_| Error::config("BALLISTA_API_TOKENS is not set (see `api-token`)"))?;
    let targets = a
        .allow_target
        .iter()
        .filter(|t| !t.trim().is_empty())
        .map(|t| crate::manager::policy::HostRule::parse(t))
        .collect::<Result<Vec<_>>>()?;
    let env = a.allow_env.iter().map(|e| e.trim().to_owned()).filter(|e| !e.is_empty()).collect::<std::collections::HashSet<_>>();
    if let Some(bad) = env.iter().find(|e| {
        !crate::config::is_valid_env_name(e)
            || e.starts_with("BALLISTA_API")
            || e.starts_with("BALLISTA_ADMIN")
            || e.as_str() == "BALLISTA_WEBHOOK_SECRET"
    }) {
        return Err(Error::config(format!("environment variable '{bad}' cannot be exposed to plans")));
    }
    if !a.max_users.is_finite() || a.max_users <= 0.0 || a.max_queue == 0 || a.max_queue > 10_000 {
        return Err(Error::config("max-users must be positive and max-queue within 1..10000"));
    }
    let cfg = crate::manager::ManagerConfig {
        bind: a.bind,
        data_dir: a.data_dir,
        tls_cert: a.tls_cert,
        tls_key: a.tls_key,
        allow_remote_plaintext: a.allow_remote_plaintext,
        tokens,
        policy: crate::manager::policy::Policy { targets, env, max_users: a.max_users, max_run_time: a.max_run_time },
        max_queue: a.max_queue,
        keep_runs: a.keep_runs.max(1),
        webhook_url: a.webhook_url,
        webhook_secret: std::env::var("BALLISTA_WEBHOOK_SECRET").ok(),
        cluster: match a.cluster_bind {
            Some(bind) => Some((
                ClusterServerConfig {
                    bind,
                    tls_cert: a.cluster_tls_cert,
                    tls_key: a.cluster_tls_key,
                    allow_remote_plaintext: a.cluster_allow_remote_plaintext,
                },
                a.cluster_token.ok_or_else(|| Error::config("--cluster-bind requires BALLISTA_CLUSTER_TOKEN"))?,
            )),
            None => None,
        },
        console: !a.disable_console,
    };
    let shutdown = CancellationToken::new();
    let sd = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("shutting down, stopping the active run");
            sd.cancel();
        }
    });
    crate::manager::serve(cfg, registry.clone(), shutdown).await?;
    Ok(0)
}

fn hash_password() -> Result<u8> {
    eprintln!("Enter password (12+ characters) followed by newline:");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let pw = line.trim_end_matches(['\r', '\n']);
    println!("{}", web::auth::hash_password(pw)?);
    Ok(0)
}

async fn debug(args: DebugArgs, registry: &Registry) -> Result<u8> {
    let plan = load_plan(&args.plan, &args.overrides)?;
    let snap = debug_run(plan, registry, args.scenario.as_deref(), args.overrides.iterations.unwrap_or(1)).await?;
    println!("\n{}", console::render_table(&snap));
    if !snap.errors.is_empty() {
        println!("Errors:\n{}", console::render_errors(&snap, 20));
    }
    let failed = snap.total.failures > 0 || snap.iterations.iter().any(|r| r.failures > 0);
    Ok(if failed { EXIT_THRESHOLDS } else { 0 })
}

async fn run(args: RunArgs, registry: &Registry) -> Result<u8> {
    let plan = load_plan(&args.plan, &args.overrides)?;
    let report_cfg = plan.report.clone();
    let (engine, controller) = build_engine(plan, registry, &args.master)?;
    if let Some(c) = &controller
        && let Err(e) = wait_workers(c, &args.master).await
    {
        stop_controller(controller).await;
        return Err(e);
    }

    let web_shutdown = CancellationToken::new();
    let web_task = if args.web {
        let cfg = args.web_opts.into_config();
        let e = engine.clone();
        let token = web_shutdown.clone();
        Some(tokio::spawn(async move { web::serve(e, cfg, token).await }))
    } else {
        None
    };

    engine.start()?;
    let printer = (!args.quiet).then(|| tokio::spawn(console::report_loop(engine.clone(), args.interval)));

    let mut interrupted = false;
    let waiter = engine.wait();
    tokio::pin!(waiter);
    let final_snap = loop {
        tokio::select! {
            snap = &mut waiter => break snap,
            sig = tokio::signal::ctrl_c() => {
                if sig.is_err() || interrupted {
                    eprintln!("second interrupt, exiting immediately");
                    return Ok(EXIT_INTERRUPTED);
                }
                interrupted = true;
                eprintln!("interrupt received, stopping gracefully (press Ctrl+C again to force)");
                let _ = engine.stop();
            }
        }
    };
    if let Some(p) = printer {
        p.abort();
    }

    println!("{}", console::render_table(&final_snap));
    if !final_snap.errors.is_empty() {
        println!("Errors:\n{}", console::render_errors(&final_snap, 20));
    }
    if !final_snap.thresholds.is_empty() {
        println!("{}", console::render_thresholds(&final_snap.thresholds));
    }
    if final_snap.generator.saturated_secs > 0 {
        eprintln!("warning: load generator was saturated for {}s; treat latency figures with caution", final_snap.generator.saturated_secs);
    }

    let report = Report::from_engine(&engine);
    report.write(&report_cfg)?;

    web_shutdown.cancel();
    if let Some(t) = web_task
        && let Ok(Ok(Err(e))) = tokio::time::timeout(Duration::from_secs(10), t).await
    {
        tracing::error!(error = %e, "dashboard error");
    }
    stop_controller(controller).await;
    Ok(if report.passed { 0 } else { EXIT_THRESHOLDS })
}

async fn serve(args: ServeArgs, registry: &Registry) -> Result<u8> {
    if args.autoquit.is_some() && !args.autostart {
        return Err(Error::config("--autoquit requires --autostart"));
    }
    let plan = load_plan(&args.plan, &args.overrides)?;
    let report_cfg = plan.report.clone();
    let (engine, controller) = build_engine(plan, registry, &args.master)?;
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    let cfg = args.web_opts.into_config();
    let e = engine.clone();
    let server = tokio::spawn(async move { web::serve(e, cfg, token).await });
    tokio::pin!(server);

    if args.autostart {
        if let Some(c) = &controller {
            wait_workers(c, &args.master).await?;
        }
        engine.start()?;
    }
    let autoquit = async {
        match args.autoquit {
            Some(d) => {
                engine.wait().await;
                Report::from_engine(&engine).write(&report_cfg)?;
                tracing::info!(after = ?d, "run finished, dashboard will exit");
                tokio::time::sleep(d).await;
                Ok::<(), Error>(())
            }
            None => std::future::pending().await,
        }
    };

    let mut code = 0;
    tokio::select! {
        res = &mut server => {
            return match res {
                Ok(r) => r.map(|()| 0),
                Err(e) => Err(Error::Engine(format!("dashboard task failed: {e}"))),
            };
        }
        _ = tokio::signal::ctrl_c() => eprintln!("shutting down"),
        r = autoquit => {
            r?;
            code = if Report::from_engine(&engine).passed { 0 } else { EXIT_THRESHOLDS };
        }
    }
    if engine.state() == RunState::Running {
        let _ = engine.stop();
        let _ = tokio::time::timeout(engine.plan().load.graceful_stop + Duration::from_secs(10), engine.wait()).await;
    }
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), server).await;
    stop_controller(controller).await;
    Ok(code)
}

async fn worker(args: WorkerArgs, registry: &Registry) -> Result<u8> {
    let token = args.cluster_token.clone().ok_or_else(|| Error::config("worker requires BALLISTA_CLUSTER_TOKEN"))?;
    let mode = match &args.plan {
        Some(path) => {
            let plan = load_plan(path, &args.overrides)?;
            let fp = fingerprint(&plan)?;
            cluster::worker::Mode::Fixed { loaded: cluster::worker::Loaded::new(plan, registry, None)?, fingerprint: fp }
        }
        None => {
            let work_dir =
                args.work_dir.clone().unwrap_or_else(|| std::env::temp_dir().join(format!("ballista-worker-{}", std::process::id())));
            cluster::worker::Mode::Pool { registry: registry.clone(), work_dir }
        }
    };
    let name = args.name.clone().or_else(|| std::env::var("HOSTNAME").ok()).unwrap_or_else(|| format!("worker-{}", std::process::id()));
    let cfg = WorkerConfig {
        controller: args.controller.clone(),
        token,
        ca_cert: args.ca_cert.clone(),
        name: crate::error::sanitize(&name, 64),
        allow_plaintext: args.allow_plaintext,
    };
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("interrupt received, stopping worker");
            token.cancel();
        }
    });
    cluster::worker::run(mode, cfg, shutdown).await?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_line_definition_is_consistent() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
