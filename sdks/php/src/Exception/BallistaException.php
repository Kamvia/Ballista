<?php
// File: ballista/sdks/php/src/Exception/BallistaException.php

declare(strict_types=1);

namespace Ballista\Client\Exception;

/** Base class for every exception thrown by this package. Messages never include the API token. */
class BallistaException extends \RuntimeException
{
}
