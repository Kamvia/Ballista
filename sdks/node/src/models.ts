// File: ballista/sdks/node/src/models.ts
import { BallistaError, ConfigurationError } from "./errors.js";

export const NAME_RE = /^[A-Za-z0-9_.-]{1,64}$/;
export const BUNDLE_ID_RE = /^[0-9a-f]{32}$/;
export const RUN_ID_RE = /^r[0-9a-f]{24}$/;
export const DURATION_RE = /^\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d)(\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d))*\s*$/;

export type Scope = "read" | "run" | "admin";
export type RunState = "queued" | "running" | "stopping" | "passed" | "failed" | "stopped" | "cancelled" | "error";
export const RUN_STATES: readonly RunState[] = ["queued", "running", "stopping", "passed", "failed", "stopped", "cancelled", "error"];
const TERMINAL: ReadonlySet<RunState> = new Set(["passed", "failed", "stopped", "cancelled", "error"]);

export function isTerminal(state: RunState): boolean {
  return TERMINAL.has(state);
}

export interface Health {
  status: "ok";
  version: string;
}

export interface WhoAmI {
  name: string;
  scope: Scope;
}

export interface ServerStatus {
  version: string;
  active_run: string | null;
  queued: number;
  limits: { max_users: number; max_run_time_secs: number; max_queue: number };
  workers: { id: number; name: string; address: string; cores: number; state: string; users: number; iterations: number; cpu_pct: number | null; saturated: boolean }[];
}

export interface BundleFile {
  path: string;
  size: number;
  sha256: string;
}

export interface BundleInfo {
  id: string;
  plan_name: string;
  scenarios: string[];
  files: BundleFile[];
  size: number;
  labels: Record<string, string>;
  created_at: string;
  created_by: string;
}

export interface RunOverrides {
  users?: number;
  spawn_rate?: number;
  run_time?: string;
  iterations?: number;
  tags?: string[];
  exclude_tags?: string[];
  host?: string;
  /** Pool workers to use: 0 runs on the server only; unset uses every available worker. */
  workers?: number;
}

export type CompareMetric = "p50" | "p95" | "p99" | "avg" | "error_rate" | "rps";

/** Fails a run that is worse than a baseline run beyond these limits. */
export interface RegressionGate {
  /** "auto" (latest passed run of the same plan) or a run id. */
  baseline?: string;
  max_regression_pct?: number;
  metrics?: CompareMetric[];
  min_samples?: number;
  min_change_ms?: number;
  max_error_rate_increase?: number;
}

export interface MetricChange {
  kind: string;
  method: string;
  name: string;
  metric: CompareMetric;
  baseline: number;
  current: number;
  change_pct: number | null;
  regressed: boolean;
}

export interface Comparison {
  baseline: string | null;
  passed: boolean;
  regressions: number;
  changes: MetricChange[];
  unmatched: string[];
  note: string | null;
}

export function checkGate(g: RegressionGate): RegressionGate {
  const allowed = new Set(["baseline", "max_regression_pct", "metrics", "min_samples", "min_change_ms", "max_error_rate_increase"]);
  for (const k of Object.keys(g)) if (!allowed.has(k)) throw new ConfigurationError(`unknown regression option '${k}'`);
  if (g.baseline !== undefined && g.baseline !== "auto" && !RUN_ID_RE.test(g.baseline)) throw new ConfigurationError("baseline must be 'auto' or a run id");
  const nonneg = (n: unknown): boolean => typeof n === "number" && Number.isFinite(n) && n >= 0;
  for (const k of ["max_regression_pct", "min_samples", "min_change_ms", "max_error_rate_increase"] as const) {
    if (g[k] !== undefined && !nonneg(g[k])) throw new ConfigurationError(`${k} must be a non-negative number`);
  }
  if (g.metrics !== undefined && (g.metrics.length === 0 || g.metrics.length > 6)) throw new ConfigurationError("metrics must list 1..6 metrics");
  return { ...g };
}

export interface ThresholdOutcome {
  metric: string;
  request: string;
  min: number | null;
  max: number | null;
  actual: number | null;
  passed: boolean;
}

