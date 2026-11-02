# File: ballista/sdks/python/src/ballista_client/errors.py
"""Exception hierarchy. Messages never include the API token."""

from __future__ import annotations


class BallistaError(Exception):
    """Base class for every error raised by this package."""


class ConfigurationError(BallistaError):
    """Invalid client configuration or input rejected before sending."""


class TransportError(BallistaError):
    """The server could not be reached or the connection failed."""


class ApiError(BallistaError):
    """The server answered with an error status."""

    def __init__(self, status: int, code: str, message: str, retry_after: float | None = None) -> None:
        super().__init__(f"{status} {code}: {message}")
        self.status = status
        self.code = code
        self.message = message
        self.retry_after = retry_after


class AuthenticationError(ApiError):
    """401: missing or invalid token."""


class PermissionDeniedError(ApiError):
    """403: the token lacks the required scope."""


class NotFoundError(ApiError):
    """404."""


class ConflictError(ApiError):
    """409: the resource is in a state that does not allow the operation."""


class ValidationError(ApiError):
    """422: the request, bundle or plan was rejected."""


class RateLimitError(ApiError):
    """429: rate limited or run queue full."""


class ServerError(ApiError):
    """5xx."""


class RunTimeoutError(BallistaError):
    """A wait for run completion exceeded its deadline."""


_BY_STATUS: dict[int, type[ApiError]] = {
    401: AuthenticationError,
    403: PermissionDeniedError,
    404: NotFoundError,
    409: ConflictError,
    422: ValidationError,
    429: RateLimitError,
}


def error_for(status: int, code: str, message: str, retry_after: float | None) -> ApiError:
    cls = _BY_STATUS.get(status, ServerError if status >= 500 else ApiError)
    return cls(status, code, message, retry_after)
