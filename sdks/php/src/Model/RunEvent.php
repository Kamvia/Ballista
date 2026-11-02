<?php
// File: ballista/sdks/php/src/Model/RunEvent.php

declare(strict_types=1);

namespace Ballista\Client\Model;

/** One server-sent event: `run`/`end` carry a Run, `stats` a Snapshot. */
final readonly class RunEvent
{
    public function __construct(
        public string $type,
        public ?Run $run = null,
        public ?Snapshot $stats = null,
    ) {
    }
}
