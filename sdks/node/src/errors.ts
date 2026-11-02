// File: ballista/sdks/node/src/errors.ts
/** Base class for every error raised by this package. Messages never include the API token. */
export class BallistaError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name;
  }
}

/** Invalid client configuration or input rejected before sending. */
export class ConfigurationError extends BallistaError {}

/** The server could not be reached or the connection failed. */
export class TransportError extends BallistaError {}

/** A wait for run completion exceeded its deadline. */
export class RunTimeoutError extends BallistaError {}

/** The server answered with an error status. */
export class ApiError extends BallistaError {
  constructor(
    readonly status: number,
    readonly code: string,
    readonly detail: string,
    readonly retryAfter: number | null = null,
  ) {
    super(`${status} ${code}: ${detail}`);
  }
}

export class AuthenticationError extends ApiError {}
export class PermissionDeniedError extends ApiError {}
export class NotFoundError extends ApiError {}
export class ConflictError extends ApiError {}
export class ValidationError extends ApiError {}
export class RateLimitError extends ApiError {}
export class ServerError extends ApiError {}

const BY_STATUS: Record<number, typeof ApiError> = {
  401: AuthenticationError,
  403: PermissionDeniedError,
  404: NotFoundError,
  409: ConflictError,
  422: ValidationError,
  429: RateLimitError,
};

export function errorFor(status: number, code: string, detail: string, retryAfter: number | null): ApiError {
  const Cls = BY_STATUS[status] ?? (status >= 500 ? ServerError : ApiError);
  return new Cls(status, code, detail, retryAfter);
}
