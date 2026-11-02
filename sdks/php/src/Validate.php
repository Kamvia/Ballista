<?php
// File: ballista/sdks/php/src/Validate.php

declare(strict_types=1);

namespace Ballista\Client;

use Ballista\Client\Exception\ConfigurationException;

/** Input checks shared by the client and the plan builder (they mirror the server's rules). */
final class Validate
{
    public const NAME = '/^[A-Za-z0-9_.-]{1,64}$/';
    public const IDENT = '/^[A-Za-z_][A-Za-z0-9_]{0,63}$/';
    public const BUNDLE_ID = '/^[0-9a-f]{32}$/';
    public const RUN_ID = '/^r[0-9a-f]{24}$/';
    public const DURATION = '/^\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d)(\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d))*\s*$/';
    public const BASE_URL = '/^https?:\/\/[^\s\/@]+(\/\S*)?$/';

    public static function name(string $v, string $what): string
    {
        if (preg_match(self::NAME, $v) !== 1) {
            throw new ConfigurationException("{$what}: '{$v}' must match [A-Za-z0-9_.-]{1,64}");
        }
        return $v;
    }

    public static function duration(string $v, string $what): string
    {
        if (preg_match(self::DURATION, $v) !== 1) {
            throw new ConfigurationException("{$what}: '{$v}' is not a duration such as '500ms' or '2m'");
        }
        return $v;
    }

    public static function id(string $v, string $pattern, string $what): string
    {
        if (preg_match($pattern, $v) !== 1) {
            throw new ConfigurationException("invalid {$what} id");
        }
        return $v;
    }

    /**
     * @param array<array-key, mixed> $labels
     * @return array<string, string>
     */
    public static function labels(array $labels): array
    {
        if (count($labels) > 32) {
            throw new ConfigurationException('at most 32 labels');
        }
        $out = [];
        foreach ($labels as $k => $v) {
            $k = (string) $k;
            if (preg_match(self::NAME, $k) !== 1) {
                throw new ConfigurationException("label key '{$k}' must match [A-Za-z0-9_.-]{1,64}");
            }
            if (!is_string($v) || strlen($v) > 256 || preg_match('/[\x00-\x1f\x7f]/', $v) === 1) {
                throw new ConfigurationException("label '{$k}' must be up to 256 printable characters");
            }
            $out[$k] = $v;
        }
        return $out;
    }

    /**
     * @param array<string, mixed> $o
     * @return array<string, mixed>
     */
    public static function overrides(array $o): array
    {
        $allowed = ['users', 'spawn_rate', 'run_time', 'iterations', 'tags', 'exclude_tags', 'host', 'workers'];
        foreach ($o as $k => $v) {
            if (!in_array($k, $allowed, true)) {
                throw new ConfigurationException("unknown override '{$k}'");
            }
        }
        foreach (['users', 'spawn_rate'] as $k) {
            if (isset($o[$k]) && (!(is_int($o[$k]) || is_float($o[$k])) || !is_finite((float) $o[$k]) || $o[$k] <= 0)) {
                throw new ConfigurationException("{$k} must be positive");
            }
        }
        if (isset($o['iterations']) && (!is_int($o['iterations']) || $o['iterations'] < 1)) {
            throw new ConfigurationException('iterations must be a positive integer');
        }
        if (isset($o['workers']) && (!is_int($o['workers']) || $o['workers'] < 0 || $o['workers'] > 512)) {
            throw new ConfigurationException('workers must be an integer 0..512');
        }
        if (isset($o['run_time'])) {
            self::duration((string) $o['run_time'], 'run_time');
        }
        foreach (['tags', 'exclude_tags'] as $k) {
            if (isset($o[$k])) {
                if (!is_array($o[$k]) || count($o[$k]) > 64) {
                    throw new ConfigurationException("{$k} must be a list of up to 64 names");
                }
                foreach ($o[$k] as $t) {
                    self::name((string) $t, $k);
                }
            }
        }
        if (isset($o['host']) && preg_match(self::BASE_URL, (string) $o['host']) !== 1) {
            throw new ConfigurationException('host must be an http(s) base URL without credentials');
        }
        return array_filter($o, static fn ($v) => $v !== null);
    }

    /**
     * @param array<string, mixed> $g
     * @return array<string, mixed>
     */
    public static function regression(array $g): array
    {
        $allowed = ['baseline', 'max_regression_pct', 'metrics', 'min_samples', 'min_change_ms', 'max_error_rate_increase'];
        foreach ($g as $k => $v) {
            if (!in_array($k, $allowed, true)) {
                throw new ConfigurationException("unknown regression option '{$k}'");
            }
        }
        if (isset($g['baseline']) && $g['baseline'] !== 'auto' && preg_match(self::RUN_ID, (string) $g['baseline']) !== 1) {
            throw new ConfigurationException("baseline must be 'auto' or a run id");
        }
        foreach (['max_regression_pct', 'min_samples', 'min_change_ms', 'max_error_rate_increase'] as $k) {
            if (isset($g[$k]) && (!is_numeric($g[$k]) || $g[$k] < 0)) {
                throw new ConfigurationException("{$k} must be a non-negative number");
            }
        }
        $metrics = ['p50', 'p95', 'p99', 'avg', 'error_rate', 'rps'];
        if (isset($g['metrics'])) {
            if (!is_array($g['metrics']) || $g['metrics'] === [] || count($g['metrics']) > 6 || array_diff($g['metrics'], $metrics) !== []) {
                throw new ConfigurationException('metrics must list 1..6 of ' . implode(', ', $metrics));
            }
        }
        return $g;
    }
}
