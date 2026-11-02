<?php
// File: ballista/sdks/php/examples/manage_runs.php

/*
 * Housekeeping: server status, recent runs, a latency trend per plan and optional cleanup (admin token).
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Exception\PermissionDeniedException;
use Ballista\Client\Model\RunState;

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php manage_runs.php [--limit N] [--delete-older-than-days N]', ['limit' => '20', 'delete-older-than-days' => '']);
    $limit = max(1, (int) $args['limit']);
    $c = client();
    $me = $c->whoami();
    $status = $c->status();
    printf("server %s as %s (%s); active=%s queued=%d\n", $status['version'], $me['name'], $me['scope'], $status['active_run'] ?? 'none', $status['queued']);
    printf("limits: users<=%.0f run<=%ds queue<=%d\n", $status['limits']['max_users'], $status['limits']['max_run_time_secs'], $status['limits']['max_queue']);

    $trend = [];
    printf("\n%-26s %-10s %-24s %8s %9s\n", 'run', 'state', 'plan', 'p95 ms', 'requests');
    $i = 0;
    foreach ($c->iterRuns() as $run) {
        if ($i++ >= $limit) {
            break;
        }
        $s = $run->summary;
        printf("%-26s %-10s %-24s %8.1f %9d\n", $run->id, $run->state->value, mb_substr($run->planName, 0, 24), $s?->p95Ms ?? 0, $s?->requests ?? 0);
        if ($s !== null && $run->state === RunState::Passed) {
            $trend[$run->planName][] = $s->p95Ms;
        }
    }
    echo "\np95 trend (newest first):\n";
    foreach ($trend as $plan => $values) {
        printf("  %-30s %s\n", mb_substr((string) $plan, 0, 30), implode(' ', array_map(static fn (float $v): string => sprintf('%.0f', $v), array_slice($values, 0, 10))));
    }

    if ($args['delete-older-than-days'] === '') {
        return EXIT_OK;
    }
    $days = filter_var($args['delete-older-than-days'], FILTER_VALIDATE_INT, ['options' => ['min_range' => 0]]);
    if ($days === false) {
        throw new Ballista\Client\Exception\ConfigurationException('--delete-older-than-days must be a non-negative integer');
    }
    $cutoff = new DateTimeImmutable("-{$days} days");
    $old = [];
    foreach ($c->iterRuns() as $run) {
        if ($run->isFinished() && new DateTimeImmutable($run->createdAt) < $cutoff) {
            $old[] = $run->id;
        }
    }
    try {
        foreach ($old as $id) {
            $c->deleteRun($id);
        }
    } catch (PermissionDeniedException) {
        echo "\ndeleting runs requires a token with the admin scope\n";
        return EXIT_ERROR;
    }
    printf("\ndeleted %d runs older than %d days\n", count($old), $days);
    return EXIT_OK;
});
