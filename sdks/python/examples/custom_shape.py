# File: ballista/sdks/python/examples/custom_shape.py
"""Custom load shape: a sine wave between 5 and 25 users for two minutes, defined by an inline script."""

from common import Progress, client, parse_args, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import get

WAVE = """
fn shape(t) {
    let total = {total};
    if t >= total { return (); }
    let pi = 3.14159265;
    15.0 + 10.0 * sin(2.0 * pi * t / {period})
}
"""


def build(target: str, total: int, period: int) -> Plan:
    source = WAVE.replace("{total}", f"{total}.0").replace("{period}", f"{period}.0")
    return Plan(
        name="wave",
        target=Target(base_url=target),
        load=Load.shape(source, graceful_stop="3s"),
        scenarios=[Scenario(name="reader", wait=Wait.uniform("100ms", "300ms"), steps=[get("/items", name="list items")])],
        thresholds=[Threshold(metric="p95", max=300)],
    )


def main() -> int:
    args = parse_args(__doc__ or "")
    total, period = (10, 5) if args.quick else (120, 30)
    with client() as c:
        run = c.run(build(args.target, total, period), labels={"suite": "shape"}, wait=not args.no_wait, on_stats=Progress())
        return 0 if args.no_wait else report(run)


if __name__ == "__main__":
    raise SystemExit(main())
