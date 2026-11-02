// File: ballista/sdks/node/test/planBuilder.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";

import { ConfigurationError, Load, Plan, steps } from "../dist/index.js";
import { shopPlan } from "./fixtures.mjs";

const { get, think } = steps;

test("produces a JSON-ready structured document", () => {
  const doc = shopPlan().toDocument();
  JSON.stringify(doc);
  assert.deepEqual(doc.load, { mode: "users", stages: [{ duration: "2s", target: 4 }, { duration: "4s", target: 4 }] });
  const sc = doc.scenario[0];
  assert.deepEqual(sc.wait, { between: ["50ms", "100ms"] });
  assert.equal(sc.on_start[1].json.username, "${users.username}");
  assert.equal(sc.step.length, 7);
  assert.equal(sc.step[2].txn_begin, "purchase");
  assert.deepEqual(sc.step[6].batch[1], { get: "/slow", name: "slow page" });
  assert.deepEqual(doc.feeder[0].rows[0], { username: "user1", password: "secret-pass" });
  assert.deepEqual(doc.threshold[0], { request: "purchase", metric: "p95", max: 1000 });
  assert.deepEqual(doc.report, { percentiles: [75, 99.9] });
});

test("inline script and shape", () => {
  const p = new Plan({ name: "x", target: { base_url: "http://127.0.0.1/" }, load: Load.shape("fn shape(t) { 1.0 }"), scenarios: [{ name: "s", source: "fn run(step, res) { () }" }] });
  const doc = p.toDocument();
  assert.deepEqual(doc.load, { mode: "users", shape_source: "fn shape(t) { 1.0 }" });
  assert.deepEqual(doc.scenario[0], { name: "s", source: "fn run(step, res) { () }" });
});

test("rejects invalid specs", () => {
  const base = { name: "p", target: { base_url: "http://x/" }, load: Load.users([["1s", 1]]) };
  const bad = [
    () => new Plan({ ...base, scenarios: [{ name: "bad name!", steps: [get("/")] }] }),
    () => new Plan({ ...base, target: { base_url: "ftp://x" }, scenarios: [{ name: "a", steps: [get("/")] }] }),
    () => new Plan({ ...base, scenarios: [{ name: "a", source: "x", steps: [get("/")] }] }),
    () => new Plan({ ...base, scenarios: [{ name: "a", steps: [get("/")] }], feeders: [{ name: "u", rows: [{ a: 1 }], path: "x.csv" }] }),
    () => new Plan({ ...base, scenarios: [{ name: "a", steps: [get("/", { capture: { "not-ident": "json.a" } })] }] }).toDocument(),
    () => new Plan({ ...base, scenarios: [{ name: "a", steps: [think("soon")] }] }).toDocument(),
    () => new Plan({ ...base, scenarios: [{ name: "a", steps: [{ get: "/", think: "1s" }] }] }).toDocument(),
  ];
  for (const f of bad) assert.throws(f, ConfigurationError);
});
