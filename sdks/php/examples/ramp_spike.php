<?php
// File: ballista/sdks/php/examples/ramp_spike.php

/*
 * Ramp, spike and recovery profile. Aborts early when the error rate stays above 5% for 10 seconds.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

function build(string $target, int $peak, bool $quick): Plan
{
    $unit = $quick ? '1s' : '15s';
    $base = intdiv($peak, 4);
    return new Plan(
        name: 'ramp and spike',
        target: ['base_url' => $target, 'timeout' => '10s'],
        load: Load::users(
            [[$unit, $base], [$unit, $base], ['2s', $peak], [$unit, $peak], ['2s', $base], [$unit, $base], ['2s', 0]],
            ['graceful_stop' => '5s'],
        ),
        scenarios: [[
            'name' => 'reader',
            'wait' => Wait::exponential('300ms'),
            'steps' => [Steps::get('/items', ['name' => 'list items']), Steps::get('/slow', ['name' => 'slow page'])],
        ]],
        thresholds: [
            ['metric' => 'error_rate', 'max' => 0.05, 'abort_on_fail' => true, 'abort_delay' => '10s'],
            ['metric' => 'p99', 'request' => 'slow page', 'max' => 1500],
        ],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php ramp_spike.php [--target URL] [--peak N] [--quick] [--no-wait]', ['peak' => '200']);
    $peak = filter_var($args['peak'], FILTER_VALIDATE_INT, ['options' => ['min_range' => 4, 'max_range' => 100000]]);
    if ($peak === false) {
        throw new Ballista\Client\Exception\ConfigurationException('--peak must be an integer 4..100000');
    }
    $quick = $args['quick'] === true;
    $run = client()->run(build((string) $args['target'], $quick ? min($peak, 20) : $peak, $quick), [], ['suite' => 'spike'], wait: $args['no-wait'] !== true, onStats: progress());
    return $args['no-wait'] === true ? EXIT_OK : report($run);
});
