# File: ballista/sdks/python/src/ballista_client/client.py
"""Synchronous and asynchronous clients for the run manager API."""

from __future__ import annotations

import asyncio
import ipaddress
import json
import logging
import os
import random
import time
import uuid
from collections.abc import AsyncIterator, Callable, Iterator, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal, TypeVar
from urllib.parse import urlsplit

import httpx
from pydantic import BaseModel

from .plan import Plan

from .bundle import Bundle, PlanInput, plan_payload
from .errors import ApiError, ConfigurationError, RunTimeoutError, TransportError, error_for
from .models import (
    BUNDLE_ID_RE,
    RUN_ID_RE,
    BundleInfo,
    Health,
    Comparison,
    Page,
    PlanCheck,
    RegressionGate,
    Run,
    RunEvent,
    RunOverrides,
    RunRequest,
    RunState,
    ServerStatus,
    Snapshot,
    WhoAmI,
)

log = logging.getLogger("ballista_client")

TOKEN_ENV = "BALLISTA_TOKEN"
URL_ENV = "BALLISTA_URL"
USER_AGENT = "ballista-client-python/0.1.0"
RETRY_STATUSES = frozenset({429, 502, 503, 504})
ARTIFACTS = frozenset({"report.json", "report.html", "stats.csv", "history.csv", "failures.csv"})
Artifact = Literal["report.json", "report.html", "stats.csv", "history.csv", "failures.csv"]
M = TypeVar("M", bound=BaseModel)


@dataclass(frozen=True, slots=True)
class RetryPolicy:
    """Retries on 429/502/503/504 and connection errors, with jittered exponential backoff.

    Requests that change state are retried only when they are idempotent (DELETE, stop) or carry
    an idempotency key (run creation does so automatically).
    """

    attempts: int = 4
    base_delay: float = 0.5
    max_delay: float = 20.0

    def delay(self, attempt: int, retry_after: float | None) -> float:
        if retry_after is not None:
            return min(max(retry_after, 0.0), self.max_delay)
        backoff: float = min(self.base_delay * (2.0**attempt), self.max_delay)
        return backoff * (0.5 + random.random() / 2)


def _is_loopback(host: str) -> bool:
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host.strip("[]")).is_loopback
    except ValueError:
        return False


def _base_url(url: str | None, allow_insecure_http: bool) -> str:
    raw = (url or os.environ.get(URL_ENV) or "").strip()
    if not raw:
        raise ConfigurationError(f"server url required (argument or {URL_ENV})")
    parts = urlsplit(raw)
    if parts.scheme not in ("http", "https") or not parts.hostname or parts.username or parts.password:
        raise ConfigurationError("server url must be http(s)://host[:port] without credentials")
    if parts.scheme == "http" and not _is_loopback(parts.hostname) and not allow_insecure_http:
        raise ConfigurationError("refusing to send the token over plain http to a remote host; use https")
    return f"{parts.scheme}://{parts.netloc}/api/v1"


def _token(token: str | None) -> str:
    t = (token or os.environ.get(TOKEN_ENV) or "").strip()
    if len(t) < 32 or any(c.isspace() for c in t):
        raise ConfigurationError(f"api token required (argument or {TOKEN_ENV})")
    return t


def _check_id(value: str, pattern: Any, what: str) -> str:
    if not pattern.match(value):
        raise ConfigurationError(f"invalid {what} id {value!r}")
    return value


def _retry_after(resp: httpx.Response) -> float | None:
    v = resp.headers.get("retry-after")
    try:
        return float(v) if v is not None else None
    except ValueError:
        return None


def _raise_for(resp: httpx.Response) -> None:
    if resp.is_success:
        return
    code, message = "error", resp.reason_phrase or "request failed"
    try:
        body = resp.json()
        err = body.get("error", {}) if isinstance(body, dict) else {}
        code = str(err.get("code", code))
        message = str(err.get("message", message))
    except (ValueError, json.JSONDecodeError):
        pass
    raise error_for(resp.status_code, code, message[:1000], _retry_after(resp))


