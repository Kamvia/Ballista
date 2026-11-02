# File: ballista/sdks/python/examples/common.py
"""Shared setup for the examples: configuration, progress output and exit codes.

Environment:
  BALLISTA_URL     run manager, e.g. https://loadtest.example.com
  BALLISTA_TOKEN   API token with the 'run' scope
  TARGET_URL       system under test as reached from the load generator host
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
from collections.abc import Callable

from ballista_client import Client, Run, RunState, Snapshot

EXIT_FAILED = 2
EXIT_ERROR = 1


def parse_args(description: str, extra: Callable[[argparse.ArgumentParser], object] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=description)
    p.add_argument("--target", default=os.environ.get("TARGET_URL", "http://127.0.0.1:8080/"), help="system under test (TARGET_URL)")
    p.add_argument("--quick", action="store_true", help="shorten the run for a functional check")
    p.add_argument("--no-wait", action="store_true", help="queue the run and exit")
    p.add_argument("-v", "--verbose", action="store_true")
    if extra:
        extra(p)
    args = p.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.WARNING, format="%(levelname)s %(name)s %(message)s")
    return args


def client() -> Client:
    return Client(timeout=60.0)


def quick(args: argparse.Namespace, run_time: str = "5s") -> dict[str, object]:
    """Overrides that shorten a run when --quick is given."""
    return {"run_time": run_time} if args.quick else {}


class Progress:
    """One line per elapsed second."""

    def __init__(self) -> None:
        self._last = -1

    def __call__(self, s: Snapshot) -> None:
        second = int(s.elapsed_secs)
        if second == self._last:
            return
        self._last = second
        t = s.total
        print(f"  t={second:>4}s users={s.users:>5} rps={t.rps:>8.1f} p95={t.p95_ms:>8.1f}ms failures={t.failures}", file=sys.stderr, flush=True)


def report(run: Run) -> int:
    """Prints the outcome and returns a process exit code (0 passed, 2 failed, 1 error)."""
    print(f"run {run.id}: {run.state.value}")
    if run.error:
        print(f"  error: {run.error}")
    s = run.summary
    if s is not None:
        print(f"  requests={s.requests} failures={s.failures} ({s.fail_ratio:.2%}) rps={s.rps:.1f}")
        print(f"  latency p50={s.p50_ms:.1f}ms p95={s.p95_ms:.1f}ms p99={s.p99_ms:.1f}ms max={s.max_ms:.1f}ms")
        for t in s.thresholds:
            actual = "n/a" if t.actual is None else f"{t.actual:.3f}"
            print(f"  threshold {t.metric:<11} {t.request:<16} actual={actual:<10} {'pass' if t.passed else 'FAIL'}")
    if run.state is RunState.PASSED:
        return 0
    return EXIT_FAILED if run.state in (RunState.FAILED, RunState.STOPPED) else EXIT_ERROR
