<?php
// File: ballista/sdks/php/examples/weighted_mix.php

/*
 * Realistic traffic mix: weighted scenarios, a fixed pool of admin users, pacing and tag filtering.
 * Retried attempts are recorded individually, so the flaky endpoint gets its own, looser threshold.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

/** @param list<string>|null $tags */
function build(string $target, ?array $tags): Plan
{
    $writes = $tags === null || in_array('write', $tags, true);
    $thresholds = [
        ['metric' => 'p95', 'request' => 'list items', 'max' => 300],
        ['metric' => 'error_rate', 'request' => 'list items', 'max' => 0.0],
    ];
    if ($writes) {
        $thresholds[] = ['metric' => 'error_rate', 'request' => 'add to cart', 'max' => 0.05];
        $thresholds[] = ['metric' => 'error_rate', 'request' => 'flaky health', 'max' => 0.8];
    }
    return new Plan(
        name: 'traffic mix',
        target: ['base_url' => $target, 'headers' => ['accept' => 'application/json'], 'retries' => 2, 'retry_backoff' => '100ms'],
        load: Load::users([['10s', 30], ['40s', 30]]),
        scenarios: [
            [
                'name' => 'browser',
                'weight' => 7,
                'tags' => ['read'],
                'wait' => Wait::uniform('500ms', '2s'),
                'steps' => [Steps::get('/items', ['name' => 'list items']), Steps::think('300ms'), Steps::get('/items/5', ['name' => 'item detail'])],
            ],
            [
                'name' => 'searcher',
                'weight' => 2,
                'tags' => ['read'],
                'wait' => Wait::exponential('1s'),
                'steps' => [Steps::get('/items', ['name' => 'search', 'query' => ['q' => 'item-1']])],
            ],
            [
                'name' => 'buyer',
                'weight' => 1,
                'tags' => ['write'],
                'wait' => Wait::pace('3s'),
                'on_start' => [Steps::post('/login', ['name' => 'login', 'json' => ['username' => 'buyer', 'password' => 'secret-pw'], 'capture' => ['token' => 'json.token']])],
                'steps' => [Steps::post('/cart', ['name' => 'add to cart', 'headers' => ['authorization' => 'Bearer ${token}'], 'json' => ['item' => 1, 'qty' => 1], 'expect' => [201, 503]])],
            ],
            [
                'name' => 'admin',
                'fixed_count' => 2,
                'tags' => ['write'],
                'wait' => Wait::fixed('2s'),
                'steps' => [Steps::get('/flaky', ['name' => 'flaky health', 'retries' => 3])],
            ],
        ],
        thresholds: $thresholds,
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php weighted_mix.php [--target URL] [--tags read,write] [--quick] [--no-wait]', ['tags' => '']);
    $overrides = quick($args, '8s');
    $tags = $args['tags'] !== '' ? explode(',', (string) $args['tags']) : null;
    if ($tags !== null) {
        $overrides['tags'] = $tags;
    }
    $c = client();
    $run = $c->run(build((string) $args['target'], $tags), $overrides, ['suite' => 'mix'], wait: $args['no-wait'] !== true, onStats: progress());
    if ($args['no-wait'] === true) {
        return EXIT_OK;
    }
    $code = report($run);
    foreach ($c->getStats($run->id)->raw['iterations'] ?? [] as $row) {
        printf("  scenario %-10s iterations=%d\n", (string) ($row['scenario'] ?? ''), (int) ($row['count'] ?? 0));
    }
    return $code;
});
