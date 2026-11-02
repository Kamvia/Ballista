<?php
// File: ballista/sdks/php/src/Model/RunState.php

declare(strict_types=1);

namespace Ballista\Client\Model;

enum RunState: string
{
    case Queued = 'queued';
    case Running = 'running';
    case Stopping = 'stopping';
    case Passed = 'passed';
    case Failed = 'failed';
    case Stopped = 'stopped';
    case Cancelled = 'cancelled';
    case Error = 'error';

    public function isTerminal(): bool
    {
        return match ($this) {
            self::Passed, self::Failed, self::Stopped, self::Cancelled, self::Error => true,
            default => false,
        };
    }
}
