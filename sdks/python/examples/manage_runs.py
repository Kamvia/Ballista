# File: ballista/sdks/python/examples/manage_runs.py
"""Housekeeping: server status, recent runs, a latency trend per plan and optional cleanup (admin token)."""

import argparse
from collections import defaultdict
from datetime import UTC, datetime, timedelta

from common import client

from ballista_client import PermissionDeniedError, RunState


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--limit", type=int, default=20)
    p.add_argument("--delete-older-than-days", type=int, help="delete finished runs older than this (admin scope)")
    a = p.parse_args()
    with client() as c:
        me = c.whoami()
        status = c.status()
        print(f"server {status.version} as {me.name} ({me.scope.value}); active={status.active_run} queued={status.queued}")
        print(f"limits: users<={status.limits.max_users:.0f} run<={status.limits.max_run_time_secs}s queue<={status.limits.max_queue}")

        trend: dict[str, list[float]] = defaultdict(list)
        print(f"\n{'run':<26} {'state':<10} {'plan':<24} {'p95 ms':>8} {'requests':>9}")
        for i, run in enumerate(c.iter_runs()):
            if i >= a.limit:
                break
            s = run.summary
            print(f"{run.id:<26} {run.state.value:<10} {run.plan_name[:24]:<24} {s.p95_ms if s else 0:>8.1f} {s.requests if s else 0:>9}")
            if s is not None and run.state is RunState.PASSED:
                trend[run.plan_name].append(s.p95_ms)

        print("\np95 trend (newest first):")
        for plan, values in trend.items():
            print(f"  {plan[:30]:<30} " + " ".join(f"{v:.0f}" for v in values[:10]))

        if a.delete_older_than_days is not None:
            cutoff = datetime.now(UTC) - timedelta(days=a.delete_older_than_days)
            old = [r for r in c.iter_runs() if r.state.terminal and r.created_at < cutoff]
            try:
                for r in old:
                    c.delete_run(r.id)
                print(f"\ndeleted {len(old)} runs older than {a.delete_older_than_days} days")
            except PermissionDeniedError:
                print("\ndeleting runs requires a token with the admin scope")
                return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
