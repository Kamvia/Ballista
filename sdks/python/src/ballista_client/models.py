# File: ballista/sdks/python/src/ballista_client/models.py
"""Typed API models (Pydantic v2), mirroring the server's OpenAPI document."""

from __future__ import annotations

import re
from datetime import datetime
from enum import StrEnum
from typing import Annotated, Any, Generic, Literal, Self, TypeVar

from pydantic import BaseModel, ConfigDict, Field, computed_field, field_validator, model_validator

NAME_RE = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")
BUNDLE_ID_RE = re.compile(r"^[0-9a-f]{32}$")
RUN_ID_RE = re.compile(r"^r[0-9a-f]{24}$")
DURATION_RE = re.compile(r"^\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d)(\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d))*\s*$")

Labels = Annotated[dict[str, str], Field(max_length=32)]


def check_labels(labels: dict[str, str]) -> dict[str, str]:
    for k, v in labels.items():
        if not NAME_RE.match(k):
            raise ValueError(f"label key {k!r} must match [A-Za-z0-9_.-]{{1,64}}")
        if len(v) > 256 or any(ord(c) < 32 or ord(c) == 127 for c in v):
            raise ValueError(f"label {k!r} must be up to 256 printable characters")
    return labels


class _Model(BaseModel):
    """Response model: tolerates fields added by newer servers."""

    model_config = ConfigDict(extra="allow", frozen=True, populate_by_name=True)


class _Request(BaseModel):
    """Request model: rejects unknown fields like the server does."""

    model_config = ConfigDict(extra="forbid", validate_assignment=True)


class Scope(StrEnum):
    READ = "read"
    RUN = "run"
    ADMIN = "admin"


class RunState(StrEnum):
    QUEUED = "queued"
    RUNNING = "running"
    STOPPING = "stopping"
    PASSED = "passed"
    FAILED = "failed"
    STOPPED = "stopped"
    CANCELLED = "cancelled"
    ERROR = "error"

    @property
    def terminal(self) -> bool:
        return self in _TERMINAL


_TERMINAL = frozenset({RunState.PASSED, RunState.FAILED, RunState.STOPPED, RunState.CANCELLED, RunState.ERROR})


class Health(_Model):
    status: Literal["ok"]
    version: str


class WhoAmI(_Model):
    name: str
    scope: Scope


class Limits(_Model):
    max_users: float
    max_run_time_secs: int
    max_queue: int


class WorkerInfo(_Model):
    id: int
    name: str
    address: str
    cores: int
    state: str
    users: int
    iterations: int
    cpu_pct: float | None = None
    saturated: bool = False


class ServerStatus(_Model):
    version: str
    active_run: str | None
    queued: int
    limits: Limits
    workers: list[WorkerInfo] = Field(default_factory=list)


class BundleFile(_Model):
    path: str
    size: int
    sha256: str


class BundleInfo(_Model):
    id: str
    plan_name: str
    scenarios: list[str]
    files: list[BundleFile]
    size: int
    labels: dict[str, str]
    created_at: datetime
    created_by: str


class PlanCheck(_Model):
    """Server-side validation result: the files a bundle must still carry and the normalized plan."""

    name: str
    scenarios: list[str]
    files: list[str]
    document: dict[str, Any]


class RunOverrides(_Request):
    users: Annotated[float, Field(gt=0)] | None = None
    spawn_rate: Annotated[float, Field(gt=0)] | None = None
    run_time: str | None = None
    iterations: Annotated[int, Field(ge=1)] | None = None
    tags: Annotated[list[str], Field(max_length=64)] | None = None
    exclude_tags: Annotated[list[str], Field(max_length=64)] | None = None
    host: str | None = None
    workers: Annotated[int, Field(ge=0, le=512)] | None = None
    """Pool workers to use: 0 runs on the server only; unset uses every available worker."""

    @field_validator("run_time")
    @classmethod
    def _duration(cls, v: str | None) -> str | None:
        if v is not None and not DURATION_RE.match(v):
            raise ValueError("run_time must be a duration such as '90s' or '5m'")
        return v

    @field_validator("tags", "exclude_tags")
    @classmethod
    def _tags(cls, v: list[str] | None) -> list[str] | None:
        if v is not None:
            bad = [t for t in v if not NAME_RE.match(t)]
            if bad:
                raise ValueError(f"invalid tags: {bad}")
        return v

    @field_validator("host")
    @classmethod
    def _host(cls, v: str | None) -> str | None:
        if v is not None and not re.match(r"^https?://[^\s/@]+(/[^\s]*)?$", v):
            raise ValueError("host must be an http(s) base URL without credentials")
        return v


Metric = Literal["p50", "p95", "p99", "avg", "error_rate", "rps"]
_DEFAULT_METRICS: tuple[Metric, ...] = ("p95", "p99", "error_rate")


