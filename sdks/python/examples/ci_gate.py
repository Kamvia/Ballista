# File: ballista/sdks/python/examples/ci_gate.py
"""CI quality gate: run a performance check for a build, keep the reports and fail the pipeline on regressions.

Exit codes: 0 passed, 2 thresholds failed or stopped, 1 error, 3 timeout.
"""

import os
from pathlib import Path

from common import client, parse_args, quick, report

from ballista_client import ApiError, Load, Plan, RunTimeoutError, Scenario, Target, Threshold, Wait
from ballista_client.plan import get, post

EXIT_TIMEOUT = 3


def build(target: str) -> Plan:
    return Plan(
        name="ci gate",
        target=Target(base_url=target, headers={"accept": "application/json"}),
        load=Load.users(("15s", 25), ("45s", 25)),
        scenarios=[
            Scenario(
                name="api",
                wait=Wait.uniform("100ms", "400ms"),
                on_start=[post("/login", name="login", json={"username": "ci", "password": "ci-secret"}, capture={"token": "json.token"})],
                steps=[
                    get("/items", name="list items"),
                    get("/items/7", name="item detail"),
                    post("/cart", name="add to cart", headers={"authorization": "Bearer ${token}"}, json={"item": 7, "qty": 1}, expect=[201, 503]),
                ],
            )
        ],
        thresholds=[
            Threshold(metric="p95", request="list items", max=200),
            Threshold(metric="p95", request="add to cart", max=300),
            Threshold(metric="error_rate", max=0.03, abort_on_fail=True, abort_delay="15s"),
        ],
    )


def main() -> int:
    args = parse_args(
        __doc__ or "",
        lambda p: (
            p.add_argument("--reports", type=Path, default=Path("perf-reports")),
            p.add_argument("--timeout", type=float, default=900.0, help="seconds to wait for the result"),
        ),
    )
    labels = {
        "pipeline": os.environ.get("CI_PIPELINE_ID", "local"),
        "commit": os.environ.get("CI_COMMIT_SHA", "unknown")[:40],
        "branch": os.environ.get("CI_COMMIT_REF_NAME", "local")[:256],
    }
    key = f"ci-{labels['pipeline']}-{labels['commit']}"
    with client() as c:
        try:
            info = c.upload_bundle(build(args.target))
            run = c.create_run(info, quick(args, "8s"), labels=labels, idempotency_key=key[:128])
            print(f"run {run.id} queued (position {run.queue_position})")
            run = c.wait(run.id, timeout=args.timeout)
        except RunTimeoutError as e:
            print(f"timeout: {e}")
            return EXIT_TIMEOUT
        except ApiError as e:
            print(f"server rejected the request: {e.status} {e.code}: {e.message}")
            return 1
        code = report(run)
        if run.summary is not None:
            args.reports.mkdir(parents=True, exist_ok=True)
            for name in ("report.html", "report.json", "stats.csv", "failures.csv"):
                print(f"  saved {c.download(run.id, name, args.reports)}")
        return code


if __name__ == "__main__":
    raise SystemExit(main())
