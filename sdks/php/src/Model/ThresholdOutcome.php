<?php
// File: ballista/sdks/php/src/Model/ThresholdOutcome.php

declare(strict_types=1);

namespace Ballista\Client\Model;

final readonly class ThresholdOutcome
{
    public function __construct(
        public string $metric,
        public string $request,
        public ?float $min,
        public ?float $max,
        public ?float $actual,
        public bool $passed,
    ) {
    }

    public static function fromArray(mixed $data): self
    {
        $f = Fields::of($data, 'threshold');
        return new self($f->str('metric'), $f->str('request'), $f->optFloat('min'), $f->optFloat('max'), $f->optFloat('actual'), $f->bool('passed'));
    }
}
