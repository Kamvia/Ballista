// File: ballista/sdks/node/src/plan.ts
import { ConfigurationError } from "./errors.js";
import { DURATION_RE, NAME_RE } from "./models.js";

const IDENT = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

type Json = string | number | boolean | Json[] | { [k: string]: Json };

function duration(d: string, what: string): string {
  if (!DURATION_RE.test(d)) throw new ConfigurationError(`${what}: '${d}' is not a duration such as '500ms' or '2m'`);
  return d;
}

function name(n: string, what: string): string {
  if (!NAME_RE.test(n)) throw new ConfigurationError(`${what}: '${n}' must match [A-Za-z0-9_.-]{1,64}`);
  return n;
}

function idents(m: Record<string, string> | undefined, what: string): void {
  for (const k of Object.keys(m ?? {})) if (!IDENT.test(k)) throw new ConfigurationError(`${what}: variable '${k}' must be an identifier`);
}

function compact<T extends object>(o: T): T {
  return Object.fromEntries(
    Object.entries(o).filter(([, v]) => v !== undefined && v !== false && !(typeof v === "object" && v !== null && !Array.isArray(v) && Object.keys(v).length === 0) && !(Array.isArray(v) && v.length === 0)),
  ) as T;
}

export interface TargetSpec {
  base_url: string;
  allowed_hosts?: string[];
  timeout?: string;
  connect_timeout?: string;
  headers?: Record<string, string>;
  cookies?: boolean;
  retries?: number;
  retry_backoff?: string;
  retry_on?: number[];
  max_body_bytes?: number;
  http2_prior_knowledge?: boolean;
}

export interface StageSpec {
  duration: string;
  target: number;
}

export interface LoadSpec {
  mode?: "users" | "arrival_rate";
  stages?: StageSpec[];
  /** Inline shape script (`fn shape(t)`), instead of stages. */
  shape_source?: string;
  graceful_stop?: string;
  max_users?: number;
  iterations?: number;
  tags?: string[];
  exclude_tags?: string[];
}

export const Load = {
  /** Custom profile from a script defining `fn shape(t)`; the source travels inside the plan. */
  shape: (source: string, mode: "users" | "arrival_rate" = "users", extra: Omit<LoadSpec, "mode" | "stages" | "shape_source"> = {}): LoadSpec => ({
    mode,
    shape_source: source,
    ...extra,
  }),
  /** Closed model: each stage ramps to `target` concurrent users over `duration`. */
  users: (stages: [string, number][], extra: Omit<LoadSpec, "mode" | "stages"> = {}): LoadSpec => ({
    mode: "users",
    stages: stages.map(([d, t]) => ({ duration: d, target: t })),
    ...extra,
  }),
  /** Open model: each stage ramps to `target` iterations per second over `duration`. */
  arrivalRate: (stages: [string, number][], extra: Omit<LoadSpec, "mode" | "stages"> = {}): LoadSpec => ({
    mode: "arrival_rate",
    stages: stages.map(([d, t]) => ({ duration: d, target: t })),
    ...extra,
  }),
};

export type WaitSpec = { between: [string, string] } | { constant: string } | { pacing: string } | { throughput: number } | { poisson: string };

export const Wait = {
  uniform: (low: string, high: string): WaitSpec => ({ between: [low, high] }),
  fixed: (d: string): WaitSpec => ({ constant: d }),
  pace: (d: string): WaitSpec => ({ pacing: d }),
  perSecond: (rate: number): WaitSpec => ({ throughput: rate }),
  exponential: (mean: string): WaitSpec => ({ poisson: mean }),
};

/** One declarative step. Use the helpers below (get, post, think, ...). */
export interface Step {
  get?: string;
  post?: string;
  put?: string;
  patch?: string;
  delete?: string;
  head?: string;
  name?: string;
  headers?: Record<string, string>;
  query?: Record<string, string>;
  json?: Json;
  body?: string;
  form?: Record<string, string>;
  expect?: number | number[];
  contains?: string;
  capture?: Record<string, string>;
  timeout?: string;
  retries?: number;
  continue_on_failure?: boolean;
  sse?: number;
  reconnects?: number;
  context?: Record<string, string>;
  think?: string | [string, string];
  feed?: string;
  set?: Record<string, string>;
  txn_begin?: string;
  txn_end?: string;
  batch?: Step[];
  concurrency?: number;
}

