<?php
// File: ballista/sdks/php/examples/validate_plans.php

/*
 * Dry run: let the server validate plans (structure, steps, allowed hosts, limits) without running them.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Exception\ValidationException;
use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Steps;

function good(string $target, int $users = 5): Plan
{
    return new Plan(
        name: 'valid',
        target: ['base_url' => $target],
        load: Load::users([['10s', $users]]),
        scenarios: [['name' => 'a', 'steps' => [Steps::get('/items')]]],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php validate_plans.php [--target URL]');
    $t = (string) $args['target'];
    $cases = [
        'foreign host' => good('http://10.255.255.1:9000/'),
        'too many users' => good($t, 1_000_000),
        'unknown variable source' => [
            'target' => ['base_url' => $t],
            'load' => ['stages' => [['duration' => '5s', 'target' => 1]]],
            'scenario' => [['name' => 'a', 'step' => [['get' => '/', 'capture' => ['x' => 'cookie.session']]]]],
        ],
        'missing asset (plan text)' => "[target]\nbase_url = \"{$t}\"\n[load]\nstages = [{ duration = \"5s\", target = 1 }]\n"
            . "[[scenario]]\nname = \"a\"\nscript = \"scenarios/a.rhai\"\n",
    ];
    $c = client();
    $check = $c->validatePlan(good($t));
    printf("[ok]       valid plan '%s', needs files: %s\n", $check['name'], $check['files'] === [] ? 'none' : implode(', ', $check['files']));
    $failures = 0;
    foreach ($cases as $label => $plan) {
        try {
            $result = $c->validatePlan($plan);
            if ($result['files'] !== []) {
                printf("[files]    %s: bundle must also carry %s\n", $label, implode(', ', $result['files']));
            } else {
                printf("[UNEXPECTED] %s: accepted\n", $label);
                $failures++;
            }
        } catch (ValidationException $e) {
            printf("[rejected] %s: %s\n", $label, $e->detail);
        }
    }
    return $failures > 0 ? EXIT_ERROR : EXIT_OK;
});
