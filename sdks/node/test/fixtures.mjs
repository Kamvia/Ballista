// File: ballista/sdks/node/test/fixtures.mjs
import { Load, Plan, Wait, steps } from "../dist/index.js";

const { batch, feed, get, post, think, transaction } = steps;

export function shopPlan(baseUrl = "http://127.0.0.1:8080/") {
  return new Plan({
    name: "shop api",
    target: { base_url: baseUrl, headers: { accept: "application/json" } },
    load: Load.users([["2s", 4], ["4s", 4]]),
    scenarios: [
      {
        name: "buyer",
        wait: Wait.uniform("50ms", "100ms"),
        on_start: [feed("users"), post("/login", { json: { username: "${users.username}", password: "${users.password}" }, capture: { token: "json.token" } })],
        steps: [
          get("/items", { name: "list items", capture: { item_id: "json.items.1.id" } }),
          think("20ms"),
          transaction(
            "purchase",
            get("/items/${item_id}", { name: "item", contains: "stock" }),
            post("/cart", { name: "cart", headers: { authorization: "Bearer ${token}" }, json: { item: "${item_id}", qty: 1 }, expect: [201, 503] }),
          ),
          batch([get("/items"), get("/slow", { name: "slow page" })], { name: "page", concurrency: 2 }),
        ],
      },
    ],
    feeders: [{ name: "users", rows: [1, 2, 3, 4, 5].map((i) => ({ username: `user${i}`, password: "secret-pass" })) }],
    thresholds: [{ metric: "p95", request: "purchase", max: 1000 }],
    percentiles: [75, 99.9],
  });
}
