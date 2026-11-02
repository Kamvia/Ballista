<?php
// File: ballista/sdks/php/examples/regression_gate.php

/*
 * Regression gate: the run fails when it is slower than the latest passed run of the same plan.
 * The first run becomes the baseline. Pass --path /slow on a later run to see the gate trip.
 * --workers spreads the run across pool workers (0 runs on the server only).
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
        name: 'regression-gate',
        target: ['base_url' => $target, 'timeout' => '5s'],
        load: Load::users([['3s', 20], ['12s', 20]]),
        scenarios: [['name' => 'detail', 'wait' => Wait::fixed('50ms'), 'steps' => [Steps::get($path, ['name' => 'item detail', 'expect' => 200])]]],
    );
}

/** @param array<string, mixed> $c */
function show(array $c): void
{
    printf("baseline=%s passed=%s regressions=%d\n", $c['baseline'] ?? '-', $c['passed'] ? 'true' : 'false', $c['regressions']);
    foreach (array_slice($c['changes'] ?? [], 0, 8) as $ch) {
        $pct = $ch['change_pct'] === null ? 'n/a' : sprintf('%+.1f%%', $ch['change_pct']);
        printf("  %-11s %-4s %-14s %-10s %10.3f -> %10.3f %9s %s\n", $ch['kind'], $ch['method'], $ch['name'], $ch['metric'],
            $ch['baseline'], $ch['current'], $pct, $ch['regressed'] ? 'REGRESSED' : 'ok');
    }
    if (($c['note'] ?? null) !== null) {
        printf("  note: %s\n", $c['note']);
    }
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php regression_gate.php [--target URL] [--path /items/1] [--workers N] [--max-regression-pct 20] [--quick]', [
        'path' => '/items/1',
        'workers' => '',
        'max-regression-pct' => '20',
    ]);
    $overrides = quick($args, '8s');
    if ($args['workers'] !== '') {
        $overrides['workers'] = (int) $args['workers'];
    }
    $gate = ['max_regression_pct' => (float) $args['max-regression-pct'], 'min_change_ms' => 2.0];
    $run = client()->run(build((string) $args['target'], (string) $args['path']), $overrides, ['suite' => 'regression'], onStats: progress(), regression: $gate);
    $code = report($run);
    if ($run->comparison === null) {
        fwrite(STDERR, "no comparison recorded\n");
        return $code;
    }
    show($run->comparison);
    return $run->comparison['passed'] ? $code : EXIT_FAILED;
});