def _parse_sse(lines: Iterator[str]) -> Iterator[tuple[str, str]]:
    event: str = "message"
    data: list[str] = []
    for line in lines:
        if line == "":
            if data:
                yield event, "\n".join(data)
            event, data = "message", []
        elif line.startswith(":"):
            continue
        else:
            field, _, value = line.partition(":")
            value = value.removeprefix(" ")
            if field == "event":
                event = value
            elif field == "data":
                data.append(value)


def _to_event(event: str, data: str) -> RunEvent | None:
    if event == "stats":
        return RunEvent(type="stats", stats=Snapshot.model_validate_json(data))
    if event in ("run", "end"):
        return RunEvent(type="run" if event == "run" else "end", run=Run.model_validate_json(data))
    return None


class _Base:
    def __init__(
        self,
        url: str | None,
        token: str | None,
        *,
        timeout: float,
        verify: bool | str,
        retry: RetryPolicy,
        allow_insecure_http: bool,
    ) -> None:
        self.base_url = _base_url(url, allow_insecure_http)
        self._headers = {
            "authorization": f"Bearer {_token(token)}",
            "user-agent": USER_AGENT,
            "accept": "application/json",
        }
        self._timeout = httpx.Timeout(timeout, connect=min(timeout, 10.0))
        self._verify = verify
        self.retry = retry

    def __repr__(self) -> str:
        return f"{type(self).__name__}(base_url={self.base_url!r})"

    @staticmethod
    def _validate_payload(plan: PlanInput, overrides: RunOverrides | Mapping[str, Any] | None) -> dict[str, Any]:
        body: dict[str, Any] = {"plan": plan_payload(plan)}
        if overrides:
            ov = overrides if isinstance(overrides, RunOverrides) else RunOverrides(**overrides)
            body["overrides"] = ov.model_dump(mode="json", exclude_none=True)
        return body

    @staticmethod
    def _run_payload(
        bundle: str,
        overrides: RunOverrides | Mapping[str, Any] | None,
        labels: Mapping[str, str] | None,
        regression: RegressionGate | Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        ov = overrides if isinstance(overrides, RunOverrides) else RunOverrides(**(overrides or {}))
        gate = regression if isinstance(regression, RegressionGate) or regression is None else RegressionGate(**regression)
        req = RunRequest(bundle=bundle, overrides=ov, labels=dict(labels or {}), regression=gate)
        return req.model_dump(mode="json", exclude_none=True)

    @staticmethod
    def _compare_params(baseline: str, max_regression_pct: float | None, metrics: list[str] | None) -> dict[str, str]:
        params = {"baseline": baseline}
        if max_regression_pct is not None:
            params["max_regression_pct"] = str(max_regression_pct)
        if metrics:
            params["metrics"] = ",".join(metrics)
        return params


class Client(_Base):
    """Blocking client. Use as a context manager or call `close()`."""

    def __init__(
        self,
        url: str | None = None,
        token: str | None = None,
        *,
        timeout: float = 30.0,
        verify: bool | str = True,
        retry: RetryPolicy | None = None,
        allow_insecure_http: bool = False,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        super().__init__(url, token, timeout=timeout, verify=verify, retry=retry or RetryPolicy(), allow_insecure_http=allow_insecure_http)
        self._http = httpx.Client(
            base_url=self.base_url,
            headers=self._headers,
            timeout=self._timeout,
            verify=verify,
            transport=transport,
            follow_redirects=False,
        )

    def __enter__(self) -> Client:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        self._http.close()

    def _request(self, method: str, path: str, *, retryable: bool, **kw: Any) -> httpx.Response:
        attempts = self.retry.attempts if retryable else 1
        for attempt in range(attempts):
            try:
                resp = self._http.request(method, path, **kw)
            except httpx.TransportError as e:
                if attempt + 1 >= attempts:
                    raise TransportError(f"{method} {path}: {type(e).__name__}") from e
                time.sleep(self.retry.delay(attempt, None))
                continue
            if resp.status_code in RETRY_STATUSES and attempt + 1 < attempts:
                wait = self.retry.delay(attempt, _retry_after(resp))
                log.debug("retrying %s %s after status %d in %.1fs", method, path, resp.status_code, wait)
                time.sleep(wait)
                continue
            _raise_for(resp)
            return resp
        raise TransportError(f"{method} {path}: retries exhausted")

    def _get(self, path: str, model: type[M], **kw: Any) -> M:
        return model.model_validate_json(self._request("GET", path, retryable=True, **kw).content)

    def health(self) -> Health:
        return self._get("/health", Health)

    def whoami(self) -> WhoAmI:
        return self._get("/whoami", WhoAmI)

    def status(self) -> ServerStatus:
        return self._get("/status", ServerStatus)

    def validate_plan(self, plan: PlanInput, overrides: RunOverrides | Mapping[str, Any] | None = None) -> PlanCheck:
        """Validates a plan (as it would run with `overrides`) without storing it; lists files a bundle must still carry."""
        resp = self._request("POST", "/plans/validate", retryable=True, json=self._validate_payload(plan, overrides))
        return PlanCheck.model_validate_json(resp.content)

    def upload_bundle(self, bundle: Bundle | Plan) -> BundleInfo:
        """Uploads a plan (with assets); identical content returns the stored bundle. Safe to retry."""
        payload = (bundle if isinstance(bundle, Bundle) else Bundle(bundle)).to_payload()
        resp = self._request("POST", "/bundles", retryable=True, json=payload)
        return BundleInfo.model_validate_json(resp.content)

    def list_bundles(self) -> list[BundleInfo]:
        return self._get("/bundles", Page[BundleInfo]).items

    def get_bundle(self, bundle_id: str) -> BundleInfo:
        return self._get(f"/bundles/{_check_id(bundle_id, BUNDLE_ID_RE, 'bundle')}", BundleInfo)

    def delete_bundle(self, bundle_id: str) -> None:
        self._request("DELETE", f"/bundles/{_check_id(bundle_id, BUNDLE_ID_RE, 'bundle')}", retryable=True)

    def create_run(
        self,
        bundle: str | BundleInfo,
        overrides: RunOverrides | Mapping[str, Any] | None = None,
        *,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
        regression: RegressionGate | Mapping[str, Any] | None = None,
    ) -> Run:
        """Queues a run. A generated idempotency key makes retries safe against duplicate runs.

        With `regression`, the run fails when it is worse than its baseline (see `RegressionGate`).
        """
        bundle_id = bundle.id if isinstance(bundle, BundleInfo) else bundle
        payload = self._run_payload(bundle_id, overrides, labels, regression)
        key = idempotency_key or str(uuid.uuid4())
        resp = self._request("POST", "/runs", retryable=True, json=payload, headers={"idempotency-key": key})
        return Run.model_validate_json(resp.content)

    def list_runs(self, *, state: RunState | str | None = None, limit: int = 50, cursor: str | None = None) -> Page[Run]:
        params: dict[str, Any] = {"limit": max(1, min(limit, 200))}
        if state:
            params["state"] = RunState(state).value
        if cursor:
            params["cursor"] = cursor
        return self._get("/runs", Page[Run], params=params)

    def iter_runs(self, *, state: RunState | str | None = None, page_size: int = 100) -> Iterator[Run]:
        cursor: str | None = None
        while True:
            page = self.list_runs(state=state, limit=page_size, cursor=cursor)
            yield from page.items
            if not page.next_cursor:
                return
            cursor = page.next_cursor

    def get_run(self, run_id: str) -> Run:
        return self._get(f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}", Run)

    def stop_run(self, run_id: str) -> Run:
        resp = self._request("POST", f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/stop", retryable=True)
        return Run.model_validate_json(resp.content)

    def delete_run(self, run_id: str) -> None:
        self._request("DELETE", f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}", retryable=True)

    def get_stats(self, run_id: str) -> Snapshot:
        return self._get(f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/stats", Snapshot)

    def compare(self, run_id: str, *, baseline: str = "auto", max_regression_pct: float | None = None, metrics: list[str] | None = None) -> Comparison:
        """Compares a finished run with a baseline run (default: the latest earlier passed run of the same plan)."""
        path = f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/compare"
        return self._get(path, Comparison, params=self._compare_params(baseline, max_regression_pct, metrics))

    def artifact(self, run_id: str, name: Artifact) -> bytes:
        if name not in ARTIFACTS:
            raise ConfigurationError(f"unknown artifact {name!r}")
        return self._request("GET", f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/artifacts/{name}", retryable=True).content

    def report(self, run_id: str) -> dict[str, Any]:
        data: dict[str, Any] = json.loads(self.artifact(run_id, "report.json"))
        return data

    def download(self, run_id: str, name: Artifact, dest: str | Path) -> Path:
        """Writes an artifact to `dest` (a file path, or a directory to place it in)."""
        target = Path(dest)
        if target.is_dir():
            target = target / f"{run_id}-{name}"
        data = self.artifact(run_id, name)
        tmp = target.with_name(target.name + ".part")
        tmp.write_bytes(data)
        tmp.replace(target)
        return target

    def events(self, run_id: str, *, read_timeout: float = 60.0) -> Iterator[RunEvent]:
        """Streams run events until the run finishes (`end` is the last event)."""
        path = f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/events"
        timeout = httpx.Timeout(self._timeout.connect or 10.0, read=read_timeout)
        with self._http.stream("GET", path, headers={"accept": "text/event-stream"}, timeout=timeout) as resp:
            if not resp.is_success:
                resp.read()
                _raise_for(resp)
            for name, data in _parse_sse(resp.iter_lines()):
                ev = _to_event(name, data)
                if ev is not None:
                    yield ev
                    if ev.type == "end":
                        return

    def wait(
        self,
        run_id: str,
        *,
        timeout: float | None = None,
        on_stats: Callable[[Snapshot], None] | None = None,
        on_state: Callable[[Run], None] | None = None,
        poll_interval: float = 2.0,
    ) -> Run:
        """Blocks until the run finishes. Uses the event stream and falls back to polling."""
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            try:
                for ev in self.events(run_id, read_timeout=max(poll_interval * 10, 30.0)):
                    if deadline is not None and time.monotonic() > deadline:
                        raise RunTimeoutError(f"run {run_id} did not finish within {timeout}s")
                    if ev.stats is not None and on_stats:
                        on_stats(ev.stats)
                    if ev.run is not None:
                        if on_state:
                            on_state(ev.run)
                        if ev.type == "end":
                            return ev.run
            except (TransportError, httpx.TransportError, ApiError) as e:
                if isinstance(e, ApiError) and e.status not in RETRY_STATUSES:
                    raise
                log.debug("event stream interrupted, polling: %s", type(e).__name__)
            run = self.get_run(run_id)
            if run.state.terminal:
                return run
            if deadline is not None and time.monotonic() > deadline:
                raise RunTimeoutError(f"run {run_id} did not finish within {timeout}s")
            time.sleep(poll_interval)

    def run(
        self,
        bundle: Plan | Bundle | BundleInfo | str,
        overrides: RunOverrides | Mapping[str, Any] | None = None,
        *,
        labels: Mapping[str, str] | None = None,
        wait: bool = True,
        timeout: float | None = None,
        on_stats: Callable[[Snapshot], None] | None = None,
        regression: RegressionGate | Mapping[str, Any] | None = None,
    ) -> Run:
        """Uploads (when given a Plan or Bundle), queues and optionally waits for a run."""
        info: BundleInfo | str = self.upload_bundle(bundle) if isinstance(bundle, (Bundle, Plan)) else bundle
        run = self.create_run(info, overrides, labels=labels, regression=regression)
        return self.wait(run.id, timeout=timeout, on_stats=on_stats) if wait else run


class AsyncClient(_Base):
    """Asyncio client with the same surface as `Client`."""

    def __init__(
        self,
        url: str | None = None,
        token: str | None = None,
        *,
        timeout: float = 30.0,
        verify: bool | str = True,
        retry: RetryPolicy | None = None,
        allow_insecure_http: bool = False,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        super().__init__(url, token, timeout=timeout, verify=verify, retry=retry or RetryPolicy(), allow_insecure_http=allow_insecure_http)
        self._http = httpx.AsyncClient(
            base_url=self.base_url,
            headers=self._headers,
            timeout=self._timeout,
            verify=verify,
            transport=transport,
            follow_redirects=False,
        )

    async def __aenter__(self) -> AsyncClient:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._http.aclose()

    async def _request(self, method: str, path: str, *, retryable: bool, **kw: Any) -> httpx.Response:
        attempts = self.retry.attempts if retryable else 1
        for attempt in range(attempts):
            try:
                resp = await self._http.request(method, path, **kw)
            except httpx.TransportError as e:
                if attempt + 1 >= attempts:
                    raise TransportError(f"{method} {path}: {type(e).__name__}") from e
                await asyncio.sleep(self.retry.delay(attempt, None))
                continue
            if resp.status_code in RETRY_STATUSES and attempt + 1 < attempts:
                await asyncio.sleep(self.retry.delay(attempt, _retry_after(resp)))
                continue
            _raise_for(resp)
            return resp
        raise TransportError(f"{method} {path}: retries exhausted")

    async def _get(self, path: str, model: type[M], **kw: Any) -> M:
        return model.model_validate_json((await self._request("GET", path, retryable=True, **kw)).content)

    async def health(self) -> Health:
        return await self._get("/health", Health)

    async def whoami(self) -> WhoAmI:
        return await self._get("/whoami", WhoAmI)

    async def status(self) -> ServerStatus:
        return await self._get("/status", ServerStatus)

    async def validate_plan(self, plan: PlanInput, overrides: RunOverrides | Mapping[str, Any] | None = None) -> PlanCheck:
        resp = await self._request("POST", "/plans/validate", retryable=True, json=self._validate_payload(plan, overrides))
        return PlanCheck.model_validate_json(resp.content)

    async def upload_bundle(self, bundle: Bundle | Plan) -> BundleInfo:
        payload = (bundle if isinstance(bundle, Bundle) else Bundle(bundle)).to_payload()
        resp = await self._request("POST", "/bundles", retryable=True, json=payload)
        return BundleInfo.model_validate_json(resp.content)

    async def list_bundles(self) -> list[BundleInfo]:
        return (await self._get("/bundles", Page[BundleInfo])).items

    async def get_bundle(self, bundle_id: str) -> BundleInfo:
        return await self._get(f"/bundles/{_check_id(bundle_id, BUNDLE_ID_RE, 'bundle')}", BundleInfo)

    async def delete_bundle(self, bundle_id: str) -> None:
        await self._request("DELETE", f"/bundles/{_check_id(bundle_id, BUNDLE_ID_RE, 'bundle')}", retryable=True)

    async def create_run(
        self,
        bundle: str | BundleInfo,
        overrides: RunOverrides | Mapping[str, Any] | None = None,
        *,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
        regression: RegressionGate | Mapping[str, Any] | None = None,
    ) -> Run:
        bundle_id = bundle.id if isinstance(bundle, BundleInfo) else bundle
        payload = self._run_payload(bundle_id, overrides, labels, regression)
        key = idempotency_key or str(uuid.uuid4())
        resp = await self._request("POST", "/runs", retryable=True, json=payload, headers={"idempotency-key": key})
        return Run.model_validate_json(resp.content)

    async def list_runs(self, *, state: RunState | str | None = None, limit: int = 50, cursor: str | None = None) -> Page[Run]:
        params: dict[str, Any] = {"limit": max(1, min(limit, 200))}
        if state:
            params["state"] = RunState(state).value
        if cursor:
            params["cursor"] = cursor
        return await self._get("/runs", Page[Run], params=params)

    async def iter_runs(self, *, state: RunState | str | None = None, page_size: int = 100) -> AsyncIterator[Run]:
        cursor: str | None = None
        while True:
            page = await self.list_runs(state=state, limit=page_size, cursor=cursor)
            for r in page.items:
                yield r
            if not page.next_cursor:
                return
            cursor = page.next_cursor

    async def get_run(self, run_id: str) -> Run:
        return await self._get(f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}", Run)

    async def stop_run(self, run_id: str) -> Run:
        resp = await self._request("POST", f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/stop", retryable=True)
        return Run.model_validate_json(resp.content)

    async def delete_run(self, run_id: str) -> None:
        await self._request("DELETE", f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}", retryable=True)

    async def get_stats(self, run_id: str) -> Snapshot:
        return await self._get(f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/stats", Snapshot)

    async def compare(self, run_id: str, *, baseline: str = "auto", max_regression_pct: float | None = None, metrics: list[str] | None = None) -> Comparison:
        path = f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/compare"
        return await self._get(path, Comparison, params=self._compare_params(baseline, max_regression_pct, metrics))

    async def artifact(self, run_id: str, name: Artifact) -> bytes:
        if name not in ARTIFACTS:
            raise ConfigurationError(f"unknown artifact {name!r}")
        resp = await self._request("GET", f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/artifacts/{name}", retryable=True)
        return resp.content

    async def report(self, run_id: str) -> dict[str, Any]:
        data: dict[str, Any] = json.loads(await self.artifact(run_id, "report.json"))
        return data

    async def events(self, run_id: str, *, read_timeout: float = 60.0) -> AsyncIterator[RunEvent]:
        path = f"/runs/{_check_id(run_id, RUN_ID_RE, 'run')}/events"
        timeout = httpx.Timeout(self._timeout.connect or 10.0, read=read_timeout)
        async with self._http.stream("GET", path, headers={"accept": "text/event-stream"}, timeout=timeout) as resp:
            if not resp.is_success:
                await resp.aread()
                _raise_for(resp)
            lines: list[str] = []
            async for line in resp.aiter_lines():
                lines.append(line)
                if line != "":
                    continue
                for name, data in _parse_sse(iter(lines)):
                    ev = _to_event(name, data)
                    if ev is not None:
                        yield ev
                        if ev.type == "end":
                            return
                lines.clear()

    async def wait(
        self,
        run_id: str,
        *,
        timeout: float | None = None,
        on_stats: Callable[[Snapshot], None] | None = None,
        poll_interval: float = 2.0,
    ) -> Run:
        async def follow() -> Run:
            while True:
                try:
                    async for ev in self.events(run_id, read_timeout=max(poll_interval * 10, 30.0)):
                        if ev.stats is not None and on_stats:
                            on_stats(ev.stats)
                        if ev.type == "end" and ev.run is not None:
                            return ev.run
                except (TransportError, httpx.TransportError, ApiError) as e:
                    if isinstance(e, ApiError) and e.status not in RETRY_STATUSES:
                        raise
                run = await self.get_run(run_id)
                if run.state.terminal:
                    return run
                await asyncio.sleep(poll_interval)

        try:
            return await asyncio.wait_for(follow(), timeout)
        except TimeoutError as e:
            raise RunTimeoutError(f"run {run_id} did not finish within {timeout}s") from e

    async def run(
        self,
        bundle: Plan | Bundle | BundleInfo | str,
        overrides: RunOverrides | Mapping[str, Any] | None = None,
        *,
        labels: Mapping[str, str] | None = None,
        wait: bool = True,
        timeout: float | None = None,
        on_stats: Callable[[Snapshot], None] | None = None,
        regression: RegressionGate | Mapping[str, Any] | None = None,
    ) -> Run:
        info: BundleInfo | str = await self.upload_bundle(bundle) if isinstance(bundle, (Bundle, Plan)) else bundle
        run = await self.create_run(info, overrides, labels=labels, regression=regression)
        return await self.wait(run.id, timeout=timeout, on_stats=on_stats) if wait else run
