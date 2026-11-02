<?php
// File: ballista/sdks/php/src/Load.php

declare(strict_types=1);

namespace Ballista\Client;

/** Load profiles: closed (concurrent users) or open (iterations per second). */
final class Load
{
    /**
     * @param list<array{0: string, 1: int|float}> $stages duration and target per stage
     * @param array<string, mixed> $extra
     * @return array<string, mixed>
     */
    public static function users(array $stages, array $extra = []): array
    {
        return ['mode' => 'users', 'stages' => self::stages($stages)] + $extra;
    }

    /**
     * @param list<array{0: string, 1: int|float}> $stages
     * @param array<string, mixed> $extra
     * @return array<string, mixed>
     */
    public static function arrivalRate(array $stages, array $extra = []): array
    {
        return ['mode' => 'arrival_rate', 'stages' => self::stages($stages)] + $extra;
    }

    /**
     * Custom profile from a script defining `fn shape(t)`; the source travels inside the plan.
     *
     * @param array<string, mixed> $extra
     * @return array<string, mixed>
     */
    public static function shape(string $source, string $mode = 'users', array $extra = []): array
    {
        return ['mode' => $mode, 'shape_source' => $source] + $extra;
    }

    /**
     * @param list<array{0: string, 1: int|float}> $stages
     * @return list<array{duration: string, target: int|float}>
     */
    private static function stages(array $stages): array
    {
        return array_map(static fn (array $s): array => ['duration' => $s[0], 'target' => $s[1]], $stages);
    }
}