class RegressionGate(_Request):
    """Fails a run that is worse than a baseline run beyond these limits."""

    baseline: str = "auto"
    max_regression_pct: Annotated[float, Field(ge=0, le=10_000)] = 10.0
    metrics: Annotated[list[Metric], Field(min_length=1, max_length=6)] = Field(default_factory=lambda: list(_DEFAULT_METRICS))
    min_samples: Annotated[int, Field(ge=0)] = 30
    min_change_ms: Annotated[float, Field(ge=0)] = 1.0
    max_error_rate_increase: Annotated[float, Field(ge=0, le=1)] = 0.01

    @field_validator("baseline")
    @classmethod
    def _baseline(cls, v: str) -> str:
        if v != "auto" and not RUN_ID_RE.match(v):
            raise ValueError("baseline must be 'auto' or a run id")
        return v


class MetricChange(_Model):
    kind: str
    method: str
    name: str
    metric: str
    baseline: float
    current: float
    change_pct: float | None = None
    regressed: bool


class Comparison(_Model):
    baseline: str | None = None
    passed: bool
    regressions: int
    changes: list[MetricChange] = Field(default_factory=list)
    unmatched: list[str] = Field(default_factory=list)
    note: str | None = None

    @property
    def regressed(self) -> list[MetricChange]:
        return [c for c in self.changes if c.regressed]


class RunRequest(_Request):
    bundle: str
    overrides: RunOverrides = Field(default_factory=RunOverrides)
    labels: Labels = Field(default_factory=dict)
    regression: RegressionGate | None = None

    @field_validator("bundle")
    @classmethod
    def _bundle(cls, v: str) -> str:
        if not BUNDLE_ID_RE.match(v):
            raise ValueError("bundle must be a 32-character bundle id")
        return v

    @field_validator("labels")
    @classmethod
    def _labels(cls, v: dict[str, str]) -> dict[str, str]:
        return check_labels(v)


class ThresholdOutcome(_Model):
    metric: str
    request: str
    min: float | None = None
    max: float | None = None
    actual: float | None = None
    passed: bool


class Summary(_Model):
    requests: int
    failures: int
    fail_ratio: float
    rps: float
    avg_ms: float
    p50_ms: float
    p95_ms: float
    p99_ms: float
    max_ms: float
    iterations: int
    duration_secs: float
    aborted: bool
    generator_saturated_secs: int
    thresholds: list[ThresholdOutcome]


class Run(_Model):
    id: str
    seq: int
    bundle: str
    plan_name: str
    state: RunState
    created_at: datetime
    created_by: str
    started_at: datetime | None = None
    ended_at: datetime | None = None
    labels: dict[str, str] = Field(default_factory=dict)
    overrides: RunOverrides = Field(default_factory=RunOverrides)
    passed: bool | None = None
    error: str | None = None
    summary: Summary | None = None
    stopped_by: str | None = None
    workers: int | None = None
    regression: RegressionGate | None = None
    comparison: Comparison | None = None
    idempotency_key: str | None = None
    queue_position: int | None = None

    @computed_field  # type: ignore[prop-decorator]
    @property
    def finished(self) -> bool:
        return self.state.terminal


class StatsRow(_Model):
    scenario: str = ""
    method: str = ""
    name: str = ""
    count: int = 0
    failures: int = 0
    fail_ratio: float = 0.0
    rps: float = 0.0
    fail_rps: float = 0.0
    rps_avg: float = 0.0
    avg_ms: float = 0.0
    min_ms: float = 0.0
    max_ms: float = 0.0
    p50_ms: float = 0.0
    p90_ms: float = 0.0
    p95_ms: float = 0.0
    p99_ms: float = 0.0
    p999_ms: float = 0.0
    avg_bytes: float = 0.0
    percentiles: list[float] = Field(default_factory=list)


class LatencyBucket(_Model):
    upper_ms: float | None
    count: int


class Snapshot(_Model):
    plan: str = ""
    state: str = ""
    elapsed_secs: float = 0.0
    users: int = 0
    target: float = 0.0
    total: StatsRow = Field(default_factory=StatsRow)
    rows: list[StatsRow] = Field(default_factory=list)
    transactions: list[StatsRow] = Field(default_factory=list)
    iterations: list[StatsRow] = Field(default_factory=list)
    errors: list[dict[str, Any]] = Field(default_factory=list)
    thresholds: list[dict[str, Any]] = Field(default_factory=list)
    distribution: list[LatencyBucket] = Field(default_factory=list)
    iterations_started: int = 0
    dropped_iterations: int = 0
    aborted: bool = False

    def row(self, name: str, method: str | None = None) -> StatsRow | None:
        """First request or transaction row with this name (and method, when given)."""
        for r in [*self.rows, *self.transactions]:
            if r.name == name and (method is None or r.method == method):
                return r
        return None


T = TypeVar("T")


class Page(_Model, Generic[T]):
    items: list[T]
    next_cursor: str | None = None


EventType = Literal["run", "stats", "end"]


class RunEvent(BaseModel):
    """One server-sent event: `run`/`end` carry a Run, `stats` a Snapshot."""

    model_config = ConfigDict(frozen=True)

    type: EventType
    run: Run | None = None
    stats: Snapshot | None = None

    @model_validator(mode="after")
    def _payload(self) -> Self:
        if (self.type == "stats") != (self.stats is not None) or (self.type != "stats") != (self.run is not None):
            raise ValueError("event payload does not match its type")
        return self
