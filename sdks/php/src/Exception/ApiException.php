<?php
// File: ballista/sdks/php/src/Exception/ApiException.php

declare(strict_types=1);

namespace Ballista\Client\Exception;

/** The server answered with an error status. */
class ApiException extends BallistaException
{
    public function __construct(
        public readonly int $status,
        public readonly string $errorCode,
        public readonly string $detail,
        public readonly ?float $retryAfter = null,
    ) {
        parent::__construct(sprintf("%d %s: %s", $status, $errorCode, $detail), $status);
    }

    public static function forStatus(int $status, string $code, string $detail, ?float $retryAfter): self
    {
        return match (true) {
            $status === 401 => new AuthenticationException($status, $code, $detail, $retryAfter),
            $status === 403 => new PermissionDeniedException($status, $code, $detail, $retryAfter),
            $status === 404 => new NotFoundException($status, $code, $detail, $retryAfter),
            $status === 409 => new ConflictException($status, $code, $detail, $retryAfter),
            $status === 422 => new ValidationException($status, $code, $detail, $retryAfter),
            $status === 429 => new RateLimitException($status, $code, $detail, $retryAfter),
            $status >= 500 => new ServerException($status, $code, $detail, $retryAfter),
            default => new self($status, $code, $detail, $retryAfter),
        };
    }
}
