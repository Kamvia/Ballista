# File: ballista/sdks/python/src/ballista_client/plan.py
"""Plan builder: compose structured plans that are sent to the server as JSON documents."""

from __future__ import annotations

import re
from typing import Annotated, Any, Literal, Self

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

from .models import DURATION_RE, NAME_RE

_IDENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$")

Duration = Annotated[str, Field(pattern=DURATION_RE.pattern)]
Name = Annotated[str, Field(pattern=NAME_RE.pattern)]


class _Spec(BaseModel):
    model_config = ConfigDict(extra="forbid", validate_assignment=True)

    def to_dict(self) -> dict[str, Any]:
        return self.model_dump(mode="json", exclude_none=True, exclude_defaults=True)


# ---------------------------------------------------------------- plan model


class Target(_Spec):
    base_url: str
    allowed_hosts: list[str] = Field(default_factory=list)
    timeout: Duration | None = None
    connect_timeout: Duration | None = None
    headers: dict[str, str] = Field(default_factory=dict)
    cookies: bool | None = None
    retries: Annotated[int, Field(ge=0, le=10)] | None = None
    retry_backoff: Duration | None = None
    retry_on: list[int] | None = None
    max_body_bytes: Annotated[int, Field(gt=0)] | None = None
    http2_prior_knowledge: bool | None = None

    @field_validator("base_url")
    @classmethod
    def _url(cls, v: str) -> str:
        if not re.match(r"^https?://[^\s/@]+(/[^\s]*)?$", v):
            raise ValueError("base_url must be an http(s) URL without credentials")
        return v


class Stage(_Spec):
    duration: Duration
    target: Annotated[float, Field(ge=0)]

    def to_dict(self) -> dict[str, Any]:
        return {"duration": self.duration, "target": self.target}


class Load(_Spec):
    mode: Literal["users", "arrival_rate"] = "users"
    stages: list[Stage] = Field(default_factory=list)
    shape_source: str | None = None
    graceful_stop: Duration | None = None
    max_users: Annotated[int, Field(gt=0)] | None = None
    iterations: Annotated[int, Field(gt=0)] | None = None
    tags: list[Name] = Field(default_factory=list)
    exclude_tags: list[Name] = Field(default_factory=list)

    @classmethod
    def users(cls, *stages: tuple[str, float], **kw: Any) -> Self:
        """Closed model: each stage ramps to `target` concurrent users over `duration`."""
        return cls(mode="users", stages=[Stage(duration=d, target=t) for d, t in stages], **kw)

    @classmethod
    def arrival_rate(cls, *stages: tuple[str, float], **kw: Any) -> Self:
        """Open model: each stage ramps to `target` iterations per second over `duration`."""
        return cls(mode="arrival_rate", stages=[Stage(duration=d, target=t) for d, t in stages], **kw)

    @classmethod
    def shape(cls, source: str, mode: Literal["users", "arrival_rate"] = "users", **kw: Any) -> Self:
        """Custom profile from a script defining `fn shape(t)`; the source travels inside the plan."""
        return cls(mode=mode, shape_source=source, **kw)

    @model_validator(mode="after")
    def _profile(self) -> Self:
        if bool(self.stages) == (self.shape_source is not None):
            raise ValueError("load needs either stages or shape_source")
        return self

    def to_dict(self) -> dict[str, Any]:
        d = super().to_dict()
        d["mode"] = self.mode
        if self.stages:
            d["stages"] = [s.to_dict() for s in self.stages]
        return d


