# File: ballista/sdks/python/examples/event_stream.py
"""Server-sent events: each user reads a price stream, resuming after disconnects with Last-Event-ID.

Time to first event is reported as `SSE <name>`.
"""

from common import Progress, client, parse_args, quick, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import get


def build(target: str, path: str) -> Plan:
    return Plan(
        name="event stream",
        target=Target(base_url=target, timeout="30s"),
        load=Load.users(("5s", 10), ("30s", 10)),
        scenarios=[
            Scenario(
                name="subscriber",
                wait=Wait.fixed("500ms"),
                steps=[get(path, name="price stream", sse=6, reconnects=3)],
            )
        ],
        thresholds=[Threshold(metric="p95", request="price stream", max=500), Threshold(metric="error_rate", max=0.0)],
    )


def main() -> int:
    args = parse_args(__doc__ or "", lambda p: p.add_argument("--path", default="/sse", help="event stream path"))
    with client() as c:
        run = c.run(build(args.target, args.path), quick(args, "6s"), labels={"suite": "sse"}, wait=not args.no_wait, on_stats=Progress())
        if args.no_wait:
            return 0
        first = c.get_stats(run.id).row("price stream", method="SSE")
        if first is not None:
            print(f"  time to first event p50={first.p50_ms:.1f}ms p95={first.p95_ms:.1f}ms")
        return report(run)


if __name__ == "__main__":
    raise SystemExit(main())
