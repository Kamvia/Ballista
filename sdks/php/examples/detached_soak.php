<?php
// File: ballista/sdks/php/examples/detached_soak.php

/*
 * Long soak test started detached: queue it, exit, and attach later from any machine with the run id.
 *   php detached_soak.php start --duration 2h
 *   php detached_soak.php attach <run-id>
 *   php detached_soak.php stop <run-id>
 *   php detached_soak.php status <run-id>
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Exception\ValidationException;
use Ballista\Client\Load;
use Ballista\Client\Model\RunState;
use Ballista\Client\Plan;
use Ballista\Client\Steps;
use Ballista\Client\Wait;

const USAGE = "php detached_soak.php start [--target URL] [--users N] [--duration 2h]\n"
    . "php detached_soak.php attach|stop|status <run-id>";

function build(string $target, int $users, string $duration): Plan
{
    return new Plan(
        name: 'soak',
        target: ['base_url' => $target],
        load: Load::users([['30s', $users], [$duration, $users]]),
        scenarios: [[
            'name' => 'steady',
            'wait' => Wait::uniform('1s', '3s'),
            'steps' => [Steps::get('/items', ['name' => 'list items']), Steps::think('500ms'), Steps::get('/items/9', ['name' => 'item detail'])],
        ]],
        thresholds: [
            ['metric' => 'error_rate', 'max' => 0.001, 'abort_on_fail' => true, 'abort_delay' => '5m'],
            ['metric' => 'p99', 'max' => 800],
        ],
    );
}

main(static function () use ($argv): int {
    [$args, $pos] = parse_args($argv, USAGE, ['users' => '50', 'duration' => '2h']);
    $cmd = $pos[0] ?? '';
    $c = client();
    if ($cmd === 'start') {
        $users = filter_var($args['users'], FILTER_VALIDATE_INT, ['options' => ['min_range' => 1]]);
        if ($users === false) {
            throw new Ballista\Client\Exception\ConfigurationException('--users must be a positive integer');
        }
        $plan = build((string) $args['target'], $users, (string) $args['duration']);
        try {
            $c->validatePlan($plan);
        } catch (ValidationException $e) {
            fwrite(STDERR, "rejected: {$e->detail}\n");
            return EXIT_ERROR;
        }
        echo $c->run($plan, [], ['suite' => 'soak'], wait: false)->id, "\n";
        return EXIT_OK;
    }
    $id = $pos[1] ?? null;
    if ($id === null || !in_array($cmd, ['attach', 'stop', 'status'], true)) {
        fwrite(STDERR, USAGE . "\n");
        return EXIT_ERROR;
    }
    if ($cmd === 'attach') {
        fwrite(STDERR, "following {$id} (Ctrl+C detaches; the run continues)\n");
        return report($c->wait($id, null, progress()));
    }
    if ($cmd === 'stop') {
        echo $c->stopRun($id)->state->value, "\n";
        return EXIT_OK;
    }
    $run = $c->getRun($id);
    if ($run->isFinished()) {
        return report($run);
    }
    $line = "{$run->id}: {$run->state->value}";
    if ($run->state === RunState::Running) {
        $live = $c->getStats($id);
        $line .= sprintf(', %d users, p95 %.1fms', $live->users, $live->total->p95Ms);
    }
    echo $line, "\n";
    return EXIT_OK;
});