export interface Summary {
  requests: number;
  failures: number;
  fail_ratio: number;
  rps: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  max_ms: number;
  iterations: number;
  duration_secs: number;
  aborted: boolean;
  generator_saturated_secs: number;
  thresholds: ThresholdOutcome[];
}

export interface Run {
  id: string;
  seq: number;
  bundle: string;
  plan_name: string;
  state: RunState;
  created_at: string;
  created_by: string;
  started_at?: string | null;
  ended_at?: string | null;
  labels: Record<string, string>;
  overrides: RunOverrides;
  passed?: boolean | null;
  error?: string | null;
  summary?: Summary | null;
  stopped_by?: string;
  workers?: number;
  regression?: RegressionGate;
  comparison?: Comparison;
  idempotency_key?: string;
  queue_position?: number;
}

export interface StatsRow {
  scenario: string;
  method: string;
  name: string;
  count: number;
  failures: number;
  fail_ratio: number;
  rps: number;
  fail_rps: number;
  rps_avg: number;
  avg_ms: number;
  min_ms: number;
  max_ms: number;
  p50_ms: number;
  p90_ms: number;
  p95_ms: number;
  p99_ms: number;
  p999_ms: number;
  avg_bytes: number;
  percentiles: number[];
}

export interface Snapshot {
  plan: string;
  state: string;
  elapsed_secs: number;
  users: number;
  target: number;
  total: StatsRow;
  rows: StatsRow[];
  transactions: StatsRow[];
  iterations: StatsRow[];
  errors: Record<string, unknown>[];
  thresholds: Record<string, unknown>[];
  distribution: { upper_ms: number | null; count: number }[];
  iterations_started: number;
  dropped_iterations: number;
  aborted: boolean;
  [extra: string]: unknown;
}

