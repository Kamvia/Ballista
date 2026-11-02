<!-- File: ballista/README.md -->
# Ballista

Distributed load testing in a single binary. Describe user behaviour as declarative steps or scripts, drive it from one machine or a pool of workers, watch it live in the browser, and fail CI builds when a run is slower than the last good one.

Ballista started as a faster replacement for Python-based load tools. On a 2-core machine it holds 10,000 concurrent users at about 320 MB of memory (see [Performance](#performance)).

## Contents

- [Highlights](#highlights)
- [Quick start](#quick-start)
- [Writing tests](#writing-tests)
- [Run manager and web console](#run-manager-and-web-console)
- [Client SDKs](#client-sdks)
- [Deployment](#deployment)
- [Distributed load](#distributed-load)
- [Reports and integrations](#reports-and-integrations)
- [Security model](#security-model)
- [Performance](#performance)
- [Repository layout](#repository-layout)
- [Development](#development)
- [Reporting security issues](#reporting-security-issues)

## Highlights

- **Two ways to describe users.** Declarative steps in TOML or JSON for the common cases, or an embedded script language for anything else: conditional flows, captured values, transactions, parallel page loads, custom checks.
- **Accurate numbers.** Open (arrival rate) and closed (concurrent users) load models, HDR histograms for latency, and arrival scheduling that does not hide slow responses (coordinated omission).
- **Many protocols.** HTTP/1.1 and HTTP/2, WebSocket, server-sent events and Socket.IO by default. gRPC, MQTT, Kafka and SQL are optional build features.
- **Pass or fail, automatically.** Thresholds on latency percentiles, error rate, throughput and dropped iterations, with optional early abort. A regression gate compares each run with the last passed run of the same plan.
- **Run manager for teams and CI.** A token-authenticated HTTP API stores test plans, queues runs, keeps reports and streams live statistics. Includes a browser console and an OpenAPI 3.1 description.
- **Client SDKs** for Python, Node.js, PHP, Go and Java. They send structured plans to the manager; no load is generated inside the client process.
- **Scale out.** Pool workers join the manager and receive each run's plan automatically. A run uses every idle worker, or a number you choose.
- **Reports everywhere.** Self-contained HTML, JSON, CSV, JMeter-compatible JTL, Prometheus metrics, InfluxDB and OpenTelemetry export, a Grafana dashboard, and signed webhooks.

## Quick start

Requirements: Rust 1.95 or newer.

```sh
git clone https://github.com/Kamvia/Ballista.git
cd Ballista
cargo build --release
```

The binary is `target/release/ballista`. Add `--features full` to include gRPC, MQTT, Kafka, SQL and OpenTelemetry support.

Try it against the bundled demo service:

```sh
# terminal 1: a small HTTP, WebSocket, SSE and Socket.IO service on 127.0.0.1:8080
cargo run --release --example demo_target

# terminal 2: run the example plan headless with live stats and reports
./target/release/ballista run examples/plan.toml
```

Useful variations:

```sh
ballista check examples/plan.toml                 # validate a plan without generating load
ballista run examples/plan.toml -u 200 -r 20 -t 2m # override users, ramp rate and run time
ballista debug examples/plan.toml                 # one user with a request and response trace
ballista run examples/plan.toml --web             # live dashboard while the run executes
ballista import recording.har -o plan.toml        # turn a browser recording into a plan
```

The live dashboard is password protected. Create a hash with `ballista hash-password` and pass it as `BALLISTA_ADMIN_HASH`. Without one, Ballista prints a one-time password at startup.

## Writing tests

A plan names the target, the load profile, one or more scenarios and the pass criteria. TOML and JSON are equivalent.

```toml
name = "checkout"

[target]
base_url = "https://shop.example.com/"
timeout = "5s"

[load]
mode = "users"                       # or "arrival_rate" for iterations per second
stages = [
  { duration = "30s", target = 50 }, # ramp to 50 users
  { duration = "5m",  target = 50 }, # hold
]

[[scenario]]
name = "buyer"
wait = { between = ["500ms", "2s"] }
on_start = [
  { feed = "accounts" },
  { post = "/login", name = "login", json = { username = "${accounts.username}", password = "${accounts.password}" }, capture = { token = "json.token" } },
]
step = [
  { get = "/items", name = "list items", capture = { item = "json.items.0.id" } },
  { txn_begin = "purchase" },
  { get = "/items/${item}", name = "item detail", contains = "stock" },
  { post = "/cart", name = "add to cart", headers = { authorization = "Bearer ${token}" }, json = { item = "${item}", qty = 1 }, expect = [201] },
  { txn_end = "purchase" },
]

[[feeder]]
name = "accounts"
path = "data/accounts.csv"

[[threshold]]
metric = "p95"
request = "purchase"
max = 400

[[threshold]]
metric = "error_rate"
max = 0.01
abort_on_fail = true
abort_delay = "30s"
```

When steps are not enough, point a scenario at a script instead. Scripts run inside the engine, one state machine per user:

```rust
// scenarios/shop.rhai
fn run(step, res) {
    switch step {
        0 => get("/items", "list items"),
        1 => {
            let id = res.json.items[rand_int(0, 9)].id;
            #{ path: `/items/${id}`, name: "item detail", check: |r| r.status == 200 }
        }
        2 => sleep(rand_int(200, 800)),
    }
}
```

Scripts can also open WebSocket, SSE and Socket.IO sessions, send batches of parallel requests, share values between users, and define a custom load shape with `fn shape(t)`. The `examples/` directory has a runnable plan for each feature.

## Run manager and web console

`ballista manager` turns a machine into a shared load-testing service. Clients upload plans, the manager queues and executes runs one at a time, and keeps every report.

```sh
# create tokens: read (view), run (upload and start), admin (also delete)
ballista api-token --name ci --scope run
ballista api-token --name ops --scope admin

export BALLISTA_API_TOKENS="ci:run:<sha256>,ops:admin:<sha256>"   # entries printed above
ballista manager --allow-target 'shop.example.com,*.staging.example.com' \
                 --max-users 5000 --max-run-time 1h
```

- **API:** `http://127.0.0.1:8070/api/v1`. The OpenAPI description is at `/api/v1/openapi.json`.
- **Console:** `http://127.0.0.1:8070/console/`. Sign in with an API token. You can follow live runs, compare runs, start runs from stored plans and upload new ones. What you can do follows the token's scope. Disable it with `--disable-console`.
- **Policy:** plans may only target hosts allowed by `--allow-target`, and only read environment variables allowed by `--allow-env`. User counts and run time are capped.
- **Regression gate:** ask for it when creating a run, and the run fails if p95, p99 or error rate is worse than the last passed run of the same plan beyond your limit. `GET /runs/{id}/compare` compares any two finished runs.
- **Webhooks:** run start and finish events are signed with HMAC-SHA256 (`--webhook-url` plus `BALLISTA_WEBHOOK_SECRET`).

## Client SDKs

Each SDK builds structured plans, uploads them with any asset files, starts runs, follows live statistics and downloads reports. They all validate input locally with the server's rules, retry safely with idempotency keys, never follow redirects, and refuse to send a token over plain HTTP to a remote host.

| Language | Path | Runtime dependencies | Includes |
|---|---|---|---|
| Python 3.11+ | [`sdks/python`](sdks/python) | httpx, pydantic | sync and async clients, CLI, 16 examples |
| Node.js 20+ | [`sdks/node`](sdks/node) | none | TypeScript types, CLI |
| PHP 8.2+ | [`sdks/php`](sdks/php) | ext-curl, ext-json | CLI, 16 examples |
| Go 1.23+ | [`sdks/go`](sdks/go) | none (standard library) | CLI |
| Java 21+ | [`sdks/java`](sdks/java) | none | CLI |

Python:

```python
from ballista_client import Client, Load, Plan, RegressionGate, Scenario, Target, Threshold
from ballista_client.plan import get

plan = Plan(
    name="smoke",
    target=Target(base_url="https://shop.example.com/"),
    load=Load.users(("10s", 20), ("1m", 20)),
    scenarios=[Scenario(name="browse", steps=[get("/items", name="list items", expect=200)])],
    thresholds=[Threshold(metric="p95", max=300)],
)

with Client() as c:  # BALLISTA_URL and BALLISTA_TOKEN
    run = c.run(plan, regression=RegressionGate(max_regression_pct=15))
    print(run.state, run.summary.p95_ms if run.summary else None)
```

Go:

```go
c, _ := ballista.New(ballista.Options{}) // BALLISTA_URL and BALLISTA_TOKEN
run, err := c.Run(ctx, plan, ballista.RunOptions{
    CreateRunOptions: ballista.CreateRunOptions{Overrides: &ballista.Overrides{Workers: ballista.Ptr(4)}},
})
```

```sh
go get github.com/Kamvia/Ballista/sdks/go
```

Each SDK also ships a `ballista-client` command for CI pipelines:

```sh
ballista-client submit plan.json --run-time 2m --label build=$CI_PIPELINE_ID --report ./perf-reports
```

It exits with 0 when the run passes, 2 when it fails or is stopped, 1 on errors and 3 on timeout. The Go and Java commands also take `--gate PCT` and `--workers N`.

## Deployment

The system under test, the manager and the clients can each be on their own machine or share one:

```
 developer laptop / CI job                load generator                  system under test
 +------------------------+   HTTPS    +-----------------------+   load   +---------------------+
 | SDK or ballista-client | ---------> | ballista manager      | -------> | web server, API,    |
 | browser console        |  /api/v1   |  + optional workers   |          | XAMPP, staging ...  |
 +------------------------+            +-----------------------+          +---------------------+
```

- The manager listens on `127.0.0.1` by default. On any other address it refuses plaintext unless you pass `--tls-cert` and `--tls-key`, or explicitly allow it behind a TLS-terminating proxy with `--allow-remote-plaintext`.
- Generate load from a machine with enough CPU and network capacity. The target's own host is rarely a good choice.
- Run the manager under a service manager with a dedicated, unprivileged user and a private `--data-dir`.

## Distributed load

**Worker pool (recommended).** Start the manager with a worker port, then start any number of workers without a plan:

```sh
export BALLISTA_CLUSTER_TOKEN=$(openssl rand -base64 48)
ballista manager --allow-target '*.staging.example.com' --cluster-bind 0.0.0.0:8071 \
                 --cluster-tls-cert cert.pem --cluster-tls-key key.pem

ballista worker --controller wss://loadgen.internal:8071 --ca-cert ca.pem --name worker-1
```

The manager sends each run's plan and files to the workers and splits the users between them. Use `workers` in a run's overrides to choose how many: `0` runs on the manager only, and leaving it out uses every idle worker.

**Standalone.** `ballista run plan.toml --master --expect-workers 4` coordinates workers started with `ballista worker plan.toml --controller ...` without a manager.

## Reports and integrations

| Output | How |
|---|---|
| HTML report (self-contained) | `--html report.html` or `[report] html` |
| JSON, CSV, per-request log | `--json`, `--csv PREFIX`, `--request-log` |
| JMeter JTL | `--jtl results.jtl` |
| Prometheus | `/metrics` on the live dashboard, protected by `--metrics-token` |
| InfluxDB line protocol | `[report.influx]` in the plan |
| OpenTelemetry OTLP/HTTP | `[report.otlp]` in the plan (feature `otel`) |
| Grafana | dashboard in `examples/grafana/dashboard.json` |

## Security model

- **Tokens:** API tokens are stored only as SHA-256 digests and carry one of three scopes: read, run or admin. Failed authentication is rate limited per client address.
- **Console sessions:** HttpOnly, SameSite=Strict cookies (`__Host-` prefixed under TLS) with server-side expiry. Every state-changing request needs a CSRF token and a same-origin check. Pages are served with a strict Content Security Policy.
- **Plans are untrusted input:** the manager restricts which hosts a plan may target and which environment variables it may read, caps users and run time, and validates bundle paths against traversal. Scripts run with operation, recursion and size limits.
- **HTML reports** are served sandboxed and with a CSP that blocks scripts.
- **Transport:** TLS via rustls. Cluster connections require a shared secret of at least 32 characters, and TLS outside loopback unless explicitly overridden.
- **Logs:** tokens and passwords are never logged, only token names. Debug traces and HAR imports redact cookies, authorization headers and other credentials.

## Performance

Measured on a 2-core virtual machine against the bundled demo target. Each user sends one request per second, with a 5 s ramp and a 20 s run. The load generator and the target share the machine.

| Tool | Users sustained | Throughput | p95 latency |
|---|---|---|---|
| Ballista | 10,000 | 8,072 req/s | 59 ms |
| Locust, fastest configuration tried | 6,847 (its ceiling) | 5,899 req/s | 2.4 s |

A later change shared one TLS configuration across all users instead of building one per user. At 10,000 users that cut memory from 1,068 MB to 323 MB, CPU time from 18.5 s to 11.1 s, and p95 latency from 59 ms to 14 ms.

Reproduce with `benchmarks/run.sh`. It needs release builds, Locust and psutil. Results depend heavily on hardware and on the target, so measure on your own setup before planning capacity.

## Repository layout

```
src/            engine, protocols, scripting, reports, live dashboard, run manager, cluster
assets/         dashboard and console front-end (embedded into the binary)
examples/       plans, scripts, data, demo target, Grafana dashboard
benchmarks/     comparison scripts
sdks/           python, node, php, go, java
```

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --features full -- -D warnings
cargo audit
cargo test --features full

cd sdks/python && pip install -e '.[dev]' && mypy --strict src && pytest
cd sdks/node   && npm ci && npm test
cd sdks/php    && composer install && vendor/bin/phpunit
cd sdks/go     && go vet ./... && go test -race ./...
cd sdks/java   && mvn test
```

The SDK suites include live integration tests that run when `BALLISTA_URL`, `BALLISTA_TOKEN`, `BALLISTA_READ_TOKEN` and `BALLISTA_DEMO_URL` point at a running manager and demo target. Admin-only tests also need `BALLISTA_ADMIN_TOKEN`.

## Reporting security issues

Please report vulnerabilities privately through [GitHub security advisories](https://github.com/Kamvia/Ballista/security/advisories/new) rather than public issues.

## License

[MIT](LICENSE)