class Wait(_Spec):
    """Delay between iterations of one user. Use the constructors."""

    between: list[Duration] | None = None
    constant: Duration | None = None
    pacing: Duration | None = None
    throughput: Annotated[float, Field(gt=0)] | None = None
    poisson: Duration | None = None

    @classmethod
    def uniform(cls, low: str, high: str) -> Self:
        return cls(between=[low, high])

    @classmethod
    def fixed(cls, d: str) -> Self:
        return cls(constant=d)

    @classmethod
    def pace(cls, d: str) -> Self:
        return cls(pacing=d)

    @classmethod
    def per_second(cls, rate: float) -> Self:
        return cls(throughput=rate)

    @classmethod
    def exponential(cls, mean: str) -> Self:
        return cls(poisson=mean)

    @model_validator(mode="after")
    def _one(self) -> Self:
        set_ = [x for x in (self.between, self.constant, self.pacing, self.throughput, self.poisson) if x is not None]
        if len(set_) != 1:
            raise ValueError("wait takes exactly one of between, constant, pacing, throughput, poisson")
        if self.between is not None and len(self.between) != 2:
            raise ValueError("between takes [min, max]")
        return self


class Step(_Spec):
    """One declarative step. Use the module-level helpers (get, post, think, ...)."""

    get: str | None = None
    post: str | None = None
    put: str | None = None
    patch: str | None = None
    delete: str | None = None
    head: str | None = None
    name: str | None = None
    headers: dict[str, str] = Field(default_factory=dict)
    query: dict[str, str] = Field(default_factory=dict)
    json_body: Any = Field(default=None, alias="json")
    body: str | None = None
    form: dict[str, str] | None = None
    expect: int | list[int] | None = None
    contains: str | None = None
    capture: dict[str, str] = Field(default_factory=dict)
    timeout: Duration | None = None
    retries: Annotated[int, Field(ge=0, le=10)] | None = None
    continue_on_failure: bool = False
    sse: Annotated[int, Field(ge=1, le=100_000)] | None = None
    reconnects: Annotated[int, Field(ge=0, le=100)] | None = None
    context: dict[str, str] = Field(default_factory=dict)
    think: Duration | list[Duration] | None = None
    feed: str | None = None
    set: dict[str, str] = Field(default_factory=dict)
    txn_begin: Name | None = None
    txn_end: Name | None = None
    batch: list[Step] = Field(default_factory=list)
    concurrency: Annotated[int, Field(ge=1, le=64)] | None = None

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    @field_validator("capture", "set")
    @classmethod
    def _idents(cls, v: dict[str, str]) -> dict[str, str]:
        bad = [k for k in v if not _IDENT.match(k)]
        if bad:
            raise ValueError(f"variable names must be identifiers: {bad}")
        return v

    def to_dict(self) -> dict[str, Any]:
        d = self.model_dump(mode="json", exclude_none=True, exclude_defaults=True, by_alias=True, exclude={"batch"})
        if self.batch:
            d["batch"] = [b.to_dict() for b in self.batch]
        return d


def _req(method: str, path: str, **kw: Any) -> Step:
    if "json" in kw:
        kw["json_body"] = kw.pop("json")
    return Step(**{method: path}, **kw)


def get(path: str, **kw: Any) -> Step:
    return _req("get", path, **kw)


def post(path: str, **kw: Any) -> Step:
    return _req("post", path, **kw)


def put(path: str, **kw: Any) -> Step:
    return _req("put", path, **kw)


def patch(path: str, **kw: Any) -> Step:
    return _req("patch", path, **kw)


def delete(path: str, **kw: Any) -> Step:
    return _req("delete", path, **kw)


def think(low: str, high: str | None = None) -> Step:
    return Step(think=[low, high] if high else low)


def feed(feeder: str) -> Step:
    return Step(feed=feeder)


def assign(**variables: str) -> Step:
    return Step(set=dict(variables))


def batch(*requests: Step, name: str = "batch", concurrency: int = 6) -> Step:
    return Step(batch=list(requests), name=name, concurrency=concurrency)


def transaction(name: str, *steps: Step) -> list[Step]:
    """Wraps steps so their total duration is recorded as one transaction."""
    return [Step(txn_begin=name), *steps, Step(txn_end=name)]


