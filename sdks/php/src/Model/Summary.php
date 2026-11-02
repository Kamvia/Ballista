<?php
// File: ballista/sdks/php/src/Model/Summary.php

declare(strict_types=1);

namespace Ballista\Client\Model;

final readonly class Summary
{
    /** @param list<ThresholdOutcome> $thresholds */
    public function __construct(
        public int $requests,
        public int $failures,
        public float $failRatio,
        public float $rps,
        public float $avgMs,
        public float $p50Ms,
        public float $p95Ms,
        public float $p99Ms,
        public float $maxMs,
        public int $iterations,
        public float $durationSecs,
        public bool $aborted,
        public int $generatorSaturatedSecs,
        public array $thresholds,
    ) {
    }

    public static function fromArray(mixed $data): self
    {
        $f = Fields::of($data, 'summary');
        return new self(
            $f->int('requests'),
            $f->int('failures'),
            $f->float('fail_ratio'),
            $f->float('rps'),
            $f->float('avg_ms'),
            $f->float('p50_ms'),
            $f->float('p95_ms'),
            $f->float('p99_ms'),
            $f->float('max_ms'),
            $f->int('iterations'),
            $f->float('duration_secs'),
            $f->bool('aborted'),
            $f->int('generator_saturated_secs'),
            array_map(ThresholdOutcome::fromArray(...), $f->list('thresholds')),
        );
    }
}
