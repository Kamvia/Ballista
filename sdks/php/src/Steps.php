<?php
// File: ballista/sdks/php/src/Steps.php

declare(strict_types=1);

namespace Ballista\Client;

/** Factories for declarative scenario steps. Request options mirror the plan's step fields. */
final class Steps
{
    /** @param array<string, mixed> $options @return array<string, mixed> */
    public static function get(string $path, array $options = []): array
    {
        return ['get' => $path] + $options;
    }

    /** @param array<string, mixed> $options @return array<string, mixed> */
    public static function post(string $path, array $options = []): array
    {
        return ['post' => $path] + $options;
    }

    /** @param array<string, mixed> $options @return array<string, mixed> */
    public static function put(string $path, array $options = []): array
    {
        return ['put' => $path] + $options;
    }

    /** @param array<string, mixed> $options @return array<string, mixed> */
    public static function patch(string $path, array $options = []): array
    {
        return ['patch' => $path] + $options;
    }

    /** @param array<string, mixed> $options @return array<string, mixed> */
    public static function delete(string $path, array $options = []): array
    {
        return ['delete' => $path] + $options;
    }

    /** @return array<string, mixed> */
    public static function think(string $low, ?string $high = null): array
    {
        return ['think' => $high === null ? $low : [$low, $high]];
    }

    /** @return array<string, mixed> */
    public static function feed(string $feeder): array
    {
        return ['feed' => $feeder];
    }

    /** @param array<string, string> $vars @return array<string, mixed> */
    public static function assign(array $vars): array
    {
        return ['set' => $vars];
    }

    /** @param list<array<string, mixed>> $requests @return array<string, mixed> */
    public static function batch(array $requests, string $name = 'batch', int $concurrency = 6): array
    {
        return ['batch' => $requests, 'name' => $name, 'concurrency' => $concurrency];
    }

    /**
     * Wraps steps so their total duration is recorded as one transaction.
     *
     * @param array<string, mixed> ...$steps
     * @return list<array<string, mixed>>
     */
    public static function transaction(string $name, array ...$steps): array
    {
        return [['txn_begin' => $name], ...array_values($steps), ['txn_end' => $name]];
    }
}
