<?php
// File: ballista/sdks/php/examples/parallel_suites.php

/*
 * Queue several test suites at once, then follow each to completion.
 * The server executes one run at a time; the others wait in its queue.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Model\RunState;
use Ballista\Client\Model\Snapshot;
use Ballista\Client\Plan;
use Ballista\Client\Steps;

const SUITES = [
    'catalog' => ['/items', 'list items'],
    'detail' => ['/items/4', 'item detail'],
    'slow' => ['/slow', 'slow page'],
];

function build(string $target, string $name): Plan
{
    [$path, $request] = SUITES[$name];
    return new Plan(
        name: "suite {$name}",
        target: ['base_url' => $target],
        load: Load::users([['2s', 5], ['6s', 5]]),
        scenarios: [['name' => $name, 'steps' => [Steps::get($path, ['name' => $request])]]],
        thresholds: [['metric' => 'error_rate', 'max' => 0.0]],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php parallel_suites.php [--target URL]');
    $c = client();
    $runs = [];
    foreach (array_keys(SUITES) as $name) {
        $run = $c->createRun($c->uploadBundle(build((string) $args['target'], $name)), [], ['suite' => $name]);
        printf("[%s] queued %s at position %s\n", $name, $run->id, $run->queuePosition ?? '-');
        $runs[$name] = $run;
    }
    $passed = true;
    foreach ($runs as $name => $run) {
        $tick = static function (Snapshot $s) use ($name): void {
            if ((int) $s->elapsedSecs % 2 === 0) {
                printf("  [%s] t=%.0fs rps=%.1f\n", $name, $s->elapsedSecs, $s->total->rps);
            }
        };
        $done = $c->wait($run->id, 600.0, $tick);
        printf("[%s] %s\n", $name, $done->state->value);
        $passed = $passed && $done->state === RunState::Passed;
    }
    return $passed ? EXIT_OK : EXIT_FAILED;
});
