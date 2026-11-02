# File: ballista/sdks/python/examples/ramp_spike.py
"""Ramp, spike and recovery profile. Aborts early when the error rate stays above 5% for 10 seconds."""

from common import Progress, client, parse_args, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import get


def build(target: str, peak: int, quick: bool) -> Plan:
    unit = "1s" if quick else "15s"
    return Plan(
        name="ramp and spike",
        target=Target(base_url=target, timeout="10s"),
        load=Load.users(
            (unit, peak // 4),
            (unit, peak // 4),
            ("2s", peak),
            (unit, peak),
            ("2s", peak // 4),
            (unit, peak // 4),
            ("2s", 0),
            graceful_stop="5s",
        ),
        scenarios=[
            Scenario(
                name="reader",
                wait=Wait.exponential("300ms"),
                steps=[get("/items", name="list items"), get("/slow", name="slow page")],
            )
        ],
        thresholds=[
            Threshold(metric="error_rate", max=0.05, abort_on_fail=True, abort_delay="10s"),
            Threshold(metric="p99", request="slow page", max=1500),
        ],
    )


def main() -> int:
    args = parse_args(__doc__ or "", lambda p: p.add_argument("--peak", type=int, default=200, help="spike user count"))
    peak = min(args.peak, 20) if args.quick else args.peak
    with client() as c:
        run = c.run(build(args.target, peak, args.quick), labels={"suite": "spike"}, wait=not args.no_wait, on_stats=Progress())
        return 0 if args.no_wait else report(run)


if __name__ == "__main__":
    raise SystemExit(main())
