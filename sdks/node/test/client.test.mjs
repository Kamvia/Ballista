// File: ballista/sdks/node/test/client.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";

import { Client, ConfigurationError, NotFoundError, ValidationError } from "../dist/index.js";

const TOKEN = "bt_" + "x".repeat(43);
const BUNDLE = "0123456789abcdef0123456789abcdef";
const RUN_ID = "r" + "a".repeat(24);
const run = (state = "queued") => ({ id: RUN_ID, seq: 1, bundle: BUNDLE, plan_name: "p", state, created_at: "2026-11-01T10:00:00Z", created_by: "ci", labels: {}, overrides: {} });
const json = (status, body, headers = {}) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });
const client = (fetch) => new Client({ url: "http://127.0.0.1:8070", token: TOKEN, fetch, retry: { attempts: 3, baseDelayMs: 0 } });

test("refuses plain http to remote hosts, short tokens and malformed ids", async () => {
  assert.throws(() => new Client({ url: "http://example.com", token: TOKEN }), ConfigurationError);
  assert.throws(() => new Client({ url: "https://example.com", token: "short" }), ConfigurationError);
  const c = client(async () => json(200, {}));
  await assert.rejects(c.getRun("../../etc/passwd"), ConfigurationError);
  assert.equal(String(c).includes(TOKEN), false);
});

test("createRun retries with a single idempotency key", async () => {
  const keys = [];
  const c = client(async (url, init) => {
    assert.equal(init.headers.authorization, `Bearer ${TOKEN}`);
    keys.push(init.headers["idempotency-key"]);
    if (keys.length < 3) return json(503, { error: { code: "busy", message: "x" } }, { "retry-after": "0" });
    assert.deepEqual(JSON.parse(init.body), { bundle: BUNDLE, overrides: { users: 5, run_time: "30s" }, labels: { ci: "1" } });
    return json(201, run());
  });
  const r = await c.createRun(BUNDLE, { overrides: { users: 5, run_time: "30s" }, labels: { ci: "1" } });
  assert.equal(r.state, "queued");
  assert.equal(keys.length, 3);
  assert.equal(new Set(keys).size, 1);
});

test("maps error statuses and validates overrides locally", async () => {
  const c = client(async (url) =>
    String(url).endsWith("/runs")
      ? json(422, { error: { code: "invalid", message: "plan peak exceeds limit" } })
      : json(404, { error: { code: "not_found", message: "run not found" } }),
  );
  await assert.rejects(c.getRun(RUN_ID), NotFoundError);
  await assert.rejects(c.createRun(BUNDLE), (e) => e instanceof ValidationError && e.detail.includes("peak"));
  await assert.rejects(c.createRun(BUNDLE, { overrides: { users: -1 } }), ConfigurationError);
});

test("wait follows the event stream across chunk boundaries", async () => {
  const snap = { plan: "p", state: "running", users: 3, total: { count: 10, p95_ms: 12.5 } };
  const text = `event: run\ndata: ${JSON.stringify(run("running"))}\n\n: keep-alive\n\nevent: stats\r\ndata: ${JSON.stringify(snap)}\r\n\r\nevent: end\ndata: ${JSON.stringify(run("passed"))}\n\n`;
  const enc = new TextEncoder().encode(text);
  const body = new ReadableStream({
    start(ctrl) {
      for (let i = 0; i < enc.length; i += 7) ctrl.enqueue(enc.slice(i, i + 7));
      ctrl.close();
    },
  });
  const c = client(async () => new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } }));
  const seen = [];
  const done = await c.wait(RUN_ID, { onStats: (s) => seen.push(s.total.p95_ms) });
  assert.equal(done.state, "passed");
  assert.deepEqual(seen, [12.5]);
});
