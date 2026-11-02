<?php
// File: ballista/sdks/php/tests/IntegrationTest.php

declare(strict_types=1);

namespace Ballista\Client\Tests;

use Ballista\Client\Bundle;
use Ballista\Client\Client;
use Ballista\Client\Exception\PermissionDeniedException;
use Ballista\Client\Exception\ValidationException;
use Ballista\Client\Model\RunState;
use PHPUnit\Framework\TestCase;

/** Requires BALLISTA_URL, BALLISTA_TOKEN (run scope), BALLISTA_READ_TOKEN and BALLISTA_DEMO_URL. */
final class IntegrationTest extends TestCase
{
    protected function setUp(): void
    {
        foreach (['BALLISTA_URL', 'BALLISTA_TOKEN', 'BALLISTA_READ_TOKEN', 'BALLISTA_DEMO_URL'] as $k) {
            if (getenv($k) === false || getenv($k) === '') {
                self::markTestSkipped('integration environment not configured');
            }
        }
    }

    private static function plan(?string $base = null): \Ballista\Client\Plan
    {
        return Fixtures::shopPlan($base ?? (string) getenv('BALLISTA_DEMO_URL'));
    }

    public function testFullLifecycle(): void
    {
        $c = new Client();
        self::assertSame('run', $c->whoami()['scope']);
        self::assertSame([], $c->validatePlan(self::plan())['files']);
        $info = $c->uploadBundle(self::plan());
        self::assertSame($info->id, $c->uploadBundle(new Bundle(self::plan()))->id);
        $key = 'php-' . hrtime(true);
        $run = $c->createRun($info, ['run_time' => '5s'], ['build' => '9'], $key);
        self::assertSame($run->id, $c->createRun($info, ['run_time' => '5s'], ['build' => '9'], $key)->id);
        $counts = [];
        $done = $c->wait($run->id, 90.0, static function ($s) use (&$counts): void {
            $counts[] = $s->total->count;
        });
        self::assertSame(RunState::Passed, $done->state, (string) $done->error);
        self::assertNotNull($done->summary);
        self::assertGreaterThan(10, $done->summary->requests);
        self::assertNotEmpty($counts);
        self::assertNotNull($c->getStats($run->id)->row('purchase'));
        $html = $c->download($run->id, 'report.html', sys_get_temp_dir());
        self::assertStringStartsWith('<!DOCTYPE html>', (string) file_get_contents($html));
        unlink($html);
        $found = false;
        foreach ($c->iterRuns(RunState::Passed) as $r) {
            $found = $found || $r->id === $run->id;
        }
        self::assertTrue($found);
    }

    public function testPlanTextReportsMissingAssets(): void
    {
        $c = new Client();
        $text = "name = \"text\"\n[target]\nbase_url = \"" . getenv('BALLISTA_DEMO_URL') . "\"\n[load]\nstages = [{ duration = \"1s\", target = 1 }]\n[[scenario]]\nname = \"s\"\nscript = \"scenarios/s.rhai\"\n";
        self::assertSame(['scenarios/s.rhai'], $c->validatePlan($text)['files']);
        try {
            $c->uploadBundle(new Bundle($text));
            self::fail('missing asset accepted');
        } catch (ValidationException) {
            self::addToAssertionCount(1);
        }
        $bundle = (new Bundle($text))->addBytes('scenarios/s.rhai', 'fn run(step, res) { if step == 0 { get("/items") } }');
        $done = $c->run($bundle, ['run_time' => '2s'], timeout: 60.0);
        self::assertSame(RunState::Passed, $done->state, (string) $done->error);
    }

    public function testStopCancelPolicyAndScopes(): void
    {
        $c = new Client();
        $info = $c->uploadBundle(self::plan());
        $first = $c->createRun($info, ['run_time' => '60s']);
        $second = $c->createRun($info, ['run_time' => '60s']);
        self::assertSame(RunState::Cancelled, $c->stopRun($second->id)->state);
        for ($i = 0; $i < 100 && $c->getRun($first->id)->state === RunState::Queued; $i++) {
            usleep(200_000);
        }
        $c->stopRun($first->id);
        self::assertSame(RunState::Stopped, $c->wait($first->id, 60.0)->state);
        try {
            $c->validatePlan(self::plan('http://10.0.0.1:8080/'));
            self::fail('foreign host accepted');
        } catch (ValidationException $e) {
            self::assertStringContainsString('not permitted', $e->detail);
        }
        try {
            $c->createRun($info, ['users' => 100000, 'spawn_rate' => 100000, 'run_time' => '10s']);
            self::fail('limit not enforced');
        } catch (ValidationException) {
            self::addToAssertionCount(1);
        }
        $ro = new Client(token: (string) getenv('BALLISTA_READ_TOKEN'));
        self::assertGreaterThan(0, $ro->status()['limits']['max_queue']);
        $this->expectException(PermissionDeniedException::class);
        $ro->createRun($info);
    }
}
