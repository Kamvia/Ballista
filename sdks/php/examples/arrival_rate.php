<?php
// File: ballista/sdks/php/examples/arrival_rate.php

/*
 * Open model: a fixed number of iterations per second regardless of response times.
 * Dropped iterations show the generator could not keep the requested rate (users exhausted).
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;

function build(string $target, float $rate): Plan
{
    return new Plan(
        name: 'constant arrival rate',
        target: ['base_url' => $target],
        load: Load::arrivalRate([['10s', $rate], ['60s', $rate]], ['max_users' => 500]),
        scenarios: [[
            'name' => 'api',
            'steps' => [Steps::get('/items', ['name' => 'list items']), Steps::get('/items/2', ['name' => 'item detail'])],
        ]],
        thresholds: [
            ['metric' => 'dropped_iterations', 'max' => 0],
            ['metric' => 'p99', 'max' => 500],
            ['metric' => 'p95', 'max' => 250],
        ],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php arrival_rate.php [--target URL] [--rate N] [--quick] [--no-wait]', ['rate' => '50']);
    $rate = filter_var($args['rate'], FILTER_VALIDATE_FLOAT);
    if ($rate === false || $rate <= 0) {
        throw new Ballista\Client\Exception\ConfigurationException('--rate must be a positive number');
    }
    $run = client()->run(build((string) $args['target'], $rate), quick($args, '15s'), ['suite' => 'arrival'], wait: $args['no-wait'] !== true, onStats: progress());
    return $args['no-wait'] === true ? EXIT_OK : report($run);
});