class Scenario(_Spec):
    name: Name
    weight: Annotated[int, Field(ge=1, le=10_000)] = 1
    source: str | None = None
    script: str | None = None
    steps: list[Step] = Field(default_factory=list)
    on_start: list[Step] = Field(default_factory=list)
    wait: Wait | None = None
    fixed_count: Annotated[int, Field(ge=1)] | None = None
    tags: list[Name] = Field(default_factory=list)

    @field_validator("steps", "on_start", mode="before")
    @classmethod
    def _flatten(cls, v: Any) -> Any:
        if isinstance(v, list):
            out: list[Any] = []
            for item in v:
                out.extend(item if isinstance(item, list) else [item])
            return out
        return v

    @model_validator(mode="after")
    def _kind(self) -> Self:
        kinds = sum(x for x in (self.source is not None, self.script is not None, bool(self.steps)))
        if kinds != 1:
            raise ValueError(f"scenario {self.name!r} needs exactly one of source (inline script), script (bundle file) or steps")
        if self.on_start and not self.steps:
            raise ValueError("on_start steps require steps")
        return self

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {"name": self.name}
        if self.weight != 1:
            d["weight"] = self.weight
        if self.source is not None:
            d["source"] = self.source
        if self.script:
            d["script"] = self.script
        if self.wait:
            d["wait"] = self.wait.to_dict()
        if self.fixed_count:
            d["fixed_count"] = self.fixed_count
        if self.tags:
            d["tags"] = self.tags
        if self.on_start:
            d["on_start"] = [s.to_dict() for s in self.on_start]
        if self.steps:
            d["step"] = [s.to_dict() for s in self.steps]
        return d


Metric = Literal["p50", "p90", "p95", "p99", "avg", "max", "error_rate", "rps", "dropped_iterations"]


class Threshold(_Spec):
    metric: Metric
    request: str = "*"
    max: float | None = None
    min: float | None = None
    abort_on_fail: bool = False
    abort_delay: Duration | None = None

    @model_validator(mode="after")
    def _bound(self) -> Self:
        if self.max is None and self.min is None:
            raise ValueError("threshold needs min or max")
        return self

    def to_dict(self) -> dict[str, Any]:
        d = super().to_dict()
        d["metric"] = self.metric
        return d


Cell = str | int | float | bool


class Feeder(_Spec):
    """Test data. Give `rows` inline (dicts, or lists with `columns`), or `path` to a bundle file."""

    name: Name
    rows: list[dict[str, Cell] | list[Cell]] | None = None
    columns: list[str] | None = None
    path: str | None = None
    mode: Literal["sequential", "random"] = "sequential"

    @model_validator(mode="after")
    def _source(self) -> Self:
        if (self.rows is None) == (self.path is None):
            raise ValueError(f"feeder {self.name!r} needs either rows or path")
        if self.rows is not None and not self.rows:
            raise ValueError(f"feeder {self.name!r} has no rows")
        if self.columns is not None and self.rows is None:
            raise ValueError("columns require rows")
        return self

    def to_dict(self) -> dict[str, Any]:
        d = super().to_dict()
        d["mode"] = self.mode
        return d


class Plan(_Spec):
    """A complete plan; `to_document()` is the structured form the server accepts."""

    name: str
    target: Target
    load: Load
    scenarios: list[Scenario]
    thresholds: list[Threshold] = Field(default_factory=list)
    feeders: list[Feeder] = Field(default_factory=list)
    percentiles: list[Annotated[float, Field(gt=0, le=100)]] = Field(default_factory=list, max_length=12)

    @model_validator(mode="after")
    def _unique(self) -> Self:
        names = [s.name for s in self.scenarios]
        if not names:
            raise ValueError("plan requires at least one scenario")
        if len(set(names)) != len(names):
            raise ValueError("scenario names must be unique")
        return self

    def to_document(self) -> dict[str, Any]:
        doc: dict[str, Any] = {"name": self.name, "target": self.target.to_dict(), "load": self.load.to_dict()}
        doc["scenario"] = [s.to_dict() for s in self.scenarios]
        if self.feeders:
            doc["feeder"] = [f.to_dict() for f in self.feeders]
        if self.thresholds:
            doc["threshold"] = [t.to_dict() for t in self.thresholds]
        if self.percentiles:
            doc["report"] = {"percentiles": self.percentiles}
        return doc
