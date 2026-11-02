<?php
// File: ballista/sdks/php/src/Plan.php

declare(strict_types=1);

namespace Ballista\Client;

use Ballista\Client\Exception\ConfigurationException;

/** A validated plan; `toDocument()` is the structured form the server accepts. */
final readonly class Plan
{
    private const METHODS = ['get', 'post', 'put', 'patch', 'delete', 'head'];
    private const ACTIONS = ['think', 'feed', 'set', 'txn_begin', 'txn_end', 'batch'];
    private const STEP_KEYS = [
        'get', 'post', 'put', 'patch', 'delete', 'head', 'name', 'headers', 'query', 'json', 'body', 'form', 'expect',
        'contains', 'capture', 'timeout', 'retries', 'continue_on_failure', 'sse', 'reconnects', 'context', 'think',
        'feed', 'set', 'txn_begin', 'txn_end', 'batch', 'concurrency',
    ];
    private const METRICS = ['p50', 'p90', 'p95', 'p99', 'avg', 'max', 'error_rate', 'rps', 'dropped_iterations'];

    /**
     * @param array<string, mixed> $target
     * @param array<string, mixed> $load
     * @param list<array<string, mixed>> $scenarios
     * @param list<array<string, mixed>> $thresholds
     * @param list<array<string, mixed>> $feeders
     * @param list<int|float> $percentiles
     */
    public function __construct(
        public string $name,
        public array $target,
        public array $load,
        public array $scenarios,
        public array $thresholds = [],
        public array $feeders = [],
        public array $percentiles = [],
    ) {
        $this->validate();
    }

    private function validate(): void
    {
        if ($this->name === '' || strlen($this->name) > 128) {
            throw new ConfigurationException('plan name must be 1..128 characters');
        }
        if (preg_match(Validate::BASE_URL, (string) ($this->target['base_url'] ?? '')) !== 1) {
            throw new ConfigurationException('target.base_url must be an http(s) URL without credentials');
        }
        foreach (['timeout', 'connect_timeout', 'retry_backoff'] as $k) {
            if (isset($this->target[$k])) {
                Validate::duration((string) $this->target[$k], "target.{$k}");
            }
        }
        $stages = $this->load['stages'] ?? [];
        if (!is_array($stages) || (($stages !== []) === isset($this->load['shape_source']))) {
            throw new ConfigurationException('load needs either stages or shape_source');
        }
        foreach ($stages as $s) {
            Validate::duration((string) ($s['duration'] ?? ''), 'load.stages');
            if (!is_numeric($s['target'] ?? null) || $s['target'] < 0) {
                throw new ConfigurationException('stage targets must be non-negative numbers');
            }
        }
        if ($this->scenarios === []) {
            throw new ConfigurationException('plan requires at least one scenario');
        }
        $seen = [];
        foreach ($this->scenarios as $sc) {
            $name = Validate::name((string) ($sc['name'] ?? ''), 'scenario');
            if (isset($seen[$name])) {
                throw new ConfigurationException("duplicate scenario '{$name}'");
            }
            $seen[$name] = true;
            $kinds = (int) isset($sc['source']) + (int) isset($sc['script']) + (int) !empty($sc['steps']);
            if ($kinds !== 1) {
                throw new ConfigurationException("scenario '{$name}' needs exactly one of source (inline script), script (bundle asset) or steps");
            }
            foreach ($sc['tags'] ?? [] as $t) {
                Validate::name((string) $t, "scenario '{$name}' tags");
            }
        }
        foreach ($this->thresholds as $t) {
            if (!in_array($t['metric'] ?? null, self::METRICS, true)) {
                throw new ConfigurationException('threshold metric must be one of ' . implode(', ', self::METRICS));
            }
            if (!isset($t['max']) && !isset($t['min'])) {
                throw new ConfigurationException("threshold {$t['metric']} needs min or max");
            }
        }
        foreach ($this->feeders as $f) {
            $fname = Validate::name((string) ($f['name'] ?? ''), 'feeder');
            if (isset($f['rows']) === isset($f['path'])) {
                throw new ConfigurationException("feeder '{$fname}' needs either rows or path");
            }
            if (isset($f['rows']) && (!is_array($f['rows']) || $f['rows'] === [])) {
                throw new ConfigurationException("feeder '{$fname}' has no rows");
            }
        }
        if (count($this->percentiles) > 12) {
            throw new ConfigurationException('at most 12 percentiles');
        }
        foreach ($this->percentiles as $p) {
            if ($p <= 0 || $p > 100) {
                throw new ConfigurationException('percentiles must be within (0, 100]');
            }
        }
    }

    /**
     * @param array<string, mixed> $s
     * @return array<string, mixed>
     */
    private static function step(array $s, string $where): array
    {
        $unknown = array_diff(array_keys($s), self::STEP_KEYS);
        if ($unknown !== []) {
            throw new ConfigurationException("{$where}: unknown step field '" . implode("', '", $unknown) . "'");
        }
        $methods = array_intersect(array_keys($s), self::METHODS);
        $actions = count(array_intersect(array_keys($s), self::ACTIONS)) + ($methods === [] ? 0 : 1);
        if (count($methods) > 1 || $actions !== 1) {
            throw new ConfigurationException("{$where}: a step does exactly one thing (request, think, feed, set, transaction boundary or batch)");
        }
        foreach (['capture', 'set'] as $k) {
            foreach (array_keys($s[$k] ?? []) as $var) {
                if (preg_match(Validate::IDENT, (string) $var) !== 1) {
                    throw new ConfigurationException("{$where}: variable '{$var}' must be an identifier");
                }
            }
        }
        foreach ((array) ($s['think'] ?? []) as $d) {
            Validate::duration((string) $d, $where);
        }
        if (isset($s['timeout'])) {
            Validate::duration((string) $s['timeout'], $where);
        }
        foreach (['txn_begin', 'txn_end'] as $k) {
            if (isset($s[$k])) {
                Validate::name((string) $s[$k], $where);
            }
        }
        if (isset($s['batch'])) {
            $s['batch'] = array_map(static fn (array $b): array => self::step($b, "{$where} batch"), $s['batch']);
        }
        return array_filter($s, static fn ($v) => $v !== null && $v !== false && $v !== []);
    }

    /**
     * @param list<array<string, mixed>|list<array<string, mixed>>> $steps
     * @return list<array<string, mixed>>
     */
    private static function steps(array $steps, string $where): array
    {
        $flat = [];
        foreach ($steps as $s) {
            if (array_is_list($s)) {
                array_push($flat, ...$s);
            } else {
                $flat[] = $s;
            }
        }
        $out = [];
        foreach ($flat as $i => $s) {
            $out[] = self::step($s, "{$where} step " . ($i + 1));
        }
        return $out;
    }

    /** @return array<string, mixed> */
    public function toDocument(): array
    {
        $scenarios = [];
        foreach ($this->scenarios as $sc) {
            $name = (string) $sc['name'];
            $doc = array_filter([
                'name' => $name,
                'weight' => $sc['weight'] ?? null,
                'source' => $sc['source'] ?? null,
                'script' => $sc['script'] ?? null,
                'wait' => $sc['wait'] ?? null,
                'fixed_count' => $sc['fixed_count'] ?? null,
                'tags' => $sc['tags'] ?? null,
            ], static fn ($v) => $v !== null && $v !== []);
            if (!empty($sc['on_start'])) {
                $doc['on_start'] = self::steps($sc['on_start'], "scenario '{$name}' on_start");
            }
            if (!empty($sc['steps'])) {
                $doc['step'] = self::steps($sc['steps'], "scenario '{$name}'");
            }
            $scenarios[] = $doc;
        }
        $load = $this->load + ['mode' => 'users'];
        return array_filter([
            'name' => $this->name,
            'target' => array_filter($this->target, static fn ($v) => $v !== null && $v !== []),
            'load' => array_filter($load, static fn ($v) => $v !== null && $v !== []),
            'scenario' => $scenarios,
            'feeder' => $this->feeders ? array_map(static fn (array $f): array => $f + ['mode' => 'sequential'], $this->feeders) : null,
            'threshold' => $this->thresholds ? array_map(static fn (array $t): array => $t + ['request' => '*'], $this->thresholds) : null,
            'report' => $this->percentiles ? ['percentiles' => $this->percentiles] : null,
        ], static fn ($v) => $v !== null);
    }
}
