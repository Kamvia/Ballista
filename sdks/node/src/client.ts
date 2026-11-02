// File: ballista/sdks/node/src/client.ts
import { randomUUID } from "node:crypto";
import { writeFile, rename, stat } from "node:fs/promises";
import { isIP } from "node:net";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { Bundle, type PlanInput, planPayload } from "./bundle.js";
import { Plan } from "./plan.js";
import { ApiError, ConfigurationError, RunTimeoutError, TransportError, errorFor } from "./errors.js";
import {
  BUNDLE_ID_RE,
  RUN_ID_RE,
  RUN_STATES,
  type BundleInfo,
  type CompareMetric,
  type Comparison,
  type RegressionGate,
  checkGate,
  type Health,
  type Page,
  type PlanCheck,
  type Run,
  type RunEvent,
  type RunOverrides,
  type RunState,
  type ServerStatus,
  type Snapshot,
  type WhoAmI,
  asBundle,
  asObject,
  asPage,
  asRun,
  asSnapshot,
  checkLabels,
  checkOverrides,
  isTerminal,
} from "./models.js";
import { parseSse } from "./sse.js";

export const TOKEN_ENV = "BALLISTA_TOKEN";
export const URL_ENV = "BALLISTA_URL";
const USER_AGENT = "ballista-client-node/0.1.0";
const RETRY_STATUSES = new Set([429, 502, 503, 504]);
export const ARTIFACTS = ["report.json", "report.html", "stats.csv", "history.csv", "failures.csv"] as const;
export type Artifact = (typeof ARTIFACTS)[number];

export interface RetryPolicy {
  attempts: number;
  baseDelayMs: number;
  maxDelayMs: number;
}

export interface ClientOptions {
  /** Server url, e.g. https://loadtest.example.com (or BALLISTA_URL). */
  url?: string;
  /** API token (or BALLISTA_TOKEN). */
  token?: string;
  /** Per-request timeout. */
  timeoutMs?: number;
  retry?: Partial<RetryPolicy>;
  /** Permit plain http to a non-loopback host (the token is sent in clear). */
  allowInsecureHttp?: boolean;
  /** Custom fetch, e.g. with a dispatcher for private CAs (or set NODE_EXTRA_CA_CERTS). */
  fetch?: typeof fetch;
}

export interface WaitOptions {
  timeoutMs?: number;
  pollIntervalMs?: number;
  onStats?: (s: Snapshot) => void;
  onState?: (r: Run) => void;
  signal?: AbortSignal;
}

export interface CreateRunOptions {
  overrides?: RunOverrides;
  labels?: Record<string, string>;
  idempotencyKey?: string;
  /** Fail the run when it is worse than its baseline. */
  regression?: RegressionGate;
}

interface RequestOpts {
  body?: unknown;
  headers?: Record<string, string>;
  retryable: boolean;
  query?: Record<string, string>;
}

function isLoopback(host: string): boolean {
  const h = host.replace(/^\[|\]$/g, "");
  if (h === "localhost") return true;
  if (isIP(h) === 4) return h.startsWith("127.");
  return isIP(h) === 6 && h === "::1";
}

function baseUrl(raw: string | undefined, allowInsecure: boolean): string {
  const value = (raw ?? process.env[URL_ENV] ?? "").trim();
  if (!value) throw new ConfigurationError(`server url required (option or ${URL_ENV})`);
  let u: URL;
  try {
    u = new URL(value);
  } catch {
    throw new ConfigurationError("server url is not a valid URL");
  }
  if (!["http:", "https:"].includes(u.protocol) || u.username || u.password || !u.hostname) {
    throw new ConfigurationError("server url must be http(s)://host[:port] without credentials");
  }
  if (u.protocol === "http:" && !isLoopback(u.hostname) && !allowInsecure) {
    throw new ConfigurationError("refusing to send the token over plain http to a remote host; use https");
  }
  return `${u.protocol}//${u.host}/api/v1`;
}

function checkId(id: string, re: RegExp, what: string): string {
  if (!re.test(id)) throw new ConfigurationError(`invalid ${what} id '${id}'`);
  return id;
}

function retryAfter(res: Response): number | null {
  const v = res.headers.get("retry-after");
  const n = v === null ? NaN : Number(v);
  return Number.isFinite(n) ? n : null;
}

async function raiseFor(res: Response): Promise<void> {
  if (res.ok) return;
  let code = "error";
  let detail = res.statusText || "request failed";
  try {
    const body = (await res.json()) as { error?: { code?: unknown; message?: unknown } };
    if (typeof body.error?.code === "string") code = body.error.code;
    if (typeof body.error?.message === "string") detail = body.error.message;
  } catch {
    // non-JSON error body
  }
  throw errorFor(res.status, code, detail.slice(0, 1000), retryAfter(res));
}

