<?php
// File: ballista/sdks/php/examples/event_stream.php

/*
 * Server-sent events: each user reads a price stream, resuming after disconnects with Last-Event-ID.
 * Time to first event is reported as `SSE <name>`.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

function build(string $target, string $path): Plan
{
    return new Plan(
        name: 'event stream',
        target: ['base_url' => $target, 'timeout' => '30s'],
        load: Load::users([['5s', 10], ['30s', 10]]),
        scenarios: [[
            'name' => 'subscriber',
            'wait' => Wait::fixed('500ms'),
            'steps' => [Steps::get($path, ['name' => 'price stream', 'sse' => 6, 'reconnects' => 3])],
        ]],
        thresholds: [
            ['metric' => 'p95', 'request' => 'price stream', 'max' => 500],
            ['metric' => 'error_rate', 'max' => 0.0],
        ],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php event_stream.php [--target URL] [--path /sse] [--quick] [--no-wait]', ['path' => '/sse']);
    $c = client();
    $run = $c->run(build((string) $args['target'], (string) $args['path']), quick($args, '6s'), ['suite' => 'sse'], wait: $args['no-wait'] !== true, onStats: progress());
    if ($args['no-wait'] === true) {
        return EXIT_OK;
    }
    $first = $c->getStats($run->id)->row('price stream', 'SSE');
    if ($first !== null) {
        printf("  time to first event p50=%.1fms p95=%.1fms\n", $first->p50Ms, $first->p95Ms);
    }
    return report($run);
});
