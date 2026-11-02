<?php
// File: ballista/sdks/php/examples/login_flow.php

/*
 * Authenticated user journey: inline test accounts, login once per user, token reuse, purchase transaction.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

function build(string $target): Plan
{
    $accounts = [];
    for ($i = 1; $i <= 50; $i++) {
        $accounts[] = ['username' => sprintf('shopper%03d', $i), 'password' => sprintf('pw-%03d-secret', $i)];
    }
    return new Plan(
        name: 'login flow',
        target: ['base_url' => $target, 'headers' => ['accept' => 'application/json']],
        load: Load::users([['10s', 20], ['30s', 20], ['5s', 0]]),
        scenarios: [[
            'name' => 'shopper',
            'wait' => Wait::uniform('300ms', '1s'),
            'on_start' => [
                Steps::feed('accounts'),
                Steps::post('/login', [
                    'name' => 'login',
                    'json' => ['username' => '${accounts.username}', 'password' => '${accounts.password}'],
                    'capture' => ['token' => 'json.token'],
                ]),
            ],
            'steps' => [
                Steps::get('/items', ['name' => 'list items', 'query' => ['q' => 'item'], 'capture' => ['item_id' => 'json.items.3.id']]),
                Steps::think('200ms', '600ms'),
                Steps::transaction(
                    'purchase',
                    Steps::get('/items/${item_id}', ['name' => 'item detail']),
                    Steps::post('/cart', [
                        'name' => 'add to cart',
                        'headers' => ['authorization' => 'Bearer ${token}'],
                        'json' => ['item' => '${item_id}', 'qty' => 1, 'request_id' => '${uuid()}'],
                        'expect' => [201, 503],
                    ]),
                ),
            ],
        ]],
        thresholds: [
            ['metric' => 'p95', 'request' => 'purchase', 'max' => 400],
            ['metric' => 'error_rate', 'request' => 'login', 'max' => 0.0],
        ],
        feeders: [['name' => 'accounts', 'rows' => $accounts, 'mode' => 'sequential']],
        percentiles: [75, 99.9],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php login_flow.php [--target URL] [--quick] [--no-wait]');
    $run = client()->run(build((string) $args['target']), quick($args), ['suite' => 'login'], wait: $args['no-wait'] !== true, onStats: progress());
    return $args['no-wait'] === true ? EXIT_OK : report($run);
});
