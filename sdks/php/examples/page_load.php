<?php
// File: ballista/sdks/php/examples/page_load.php

/*
 * Browser-like page loads: the document first, then its resources in parallel, timed as one page.
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
        name: 'page load',
        target: ['base_url' => $target],
        load: Load::users([['10s', 15], ['30s', 15]]),
        scenarios: [[
            'name' => 'visitor',
            'wait' => Wait::uniform('1s', '3s'),
            'steps' => [
                Steps::get('/items', ['name' => 'catalog document', 'capture' => ['first' => 'json.items.0.id']]),
                Steps::batch([
                    Steps::get('/items/${first}', ['name' => 'hero item']),
                    Steps::get('/items/2', ['name' => 'related 1']),
                    Steps::get('/items/3', ['name' => 'related 2']),
                    Steps::get('/slow', ['name' => 'recommendations']),
                ], 'catalog_assets', 4),
                Steps::think('500ms', '1500ms'),
            ],
        ]],
        thresholds: [['metric' => 'p95', 'request' => 'catalog_assets', 'max' => 400]],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php page_load.php [--target URL] [--quick] [--no-wait]');
    $c = client();
    $run = $c->run(build((string) $args['target']), quick($args, '8s'), ['suite' => 'page'], wait: $args['no-wait'] !== true, onStats: progress());
    if ($args['no-wait'] === true) {
        return EXIT_OK;
    }
    $page = $c->getStats($run->id)->row('catalog_assets');
    if ($page !== null) {
        printf("  page assets p50=%.1fms p95=%.1fms over %d pages\n", $page->p50Ms, $page->p95Ms, $page->count);
    }
    return report($run);
});
