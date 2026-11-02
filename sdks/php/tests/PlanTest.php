<?php
// File: ballista/sdks/php/tests/PlanTest.php

declare(strict_types=1);

namespace Ballista\Client\Tests;

use Ballista\Client\Exception\ConfigurationException;
use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use PHPUnit\Framework\TestCase;

final class PlanTest extends TestCase
{
    public function testProducesJsonReadyDocument(): void
    {
        $doc = Fixtures::shopPlan()->toDocument();
        $json = json_decode((string) json_encode($doc), true);
        self::assertSame(['mode' => 'users', 'stages' => [['duration' => '2s', 'target' => 4], ['duration' => '4s', 'target' => 4]]], $json['load']);
        $sc = $json['scenario'][0];
        self::assertSame(['between' => ['50ms', '100ms']], $sc['wait']);
        self::assertSame('${users.username}', $sc['on_start'][1]['json']['username']);
        self::assertCount(7, $sc['step']);
        self::assertSame('purchase', $sc['step'][2]['txn_begin']);
        self::assertSame(['get' => '/slow', 'name' => 'slow page'], $sc['step'][6]['batch'][1]);
        self::assertSame(['username' => 'user1', 'password' => 'secret-pass'], $json['feeder'][0]['rows'][0]);
        self::assertSame(['percentiles' => [75, 99.9]], $json['report']);
    }

    public function testInlineScriptAndShape(): void
    {
        $doc = (new Plan('x', ['base_url' => 'http://127.0.0.1/'], Load::shape('fn shape(t) { 1.0 }'), [['name' => 's', 'source' => 'fn run(step, res) { () }']]))->toDocument();
        self::assertSame(['mode' => 'users', 'shape_source' => 'fn shape(t) { 1.0 }'], $doc['load']);
        self::assertSame(['name' => 's', 'source' => 'fn run(step, res) { () }'], $doc['scenario'][0]);
    }

    public function testRejectsInvalidSpecs(): void
    {
        $base = static fn (array $scenarios, string $url = 'http://x/', array $feeders = []): Plan => new Plan('p', ['base_url' => $url], Load::users([['1s', 1]]), $scenarios, feeders: $feeders);
        $bad = [
            static fn () => $base([['name' => 'a', 'steps' => [Steps::get('/')]]], 'ftp://x'),
            static fn () => $base([['name' => 'bad name!', 'steps' => [Steps::get('/')]]]),
            static fn () => $base([['name' => 'a', 'source' => 'x', 'steps' => [Steps::get('/')]]]),
            static fn () => $base([['name' => 'a', 'steps' => [Steps::get('/')]]], 'http://x/', [['name' => 'u', 'rows' => [['a' => 1]], 'path' => 'x.csv']]),
            static fn () => $base([['name' => 'a', 'steps' => [Steps::get('/', ['capture' => ['not-ident' => 'json.a']])]]])->toDocument(),
            static fn () => $base([['name' => 'a', 'steps' => [['get' => '/', 'think' => '1s']]]])->toDocument(),
            static fn () => $base([['name' => 'a', 'steps' => [Steps::think('soon')]]])->toDocument(),
        ];
        foreach ($bad as $i => $f) {
            try {
                $f();
                self::fail("case {$i} was accepted");
            } catch (ConfigurationException) {
                self::addToAssertionCount(1);
            }
        }
    }
}
