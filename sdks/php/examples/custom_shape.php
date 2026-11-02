<?php
// File: ballista/sdks/php/examples/custom_shape.php

/*
 * Custom load shape: a sine wave between 5 and 25 users for two minutes, defined by an inline script.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

const WAVE = <<<'RHAI'
fn shape(t) {
    let total = {total};
    if t >= total { return (); }
    let pi = 3.14159265;
    15.0 + 10.0 * sin(2.0 * pi * t / {period})
}
RHAI;

function build(string $target, int $total, int $period): Plan
{
    $source = strtr(WAVE, ['{total}' => "{$total}.0", '{period}' => "{$period}.0"]);
    return new Plan(
        name: 'wave',
        target: ['base_url' => $target],
        load: Load::shape($source, 'users', ['graceful_stop' => '3s']),
        scenarios: [['name' => 'reader', 'wait' => Wait::uniform('100ms', '300ms'), 'steps' => [Steps::get('/items', ['name' => 'list items'])]]],
        thresholds: [['metric' => 'p95', 'max' => 300]],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php custom_shape.php [--target URL] [--quick] [--no-wait]');
    [$total, $period] = $args['quick'] === true ? [10, 5] : [120, 30];
    $run = client()->run(build((string) $args['target'], $total, $period), [], ['suite' => 'shape'], wait: $args['no-wait'] !== true, onStats: progress());
    return $args['no-wait'] === true ? EXIT_OK : report($run);
});
