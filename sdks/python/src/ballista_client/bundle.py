# File: ballista/sdks/python/src/ballista_client/bundle.py
"""Test bundles: a structured plan plus any binary assets it references."""

from __future__ import annotations

import base64
import re
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Self

from .errors import ConfigurationError
from .models import check_labels
from .plan import Plan

MAX_FILES = 256
MAX_FILE_BYTES = 16 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_PLAN_BYTES = 1024 * 1024
_SEGMENT = re.compile(r"^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}$")
_RESERVED = {"plan.toml", "bundle.json"}

PlanInput = Plan | Mapping[str, Any] | str


def valid_bundle_path(rel: str) -> bool:
    """Mirrors the server rule: relative, at most 8 segments, no hidden or parent segments."""
    if not rel or len(rel) > 200 or rel.startswith("/"):
        return False
    parts = rel.split("/")
    return len(parts) <= 8 and all(_SEGMENT.match(p) for p in parts) and parts[0] != "inline"


def plan_payload(plan: PlanInput) -> dict[str, Any] | str:
    """The `plan` field sent to the server: a structured document, or plan file text."""
    if isinstance(plan, Plan):
        return plan.to_document()
    if isinstance(plan, str):
        if not plan.strip() or len(plan.encode()) > MAX_PLAN_BYTES:
            raise ConfigurationError("plan text must be non-empty and at most 1 MiB")
        return plan
    if isinstance(plan, Mapping):
        return dict(plan)
    raise ConfigurationError("plan must be a Plan, a mapping or plan text")


class Bundle:
    """A plan and optional assets (descriptor sets, certificates, upload files) sent as content.

    Inline scripts (`Scenario(source=...)`) and inline feeder rows need no files.
    """

    def __init__(self, plan: PlanInput, *, labels: Mapping[str, str] | None = None) -> None:
        self.plan = plan_payload(plan)
        try:
            self.labels: dict[str, str] = check_labels(dict(labels or {}))
        except ValueError as e:
            raise ConfigurationError(str(e)) from e
        self._files: dict[str, bytes] = {}

    def add_bytes(self, rel: str, data: bytes) -> Self:
        if not valid_bundle_path(rel) or rel in _RESERVED:
            raise ConfigurationError(f"invalid bundle path {rel!r}")
        if len(data) > MAX_FILE_BYTES:
            raise ConfigurationError(f"{rel!r} exceeds 16 MiB")
        if rel not in self._files and len(self._files) >= MAX_FILES:
            raise ConfigurationError(f"a bundle holds at most {MAX_FILES} files")
        if sum(len(d) for k, d in self._files.items() if k != rel) + len(data) > MAX_TOTAL_BYTES:
            raise ConfigurationError("bundle exceeds 64 MiB")
        self._files[rel] = data
        return self

    def add_text(self, rel: str, text: str) -> Self:
        return self.add_bytes(rel, text.encode("utf-8"))

    def add_path(self, rel: str, path: str | Path) -> Self:
        """Adds the content of a local file under the bundle path `rel`."""
        p = Path(path)
        if not p.is_file():
            raise ConfigurationError(f"{rel!r}: {p} is not a regular file")
        if p.stat().st_size > MAX_FILE_BYTES:
            raise ConfigurationError(f"{rel!r} exceeds 16 MiB")
        return self.add_bytes(rel, p.read_bytes())

    @property
    def files(self) -> dict[str, int]:
        """Relative path to size in bytes."""
        return {k: len(v) for k, v in sorted(self._files.items())}

    def to_payload(self) -> dict[str, Any]:
        return {
            "plan": self.plan,
            "files": {k: base64.b64encode(v).decode("ascii") for k, v in sorted(self._files.items())},
            "labels": self.labels,
        }

    def __repr__(self) -> str:
        kind = "document" if isinstance(self.plan, dict) else "text"
        return f"Bundle(plan={kind}, files={len(self._files)})"