/** Server-side validation result: files a bundle must still carry and the normalized plan. */
export interface PlanCheck {
  name: string;
  scenarios: string[];
  files: string[];
  document: Record<string, unknown>;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

export type RunEvent =
  | { type: "run"; run: Run }
  | { type: "end"; run: Run }
  | { type: "stats"; stats: Snapshot };

/** Finds a request or transaction row by name (and method). */
export function findRow(s: Snapshot, name: string, method?: string): StatsRow | undefined {
  return [...s.rows, ...s.transactions].find((r) => r.name === name && (method === undefined || r.method === method));
}

// ------------------------------------------------------------- response checks

function fail(what: string): never {
  throw new BallistaError(`unexpected response: ${what}`);
}

function obj(v: unknown, what: string): Record<string, unknown> {
  if (typeof v !== "object" || v === null || Array.isArray(v)) fail(`${what} is not an object`);
  return v as Record<string, unknown>;
}

function str(o: Record<string, unknown>, k: string, what: string): void {
  if (typeof o[k] !== "string") fail(`${what}.${k} is not a string`);
}

function num(o: Record<string, unknown>, k: string, what: string): void {
  if (typeof o[k] !== "number" || !Number.isFinite(o[k])) fail(`${what}.${k} is not a number`);
}

export function asRun(v: unknown): Run {
  const o = obj(v, "run");
  for (const k of ["id", "bundle", "plan_name", "created_at", "created_by"]) str(o, k, "run");
  num(o, "seq", "run");
  if (!RUN_STATES.includes(o.state as RunState)) fail("run.state is unknown");
  if (!RUN_ID_RE.test(o.id as string)) fail("run.id is malformed");
  o.labels ??= {};
  o.overrides ??= {};
  return o as unknown as Run;
}

export function asBundle(v: unknown): BundleInfo {
  const o = obj(v, "bundle");
  for (const k of ["id", "plan_name", "created_at", "created_by"]) str(o, k, "bundle");
  if (!Array.isArray(o.files) || !Array.isArray(o.scenarios)) fail("bundle lists are missing");
  return o as unknown as BundleInfo;
}

export function asPage<T>(v: unknown, item: (x: unknown) => T): Page<T> {
  const o = obj(v, "page");
  if (!Array.isArray(o.items)) fail("page.items is not an array");
  const next = o.next_cursor;
  if (next !== null && next !== undefined && typeof next !== "string") fail("page.next_cursor is malformed");
  return { items: o.items.map(item), next_cursor: (next as string | null | undefined) ?? null };
}

const EMPTY_ROW: StatsRow = {
  scenario: "", method: "", name: "", count: 0, failures: 0, fail_ratio: 0, rps: 0, fail_rps: 0, rps_avg: 0,
  avg_ms: 0, min_ms: 0, max_ms: 0, p50_ms: 0, p90_ms: 0, p95_ms: 0, p99_ms: 0, p999_ms: 0, avg_bytes: 0, percentiles: [],
};

export function asSnapshot(v: unknown): Snapshot {
  const o = obj(v, "snapshot");
  const row = (r: unknown): StatsRow => ({ ...EMPTY_ROW, ...obj(r, "row") });
  const rows = (k: string): StatsRow[] => (Array.isArray(o[k]) ? (o[k] as unknown[]).map(row) : []);
  return {
    plan: "", state: "", elapsed_secs: 0, users: 0, target: 0, errors: [], thresholds: [], distribution: [],
    iterations_started: 0, dropped_iterations: 0, aborted: false,
    ...o,
    total: o.total ? row(o.total) : { ...EMPTY_ROW },
    rows: rows("rows"),
    transactions: rows("transactions"),
    iterations: rows("iterations"),
  };
}

export function asObject<T>(v: unknown, what: string, keys: string[]): T {
  const o = obj(v, what);
  for (const k of keys) if (!(k in o)) fail(`${what}.${k} is missing`);
  return o as T;
}

// ------------------------------------------------------------- request checks

export function checkLabels(labels: Record<string, string>): Record<string, string> {
  const entries = Object.entries(labels);
  if (entries.length > 32) throw new ConfigurationError("at most 32 labels");
  for (const [k, v] of entries) {
    if (!NAME_RE.test(k)) throw new ConfigurationError(`label key '${k}' must match [A-Za-z0-9_.-]{1,64}`);
    // eslint-disable-next-line no-control-regex
    if (typeof v !== "string" || v.length > 256 || /[\u0000-\u001f\u007f]/.test(v)) {
      throw new ConfigurationError(`label '${k}' must be up to 256 printable characters`);
    }
  }
  return { ...labels };
}

export function checkOverrides(o: RunOverrides): RunOverrides {
  const allowed = new Set(["users", "spawn_rate", "run_time", "iterations", "tags", "exclude_tags", "host", "workers"]);
  for (const k of Object.keys(o)) if (!allowed.has(k)) throw new ConfigurationError(`unknown override '${k}'`);
  const positive = (n: unknown): boolean => typeof n === "number" && Number.isFinite(n) && n > 0;
  if (o.users !== undefined && !positive(o.users)) throw new ConfigurationError("users must be positive");
  if (o.spawn_rate !== undefined && !positive(o.spawn_rate)) throw new ConfigurationError("spawn_rate must be positive");
  if (o.iterations !== undefined && !(Number.isInteger(o.iterations) && o.iterations >= 1)) {
    throw new ConfigurationError("iterations must be a positive integer");
  }
  if (o.workers !== undefined && !(Number.isInteger(o.workers) && o.workers >= 0 && o.workers <= 512)) {
    throw new ConfigurationError("workers must be an integer 0..512");
  }
  if (o.run_time !== undefined && !DURATION_RE.test(o.run_time)) throw new ConfigurationError("run_time must be a duration such as '90s'");
  for (const list of [o.tags, o.exclude_tags]) {
    if (list !== undefined && (list.length > 64 || !list.every((t) => NAME_RE.test(t)))) {
      throw new ConfigurationError("tags must be up to 64 simple names");
    }
  }
  if (o.host !== undefined && !/^https?:\/\/[^\s/@]+(\/\S*)?$/.test(o.host)) {
    throw new ConfigurationError("host must be an http(s) base URL without credentials");
  }
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as RunOverrides;
}
