<?php
// File: ballista/sdks/php/src/Model/BundleInfo.php

declare(strict_types=1);

namespace Ballista\Client\Model;

final readonly class BundleInfo
{
    /**
     * @param list<string> $scenarios
     * @param list<array{path: string, size: int, sha256: string}> $files
     * @param array<string, string> $labels
     */
    public function __construct(
        public string $id,
        public string $planName,
        public array $scenarios,
        public array $files,
        public int $size,
        public array $labels,
        public string $createdAt,
        public string $createdBy,
    ) {
    }

    public static function fromArray(mixed $data): self
    {
        $f = Fields::of($data, 'bundle');
        $files = [];
        foreach ($f->list('files') as $item) {
            $ff = Fields::of($item, 'bundle file');
            $files[] = ['path' => $ff->str('path'), 'size' => $ff->int('size'), 'sha256' => $ff->str('sha256')];
        }
        return new self(
            $f->str('id'),
            $f->str('plan_name'),
            array_map(static fn (mixed $s): string => is_string($s) ? $s : '', $f->list('scenarios')),
            $files,
            $f->int('size'),
            $f->stringMap('labels'),
            $f->str('created_at'),
            $f->str('created_by'),
        );
    }
}
