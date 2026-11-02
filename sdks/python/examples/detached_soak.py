# File: ballista/sdks/python/examples/detached_soak.py
"""Long soak test started detached: queue it, exit, and attach later from any machine with the run id.

  python detached_soak.py start --duration 2h
  python detached_soak.py attach <run-id>
  python detached_soak.py stop <run-id>
"""

import argparse
import sys

from common import Progress, client, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, ValidationError, Wait
from ballista_client.plan import get, think


def build(target: str, users: int, duration: str) -> Plan:
    return Plan(
        name="soak",
        target=Target(base_url=target),
        load=Load.users(("30s", users), (duration, users)),
        scenarios=[
            Scenario(
                name="steady",
                wait=Wait.uniform("1s", "3s"),
                steps=[get("/items", name="list items"), think("500ms"), get("/items/9", name="item detail")],
            )
        ],
        thresholds=[Threshold(metric="error_rate", max=0.001, abort_on_fail=True, abort_delay="5m"), Threshold(metric="p99", max=800)],
    )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("start")
    s.add_argument("--target", default="http://127.0.0.1:8080/")
    s.add_argument("--users", type=int, default=50)
    s.add_argument("--duration", default="2h", help="steady phase after a 30s ramp, e.g. 30m, 2h")
    for name in ("attach", "stop", "status"):
        sub.add_parser(name).add_argument("run_id")
    a = p.parse_args()
    with client() as c:
        if a.cmd == "start":
            plan = build(a.target, a.users, a.duration)
            try:
                c.validate_plan(plan)
            except ValidationError as e:
                print(f"rejected: {e.message}", file=sys.stderr)
                return 1
            run = c.run(plan, labels={"suite": "soak"}, wait=False)
            print(run.id)
            return 0
        if a.cmd == "attach":
            print(f"following {a.run_id} (Ctrl+C detaches; the run continues)", file=sys.stderr)
            try:
                return report(c.wait(a.run_id, on_stats=Progress()))
            except KeyboardInterrupt:
                return 0
        if a.cmd == "stop":
            print(c.stop_run(a.run_id).state.value)
            return 0
        run = c.get_run(a.run_id)
        if run.state.terminal:
            return report(run)
        live = c.get_stats(a.run_id) if run.state.value == "running" else None
        print(f"{run.id}: {run.state.value}" + (f", {live.users} users, p95 {live.total.p95_ms:.1f}ms" if live else ""))
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