export type RequestOptions = Omit<Step, "get" | "post" | "put" | "patch" | "delete" | "head" | "think" | "feed" | "set" | "txn_begin" | "txn_end" | "batch" | "concurrency">;

const request = (method: "get" | "post" | "put" | "patch" | "delete" | "head") => (path: string, opts: RequestOptions = {}): Step => ({ [method]: path, ...opts });

export const get = request("get");
export const post = request("post");
export const put = request("put");
export const patch = request("patch");
export const del = request("delete");
export const head = request("head");
export const think = (low: string, high?: string): Step => ({ think: high ? [low, high] : low });
export const feed = (feeder: string): Step => ({ feed: feeder });
export const assign = (vars: Record<string, string>): Step => ({ set: vars });
export const batch = (requests: Step[], opts: { name?: string; concurrency?: number } = {}): Step => ({
  batch: requests,
  name: opts.name ?? "batch",
  concurrency: opts.concurrency ?? 6,
});
/** Wraps steps so their total duration is recorded as one transaction. */
export const transaction = (txn: string, ...steps: Step[]): Step[] => [{ txn_begin: txn }, ...steps, { txn_end: txn }];

const METHODS = ["get", "post", "put", "patch", "delete", "head"] as const;

function checkStep(s: Step, where: string): Step {
  const methods = METHODS.filter((m) => s[m] !== undefined);
  const actions = (methods.length > 0 ? 1 : 0) + [s.think, s.feed, s.set, s.txn_begin, s.txn_end, s.batch].filter((x) => x !== undefined).length;
  if (methods.length > 1 || actions !== 1) throw new ConfigurationError(`${where}: a step does exactly one thing (request, think, feed, set, transaction boundary or batch)`);
  idents(s.capture, where);
  idents(s.set, where);
  if (s.timeout) duration(s.timeout, where);
  if (s.think !== undefined) (Array.isArray(s.think) ? s.think : [s.think]).forEach((d) => duration(d, where));
  if (s.txn_begin) name(s.txn_begin, where);
  if (s.txn_end) name(s.txn_end, where);
  if (s.retries !== undefined && !(Number.isInteger(s.retries) && s.retries >= 0 && s.retries <= 10)) throw new ConfigurationError(`${where}: retries must be 0..10`);
  if (s.concurrency !== undefined && !(Number.isInteger(s.concurrency) && s.concurrency >= 1 && s.concurrency <= 64)) throw new ConfigurationError(`${where}: concurrency must be 1..64`);
  if (s.batch) s.batch.forEach((b, i) => checkStep(b, `${where} batch[${i}]`));
  return compact({ ...s, ...(s.batch ? { batch: s.batch.map((b) => compact(b)) } : {}) });
}

export interface ScenarioSpec {
  name: string;
  weight?: number;
  /** Inline script source. */
  source?: string;
  /** Script file inside the bundle (sent as an asset). */
  script?: string;
  steps?: (Step | Step[])[];
  on_start?: (Step | Step[])[];
  wait?: WaitSpec;
  fixed_count?: number;
  tags?: string[];
}

export type Metric = "p50" | "p90" | "p95" | "p99" | "avg" | "max" | "error_rate" | "rps" | "dropped_iterations";

export interface ThresholdSpec {
  metric: Metric;
  request?: string;
  max?: number;
  min?: number;
  abort_on_fail?: boolean;
  abort_delay?: string;
}

export type Cell = string | number | boolean;

/** Test data: `rows` inline (objects, or arrays with `columns`), or `path` to a bundle asset. */
export interface FeederSpec {
  name: string;
  rows?: Record<string, Cell>[] | Cell[][];
  columns?: string[];
  path?: string;
  mode?: "sequential" | "random";
}

export interface PlanSpec {
  name: string;
  target: TargetSpec;
  load: LoadSpec;
  scenarios: ScenarioSpec[];
  thresholds?: ThresholdSpec[];
  feeders?: FeederSpec[];
  percentiles?: number[];
}

/** A validated plan; `toDocument()` is the structured form the server accepts. */
export class Plan {
  readonly spec: PlanSpec;

