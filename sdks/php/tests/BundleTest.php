<?php
// File: ballista/sdks/php/tests/BundleTest.php

declare(strict_types=1);

namespace Ballista\Client\Tests;

use Ballista\Client\Bundle;
use Ballista\Client\Exception\ConfigurationException;
use PHPUnit\Framework\TestCase;

final class BundleTest extends TestCase
{
    public function testPaths(): void
    {
        foreach (['', '/etc/passwd', '../x', 'a/../b', '.env', 'a//b', 'a b', 'inline/x.csv'] as $bad) {
            self::assertFalse(Bundle::validPath($bad), $bad);
        }
        self::assertTrue(Bundle::validPath('proto/api.pb'));
    }

    public function testPayloadCarriesStructuredPlanAndAssets(): void
    {
        $p = (new Bundle(Fixtures::shopPlan(), ['ci' => '1']))->addBytes('proto/api.pb', "\x00\x01")->toPayload();
        $json = json_decode((string) json_encode($p), true);
        self::assertSame('shop api', $json['plan']['name']);
        self::assertSame(['proto/api.pb' => 'AAE='], $json['files']);
        self::assertSame(['ci' => '1'], $json['labels']);
        self::assertIsString((new Bundle('name = "x"'))->toPayload()['plan']);
        self::assertSame('{"plan":{"name":"x"},"files":{},"labels":{}}', json_encode((new Bundle(['name' => 'x']))->toPayload()));
    }

    public function testLimits(): void
    {
        $cases = [
            static fn () => (new Bundle(['name' => 'x']))->addBytes('plan.toml', ''),
            static fn () => (new Bundle(['name' => 'x']))->addBytes('big.bin', str_repeat("\0", 16 * 1024 * 1024 + 1)),
            static fn () => new Bundle('   '),
            static fn () => new Bundle(['a', 'b']),
            static fn () => new Bundle(['name' => 'x'], ['bad key' => 'v']),
        ];
        foreach ($cases as $i => $f) {
            try {
                $f();
                self::fail("case {$i} was accepted");
            } catch (ConfigurationException) {
                self::addToAssertionCount(1);
            }
        }
    }
}