/** Client for the run manager API. All methods are async; requests are retried where safe. */
export class Client {
  readonly baseUrl: string;
  readonly #token: string;
  readonly #timeoutMs: number;
  readonly #retry: RetryPolicy;
  readonly #fetch: typeof fetch;

  constructor(options: ClientOptions = {}) {
    this.baseUrl = baseUrl(options.url, options.allowInsecureHttp ?? false);
    const token = (options.token ?? process.env[TOKEN_ENV] ?? "").trim();
    if (token.length < 32 || /\s/.test(token)) throw new ConfigurationError(`api token required (option or ${TOKEN_ENV})`);
    this.#token = token;
    this.#timeoutMs = options.timeoutMs ?? 30_000;
    this.#retry = { attempts: 4, baseDelayMs: 500, maxDelayMs: 20_000, ...options.retry };
    this.#fetch = options.fetch ?? globalThis.fetch;
  }

  toString(): string {
    return `Client(${this.baseUrl})`;
  }

  [Symbol.for("nodejs.util.inspect.custom")](): string {
    return this.toString();
  }

  #delay(attempt: number, after: number | null): number {
    if (after !== null) return Math.min(Math.max(after * 1000, 0), this.#retry.maxDelayMs);
    const backoff = Math.min(this.#retry.baseDelayMs * 2 ** attempt, this.#retry.maxDelayMs);
    return backoff * (0.5 + Math.random() / 2);
  }

  #headers(extra: Record<string, string> = {}): Record<string, string> {
    return { authorization: `Bearer ${this.#token}`, "user-agent": USER_AGENT, accept: "application/json", ...extra };
  }