  constructor(spec: PlanSpec) {
    this.spec = spec;
    this.#validate();
  }

  #validate(): void {
    const s = this.spec;
    if (!s.name || s.name.length > 128) throw new ConfigurationError("plan name must be 1..128 characters");
    if (!/^https?:\/\/[^\s/@]+(\/\S*)?$/.test(s.target.base_url)) throw new ConfigurationError("target.base_url must be an http(s) URL without credentials");
    for (const k of ["timeout", "connect_timeout", "retry_backoff"] as const) if (s.target[k]) duration(s.target[k], `target.${k}`);
    const stages = s.load.stages ?? [];
    if ((stages.length > 0) === (s.load.shape_source !== undefined)) throw new ConfigurationError("load needs either stages or shape_source");
    for (const st of stages) {
      duration(st.duration, "load.stages");
      if (!(Number.isFinite(st.target) && st.target >= 0)) throw new ConfigurationError("stage targets must be non-negative");
    }
    if (!s.scenarios.length) throw new ConfigurationError("plan requires at least one scenario");
    const seen = new Set<string>();
    for (const sc of s.scenarios) {
      name(sc.name, "scenario");
      if (seen.has(sc.name)) throw new ConfigurationError(`duplicate scenario '${sc.name}'`);
      seen.add(sc.name);
      const kinds = [sc.source !== undefined, sc.script !== undefined, (sc.steps?.length ?? 0) > 0].filter(Boolean).length;
      if (kinds !== 1) throw new ConfigurationError(`scenario '${sc.name}' needs exactly one of source (inline script), script (bundle asset) or steps`);
      if (sc.weight !== undefined && !(Number.isInteger(sc.weight) && sc.weight >= 1 && sc.weight <= 10_000)) throw new ConfigurationError(`scenario '${sc.name}': weight must be 1..10000`);
      sc.tags?.forEach((t) => name(t, `scenario '${sc.name}' tags`));
    }
    for (const t of s.thresholds ?? []) {
      if (t.max === undefined && t.min === undefined) throw new ConfigurationError(`threshold ${t.metric} needs min or max`);
      if (t.abort_delay) duration(t.abort_delay, "threshold.abort_delay");
    }
    for (const f of s.feeders ?? []) {
      name(f.name, "feeder");
      if ((f.rows === undefined) === (f.path === undefined)) throw new ConfigurationError(`feeder '${f.name}' needs either rows or path`);
      if (f.rows !== undefined && f.rows.length === 0) throw new ConfigurationError(`feeder '${f.name}' has no rows`);
      if (f.columns !== undefined && f.rows === undefined) throw new ConfigurationError("feeder columns require rows");
    }
    if ((s.percentiles?.length ?? 0) > 12 || s.percentiles?.some((p) => !(p > 0 && p <= 100))) throw new ConfigurationError("percentiles: up to 12 values in (0, 100]");
  }

  toDocument(): Record<string, unknown> {
    const s = this.spec;
    const steps = (list: (Step | Step[])[] | undefined, where: string): Step[] =>
      (list ?? []).flat().map((st, i) => checkStep(st, `${where} step ${i + 1}`));
    const doc: Record<string, unknown> = {
      name: s.name,
      target: compact({ ...s.target }),
      load: compact({ ...s.load, mode: s.load.mode ?? "users", stages: s.load.stages?.map((st) => ({ duration: st.duration, target: st.target })) }),
      scenario: s.scenarios.map((sc) =>
        compact({
          name: sc.name,
          weight: sc.weight,
          source: sc.source,
          script: sc.script,
          wait: sc.wait,
          fixed_count: sc.fixed_count,
          tags: sc.tags,
          on_start: steps(sc.on_start, `scenario '${sc.name}' on_start`),
          step: steps(sc.steps, `scenario '${sc.name}'`),
        }),
      ),
    };
    if (s.feeders?.length) doc.feeder = s.feeders.map((f) => compact({ ...f, mode: f.mode ?? "sequential" }));
    if (s.thresholds?.length) doc.threshold = s.thresholds.map((t) => compact({ request: "*", ...t }));
    if (s.percentiles?.length) doc.report = { percentiles: s.percentiles };
    return doc;
  }
}
