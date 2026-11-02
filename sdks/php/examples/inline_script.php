<?php
// File: ballista/sdks/php/examples/inline_script.php

/*
 * Scripted scenario sent inline: WebSocket round trips and Socket.IO acknowledgements with custom checks.
 * The script travels inside the plan and runs on the load generator; nothing executes in this process.
 */

declare(strict_types=1);

require_once __DIR__ . '/common.php';

use Ballista\Client\Load;
use Ballista\Client\Plan;
use Ballista\Client\Wait;

const REALTIME = <<<'RHAI'
fn tasks() { #{ websocket: 1, socketio: 2 } }

fn websocket(step, res) {
    switch step {
        0 => ws("/ws", `hello from ${this.vu_id}`, "ws echo"),
        1 => { if !res.body.starts_with("echo:") { throw "unexpected websocket reply"; } }
    }
}

fn socketio(step, res) {
    switch step {
        0 => #{ socketio: "/socket.io/", emit: "echo", data: #{ user: this.vu_id, n: rand_int(1, 99) }, ack: true, name: "sio echo" },
        1 => {
            if res.json.user != this.vu_id { throw "ack does not match"; }
            #{ socketio: "/socket.io/", emit: "ping", wait: "pong", name: "sio ping" }
        }
    }
}
RHAI;

function build(string $target): Plan
{
    return new Plan(
        name: 'realtime',
        target: ['base_url' => $target],
        load: Load::users([['5s', 10], ['25s', 10]]),
        scenarios: [['name' => 'realtime', 'source' => REALTIME, 'wait' => Wait::fixed('200ms')]],
        thresholds: [
            ['metric' => 'p95', 'request' => 'sio echo', 'max' => 50],
            ['metric' => 'error_rate', 'max' => 0.0],
        ],
    );
}

main(static function () use ($argv): int {
    [$args] = parse_args($argv, 'php inline_script.php [--target URL] [--quick] [--no-wait]');
    $c = client();
    $plan = build((string) $args['target']);
    $check = $c->validatePlan($plan);
    printf("plan '%s' valid, scenarios: %s\n", $check['name'], implode(', ', $check['scenarios']));
    $run = $c->run($plan, quick($args), ['suite' => 'realtime'], wait: $args['no-wait'] !== true, onStats: progress());
    return $args['no-wait'] === true ? EXIT_OK : report($run);
});
