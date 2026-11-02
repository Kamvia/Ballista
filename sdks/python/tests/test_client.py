# File: ballista/sdks/python/tests/test_client.py
import json

import httpx
import pytest

from ballista_client import (
    AsyncClient,
    Client,
    ConfigurationError,
    NotFoundError,
    RetryPolicy,
    RunState,
    ValidationError,
)

TOKEN = "bt_" + "x" * 43
BUNDLE = "0123456789abcdef0123456789abcdef"
RUN_ID = "r" + "a" * 24


def run_json(state: str = "queued") -> dict[str, object]:
    return {
        "id": RUN_ID, "seq": 1, "bundle": BUNDLE, "plan_name": "p", "state": state,
        "created_at": "2026-11-01T10:00:00Z", "created_by": "ci", "labels": {}, "overrides": {},
    }


def client(handler: httpx.MockTransport) -> Client:
    return Client("http://127.0.0.1:8070", TOKEN, transport=handler, retry=RetryPolicy(attempts=3, base_delay=0.0))


def test_refuses_plain_http_to_remote_hosts_and_bad_ids() -> None:
    with pytest.raises(ConfigurationError):
        Client("http://example.com", TOKEN)
    with pytest.raises(ConfigurationError):
        Client("https://example.com", "short")
    c = client(httpx.MockTransport(lambda r: httpx.Response(200)))
    with pytest.raises(ConfigurationError):
        c.get_run("../../etc/passwd")
    assert TOKEN not in repr(c)


def test_create_run_retries_with_one_idempotency_key() -> None:
    keys: list[str] = []

    def handler(req: httpx.Request) -> httpx.Response:
        assert req.headers["authorization"] == f"Bearer {TOKEN}"
        keys.append(req.headers["idempotency-key"])
        if len(keys) < 3:
            return httpx.Response(503, headers={"retry-after": "0"}, json={"error": {"code": "busy", "message": "x"}})
        body = json.loads(req.content)
        assert body == {"bundle": BUNDLE, "overrides": {"users": 5.0, "run_time": "30s"}, "labels": {"ci": "1"}}
        return httpx.Response(201, json=run_json())

    run = client(httpx.MockTransport(handler)).create_run(BUNDLE, {"users": 5, "run_time": "30s"}, labels={"ci": "1"})
    assert run.state is RunState.QUEUED and len(set(keys)) == 1 and len(keys) == 3


def test_error_mapping() -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        if req.url.path.endswith("/runs"):
            return httpx.Response(422, json={"error": {"code": "invalid", "message": "plan peak exceeds limit"}})
        return httpx.Response(404, json={"error": {"code": "not_found", "message": "run not found"}})

    c = client(httpx.MockTransport(handler))
    with pytest.raises(NotFoundError):
        c.get_run(RUN_ID)
    with pytest.raises(ValidationError) as e:
        c.create_run(BUNDLE)
    assert "peak" in e.value.message


def test_event_stream_and_wait() -> None:
    snap = {"plan": "p", "state": "running", "users": 3, "total": {"count": 10, "p95_ms": 12.5}}
    body = (
        f"event: run\ndata: {json.dumps(run_json('running'))}\n\n"
        ": keep-alive\n\n"
        f"event: stats\ndata: {json.dumps(snap)}\n\n"
        f"event: end\ndata: {json.dumps(run_json('passed'))}\n\n"
    )
    transport = httpx.MockTransport(lambda r: httpx.Response(200, headers={"content-type": "text/event-stream"}, text=body))
    seen: list[float] = []
    run = client(transport).wait(RUN_ID, on_stats=lambda s: seen.append(s.total.p95_ms))
    assert run.state is RunState.PASSED and run.finished and seen == [12.5]


async def test_async_client_events() -> None:
    body = f"event: end\ndata: {json.dumps(run_json('failed'))}\n\n"
    transport = httpx.MockTransport(lambda r: httpx.Response(200, text=body))
    async with AsyncClient("http://localhost:8070", TOKEN, transport=transport) as c:
        run = await c.wait(RUN_ID, timeout=5)
    assert run.state is RunState.FAILED
