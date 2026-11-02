<?php
// File: ballista/sdks/php/src/Model/Snapshot.php

declare(strict_types=1);

namespace Ballista\Client\Model;

/** Engine statistics. `raw` keeps every field, including timeline and distribution. */
final readonly class Snapshot
{
    /**
     * @param list<StatsRow> $rows
     * @param list<StatsRow> $transactions
     * @param array<string, mixed> $raw
     */
    public function __construct(
        public string $plan,
        public string $state,
        public float $elapsedSecs,
        public int $users,
        public StatsRow $total,
        public array $rows,
        public array $transactions,
        public array $raw,
    ) {
    }

    public static function fromArray(mixed $data): self
    {
        $f = Fields::of($data, 'snapshot');
        return new self(
            $f->optStr('plan') ?? '',
            $f->optStr('state') ?? '',
            $f->float('elapsed_secs', 0.0),
            $f->int('users', 0),
            StatsRow::fromArray($f->has('total') ? $f->raw()['total'] : ['count' => 0]),
            array_map(StatsRow::fromArray(...), $f->list('rows')),
            array_map(StatsRow::fromArray(...), $f->list('transactions')),
            $f->raw(),
        );
    }

    public function row(string $name, ?string $method = null): ?StatsRow
    {
        foreach ([...$this->rows, ...$this->transactions] as $r) {
            if ($r->name === $name && ($method === null || $r->method === $method)) {
                return $r;
            }
        }
        return null;
    }
}
