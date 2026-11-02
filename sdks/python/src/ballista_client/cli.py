# File: ballista/sdks/python/src/ballista_client/cli.py
"""Command line for CI pipelines: submit a plan, wait, print the summary and set the exit code."""

from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path

from .bundle import Bundle
from .client import Client
from .errors import ApiError, BallistaError, RunTimeoutError
from .models import Run, RunState, Snapshot

EXIT_FAILED = 2
EXIT_ERROR = 1
EXIT_TIMEOUT = 3


def _labels(values: list[str]) -> dict[str, str]:
    out: dict[str, str] = {}
    for v in values:
        key, sep, val = v.partition("=")
        if not sep:
            raise SystemExit(f"label {v!r} must be key=value")
        out[key] = val
    return out


def _bundle(plan_path: Path, files: list[str], labels: dict[str, str]) -> Bundle:
    raw = plan_path.read_text(encoding="utf-8")
    plan: dict[str, object] | str = raw
    if plan_path.suffix.lower() == ".json":
        doc = json.loads(raw)
        if not isinstance(doc, dict):
            raise SystemExit("plan document must be a JSON object")
        plan = doc
    bundle = Bundle(plan, labels=labels)
    for spec in files:
        rel, sep, local = spec.partition("=")
        if not sep:
            raise SystemExit(f"--file {spec!r} must be REL=PATH")
        bundle.add_path(rel, local)
    return bundle


def _print_summary(run: Run) -> None:
    print(f"run {run.id} {run.state.value}")
    if run.error:
        print(f"error: {run.error}")
    s = run.summary
    if s is None:
        return
    print(
        f"requests={s.requests} failures={s.failures} rps={s.rps:.1f} "
        f"p50={s.p50_ms:.1f}ms p95={s.p95_ms:.1f}ms p99={s.p99_ms:.1f}ms duration={s.duration_secs:.0f}s"
    )
    for t in s.thresholds:
        actual = "n/a" if t.actual is None else f"{t.actual:.3f}"
        print(f"threshold {t.metric} {t.request} actual={actual} {'pass' if t.passed else 'FAIL'}")


class _Progress:
    """Prints at most one progress line per elapsed second."""

    def __init__(self) -> None:
        self._last = -1

    def __call__(self, snap: Snapshot) -> None:
        second = int(snap.elapsed_secs)
        if second == self._last:
            return
        self._last = second
        t = snap.total
        print(
            f"  t={second}s users={snap.users} rps={t.rps:.1f} p95={t.p95_ms:.1f}ms failures={t.failures}",
            file=sys.stderr,
            flush=True,
        )


def _exit_code(run: Run) -> int:
    return 0 if run.state == RunState.PASSED else EXIT_FAILED if run.state in (RunState.FAILED, RunState.STOPPED) else EXIT_ERROR


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="ballista-client")
    p.add_argument("--url", help="server url (or BALLISTA_URL)")
    p.add_argument("--ca", help="CA bundle for TLS verification")
    p.add_argument("-v", "--verbose", action="store_true")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("submit", help="send a plan (JSON document or plan text) and queue a run")
    s.add_argument("plan", type=Path, help="plan document (.json) or plan text (.toml)")
    s.add_argument("--file", action="append", default=[], metavar="REL=PATH", help="asset to include: bundle path=local file")
    s.add_argument("--users", type=float)
    s.add_argument("--spawn-rate", type=float)
    s.add_argument("--run-time")
    s.add_argument("--iterations", type=int)
    s.add_argument("--tags")
    s.add_argument("--host")
    s.add_argument("--label", action="append", default=[], help="key=value")
    s.add_argument("--no-wait", action="store_true")
    s.add_argument("--timeout", type=float, help="seconds to wait for completion")
    s.add_argument("--quiet", action="store_true", help="no live progress")
    s.add_argument("--report", type=Path, help="directory to save report.json and report.html")

    for name in ("status", "stop", "wait"):
        sp = sub.add_parser(name)
        sp.add_argument("run_id")
    r = sub.add_parser("report", help="download an artifact of a finished run")
    r.add_argument("run_id")
    r.add_argument("--name", default="report.html", choices=["report.json", "report.html", "stats.csv", "history.csv", "failures.csv"])
    r.add_argument("--out", type=Path, default=Path("."))
    sub.add_parser("list")

    a = p.parse_args(argv)
    logging.basicConfig(level=logging.DEBUG if a.verbose else logging.WARNING, format="%(levelname)s %(name)s %(message)s")
    try:
        with Client(a.url, verify=a.ca or True) as c:
            if a.cmd == "submit":
                bundle = _bundle(a.plan, a.file, _labels(a.label))
                overrides = {
                    k: v
                    for k, v in {
                        "users": a.users,
                        "spawn_rate": a.spawn_rate,
                        "run_time": a.run_time,
                        "iterations": a.iterations,
                        "tags": a.tags.split(",") if a.tags else None,
                        "host": a.host,
                    }.items()
                    if v is not None
                }
                info = c.upload_bundle(bundle)
                run = c.create_run(info, overrides, labels=bundle.labels)
                print(f"queued run {run.id} (bundle {info.id})")
                if a.no_wait:
                    return 0
                run = c.wait(run.id, timeout=a.timeout, on_stats=None if a.quiet else _Progress())
                _print_summary(run)
                if a.report and run.summary is not None:
                    a.report.mkdir(parents=True, exist_ok=True)
                    c.download(run.id, "report.json", a.report)
                    c.download(run.id, "report.html", a.report)
                return _exit_code(run)
            if a.cmd == "status":
                _print_summary(c.get_run(a.run_id))
                return 0
            if a.cmd == "stop":
                print(f"run {a.run_id} {c.stop_run(a.run_id).state.value}")
                return 0
            if a.cmd == "wait":
                run = c.wait(a.run_id)
                _print_summary(run)
                return _exit_code(run)
            if a.cmd == "report":
                print(c.download(a.run_id, a.name, a.out))
                return 0
            if a.cmd == "list":
                for run in c.list_runs(limit=50).items:
                    print(json.dumps({"id": run.id, "state": run.state.value, "plan": run.plan_name, "created_at": run.created_at.isoformat()}))
                return 0
    except RunTimeoutError as e:
        print(f"error: {e}", file=sys.stderr)
        return EXIT_TIMEOUT
    except ApiError as e:
        print(f"error: {e.status} {e.code}: {e.message}", file=sys.stderr)
        return EXIT_ERROR
    except BallistaError as e:
        print(f"error: {e}", file=sys.stderr)
        return EXIT_ERROR
    return EXIT_ERROR


if __name__ == "__main__":
    raise SystemExit(main())
