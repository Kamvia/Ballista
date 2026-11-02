<?php
// File: ballista/sdks/php/examples/smoke_test.php

/*
 * Smoke test: a few users hit the main endpoints and the run fails on any error.
 *
 *   php smoke_test.php [--target URL] [--quick] [--no-wait]
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

function build(string $target): Plan
{
    return new Plan(
        name: 'smoke',
        target: ['base_url' => $target, 'timeout' => '5s', 'headers' => ['accept' => 'application/json']],
        load: Load::users([['5s', 3], ['20s', 3]]),
        scenarios: [[
            'name' => 'browse',
            'wait' => Wait::uniform('200ms', '500ms'),
            'steps' => [
                Steps::get('/items', ['name' => 'list items', 'expect' => 200]),
                Steps::get('/items/1', ['name' => 'item detail', 'contains' => 'stock']),
            ],
        ]],
        thresholds: [
            ['metric' => 'error_rate', 'max' => 0.0],
            ['metric' => 'p95', 'max' => 500],
        ],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php smoke_test.php [--target URL] [--quick] [--no-wait]');
    $run = client()->run(build((string) $args['target']), quick($args), ['suite' => 'smoke'], wait: $args['no-wait'] !== true, onStats: progress());
    if ($args['no-wait'] === true) {
        echo "queued {$run->id}\n";
        return EXIT_OK;
    }
    return report($run);
});
