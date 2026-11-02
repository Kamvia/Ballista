# File: ballista/sdks/python/examples/regression_gate.py
"""Regression gate: the run fails when it is slower than the latest passed run of the same plan.

The first run becomes the baseline. Pass --path /slow on a later run to see the gate trip.
"""

import argparse
import sys

from common import EXIT_FAILED, Progress, client, parse_args, quick, report

from ballista_client import Comparison, Load, Plan, RegressionGate, Scenario, Target, Wait
from ballista_client.plan import get


def extra(p: argparse.ArgumentParser) -> None:
    p.add_argument("--path", default="/items/1", help="endpoint measured as 'item detail'")
    p.add_argument("--workers", type=int, default=None, help="pool workers to spread the run across (0 = local)")
    p.add_argument("--max-regression-pct", type=float, default=20.0)


def build(target: str, path: str) -> Plan:
    return Plan(
        name="regression-gate",
        target=Target(base_url=target, timeout="5s"),
        load=Load.users(("3s", 20), ("12s", 20)),
        scenarios=[Scenario(name="detail", wait=Wait.fixed("50ms"), steps=[get(path, name="item detail", expect=200)])],
    )


def show(c: Comparison) -> None:
    print(f"baseline={c.baseline or '-'} passed={c.passed} regressions={c.regressions}")
    for ch in c.changes[:8]:
        pct = "n/a" if ch.change_pct is None else f"{ch.change_pct:+.1f}%"
        flag = "REGRESSED" if ch.regressed else "ok"
        print(f"  {ch.kind:<11} {ch.method:<4} {ch.name:<14} {ch.metric:<10} {ch.baseline:>10.3f} -> {ch.current:>10.3f} {pct:>9} {flag}")
    if c.note:
        print(f"  note: {c.note}")


def main() -> int:
    args = parse_args(__doc__ or "", extra)
    overrides: dict[str, object] = quick(args, "8s")
    if args.workers is not None:
        overrides["workers"] = args.workers
    gate = RegressionGate(max_regression_pct=args.max_regression_pct, min_change_ms=2.0)
    with client() as c:
        run = c.run(build(args.target, args.path), overrides, labels={"suite": "regression"}, regression=gate, on_stats=Progress())
        code = report(run)
        if run.comparison is not None:
            show(run.comparison)
            return EXIT_FAILED if not run.comparison.passed else code
        print("no comparison recorded", file=sys.stderr)
        return code


if __name__ == "__main__":
    raise SystemExit(main())
