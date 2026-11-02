<?php
// File: ballista/sdks/php/examples/ci_gate.php

/*
 * CI quality gate: run a performance check for a build, keep the reports and fail the pipeline on regressions.
 * Exit codes: 0 passed, 2 thresholds failed or stopped, 1 error, 3 timeout.
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
        name: 'ci gate',
        target: ['base_url' => $target, 'headers' => ['accept' => 'application/json']],
        load: Load::users([['15s', 25], ['45s', 25]]),
        scenarios: [[
            'name' => 'api',
            'wait' => Wait::uniform('100ms', '400ms'),
            'on_start' => [Steps::post('/login', ['name' => 'login', 'json' => ['username' => 'ci', 'password' => 'ci-secret'], 'capture' => ['token' => 'json.token']])],
            'steps' => [
                Steps::get('/items', ['name' => 'list items']),
                Steps::get('/items/7', ['name' => 'item detail']),
                Steps::post('/cart', ['name' => 'add to cart', 'headers' => ['authorization' => 'Bearer ${token}'], 'json' => ['item' => 7, 'qty' => 1], 'expect' => [201, 503]]),
            ],
        ]],
        thresholds: [
            ['metric' => 'p95', 'request' => 'list items', 'max' => 200],
            ['metric' => 'p95', 'request' => 'add to cart', 'max' => 300],
            ['metric' => 'error_rate', 'max' => 0.03, 'abort_on_fail' => true, 'abort_delay' => '15s'],
        ],
    );
}

/** CI metadata reduced to label-safe text. */
function ci_value(string $name, string $default, int $max): string
{
    $v = preg_replace('/[\x00-\x1f\x7f]/', '', (string) (getenv($name) ?: $default)) ?? $default;
    return mb_substr($v === '' ? $default : $v, 0, $max);
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php ci_gate.php [--target URL] [--reports DIR] [--timeout SECS] [--quick]', ['reports' => 'perf-reports', 'timeout' => '900']);
    $labels = [
        'pipeline' => ci_value('CI_PIPELINE_ID', 'local', 64),
        'commit' => ci_value('CI_COMMIT_SHA', 'unknown', 40),
        'branch' => ci_value('CI_COMMIT_REF_NAME', 'local', 256),
    ];
    $key = substr(preg_replace('/[^\x21-\x7e]/', '', "ci-{$labels['pipeline']}-{$labels['commit']}") ?? 'ci', 0, 128);
    $c = client();
    $info = $c->uploadBundle(build((string) $args['target']));
    $run = $c->createRun($info, quick($args, '8s'), $labels, $key);
    printf("run %s queued (position %s)\n", $run->id, $run->queuePosition ?? '-');
    $run = $c->wait($run->id, (float) $args['timeout']);
    $code = report($run);
    if ($run->summary !== null) {
        $dir = (string) $args['reports'];
        if (!is_dir($dir) && !mkdir($dir, 0o755, true) && !is_dir($dir)) {
            throw new Ballista\Client\Exception\BallistaException("cannot create {$dir}");
        }
        foreach (['report.html', 'report.json', 'stats.csv', 'failures.csv'] as $name) {
            printf("  saved %s\n", $c->download($run->id, $name, $dir));
        }
    }
    return $code;
});
