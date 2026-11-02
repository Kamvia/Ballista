# File: ballista/sdks/python/examples/arrival_rate.py
"""Open model: a fixed number of iterations per second regardless of response times.

Dropped iterations show the generator could not keep the requested rate (users exhausted).
"""

from common import Progress, client, parse_args, quick, report

from ballista_client import Load, Plan, Scenario, Target, Threshold
from ballista_client.plan import get


def build(target: str, rate: float) -> Plan:
    return Plan(
        name="constant arrival rate",
        target=Target(base_url=target),
        load=Load.arrival_rate(("10s", rate), ("60s", rate), max_users=500),
        scenarios=[Scenario(name="api", steps=[get("/items", name="list items"), get("/items/2", name="item detail")])],
        thresholds=[
            Threshold(metric="dropped_iterations", max=0),
            Threshold(metric="p99", max=500),
            Threshold(metric="p95", max=250),
        ],
    )


def main() -> int:
    args = parse_args(__doc__ or "", lambda p: p.add_argument("--rate", type=float, default=50.0, help="iterations per second"))
    with client() as c:
        run = c.run(build(args.target, args.rate), quick(args, "15s"), labels={"suite": "arrival"}, wait=not args.no_wait, on_stats=Progress())
        return 0 if args.no_wait else report(run)


if __name__ == "__main__":
    raise SystemExit(main())
