<?php
// File: ballista/sdks/php/examples/plan_with_assets.php

/*
 * Existing plan text plus assets sent as content: a script, test data and a file the script uploads.
 * Assets are attached explicitly by name; the server reports any file the plan still needs.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Bundle;

const PLAN = <<<'TOML'
name = "uploads"

[target]
base_url = "{target}"

[load]
mode = "users"
stages = [{ duration = "5s", target = 5 }, { duration = "20s", target = 5 }]

[[scenario]]
name = "uploader"
script = "scenarios/upload.rhai"
wait = { between = ["200ms", "600ms"] }

[[feeder]]
name = "notes"
path = "data/notes.csv"

[[threshold]]
metric = "p95"
request = "upload"
max = 300.0
TOML;

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php plan_with_assets.php [--target URL] [--quick] [--no-wait]');
    $target = (string) $args['target'];
    if (preg_match('#^https?://[^\s"\\\\]+$#', $target) !== 1) {
        throw new Ballista\Client\Exception\ConfigurationException('--target must be an http(s) URL');
    }
    $text = str_replace('{target}', $target, PLAN);
    $c = client();
    $needed = $c->validatePlan($text)['files'];
    printf("plan needs: %s\n", implode(', ', $needed));
    $notes = "note\n";
    for ($i = 1; $i <= 100; $i++) {
        $notes .= "note number {$i}\n";
    }
    $bundle = (new Bundle($text, ['suite' => 'assets']))
        ->addPath('scenarios/upload.rhai', __DIR__ . '/assets/upload.rhai')
        ->addBytes('data/notes.csv', $notes)
        ->addPath('files/report.txt', __DIR__ . '/assets/report.txt');
    $missing = array_diff($needed, array_keys($bundle->files()));
    if ($missing !== []) {
        printf("missing assets: %s\n", implode(', ', $missing));
        return EXIT_ERROR;
    }
    $run = $c->run($bundle, quick($args), wait: $args['no-wait'] !== true, onStats: progress());
    return $args['no-wait'] === true ? EXIT_OK : report($run);
});
