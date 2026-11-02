# File: ballista/sdks/python/examples/smoke_test.py
"""Smoke test: a few users hit the main endpoints and the run fails on any error."""

from common import Progress, client, parse_args, quick, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import get


def build(target: str) -> Plan:
    return Plan(
        name="smoke",
        target=Target(base_url=target, timeout="5s", headers={"accept": "application/json"}),
        load=Load.users(("5s", 3), ("20s", 3)),
        scenarios=[
            Scenario(
                name="browse",
                wait=Wait.uniform("200ms", "500ms"),
                steps=[
                    get("/items", name="list items", expect=200),
                    get("/items/1", name="item detail", contains="stock"),
                ],
            )
        ],
        thresholds=[
            Threshold(metric="error_rate", max=0.0),
            Threshold(metric="p95", max=500),
        ],
    )


def main() -> int:
    args = parse_args(__doc__ or "")
    with client() as c:
        run = c.run(build(args.target), quick(args), labels={"suite": "smoke"}, wait=not args.no_wait, on_stats=Progress())
        if args.no_wait:
            print(f"queued {run.id}")
            return 0
        return report(run)


if __name__ == "__main__":
    raise SystemExit(main())