  async #request(method: string, path: string, opts: RequestOpts): Promise<Response> {
    const attempts = opts.retryable ? Math.max(1, this.#retry.attempts) : 1;
    const url = new URL(this.baseUrl + path);
    for (const [k, v] of Object.entries(opts.query ?? {})) url.searchParams.set(k, v);
    const headers = this.#headers(opts.headers);
    let body: string | undefined;
    if (opts.body !== undefined) {
      body = JSON.stringify(opts.body);
      headers["content-type"] = "application/json";
    }
    for (let attempt = 0; attempt < attempts; attempt++) {
      let res: Response;
      try {
        const init: RequestInit = { method, headers, redirect: "manual", signal: AbortSignal.timeout(this.#timeoutMs) };
        if (body !== undefined) init.body = body;
        res = await this.#fetch(url, init);
      } catch (e) {
        if (attempt + 1 >= attempts) throw new TransportError(`${method} ${path}: ${(e as Error).name}`, { cause: e });
        await sleep(this.#delay(attempt, null));
        continue;
      }
      if (RETRY_STATUSES.has(res.status) && attempt + 1 < attempts) {
        await res.body?.cancel();
        await sleep(this.#delay(attempt, retryAfter(res)));
        continue;
      }
      await raiseFor(res);
      return res;
    }
    throw new TransportError(`${method} ${path}: retries exhausted`);
  }

  async #json(method: string, path: string, opts: RequestOpts): Promise<unknown> {
    const res = await this.#request(method, path, opts);
    return res.status === 204 ? null : res.json();
  }

  async health(): Promise<Health> {
    return asObject<Health>(await this.#json("GET", "/health", { retryable: true }), "health", ["status", "version"]);
  }

  async whoami(): Promise<WhoAmI> {
    return asObject<WhoAmI>(await this.#json("GET", "/whoami", { retryable: true }), "whoami", ["name", "scope"]);
  }

  async status(): Promise<ServerStatus> {
    return asObject<ServerStatus>(await this.#json("GET", "/status", { retryable: true }), "status", ["version", "queued", "limits"]);
  }

  /** Validates a plan (as it would run with `overrides`) without storing it; lists files a bundle must still carry. */
  async validatePlan(plan: PlanInput, overrides?: RunOverrides): Promise<PlanCheck> {
    const body = { plan: planPayload(plan), ...(overrides ? { overrides: checkOverrides(overrides) } : {}) };
    return asObject<PlanCheck>(await this.#json("POST", "/plans/validate", { body, retryable: true }), "plan check", ["name", "scenarios", "files", "document"]);
  }

  /** Uploads a plan (with assets); identical content returns the stored bundle. Safe to retry. */
  async uploadBundle(bundle: Bundle | Plan): Promise<BundleInfo> {
    const b = bundle instanceof Bundle ? bundle : new Bundle(bundle);
    return asBundle(await this.#json("POST", "/bundles", { body: b.toPayload(), retryable: true }));
  }

  async listBundles(): Promise<BundleInfo[]> {
    return asPage(await this.#json("GET", "/bundles", { retryable: true }), asBundle).items;
  }

  async getBundle(id: string): Promise<BundleInfo> {
    return asBundle(await this.#json("GET", `/bundles/${checkId(id, BUNDLE_ID_RE, "bundle")}`, { retryable: true }));
  }

  async deleteBundle(id: string): Promise<void> {
    await this.#request("DELETE", `/bundles/${checkId(id, BUNDLE_ID_RE, "bundle")}`, { retryable: true });
  }

  /** Queues a run. A generated idempotency key makes retries safe against duplicate runs. */
  async createRun(bundle: string | BundleInfo, opts: CreateRunOptions = {}): Promise<Run> {
    const id = checkId(typeof bundle === "string" ? bundle : bundle.id, BUNDLE_ID_RE, "bundle");
    const key = opts.idempotencyKey ?? randomUUID();
    if (!/^[\x21-\x7e]{1,128}$/.test(key)) throw new ConfigurationError("idempotency key must be 1..128 printable characters");
    const body = {
      bundle: id,
      overrides: checkOverrides(opts.overrides ?? {}),
      labels: checkLabels(opts.labels ?? {}),
      ...(opts.regression ? { regression: checkGate(opts.regression) } : {}),
    };
    return asRun(await this.#json("POST", "/runs", { body, headers: { "idempotency-key": key }, retryable: true }));
  }

  async listRuns(opts: { state?: RunState; limit?: number; cursor?: string } = {}): Promise<Page<Run>> {
    const query: Record<string, string> = { limit: String(Math.max(1, Math.min(opts.limit ?? 50, 200))) };
    if (opts.state) {
      if (!RUN_STATES.includes(opts.state)) throw new ConfigurationError(`unknown state '${opts.state}'`);
      query.state = opts.state;
    }
    if (opts.cursor) query.cursor = opts.cursor;
    return asPage(await this.#json("GET", "/runs", { retryable: true, query }), asRun);
  }

  async *iterRuns(opts: { state?: RunState; pageSize?: number } = {}): AsyncGenerator<Run> {
    let cursor: string | undefined;
    for (;;) {
      const page = await this.listRuns({ ...(opts.state ? { state: opts.state } : {}), limit: opts.pageSize ?? 100, ...(cursor ? { cursor } : {}) });
      yield* page.items;
      if (!page.next_cursor) return;
      cursor = page.next_cursor;
    }
  }

  async getRun(id: string): Promise<Run> {
    return asRun(await this.#json("GET", `/runs/${checkId(id, RUN_ID_RE, "run")}`, { retryable: true }));
  }

  async stopRun(id: string): Promise<Run> {
    return asRun(await this.#json("POST", `/runs/${checkId(id, RUN_ID_RE, "run")}/stop`, { retryable: true }));
  }

  async deleteRun(id: string): Promise<void> {
    await this.#request("DELETE", `/runs/${checkId(id, RUN_ID_RE, "run")}`, { retryable: true });
  }

  async getStats(id: string): Promise<Snapshot> {
    return asSnapshot(await this.#json("GET", `/runs/${checkId(id, RUN_ID_RE, "run")}/stats`, { retryable: true }));
  }

  /** Compares a finished run with a baseline (default: the latest earlier passed run of the same plan). */
  async compare(id: string, opts: { baseline?: string; maxRegressionPct?: number; metrics?: CompareMetric[] } = {}): Promise<Comparison> {
    const query: Record<string, string> = { baseline: opts.baseline ?? "auto" };
    if (opts.maxRegressionPct !== undefined) query.max_regression_pct = String(opts.maxRegressionPct);
    if (opts.metrics?.length) query.metrics = opts.metrics.join(",");
    const out = await this.#json("GET", `/runs/${checkId(id, RUN_ID_RE, "run")}/compare`, { retryable: true, query });
    return asObject<Comparison>(out, "comparison", ["passed", "regressions", "changes"]);
  }

  async artifact(id: string, name: Artifact): Promise<Buffer> {
    if (!ARTIFACTS.includes(name)) throw new ConfigurationError(`unknown artifact '${name}'`);
    const res = await this.#request("GET", `/runs/${checkId(id, RUN_ID_RE, "run")}/artifacts/${name}`, { retryable: true });
    return Buffer.from(await res.arrayBuffer());
  }

  async report(id: string): Promise<Record<string, unknown>> {
    return JSON.parse((await this.artifact(id, "report.json")).toString("utf8")) as Record<string, unknown>;
  }

  /** Writes an artifact to `dest` (a file path, or an existing directory to place it in). */
  async download(id: string, name: Artifact, dest: string): Promise<string> {
    const isDir = await stat(dest).then((s) => s.isDirectory(), () => false);
    const target = isDir ? join(dest, `${id}-${name}`) : dest;
    const tmp = `${target}.part`;
    await writeFile(tmp, await this.artifact(id, name), { mode: 0o644 });
    await rename(tmp, target);
    return target;
  }

  /** Streams run events until the run finishes (`end` is the last event). */
  async *events(id: string, opts: { signal?: AbortSignal; idleTimeoutMs?: number } = {}): AsyncGenerator<RunEvent> {
    const path = `/runs/${checkId(id, RUN_ID_RE, "run")}/events`;
    const idle = new AbortController();
    const signal = opts.signal ? AbortSignal.any([opts.signal, idle.signal]) : idle.signal;
    const idleMs = opts.idleTimeoutMs ?? 60_000;
    let timer = setTimeout(() => idle.abort(), idleMs);
    try {
      let res: Response;
      try {
        res = await this.#fetch(this.baseUrl + path, { headers: this.#headers({ accept: "text/event-stream" }), redirect: "manual", signal });
      } catch (e) {
        throw new TransportError(`GET ${path}: ${(e as Error).name}`, { cause: e });
      }
      await raiseFor(res);
      if (!res.body) throw new TransportError("event stream has no body");
      const decoder = new TextDecoder();
      async function* chunks(body: ReadableStream<Uint8Array>): AsyncGenerator<string> {
        for await (const chunk of body) {
          clearTimeout(timer);
          timer = setTimeout(() => idle.abort(), idleMs);
          yield decoder.decode(chunk, { stream: true });
        }
      }
      try {
        for await (const ev of parseSse(chunks(res.body))) {
          let out: RunEvent | null = null;
          if (ev.event === "stats") out = { type: "stats", stats: asSnapshot(JSON.parse(ev.data)) };
          else if (ev.event === "run" || ev.event === "end") out = { type: ev.event, run: asRun(JSON.parse(ev.data)) };
          if (out) {
            yield out;
            if (out.type === "end") return;
          }
        }
      } catch (e) {
        if (e instanceof Error && (e.name === "AbortError" || e.name === "TypeError")) throw new TransportError(`event stream interrupted: ${e.name}`, { cause: e });
        throw e;
      }
    } finally {
      clearTimeout(timer);
    }
  }

  /** Resolves when the run finishes. Uses the event stream and falls back to polling. */
  async wait(id: string, opts: WaitOptions = {}): Promise<Run> {
    const deadline = opts.timeoutMs === undefined ? Infinity : Date.now() + opts.timeoutMs;
    const timeoutSignal = opts.timeoutMs === undefined ? undefined : AbortSignal.timeout(opts.timeoutMs);
    const signals = [opts.signal, timeoutSignal].filter((s): s is AbortSignal => s !== undefined);
    const signal = signals.length ? AbortSignal.any(signals) : undefined;
    const poll = opts.pollIntervalMs ?? 2000;
    const expired = (): RunTimeoutError => new RunTimeoutError(`run ${id} did not finish within ${opts.timeoutMs}ms`);
    for (;;) {
      try {
        for await (const ev of this.events(id, { ...(signal ? { signal } : {}), idleTimeoutMs: Math.max(poll * 10, 30_000) })) {
          if (ev.type === "stats") opts.onStats?.(ev.stats);
          else {
            opts.onState?.(ev.run);
            if (ev.type === "end") return ev.run;
          }
        }
      } catch (e) {
        if (timeoutSignal?.aborted) throw expired();
        if (opts.signal?.aborted) throw e;
        if (e instanceof ApiError && !RETRY_STATUSES.has(e.status)) throw e;
        if (!(e instanceof TransportError) && !(e instanceof ApiError)) throw e;
      }
      const run = await this.getRun(id);
      if (isTerminal(run.state)) return run;
      if (Date.now() > deadline) throw expired();
      await sleep(poll, undefined, signal ? { signal } : {}).catch(() => {
        throw timeoutSignal?.aborted ? expired() : new RunTimeoutError("wait aborted");
      });
    }
  }

  /** Uploads (when given a Plan or Bundle), queues and optionally waits for a run. */
  async run(bundle: Plan | Bundle | BundleInfo | string, opts: CreateRunOptions & WaitOptions & { wait?: boolean } = {}): Promise<Run> {
    const target = bundle instanceof Bundle || bundle instanceof Plan ? await this.uploadBundle(bundle) : bundle;
    const run = await this.createRun(target, opts);
    return opts.wait === false ? run : this.wait(run.id, opts);
  }
}
