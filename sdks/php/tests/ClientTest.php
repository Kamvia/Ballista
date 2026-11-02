<?php
// File: ballista/sdks/php/tests/ClientTest.php

declare(strict_types=1);

namespace Ballista\Client\Tests;

use Ballista\Client\Client;
use Ballista\Client\Exception\ConfigurationException;
use Ballista\Client\Exception\NotFoundException;
use Ballista\Client\Exception\ValidationException;
use Ballista\Client\Http\Response;
use Ballista\Client\Model\RunState;
use PHPUnit\Framework\TestCase;

final class ClientTest extends TestCase
{
    private const TOKEN = 'bt_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx';
    private const BUNDLE = '0123456789abcdef0123456789abcdef';
    private const RUN = 'raaaaaaaaaaaaaaaaaaaaaaaa';

    /** @return array<string, mixed> */
    private static function runData(string $state = 'queued'): array
    {
        return ['id' => self::RUN, 'seq' => 1, 'bundle' => self::BUNDLE, 'plan_name' => 'p', 'state' => $state, 'created_at' => '2026-11-01T10:00:00Z', 'created_by' => 'ci', 'labels' => [], 'overrides' => []];
    }

    private static function json(int $status, mixed $body, array $headers = []): Response
    {
        return new Response($status, ['content-type' => 'application/json'] + $headers, (string) json_encode($body));
    }

    private function client(FakeTransport $t): Client
    {
        return new Client('http://127.0.0.1:8070', self::TOKEN, transport: $t, baseDelay: 0.0);
    }

    public function testRefusesInsecureConfigurations(): void
    {
        foreach ([['http://example.com', self::TOKEN], ['https://example.com', 'short'], ['ftp://x', self::TOKEN]] as [$url, $token]) {
            try {
                new Client($url, $token);
                self::fail("accepted {$url}");
            } catch (ConfigurationException) {
                self::addToAssertionCount(1);
            }
        }
        $c = $this->client(new FakeTransport(static fn () => self::json(200, [])));
        self::assertStringNotContainsString(self::TOKEN, print_r($c, true));
        $this->expectException(ConfigurationException::class);
        $c->getRun('../../etc/passwd');
    }

    public function testCreateRunRetriesWithOneIdempotencyKey(): void
    {
        $calls = 0;
        $t = new FakeTransport(static function (string $m, string $u, array $h, ?string $body) use (&$calls): Response {
            ++$calls;
            if ($calls < 3) {
                return self::json(503, ['error' => ['code' => 'busy', 'message' => 'x']], ['retry-after' => '0']);
            }
            TestCase::assertSame(['bundle' => self::BUNDLE, 'overrides' => ['users' => 5, 'run_time' => '30s'], 'labels' => ['ci' => '1']], json_decode((string) $body, true));
            return self::json(201, self::runData());
        });
        $run = $this->client($t)->createRun(self::BUNDLE, ['users' => 5, 'run_time' => '30s'], ['ci' => '1']);
        self::assertSame(RunState::Queued, $run->state);
        self::assertCount(3, $t->requests);
        self::assertCount(1, array_unique(array_map(static fn ($r) => $r['headers']['idempotency-key'], $t->requests)));
        self::assertSame('Bearer ' . self::TOKEN, $t->requests[0]['headers']['authorization']);
    }

    public function testErrorMapping(): void
    {
        $t = new FakeTransport(static fn (string $m, string $u) => str_ends_with($u, '/runs')
            ? self::json(422, ['error' => ['code' => 'invalid', 'message' => 'plan peak exceeds limit']])
            : self::json(404, ['error' => ['code' => 'not_found', 'message' => 'run not found']]));
        $c = $this->client($t);
        try {
            $c->getRun(self::RUN);
            self::fail('expected 404');
        } catch (NotFoundException $e) {
            self::assertSame('not_found', $e->errorCode);
        }
        try {
            $c->createRun(self::BUNDLE);
            self::fail('expected 422');
        } catch (ValidationException $e) {
            self::assertStringContainsString('peak', $e->detail);
        }
        $this->expectException(ConfigurationException::class);
        $c->createRun(self::BUNDLE, ['users' => -1]);
    }

    public function testWaitFollowsEventStream(): void
    {
        $snap = ['plan' => 'p', 'state' => 'running', 'users' => 3, 'total' => ['count' => 10, 'p95_ms' => 12.5]];
        $body = 'event: run' . "\n" . 'data: ' . json_encode(self::runData('running')) . "\n\n: keep-alive\n\n"
            . "event: stats\r\ndata: " . json_encode($snap) . "\r\n\r\n"
            . 'event: end' . "\n" . 'data: ' . json_encode(self::runData('passed')) . "\n\n";
        $c = $this->client(new FakeTransport(static fn () => new Response(200, ['content-type' => 'text/event-stream'], $body)));
        $seen = [];
        $run = $c->wait(self::RUN, onStats: static function ($s) use (&$seen): void {
            $seen[] = $s->total->p95Ms;
        });
        self::assertSame(RunState::Passed, $run->state);
        self::assertSame([12.5], $seen);
    }
}
