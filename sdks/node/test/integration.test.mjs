// File: ballista/sdks/node/test/integration.test.mjs
// Requires BALLISTA_URL, BALLISTA_TOKEN (run scope), BALLISTA_READ_TOKEN and BALLISTA_DEMO_URL; skipped otherwise.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Bundle, Client, PermissionDeniedError, ValidationError, findRow } from "../dist/index.js";
import { shopPlan } from "./fixtures.mjs";

const env = ["BALLISTA_URL", "BALLISTA_TOKEN", "BALLISTA_READ_TOKEN", "BALLISTA_DEMO_URL"];
const skip = env.some((k) => !process.env[k]) && "integration environment not configured";
const plan = (base) => shopPlan(base ?? process.env.BALLISTA_DEMO_URL);

test("full lifecycle", { skip, timeout: 120_000 }, async () => {
  const c = new Client();
  assert.equal((await c.whoami()).scope, "run");
  const check = await c.validatePlan(plan());
  assert.deepEqual(check.files, []);
  const info = await c.uploadBundle(plan());
  assert.equal((await c.uploadBundle(new Bundle(plan()))).id, info.id);
  const key = `node-${Date.now()}`;
  const run = await c.createRun(info, { overrides: { run_time: "5s" }, labels: { build: "7" }, idempotencyKey: key });
  assert.equal((await c.createRun(info, { overrides: { run_time: "5s" }, labels: { build: "7" }, idempotencyKey: key })).id, run.id);
  const counts = [];
  const done = await c.wait(run.id, { timeoutMs: 90_000, onStats: (s) => counts.push(s.total.count) });
  assert.equal(done.state, "passed", done.error ?? "");
  assert.ok(done.summary && done.summary.requests > 10);
  assert.ok(counts.length > 0);
  assert.ok(findRow(await c.getStats(run.id), "purchase"));
  const html = await c.download(run.id, "report.html", mkdtempSync(join(tmpdir(), "report-")));
  assert.ok(readFileSync(html, "utf8").startsWith("<!DOCTYPE html>"));
  let found = false;
  for await (const r of c.iterRuns({ state: "passed" })) if (r.id === run.id) found = true;
  assert.ok(found);
});

test("plan text reports missing assets", { skip, timeout: 60_000 }, async () => {
  const c = new Client();
  const text = `name = "text"\n[target]\nbase_url = "${process.env.BALLISTA_DEMO_URL}"\n[load]\nstages = [{ duration = "1s", target = 1 }]\n[[scenario]]\nname = "s"\nscript = "scenarios/s.rhai"\n`;
  assert.deepEqual((await c.validatePlan(text)).files, ["scenarios/s.rhai"]);
  await assert.rejects(c.uploadBundle(new Bundle(text)), ValidationError);
  const done = await c.run(new Bundle(text).addText("scenarios/s.rhai", 'fn run(step, res) { if step == 0 { get("/items") } }'), { overrides: { run_time: "2s" }, timeoutMs: 60_000 });
  assert.equal(done.state, "passed", done.error ?? "");
});

test("stop, cancel, policy and scopes", { skip, timeout: 120_000 }, async () => {
  const c = new Client();
  const info = await c.uploadBundle(plan());
  const first = await c.createRun(info, { overrides: { run_time: "60s" } });
  const second = await c.createRun(info, { overrides: { run_time: "60s" } });
  assert.equal((await c.stopRun(second.id)).state, "cancelled");
  for (let i = 0; i < 100 && (await c.getRun(first.id)).state === "queued"; i++) await new Promise((r) => setTimeout(r, 200));
  await c.stopRun(first.id);
  assert.equal((await c.wait(first.id, { timeoutMs: 60_000 })).state, "stopped");
  await assert.rejects(c.validatePlan(plan("http://10.0.0.1:8080/")), (e) => e instanceof ValidationError && e.detail.includes("not permitted"));
  await assert.rejects(c.createRun(info, { overrides: { users: 100_000, spawn_rate: 100_000, run_time: "10s" } }), ValidationError);
  const ro = new Client({ token: process.env.BALLISTA_READ_TOKEN ?? "" });
  assert.ok((await ro.status()).limits.max_queue > 0);
  await assert.rejects(ro.createRun(info), PermissionDeniedError);
});
