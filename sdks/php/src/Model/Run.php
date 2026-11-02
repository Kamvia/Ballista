<?php
// File: ballista/sdks/php/src/Model/Run.php

declare(strict_types=1);

namespace Ballista\Client\Model;

use Ballista\Client\Exception\BallistaException;

final readonly class Run
{
    /**
     * @param array<string, string> $labels
     * @param array<string, mixed> $overrides
     * @param array<string, mixed>|null $regression
     * @param array<string, mixed>|null $comparison
     */
    public function __construct(
        public string $id,
        public int $seq,
        public string $bundle,
        public string $planName,
        public RunState $state,
        public string $createdAt,
        public string $createdBy,
        public ?string $startedAt,
        public ?string $endedAt,
        public array $labels,
        public array $overrides,
        public ?bool $passed,
        public ?string $error,
        public ?Summary $summary,
        public ?string $stoppedBy,
        public ?int $queuePosition,
        public ?int $workers = null,
        public ?array $regression = null,
        public ?array $comparison = null,
    ) {
    }

    public static function fromArray(mixed $data): self
    {
        $f = Fields::of($data, 'run');
        $state = RunState::tryFrom($f->str('state')) ?? throw new BallistaException('unexpected response: unknown run state');
        $id = $f->str('id');
        if (preg_match('/^r[0-9a-f]{24}$/', $id) !== 1) {
            throw new BallistaException('unexpected response: malformed run id');
        }
        return new self(
            $id,
            $f->int('seq'),
            $f->str('bundle'),
            $f->str('plan_name'),
            $state,
            $f->str('created_at'),
            $f->str('created_by'),
            $f->optStr('started_at'),
            $f->optStr('ended_at'),
            $f->stringMap('labels'),
            $f->map('overrides'),
            $f->optBool('passed'),
            $f->optStr('error'),
            $f->has('summary') ? Summary::fromArray($f->raw()['summary']) : null,
            $f->optStr('stopped_by'),
            $f->has('queue_position') ? $f->int('queue_position') : null,
            $f->has('workers') ? $f->int('workers') : null,
            $f->has('regression') ? $f->map('regression') : null,
            $f->has('comparison') ? $f->map('comparison') : null,
        );
    }

    public function isFinished(): bool
    {
        return $this->state->isTerminal();
    }
}
