<?php
// File: ballista/sdks/php/src/Model/Fields.php

declare(strict_types=1);

namespace Ballista\Client\Model;

use Ballista\Client\Exception\BallistaException;

/** Typed accessors for decoded JSON responses; malformed responses raise instead of yielding wrong types. */
final class Fields
{
    /** @param array<string, mixed> $data */
    public function __construct(private readonly array $data, private readonly string $what)
    {
    }

    /** @param mixed $value */
    public static function of(mixed $value, string $what): self
    {
        if (!is_array($value) || ($value !== [] && array_is_list($value))) {
            throw new BallistaException("unexpected response: {$what} is not an object");
        }
        /** @var array<string, mixed> $value */
        return new self($value, $what);
    }

    private function fail(string $key, string $type): never
    {
        throw new BallistaException("unexpected response: {$this->what}.{$key} is not {$type}");
    }

    public function str(string $key): string
    {
        $v = $this->data[$key] ?? null;
        return is_string($v) ? $v : $this->fail($key, 'a string');
    }

    public function optStr(string $key): ?string
    {
        $v = $this->data[$key] ?? null;
        return $v === null || is_string($v) ? $v : $this->fail($key, 'a string');
    }

    public function int(string $key, ?int $default = null): int
    {
        $v = $this->data[$key] ?? $default;
        return is_int($v) ? $v : $this->fail($key, 'an integer');
    }

    public function float(string $key, ?float $default = null): float
    {
        $v = $this->data[$key] ?? $default;
        return is_int($v) || is_float($v) ? (float) $v : $this->fail($key, 'a number');
    }

    public function optFloat(string $key): ?float
    {
        $v = $this->data[$key] ?? null;
        return $v === null ? null : $this->float($key);
    }

    public function bool(string $key, ?bool $default = null): bool
    {
        $v = $this->data[$key] ?? $default;
        return is_bool($v) ? $v : $this->fail($key, 'a boolean');
    }

    public function optBool(string $key): ?bool
    {
        $v = $this->data[$key] ?? null;
        return $v === null ? null : $this->bool($key);
    }

    /** @return list<mixed> */
    public function list(string $key): array
    {
        $v = $this->data[$key] ?? [];
        return is_array($v) && array_is_list($v) ? $v : $this->fail($key, 'a list');
    }

    /** @return array<string, string> */
    public function stringMap(string $key): array
    {
        $v = $this->data[$key] ?? [];
        if (!is_array($v)) {
            $this->fail($key, 'an object');
        }
        $out = [];
        foreach ($v as $k => $s) {
            if (!is_string($s)) {
                $this->fail($key, 'a map of strings');
            }
            $out[(string) $k] = $s;
        }
        return $out;
    }

    /** @return array<string, mixed> */
    public function map(string $key): array
    {
        $v = $this->data[$key] ?? [];
        return is_array($v) ? $v : $this->fail($key, 'an object');
    }

    public function has(string $key): bool
    {
        return isset($this->data[$key]);
    }

    /** @return array<string, mixed> */
    public function raw(): array
    {
        return $this->data;
    }
}
