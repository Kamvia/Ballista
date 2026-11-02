#!/usr/bin/env node
// File: ballista/sdks/node/src/cli.ts
import { readFileSync, realpathSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { Bundle } from "./bundle.js";
import { ARTIFACTS, type Artifact, Client } from "./client.js";
import { ApiError, BallistaError, RunTimeoutError } from "./errors.js";
import type { Run, RunOverrides, Snapshot } from "./models.js";

const EXIT_ERROR = 1;
const EXIT_FAILED = 2;
const EXIT_TIMEOUT = 3;

const USAGE = `usage: ballista-client [--url URL] <command>
  submit <plan.json|plan.toml> [--file REL=PATH]... [--users N] [--spawn-rate N] [--run-time D] [--iterations N]
         [--tags a,b] [--host URL] [--label k=v]... [--no-wait] [--timeout SECS] [--quiet] [--report DIR]
  status <run-id> | stop <run-id> | wait <run-id> | list
  report <run-id> [--name report.html] [--out DIR]`;

function summary(run: Run): void {
  console.log(`run ${run.id} ${run.state}`);
  if (run.error) console.log(`error: ${run.error}`);
  const s = run.summary;
  if (!s) return;
  console.log(
    `requests=${s.requests} failures=${s.failures} rps=${s.rps.toFixed(1)} p50=${s.p50_ms.toFixed(1)}ms ` +
      `p95=${s.p95_ms.toFixed(1)}ms p99=${s.p99_ms.toFixed(1)}ms duration=${s.duration_secs.toFixed(0)}s`,
  );
  for (const t of s.thresholds) {
    console.log(`threshold ${t.metric} ${t.request} actual=${t.actual === null ? "n/a" : t.actual.toFixed(3)} ${t.passed ? "pass" : "FAIL"}`);
  }
}

function progress(): (s: Snapshot) => void {
  let last = -1;
  return (s) => {
    const second = Math.floor(s.elapsed_secs);
    if (second === last) return;
    last = second;
    process.stderr.write(`  t=${second}s users=${s.users} rps=${s.total.rps.toFixed(1)} p95=${s.total.p95_ms.toFixed(1)}ms failures=${s.total.failures}\n`);
  };
}

const exitCode = (run: Run): number => (run.state === "passed" ? 0 : run.state === "failed" || run.state === "stopped" ? EXIT_FAILED : EXIT_ERROR);

function num(v: string | undefined, what: string): number | undefined {
  if (v === undefined) return undefined;
  const n = Number(v);
  if (!Number.isFinite(n)) throw new BallistaError(`${what} must be a number`);
  return n;
}

export async function main(argv: string[]): Promise<number> {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      url: { type: "string" },
      file: { type: "string", multiple: true },
      users: { type: "string" },
      "spawn-rate": { type: "string" },
      "run-time": { type: "string" },
      iterations: { type: "string" },
      tags: { type: "string" },
      host: { type: "string" },
      label: { type: "string", multiple: true },
      "no-wait": { type: "boolean" },
      timeout: { type: "string" },
      quiet: { type: "boolean" },
      report: { type: "string" },
      name: { type: "string" },
      out: { type: "string" },
      help: { type: "boolean", short: "h" },
    },
  });
  const [cmd, arg] = positionals;
  if (values.help || !cmd) {
    console.log(USAGE);
    return values.help ? 0 : EXIT_ERROR;
  }
  try {
    const client = new Client(values.url ? { url: values.url } : {});
    const need = (): string => {
      if (!arg) throw new BallistaError(`${cmd} requires an argument`);
      return arg;
    };
    switch (cmd) {
      case "submit": {
        const labels: Record<string, string> = {};
        for (const l of values.label ?? []) {
          const i = l.indexOf("=");
          if (i < 1) throw new BallistaError(`label '${l}' must be key=value`);
          labels[l.slice(0, i)] = l.slice(i + 1);
        }
        const raw = readFileSync(need(), "utf8");
        let plan: Record<string, unknown> | string = raw;
        if (need().toLowerCase().endsWith(".json")) {
          const doc: unknown = JSON.parse(raw);
          if (typeof doc !== "object" || doc === null || Array.isArray(doc)) throw new BallistaError("plan document must be a JSON object");
          plan = doc as Record<string, unknown>;
        }
        const bundle = new Bundle(plan, { labels });
        for (const spec of values.file ?? []) {
          const i = spec.indexOf("=");
          if (i < 1) throw new BallistaError(`--file '${spec}' must be REL=PATH`);
          bundle.addPath(spec.slice(0, i), spec.slice(i + 1));
        }
        const overrides: RunOverrides = {};
        const users = num(values.users, "users");
        const rate = num(values["spawn-rate"], "spawn-rate");
        const iterations = num(values.iterations, "iterations");
        if (users !== undefined) overrides.users = users;
        if (rate !== undefined) overrides.spawn_rate = rate;
        if (iterations !== undefined) overrides.iterations = iterations;
        if (values["run-time"]) overrides.run_time = values["run-time"];
        if (values.tags) overrides.tags = values.tags.split(",");
        if (values.host) overrides.host = values.host;
        const info = await client.uploadBundle(bundle);
        let run = await client.createRun(info, { overrides, labels });
        console.log(`queued run ${run.id} (bundle ${info.id})`);
        if (values["no-wait"]) return 0;
        const timeout = num(values.timeout, "timeout");
        run = await client.wait(run.id, { ...(timeout !== undefined ? { timeoutMs: timeout * 1000 } : {}), ...(values.quiet ? {} : { onStats: progress() }) });
        summary(run);
        if (values.report && run.summary) {
          await mkdir(values.report, { recursive: true });
          await client.download(run.id, "report.json", values.report);
          await client.download(run.id, "report.html", values.report);
        }
        return exitCode(run);
      }
      case "status":
        summary(await client.getRun(need()));
        return 0;
      case "stop":
        console.log(`run ${need()} ${(await client.stopRun(need())).state}`);
        return 0;
      case "wait": {
        const run = await client.wait(need());
        summary(run);
        return exitCode(run);
      }
      case "report": {
        const name = (values.name ?? "report.html") as Artifact;
        if (!ARTIFACTS.includes(name)) throw new BallistaError(`name must be one of ${ARTIFACTS.join(", ")}`);
        console.log(await client.download(need(), name, values.out ?? "."));
        return 0;
      }
      case "list":
        for (const r of (await client.listRuns({ limit: 50 })).items) {
          console.log(JSON.stringify({ id: r.id, state: r.state, plan: r.plan_name, created_at: r.created_at }));
        }
        return 0;
      default:
        console.error(USAGE);
        return EXIT_ERROR;
    }
  } catch (e) {
    if (e instanceof RunTimeoutError) {
      console.error(`error: ${e.message}`);
      return EXIT_TIMEOUT;
    }
    if (e instanceof ApiError) console.error(`error: ${e.status} ${e.code}: ${e.detail}`);
    else if (e instanceof BallistaError) console.error(`error: ${e.message}`);
    else throw e;
    return EXIT_ERROR;
  }
}

function invokedDirectly(): boolean {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return realpathSync(entry) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedDirectly()) {
  main(process.argv.slice(2)).then((code) => process.exit(code));
}
