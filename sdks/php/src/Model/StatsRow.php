<?php
// File: ballista/sdks/php/src/Model/StatsRow.php

declare(strict_types=1);

namespace Ballista\Client\Model;

final readonly class StatsRow
{
    public function __construct(
        public string $scenario,
        public string $method,
        public string $name,
        public int $count,
        public int $failures,
        public float $failRatio,
        public float $rps,
        public float $avgMs,
        public float $p50Ms,
        public float $p90Ms,
        public float $p95Ms,
        public float $p99Ms,
        public float $maxMs,
    ) {
    }

    public static function fromArray(mixed $data): self
    {
        $f = Fields::of($data, 'row');
        return new self(
            $f->optStr('scenario') ?? '',
            $f->optStr('method') ?? '',
            $f->optStr('name') ?? '',
            $f->int('count', 0),
            $f->int('failures', 0),
            $f->float('fail_ratio', 0.0),
            $f->float('rps', 0.0),
            $f->float('avg_ms', 0.0),
            $f->float('p50_ms', 0.0),
            $f->float('p90_ms', 0.0),
            $f->float('p95_ms', 0.0),
            $f->float('p99_ms', 0.0),
            $f->float('max_ms', 0.0),
        );
    }
}
