// File: ballista/sdks/node/test/bundle.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";

import { Bundle, ConfigurationError, validBundlePath } from "../dist/index.js";
import { shopPlan } from "./fixtures.mjs";

test("bundle paths follow the server rule", () => {
  for (const bad of ["", "/etc/passwd", "../x", "a/../b", ".env", "a//b", "a b", "inline/x.csv"]) assert.equal(validBundlePath(bad), false, bad);
  assert.equal(validBundlePath("proto/api.pb"), true);
});

test("payload carries the structured plan and assets", () => {
  const p = new Bundle(shopPlan(), { labels: { ci: "1" } }).addBytes("proto/api.pb", new Uint8Array([0, 1])).toPayload();
  assert.equal(typeof p.plan, "object");
  assert.equal(p.plan.name, "shop api");
  assert.deepEqual(p.files, { "proto/api.pb": "AAE=" });
  assert.deepEqual(p.labels, { ci: "1" });
  assert.equal(typeof new Bundle('name = "x"').toPayload().plan, "string");
});

test("enforces limits and reserved names", () => {
  const b = new Bundle({ name: "x" });
  assert.throws(() => b.addBytes("plan.toml", new Uint8Array()), ConfigurationError);
  assert.throws(() => b.addBytes("big.bin", new Uint8Array(16 * 1024 * 1024 + 1)), ConfigurationError);
  assert.throws(() => new Bundle("   "), ConfigurationError);
  assert.throws(() => new Bundle({ name: "x" }, { labels: { "bad key": "v" } }), ConfigurationError);
});
