<?php
// File: ballista/sdks/php/src/Exception/RateLimitException.php

declare(strict_types=1);

namespace Ballista\Client\Exception;

/** 429 rate limited or run queue full */
final class RateLimitException extends ApiException
{
}
