# File: ballista/sdks/python/examples/weighted_mix.py
"""Realistic traffic mix: weighted scenarios, a fixed pool of admin users, pacing and tag filtering.

Retried attempts are recorded individually, so the flaky endpoint gets its own, looser threshold.
"""

from common import Progress, client, parse_args, quick, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import get, post, think


def build(target: str, tags: list[str] | None = None) -> Plan:
    writes = tags is None or "write" in tags
    return Plan(
        name="traffic mix",
        target=Target(base_url=target, headers={"accept": "application/json"}, retries=2, retry_backoff="100ms"),
        load=Load.users(("10s", 30), ("40s", 30)),
        scenarios=[
            Scenario(
                name="browser",
                weight=7,
                tags=["read"],
                wait=Wait.uniform("500ms", "2s"),
                steps=[get("/items", name="list items"), think("300ms"), get("/items/5", name="item detail")],
            ),
            Scenario(
                name="searcher",
                weight=2,
                tags=["read"],
                wait=Wait.exponential("1s"),
                steps=[get("/items", name="search", query={"q": "item-1"})],
            ),
            Scenario(
                name="buyer",
                weight=1,
                tags=["write"],
                wait=Wait.pace("3s"),
                on_start=[post("/login", name="login", json={"username": "buyer", "password": "secret-pw"}, capture={"token": "json.token"})],
                steps=[post("/cart", name="add to cart", headers={"authorization": "Bearer ${token}"}, json={"item": 1, "qty": 1}, expect=[201, 503])],
            ),
            Scenario(
                name="admin",
                fixed_count=2,
                tags=["write"],
                wait=Wait.fixed("2s"),
                steps=[get("/flaky", name="flaky health", retries=3)],
            ),
        ],
        thresholds=[
            Threshold(metric="p95", request="list items", max=300),
            Threshold(metric="error_rate", request="list items", max=0.0),
            *(
                [Threshold(metric="error_rate", request="add to cart", max=0.05), Threshold(metric="error_rate", request="flaky health", max=0.8)]
                if writes
                else []
            ),
        ],
    )


def main() -> int:
    args = parse_args(
        __doc__ or "",
        lambda p: p.add_argument("--tags", help="only scenarios with these tags, e.g. read"),
    )
    overrides: dict[str, object] = dict(quick(args, "8s"))
    tags = args.tags.split(",") if args.tags else None
    if tags:
        overrides["tags"] = tags
    with client() as c:
        run = c.run(build(args.target, tags), overrides, labels={"suite": "mix"}, wait=not args.no_wait, on_stats=Progress())
        if args.no_wait:
            return 0
        code = report(run)
        stats = c.get_stats(run.id)
        for row in stats.iterations:
            print(f"  scenario {row.scenario:<10} iterations={row.count}")
        return code


if __name__ == "__main__":
    raise SystemExit(main())
