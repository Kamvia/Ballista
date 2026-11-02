<?php
// File: ballista/sdks/php/tests/Fixtures.php

declare(strict_types=1);

namespace Ballista\Client\Tests;

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

final class Fixtures
{
    public static function shopPlan(string $baseUrl = 'http://127.0.0.1:8080/'): Plan
    {
        return new Plan(
            name: 'shop api',
            target: ['base_url' => $baseUrl, 'headers' => ['accept' => 'application/json']],
            load: Load::users([['2s', 4], ['4s', 4]]),
            scenarios: [[
                'name' => 'buyer',
                'wait' => Wait::uniform('50ms', '100ms'),
                'on_start' => [
                    Steps::feed('users'),
                    Steps::post('/login', ['json' => ['username' => '${users.username}', 'password' => '${users.password}'], 'capture' => ['token' => 'json.token']]),
                ],
                'steps' => [
                    Steps::get('/items', ['name' => 'list items', 'capture' => ['item_id' => 'json.items.1.id']]),
                    Steps::think('20ms'),
                    Steps::transaction(
                        'purchase',
                        Steps::get('/items/${item_id}', ['name' => 'item', 'contains' => 'stock']),
                        Steps::post('/cart', ['name' => 'cart', 'headers' => ['authorization' => 'Bearer ${token}'], 'json' => ['item' => '${item_id}', 'qty' => 1], 'expect' => [201, 503]]),
                    ),
                    Steps::batch([Steps::get('/items'), Steps::get('/slow', ['name' => 'slow page'])], 'page', 2),
                ],
            ]],
            thresholds: [['metric' => 'p95', 'request' => 'purchase', 'max' => 1000]],
            feeders: [['name' => 'users', 'rows' => array_map(static fn (int $i): array => ['username' => "user{$i}", 'password' => 'secret-pass'], range(1, 5))]],
            percentiles: [75, 99.9],
        );
    }
}
