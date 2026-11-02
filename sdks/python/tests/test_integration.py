# File: ballista/sdks/python/tests/test_integration.py
"""End-to-end checks against a running server and demo target.

Requires BALLISTA_URL, BALLISTA_TOKEN (run scope), BALLISTA_READ_TOKEN (read scope) and
BALLISTA_DEMO_URL (target base URL as reached from the server); skipped otherwise.
"""

import os
import time
from pathlib import Path

import pytest

from ballista_client import Bundle, Client, PermissionDeniedError, RunState, ValidationError

from .test_plan import shop_plan

REQUIRED = ("BALLISTA_URL", "BALLISTA_TOKEN", "BALLISTA_READ_TOKEN", "BALLISTA_DEMO_URL")
pytestmark = pytest.mark.skipif(any(not os.environ.get(k) for k in REQUIRED), reason="integration environment not configured")


def plan(base_url: str | None = None):  # type: ignore[no-untyped-def]
    return shop_plan(base_url or os.environ["BALLISTA_DEMO_URL"])


def test_full_lifecycle(tmp_path: Path) -> None:
    with Client() as c:
        assert c.whoami().scope.value == "run"
        check = c.validate_plan(plan())
        assert check.files == [] and check.scenarios == ["buyer"]
        info = c.upload_bundle(plan())
        assert c.upload_bundle(Bundle(plan())).id == info.id
        key = f"lifecycle-{time.time_ns()}"
        run = c.create_run(info, {"run_time": "6s"}, labels={"build": "42"}, idempotency_key=key)
        assert c.create_run(info, {"run_time": "6s"}, labels={"build": "42"}, idempotency_key=key).id == run.id
        samples: list[int] = []
        done = c.wait(run.id, timeout=120, on_stats=lambda s: samples.append(s.total.count))
        assert done.state is RunState.PASSED, done.error
        assert done.summary is not None and done.summary.requests > 20
        assert done.summary.thresholds[0].passed
        assert samples and samples[-1] > 0
        assert c.get_stats(run.id).row("purchase") is not None
        html = c.download(run.id, "report.html", tmp_path)
        assert html.read_text().startswith("<!DOCTYPE html>")
        assert any(r.id == run.id for r in c.iter_runs(state="passed"))


def test_plan_text_reports_missing_files() -> None:
    text = (
        'name = "text"\n[target]\nbase_url = "' + os.environ["BALLISTA_DEMO_URL"] + '"\n'
        '[load]\nstages = [{ duration = "1s", target = 1 }]\n[[scenario]]\nname = "s"\nscript = "scenarios/s.rhai"\n'
    )
    with Client() as c:
        assert c.validate_plan(text).files == ["scenarios/s.rhai"]
        with pytest.raises(ValidationError):
            c.upload_bundle(Bundle(text))
        info = c.upload_bundle(Bundle(text).add_text("scenarios/s.rhai", 'fn run(step, res) { if step == 0 { get("/items") } }'))
        done = c.run(info, {"run_time": "2s"}, timeout=60)
        assert done.state is RunState.PASSED, done.error


def test_stop_and_cancel() -> None:
    with Client() as c:
        info = c.upload_bundle(plan())
        first = c.create_run(info, {"run_time": "60s"})
        second = c.create_run(info, {"run_time": "60s"})
        assert c.stop_run(second.id).state is RunState.CANCELLED
        deadline = time.monotonic() + 30
        while c.get_run(first.id).state is RunState.QUEUED and time.monotonic() < deadline:
            time.sleep(0.2)
        c.stop_run(first.id)
        assert c.wait(first.id, timeout=60).state is RunState.STOPPED


def test_policy_and_scopes() -> None:
    with Client() as c:
        with pytest.raises(ValidationError) as e:
            c.validate_plan(plan(base_url="http://10.0.0.1:8080/"))
        assert "not permitted" in e.value.message
        info = c.upload_bundle(plan())
        with pytest.raises(ValidationError):
            c.create_run(info, {"users": 100_000, "spawn_rate": 100_000, "run_time": "10s"})
    with Client(token=os.environ["BALLISTA_READ_TOKEN"]) as ro:
        assert ro.status().limits.max_queue > 0
        with pytest.raises(PermissionDeniedError):
            ro.create_run(info)
