<?php
// File: ballista/sdks/php/examples/common.php

/*
 * Shared setup for the examples: configuration, argument parsing, progress output and exit codes.
 *
 * Environment:
 *   BALLISTA_URL     run manager, e.g. https://loadtest.example.com
 *   BALLISTA_TOKEN   API token with the 'run' scope
 *   TARGET_URL       system under test as reached from the load generator host
 */

declare(strict_types=1);

require_once __DIR__ . '/../vendor/autoload.php';

use Ballista\Client\Client;
use Ballista\Client\Model\Run;
use Ballista\Client\Model\RunState;
use Ballista\Client\Model\Snapshot;

const EXIT_OK = 0;
const EXIT_ERROR = 1;
const EXIT_FAILED = 2;
const EXIT_TIMEOUT = 3;

if (PHP_SAPI !== 'cli') {
    http_response_code(404);
    exit(1);
}

/**
 * Parses --name value, --name=value and --flag options. Unknown options are rejected.
 *
 * @param list<string> $argv
 * @param array<string, string|null> $options option name to default value (null marks a boolean flag)
 * @return array{0: array<string, string|bool|null>, 1: list<string>}
 */
function parse_args(array $argv, string $usage, array $options = []): array
{
    $options += ['target' => getenv('TARGET_URL') ?: 'http://127.0.0.1:8080/', 'quick' => null, 'no-wait' => null, 'help' => null];
    $values = [];
    foreach ($options as $k => $default) {
        $values[$k] = $default ?? false;
    }
    $positional = [];
    $args = array_slice($argv, 1);
    for ($i = 0; $i < count($args); $i++) {
        $a = $args[$i];
        if (!str_starts_with($a, '--')) {
            $positional[] = $a;
            continue;
        }
        [$key, $value] = array_pad(explode('=', substr($a, 2), 2), 2, null);
        if (!array_key_exists($key, $options)) {
            fwrite(STDERR, "unknown option --{$key}\n{$usage}\n");
            exit(EXIT_ERROR);
        }
        if ($options[$key] === null) {
            $values[$key] = true;
            continue;
        }
        if ($value === null) {
            if (!isset($args[$i + 1])) {
                fwrite(STDERR, "--{$key} requires a value\n");
                exit(EXIT_ERROR);
            }
            $value = $args[++$i];
        }
        $values[$key] = $value;
    }
    if ($values['help'] === true) {
        fwrite(STDOUT, $usage . "\n");
        exit(EXIT_OK);
    }
    return [$values, $positional];
}

function client(): Client
{
    return new Client(timeout: 60.0);
}

/**
 * Overrides that shorten a run when --quick is given.
 *
 * @param array<string, string|bool|null> $args
 * @return array<string, mixed>
 */
function quick(array $args, string $runTime = '5s'): array
{
    return $args['quick'] === true ? ['run_time' => $runTime] : [];
}

/** Returns a stats callback that prints one line per elapsed second to stderr. */
function progress(): callable
{
    $last = -1;
    return static function (Snapshot $s) use (&$last): void {
        $second = (int) $s->elapsedSecs;
        if ($second === $last) {
            return;
        }
        $last = $second;
        $t = $s->total;
        fprintf(STDERR, "  t=%4ds users=%5d rps=%8.1f p95=%8.1fms failures=%d\n", $second, $s->users, $t->rps, $t->p95Ms, $t->failures);
    };
}

/** Prints the outcome and returns a process exit code (0 passed, 2 failed, 1 error). */
function report(Run $run): int
{
    printf("run %s: %s\n", $run->id, $run->state->value);
    if ($run->error !== null) {
        printf("  error: %s\n", $run->error);
    }
    $s = $run->summary;
    if ($s !== null) {
        printf("  requests=%d failures=%d (%.2f%%) rps=%.1f\n", $s->requests, $s->failures, $s->failRatio * 100, $s->rps);
        printf("  latency p50=%.1fms p95=%.1fms p99=%.1fms max=%.1fms\n", $s->p50Ms, $s->p95Ms, $s->p99Ms, $s->maxMs);
        foreach ($s->thresholds as $t) {
            $actual = $t->actual === null ? 'n/a' : sprintf('%.3f', $t->actual);
            printf("  threshold %-11s %-16s actual=%-10s %s\n", $t->metric, $t->request, $actual, $t->passed ? 'pass' : 'FAIL');
        }
    }
    return match ($run->state) {
        RunState::Passed => EXIT_OK,
        RunState::Failed, RunState::Stopped => EXIT_FAILED,
        default => EXIT_ERROR,
    };
}

/** Runs an example body and turns client errors into a message and exit code. */
function main(callable $body): never
{
    try {
        $code = $body();
    } catch (Ballista\Client\Exception\RunTimeoutException $e) {
        fwrite(STDERR, "timeout: {$e->getMessage()}\n");
        $code = EXIT_TIMEOUT;
    } catch (Ballista\Client\Exception\ApiException $e) {
        fwrite(STDERR, "server rejected the request: {$e->status} {$e->errorCode}: {$e->detail}\n");
        $code = EXIT_ERROR;
    } catch (Ballista\Client\Exception\BallistaException $e) {
        fwrite(STDERR, "error: {$e->getMessage()}\n");
        $code = EXIT_ERROR;
    }
    exit((int) $code);
}
