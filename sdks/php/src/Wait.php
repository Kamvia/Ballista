<?php
// File: ballista/sdks/php/src/Wait.php

declare(strict_types=1);

namespace Ballista\Client;

/** Delay between iterations of one user. */
final class Wait
{
    /** @return array<string, mixed> */
    public static function uniform(string $low, string $high): array
    {
        return ['between' => [$low, $high]];
    }

    /** @return array<string, mixed> */
    public static function fixed(string $d): array
    {
        return ['constant' => $d];
    }

    /** @return array<string, mixed> */
    public static function pace(string $d): array
    {
        return ['pacing' => $d];
    }

    /** @return array<string, mixed> */
    public static function perSecond(float $rate): array
    {
        return ['throughput' => $rate];
    }

    /** @return array<string, mixed> */
    public static function exponential(string $mean): array
    {
        return ['poisson' => $mean];
    }
}
